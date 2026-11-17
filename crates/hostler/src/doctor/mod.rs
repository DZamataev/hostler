//! `hostler doctor` (plan P4 Task 6): independent checks of the service, the registration, the
//! contract, secrets, tools, the project config and docs, this workspace's claims, Linux
//! logind and sandbox reachability. Each problem comes with its fix and who acts on it.
//!
//! `--fix` runs only the safe fixes, in check order, each once, in-process (never through a
//! shell): installing the service (`Automatic(Maintain)` or `Automatic(Replace)`, never a dev
//! build), starting it, restarting an older one (`Automatic(Replace)`), and rewriting the
//! hostler block in the agent docs. Every stop goes probe → `may_stop` → the atomic
//! `service.shutdown { if_idle: true, refuse_if_unlocked: true }` (plan P4 R2-2); a refusal is
//! recorded, never forced. Each fix is followed by a re-check, then the whole report re-runs.
//!
//! Doctor never reads an env file (R1-4) and never registers a checkout (reads only).

mod block;
pub mod checks;
mod files;
pub mod fix;
mod logind;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::contract::doctor::{CheckStatus, DoctorReport, FixOutcome, FixResult};
use crate::contract::{ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::service::lifecycle::AutoPurpose;
use crate::service::manager::{self, OsManager, Registrar};

/// What `hostler doctor` was asked to do.
#[derive(Clone, Debug)]
pub struct DoctorOptions {
    /// Where doctor runs; inside a checkout the project checks run too.
    pub cwd: PathBuf,
    /// `--fix`: run the safe fixes, then re-check.
    pub fix: bool,
}

/// What doctor checks against and acts through: the OS in production, fakes in tests (a test
/// registrar, a non-dev exe), so no test touches the machine's real registration.
#[derive(Clone)]
pub struct DoctorEnv {
    pub paths: Paths,
    /// The `PermissionDenied` (`sandbox_denied`) `client::resolve_paths` met, if any: the
    /// service is then reported unreachable without a connect (`paths` is the planned set).
    pub paths_error: Option<HostlerError>,
    pub registrar: Arc<dyn Registrar>,
    /// The hostler binary a fix registers or starts (`current_exe` in production).
    pub exe: PathBuf,
    /// Looks up the binary the OS registration runs (`manager::registered_exe`); doctor calls
    /// it on a bounded worker inside its deadline (it may run `schtasks` on Windows).
    pub registered_exe: ExeLookup,
    /// D-Q2: `manager::is_dev_build(exe)`; an automatic install never registers a dev build.
    pub dev_build: bool,
}

impl DoctorEnv {
    /// The real machine: `HOSTLER_HOME` or the default home, the OS service manager, this
    /// binary. A home that cannot be resolved (other than a sandbox denial) is an error (exit
    /// 78).
    pub fn os() -> Result<DoctorEnv, HostlerError> {
        let (paths, paths_error) = match crate::client::resolve_paths() {
            Ok(paths) => (paths, None),
            Err(e) if crate::client::is_sandbox_denied(&e) => {
                let planned = Paths::planned().map_err(setup_failed)?;
                (planned, Some(e))
            }
            Err(e) => return Err(e),
        };
        let exe = std::env::current_exe().map_err(|e| setup_failed(e.into()))?;
        let registered_exe: ExeLookup = Arc::new(manager::registered_exe);
        let dev_build = manager::is_dev_build(&exe);
        Ok(DoctorEnv {
            paths,
            paths_error,
            registrar: Arc::new(OsManager),
            exe,
            registered_exe,
            dev_build,
        })
    }
}

/// A registration lookup doctor runs on a bounded worker.
pub type ExeLookup = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

/// The whole command, `--fix` included (review t6-r1 R1-1). The plan names no separate
/// allowance for `--fix`, so the fixes share the 20 s.
pub const COMMAND_BOUND: Duration = checks::CHECKS_BOUND;

/// Kept back from the fixes for the re-check that follows the last one.
const RECHECK_RESERVE: Duration = Duration::from_secs(4);

/// The time a fix that changes the service or its registration (install, start, restart) needs
/// left on the deadline to start (review t6-r2 R2-1); with less it is `refused`. Once started it
/// is never abandoned: it runs to its own bounded end, so doctor exceeds `COMMAND_BOUND` only by
/// that change in flight:
/// - install: the quiesce (at most 15 s, `QUIESCE_WAIT`), the OS registration (each
///   `launchctl`/`systemctl`/`schtasks` call killed after `MANAGER_CALL_TIMEOUT`, 10 s; at most
///   three per change), and the start (at most 5 s, plus one bounded manager call);
/// - start: the replacement stop and wait (at most 15 s, `REPLACE_WAIT`) and the start (5 s);
/// - restart: the stop (one call, at most 5 s), the wait for the old service (until the fix
///   window closes, `deadline − RECHECK_RESERVE`), and the start (5 s).
const MUTATING_BUDGET: Duration = Duration::from_secs(8);

/// Runs `lookup` on its own thread and waits at most `limit` (R1-4). The thread is detached: a
/// lookup stuck in the OS (a hung `schtasks`) never holds doctor, nor its exit.
pub(crate) async fn bounded_blocking<T: Send + 'static>(
    limit: Duration,
    lookup: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("hostler-doctor-lookup".into())
        .spawn(move || {
            let _ = tx.send(lookup());
        })
        .ok()?;
    tokio::time::timeout(limit, rx).await.ok()?.ok()
}

/// The note on a fix the deadline cut short or never let run.
pub(crate) fn out_of_time(what: &str) -> String {
    format!("doctor ran out of time ({COMMAND_BOUND:?}): {what}")
}

fn setup_failed(e: anyhow::Error) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: format!("doctor cannot run: {e:#}"),
        details: serde_json::json!({}),
        recovery: None,
    }
}

/// Every check id doctor reports, in report order; `<…>` stands for a part that varies (Task 8's
/// guide topic `doctor` lists them all).
pub const CHECK_IDS: &[&str] = &[
    "service.registered",
    "service.reachable",
    "service.contract",
    "service.binary",
    "service.linux_session",
    "secrets.database",
    "project.config",
    "project.config.<line>",
    "project.approved",
    "project.docs",
    "project.device_rules",
    "secrets.required.<NAME>",
    "accounts.required.<NAME>",
    "tools.adb",
    "tools.emulator",
    "tools.xcrun",
    "tools.ideviceinfo",
    "tools.agent_device",
    "tools.problem.<n>",
    "claims.workspace",
    "workspace.registered",
];

/// What `--fix` runs for a check whose fix is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixAction {
    /// `install_service(…, Automatic(purpose))`; refused for a dev build (D-Q2).
    Install(AutoPurpose),
    /// `connect_or_start` with `Replace::OnMismatch`.
    Start,
    /// probe → `may_stop(Automatic(Replace))` → atomic stop, then `connect_or_start`.
    Restart,
    /// The hostler block in the agent docs, and nothing else.
    DocsBlock,
}

/// Runs doctor on this machine.
pub async fn run(opts: DoctorOptions) -> Result<DoctorReport, HostlerError> {
    // Path resolution and `current_exe` only: the slow registration lookups run inside the
    // deadline (R1-4).
    let env = DoctorEnv::os()?;
    Ok(run_with(opts, &env).await)
}

/// Whether a fix may start at `now` (R2-1): one that changes the service or its registration
/// needs `MUTATING_BUDGET` left; the docs block needs only the fix window.
fn may_start(action: FixAction, now: Instant, deadline: Instant) -> bool {
    match action {
        FixAction::DocsBlock => now + RECHECK_RESERVE < deadline,
        FixAction::Install(_) | FixAction::Start | FixAction::Restart => {
            deadline.saturating_duration_since(now) >= MUTATING_BUDGET
        }
    }
}

fn failing(status: CheckStatus) -> bool {
    matches!(status, CheckStatus::Problem | CheckStatus::Warn)
}

/// Runs every check against `env` (and, with `fix`, the safe fixes, each followed by a
/// re-check, which is the report). One deadline, `COMMAND_BOUND` from now, bounds every pass,
/// wait and lookup (R1-1); a fix starts only while time is left for it (`MUTATING_BUDGET`) and is
/// otherwise `refused` with a "doctor ran out of time" note.
pub async fn run_with(opts: DoctorOptions, env: &DoctorEnv) -> DoctorReport {
    let deadline = Instant::now() + COMMAND_BOUND;
    let fixes_until = deadline - RECHECK_RESERVE;
    let mut current = checks::gather(env, &opts.cwd, deadline).await;
    let mut fixed = Vec::new();
    if opts.fix {
        let planned: Vec<String> = current
            .iter()
            .filter(|found| found.action.is_some())
            .map(|found| found.check.id.clone())
            .collect();
        for id in planned {
            // The state now: an earlier fix may have resolved this one, or changed its fix.
            let Some(found) = current.iter().find(|found| found.check.id == id) else {
                continue;
            };
            if !failing(found.check.status) {
                continue;
            }
            let Some(action) = found.action else {
                continue;
            };
            let command = found
                .check
                .fix
                .as_ref()
                .and_then(|fix| fix.command.clone())
                .unwrap_or_default();
            // R2-1 (review t6-r2): a fix that changes the service or its registration is never
            // cut off mid-way. It starts only with `MUTATING_BUDGET` left, then runs to its own
            // bounded end (see `MUTATING_BUDGET`); the docs block needs only the fix window.
            if !may_start(action, Instant::now(), deadline) {
                fixed.push(FixOutcome {
                    check: id,
                    command,
                    result: FixResult::Refused,
                    message: out_of_time("not started"),
                });
                continue;
            }
            let ran = fix::apply(env, &opts.cwd, action, fixes_until).await;
            current = checks::gather(env, &opts.cwd, deadline).await;
            let after = current.iter().find(|found| found.check.id == id);
            let passed = after.is_none_or(|found| !failing(found.check.status));
            let (result, message) = match ran {
                fix::Applied::Refused(message) => (FixResult::Refused, message),
                fix::Applied::Ran(message) if passed => (FixResult::Fixed, message),
                fix::Applied::Ran(message) => (FixResult::StillFailing, message),
            };
            fixed.push(FixOutcome {
                check: id,
                command,
                result,
                message,
            });
        }
    }
    DoctorReport {
        checks: current.into_iter().map(|found| found.check).collect(),
        fixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R1-4: a lookup that hangs is abandoned at its bound.
    #[tokio::test]
    async fn a_slow_lookup_is_bounded() {
        let started = Instant::now();
        let found = bounded_blocking(Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_secs(5));
            Some(PathBuf::from("/slow/hostler"))
        })
        .await;
        assert_eq!(found, None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let quick = bounded_blocking(Duration::from_secs(2), || 7).await;
        assert_eq!(quick, Some(7));
    }

    /// R2-1: a change to the service or its registration starts only with its budget left.
    #[test]
    fn mutating_fixes_need_their_budget() {
        let now = Instant::now();
        let at = |left: u64| now + Duration::from_secs(left);
        for action in [
            FixAction::Install(AutoPurpose::Maintain),
            FixAction::Install(AutoPurpose::Replace),
            FixAction::Start,
            FixAction::Restart,
        ] {
            assert!(may_start(action, now, at(20)), "{action:?}");
            assert!(may_start(action, now, at(8)), "{action:?}");
            assert!(!may_start(action, now, at(7)), "{action:?}");
        }
        assert!(may_start(FixAction::DocsBlock, now, at(5)));
        assert!(!may_start(FixAction::DocsBlock, now, at(4)));
    }

    #[test]
    fn out_of_time_names_the_bound() {
        assert_eq!(
            out_of_time("not run"),
            "doctor ran out of time (20s): not run"
        );
    }
}
