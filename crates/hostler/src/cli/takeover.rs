//! Takeover from the CLI: holder stoppers and the two-phase seizure (plan P3a, Task 6).
//!
//! `take_over` asks the service for a plan (`claims.seize_begin`), stops every target in it one
//! by one with the first `HolderStopper` that handles it, and commits with the evidence
//! (`claims.seize_commit`). The service moves the claims only when every target is verifiably
//! gone. Stops are not atomic: when target k fails, targets 1…k−1 stay stopped (R2-11).
//!
//! Limits (P1): processes that escaped their run's group (`setsid`, a Windows breakaway job) are
//! not tracked. On Unix a group with members of another user ends as `StillAlive` after SIGKILL
//! reached only some of them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::client::Client;
use crate::contract::claims::{
    ClaimOutcome, ClaimRequest, ResourceId, SeizeBegin, SeizeCommit, SeizeOutcome, SeizePlan,
    SeizeProgress, StopEvidence, StopTarget, errors,
};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
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
}

impl StopError {
    /// Why `target` (as `describe` names it) could not be stopped.
    fn reason(&self, target: &str) -> String {
        match self {
            StopError::PermissionDenied(why) => format!("hostler may not stop {target}: {why}"),
            StopError::StillAlive => format!("{target} still runs after it was killed"),
            StopError::Unknown(why) => format!("{target}: {why}"),
            StopError::Unsupported => "hostler cannot stop this holder".into(),
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

fn gone() -> StopEvidence {
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
    enum Sent {
        Delivered,
        Gone,
        Denied,
    }

    fn send(to_group: bool, pid: u32, sig: libc::c_int) -> Sent {
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

    /// SIGTERM, `grace`, SIGKILL, 5 s; `done` tells whether the target is gone.
    fn term_then_kill(
        to_group: bool,
        pid: u32,
        grace: Duration,
        done: &dyn Fn() -> bool,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        let what = if to_group { "process group" } else { "process" };
        let mut signalled = false;
        for (sig, wait) in [(libc::SIGTERM, grace), (libc::SIGKILL, KILL_WAIT)] {
            // Interrupted (T6-R3-2): no further signal.
            if cancel.load(Ordering::SeqCst) {
                return Err(StopError::Unknown("the takeover was interrupted".into()));
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

    /// T6-5: false when the group's leader — the run's recorded command, which leads its own
    /// group — is dead and a live process now has its pid with another start time: the pgid was
    /// reused (the kernel frees a pid only once no group uses it), so the members there are a
    /// stranger's.
    pub(super) fn group_is_ours(
        group: &RunGroup,
        child: Option<&ProcessIdentity>,
        table: &dyn ProcessTable,
    ) -> bool {
        let (RunGroup::UnixGroup { pgid, .. }, Some(child)) = (group, child) else {
            return true;
        };
        if child.pid != *pgid || !dead(child, table) {
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
        let RunGroup::UnixGroup { pgid, .. } = group else {
            // A group of the other OS family cannot exist on this machine.
            return Ok((gone(), false));
        };
        // A gone group is evidence; no signal goes to a pgid that may be reused.
        let done = || !procs::group_liveness(group, table);
        if done() || !group_is_ours(group, child, table) {
            return Ok((gone(), false));
        }
        term_then_kill(true, *pgid, grace, &done, cancel)
    }

    pub(super) fn stop_process(
        p: &ProcessIdentity,
        table: &dyn ProcessTable,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        let done = || dead(p, table);
        if done() {
            return Ok((gone(), false));
        }
        term_then_kill(false, p.pid, grace, &done, cancel)
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

/// How a target is named in messages (the service's wording for the same target).
pub fn describe(t: &StopTarget) -> String {
    match t {
        StopTarget::RunGroup {
            claim_id,
            holder,
            child,
            ..
        } => {
            let pid = child.as_ref().unwrap_or(holder).pid;
            match claim_id.strip_prefix("heavy:") {
                Some(entry) => format!("heavy run {entry} (PID {pid})"),
                None => format!("the run of claim {claim_id} (PID {pid})"),
            }
        }
        StopTarget::Process { claim_id, process } => {
            format!("PID {}, the bound process of hold {claim_id}", process.pid)
        }
        StopTarget::Foreign { source, .. } => format!("{source}'s claim"),
    }
}

fn with_target(e: StopEvidence, target: usize) -> StopEvidence {
    match e {
        StopEvidence::Gone { .. } => StopEvidence::Gone { target },
        StopEvidence::JobGone { session, .. } => StopEvidence::JobGone { target, session },
    }
}

fn unreadable(what: &str, e: serde_json::Error) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: format!("unreadable {what} answer from the service: {e}"),
        details: serde_json::json!({}),
        recovery: None,
    }
}

/// The resource as people read it, and who held it, from the plan.
fn names(plan: &SeizePlan, confirmation: &str) -> (String, String) {
    let root = plan
        .claims
        .iter()
        .find(|c| c.parent.is_none())
        .or(plan.claims.first());
    let display = root
        .map(|c| c.resource.display.clone())
        // The confirmation is the resource's token (a device's display, `screen`, an account).
        .unwrap_or_else(|| confirmation.to_owned());
    let holder = root
        .map(|c| c.workspace.path.clone())
        .or_else(|| plan.foreign.iter().find_map(|f| f.workspace_path.clone()))
        .or_else(|| plan.foreign.first().map(|f| f.source.clone()))
        .unwrap_or_else(|| "its holder".into());
    (display, holder)
}

/// The recorded takeover of `resource` for `request` (possibly already queued). On a refusal
/// the plan is aborted; withdrawing the request is the caller's.
pub async fn take_over(
    client: &mut Client,
    resource: &ResourceId,
    confirmation: &str,
    request: ClaimRequest,
) -> Result<ClaimOutcome, HostlerError> {
    take_over_with(
        client,
        resource,
        confirmation,
        request,
        stoppers(),
        takeover_grace(),
    )
    .await
}

/// `take_over` with these stoppers and this SIGTERM grace instead of `stoppers()` and
/// `TAKEOVER_GRACE` (or its test hook).
pub async fn take_over_with(
    client: &mut Client,
    resource: &ResourceId,
    confirmation: &str,
    request: ClaimRequest,
    stoppers: Vec<Box<dyn HolderStopper>>,
    grace: Duration,
) -> Result<ClaimOutcome, HostlerError> {
    take_over_until(
        client,
        resource,
        confirmation,
        request,
        stoppers,
        grace,
        interrupted(),
    )
    .await
}

/// A Ctrl+C or SIGTERM during a takeover (T7-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    Interrupt,
    Terminate,
}

impl Interrupt {
    pub fn exit_code(self) -> i32 {
        match self {
            Interrupt::Interrupt => 130,
            Interrupt::Terminate => 143,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Interrupt::Interrupt => "SIGINT",
            Interrupt::Terminate => "SIGTERM",
        }
    }
}

/// Resolves on the first Ctrl+C or SIGTERM (Windows: Ctrl+C), listening from this call on.
pub fn interrupted() -> std::pin::Pin<Box<dyn Future<Output = Interrupt> + Send>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut int), Ok(mut term)) => Box::pin(async move {
                tokio::select! {
                    _ = int.recv() => Interrupt::Interrupt,
                    _ = term.recv() => Interrupt::Terminate,
                }
            }),
            _ => Box::pin(std::future::pending()),
        }
    }
    #[cfg(windows)]
    {
        Box::pin(async {
            match tokio::signal::ctrl_c().await {
                Ok(()) => Interrupt::Interrupt,
                Err(_) => std::future::pending().await,
            }
        })
    }
}

/// How long an interrupted or refused takeover waits to abort its plan.
const ABORT_BUDGET: Duration = Duration::from_secs(2);

/// `take_over_with`, ending early when `interrupt` resolves: the plan is aborted (within 2 s)
/// and the error says how many targets were stopped (T7-8). Its shape: code `Withdrawn`,
/// `details { interrupted: "SIGINT" | "SIGTERM", exit_code: 130 | 143, resource, stopped, of }`;
/// callers exit with `details.exit_code`.
pub async fn take_over_until(
    client: &mut Client,
    resource: &ResourceId,
    confirmation: &str,
    request: ClaimRequest,
    mut stoppers: Vec<Box<dyn HolderStopper>>,
    grace: Duration,
    interrupt: impl Future<Output = Interrupt>,
) -> Result<ClaimOutcome, HostlerError> {
    tokio::pin!(interrupt);
    let requester = request.requester.clone();
    let answer = client
        .call(Request::ClaimsSeizeBegin(SeizeBegin {
            resource: resource.clone(),
            confirmation: confirmation.to_owned(),
            request,
        }))
        .await?;
    let plan: SeizePlan =
        serde_json::from_value(answer).map_err(|e| unreadable("claims.seize_begin", e))?;
    let (display, holder) = names(&plan, confirmation);
    let n = plan.stops.len();
    // The taker must not be one of the processes it would stop (it would SIGTERM itself).
    if let Some(own) = own_target(&plan, &requester) {
        abort(client, &plan).await;
        let mut e = errors::seizure_failed(
            &display,
            &format!(
                "this command runs under {own}, which a takeover would stop; run it from outside \
                 that run"
            ),
        );
        e.details["reason"] = "taker_is_a_target".into();
        return Err(e);
    }
    #[cfg(debug_assertions)]
    if let Some(ms) = test_hooks::ms("HOSTLER_TEST_SEIZE_PAUSE_MS") {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            got = &mut interrupt => return Err(interrupted_at(client, &plan, got, 0, &display).await),
        }
    }
    let mut evidence = Vec::with_capacity(n);
    // Targets signalled; one found already ended is not counted as stopped.
    let mut stopped = 0;
    for (k, target) in plan.stops.iter().enumerate() {
        let name = describe(target);
        // L29: the plan lives as long as its taker keeps stopping targets.
        let progress = client.call(Request::ClaimsSeizeProgress(SeizeProgress {
            plan_id: plan.plan_id.clone(),
            requester: requester.clone(),
        }));
        let progressed = tokio::select! {
            progressed = progress => progressed,
            got = &mut interrupt => return Err(interrupted_at(client, &plan, got, stopped, &display).await),
        };
        if let Err(e) = progressed {
            return Err(stopped_before(e, stopped, n, &display));
        }
        let Some(index) = stoppers.iter().position(|s| s.handles(target)) else {
            return Err(refuse(
                client,
                &plan,
                stopped,
                &name,
                &display,
                StopError::Unsupported,
            )
            .await);
        };
        // T7-8: every wait says what it waits for.
        eprintln!("{}", stopping_line(k, n, &name));
        // A thread of its own: an interrupted stop is left to finish while this returns, and the
        // client's reader keeps draining the socket meanwhile.
        let (done, result) = tokio::sync::oneshot::channel();
        let t = target.clone();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        std::thread::Builder::new()
            .name("hostler-stop".into())
            .spawn(move || {
                let result = stoppers[index].stop_counted(&t, grace, &cancelled);
                let _ = done.send((result, stoppers));
            })
            .map_err(|e| HostlerError {
                code: ErrorCode::Internal,
                message: format!("cannot start the stop of {name}: {e}"),
                details: serde_json::json!({}),
                recovery: None,
            })?;
        let (result, back) = tokio::select! {
            stopped = result => stopped.map_err(|_| HostlerError {
                code: ErrorCode::Internal,
                message: format!("the stop of {name} failed"),
                details: serde_json::json!({}),
                recovery: None,
            })?,
            got = &mut interrupt => {
                // Its stop had begun: it may still end, but it is never escalated (T6-R3-2).
                cancel.store(true, Ordering::SeqCst);
                return Err(interrupted_during(client, &plan, got, stopped, &name, &display).await);
            }
        };
        stoppers = back;
        match result {
            Ok((e, signalled)) => {
                evidence.push(with_target(e, k));
                stopped += usize::from(signalled);
            }
            Err(e) => return Err(refuse(client, &plan, stopped, &name, &display, e).await),
        }
    }
    let answer = match client
        .call(Request::ClaimsSeizeCommit(SeizeCommit {
            plan_id: plan.plan_id.clone(),
            requester,
            evidence,
        }))
        .await
    {
        Ok(answer) => answer,
        // Every target is gone, and still the holder keeps it (L29).
        Err(e) => return Err(stopped_before(e, stopped, n, &display)),
    };
    let committed: SeizeOutcome =
        serde_json::from_value(answer).map_err(|e| unreadable("claims.seize_commit", e))?;
    eprintln!(
        "{}",
        took_line(&display, &holder, stopped, committed.attested)
    );
    Ok(committed.outcome)
}

/// The plan target among the taker's own processes (itself and its ancestors), if any.
/// Also a target whose group (Unix) or job (Windows) the taker is in without descending from
/// it, e.g. a background job reparented to init: `killpg` would reach the taker too.
fn own_target(plan: &SeizePlan, requester: &ProcessIdentity) -> Option<String> {
    let mut own = vec![requester.pid];
    if let Ok(table) = SysProcessTable::new() {
        own.extend(procs::ancestors(requester.pid, &table));
    }
    plan.stops
        .iter()
        .find(|t| {
            target_pids(t).iter().any(|pid| own.contains(pid))
                || matches!(t, StopTarget::RunGroup { group: Some(group), .. }
                    if procs::in_group(requester.pid, group))
        })
        .map(describe)
}

fn target_pids(t: &StopTarget) -> Vec<u32> {
    match t {
        StopTarget::RunGroup { holder, child, .. } => std::iter::once(holder.pid)
            .chain(child.iter().map(|c| c.pid))
            .collect(),
        StopTarget::Process { process, .. } => vec![process.pid],
        StopTarget::Foreign { .. } => Vec::new(),
    }
}

/// The interrupted target's clause (T6-R3-2): Unix had sent SIGTERM; Windows stops a job by
/// `TerminateJobObject` at once.
fn begun_clause(name: &str) -> String {
    let how = if cfg!(windows) {
        "its job was being terminated"
    } else {
        "SIGTERM sent"
    };
    format!("; the stop of {name} had begun ({how}), so it may still end")
}

/// What a takeover prints before it stops target `k` (0-based) of `n` (T7-8).
fn stopping_line(k: usize, n: usize, name: &str) -> String {
    format!("[hostler] Stopping {} of {n}: {name} …", k + 1)
}

/// Aborts the plan within `ABORT_BUDGET`; the plan lapses on its own otherwise.
async fn abort(client: &mut Client, plan: &SeizePlan) {
    let _ = tokio::time::timeout(
        ABORT_BUDGET,
        client.call(Request::ClaimsSeizeAbort {
            plan_id: plan.plan_id.clone(),
        }),
    )
    .await;
}

/// Interrupted after `k` targets were stopped, between stops (T7-8): the plan is aborted, and
/// the error says so.
async fn interrupted_at(
    client: &mut Client,
    plan: &SeizePlan,
    got: Interrupt,
    k: usize,
    display: &str,
) -> HostlerError {
    interrupted_after(client, plan, got, k, None, display).await
}

/// Interrupted while target `k` (`name`) was being stopped: its stop had begun (T6-R3-2).
async fn interrupted_during(
    client: &mut Client,
    plan: &SeizePlan,
    got: Interrupt,
    k: usize,
    name: &str,
    display: &str,
) -> HostlerError {
    interrupted_after(client, plan, got, k, Some(name), display).await
}

async fn interrupted_after(
    client: &mut Client,
    plan: &SeizePlan,
    got: Interrupt,
    k: usize,
    signalled: Option<&str>,
    display: &str,
) -> HostlerError {
    abort(client, plan).await;
    let n = plan.stops.len();
    let begun = signalled.map(begun_clause).unwrap_or_default();
    eprintln!(
        "[hostler] Interrupted: {k} of {n} processes were stopped{begun}; {display} stays with \
         its holder."
    );
    HostlerError {
        code: ErrorCode::Withdrawn,
        message: format!(
            "the takeover of {display} was interrupted ({}); {k} of {n} processes were \
             stopped{begun}",
            got.name()
        ),
        details: serde_json::json!({
            "interrupted": got.name(),
            "exit_code": got.exit_code(),
            "resource": display,
            "stopped": k,
            "signalled": usize::from(signalled.is_some()),
            "of": n,
        }),
        recovery: None,
    }
}

/// The line a successful takeover prints; L30 when the service took the taker's word.
fn took_line(display: &str, holder: &str, n: usize, attested: bool) -> String {
    // The holder is another workspace's path (or a foreign source): sanitize its text.
    let display = crate::secrets::sanitize_for_terminal(display);
    let holder = crate::secrets::sanitize_for_terminal(holder);
    if attested {
        format!(
            "[hostler] Took {display} over from {holder} ({n} process(es) stopped); recorded as \
             forced: the service could not verify that the holder stopped."
        )
    } else {
        format!("[hostler] Took {display} over from {holder} ({n} process(es) stopped); recorded.")
    }
}

/// A refusal after `k` of `n` targets were stopped: says so (R2-11, L29), also in `details`.
fn stopped_before(mut e: HostlerError, k: usize, n: usize, display: &str) -> HostlerError {
    if k > 0 {
        eprintln!(
            "[hostler] {k} of {n} processes were stopped, but the takeover was refused; \
             {display} stays with its holder."
        );
    }
    if let Some(details) = e.details.as_object_mut() {
        details.insert("stopped".into(), k.into());
        details.insert("of".into(), n.into());
    } else {
        e.details = serde_json::json!({ "stopped": k, "of": n });
    }
    e
}

/// Aborts the plan; says how many targets were already stopped (R2-11).
async fn refuse(
    client: &mut Client,
    plan: &SeizePlan,
    k: usize,
    name: &str,
    display: &str,
    e: StopError,
) -> HostlerError {
    abort(client, plan).await;
    if let Some(line) = partial_line(k, plan.stops.len(), name, display) {
        eprintln!("{line}");
    }
    let mut error = errors::seizure_failed(display, &e.reason(name));
    error.details["stopped"] = k.into();
    error.details["of"] = plan.stops.len().into();
    error
}

/// `None` when nothing was stopped before target `k` failed.
fn partial_line(k: usize, n: usize, name: &str, display: &str) -> Option<String> {
    (k > 0).then(|| {
        format!(
            "[hostler] {k} of {n} processes were stopped before {name} could not be; {display} \
             stays with its holder."
        )
    })
}

#[cfg(debug_assertions)]
mod test_hooks {
    use std::path::PathBuf;

    use super::*;
    use crate::contract::claims::ForeignClaim;

    pub(super) fn ms(name: &str) -> Option<u64> {
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
    fn partial_stop_line_counts_the_stopped_ones() {
        assert_eq!(partial_line(0, 2, "x", "screen"), None);
        assert_eq!(
            partial_line(1, 2, "PID 9", "screen").unwrap(),
            "[hostler] 1 of 2 processes were stopped before PID 9 could not be; screen stays \
             with its holder."
        );
    }

    #[test]
    fn evidence_gets_its_target_index() {
        assert_eq!(with_target(gone(), 3), StopEvidence::Gone { target: 3 });
        assert_eq!(
            with_target(
                StopEvidence::JobGone {
                    target: 0,
                    session: 2
                },
                1
            ),
            StopEvidence::JobGone {
                target: 1,
                session: 2
            }
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
        // No recorded leader, or a child that does not lead the group: nothing to compare.
        assert!(group_is_ours(&group, None, &t));
        let other = ProcessIdentity {
            pid: 4243,
            ..leader.clone()
        };
        t.insert(4242, None, Some(999));
        assert!(group_is_ours(&group, Some(&other), &t));
        // (`stop_group` is not called here: pgid 4242 may be a real group on this machine.)
    }

    #[test]
    fn an_interrupted_stop_says_how_it_had_begun() {
        let clause = begun_clause("the run of claim c1 (PID 7)");
        #[cfg(unix)]
        assert_eq!(
            clause,
            "; the stop of the run of claim c1 (PID 7) had begun (SIGTERM sent), so it may still \
             end"
        );
        #[cfg(windows)]
        assert_eq!(
            clause,
            "; the stop of the run of claim c1 (PID 7) had begun (its job was being terminated), \
             so it may still end"
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

    #[test]
    fn each_stop_says_what_it_waits_for() {
        assert_eq!(
            stopping_line(0, 3, "the run of claim c1 (PID 7)"),
            "[hostler] Stopping 1 of 3: the run of claim c1 (PID 7) …"
        );
        assert_eq!(Interrupt::Interrupt.exit_code(), 130);
        assert_eq!(Interrupt::Terminate.exit_code(), 143);
    }

    #[test]
    fn a_late_refusal_says_what_was_stopped() {
        let e = stopped_before(errors::seizure_failed("screen", "x"), 2, 3, "screen");
        assert_eq!(e.details["stopped"], 2);
        assert_eq!(e.details["of"], 3);
        assert_eq!(e.details["resource"], "screen");
        assert!(took_line("screen", "/w/a", 1, true).contains("could not verify"));
        assert!(!took_line("screen", "/w/a", 1, false).contains("could not verify"));
    }

    #[test]
    fn took_line_sanitizes_the_holder_path() {
        let line = took_line("screen", "/w/\u{1b}]0;PWNED\u{7}\u{1b}[2J", 1, false);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(!line.contains('\u{7}'), "{line:?}");
        assert!(line.contains('\u{FFFD}'), "{line:?}");
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
}
