//! Takeover from the CLI: the two-phase seizure (plan P3a, Task 6). The holder stoppers live in
//! `procs::stop` (P5 Task 6), shared with the service's `heavy.stop`; they are re-exported here.
//!
//! `take_over` asks the service for a plan (`claims.seize_begin`), stops every target in it one
//! by one with the first `HolderStopper` that handles it, and commits with the evidence
//! (`claims.seize_commit`). The service moves the claims only when every target is verifiably
//! gone. Stops are not atomic: when target k fails, targets 1…k−1 stay stopped (R2-11).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::client::Client;
use crate::contract::claims::{
    ClaimOutcome, ClaimRequest, ResourceId, SeizeBegin, SeizeCommit, SeizeOutcome, SeizePlan,
    SeizeProgress, StopEvidence, StopTarget, errors,
};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
pub use crate::procs::stop::{
    HolderStopper, RunGroupStopper, StopError, TAKEOVER_GRACE, stoppers, takeover_grace,
};
use crate::procs::{self, ProcessIdentity, SysProcessTable};

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
    // R2-9 (P5 Task 6): nor may it stop the hostler service (the check `heavy.stop` makes).
    match service_target(client, &plan).await {
        Ok(None) => {}
        Ok(Some(target)) => {
            abort(client, &plan).await;
            let mut e = errors::seizure_failed(
                &display,
                &format!("{target} contains the hostler service, which hostler never stops"),
            );
            e.details["reason"] = "target_is_the_service".into();
            return Err(e);
        }
        Err(e) => {
            abort(client, &plan).await;
            return Err(e);
        }
    }
    #[cfg(debug_assertions)]
    if let Some(ms) = crate::procs::stop::test_hooks::ms("HOSTLER_TEST_SEIZE_PAUSE_MS") {
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
            crate::procs::stop::target_pids(t)
                .iter()
                .any(|pid| own.contains(pid))
                || matches!(t, StopTarget::RunGroup { group: Some(group), .. }
                    if procs::in_group(requester.pid, group))
        })
        .map(describe)
}

/// The plan target that contains the hostler service (`procs::stop::refuses_self_target`, with
/// the service's pid from `service.status`), if any. A plan with nothing to stop asks nothing.
async fn service_target(
    client: &mut Client,
    plan: &SeizePlan,
) -> Result<Option<String>, HostlerError> {
    if plan.stops.is_empty() {
        return Ok(None);
    }
    let answer = client.call(Request::ServiceStatus).await?;
    let status: crate::contract::heavy::ServiceStatus =
        serde_json::from_value(answer).map_err(|e| unreadable("service.status", e))?;
    Ok(plan
        .stops
        .iter()
        .find(|t| crate::procs::stop::refuses_self_target(status.pid, t))
        .map(describe))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::stop::gone;

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
}
