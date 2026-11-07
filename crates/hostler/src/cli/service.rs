//! `hostler service run | start | stop | restart | status` (plan Task 6). Lifecycle methods are
//! used against any contract version; after a restart `secrets.status` is read best-effort to
//! report that the database locked again (its failure is ignored, so an older service still works).

use std::process::ExitCode;

use clap::Subcommand;

use crate::client::{Client, socket_display};
use crate::contract::heavy::{HeavyEntry, ServiceStatus};
use crate::contract::secrets::{LockState, SecretsStatus};
use crate::contract::wire::Request;
use crate::contract::{CONTRACT_VERSION, Envelope, ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::service::manager::{self, ChangeRefused, OsManager, RegistrationState};
use crate::service::{self, StopRefused};

/// Exit code of `status` when no service runs (the LSB convention).
const NOT_RUNNING: u8 = 3;

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Run the service in the foreground (what the OS or a client starts).
    Run {
        /// Started by the OS service manager (launchd, systemd, Task Scheduler). Without it, a
        /// default-home service steps aside when an OS registration exists.
        #[arg(long)]
        managed: bool,
    },
    /// Start the service unless it runs.
    Start,
    /// Register the service with the OS so it starts at logon, replacing an existing
    /// registration; refused while heavy tasks run unless --force.
    Install {
        /// Replace even while heavy tasks run (agents: only with the operator's explicit permission).
        #[arg(long)]
        force: bool,
    },
    /// Remove the OS registration and stop the service; refused while heavy tasks run unless
    /// --force.
    Uninstall {
        /// Remove even while heavy tasks run (agents: only with the operator's explicit permission).
        #[arg(long)]
        force: bool,
    },
    /// Stop the service; refused while heavy tasks run unless --force.
    Stop {
        /// Stop even while heavy tasks run (agents: only with the operator's explicit permission).
        #[arg(long)]
        force: bool,
    },
    /// Stop, then start the service.
    Restart {
        /// Restart even while heavy tasks run (agents: only with the operator's explicit permission).
        #[arg(long)]
        force: bool,
    },
    /// Show whether the service runs, its versions and the heavy-queue counts.
    Status {
        #[arg(long)]
        json: bool,
    },
}

pub fn main(command: ServiceCommand) -> ExitCode {
    // Setup failures are reported like every other error: as an Envelope for `--json`.
    let json = matches!(command, ServiceCommand::Status { json: true });
    let setup_failed = |error: HostlerError| {
        if json {
            print_envelope(&Err(error));
        } else {
            print_error(&error);
        }
        ExitCode::FAILURE
    };
    let paths = match Paths::resolve() {
        Ok(paths) => paths,
        Err(e) => return setup_failed(setup_error(format!("{e:#}"), home_recovery())),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            return setup_failed(setup_error(
                format!("cannot start the async runtime: {e}"),
                None,
            ));
        }
    };
    runtime.block_on(async {
        match command {
            ServiceCommand::Run { managed } => match service::run_service(paths, managed).await {
                Ok(service::RunEnd::Registered) => {
                    eprintln!(
                        "[hostler] Not serving: an OS registration exists for this home; use \
                         `hostler service start`. If that registration is from an older hostler \
                         (no --managed), refresh it with `hostler service install`."
                    );
                    ExitCode::SUCCESS
                }
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("[hostler] the service failed: {e:#}");
                    ExitCode::FAILURE
                }
            },
            ServiceCommand::Start => start(&paths).await,
            ServiceCommand::Install { force } => install(&paths, force).await,
            ServiceCommand::Uninstall { force } => uninstall(&paths, force).await,
            ServiceCommand::Stop { force } => match stop(&paths, force, "stop").await {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => code,
            },
            ServiceCommand::Restart { force } => match stop(&paths, force, "restart").await {
                Ok(()) => {
                    let code = start(&paths).await;
                    if code == ExitCode::SUCCESS {
                        warn_if_secrets_locked(&paths).await;
                    }
                    code
                }
                Err(code) => code,
            },
            ServiceCommand::Status { json } => status(&paths, json).await,
        }
    })
}

fn print_envelope(result: &Result<ServiceStatus, HostlerError>) {
    let envelope = match result {
        Ok(s) => Envelope {
            ok: true,
            data: Some(s.clone()),
            error: None,
        },
        Err(e) => Envelope {
            ok: false,
            data: None,
            error: Some(e.clone()),
        },
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("an envelope always encodes")
    );
}

/// A failure before any service is contacted (state directory, runtime).
fn setup_error(message: String, recovery: Option<String>) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message,
        details: serde_json::json!({}),
        recovery,
    }
}

/// `chmod 700 <dir>` for the first state directory other users can reach, if any.
fn home_recovery() -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let home = match std::env::var_os("HOSTLER_HOME") {
            Some(home) => std::path::PathBuf::from(home),
            None => Paths::default_home().ok()?,
        };
        let paths = Paths::under(home, true);
        let mut dirs = vec![paths.home.clone(), paths.log_dir.clone()];
        if let crate::paths::SocketName::File(socket) = &paths.socket
            && let Some(dir) = socket.parent()
        {
            dirs.push(dir.to_path_buf());
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        dirs.into_iter()
            .find(|dir| {
                std::fs::symlink_metadata(dir).is_ok_and(|m| {
                    m.is_dir() && m.uid() == me && m.permissions().mode() & 0o077 != 0
                })
            })
            .map(|dir| format!("chmod 700 {}", shell_quote(&dir.display().to_string())))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
fn shell_quote(s: &str) -> String {
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn print_error(e: &HostlerError) {
    eprintln!("[hostler] {}", e.message);
    if let Some(recovery) = &e.recovery {
        eprintln!("[hostler] To fix: {recovery}");
    }
}

async fn query_status(client: &mut Client) -> Result<ServiceStatus, HostlerError> {
    let value = client.call(Request::ServiceStatus).await?;
    serde_json::from_value(value).map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("unreadable service.status answer: {e}"),
        details: serde_json::json!({}),
        recovery: None,
    })
}

async fn start(paths: &Paths) -> ExitCode {
    let mut client = match Client::connect_or_start(paths, &OsManager).await {
        Ok(client) => client,
        Err(e) => {
            print_error(&e);
            return ExitCode::FAILURE;
        }
    };
    match query_status(&mut client).await {
        Ok(s) => {
            println!("hostler service is running (pid {})", s.pid);
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

/// L1: a restart locks the database. Say so when it did (a status failure changes nothing).
async fn warn_if_secrets_locked(paths: &Paths) {
    let Ok(mut client) = Client::connect(paths).await else {
        return;
    };
    let Ok(value) = client.call(Request::SecretsStatus).await else {
        return;
    };
    let Ok(status) = serde_json::from_value::<SecretsStatus>(value) else {
        return;
    };
    if status.state == LockState::Locked {
        eprintln!(
            "[hostler] The secrets database is locked again after the restart; a person runs \
             `hostler unlock`."
        );
    }
}

/// Asks the service to stop and waits until it has released its lock.
async fn stop(paths: &Paths, force: bool, verb: &str) -> Result<(), ExitCode> {
    match service::stop_service(paths, force).await {
        Ok(false) => println!("hostler service is not running"),
        Ok(true) => println!("hostler service stopped"),
        Err(StopRefused::Busy(e)) => {
            print_busy(&e, verb);
            return Err(ExitCode::FAILURE);
        }
        Err(StopRefused::Failed(e)) => {
            print_error(&e);
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}

fn report_change(result: Result<(), ChangeRefused>, done: &str, verb: &str) -> ExitCode {
    match result {
        Ok(()) => {
            println!("{done}");
            ExitCode::SUCCESS
        }
        Err(ChangeRefused::Busy(e)) => {
            print_busy(&e, verb);
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("[hostler] {e}");
            ExitCode::FAILURE
        }
    }
}

async fn install(paths: &Paths, force: bool) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("[hostler] cannot find the hostler executable: {e}");
            return ExitCode::FAILURE;
        }
    };
    report_change(
        manager::install_service(paths, force, &exe, std::sync::Arc::new(OsManager)).await,
        "hostler service is registered with the OS and running",
        "install",
    )
}

async fn uninstall(paths: &Paths, force: bool) -> ExitCode {
    report_change(
        manager::uninstall_service(paths, force, &OsManager).await,
        "hostler service registration removed; the service is stopped",
        "uninstall",
    )
}

fn print_busy(e: &HostlerError, verb: &str) {
    eprintln!("[hostler] Not running `hostler service {verb}`: heavy tasks are running.");
    let running: Vec<HeavyEntry> =
        serde_json::from_value(e.details["running"].clone()).unwrap_or_default();
    for entry in &running {
        eprintln!(
            "[hostler]   {} — {} ({}, PID {})",
            entry.id, entry.command, entry.cwd, entry.holder.pid
        );
    }
    eprintln!(
        "[hostler] To do it anyway: hostler service {verb} --force (agents: only with the \
         operator's explicit permission). The tasks keep running; their slots are kept in the \
         queue and checked again when the service restarts."
    );
}

async fn status(paths: &Paths, json: bool) -> ExitCode {
    let result = match Client::connect_lifecycle(paths).await {
        Ok(mut client) => query_status(&mut client).await,
        Err(e) => Err(e),
    };
    let registration = if paths.custom_home {
        RegistrationState::Absent
    } else {
        registration_within(REGISTRATION_CHECK).await
    };
    let (envelope, code) = compose_status(result, registration);
    if json {
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("an envelope always encodes")
        );
        return ExitCode::from(code);
    }
    let socket = socket_display(&paths.socket);
    match &envelope.data {
        Some(s) => {
            println!("hostler service: running");
            println!("  pid: {}", s.pid);
            println!(
                "  service: hostler {} (contract {})",
                s.service_version, s.contract_version
            );
            println!(
                "  this client: hostler {} (contract {CONTRACT_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            println!("  socket: {socket}");
            println!(
                "  heavy queue: {} running, {} waiting",
                s.running_heavy, s.waiting_heavy
            );
        }
        None if code == NOT_RUNNING || registration == RegistrationState::Outdated => {
            println!("hostler service: not running");
            println!("  socket: {socket}");
        }
        None => {}
    }
    if let Some(e) = &envelope.error
        && (envelope.data.is_none() || registration == RegistrationState::Outdated)
        && code != NOT_RUNNING
    {
        print_error(e);
    }
    ExitCode::from(code)
}

/// How long `status` waits for the OS registration to be read (`schtasks` can hang).
const REGISTRATION_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

async fn registration_within(limit: std::time::Duration) -> RegistrationState {
    let (tx, rx) = tokio::sync::oneshot::channel();
    // A plain thread: a hung `schtasks` must not keep the process from exiting.
    std::thread::spawn(move || {
        let _ = tx.send(OsManager.registration());
    });
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(state)) => state,
        _ => RegistrationState::Unknown,
    }
}

/// An OS registration written by an older hostler (no `--managed`; on Windows, no headless
/// conhost).
fn outdated_registration() -> HostlerError {
    HostlerError {
        code: ErrorCode::ServiceUnavailable,
        message: "the OS registration is from an older hostler and does not match this one: \
                  without --managed its service steps aside and never serves, and an older \
                  Windows task also shows a console window"
            .into(),
        details: serde_json::json!({ "reason": "registration_outdated" }),
        recovery: Some("hostler service install".into()),
    }
}

/// The `status` Envelope and exit code: 0 running, 3 not running, 1 any other problem. An
/// outdated registration is a problem even while an old service still runs (it will not come
/// back after a restart): `ok` is false, `data` still shows the running service.
fn compose_status(
    result: Result<ServiceStatus, HostlerError>,
    registration: RegistrationState,
) -> (Envelope<ServiceStatus>, u8) {
    let outdated = registration == RegistrationState::Outdated;
    match result {
        Ok(status) if outdated => (
            Envelope {
                ok: false,
                data: Some(status),
                error: Some(outdated_registration()),
            },
            1,
        ),
        Ok(status) => (
            Envelope {
                ok: true,
                data: Some(status),
                error: None,
            },
            0,
        ),
        Err(e) => {
            let code = match e.code {
                _ if outdated => 1,
                ErrorCode::ServiceUnavailable => NOT_RUNNING,
                _ => 1,
            };
            let error = if outdated { outdated_registration() } else { e };
            (
                Envelope {
                    ok: false,
                    data: None,
                    error: Some(error),
                },
                code,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> ServiceStatus {
        ServiceStatus {
            service_version: "0.0.1".into(),
            contract_version: CONTRACT_VERSION,
            pid: 42,
            running_heavy: 0,
            waiting_heavy: 0,
        }
    }

    fn not_running() -> HostlerError {
        HostlerError {
            code: ErrorCode::ServiceUnavailable,
            message: "not running".into(),
            details: serde_json::json!({ "reason": "not_running" }),
            recovery: Some("hostler service run".into()),
        }
    }

    #[test]
    fn status_reports_an_outdated_registration_with_its_fix() {
        let (ok, code) = compose_status(Ok(running()), RegistrationState::Current);
        assert!(ok.ok && ok.error.is_none() && code == 0);

        let (env, code) = compose_status(Ok(running()), RegistrationState::Outdated);
        assert!(!env.ok);
        assert_eq!(env.data.unwrap().pid, 42);
        let error = env.error.unwrap();
        assert_eq!(error.details["reason"], "registration_outdated");
        assert_eq!(error.recovery.as_deref(), Some("hostler service install"));
        assert_eq!(code, 1);

        let (env, code) = compose_status(Err(not_running()), RegistrationState::Outdated);
        assert!(env.data.is_none());
        assert_eq!(
            env.error.unwrap().details["reason"],
            "registration_outdated"
        );
        assert_eq!(code, 1);

        for state in [
            RegistrationState::Absent,
            RegistrationState::Unknown,
            RegistrationState::Current,
        ] {
            let (env, code) = compose_status(Err(not_running()), state);
            assert_eq!(
                env.error.unwrap().details["reason"],
                "not_running",
                "{state:?}"
            );
            assert_eq!(code, NOT_RUNNING);
        }
    }
}
