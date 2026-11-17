//! The shared waiting line and acquire loop of `hold` and `run` (plan P3a, Task 7).
//!
//! One `Deadline` per hold or run: every wait of `acquire_waiting` measures from its `started`.
//! The line is P1's shape for a claim: who holds the resource, what that holder does, where we are
//! in the queue, and how to ask for a takeover. Ctrl+C and SIGTERM win over every answer: the
//! request is withdrawn (2 s budget) and a grant that won the race is released.

use std::future::Future;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::time::Instant as TokioInstant;

use crate::cli::{spawn, takeover, wait};
use crate::client::Client;
use crate::contract::claims::FORCE_SENTENCE;
use crate::contract::claims::{
    Blocker, Claim, ClaimKind, ClaimOutcome, ClaimRelease, ClaimRequest, EndReason, ResourceId,
    errors,
};
use crate::contract::wire::{Event, Request};
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::{Clock, SystemClock};
use crate::secrets::sanitize_for_terminal;

/// The default wait of a claim command when none is given (D7).
pub const DEFAULT_MAX_WAIT_MIN: f64 = 5.0;
/// How often a waiting request is re-attached to the service.
const ATTACH_EVERY: Duration = Duration::from_secs(5);
/// How long reconnecting may take before the wait fails closed.
const RECONNECT_BUDGET: Duration = Duration::from_secs(10);
const RETRY_PAUSE: Duration = Duration::from_millis(200);
/// A withdrawal is not worth a long wait: an unwithdrawn request is reaped once we are gone.
const WITHDRAW_BUDGET: Duration = Duration::from_secs(2);
/// `--no-wait` behind a sessions check only: one agent-device read bound (P3-14).
const CHECKING_NO_WAIT: Duration = Duration::from_secs(6);
const CHECKING_RETRY: Duration = Duration::from_millis(200);
/// The problem text the service gives after 30 s of unreadable agent-device status.
const SESSIONS_UNKNOWN: &str = "agent-device sessions unknown: ";

/// One deadline per hold or run, measured from its first wait (D7). `None` = unbounded.
#[derive(Clone, Debug)]
pub struct Deadline {
    pub started: Instant,
    pub max_wait: Option<Duration>,
}

/// What the request came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acquired {
    Granted(Claim),
    Joined(Claim),
    Reentrant(String),
}

/// Why the request did not come to a grant.
#[derive(Debug)]
pub enum Gave {
    TimedOut,
    Busy { position: u32, of: u32 },
    Interrupted(i32),
    WithdrawnElsewhere,
    Deadlock(HostlerError),
    Failed(HostlerError),
}

/// Minutes (fractions allowed, 0 = no limit) as a wait; too large counts as no limit.
pub fn max_wait(minutes: Option<f64>) -> Option<Duration> {
    let minutes = minutes.unwrap_or(DEFAULT_MAX_WAIT_MIN);
    (minutes > 0.0)
        .then(|| Duration::try_from_secs_f64(minutes * 60.0).ok())
        .flatten()
}

/// `claims.acquire`, then queued waits with a visible line until the request is decided.
/// Re-attaches every 5 s and on every `claims.changed`; never starts a service when reconnecting
/// (the request lives in the store, so a restarted service answers it). `stop` is the caller's,
/// one per run or hold (L34): its signals stay registered from before the first acquire to the
/// end of the command, and its deadline bounds waiting only.
pub async fn acquire_waiting(
    client: &mut Client,
    req: ClaimRequest,
    text: &WaitText<'_>,
    stop: &mut Stop,
    no_wait: bool,
) -> Result<Acquired, Gave> {
    let mut io = Live { client };
    run(&mut io, &req, text, stop, no_wait).await
}

/// One `claims.acquire`, raced only with the signals (the deadline bounds waiting, L34): an
/// interrupt withdraws the request and ends a grant that won the race. For `--force`, which
/// looks at the first answer before it waits.
pub async fn acquire_once(
    client: &mut Client,
    req: &ClaimRequest,
    stop: &mut Stop,
) -> Result<ClaimOutcome, Gave> {
    let mut io = Live { client };
    first_answer(&mut io, req, stop).await
}

async fn first_answer(
    io: &mut impl ClaimsIo,
    req: &ClaimRequest,
    stop: &mut Stop,
) -> Result<ClaimOutcome, Gave> {
    let answer = tokio::select! {
        biased;
        code = stop.interrupt() => {
            return Err(give_up(io, req, Stopped::Interrupted(code)).await);
        }
        answer = io.call(Request::ClaimsAcquire(req.clone())) => answer,
    };
    answer.and_then(decode).map_err(Gave::Failed)
}

/// The loop itself, over any `ClaimsIo` (the unit tests use a fake).
async fn run(
    io: &mut impl ClaimsIo,
    req: &ClaimRequest,
    text: &WaitText<'_>,
    stop: &mut Stop,
    _no_wait: bool,
) -> Result<Acquired, Gave> {
    let mut outcome = first_answer(io, req, stop).await?;
    loop {
        match outcome {
            ClaimOutcome::Queued {
                position,
                of,
                blockers,
                ..
            } => {
                outcome = wait_queued(io, req, text, stop, position, of, blockers).await?;
            }
            ClaimOutcome::Busy {
                position,
                of,
                blockers,
            } => {
                let unknown = if only_checking(&blockers) {
                    sessions_unknown_in(io).await
                } else {
                    None
                };
                // P3-14: behind a sessions read only, wait one read bound; never while the
                // status is unreadable (no read would come).
                if only_checking(&blockers)
                    && unknown.is_none()
                    && let Some(next) = recheck_busy(io, req, stop).await?
                {
                    outcome = next;
                    continue;
                }
                eprintln!("{}", claim_busy_line(text, &blockers, unknown.as_deref()));
                return Err(Gave::Busy { position, of });
            }
            other => return decided(other, text.display),
        }
    }
}

fn only_checking(blockers: &[Blocker]) -> bool {
    !blockers.is_empty()
        && blockers
            .iter()
            .all(|b| matches!(b, Blocker::Checking { .. }))
}

/// `--no-wait` behind a sessions check: asks again until it is decided otherwise, for up to one
/// read bound. `None`: still only checking.
async fn recheck_busy(
    io: &mut impl ClaimsIo,
    req: &ClaimRequest,
    stop: &mut Stop,
) -> Result<Option<ClaimOutcome>, Gave> {
    let until = Instant::now() + CHECKING_NO_WAIT;
    while Instant::now() < until {
        tokio::select! {
            biased;
            code = stop.interrupt() => {
                return Err(give_up(io, req, Stopped::Interrupted(code)).await);
            }
            () = tokio::time::sleep(CHECKING_RETRY) => {}
        }
        match first_answer(io, req, stop).await? {
            ClaimOutcome::Busy { blockers, .. } if only_checking(&blockers) => {}
            other => return Ok(Some(other)),
        }
    }
    Ok(None)
}

/// `agent-device sessions unknown: {reason}`'s reason, when the service says the sessions
/// cannot be read (`devices.list`'s `ToolStatus`); `None` when they can, or no answer.
async fn sessions_unknown_in(io: &mut impl ClaimsIo) -> Option<String> {
    let value = io.call(Request::DevicesList { all: false }).await.ok()?;
    let tools = value.get("tools")?;
    if tools.get("sessions_known").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let reason = tools
        .get("problems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|p| p.strip_prefix(SESSIONS_UNKNOWN))
        .unwrap_or("its status cannot be read");
    Some(reason.to_owned())
}

/// The reason agent-device sessions are unknown (the grant and run-end line), over `client`.
pub async fn sessions_unknown(client: &mut Client) -> Option<String> {
    sessions_unknown_in(&mut Live { client }).await
}

/// `[hostler] agent-device sessions unknown: {reason}` (P3-14, L51).
pub fn sessions_unknown_line(reason: &str) -> String {
    format!(
        "[hostler] {SESSIONS_UNKNOWN}{}",
        sanitize_for_terminal(reason)
    )
}

/// `takeover::take_over` under the caller's one `Stop` (L34): a Ctrl+C or SIGTERM aborts the
/// plan through `take_over_until`, and no second signal listener exists.
pub async fn take_over(
    client: &mut Client,
    resource: &ResourceId,
    confirmation: &str,
    request: ClaimRequest,
    stop: &mut Stop,
) -> Result<ClaimOutcome, HostlerError> {
    let interrupt = async {
        match stop.interrupt().await {
            143 => takeover::Interrupt::Terminate,
            _ => takeover::Interrupt::Interrupt,
        }
    };
    takeover::take_over_until(
        client,
        resource,
        confirmation,
        request,
        takeover::stoppers(),
        takeover::takeover_grace(),
        interrupt,
    )
    .await
}

/// After `take_over` failed with `e`, what became of `req` (T8-4): it may have been granted
/// meanwhile (the holder ended before the plan began, or the commit's answer was lost). A grant
/// is ours: `Ok` to go on with, unless the takeover was interrupted (then it is released). Else
/// the request is withdrawn and `e` returned.
pub async fn settle_failed_takeover(
    client: &mut Client,
    req: &ClaimRequest,
    e: HostlerError,
) -> Result<Acquired, HostlerError> {
    settle_failed(&mut Live { client }, req, e).await
}

async fn settle_failed(
    io: &mut impl ClaimsIo,
    req: &ClaimRequest,
    e: HostlerError,
) -> Result<Acquired, HostlerError> {
    let deadline = TokioInstant::now() + WITHDRAW_BUDGET;
    let attach = Request::ClaimsAttach {
        request_id: req.id.clone(),
    };
    let now = match tokio::time::timeout_at(deadline, io.call(attach)).await {
        Ok(Ok(value)) => decode(value).ok(),
        _ => None,
    };
    let interrupted = takeover_exit_code(&e) != 78;
    let claim = match now {
        Some(ClaimOutcome::Reentrant { claim_id }) if !interrupted => {
            return Ok(Acquired::Reentrant(claim_id));
        }
        Some(ClaimOutcome::Granted { claim }) if !interrupted => {
            return Ok(Acquired::Granted(claim));
        }
        Some(ClaimOutcome::Joined { claim }) if !interrupted => return Ok(Acquired::Joined(claim)),
        Some(ClaimOutcome::Granted { claim } | ClaimOutcome::Joined { claim }) => Some(claim),
        _ => None,
    };
    let undo = match claim {
        Some(claim) => Request::ClaimsRelease(ClaimRelease {
            claim_id: claim.id,
            workspace_id: req.workspace_id.clone(),
            requester: req.requester.clone(),
            exit_code: None,
        }),
        None => Request::ClaimsWithdraw {
            request_id: req.id.clone(),
            timed_out: false,
        },
    };
    let _ = tokio::time::timeout_at(deadline, io.call(undo)).await;
    Err(e)
}

/// The exit code of a refused takeover: an interrupted one's (130/143, from its
/// `details.exit_code`), else 78 (`SeizureFailed`, `taker_is_a_target` included).
pub fn takeover_exit_code(e: &HostlerError) -> i32 {
    match e.details.get("exit_code").and_then(Value::as_i64) {
        Some(code) if e.code == ErrorCode::Withdrawn => i32::try_from(code).unwrap_or(130),
        _ => 78,
    }
}

/// What the next await was interrupted by.
enum Raced<T> {
    Stopped(Stopped),
    Done(T),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stopped {
    TimedOut,
    Interrupted(i32),
}

/// The deadline and the terminal signals of one run or hold (L34). Ctrl+C, SIGHUP and SIGQUIT
/// are 130, SIGTERM 143. The signals are `spawn::Signals`, so a run hands the same listener on to
/// the heavy wait and the command (`take_signals`): no signal is swallowed in between.
pub struct Stop {
    since: Instant,
    max_wait: Option<Duration>,
    /// `None`: registration failed (or a unit test); nothing interrupts.
    signals: Option<spawn::Signals>,
}

impl Stop {
    pub fn new(deadline: &Deadline, signals: Option<spawn::Signals>) -> Stop {
        Stop {
            since: deadline.started,
            max_wait: deadline.max_wait,
            signals,
        }
    }

    pub fn max_wait(&self) -> Option<Duration> {
        self.max_wait
    }

    /// The listener, for the rest of the run; nothing interrupts this `Stop` afterwards.
    pub fn take_signals(&mut self) -> Option<spawn::Signals> {
        self.signals.take()
    }

    /// The exit code of a signal already received (130, or 143 for SIGTERM); never waits.
    pub fn interrupted(&mut self) -> Option<i32> {
        self.signals.as_mut().and_then(spawn::Signals::interrupted)
    }

    fn expired(&self) -> bool {
        wait::deadline_reached(self.since, Instant::now(), self.max_wait)
    }

    /// The next signal's exit code; never without signals. Cancel-safe.
    pub(crate) async fn interrupt(&mut self) -> i32 {
        match self.signals.as_mut() {
            Some(signals) => match signals.recv_code().await {
                Some(code) => code,
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }

    /// Resolves at the next terminal signal or at the deadline; cancel-safe.
    async fn wait(&mut self) -> Stopped {
        if self.expired() {
            return Stopped::TimedOut;
        }
        let deadline = self
            .max_wait
            .and_then(|limit| self.since.checked_add(limit));
        let expired = async move {
            match deadline {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(expired);
        tokio::select! {
            biased;
            code = self.interrupt() => Stopped::Interrupted(code),
            () = &mut expired => Stopped::TimedOut,
        }
    }

    /// Runs `future`, but a stop wins when both are ready.
    async fn race<T>(&mut self, future: impl Future<Output = T>) -> Raced<T> {
        tokio::select! {
            biased;
            stopped = self.wait() => Raced::Stopped(stopped),
            done = future => Raced::Done(done),
        }
    }

    #[cfg(test)]
    fn expired_now() -> Stop {
        Stop {
            since: Instant::now() - Duration::from_secs(1),
            max_wait: Some(Duration::ZERO),
            signals: None,
        }
    }
}

/// The queued part of the wait: print on every change (else every 30 s), re-attach every 5 s and
/// on every `claims.changed`, reconnect after a dropped connection.
async fn wait_queued(
    io: &mut impl ClaimsIo,
    req: &ClaimRequest,
    text: &WaitText<'_>,
    stop: &mut Stop,
    mut position: u32,
    mut of: u32,
    mut blockers: Vec<Blocker>,
) -> Result<ClaimOutcome, Gave> {
    let mut printed: Option<(Instant, Vec<Blocker>, u32, u32)> = None;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_attach = Instant::now();
    loop {
        let now = Instant::now();
        let changed = printed
            .as_ref()
            .is_none_or(|(_, shown, p, o)| *shown != blockers || *p != position || *o != of);
        if printed
            .as_ref()
            .is_none_or(|(at, ..)| wait::should_print(*at, now, changed))
        {
            let waited = now.saturating_duration_since(stop.since);
            let checking = blockers
                .iter()
                .any(|b| matches!(b, Blocker::Checking { .. }));
            let unknown = if checking {
                sessions_unknown_in(io).await
            } else {
                None
            };
            eprintln!(
                "{}",
                claim_wait_line(
                    text,
                    &blockers,
                    position,
                    of,
                    waited,
                    stop.max_wait,
                    unknown.as_deref()
                )
            );
            printed = Some((now, blockers.clone(), position, of));
        }
        let recheck = tokio::select! {
            biased;
            stopped = stop.wait() => return Err(give_up(io, req, stopped).await),
            _ = ticker.tick() => last_attach.elapsed() >= ATTACH_EVERY,
            event = io.next_event() => match event {
                Some(Event::ClaimsChanged(_)) => true,
                Some(_) => false,
                None => match stop.race(io.reconnect(RECONNECT_BUDGET)).await {
                    Raced::Stopped(stopped) => return Err(give_up(io, req, stopped).await),
                    Raced::Done(Ok(())) => true,
                    Raced::Done(Err(e)) => return Err(Gave::Failed(e)),
                },
            },
        };
        if !recheck {
            continue;
        }
        last_attach = Instant::now();
        let answer = stop.race(io.call(Request::ClaimsAttach {
            request_id: req.id.clone(),
        }));
        match answer.await {
            Raced::Stopped(stopped) => return Err(give_up(io, req, stopped).await),
            Raced::Done(Ok(value)) => match decode(value) {
                Ok(ClaimOutcome::Queued {
                    position: p,
                    of: o,
                    blockers: b,
                    ..
                }) => {
                    position = p;
                    of = o;
                    blockers = b;
                }
                Ok(other) => return Ok(other),
                Err(e) => return Err(Gave::Failed(e)),
            },
            // A sandbox denies every attempt alike: no reconnect (P4 R2-1).
            Raced::Done(Err(e)) if crate::client::is_sandbox_denied(&e) => {
                return Err(Gave::Failed(e));
            }
            Raced::Done(Err(e)) if e.code == ErrorCode::ServiceUnavailable => {
                match stop.race(io.reconnect(RECONNECT_BUDGET)).await {
                    Raced::Stopped(stopped) => return Err(give_up(io, req, stopped).await),
                    Raced::Done(Ok(())) => {
                        // Re-attach on the next tick, at once.
                        last_attach = Instant::now() - ATTACH_EVERY;
                    }
                    Raced::Done(Err(e)) => return Err(Gave::Failed(e)),
                }
            }
            Raced::Done(Err(e)) => return Err(Gave::Failed(e)),
        }
    }
}

fn decided(outcome: ClaimOutcome, display: &str) -> Result<Acquired, Gave> {
    match outcome {
        ClaimOutcome::Granted { claim } => Ok(Acquired::Granted(claim)),
        ClaimOutcome::Joined { claim } => Ok(Acquired::Joined(claim)),
        ClaimOutcome::Reentrant { claim_id } => Ok(Acquired::Reentrant(claim_id)),
        ClaimOutcome::Busy { position, of, .. } => Err(Gave::Busy { position, of }),
        // Only `claims.attach` reaches here queued, and it is not `--no-wait` any more.
        ClaimOutcome::Queued { position, of, .. } => Err(Gave::Busy { position, of }),
        ClaimOutcome::Finished { item } => Err(match item.reason {
            EndReason::Deadlock => Gave::Deadlock(errors::claim_deadlock(
                display,
                item.detail.as_deref().unwrap_or("the wait-for cycle"),
            )),
            // Refused on a tick (CR-A-4): the refusal itself, not a withdrawal.
            EndReason::Refused => Gave::Failed(crate::contract::secrets::errors::bad_request(
                item.detail
                    .unwrap_or_else(|| format!("{display} was refused")),
            )),
            _ => Gave::WithdrawnElsewhere,
        }),
    }
}

/// Withdraws the request (2 s budget) and ends a grant that won the race. `Gave` says which stop
/// won.
async fn give_up(io: &mut impl ClaimsIo, req: &ClaimRequest, stopped: Stopped) -> Gave {
    // One budget for all three calls (T7-9): an unwithdrawn request is reaped once we are gone.
    let deadline = TokioInstant::now() + WITHDRAW_BUDGET;
    let timed_out = matches!(stopped, Stopped::TimedOut);
    let withdraw = Request::ClaimsWithdraw {
        request_id: req.id.clone(),
        timed_out,
    };
    let _ = tokio::time::timeout_at(deadline, io.call(withdraw)).await;
    // A grant that arrived just before the stop is ours to end.
    let attach = Request::ClaimsAttach {
        request_id: req.id.clone(),
    };
    if let Ok(Ok(value)) = tokio::time::timeout_at(deadline, io.call(attach)).await
        && let Ok(ClaimOutcome::Granted { claim } | ClaimOutcome::Joined { claim }) = decode(value)
    {
        let release = Request::ClaimsRelease(ClaimRelease {
            claim_id: claim.id,
            workspace_id: req.workspace_id.clone(),
            requester: req.requester.clone(),
            exit_code: None,
        });
        let _ = tokio::time::timeout_at(deadline, io.call(release)).await;
    }
    match stopped {
        Stopped::TimedOut => Gave::TimedOut,
        Stopped::Interrupted(code) => Gave::Interrupted(code),
    }
}

fn decode(value: Value) -> Result<ClaimOutcome, HostlerError> {
    serde_json::from_value(value).map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("unreadable answer from the service: {e}"),
        details: serde_json::json!({}),
        recovery: None,
    })
}

/// The connection `acquire_waiting` talks over. A test fake implements it for the unit tests.
trait ClaimsIo {
    async fn call(&mut self, request: Request) -> Result<Value, HostlerError>;
    async fn next_event(&mut self) -> Option<Event>;
    async fn reconnect(&mut self, within: Duration) -> Result<(), HostlerError>;
}

struct Live<'a> {
    client: &'a mut Client,
}

impl ClaimsIo for Live<'_> {
    async fn call(&mut self, request: Request) -> Result<Value, HostlerError> {
        self.client.call(request).await
    }

    async fn next_event(&mut self) -> Option<Event> {
        self.client.next_event().await
    }

    /// A fresh connection to the running service; never starts one (the request is in the store).
    async fn reconnect(&mut self, within: Duration) -> Result<(), HostlerError> {
        let paths = crate::client::resolve_paths()?;
        let deadline = TokioInstant::now() + within;
        loop {
            match Client::connect(&paths).await {
                Ok(client) => {
                    *self.client = client;
                    return Ok(());
                }
                // Never retried (P4 R1-3).
                Err(e) if crate::client::is_sandbox_denied(&e) => return Err(e),
                Err(e) => {
                    if TokioInstant::now() + RETRY_PAUSE >= deadline {
                        return Err(e);
                    }
                }
            }
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    }
}

/// What the waiting line says besides the queue (L32, T7-1): the resource, the requester's
/// workspace (as `workspace.resolve` reports it), the takeover command (`hostler hold screen`,
/// `hostler hold account NAME`, `rerun with` for a run; `None` when the caller already passed
/// `--force`) and the resource's `--confirmation` token, which the caller knows from the resource
/// (`ResourceId::token`; P3b: the catalog's display), never parsed from `display`.
#[derive(Clone, Copy, Debug)]
pub struct WaitText<'a> {
    pub display: &'a str,
    pub own_workspace: &'a str,
    pub takeover: Option<&'a str>,
    pub token: &'a str,
    /// A device: a wait with no blocker means it is not registered or not connected.
    pub device: bool,
}

/// The waiting line of a claim. `{holder}` is the holding workspace (or `source: description`),
/// `{what}` the holder's kind and activity, or `being taken over by {taker}` during a takeover
/// (T7-2). The takeover command with the D9 sentence follows only for another workspace's claim
/// or a stoppable foreign claim, never during a takeover (T7-1); a wait behind this workspace's
/// own claim says so.
/// `unknown`: why agent-device sessions cannot be read, for a `Checking` blocker's fix text.
pub fn claim_wait_line(
    text: &WaitText<'_>,
    blockers: &[Blocker],
    position: u32,
    of: u32,
    waited: Duration,
    max_wait: Option<Duration>,
    unknown: Option<&str>,
) -> String {
    let (reason, hint) = reason_text(text, blockers, unknown);
    let limit = max_wait.map_or_else(|| "no limit".to_owned(), wait::duration_text);
    let mut line = format!(
        "[hostler] Waiting for {}: {reason}. You are {position} of {of}. Waited {} of \
         {limit} — this is waiting, not hanging; Ctrl+C withdraws the request.",
        sanitize_for_terminal(text.display),
        wait::duration_text(waited),
    );
    if let Some(hint) = hint {
        line.push('\n');
        line.push_str(&hint);
    }
    line
}

/// `--no-wait`'s line for a busy resource (CR-B-5): who holds it, and the takeover command when
/// one applies; it claims no wait. The caller adds the position and that it did not wait.
pub fn claim_busy_line(text: &WaitText<'_>, blockers: &[Blocker], unknown: Option<&str>) -> String {
    let (reason, hint) = reason_text(text, blockers, unknown);
    let mut line = format!(
        "[hostler] {} is {reason}.",
        sanitize_for_terminal(text.display)
    );
    if let Some(hint) = hint {
        line.push('\n');
        line.push_str(&hint);
    }
    line
}

/// Why a request waits: `held by …`; a sessions check, with its fix while the status cannot be
/// read (L52, L54); or, for a device with no blocker, that it is not there (P-14). Plus the
/// takeover line when one applies.
fn reason_text(
    text: &WaitText<'_>,
    blockers: &[Blocker],
    unknown: Option<&str>,
) -> (String, Option<String>) {
    let first = blockers
        .iter()
        .find(|b| !matches!(b, Blocker::Seizing { .. }));
    match first {
        Some(Blocker::Checking {
            resource,
            text: what,
        }) => {
            let mut reason = sanitize_for_terminal(what);
            // The service's text names the fix itself while it cannot read the status (P3bT9b-2).
            if let Some(why) = unknown.filter(|_| !what.contains("forget-sessions")) {
                reason.push_str(&format!(
                    " (agent-device status unreadable: {}; fix: hostler devices tools --detect, \
                     or hostler devices tools --agent-device <path>; a person may release this \
                     device: hostler devices forget-sessions {})",
                    sanitize_for_terminal(why),
                    sanitize_for_terminal(resource)
                ));
            }
            (reason, None)
        }
        // A Seizing-only list falls through to `holder_text`: "being taken over".
        None if text.device && blockers.is_empty() && text.token.is_empty() => (
            "no registered device matches and is free (see hostler devices list; without --boot \
             only booted emulators and simulators count)"
                .into(),
            None,
        ),
        None if text.device && blockers.is_empty() => (
            "it is not registered or not connected (see hostler devices list)".into(),
            None,
        ),
        _ => {
            let (held_by, hint) = holder_text(text, blockers);
            (format!("held by {held_by}"), hint)
        }
    }
}

/// `held by …` of the waiting and busy lines, and the takeover line when one applies.
fn holder_text(text: &WaitText<'_>, blockers: &[Blocker]) -> (String, Option<String>) {
    let seizing = blockers.iter().find_map(|b| match b {
        Blocker::Seizing { by_workspace } => Some(by_workspace.as_str()),
        _ => None,
    });
    let first = blockers
        .iter()
        .find(|b| !matches!(b, Blocker::Seizing { .. }));
    let (holder, mut what, since_ms) = describe(first);
    if let Some(taker) = seizing {
        what = format!("being taken over by {taker}");
    }
    let own = matches!(first, Some(Blocker::Claim { summary })
        if summary.workspace_path == text.own_workspace);
    let held = wait::duration_text(Duration::from_millis(
        (SystemClock.now_ms() - since_ms).max(0) as u64,
    ));
    let held_by = if own {
        format!("this workspace's {} ({held})", sanitize_for_terminal(&what))
    } else {
        format!(
            "{} ({}, {held})",
            sanitize_for_terminal(&holder),
            sanitize_for_terminal(&what)
        )
    };
    let takeable = blockers.iter().any(|b| match b {
        Blocker::Claim { summary } => summary.workspace_path != text.own_workspace,
        Blocker::Foreign { claim } => claim.stop.is_some(),
        // A takeover cannot end the wait for a fresh sessions read: no takeover line for it.
        // What `--force` answers then is Tasks 7 and 9b's (L54: a refusal with the
        // forget-sessions recovery, never a silent wait).
        Blocker::Seizing { .. } | Blocker::Checking { .. } => false,
    });
    let hint = text
        .takeover
        .filter(|_| takeable && seizing.is_none())
        .map(|takeover| {
            format!(
                "[hostler] To take it over: {takeover} --force --confirmation {}. \
                 {FORCE_SENTENCE}",
                sanitize_for_terminal(text.token)
            )
        });
    (held_by, hint)
}

/// The holder, its activity and when it started holding.
fn describe(blocker: Option<&Blocker>) -> (String, String, i64) {
    match blocker {
        Some(Blocker::Claim { summary }) => {
            let mut what = match summary.kind {
                ClaimKind::Lease => format!(
                    "run of {}, PID {}",
                    summary.command.as_deref().unwrap_or("a command"),
                    summary
                        .pid
                        .map_or_else(|| "?".to_owned(), |pid| pid.to_string())
                ),
                ClaimKind::Hold => match (summary.expires_at_ms, summary.pid) {
                    (Some(expires), _) => {
                        format!("hold {}, until {}", summary.id, hh_mm(expires))
                    }
                    (None, Some(pid)) => {
                        format!("hold {} while PID {pid} lives", summary.id)
                    }
                    (None, None) => format!("hold {}", summary.id),
                },
                ClaimKind::Reservation => format!("reservation {}", summary.id),
            };
            if summary.linked > 0 {
                what.push_str(&format!(", and {} joined run(s)", summary.linked));
            }
            (summary.workspace_path.clone(), what, summary.since_ms)
        }
        Some(Blocker::Foreign { claim }) => (
            format!("{}: {}", claim.source, claim.description),
            "an external claim".into(),
            claim.since_ms.unwrap_or_else(|| SystemClock.now_ms()),
        ),
        Some(Blocker::Seizing { by_workspace }) => (
            by_workspace.clone(),
            format!("being taken over by {by_workspace}"),
            SystemClock.now_ms(),
        ),
        // `reason_text` renders a leading one; here it is only ever behind another blocker.
        Some(Blocker::Checking { resource, text }) => {
            (resource.clone(), text.clone(), SystemClock.now_ms())
        }
        None => ("its holder".into(), "queued".into(), SystemClock.now_ms()),
    }
}

/// `HH:MM` in local time from Unix milliseconds.
pub(crate) fn hh_mm(ms: i64) -> String {
    local_hh_mm(ms).unwrap_or_else(|| "--:--".into())
}

#[cfg(unix)]
fn local_hh_mm(ms: i64) -> Option<String> {
    let seconds = ms.div_euclid(1000);
    let time: libc::time_t = seconds;
    // SAFETY: `localtime_r` writes a tm into the valid out-parameter and reads the time argument.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::localtime_r(&time, &mut tm) };
    (!ok.is_null()).then(|| format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
}

#[cfg(windows)]
fn local_hh_mm(ms: i64) -> Option<String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    #[repr(C)]
    struct SystemTimeParts {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(time: *mut SystemTimeParts);
    }
    let minute_of_day = |secs: i64| secs.rem_euclid(86_400) / 60;
    // SAFETY: `GetLocalTime` writes into the valid out-parameter.
    let mut local = SystemTimeParts {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    unsafe { GetLocalTime(&mut local) };
    let utc_now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let offset = (i64::from(local.hour) * 60 + i64::from(local.minute) - minute_of_day(utc_now))
        .rem_euclid(24 * 60);
    let local_minute = (minute_of_day(ms.div_euclid(1000)) + offset).rem_euclid(24 * 60);
    Some(format!("{:02}:{:02}", local_minute / 60, local_minute % 60))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::json;

    use super::*;
    use crate::contract::claims::ClaimSummary;

    /// CR-A-4: a request refused on a tick answers with that refusal, not as withdrawn.
    #[test]
    fn a_request_refused_on_a_tick_answers_its_refusal() {
        let finished = |reason, detail: Option<&str>| ClaimOutcome::Finished {
            item: crate::contract::claims::ClaimHistoryItem {
                id: "r1".into(),
                resource: "fake-0".into(),
                kind: ClaimKind::Hold,
                workspace_path: "/work/app".into(),
                command: None,
                granted_at_ms: None,
                ended_at_ms: 1,
                reason,
                exit_code: None,
                detail: detail.map(str::to_owned),
            },
        };
        match decided(
            finished(EndReason::Refused, Some("a distinct hold …")),
            "fake-0",
        ) {
            Err(Gave::Failed(e)) => {
                assert_eq!(e.code, ErrorCode::BadRequest);
                assert_eq!(e.message, "a distinct hold …");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            decided(finished(EndReason::Withdrawn, None), "fake-0"),
            Err(Gave::WithdrawnElsewhere)
        ));
    }

    fn summary(kind: ClaimKind) -> ClaimSummary {
        ClaimSummary {
            id: "h1".into(),
            resource: "screen".into(),
            kind,
            workspace_path: "/work/app".into(),
            pid: Some(42),
            command: Some("make test".into()),
            since_ms: SystemClock.now_ms() - 185_000,
            expires_at_ms: Some(7_200_000),
            linked: 0,
        }
    }

    fn blocker(kind: ClaimKind) -> Blocker {
        Blocker::Claim {
            summary: summary(kind),
        }
    }

    /// `hostler hold screen` from another workspace than the holder's (`/work/app`).
    const SCREEN: WaitText<'static> = WaitText {
        display: "screen",
        own_workspace: "/work/mine",
        takeover: Some("hostler hold screen"),
        token: "screen",
        device: false,
    };

    #[test]
    fn claim_wait_line_shapes() {
        let line = claim_wait_line(
            &SCREEN,
            &[blocker(ClaimKind::Lease)],
            2,
            3,
            Duration::from_secs(42),
            Some(Duration::from_secs(3600)),
            None,
        );
        assert!(line.starts_with(
            "[hostler] Waiting for screen: held by /work/app (run of make test, PID 42, 3 min). \
             You are 2 of 3. Waited 42 s of 60 min"
        ), "{line}");

        let mut hold = summary(ClaimKind::Hold);
        hold.expires_at_ms = Some(
            SystemClock.now_ms() + i64::try_from(Duration::from_secs(3600).as_millis()).unwrap(),
        );
        let line = claim_wait_line(
            &SCREEN,
            &[Blocker::Claim { summary: hold }],
            1,
            1,
            Duration::from_secs(5),
            None,
            None,
        );
        assert!(line.contains("hold h1, until "), "{line}");
        assert!(line.contains("Waited 5 s of no limit"), "{line}");
        assert!(
            line.contains(
                "[hostler] To take it over: hostler hold screen --force --confirmation screen. \
                 An agent uses --force only with the operator's explicit permission."
            ),
            "{line}"
        );

        let mut bound = summary(ClaimKind::Hold);
        bound.expires_at_ms = None;
        bound.pid = Some(77);
        let line = claim_wait_line(
            &SCREEN,
            &[Blocker::Claim { summary: bound }],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(line.contains("hold h1 while PID 77 lives"), "{line}");

        let mut joined = summary(ClaimKind::Hold);
        joined.linked = 2;
        joined.expires_at_ms = None;
        joined.pid = None;
        let line = claim_wait_line(
            &SCREEN,
            &[Blocker::Claim { summary: joined }],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(line.contains(", and 2 joined run(s)"), "{line}");
    }

    #[test]
    fn claim_wait_line_names_an_account_and_a_foreign_holder() {
        let mut account = summary(ClaimKind::Hold);
        account.resource = "account qa-user".into();
        let line = claim_wait_line(
            &WaitText {
                display: "account qa-user",
                own_workspace: "/work/mine",
                takeover: Some("hostler hold account qa-user"),
                token: "qa-user",
                device: false,
            },
            &[Blocker::Claim { summary: account }],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(line.contains("--confirmation qa-user"), "{line}");
        assert!(
            line.contains("hostler hold account qa-user --force"),
            "{line}"
        );

        let line = claim_wait_line(
            &SCREEN,
            &[Blocker::Foreign {
                claim: crate::contract::claims::fixtures::foreign(),
            }],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(
            line.contains("held by fake: agent-device session"),
            "{line}"
        );
        assert!(line.contains("To take it over"), "{line}");
        // An external claim hostler cannot stop is not offered for a takeover (T7-1).
        let mut unstoppable = crate::contract::claims::fixtures::foreign();
        unstoppable.stop = None;
        let line = claim_wait_line(
            &SCREEN,
            &[Blocker::Foreign { claim: unstoppable }],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(!line.contains("--force"), "{line}");
    }

    #[test]
    fn claim_wait_line_offers_a_takeover_only_from_another_workspace() {
        // L32, T7-1: behind this workspace's own claim there is nothing to take over.
        let own = WaitText {
            display: "screen",
            own_workspace: "/work/app",
            takeover: Some("rerun with"),
            token: "screen",
            device: false,
        };
        let line = claim_wait_line(
            &own,
            &[blocker(ClaimKind::Lease)],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(
            line.contains("held by this workspace's run of make test, PID 42 ("),
            "{line}"
        );
        assert!(!line.contains("--force"), "{line}");
        // A run's own takeover command, from another workspace.
        let run = WaitText {
            own_workspace: "/work/mine",
            ..own
        };
        let line = claim_wait_line(
            &run,
            &[blocker(ClaimKind::Lease)],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(
            line.contains(
                "[hostler] To take it over: rerun with --force --confirmation screen. An agent \
                 uses --force only with the operator's explicit permission."
            ),
            "{line}"
        );
        // The caller already passed --force: no hint to do what it just did.
        let forcing = WaitText {
            takeover: None,
            ..run
        };
        let line = claim_wait_line(
            &forcing,
            &[blocker(ClaimKind::Lease)],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(!line.contains("--force"), "{line}");
    }

    #[test]
    fn claim_busy_line_claims_no_wait() {
        // CR-B-5: `--no-wait` neither waits nor offers to withdraw.
        let line = claim_busy_line(&SCREEN, &[blocker(ClaimKind::Lease)], None);
        assert!(
            line.starts_with(
                "[hostler] screen is held by /work/app (run of make test, PID 42, 3 min)."
            ),
            "{line}"
        );
        assert!(
            !line.contains("waiting") && !line.contains("Ctrl+C"),
            "{line}"
        );
        assert!(
            line.contains("To take it over: hostler hold screen --force"),
            "{line}"
        );
    }

    #[test]
    fn claim_wait_line_during_a_takeover_names_the_holder_and_the_taker() {
        // T7-2: the service lists the seizure first.
        let line = claim_wait_line(
            &SCREEN,
            &[
                Blocker::Seizing {
                    by_workspace: "/work/taker".into(),
                },
                blocker(ClaimKind::Lease),
            ],
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
        assert!(
            line.contains("held by /work/app (being taken over by /work/taker, 3 min)"),
            "{line}"
        );
        assert!(!line.contains("--force"), "{line}");
    }

    struct FakeIo {
        answers: VecDeque<Result<Value, HostlerError>>,
        seen: Vec<Request>,
    }

    impl FakeIo {
        fn new(answers: Vec<Result<Value, HostlerError>>) -> FakeIo {
            FakeIo {
                answers: answers.into(),
                seen: Vec::new(),
            }
        }
    }

    impl ClaimsIo for FakeIo {
        async fn call(&mut self, request: Request) -> Result<Value, HostlerError> {
            self.seen.push(request);
            self.answers.pop_front().expect("an answer for every call")
        }

        async fn next_event(&mut self) -> Option<Event> {
            std::future::pending().await
        }

        async fn reconnect(&mut self, _within: Duration) -> Result<(), HostlerError> {
            Ok(())
        }
    }

    fn granted() -> Value {
        serde_json::to_value(ClaimOutcome::Granted {
            claim: crate::contract::claims::fixtures::claim(),
        })
        .unwrap()
    }

    fn queued() -> Value {
        serde_json::to_value(ClaimOutcome::Queued {
            request_id: "r1".into(),
            position: 1,
            of: 1,
            blockers: vec![blocker(ClaimKind::Lease)],
        })
        .unwrap()
    }

    #[tokio::test]
    async fn stop_wins_over_a_simultaneous_grant() {
        // The deadline is already gone once the request queues; the attach after the withdraw
        // would find the grant.
        let mut io = FakeIo::new(vec![
            Ok(queued()),  // claims.acquire
            Ok(json!({})), // claims.withdraw
            Ok(granted()), // claims.attach: the grant won the race
            Ok(json!({})), // claims.release
        ]);
        let req = crate::contract::claims::fixtures::request();
        let mut stop = Stop::expired_now();
        let gave = run(&mut io, &req, &SCREEN, &mut stop, false)
            .await
            .unwrap_err();
        assert!(matches!(gave, Gave::TimedOut), "{gave:?}");
        assert_eq!(io.seen.len(), 4, "{:?}", io.seen);
        assert!(matches!(io.seen[0], Request::ClaimsAcquire(_)));
        assert!(matches!(
            io.seen[1],
            Request::ClaimsWithdraw {
                timed_out: true,
                ..
            }
        ));
        assert!(matches!(io.seen[2], Request::ClaimsAttach { .. }));
        match &io.seen[3] {
            Request::ClaimsRelease(r) => assert_eq!(r.claim_id, "c1"),
            other => panic!("{other:?}"),
        }
        assert_eq!(stop.wait().await, Stopped::TimedOut);
    }

    /// One claims event, then silence; counts reconnects.
    struct DeniedAttachIo {
        answers: VecDeque<Result<Value, HostlerError>>,
        seen: Vec<Request>,
        event: Option<Event>,
        reconnects: usize,
    }

    impl ClaimsIo for DeniedAttachIo {
        async fn call(&mut self, request: Request) -> Result<Value, HostlerError> {
            self.seen.push(request);
            self.answers.pop_front().expect("an answer for every call")
        }

        async fn next_event(&mut self) -> Option<Event> {
            match self.event.take() {
                Some(event) => Some(event),
                None => std::future::pending().await,
            }
        }

        async fn reconnect(&mut self, _within: Duration) -> Result<(), HostlerError> {
            self.reconnects += 1;
            Ok(())
        }
    }

    /// P4 R2-1: an attach the sandbox denies ends the wait at once: no reconnect, no re-attach.
    #[tokio::test]
    async fn a_denied_attach_makes_no_reconnect() {
        let socket = crate::paths::SocketName::File("/tmp/x/hostler.sock".into());
        let denied = crate::client::sandbox_denied(
            &socket,
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let mut io = DeniedAttachIo {
            answers: vec![Ok(queued()), Err(denied)].into(),
            seen: Vec::new(),
            event: Some(Event::ClaimsChanged(
                crate::contract::claims::fixtures::snapshot(),
            )),
            reconnects: 0,
        };
        let req = crate::contract::claims::fixtures::request();
        let mut stop = Stop {
            since: Instant::now(),
            max_wait: Some(Duration::from_secs(60)),
            signals: None,
        };
        let gave = run(&mut io, &req, &SCREEN, &mut stop, false)
            .await
            .unwrap_err();
        match gave {
            Gave::Failed(e) => assert_eq!(e.details["reason"], "sandbox_denied", "{e:?}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(io.reconnects, 0, "a sandbox denial was retried");
        assert_eq!(io.seen.len(), 2, "{:?}", io.seen);
        assert!(matches!(io.seen[1], Request::ClaimsAttach { .. }));
    }

    #[tokio::test]
    async fn an_expired_deadline_still_takes_a_free_resource() {
        // L34: the deadline bounds waiting only; the acquire is always sent.
        let mut io = FakeIo::new(vec![Ok(granted())]);
        let req = crate::contract::claims::fixtures::request();
        let got = run(&mut io, &req, &SCREEN, &mut Stop::expired_now(), false).await;
        assert!(matches!(got, Ok(Acquired::Granted(_))), "{got:?}");
        assert_eq!(io.seen.len(), 1);
    }

    /// Never answers a call.
    struct SilentIo;

    impl ClaimsIo for SilentIo {
        async fn call(&mut self, _request: Request) -> Result<Value, HostlerError> {
            std::future::pending().await
        }

        async fn next_event(&mut self) -> Option<Event> {
            std::future::pending().await
        }

        async fn reconnect(&mut self, _within: Duration) -> Result<(), HostlerError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn give_up_takes_one_budget_for_all_its_calls() {
        // T7-9: withdraw and attach share the 2 s (two budgets would be 4 s).
        let req = crate::contract::claims::fixtures::request();
        let started = Instant::now();
        let gave = give_up(&mut SilentIo, &req, Stopped::Interrupted(130)).await;
        assert!(matches!(gave, Gave::Interrupted(130)), "{gave:?}");
        let took = started.elapsed();
        assert!(
            took >= WITHDRAW_BUDGET && took < Duration::from_millis(3500),
            "{took:?}"
        );
    }

    #[tokio::test]
    async fn a_grant_that_slipped_past_a_failed_takeover_is_kept() {
        // T8-4: the holder ended before `seize_begin`; the tick granted our request.
        let req = crate::contract::claims::fixtures::request();
        let refused = errors::seizure_failed("screen", "the request is no longer waiting");
        let mut io = FakeIo::new(vec![Ok(granted())]);
        let got = settle_failed(&mut io, &req, refused.clone()).await;
        assert!(matches!(got, Ok(Acquired::Granted(_))), "{got:?}");
        assert_eq!(io.seen.len(), 1, "{:?}", io.seen);
        // Interrupted: the grant is released, never kept behind.
        let mut stopped = refused.clone();
        stopped.code = ErrorCode::Withdrawn;
        stopped.details = json!({ "exit_code": 130 });
        let mut io = FakeIo::new(vec![Ok(granted()), Ok(json!({}))]);
        assert!(settle_failed(&mut io, &req, stopped).await.is_err());
        assert!(matches!(&io.seen[1], Request::ClaimsRelease(r) if r.claim_id == "c1"));
        // Still queued: withdrawn.
        let mut io = FakeIo::new(vec![Ok(queued()), Ok(json!({}))]);
        assert!(settle_failed(&mut io, &req, refused).await.is_err());
        assert!(matches!(io.seen[1], Request::ClaimsWithdraw { .. }));
    }

    #[test]
    fn takeover_exit_codes() {
        let mut interrupted = errors::seizure_failed("screen", "x");
        interrupted.code = ErrorCode::Withdrawn;
        interrupted.details = json!({ "interrupted": "SIGTERM", "exit_code": 143 });
        assert_eq!(takeover_exit_code(&interrupted), 143);
        let mut target = errors::seizure_failed("screen", "x");
        target.details["reason"] = "taker_is_a_target".into();
        assert_eq!(takeover_exit_code(&target), 78);
    }

    #[test]
    fn hh_mm_is_a_clock_time() {
        let text = hh_mm(SystemClock.now_ms());
        let (hour, minute) = text.split_once(':').unwrap();
        assert!(hour.parse::<u32>().unwrap() < 24, "{text}");
        assert!(minute.parse::<u32>().unwrap() < 60, "{text}");
    }

    fn checking() -> Blocker {
        Blocker::Checking {
            resource: "pixel_9".into(),
            text: "checking for agent-device sessions on pixel_9".into(),
        }
    }

    const DEVICE: WaitText<'static> = WaitText {
        display: "device pixel_9",
        own_workspace: "/work/mine",
        takeover: Some("rerun with"),
        token: "pixel_9",
        device: true,
    };

    #[test]
    fn checking_blocker_renders_with_the_fix_text() {
        let line = claim_wait_line(&DEVICE, &[checking()], 1, 1, Duration::ZERO, None, None);
        assert!(
            line.starts_with(
                "[hostler] Waiting for device pixel_9: checking for agent-device sessions on \
                 pixel_9. You are 1 of 1."
            ),
            "{line}"
        );
        // L54: no takeover offered; while unreadable, the fix and the person's escape.
        assert!(!line.contains("--force"), "{line}");
        let line = claim_wait_line(
            &DEVICE,
            &[checking()],
            1,
            1,
            Duration::ZERO,
            None,
            Some("node not found"),
        );
        assert!(
            line.contains(
                "checking for agent-device sessions on pixel_9 (agent-device status unreadable: \
                 node not found; fix: hostler devices tools --detect, or hostler devices tools \
                 --agent-device <path>; a person may release this device: hostler devices \
                 forget-sessions pixel_9)."
            ),
            "{line}"
        );
        let busy = claim_busy_line(&DEVICE, &[checking()], Some("node not found"));
        assert!(
            busy.starts_with("[hostler] device pixel_9 is checking for agent-device sessions"),
            "{busy}"
        );
        // P3bT9b-2: the service's text already names the fix: said once.
        let named = Blocker::Checking {
            resource: "pixel_9".into(),
            text: crate::claims::rules::checking_text("pixel_9", Some("node not found")),
        };
        let line = claim_wait_line(
            &DEVICE,
            &[named],
            1,
            1,
            Duration::ZERO,
            None,
            Some("node not found"),
        );
        assert_eq!(line.matches("forget-sessions").count(), 1, "{line}");
        assert_eq!(line.matches("tools --detect").count(), 1, "{line}");
    }

    #[test]
    fn a_device_wait_without_blockers_says_it_is_not_there() {
        // P-14: a device missing from the catalog, or not connected, has no blocker.
        let line = claim_wait_line(&DEVICE, &[], 1, 1, Duration::ZERO, None, None);
        assert!(
            line.contains(
                "Waiting for device pixel_9: it is not registered or not connected (see \
                 hostler devices list)."
            ),
            "{line}"
        );
        let line = claim_wait_line(&SCREEN, &[], 1, 1, Duration::ZERO, None, None);
        assert!(line.contains("held by its holder"), "{line}");
        // `auto` (no alias): no device matches.
        let auto = WaitText {
            display: "device auto",
            token: "",
            takeover: None,
            ..DEVICE
        };
        let line = claim_wait_line(&auto, &[], 1, 1, Duration::ZERO, None, None);
        assert!(
            line.contains(
                "Waiting for device auto: no registered device matches and is free (see hostler \
                 devices list; without --boot only booted emulators and simulators count)."
            ),
            "{line}"
        );
        // The holder's claims already ended, the commit still pending: being taken over.
        let seizing = [Blocker::Seizing {
            by_workspace: "/work/taker".into(),
        }];
        let line = claim_wait_line(&DEVICE, &seizing, 1, 1, Duration::ZERO, None, None);
        assert!(line.contains("being taken over by /work/taker"), "{line}");
        assert!(!line.contains("not connected"), "{line}");
    }

    fn busy_checking() -> Value {
        serde_json::to_value(ClaimOutcome::Busy {
            position: 1,
            of: 1,
            blockers: vec![checking()],
        })
        .unwrap()
    }

    fn sessions(known: bool) -> Value {
        json!({ "tools": { "sessions_known": known,
            "problems": ["agent-device sessions unknown: node not found"] } })
    }

    #[tokio::test]
    async fn no_wait_on_checking_only_waits_one_read_bound() {
        // Readable: asked again until the read clears the device.
        let mut io = FakeIo::new(vec![
            Ok(busy_checking()),
            Ok(sessions(true)),
            Ok(busy_checking()),
            Ok(granted()),
        ]);
        let mut req = crate::contract::claims::fixtures::request();
        req.no_wait = true;
        let mut stop = Stop::new(
            &Deadline {
                started: Instant::now(),
                max_wait: None,
            },
            None,
        );
        let got = run(&mut io, &req, &DEVICE, &mut stop, true).await;
        assert!(matches!(got, Ok(Acquired::Granted(_))), "{got:?}");
        // Unreadable: Busy at once.
        let mut io = FakeIo::new(vec![Ok(busy_checking()), Ok(sessions(false))]);
        let started = Instant::now();
        let got = run(&mut io, &req, &DEVICE, &mut stop, true).await;
        assert!(matches!(got, Err(Gave::Busy { .. })), "{got:?}");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(io.seen.len(), 2);
    }

    #[tokio::test]
    async fn no_wait_on_checking_gives_up_after_the_bound() {
        let mut answers = vec![Ok(busy_checking()), Ok(sessions(true))];
        answers.extend((0..60).map(|_| Ok(busy_checking())));
        let mut io = FakeIo::new(answers);
        let mut req = crate::contract::claims::fixtures::request();
        req.no_wait = true;
        let mut stop = Stop::new(
            &Deadline {
                started: Instant::now(),
                max_wait: None,
            },
            None,
        );
        let started = Instant::now();
        let got = run(&mut io, &req, &DEVICE, &mut stop, true).await;
        assert!(matches!(got, Err(Gave::Busy { .. })), "{got:?}");
        let took = started.elapsed();
        assert!(
            took >= CHECKING_NO_WAIT && took < Duration::from_secs(9),
            "{took:?}"
        );
    }
}
