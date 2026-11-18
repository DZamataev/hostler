//! `hostler service run | start | stop | restart | status` (plan Task 6). Lifecycle methods are
//! used against any contract version; after a restart `secrets.status` is read best-effort to
//! report that the database locked again (its failure is ignored, so an older service still works).

use std::process::ExitCode;

use clap::Subcommand;

use crate::client::{Client, Replace, is_sandbox_denied, socket_display};
use crate::contract::heavy::{HeavyEntry, ServiceStatus};
use crate::contract::secrets::{LockState, SecretsStatus};
use crate::contract::wire::Request;
use crate::contract::{CONTRACT_VERSION, Envelope, ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::service::lifecycle::{self, Probe, StopRule};
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
    /// registration; refused while heavy tasks or claim runs run, or a takeover is in progress,
    /// unless --force.
    Install {
        #[arg(
            long,
            help = "Replace the registration even while heavy tasks or claim runs run, or a \
                    takeover is in progress. An agent uses --force only with the operator's \
                    explicit permission."
        )]
        force: bool,
    },
    /// Remove the OS registration and stop the service; refused while heavy tasks or claim runs
    /// run, or a takeover is in progress, unless --force.
    Uninstall {
        #[arg(
            long,
            help = "Remove the registration even while heavy tasks or claim runs run, or a \
                    takeover is in progress. An agent uses --force only with the operator's \
                    explicit permission."
        )]
        force: bool,
    },
    /// Stop the service; refused while heavy tasks or claim runs run, or a takeover is in
    /// progress, unless --force.
    Stop {
        #[arg(
            long,
            help = "Stop even while heavy tasks or claim runs run, or a takeover is in progress. \
                    An agent uses --force only with the operator's explicit permission."
        )]
        force: bool,
    },
    /// Stop, then start the service.
    Restart {
        #[arg(
            long,
            help = "Restart even while heavy tasks or claim runs run, or a takeover is in \
                    progress. An agent uses --force only with the operator's explicit permission."
        )]
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
        // P4 R1-2: a sandbox that may not search the state or socket directories.
        Err(e) => match crate::client::sandbox_denied_in(&e) {
            Some(denied) if matches!(command, ServiceCommand::Status { json: false }) => {
                print_sandbox_denied(&denied);
                return ExitCode::FAILURE;
            }
            Some(denied) => return setup_failed(denied),
            None => return setup_failed(setup_error(format!("{e:#}"), home_recovery())),
        },
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
            ServiceCommand::Start => start(&paths, Replace::OnMismatch).await,
            ServiceCommand::Install { force } => install(&paths, force).await,
            ServiceCommand::Uninstall { force } => uninstall(&paths, force).await,
            ServiceCommand::Stop { force } => match stop(&paths, force, "stop").await {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => code,
            },
            ServiceCommand::Restart { force } => match stop(&paths, force, "restart").await {
                Ok(()) => {
                    // The service was just stopped: a mismatch now means the registration runs
                    // another binary, which no replacement fixes.
                    let code = start(&paths, Replace::Never).await;
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

/// `service status` for a `sandbox_denied`: never "not running" (plan P4 R1-1), since the
/// service may well run outside this sandbox.
fn print_sandbox_denied(e: &HostlerError) {
    let fallback = lifecycle::sandbox_recovery(None);
    println!(
        "hostler cannot reach its service from here: {}",
        e.recovery.as_deref().unwrap_or(&fallback)
    );
    if let Some(socket) = e.details["socket"].as_str() {
        println!("  socket: {socket}");
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

/// One line when the running service speaks this contract but is an older hostler (P4 Task 9,
/// Step 4): `service start` keeps it (no restart storms), and says what restarts it.
fn older_service_note(status: &ServiceStatus) -> Option<String> {
    use crate::service::lifecycle::{Direction, SERVICE_VERSION, replace_direction, this_client};

    let direction = replace_direction(
        this_client(),
        (status.contract_version, &status.service_version),
    );
    (direction == Direction::Newer && status.contract_version == crate::contract::CONTRACT_VERSION)
        .then(|| {
            format!(
                "[hostler] the running service is hostler {}, older than this hostler \
                 {SERVICE_VERSION}; it keeps running. To run this one: hostler doctor --fix \
                 (only while it is idle and its secrets database is locked), or the typed \
                 hostler service restart, which locks the secrets database",
                status.service_version
            )
        })
}

async fn start(paths: &Paths, replace: Replace) -> ExitCode {
    let mut client = match Client::connect_or_start(paths, &OsManager, replace).await {
        Ok(client) => client,
        Err(e) => {
            print_error(&e);
            return ExitCode::FAILURE;
        }
    };
    match query_status(&mut client).await {
        Ok(s) => {
            println!("hostler service is running (pid {})", s.pid);
            if let Some(note) = older_service_note(&s) {
                eprintln!("{note}");
            }
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

/// What an explicit stop prints first when the service reports an unlocked database, or
/// reports nothing (a 0.0.3 service): every stop locks it (plan P4 R1-6). It never refuses.
pub const LOCK_WARNING: &str =
    "this locks the secrets database; a person unlocks it again with: hostler unlock";

/// Prints `LOCK_WARNING` when the running service's database is unlocked or cannot say.
async fn warn_if_unlocked(paths: &Paths) {
    if let Probe::Running {
        secrets_unlocked, ..
    } = lifecycle::probe(paths).await
        && secrets_unlocked != Some(false)
    {
        eprintln!("[hostler] {LOCK_WARNING}");
    }
}

/// Asks the service to stop and waits until it has released its lock.
async fn stop(paths: &Paths, force: bool, verb: &str) -> Result<(), ExitCode> {
    warn_if_unlocked(paths).await;
    let rule = StopRule::Explicit { force };
    match service::stop_service(paths, rule, RegistrationState::Unknown).await {
        Ok(false) => println!("hostler service is not running"),
        Ok(true) => println!("hostler service stopped"),
        Err(StopRefused::Busy(e)) => {
            print_busy(&e, verb);
            return Err(ExitCode::FAILURE);
        }
        Err(StopRefused::NotStopped(reason)) => {
            eprintln!("[hostler] {}", reason.message());
            eprintln!("[hostler] To fix: {}", reason.recovery(None));
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
    if !paths.custom_home {
        warn_if_unlocked(paths).await;
    }
    let installed = manager::install_service(
        paths,
        &exe,
        std::sync::Arc::new(OsManager),
        StopRule::Explicit { force },
    )
    .await;
    report_change(
        installed.map(drop),
        "hostler service is registered with the OS and running",
        "install",
    )
}

async fn uninstall(paths: &Paths, force: bool) -> ExitCode {
    if !paths.custom_home {
        warn_if_unlocked(paths).await;
    }
    report_change(
        manager::uninstall_service(paths, force, &OsManager).await,
        "hostler service registration removed; the service is stopped",
        "uninstall",
    )
}

fn print_busy(e: &HostlerError, verb: &str) {
    for line in busy_lines(e, verb) {
        eprintln!("{line}");
    }
}

/// The refusal lines of `service stop|restart|install|uninstall`: what runs now, then the way
/// through. Every field is another agent's text, so it is sanitized before it reaches a terminal.
fn busy_lines(e: &HostlerError, verb: &str) -> Vec<String> {
    let running: Vec<HeavyEntry> =
        serde_json::from_value(e.details["running"].clone()).unwrap_or_default();
    let leases: Vec<crate::contract::claims::ClaimSummary> =
        serde_json::from_value(e.details["leases"].clone()).unwrap_or_default();
    let plans = e.details["plans"].as_array().cloned().unwrap_or_default();
    let mut lines = vec![format!(
        "[hostler] Not running `hostler service {verb}`: {}",
        crate::secrets::sanitize_for_terminal(&e.message)
    )];
    if running.is_empty() && leases.is_empty() && plans.is_empty() {
        // Busy for another reason (T5-R2: the claims engine does not answer): its own words.
        lines.push(format!(
            "[hostler] To do it anyway: hostler service {verb} --force. {}",
            crate::contract::claims::FORCE_SENTENCE
        ));
        return lines;
    }
    for entry in &running {
        lines.push(format!(
            "[hostler]   {} — {} ({}, PID {})",
            crate::secrets::sanitize_for_terminal(&entry.id),
            crate::secrets::sanitize_for_terminal(&entry.command),
            crate::secrets::sanitize_for_terminal(&entry.cwd),
            entry.holder.pid
        ));
    }
    for lease in &leases {
        lines.push(format!(
            "[hostler]   {} — {} ({}, PID {})",
            crate::secrets::sanitize_for_terminal(&lease.resource),
            crate::secrets::sanitize_for_terminal(lease.command.as_deref().unwrap_or("")),
            crate::secrets::sanitize_for_terminal(&lease.workspace_path),
            lease.pid.map_or_else(|| "?".into(), |pid| pid.to_string())
        ));
    }
    for plan in &plans {
        lines.push(format!(
            "[hostler]   a takeover of {} by {}",
            crate::secrets::sanitize_for_terminal(plan["resource"].as_str().unwrap_or("?")),
            crate::secrets::sanitize_for_terminal(plan["by_workspace"].as_str().unwrap_or("?"))
        ));
    }
    lines.push(format!(
        "[hostler] To do it anyway: hostler service {verb} --force. {} What runs now keeps \
         running: heavy tasks keep their slots in the queue, a lease's claim stays until its \
         command ends, and a takeover in progress finishes first.",
        crate::contract::claims::FORCE_SENTENCE
    ));
    lines
}

async fn status(paths: &Paths, json: bool) -> ExitCode {
    let result = match Client::connect_lifecycle(paths).await {
        Ok(mut client) => query_status(&mut client).await,
        Err(e) => Err(e),
    };
    let (registration, notes) = if paths.custom_home {
        (RegistrationState::Absent, Vec::new())
    } else {
        registration_within(REGISTRATION_CHECK).await
    };
    let (envelope, code) = compose_status(result, registration);
    if json {
        println!("{}", status_json(&envelope, &notes));
        return ExitCode::from(code);
    }
    let socket = socket_display(&paths.socket);
    if let Some(e) = envelope.error.as_ref().filter(|e| is_sandbox_denied(e)) {
        print_sandbox_denied(e);
        return ExitCode::from(code);
    }
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
    for text in notes
        .iter()
        .filter_map(|n| crate::service::manager::note_text(n))
    {
        println!("  {text}");
    }
    ExitCode::from(code)
}

/// The status envelope as P1 froze it, plus `registration_notes` beside it when there are any
/// (P2-13): composed here, so the service's answer is unchanged.
fn status_json(envelope: &Envelope<ServiceStatus>, notes: &[&'static str]) -> String {
    let mut value = serde_json::to_value(envelope).expect("an envelope always encodes");
    if !notes.is_empty()
        && let Some(map) = value.as_object_mut()
    {
        map.insert("registration_notes".into(), serde_json::json!(notes));
    }
    value.to_string()
}

/// How long `status` waits for the OS registration to be read (`schtasks` can hang).
const REGISTRATION_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

async fn registration_within(limit: std::time::Duration) -> (RegistrationState, Vec<&'static str>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    // A plain thread: a hung `schtasks` must not keep the process from exiting.
    std::thread::spawn(move || {
        let _ = tx.send((OsManager.registration(), OsManager.registration_notes()));
    });
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(answer)) => answer,
        _ => (RegistrationState::Unknown, Vec::new()),
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
/// back after a restart): `ok` is false, `data` still shows the running service. A
/// `sandbox_denied` is never "not running" and never hidden by the registration (exit 1).
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
        Err(e) if is_sandbox_denied(&e) => (
            Envelope {
                ok: false,
                data: None,
                error: Some(e),
            },
            1,
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

    /// P4 Task 9 Step 4: an older same-contract service gets one line; others get none.
    #[test]
    fn older_service_note_names_the_ways_to_restart() {
        let status = |contract, version: &str| ServiceStatus {
            exe: None,
            instance: None,
            service_version: version.into(),
            contract_version: contract,
            pid: 1,
            running_heavy: 0,
            waiting_heavy: 0,
            secrets_unlocked: Some(false),
            boot_id: None,
        };
        let current = crate::contract::CONTRACT_VERSION;
        let note = older_service_note(&status(current, "0.0.1")).unwrap();
        assert!(
            note.contains("hostler 0.0.1, older than this hostler"),
            "{note}"
        );
        assert!(note.contains("hostler doctor --fix"), "{note}");
        assert!(
            note.contains("hostler service restart, which locks"),
            "{note}"
        );
        let this = crate::service::lifecycle::SERVICE_VERSION;
        assert_eq!(older_service_note(&status(current, this)), None);
        assert_eq!(older_service_note(&status(current, "99.0.0")), None);
        assert_eq!(older_service_note(&status(current, "dev")), None);
    }

    #[test]
    fn busy_lines_sanitize_another_agents_text_and_name_the_force_rule() {
        let mut lease = crate::contract::claims::fixtures::summary();
        lease.resource = "account \u{1b}]0;PWNED\u{7}".into();
        lease.workspace_path = "/w/\u{1b}[2J".into();
        lease.command = Some("login \u{1b}]0;X\u{7}".into());
        let error = HostlerError {
            code: ErrorCode::ServiceBusy,
            message: "busy \u{1b}[2J".into(),
            details: serde_json::json!({
                "leases": [lease],
                "plans": [{ "resource": "screen\u{1b}", "by_workspace": "/w/b\u{7}" }],
            }),
            recovery: None,
        };
        let lines = busy_lines(&error, "stop");
        let text = lines.join("\n");
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(!text.contains('\u{7}'), "{text:?}");
        assert!(
            text.contains(crate::contract::claims::FORCE_SENTENCE),
            "{text}"
        );
        assert!(text.contains("a lease's claim stays"), "{text}");
    }

    fn running() -> ServiceStatus {
        ServiceStatus {
            exe: None,
            instance: None,
            service_version: "0.0.1".into(),
            contract_version: CONTRACT_VERSION,
            pid: 42,
            running_heavy: 0,
            waiting_heavy: 0,
            secrets_unlocked: None,
            boot_id: None,
        }
    }

    fn not_running() -> HostlerError {
        HostlerError {
            code: ErrorCode::ServiceUnavailable,
            message: "not running".into(),
            details: serde_json::json!({ "reason": "not_running" }),
            recovery: Some("hostler service start".into()),
        }
    }

    /// P2-13: a unit without `KillMode=process` is current and serves; `status` keeps its exit
    /// code and state, prints the note and lists it in `--json` beside the frozen data.
    #[test]
    fn old_unit_without_killmode_is_current_with_a_note() {
        let (env, code) = compose_status(Ok(running()), RegistrationState::Current);
        assert_eq!(code, 0);
        let notes = [crate::service::manager::systemd::UNIT_LACKS_KILLMODE];
        let json: serde_json::Value = serde_json::from_str(&status_json(&env, &notes)).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(
            json["registration_notes"],
            serde_json::json!(["unit_lacks_killmode"])
        );
        assert_eq!(json["data"]["pid"], 42);
        let plain: serde_json::Value = serde_json::from_str(&status_json(&env, &[])).unwrap();
        assert_eq!(
            plain,
            serde_json::to_value(&env).unwrap(),
            "unchanged without notes"
        );
        let text = crate::service::manager::note_text("unit_lacks_killmode").unwrap();
        assert!(text.starts_with("note: the unit lacks KillMode=process"));
        assert!(text.contains("run hostler service install"));
        assert_eq!(crate::service::manager::note_text("other"), None);
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

    /// P4 R1-1: `sandbox_denied` is a problem (1), never "not running" (3), and an outdated
    /// registration never hides it.
    #[test]
    fn sandbox_denied_is_not_not_running() {
        let socket = crate::paths::SocketName::File("/tmp/x/hostler.sock".into());
        let denied = crate::client::sandbox_denied(
            &socket,
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        for state in [
            RegistrationState::Absent,
            RegistrationState::Current,
            RegistrationState::Outdated,
            RegistrationState::Unknown,
        ] {
            let (env, code) = compose_status(Err(denied.clone()), state);
            assert_eq!(code, 1, "{state:?}");
            let error = env.error.unwrap();
            assert_eq!(error.details["reason"], "sandbox_denied", "{state:?}");
            assert!(
                error
                    .recovery
                    .unwrap()
                    .contains("see: hostler guide sandbox")
            );
        }
    }
}
