//! The one stop procedure (P5 Task 6, spec §11.1): moved from `cli/takeover.rs` with no
//! behaviour change, so the CLI's takeovers and the service's `heavy.stop` share it. A stop
//! checks each recorded identity (pid, boot, start time) before it signals, terminates a Unix
//! process group (SIGTERM, SIGKILL after a grace) or a Windows job, and waits for it to be gone.
//!
//! Limits (P1): processes that escaped their run's group (`setsid`, a Windows breakaway job) are
//! not tracked. On Unix a group with members of another user ends as `StillAlive` after SIGKILL
//! reached only some of them.

use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::contract::claims::{StopEvidence, StopTarget};
use crate::procs::{self, Liveness, ProcessIdentity, ProcessTable, RunGroup, SysProcessTable};

/// The SIGTERM grace before SIGKILL (Unix).
pub const TAKEOVER_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopError {
    PermissionDenied(String),
    /// Still running after the last resort (SIGKILL, TerminateJobObject) and its wait.
    StillAlive,
    /// The target's state cannot be known from here (another Windows logon session).
    Unknown(String),
    Unsupported,
    /// P5 T6-R1-2: right before a signal, the target was no longer the recorded process or
    /// group (a reused pid, a group whose leader changed); it was not signalled.
    Changed(String),
}

impl StopError {
    /// Why `target` (as `describe` names it) could not be stopped.
    pub fn reason(&self, target: &str) -> String {
        match self {
            StopError::PermissionDenied(why) => format!("hostler may not stop {target}: {why}"),
            StopError::StillAlive => format!("{target} still runs after it was killed"),
            StopError::Unknown(why) => format!("{target}: {why}"),
            StopError::Unsupported => "hostler cannot stop this holder".into(),
            StopError::Changed(why) => format!("{target}: {why}"),
        }
    }
}

/// Stops one kind of holder. P3a ships `RunGroupStopper`; P3b appends a device session stopper.
pub trait HolderStopper: Send + Sync {
    fn handles(&self, t: &StopTarget) -> bool;
    /// Evidence that `t` is gone. Its `target` index is set by `take_over`.
    fn stop(&self, t: &StopTarget, grace: Duration) -> Result<StopEvidence, StopError>;
    /// `stop`, giving up at its next step once `cancel` is set: an interrupted takeover never
    /// escalates (T6-R3-2). By default the stop runs to its end.
    fn stop_unless(
        &self,
        t: &StopTarget,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<StopEvidence, StopError> {
        let _ = cancel;
        self.stop(t, grace)
    }
    /// `stop_unless`, and whether it signalled anything: `false` when the target had already
    /// ended, so the takeover never counts it as stopped. By default every stop signalled.
    fn stop_counted(
        &self,
        t: &StopTarget,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        self.stop_unless(t, grace, cancel).map(|e| (e, true))
    }
}

/// `RunGroup` targets (a run's group or job, or its wrapper before it spawned) and `Process`
/// targets (a process-bound hold's process).
pub struct RunGroupStopper;

pub(crate) fn gone() -> StopEvidence {
    StopEvidence::Gone { target: 0 }
}

impl HolderStopper for RunGroupStopper {
    fn handles(&self, t: &StopTarget) -> bool {
        matches!(t, StopTarget::RunGroup { .. } | StopTarget::Process { .. })
    }

    fn stop(&self, t: &StopTarget, grace: Duration) -> Result<StopEvidence, StopError> {
        self.stop_unless(t, grace, &AtomicBool::new(false))
    }

    fn stop_unless(
        &self,
        t: &StopTarget,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<StopEvidence, StopError> {
        self.stop_counted(t, grace, cancel).map(|(e, _)| e)
    }

    fn stop_counted(
        &self,
        t: &StopTarget,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        #[cfg(debug_assertions)]
        if let Some(e) = test_hooks::stopper_fails(t) {
            return Err(e);
        }
        let table = SysProcessTable::new()
            .map_err(|e| StopError::Unknown(format!("cannot read the process table: {e:#}")))?;
        match t {
            StopTarget::RunGroup {
                group: Some(group),
                child,
                ..
            } => stop_group(group, child.as_ref(), &table, grace, cancel),
            StopTarget::RunGroup { holder, child, .. } => {
                stop_process(child.as_ref().unwrap_or(holder), &table, grace, cancel)
            }
            StopTarget::Process { process, .. } => stop_process(process, &table, grace, cancel),
            StopTarget::Foreign { .. } => Err(StopError::Unsupported),
        }
    }
}

fn dead(p: &ProcessIdentity, table: &dyn ProcessTable) -> bool {
    procs::liveness(p, table) != Liveness::Alive
}

/// Waits up to `limit`, polling every 50 ms, for `p` to be gone. A terminated job reads empty
/// before its processes have exited (measured on Windows 11: `ActiveProcesses` is 0 within
/// microseconds, the process object is signalled about 200 ms later), so "gone" is said only
/// once the run's own process is.
#[cfg_attr(not(windows), allow(dead_code))]
fn wait_gone(p: &ProcessIdentity, table: &dyn ProcessTable, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if dead(p, table) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
mod os {
    use std::time::Instant;

    use super::*;

    /// How long a killed group or process gets to be gone (Windows: `procs::terminate_*`).
    const KILL_WAIT: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(100);

    /// Polls `done` every 100 ms for up to `limit`.
    fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// The result of one signal: delivered, nobody there, or not permitted.
    pub(super) enum Sent {
        Delivered,
        Gone,
        Denied,
    }

    /// Sends one signal: `(to_group, pid, signal)`. Tests pass their own.
    pub(super) type Sender<'a> = &'a dyn Fn(bool, u32, libc::c_int) -> Sent;

    pub(super) fn send(to_group: bool, pid: u32, sig: libc::c_int) -> Sent {
        let pid = pid as libc::pid_t;
        // SAFETY: kill/killpg have no memory preconditions.
        let rc = unsafe {
            if to_group {
                libc::killpg(pid, sig)
            } else {
                libc::kill(pid, sig)
            }
        };
        if rc == 0 {
            return Sent::Delivered;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => Sent::Denied,
            _ => Sent::Gone,
        }
    }

    /// SIGTERM, `grace`, SIGKILL, 5 s; `done` tells whether the target is gone, `ours`
    /// whether it is still the recorded process or group. Both are asked again right before
    /// every signal, SIGKILL included (P5 T6-R1-2): a changed target is never signalled, and
    /// the stop fails with `Changed`. A small window between that check and the signal remains
    /// (no pidfd here; P6 hardening).
    fn term_then_kill(
        to_group: bool,
        pid: u32,
        grace: Duration,
        done: &dyn Fn() -> bool,
        ours: &dyn Fn() -> bool,
        cancel: &AtomicBool,
        send: Sender<'_>,
    ) -> Result<(StopEvidence, bool), StopError> {
        let what = if to_group { "process group" } else { "process" };
        let mut signalled = false;
        for (sig, wait) in [(libc::SIGTERM, grace), (libc::SIGKILL, KILL_WAIT)] {
            // Interrupted (T6-R3-2): no further signal.
            if cancel.load(Ordering::SeqCst) {
                return Err(StopError::Unknown("the takeover was interrupted".into()));
            }
            if done() {
                return Ok((gone(), signalled));
            }
            if !ours() {
                return Err(StopError::Changed(format!(
                    "{what} {pid} is no longer the recorded one; it was not signalled{}",
                    if signalled { " again" } else { "" }
                )));
            }
            match send(to_group, pid, sig) {
                Sent::Denied => {
                    return Err(StopError::PermissionDenied(format!(
                        "not permitted to signal {what} {pid}"
                    )));
                }
                Sent::Gone => return Ok((gone(), signalled)),
                Sent::Delivered => signalled = true,
            }
            if wait_until(wait, || done() || cancel.load(Ordering::SeqCst)) && done() {
                return Ok((gone(), true));
            }
        }
        Err(StopError::StillAlive)
    }

    /// Whether the group is the run's: its recorded command leads it (`pgid == child.pid`;
    /// a pgid without a recorded leader is never ours, P5 T6-R1-1), and that leader either still
    /// lives with its recorded identity or is dead with its pid not taken by another process
    /// (T6-5: the kernel frees a pid only once no group uses it, so a live process there with
    /// another start time means the pgid was reused and the members are a stranger's).
    pub(super) fn group_is_ours(
        group: &RunGroup,
        child: Option<&ProcessIdentity>,
        table: &dyn ProcessTable,
    ) -> bool {
        let (RunGroup::UnixGroup { pgid, .. }, Some(child)) = (group, child) else {
            return false;
        };
        if child.pid != *pgid {
            return false;
        }
        if !dead(child, table) {
            return true;
        }
        !matches!(table.info(*pgid), Some(info)
            if !info.zombie && info.start_time.is_some_and(|t| t != child.start_time))
    }

    pub(super) fn stop_group(
        group: &RunGroup,
        child: Option<&ProcessIdentity>,
        table: &dyn ProcessTable,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        stop_group_with(group, child, table, grace, cancel, &send)
    }

    /// `stop_group` with this sender.
    pub(super) fn stop_group_with(
        group: &RunGroup,
        child: Option<&ProcessIdentity>,
        table: &dyn ProcessTable,
        grace: Duration,
        cancel: &AtomicBool,
        send: Sender<'_>,
    ) -> Result<(StopEvidence, bool), StopError> {
        let RunGroup::UnixGroup { pgid, .. } = group else {
            // A group of the other OS family cannot exist on this machine.
            return Ok((gone(), false));
        };
        // A gone group is evidence; no signal goes to a pgid that may be reused, nor to one
        // the run's recorded command does not lead (the verification then decides).
        let done = || !procs::group_liveness(group, table);
        let ours = || group_is_ours(group, child, table);
        if done() || !ours() {
            return Ok((gone(), false));
        }
        term_then_kill(true, *pgid, grace, &done, &ours, cancel, send)
    }

    pub(super) fn stop_process(
        p: &ProcessIdentity,
        table: &dyn ProcessTable,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        stop_process_with(p, table, grace, cancel, &send)
    }

    /// `stop_process` with this sender. Alive by its identity (pid, boot, start time) is the
    /// recorded process; anything else at its pid is not, and is never signalled.
    pub(super) fn stop_process_with(
        p: &ProcessIdentity,
        table: &dyn ProcessTable,
        grace: Duration,
        cancel: &AtomicBool,
        send: Sender<'_>,
    ) -> Result<(StopEvidence, bool), StopError> {
        let done = || dead(p, table);
        if done() {
            return Ok((gone(), false));
        }
        term_then_kill(
            false,
            p.pid,
            grace,
            &done,
            &|| !dead(p, table),
            cancel,
            send,
        )
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use crate::procs::KillOutcome;

    /// Gone evidence for a job seen in `session`'s namespace (`None`: no job evidence). When
    /// the service cannot open the job itself, it attests it only if this is the job's recorded
    /// session (L23, L30).
    fn job_gone(session: Option<u32>) -> StopEvidence {
        match session {
            Some(session) => StopEvidence::JobGone { target: 0, session },
            None => gone(),
        }
    }

    /// How long a terminated run's own process gets to exit (as on Unix).
    const KILL_WAIT: Duration = Duration::from_secs(5);

    /// No graceful phase on Windows, so nothing for `_cancel` to hold back.
    pub(super) fn stop_group(
        group: &RunGroup,
        child: Option<&ProcessIdentity>,
        table: &dyn ProcessTable,
        _grace: Duration,
        _cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        stop_group_from(group, child, table, procs::current_session())
    }

    /// `stop_group` as seen from logon session `own`.
    pub(super) fn stop_group_from(
        group: &RunGroup,
        child: Option<&ProcessIdentity>,
        table: &dyn ProcessTable,
        own: Option<u32>,
    ) -> Result<(StopEvidence, bool), StopError> {
        let RunGroup::WindowsJob { name, session, .. } = group else {
            return Ok((gone(), false));
        };
        // Another logon session's job opens by its `Global\` / `Session\<n>\` name; it stays
        // there only when that open is denied, or finds nothing while the launcher lives (P2-11).
        let elsewhere = matches!((*session, own), (Some(s), Some(o)) if s != o);
        let stop_it_there = || {
            StopError::Unknown(format!(
                "the run is in Windows logon session {}; stop it there",
                session.unwrap_or_default()
            ))
        };
        // No graceful phase: a console Ctrl+C cannot be sent to another console's job.
        match procs::terminate_job_from(name, *session, own) {
            // The job reads empty before its processes have exited: the run's own process
            // must be gone too before the taker commits (galactica, W-12). It was seen in its
            // recorded session's namespace, so the evidence says that session (P3bT10-2).
            KillOutcome::Gone => match child {
                Some(child) if !wait_gone(child, table, KILL_WAIT) => Err(StopError::StillAlive),
                _ => Ok((job_gone(session.or(own)), true)),
            },
            // Nothing was terminated: the name went with its last handle (L31). The evidence
            // keeps this session, so a job not found elsewhere is never attested.
            KillOutcome::NotFound if !elsewhere => match child {
                Some(child) if !wait_gone(child, table, KILL_WAIT) => Err(StopError::StillAlive),
                _ => Ok((job_gone(own), false)),
            },
            KillOutcome::NotFound if child.is_some_and(|c| dead(c, table)) => {
                Ok((job_gone(own), false))
            }
            KillOutcome::NotFound => Err(stop_it_there()),
            KillOutcome::AccessDenied if elsewhere => Err(stop_it_there()),
            KillOutcome::AccessDenied => Err(StopError::PermissionDenied(format!(
                "access to the job object {name} is denied"
            ))),
            KillOutcome::StillAlive => Err(StopError::StillAlive),
            KillOutcome::Failed(code) => Err(StopError::Unknown(format!(
                "cannot open the job object {name} (error {code})"
            ))),
        }
    }

    pub(super) fn stop_process(
        p: &ProcessIdentity,
        table: &dyn ProcessTable,
        _grace: Duration,
        _cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        if dead(p, table) {
            return Ok((gone(), false));
        }
        // T6-5: the identity is checked again on the handle that terminates it.
        match procs::terminate_process(p) {
            KillOutcome::Gone => Ok((gone(), true)),
            // A job's answer only: never read as gone (P3bT10-3).
            KillOutcome::NotFound => Err(StopError::Unknown(format!(
                "unexpected not-found for process {}",
                p.pid
            ))),
            KillOutcome::AccessDenied => Err(StopError::PermissionDenied(format!(
                "access to process {} is denied",
                p.pid
            ))),
            KillOutcome::StillAlive => Err(StopError::StillAlive),
            KillOutcome::Failed(code) => Err(StopError::Unknown(format!(
                "cannot open process {} (error {code})",
                p.pid
            ))),
        }
    }
}

use os::{stop_group, stop_process};

/// The stoppers `take_over` tries in order. P3a: `RunGroupStopper` (and, in debug builds, the
/// fake foreign source's stopper under `HOSTLER_TEST_FOREIGN_STOPPER`); P3b appends its own.
pub fn stoppers() -> Vec<Box<dyn HolderStopper>> {
    #[allow(unused_mut)]
    let mut out: Vec<Box<dyn HolderStopper>> = vec![
        Box::new(RunGroupStopper),
        Box::new(crate::devices::agent_device::AgentDeviceStopper),
    ];
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("HOSTLER_TEST_FOREIGN_STOPPER") {
        out.push(Box::new(test_hooks::FakeForeignStopper(path.into())));
    }
    out
}

/// The SIGTERM grace a takeover uses: `TAKEOVER_GRACE`, or `HOSTLER_TEST_TAKEOVER_GRACE_MS` in
/// debug builds.
pub fn takeover_grace() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = test_hooks::ms("HOSTLER_TEST_TAKEOVER_GRACE_MS") {
        return Duration::from_millis(ms);
    }
    TAKEOVER_GRACE
}

/// The pids a stop target names: a run's holder and child, a hold's process.
pub(crate) fn target_pids(t: &StopTarget) -> Vec<u32> {
    match t {
        StopTarget::RunGroup { holder, child, .. } => std::iter::once(holder.pid)
            .chain(child.iter().map(|c| c.pid))
            .collect(),
        StopTarget::Process { process, .. } => vec![process.pid],
        StopTarget::Foreign { .. } => Vec::new(),
    }
}

/// R2-9 (spec §11.1): whether stopping `t` could reach the hostler service, whose pid is
/// `service`: its holder, child or process is the service or one of the service's ancestors, or
/// its group is the service's own (Unix: the service's pgid, or a group the service leads;
/// Windows: the service is in its job). A process table that cannot be read refuses, since the
/// ancestors cannot be ruled out. Checked before anything is marked or signalled, by the
/// service's `heavy.stop` and by the CLI's takeovers.
pub fn refuses_self_target(service: u32, t: &StopTarget) -> bool {
    let Ok(table) = SysProcessTable::new() else {
        return true;
    };
    refuses_self_target_with(service, t, &procs::ancestors(service, &table), &|group| {
        procs::in_group(service, group)
    })
}

/// `refuses_self_target` with the service's ancestors and its group membership given.
pub fn refuses_self_target_with(
    service: u32,
    t: &StopTarget,
    service_ancestors: &[u32],
    service_in: &dyn Fn(&RunGroup) -> bool,
) -> bool {
    let pids = target_pids(t);
    if pids
        .iter()
        .any(|pid| *pid == service || service_ancestors.contains(pid))
    {
        return true;
    }
    match t {
        StopTarget::RunGroup {
            group: Some(group), ..
        } => {
            matches!(group, RunGroup::UnixGroup { pgid, .. } if *pgid == service)
                || service_in(group)
        }
        _ => false,
    }
}

/// How long a run's holder, outside its run's group, gets to exit by itself once its command is
/// gone (`hostler run` releases its slot, then exits).
pub const HOLDER_WAIT: Duration = Duration::from_secs(5);

/// The service's own stop of one run (spec §11.1): what it did and what it found after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStop {
    /// A signal reached the run (or its job was terminated); `false` when it had already ended.
    pub signalled: bool,
    /// Verified: the holder, the child and the group are all gone.
    pub gone: bool,
    /// What still runs when not `gone`: live holder and child pids, and a live group's pgid.
    pub alive: Vec<u32>,
    /// Why the stop itself failed, if it did.
    pub error: Option<StopError>,
}

impl RunStop {
    /// A stop that could not run at all (`t`'s pids are reported as still running).
    pub fn failed(t: &StopTarget, error: StopError) -> RunStop {
        RunStop {
            signalled: false,
            gone: false,
            alive: target_pids(t),
            error: Some(error),
        }
    }
}

/// Whether nothing of run `t` is alive: its holder, its child and its group are gone (a Windows
/// job of another session that is not found counts once the holder and child are dead, P2-11).
/// Only `RunGroup` targets are runs.
pub fn run_gone(t: &StopTarget, table: &dyn ProcessTable) -> bool {
    let StopTarget::RunGroup {
        holder,
        child,
        group,
        ..
    } = t
    else {
        return false;
    };
    dead(holder, table)
        && child.as_ref().is_none_or(|c| dead(c, table))
        && group.as_ref().is_none_or(|g| {
            matches!(
                procs::group_state(g, table),
                procs::JobState::Gone | procs::JobState::NotFound
            )
        })
}

/// What of run `t` still runs: its live holder and child, and its live Unix group's pgid.
pub fn alive_pids(t: &StopTarget, table: &dyn ProcessTable) -> Vec<u32> {
    let StopTarget::RunGroup {
        holder,
        child,
        group,
        ..
    } = t
    else {
        return Vec::new();
    };
    let mut alive: Vec<u32> = std::iter::once(holder)
        .chain(child.as_ref())
        .filter(|p| !dead(p, table))
        .map(|p| p.pid)
        .collect();
    if let Some(group @ RunGroup::UnixGroup { pgid, .. }) = group
        && procs::group_state(group, table) == procs::JobState::Alive
        && !alive.contains(pgid)
    {
        alive.push(*pgid);
    }
    alive
}

/// The service's stop of run `t` (spec §11.1): `RunGroupStopper` checks each identity before it
/// signals (a reused pid is never signalled), terminates the group (SIGTERM, SIGKILL after
/// `grace`; on Windows, its job) or, before the command started, the holder; then the holder,
/// the child and the group must all be gone, the holder given `HOLDER_WAIT` to exit by itself.
pub fn stop_run(t: &StopTarget, grace: Duration) -> RunStop {
    let table = match SysProcessTable::new() {
        Ok(table) => table,
        Err(e) => {
            return RunStop::failed(
                t,
                StopError::Unknown(format!("cannot read the process table: {e:#}")),
            );
        }
    };
    stop_run_with(t, grace, &RunGroupStopper, &table, HOLDER_WAIT)
}

/// `stop_run` with this stopper, process table and holder wait.
pub fn stop_run_with(
    t: &StopTarget,
    grace: Duration,
    stopper: &dyn HolderStopper,
    table: &dyn ProcessTable,
    holder_wait: Duration,
) -> RunStop {
    let (signalled, error) = match stopper.stop_counted(t, grace, &AtomicBool::new(false)) {
        Ok((_, signalled)) => (signalled, None),
        Err(e) => (false, Some(e)),
    };
    let limit = if error.is_none() {
        holder_wait
    } else {
        Duration::ZERO
    };
    let deadline = std::time::Instant::now() + limit;
    let gone = loop {
        if run_gone(t, table) {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    RunStop {
        signalled,
        gone,
        alive: if gone {
            Vec::new()
        } else {
            alive_pids(t, table)
        },
        error,
    }
}

/// Debug builds: `HOSTLER_TEST_PAUSE_AT=<point>:<file>` holds the caller at `<point>` while
/// `<file>` exists (up to 60 s), after writing `<file>.reached` (the secrets engine's hook,
/// P5 Task 4). `heavy_stop`: after the entry is marked `stopping`, before anything is
/// signalled.
pub fn pause_at(point: &str) {
    #[cfg(debug_assertions)]
    {
        let Some(spec) = std::env::var_os("HOSTLER_TEST_PAUSE_AT") else {
            return;
        };
        let spec = spec.to_string_lossy();
        let Some((at, file)) = spec.split_once(':') else {
            return;
        };
        if at != point {
            return;
        }
        let file = std::path::PathBuf::from(file);
        let mut reached = file.clone().into_os_string();
        reached.push(".reached");
        let _ = std::fs::write(&reached, b"");
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while file.exists() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = point;
}

#[cfg(debug_assertions)]
pub(crate) mod test_hooks {
    use std::path::PathBuf;

    use super::*;
    use crate::contract::claims::ForeignClaim;

    pub(crate) fn ms(name: &str) -> Option<u64> {
        std::env::var(name).ok()?.parse().ok()
    }

    /// `HOSTLER_TEST_STOPPER_FAILS=1` fails every target; `=<pid>` only the one whose holder,
    /// child or process has that pid.
    pub(super) fn stopper_fails(t: &StopTarget) -> Option<StopError> {
        fails_for(&std::env::var("HOSTLER_TEST_STOPPER_FAILS").ok()?, t)
    }

    pub(super) fn fails_for(value: &str, t: &StopTarget) -> Option<StopError> {
        let pids: Vec<u32> = match t {
            StopTarget::RunGroup { holder, child, .. } => std::iter::once(holder.pid)
                .chain(child.iter().map(|c| c.pid))
                .collect(),
            StopTarget::Process { process, .. } => vec![process.pid],
            StopTarget::Foreign { .. } => Vec::new(),
        };
        let hit = value == "1" || value.parse::<u32>().is_ok_and(|pid| pids.contains(&pid));
        hit.then(|| StopError::PermissionDenied("HOSTLER_TEST_STOPPER_FAILS".into()))
    }

    /// Stops `Foreign { source: "fake" }` by removing that claim from the fake source's file.
    pub(super) struct FakeForeignStopper(pub PathBuf);

    impl HolderStopper for FakeForeignStopper {
        fn handles(&self, t: &StopTarget) -> bool {
            matches!(t, StopTarget::Foreign { source, .. } if source == "fake")
        }

        fn stop(&self, t: &StopTarget, _grace: Duration) -> Result<StopEvidence, StopError> {
            let StopTarget::Foreign { resource, .. } = t else {
                return Err(StopError::Unsupported);
            };
            let fail = |e: &dyn std::fmt::Display| StopError::Unknown(e.to_string());
            let bytes = std::fs::read(&self.0).map_err(|e| fail(&e))?;
            let mut claims: Vec<ForeignClaim> =
                serde_json::from_slice(&bytes).map_err(|e| fail(&e))?;
            claims.retain(|c| !(c.resource == *resource && c.source == "fake"));
            let out = serde_json::to_vec(&claims).map_err(|e| fail(&e))?;
            std::fs::write(&self.0, out).map_err(|e| fail(&e))?;
            Ok(gone())
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Instant;

    use super::*;
    use crate::contract::claims::ResourceId;

    fn me() -> ProcessIdentity {
        procs::current_identity(&SysProcessTable::new().unwrap()).unwrap()
    }

    fn run(holder: u32, child: Option<u32>) -> StopTarget {
        let id = |pid| ProcessIdentity {
            pid,
            boot_id: "b".into(),
            start_time: 1,
        };
        StopTarget::RunGroup {
            claim_id: "c1".into(),
            holder: id(holder),
            child: child.map(id),
            group: None,
        }
    }

    #[test]
    fn stopper_fails_hook_matches_every_target_or_one_pid() {
        use test_hooks::fails_for;
        let failing = Some(StopError::PermissionDenied(
            "HOSTLER_TEST_STOPPER_FAILS".into(),
        ));
        assert_eq!(fails_for("1", &run(10, None)), failing);
        assert_eq!(fails_for("11", &run(10, Some(11))), failing);
        assert_eq!(fails_for("10", &run(10, Some(11))), failing);
        assert_eq!(fails_for("12", &run(10, Some(11))), None);
        assert_eq!(fails_for("no", &run(10, None)), None);
    }

    #[test]
    fn fake_foreign_stopper_removes_only_its_claim() {
        use crate::contract::claims::ForeignClaim;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("foreign.json");
        let claim = |r: &str, source: &str| ForeignClaim {
            resource: ResourceId(r.into()),
            source: source.into(),
            description: "d".into(),
            workspace_path: None,
            since_ms: None,
            stop: Some(serde_json::json!({})),
            workspace_id: None,
        };
        let all = vec![
            claim("device:test:0", "fake"),
            claim("device:test:1", "fake"),
            claim("device:test:0", "other"),
        ];
        std::fs::write(&file, serde_json::to_vec(&all).unwrap()).unwrap();
        let s = test_hooks::FakeForeignStopper(file.clone());
        let target = StopTarget::Foreign {
            source: "fake".into(),
            resource: ResourceId("device:test:0".into()),
            data: serde_json::json!({}),
        };
        assert!(s.handles(&target));
        assert!(!s.handles(&run(1, None)));
        assert_eq!(s.stop(&target, Duration::ZERO).unwrap(), gone());
        let left: Vec<ForeignClaim> =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(left, all[1..].to_vec());
    }

    #[test]
    fn stop_error_reasons_name_the_target() {
        let t = "the run of claim c1 (PID 7)";
        assert_eq!(
            StopError::PermissionDenied("no".into()).reason(t),
            "hostler may not stop the run of claim c1 (PID 7): no"
        );
        assert_eq!(
            StopError::StillAlive.reason(t),
            "the run of claim c1 (PID 7) still runs after it was killed"
        );
        assert_eq!(
            StopError::Unsupported.reason(t),
            "hostler cannot stop this holder"
        );
    }

    #[test]
    fn run_group_stopper_handles_runs_and_processes_only() {
        let s = RunGroupStopper;
        let process = StopTarget::Process {
            claim_id: "c1".into(),
            process: me(),
        };
        let foreign = StopTarget::Foreign {
            source: "fake".into(),
            resource: ResourceId::screen(),
            data: serde_json::json!({}),
        };
        assert!(s.handles(&process));
        assert!(!s.handles(&foreign));
        assert_eq!(
            s.stop(&foreign, Duration::ZERO).unwrap_err(),
            StopError::Unsupported
        );
    }

    #[test]
    fn a_dead_process_is_gone_without_a_signal() {
        let table = SysProcessTable::new().unwrap();
        let mut p = me();
        p.start_time += 1; // not this process any more: a reused pid must not be signalled
        assert_eq!(
            stop_process(&p, &table, Duration::ZERO, &AtomicBool::new(false)).unwrap(),
            (gone(), false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_stops_a_group_and_sigkill_follows_after_grace() {
        use std::os::unix::process::CommandExt;

        let table = SysProcessTable::new().unwrap();
        for (script, grace) in [
            ("sleep 30", Duration::from_secs(10)),
            ("trap '' TERM; sleep 30 & wait", Duration::from_millis(300)),
        ] {
            let mut child = std::process::Command::new("sh")
                .args(["-c", script])
                .process_group(0)
                .spawn()
                .unwrap();
            let group = RunGroup::UnixGroup {
                pgid: child.id(),
                boot_id: table.boot_id(),
            };
            let leader = procs::identity_of(&table, child.id()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            let reaper = std::thread::spawn(move || child.wait());
            let started = Instant::now();
            assert_eq!(
                stop_group(
                    &group,
                    Some(&leader),
                    &table,
                    grace,
                    &AtomicBool::new(false)
                )
                .unwrap(),
                (gone(), true)
            );
            assert!(
                started.elapsed() < grace + Duration::from_secs(3),
                "{script}"
            );
            assert!(!procs::group_liveness(&group, &table));
            reaper.join().unwrap().unwrap();
        }
    }

    /// T6-5: a dead leader whose pid now belongs to a process with another start time means the
    /// pgid was reused: not our group, never signalled.
    #[cfg(unix)]
    #[test]
    fn a_reused_pgid_is_not_our_group() {
        use crate::procs::fake::FakeTable;
        use os::group_is_ours;

        let t = FakeTable::new("b1");
        let leader = ProcessIdentity {
            pid: 4242,
            boot_id: "b1".into(),
            start_time: 100,
        };
        let group = RunGroup::UnixGroup {
            pgid: 4242,
            boot_id: "b1".into(),
        };
        // The leader itself lives: ours.
        t.insert(4242, None, Some(100));
        assert!(group_is_ours(&group, Some(&leader), &t));
        // Another process now has its pid: a stranger's group.
        t.insert(4242, None, Some(999));
        assert!(!group_is_ours(&group, Some(&leader), &t));
        // Nobody has it: survivors of our group (the pid stays taken while they live).
        t.remove(4242);
        assert!(group_is_ours(&group, Some(&leader), &t));
        // P5 T6-R1-1: no recorded leader, or a recorded command that does not lead the group:
        // never ours, whatever runs there.
        t.insert(4242, None, Some(100));
        assert!(!group_is_ours(&group, None, &t));
        let other = ProcessIdentity {
            pid: 4243,
            ..leader.clone()
        };
        t.insert(4243, None, Some(100));
        assert!(!group_is_ours(&group, Some(&other), &t));
        // (`stop_group` is not called here: pgid 4242 may be a real group on this machine.)
    }

    /// P5 T6-R1-2: the recorded leader is checked again right before every signal. Here the
    /// leader is replaced (its pid reused) after SIGTERM while the group lives on: SIGKILL is
    /// never sent, and the stop fails with `Changed`. A fake sender: nothing real is signalled.
    #[cfg(unix)]
    #[test]
    fn identity_is_checked_again_before_sigkill() {
        use std::sync::Mutex;

        use crate::procs::fake::FakeTable;

        let t = FakeTable::new("b1");
        let leader = ProcessIdentity {
            pid: 4242,
            boot_id: "b1".into(),
            start_time: 100,
        };
        let group = RunGroup::UnixGroup {
            pgid: 4242,
            boot_id: "b1".into(),
        };
        t.insert(4242, None, Some(100));
        t.set_group_alive(&group, true);
        let sent = Mutex::new(Vec::new());
        let send = |to_group: bool, pid: u32, sig: libc::c_int| {
            sent.lock().unwrap().push((to_group, pid, sig));
            // The recorded leader exits and its pid is taken by another process at once.
            t.insert(4242, None, Some(999));
            os::Sent::Delivered
        };
        let result = os::stop_group_with(
            &group,
            Some(&leader),
            &t,
            Duration::from_millis(100),
            &AtomicBool::new(false),
            &send,
        );
        assert_eq!(
            result,
            Err(StopError::Changed(
                "process group 4242 is no longer the recorded one; it was not signalled again"
                    .into()
            ))
        );
        assert_eq!(*sent.lock().unwrap(), vec![(true, 4242, libc::SIGTERM)]);

        // A process: its pid reused after SIGTERM is not the recorded process any more; no
        // SIGKILL reaches the stranger, and the recorded one counts as gone.
        let p = ProcessIdentity {
            pid: 4300,
            boot_id: "b1".into(),
            start_time: 430,
        };
        t.insert(4300, None, Some(430));
        sent.lock().unwrap().clear();
        let send = |to_group: bool, pid: u32, sig: libc::c_int| {
            sent.lock().unwrap().push((to_group, pid, sig));
            t.insert(4300, None, Some(999));
            os::Sent::Delivered
        };
        let result = os::stop_process_with(
            &p,
            &t,
            Duration::from_millis(100),
            &AtomicBool::new(false),
            &send,
        );
        assert_eq!(result, Ok((gone(), true)));
        assert_eq!(*sent.lock().unwrap(), vec![(false, 4300, libc::SIGTERM)]);

        // Unchanged, it escalates: SIGTERM, then SIGKILL.
        let p = ProcessIdentity {
            pid: 4400,
            boot_id: "b1".into(),
            start_time: 440,
        };
        t.insert(4400, None, Some(440));
        sent.lock().unwrap().clear();
        let send = |to_group: bool, pid: u32, sig: libc::c_int| {
            sent.lock().unwrap().push((to_group, pid, sig));
            if sig == libc::SIGKILL {
                t.remove(4400);
            }
            os::Sent::Delivered
        };
        let result = os::stop_process_with(
            &p,
            &t,
            Duration::from_millis(100),
            &AtomicBool::new(false),
            &send,
        );
        assert_eq!(result, Ok((gone(), true)));
        assert_eq!(
            *sent.lock().unwrap(),
            vec![(false, 4400, libc::SIGTERM), (false, 4400, libc::SIGKILL)]
        );
    }

    /// W-12: the wait for a terminated run's own process to be gone.
    #[test]
    fn wait_gone_waits_for_the_process_itself() {
        use crate::procs::fake::FakeTable;

        let t = std::sync::Arc::new(FakeTable::new("b1"));
        let p = ProcessIdentity {
            pid: 77,
            boot_id: "b1".into(),
            start_time: 770,
        };
        assert!(wait_gone(&p, t.as_ref(), Duration::ZERO));
        t.insert(77, None, Some(770));
        let started = std::time::Instant::now();
        assert!(!wait_gone(&p, t.as_ref(), Duration::from_millis(120)));
        assert!(started.elapsed() >= Duration::from_millis(120));
        let exiting = t.clone();
        let exit = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            exiting.set_zombie(77, true);
        });
        assert!(wait_gone(&p, t.as_ref(), Duration::from_secs(5)));
        exit.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_gone_group_is_never_signalled() {
        let table = SysProcessTable::new().unwrap();
        // pgid 0 is "the caller's group" to the kernel: never signalled, always gone.
        let group = RunGroup::UnixGroup {
            pgid: 0,
            boot_id: table.boot_id(),
        };
        assert_eq!(
            stop_group(
                &group,
                None,
                &table,
                Duration::ZERO,
                &AtomicBool::new(false)
            )
            .unwrap(),
            (gone(), false)
        );
        let other_boot = RunGroup::UnixGroup {
            pgid: std::process::id(),
            boot_id: "another boot".into(),
        };
        assert_eq!(
            stop_group(
                &other_boot,
                None,
                &table,
                Duration::ZERO,
                &AtomicBool::new(false)
            )
            .unwrap(),
            (gone(), false)
        );
    }

    /// P2-11: another session's job that is not found proves nothing while its launcher lives:
    /// the takeover is refused there; once the launcher is dead too, it is gone (L31), nothing
    /// was signalled, and the evidence keeps the taker's session (P3bT10-2).
    #[cfg(windows)]
    #[test]
    fn other_session_not_found_with_live_launcher_is_unknown() {
        use crate::procs::fake::FakeTable;
        use crate::procs::{JobState, group_state};

        let table = SysProcessTable::new().unwrap();
        let mine = procs::current_session().unwrap();
        let other = mine + 1;
        let group = RunGroup::WindowsJob {
            name: format!(r"Local\hostler-test-missing-{}", uuid::Uuid::new_v4()),
            boot_id: table.boot_id(),
            session: Some(other),
        };
        assert_eq!(group_state(&group, &table), JobState::NotFound);
        let launcher = me();
        let stop = |table: &dyn ProcessTable| {
            stop_group(
                &group,
                Some(&launcher),
                table,
                Duration::ZERO,
                &AtomicBool::new(false),
            )
        };
        assert_eq!(
            stop(&table).unwrap_err(),
            StopError::Unknown(format!(
                "the run is in Windows logon session {other}; stop it there"
            ))
        );
        // The same launcher, dead: a table without it.
        let dead = FakeTable::new(&table.boot_id());
        assert_eq!(
            stop(&dead).unwrap(),
            (
                StopEvidence::JobGone {
                    target: 0,
                    session: mine
                },
                false
            )
        );
    }

    /// P3bT10-2: a job terminated by its session name was seen in its recorded session's
    /// namespace, so the evidence says that session.
    #[cfg(windows)]
    #[test]
    fn a_job_found_by_its_session_name_is_evidenced_in_that_session() {
        let table = SysProcessTable::new().unwrap();
        let mut job = procs::test_job();
        let recorded = procs::current_session().unwrap();
        let group = RunGroup::WindowsJob {
            name: job.name.clone(),
            boot_id: table.boot_id(),
            session: Some(recorded),
        };
        let launcher = procs::identity_of(&table, job.child.id()).unwrap();
        let taker = Some(recorded + 1);
        assert_eq!(
            os::stop_group_from(&group, Some(&launcher), &table, taker).unwrap(),
            (
                StopEvidence::JobGone {
                    target: 0,
                    session: recorded
                },
                true
            )
        );
        assert!(job.child.try_wait().unwrap().is_some());
    }

    fn ident(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: "b1".into(),
            start_time: u64::from(pid) * 10,
        }
    }

    fn heavy_run(holder: u32, child: Option<u32>, pgid: Option<u32>) -> StopTarget {
        StopTarget::RunGroup {
            claim_id: "heavy:h1".into(),
            holder: ident(holder),
            child: child.map(ident),
            group: pgid.map(|pgid| RunGroup::UnixGroup {
                pgid,
                boot_id: "b1".into(),
            }),
        }
    }

    /// R2-9: every way a target can contain the service is refused; an unrelated one is not.
    #[test]
    fn self_target_is_refused_every_way() {
        const SERVICE: u32 = 500;
        let ancestors = [400, 1];
        let nobody = |_: &RunGroup| false;
        let check = |t: &StopTarget, service_in: &dyn Fn(&RunGroup) -> bool| {
            refuses_self_target_with(SERVICE, t, &ancestors, service_in)
        };
        // Unrelated: a run of its own.
        assert!(!check(&heavy_run(10, Some(11), Some(11)), &nobody));
        assert!(!check(&heavy_run(10, None, None), &nobody));
        // The holder or the child is the service.
        assert!(check(&heavy_run(SERVICE, Some(11), Some(11)), &nobody));
        assert!(check(&heavy_run(10, Some(SERVICE), Some(11)), &nobody));
        // The holder or the child is an ancestor of the service.
        assert!(check(&heavy_run(400, Some(11), Some(11)), &nobody));
        assert!(check(&heavy_run(10, Some(400), Some(11)), &nobody));
        // The group is the one the service leads.
        assert!(check(&heavy_run(10, Some(11), Some(SERVICE)), &nobody));
        // The service is a member of the group.
        assert!(check(&heavy_run(10, Some(11), Some(11)), &|_| true));
        // Only the group in question is asked about.
        let only_77 = |g: &RunGroup| matches!(g, RunGroup::UnixGroup { pgid, .. } if *pgid == 77);
        assert!(!check(&heavy_run(10, Some(11), Some(11)), &only_77));
        assert!(check(&heavy_run(10, Some(11), Some(77)), &only_77));
        // A hold's process, and a foreign claim.
        let process = |pid| StopTarget::Process {
            claim_id: "c1".into(),
            process: ident(pid),
        };
        assert!(check(&process(SERVICE), &nobody));
        assert!(check(&process(1), &nobody));
        assert!(!check(&process(12), &nobody));
        let foreign = StopTarget::Foreign {
            source: "fake".into(),
            resource: ResourceId::screen(),
            data: serde_json::json!({}),
        };
        assert!(!check(&foreign, &|_| true));
    }

    /// The real check: this test process stands in for the service.
    #[cfg(unix)]
    #[test]
    fn self_target_reads_the_live_process_table() {
        let me = std::process::id();
        let table = SysProcessTable::new().unwrap();
        // SAFETY: getpgid(0) has no memory preconditions.
        let own_pgid = unsafe { libc::getpgid(0) } as u32;
        let group = |pgid| {
            Some(RunGroup::UnixGroup {
                pgid,
                boot_id: table.boot_id(),
            })
        };
        let parent = procs::ancestors(me, &table)[0];
        let run = |holder: u32, group: Option<RunGroup>| StopTarget::RunGroup {
            claim_id: "heavy:h1".into(),
            holder: ident(holder),
            child: None,
            group,
        };
        assert!(refuses_self_target(me, &run(parent, None)));
        assert!(refuses_self_target(me, &run(u32::MAX - 1, group(own_pgid))));
        assert!(!refuses_self_target(me, &run(u32::MAX - 1, None)));
    }

    #[test]
    fn run_gone_needs_holder_child_and_group_gone() {
        use crate::procs::fake::FakeTable;

        let t = FakeTable::new("b1");
        let run = heavy_run(10, Some(11), Some(11));
        let group = RunGroup::UnixGroup {
            pgid: 11,
            boot_id: "b1".into(),
        };
        t.insert(10, None, Some(100));
        t.insert(11, Some(10), Some(110));
        t.set_group_alive(&group, true);
        assert!(!run_gone(&run, &t));
        assert_eq!(alive_pids(&run, &t), vec![10, 11]);
        // The child is gone, a member of its group still runs.
        t.remove(11);
        assert!(!run_gone(&run, &t));
        assert_eq!(alive_pids(&run, &t), vec![10, 11]);
        // The group is gone too; the holder still runs.
        t.set_group_alive(&group, false);
        assert!(!run_gone(&run, &t));
        assert_eq!(alive_pids(&run, &t), vec![10]);
        // Its pid reused by another process is not the holder.
        t.insert(10, None, Some(999));
        assert!(run_gone(&run, &t));
        assert!(alive_pids(&run, &t).is_empty());
        // A Windows job the service cannot see is not gone.
        t.set_group_unknown(&group, true);
        assert!(!run_gone(&run, &t));
        // Not started: the holder alone.
        let waiting = heavy_run(20, None, None);
        t.insert(20, None, Some(200));
        assert!(!run_gone(&waiting, &t));
        t.remove(20);
        assert!(run_gone(&waiting, &t));
        // Not a run.
        let process = StopTarget::Process {
            claim_id: "c1".into(),
            process: ident(30),
        };
        assert!(!run_gone(&process, &t));
    }

    /// A stopper that only says what it was told; the fake table decides what is gone.
    struct Scripted(Result<(StopEvidence, bool), StopError>);

    impl HolderStopper for Scripted {
        fn handles(&self, _: &StopTarget) -> bool {
            true
        }
        fn stop(&self, _: &StopTarget, _: Duration) -> Result<StopEvidence, StopError> {
            unreachable!()
        }
        fn stop_counted(
            &self,
            _: &StopTarget,
            _: Duration,
            _: &AtomicBool,
        ) -> Result<(StopEvidence, bool), StopError> {
            self.0.clone()
        }
    }

    #[test]
    fn stop_run_verifies_and_waits_for_the_holder() {
        use crate::procs::fake::FakeTable;

        let t = std::sync::Arc::new(FakeTable::new("b1"));
        let run = heavy_run(10, Some(11), None);
        t.insert(10, None, Some(100));
        // The command is gone, the holder exits by itself a little later.
        let exiting = t.clone();
        let exit = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            exiting.remove(10);
        });
        let ok = Scripted(Ok((gone(), true)));
        let done = stop_run_with(
            &run,
            Duration::ZERO,
            &ok,
            t.as_ref(),
            Duration::from_secs(5),
        );
        exit.join().unwrap();
        assert_eq!(
            done,
            RunStop {
                signalled: true,
                gone: true,
                alive: Vec::new(),
                error: None
            }
        );
        // A holder that stays: not verified, after the wait.
        t.insert(10, None, Some(100));
        let started = std::time::Instant::now();
        let kept = stop_run_with(
            &run,
            Duration::ZERO,
            &ok,
            t.as_ref(),
            Duration::from_millis(200),
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!((kept.gone, kept.alive), (false, vec![10]));
        // A failed stop is checked once, without the wait.
        t.insert(11, Some(10), Some(110));
        let denied = Scripted(Err(StopError::PermissionDenied("no".into())));
        let started = std::time::Instant::now();
        let failed = stop_run_with(
            &run,
            Duration::ZERO,
            &denied,
            t.as_ref(),
            Duration::from_secs(30),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            failed,
            RunStop {
                signalled: false,
                gone: false,
                alive: vec![10, 11],
                error: Some(StopError::PermissionDenied("no".into()))
            }
        );
    }

    /// The real stop of a real group: SIGTERM ends it and it is verified gone (the command is its
    /// own holder here).
    #[cfg(unix)]
    #[test]
    fn stop_run_ends_a_real_group() {
        use std::os::unix::process::CommandExt;

        let table = SysProcessTable::new().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let leader = procs::identity_of(&table, child.id()).unwrap();
        let group = RunGroup::UnixGroup {
            pgid: child.id(),
            boot_id: table.boot_id(),
        };
        let run = StopTarget::RunGroup {
            claim_id: "heavy:h1".into(),
            holder: leader.clone(),
            child: Some(leader),
            group: Some(group.clone()),
        };
        let reaper = std::thread::spawn(move || child.wait());
        let done = stop_run(&run, Duration::from_secs(10));
        reaper.join().unwrap().unwrap();
        assert_eq!(
            done,
            RunStop {
                signalled: true,
                gone: true,
                alive: Vec::new(),
                error: None
            }
        );
        assert!(!procs::group_liveness(&group, &table));
    }
}
