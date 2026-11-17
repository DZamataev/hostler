//! Doctor's safe fixes, run in-process under probe → `may_stop` → atomic stop (plan P4 Task 6,
//! R2-2). Nothing here goes through a shell, needs a person, takes a resource, deletes data or
//! forces: every stop is an `Automatic` rule, so `service.shutdown` always carries `if_idle:
//! true, refuse_if_unlocked: true`, and a `ServiceBusy` is a refusal, never retried with less.

use std::path::Path;
use std::time::Instant;

use crate::client::{Client, Replace};
use crate::contract::ErrorCode;
use crate::project::config;
use crate::service::lifecycle::{AutoPurpose, StopRule};
use crate::service::manager::{self, ChangeRefused, RegistrationState};
use crate::service::{self, StopRefused, Stopping};

use super::block;
use super::files;
use super::{DoctorEnv, FixAction};

/// What a fix did; the re-check decides between `fixed` and `still_failing`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// It ran (successfully or not): what happened.
    Ran(String),
    /// Not run, or refused by the service or by `may_stop`: why. Never forced.
    Refused(String),
}

/// D-Q2: what an automatic install says to a dev build.
pub fn dev_build_note(exe: &Path) -> String {
    format!(
        "this is a dev build ({}); installing registers it as the machine's service, so doctor \
         does not; whoever means it types: hostler service install",
        exe.display()
    )
}

/// Runs one safe fix, finishing by `until` (the caller also cuts it off there).
pub async fn apply(env: &DoctorEnv, cwd: &Path, action: FixAction, until: Instant) -> Applied {
    match action {
        FixAction::Install(purpose) => install(env, purpose).await,
        FixAction::Start => start(env).await,
        FixAction::Restart => restart(env, until).await,
        FixAction::DocsBlock => docs_block(cwd),
    }
}

async fn install(env: &DoctorEnv, purpose: AutoPurpose) -> Applied {
    if env.dev_build {
        return Applied::Refused(dev_build_note(&env.exe));
    }
    // R2-2 (review t6-r2): an automatic install needs a registration it could read. Checked
    // here again (the check pass also never marks it safe on `Unknown`); bounded like every
    // registration lookup.
    let registrar = env.registrar.clone();
    let known = super::bounded_blocking(super::checks::LOOKUP_BOUND, move || {
        registrar.registration()
    })
    .await;
    if matches!(known, None | Some(RegistrationState::Unknown)) {
        return Applied::Refused(unknown_registration_note());
    }
    // R2-1: on its own worker, with its own runtime, awaited to its end: the OS calls inside
    // are synchronous (each bounded by `MANAGER_CALL_TIMEOUT`), so they never block doctor's
    // runtime, and the change is never abandoned half-way.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (paths, exe, registrar) = (env.paths.clone(), env.exe.clone(), env.registrar.clone());
    let spawned = std::thread::Builder::new()
        .name("hostler-doctor-install".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| ChangeRefused::Failed(format!("cannot start a runtime: {e}")))
                .and_then(|runtime| {
                    runtime.block_on(manager::install_service(
                        &paths,
                        &exe,
                        registrar,
                        StopRule::Automatic(purpose),
                    ))
                });
            let _ = tx.send(result);
        });
    if let Err(e) = spawned {
        return Applied::Ran(format!("cannot start the install: {e}"));
    }
    let Ok(result) = rx.await else {
        return Applied::Ran("the install ended without an answer".into());
    };
    match result {
        Ok(installed) if installed.stopped => {
            Applied::Ran("registered this hostler with the OS and replaced the idle service".into())
        }
        Ok(_) => Applied::Ran("registered this hostler with the OS and started it".into()),
        Err(ChangeRefused::Busy(e)) => Applied::Refused(e.message),
        Err(refused @ (ChangeRefused::NotStopped { .. } | ChangeRefused::CustomHome)) => {
            Applied::Refused(refused.to_string())
        }
        Err(ChangeRefused::Failed(message)) => Applied::Ran(message),
    }
}

/// Why an automatic install is never run over a registration doctor could not read.
pub fn unknown_registration_note() -> String {
    "cannot read the OS registration, so doctor registers nothing; a person who means it types: \
     hostler service install"
        .into()
}

/// `connect_or_start` with `Replace::OnMismatch`: what `hostler service start` does (D-Q1).
async fn start(env: &DoctorEnv) -> Applied {
    match Client::connect_or_start_with(&env.paths, &env.registrar, &env.exe, Replace::OnMismatch)
        .await
    {
        Ok(_) => Applied::Ran("the service answers".into()),
        // `may_stop` refused, or the atomic stop answered busy: nothing was stopped.
        Err(e)
            if e.code == ErrorCode::ContractMismatch
                && (!e.details["refusal"].is_null() || !e.details["busy"].is_null()) =>
        {
            Applied::Refused(e.message)
        }
        Err(e) if crate::client::is_sandbox_denied(&e) => Applied::Refused(e.message),
        Err(e) => Applied::Ran(e.message),
    }
}

/// The `service.binary` restart: probe → `may_stop(Automatic(Replace))` → the atomic stop (one
/// connection, `service::request_stop`), a wait for the old service to go that ends at `until`
/// (R1-1: never `STOP_WAIT`'s 40 s), then `connect_or_start`.
async fn restart(env: &DoctorEnv, until: Instant) -> Applied {
    let process = match service::request_stop(
        &env.paths,
        StopRule::Automatic(AutoPurpose::Replace),
        RegistrationState::Unknown,
    )
    .await
    {
        Ok(Stopping::Accepted { process, .. }) => process,
        Ok(Stopping::NotRunning) => None,
        Err(StopRefused::NotStopped(refusal)) => {
            return Applied::Refused(refusal.message().into());
        }
        Err(StopRefused::Busy(e)) => return Applied::Refused(e.message),
        Err(StopRefused::Failed(e)) => return Applied::Ran(e.message),
    };
    let limit = until.saturating_duration_since(Instant::now());
    if let Err(refused) = service::wait_gone(&env.paths, process.as_ref(), limit).await {
        let message = match refused {
            StopRefused::Failed(e) | StopRefused::Busy(e) => e.message,
            StopRefused::NotStopped(refusal) => refusal.message().into(),
        };
        return Applied::Ran(super::out_of_time(&format!(
            "the service accepted the stop but had not gone ({message})"
        )));
    }
    match Client::connect_or_start_with(&env.paths, &env.registrar, &env.exe, Replace::Never).await
    {
        Ok(_) => Applied::Ran("restarted the service".into()),
        Err(e) => Applied::Ran(format!("stopped, but the start failed: {}", e.message)),
    }
}

/// Task 5's block edit (`init::docs::bring_current`), and nothing else: never `hostler.toml`, never another file.
fn docs_block(cwd: &Path) -> Applied {
    let Ok(checkout) = crate::project::find_checkout(cwd) else {
        return Applied::Refused("not in a git checkout".into());
    };
    let root = &checkout.workspace_root;
    let config = match files::read_text(root, "hostler.toml")
        .map_err(|e| e.to_string())
        .and_then(|src| config::parse(&src).map_err(|_| "hostler.toml is invalid".to_string()))
    {
        Ok(config) => config,
        Err(why) => return Applied::Refused(format!("hostler.toml: {why}")),
    };
    // Task 5's `init::docs::bring_current` is the only writer.
    let changed = match block::bring_current(root, &config) {
        Ok(changed) => changed,
        Err(why) => return Applied::Ran(why),
    };
    if changed.is_empty() {
        Applied::Ran("the hostler block was already current".into())
    } else {
        Applied::Ran(format!("wrote the hostler block in {}", changed.join(", ")))
    }
}
