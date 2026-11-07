//! `hostler secret request | requests | decline` (plan P2, Task 9).
//!
//! A request waits visibly, like P1's heavy wait: status lines on stderr, Ctrl+C and the
//! `--max-wait` deadline win over every answer. Events are only hints (the service coalesces
//! them), so the waiter re-sends its request id on every `secrets.*` event, every 5 s and after
//! every reconnect; a known id answers its recorded state even while the database is locked.

use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::time::Instant as TokioInstant;

use crate::cli::output::{self, report};
use crate::cli::secrets::{checked_name, current_project, internal};
use crate::cli::{spawn, wait};
use crate::client::Client;
use crate::contract::secrets::{Decline, RequestState, SecretRequest, SecretRequestInfo};
use crate::contract::wire::{Event, Request};
use crate::contract::{Envelope, ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::procs::current_identity;
use crate::secrets::{sanitize_for_terminal, valid_secret_name};
use crate::service::manager::OsManager;

const DEFAULT_MAX_WAIT_MIN: f64 = 10.0;
const POLL_EVERY: Duration = Duration::from_secs(5);
const RECONNECT_BUDGET: Duration = Duration::from_secs(10);
const RETRY_PAUSE: Duration = Duration::from_millis(200);
const WITHDRAW_BUDGET: Duration = Duration::from_secs(2);
const EXIT_TIMED_OUT: u8 = 75;
const EXIT_INTERRUPTED: u8 = 130;

/// Minutes, fractions allowed; 0 = no limit.
pub fn parse_minutes(s: &str) -> Result<f64, String> {
    match s.trim().parse::<f64>() {
        Ok(m) if m.is_finite() && m >= 0.0 => Ok(m),
        _ => Err("not a number of minutes (0 = no limit)".into()),
    }
}

/// `None` = no limit; too large to represent counts as no limit.
fn max_wait(minutes: Option<f64>) -> Option<Duration> {
    let minutes = minutes.unwrap_or(DEFAULT_MAX_WAIT_MIN);
    (minutes > 0.0)
        .then(|| Duration::try_from_secs_f64(minutes * 60.0).ok())
        .flatten()
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The waiting status line; `project` is agent-supplied, so it is sanitized here.
pub fn request_line(
    name: &str,
    project: &str,
    short_id: &str,
    waited: Duration,
    max_wait: Option<Duration>,
    locked: bool,
) -> String {
    let limit = max_wait.map_or_else(|| "no limit".to_owned(), wait::duration_text);
    let runs = if locked {
        format!(
            "The database is locked: a person runs hostler unlock, then hostler secret set {name}"
        )
    } else {
        format!("A person runs: hostler secret set {name}")
    };
    format!(
        "[hostler] Waiting for a person to provide {name} for project {} (request {short_id}). \
         Waited {} of {limit} — this is waiting, not hanging; Ctrl+C withdraws the request. \
         {runs}   (or declines: hostler secret decline {short_id})",
        sanitize_for_terminal(project),
        wait::duration_text(waited),
    )
}

pub fn main_request(
    name: String,
    reason: String,
    max_wait_min: Option<f64>,
    json: bool,
) -> ExitCode {
    if let Err(code) = checked_name(&name) {
        return code;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let error = internal(format!("cannot start the async runtime: {e}"));
            return ExitCode::from(finish(json, Err(error), 1));
        }
    };
    let code = runtime.block_on(request(name, reason, max_wait(max_wait_min), json));
    runtime.shutdown_background();
    ExitCode::from(code)
}

/// Prints the outcome (`--json`: one envelope on stdout) and returns `code`.
fn finish(json: bool, result: Result<SecretRequestInfo, HostlerError>, code: u8) -> u8 {
    if json {
        let envelope = match result {
            Ok(info) => Envelope {
                ok: true,
                data: Some(info),
                error: None,
            },
            Err(e) => Envelope {
                ok: false,
                data: None,
                error: Some(e),
            },
        };
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("an envelope always encodes")
        );
    } else if let Err(e) = &result {
        output::print_error(e);
    }
    code
}

/// The exit code and human output of a decided state.
fn decided(json: bool, info: SecretRequestInfo) -> u8 {
    let code = match info.state {
        RequestState::Stored => {
            if !json {
                println!("stored");
            }
            0
        }
        RequestState::Declined => {
            if !json {
                println!("declined");
            }
            1
        }
        RequestState::Withdrawn | RequestState::TimedOut => {
            // A status line: on stderr with `--json` too.
            eprintln!(
                "[hostler] The request for {} was withdrawn by an operator, or because its \
                 requester was gone; not waiting any more.",
                info.name
            );
            EXIT_TIMED_OUT
        }
        RequestState::Pending => unreachable!("a pending request is not decided"),
    };
    finish(json, Ok(info), code)
}

async fn request(name: String, reason: String, max_wait: Option<Duration>, json: bool) -> u8 {
    // Registered before anything is sent: a Ctrl+C from here on withdraws the request.
    let mut stop = Stop::new(max_wait);
    let _ = stop.now().await;
    let waiter = (|| {
        let project = current_project()?;
        let paths = Paths::resolve().map_err(|e| internal(format!("{e:#}")))?;
        // Before the request names this process (W-3: a non-elevated service on Windows must be
        // able to judge an elevated requester's liveness).
        spawn::let_service_observe_self();
        let table = spawn::process_table().map_err(|e| internal(format!("{e:#}")))?;
        let requester = current_identity(&table).map_err(|e| internal(format!("{e:#}")))?;
        Ok::<_, HostlerError>(Waiter {
            paths,
            client: None,
            req: SecretRequest {
                // Made once: every re-send is the same request.
                id: uuid::Uuid::new_v4().to_string(),
                project,
                name,
                reason,
                requester,
            },
        })
    })();
    let mut waiter = match waiter {
        Ok(w) => w,
        Err(e) => return finish(json, Err(e), 1),
    };
    // The first call: locked, unapproved or a bad reason is reported at once.
    let first = tokio::select! {
        biased;
        stopped = stop.wait() => return give_up(&mut waiter, stopped, max_wait, json, None).await,
        first = waiter.first() => first,
    };
    if let Some(stopped) = stop.now().await {
        let known = first
            .ok()
            .filter(|info| info.state != RequestState::Pending);
        return give_up(&mut waiter, stopped, max_wait, json, known).await;
    }
    let info = match first {
        Ok(info) => info,
        Err(e) => return finish(json, Err(e), 1),
    };
    if info.state != RequestState::Pending {
        return decided(json, info);
    }
    wait_for_answer(&mut waiter, &mut stop, info, max_wait, json).await
}

async fn wait_for_answer(
    waiter: &mut Waiter,
    stop: &mut Stop,
    info: SecretRequestInfo,
    max_wait: Option<Duration>,
    json: bool,
) -> u8 {
    let short = short_id(&waiter.req.id).to_owned();
    let mut locked = match locked_or_stop(waiter, stop).await {
        Ok(locked) => locked.unwrap_or(false),
        Err(stopped) => return give_up(waiter, stopped, max_wait, json, None).await,
    };
    let mut printed: Option<(Instant, bool)> = None;
    let mut poll = tokio::time::interval_at(TokioInstant::now() + POLL_EVERY, POLL_EVERY);
    let events_count = !events_ignored();
    loop {
        let now = Instant::now();
        let changed = printed.is_none_or(|(_, shown)| shown != locked);
        if printed.is_none_or(|(at, _)| wait::should_print(at, now, changed)) {
            let waited = now.saturating_duration_since(stop.since);
            eprintln!(
                "{}",
                request_line(
                    &waiter.req.name,
                    &info.project_name,
                    &short,
                    waited,
                    max_wait,
                    locked
                )
            );
            printed = Some((now, locked));
        }
        // The poll comes before events: a stream of queue events never starves it.
        let recheck = tokio::select! {
            biased;
            stopped = stop.wait() => return give_up(waiter, stopped, max_wait, json, None).await,
            _ = poll.tick() => true,
            event = next_event(&mut waiter.client) => match event {
                Some(Event::SecretRequestChanged(_) | Event::SecretsLockChanged(_)) => events_count,
                Some(Event::QueueChanged(_)) => false,
                None => {
                    // The connection is gone: reconnect (inside `ask`) and re-send.
                    waiter.client = None;
                    true
                }
            },
        };
        if !recheck {
            continue;
        }
        let answer = tokio::select! {
            biased;
            stopped = stop.wait() => return give_up(waiter, stopped, max_wait, json, None).await,
            answer = waiter.ask() => answer,
        };
        if let Some(stopped) = stop.now().await {
            let known = answer
                .ok()
                .filter(|info| info.state != RequestState::Pending);
            return give_up(waiter, stopped, max_wait, json, known).await;
        }
        match answer {
            Ok(answer) if answer.state == RequestState::Pending => {
                match locked_or_stop(waiter, stop).await {
                    Ok(Some(now_locked)) => locked = now_locked,
                    Ok(None) => {}
                    Err(stopped) => return give_up(waiter, stopped, max_wait, json, None).await,
                }
            }
            Ok(answer) => return decided(json, answer),
            Err(mut e) if e.code == ErrorCode::ServiceUnavailable => {
                e.message = "The hostler service is unavailable; the request may still be \
                             pending (hostler secret requests)."
                    .into();
                e.recovery = None;
                return finish(json, Err(e), 1);
            }
            Err(e) => return finish(json, Err(e), 1),
        }
    }
}

/// The lock state for the waiting line, or the stop if it comes first.
async fn locked_or_stop(waiter: &mut Waiter, stop: &mut Stop) -> Result<Option<bool>, Stopped> {
    tokio::select! {
        biased;
        stopped = stop.wait() => Err(stopped),
        locked = waiter.locked() => Ok(locked),
    }
}

/// Withdraws the request (2 s budget; a gone service reaps it once we are gone). `known` is an
/// answer that came with the stop: the request is already decided, nothing to withdraw.
async fn give_up(
    waiter: &mut Waiter,
    stopped: Stopped,
    max_wait: Option<Duration>,
    json: bool,
    known: Option<SecretRequestInfo>,
) -> u8 {
    let (timed_out, code) = match stopped {
        Stopped::TimedOut => (true, EXIT_TIMED_OUT),
        Stopped::Interrupted => (false, EXIT_INTERRUPTED),
    };
    let answer: Result<SecretRequestInfo, HostlerError> = match known {
        Some(info) => Ok(info),
        None => {
            let withdraw = Request::SecretRequestWithdraw {
                id: waiter.req.id.clone(),
                timed_out,
            };
            let overall = TokioInstant::now() + WITHDRAW_BUDGET;
            waiter
                .call_until(withdraw, WITHDRAW_BUDGET, Some(overall))
                .await
        }
    };
    let outcome = match &answer {
        Ok(info) if matches!(info.state, RequestState::Withdrawn | RequestState::TimedOut) => {
            "the request was withdrawn".to_owned()
        }
        Ok(info) => format!("the request was already {}", state_text(info.state)),
        Err(_) => "the request could not be withdrawn; it ends once the service sees this \
                   process gone (hostler secret requests)"
            .to_owned(),
    };
    // Status lines: on stderr with `--json` too.
    match stopped {
        Stopped::TimedOut => {
            let limit = max_wait.map_or_else(String::new, wait::duration_text);
            eprintln!(
                "[hostler] Gave up after {limit} waiting for {}; {outcome}.",
                waiter.req.name
            );
        }
        Stopped::Interrupted => eprintln!("[hostler] Interrupted; {outcome}."),
    }
    if json {
        finish(json, answer, code)
    } else {
        code
    }
}

fn state_text(state: RequestState) -> &'static str {
    match state {
        RequestState::Pending => "pending",
        RequestState::Stored => "stored",
        RequestState::Declined => "declined",
        RequestState::Withdrawn => "withdrawn",
        RequestState::TimedOut => "timed out",
    }
}

/// The request and the connection it is made over.
struct Waiter {
    paths: Paths,
    client: Option<Client>,
    req: SecretRequest,
}

impl Waiter {
    /// The first `secret.request`: one connection (the service started if needed), no retry.
    async fn first(&mut self) -> Result<SecretRequestInfo, HostlerError> {
        let client = Client::connect_or_start(&self.paths, &OsManager).await?;
        let client = self.client.insert(client);
        decode(
            client
                .call(Request::SecretRequest(self.req.clone()))
                .await?,
        )
    }

    /// The request's recorded state, re-sent under the same id; 10 s to reconnect.
    async fn ask(&mut self) -> Result<SecretRequestInfo, HostlerError> {
        self.call_until(
            Request::SecretRequest(self.req.clone()),
            RECONNECT_BUDGET,
            None,
        )
        .await
    }

    /// Locked or missing database, for the waiting line; one attempt on the current connection.
    async fn locked(&mut self) -> Option<bool> {
        let value = self
            .client
            .as_mut()?
            .call(Request::SecretsStatus)
            .await
            .ok()?;
        let state = value.get("state")?.as_str()?;
        Some(state != "unlocked")
    }

    /// Sends `request`; on unavailability reconnects and sends it again. `reconnect` bounds
    /// only the reconnecting, from the first failure: a call on a live connection keeps its own
    /// bound (`call_bound`, 60 s for `secret.request`), since the service may answer late behind
    /// vault work. `overall` (if any) bounds everything. Every request here is idempotent. A
    /// reconnect never starts a service: a person may have stopped it, and a service started here
    /// would take the lock a `service restart` waits for.
    async fn call_until<T: DeserializeOwned>(
        &mut self,
        request: Request,
        reconnect: Duration,
        overall: Option<TokioInstant>,
    ) -> Result<T, HostlerError> {
        let mut reconnect_until: Option<TokioInstant> = None;
        let mut last = None;
        loop {
            let (paths, slot) = (&self.paths, &mut self.client);
            let attempt = async {
                let client = match slot {
                    Some(client) => client,
                    None => slot.insert(Client::connect(paths).await?),
                };
                client.call(request.clone()).await
            };
            let result = match overall {
                Some(at) => tokio::time::timeout_at(at, attempt).await.ok(),
                None => Some(attempt.await),
            };
            match result {
                Some(Err(e)) if e.code == ErrorCode::ServiceUnavailable => last = Some(e),
                Some(result) => return result.and_then(decode),
                None => {}
            }
            self.client = None;
            let until = *reconnect_until.get_or_insert_with(|| TokioInstant::now() + reconnect);
            let until = overall.map_or(until, |at| at.min(until));
            if TokioInstant::now() + RETRY_PAUSE >= until {
                return Err(last.unwrap_or_else(|| HostlerError {
                    code: ErrorCode::ServiceUnavailable,
                    message: "the hostler service did not answer in time".into(),
                    details: json!({ "reason": "timeout" }),
                    recovery: Some("hostler service run".into()),
                }));
            }
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, HostlerError> {
    serde_json::from_value(value)
        .map_err(|e| internal(format!("unreadable answer from the service: {e}")))
}

/// Debug builds: `HOSTLER_TEST_REQUEST_IGNORE_EVENTS=1` drops every pushed event, as a service
/// that coalesces events may; only the poll and reconnects re-check then.
fn events_ignored() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var("HOSTLER_TEST_REQUEST_IGNORE_EVENTS").as_deref() == Ok("1") {
        return true;
    }
    false
}

/// The next pushed event; never resolves without a connection (the poll reconnects).
async fn next_event(client: &mut Option<Client>) -> Option<Event> {
    match client {
        Some(client) => client.next_event().await,
        None => std::future::pending().await,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stopped {
    TimedOut,
    Interrupted,
}

/// Ctrl+C and the `--max-wait` deadline; one listener for the whole command.
struct Stop {
    since: Instant,
    max_wait: Option<Duration>,
    interrupt: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>,
    can_interrupt: bool,
}

impl Stop {
    fn new(max_wait: Option<Duration>) -> Stop {
        Stop {
            since: Instant::now(),
            max_wait,
            interrupt: Box::pin(tokio::signal::ctrl_c()),
            can_interrupt: true,
        }
    }

    /// Resolves at Ctrl+C or at the deadline. Cancel-safe.
    async fn wait(&mut self) -> Stopped {
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
        loop {
            tokio::select! {
                result = &mut self.interrupt, if self.can_interrupt => {
                    if result.is_ok() {
                        return Stopped::Interrupted;
                    }
                    self.can_interrupt = false;
                }
                () = &mut expired => return Stopped::TimedOut,
            }
        }
    }

    /// A stop that has already happened; never waits. The first call registers Ctrl+C.
    async fn now(&mut self) -> Option<Stopped> {
        tokio::select! {
            biased;
            result = &mut self.interrupt, if self.can_interrupt => {
                if result.is_ok() {
                    return Some(Stopped::Interrupted);
                }
                self.can_interrupt = false;
            }
            () = std::future::ready(()) => {}
        }
        wait::deadline_reached(self.since, Instant::now(), self.max_wait)
            .then_some(Stopped::TimedOut)
    }
}

pub fn main_requests(json: bool) -> ExitCode {
    report(
        crate::cli::secrets::call::<Vec<SecretRequestInfo>>(Request::SecretRequests),
        json,
        print_requests,
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn print_requests(requests: &Vec<SecretRequestInfo>) {
    if requests.is_empty() {
        println!("no pending secret requests");
        return;
    }
    let now = now_ms();
    for r in requests {
        let age = Duration::from_millis((now - r.created_at_ms).max(0) as u64);
        println!(
            "{}  {}  {}  {} ago  {}",
            short_id(&r.id),
            sanitize_for_terminal(&r.project_name),
            r.name,
            wait::duration_text(age),
            sanitize_for_terminal(&r.reason)
        );
    }
}

/// `<id|NAME>`: a valid secret NAME declines every pending request for it in this project;
/// anything else is a request id (8 or more characters of it).
pub fn main_decline(target: String, json: bool) -> ExitCode {
    let result = (|| {
        let decline = if valid_secret_name(&target) {
            Decline {
                id: None,
                project: Some(current_project()?),
                name: Some(target),
            }
        } else if target.len() >= 8
            && target.len() <= 36
            && target.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        {
            Decline {
                id: Some(target.to_ascii_lowercase()),
                project: None,
                name: None,
            }
        } else {
            return Err(crate::contract::secrets::errors::bad_request(
                "give a request id (at least 8 characters of it, see hostler secret requests) or \
                 a secret NAME"
                    .into(),
            ));
        };
        crate::cli::secrets::call::<Vec<SecretRequestInfo>>(Request::SecretDecline(decline))
    })();
    report(result, json, print_declined)
}

fn print_declined(declined: &Vec<SecretRequestInfo>) {
    for r in declined {
        println!(
            "declined {} ({} for project {})",
            short_id(&r.id),
            r.name,
            sanitize_for_terminal(&r.project_name)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_line_shapes() {
        let limit = request_line(
            "API_KEY",
            "app",
            "1a2b3c4d",
            Duration::from_secs(42),
            Some(Duration::from_secs(600)),
            false,
        );
        assert_eq!(
            limit,
            "[hostler] Waiting for a person to provide API_KEY for project app (request \
             1a2b3c4d). Waited 42 s of 10 min — this is waiting, not hanging; Ctrl+C withdraws \
             the request. A person runs: hostler secret set API_KEY   (or declines: hostler \
             secret decline 1a2b3c4d)"
        );
        let unbounded = request_line(
            "API_KEY",
            "app",
            "1a2b3c4d",
            Duration::from_secs(61),
            None,
            false,
        );
        assert!(
            unbounded.contains("Waited 1 min of no limit — "),
            "{unbounded}"
        );
        let locked = request_line(
            "API_KEY",
            "app",
            "1a2b3c4d",
            Duration::ZERO,
            Some(Duration::from_secs(60)),
            true,
        );
        assert!(
            locked.ends_with(
                "Ctrl+C withdraws the request. The database is locked: a person runs hostler \
                 unlock, then hostler secret set API_KEY   (or declines: hostler secret decline \
                 1a2b3c4d)"
            ),
            "{locked}"
        );
        assert!(!locked.contains("A person runs:"));
        // The project name is agent-supplied.
        let hostile = request_line("A", "x\u{1b}[2Jy", "1a2b3c4d", Duration::ZERO, None, false);
        assert!(
            hostile.contains("project x\u{FFFD}[2Jy (request"),
            "{hostile}"
        );
    }

    #[test]
    fn max_wait_defaults_to_10_min_and_0_is_no_limit() {
        assert_eq!(max_wait(None), Some(Duration::from_secs(600)));
        assert_eq!(max_wait(Some(0.0)), None);
        assert_eq!(max_wait(Some(0.02)), Some(Duration::from_millis(1200)));
        assert_eq!(max_wait(Some(f64::MAX)), None);
        assert!(parse_minutes("1.5").is_ok());
        assert!(parse_minutes("-1").is_err());
        assert!(parse_minutes("soon").is_err());
        assert!(parse_minutes("inf").is_err());
    }

    #[test]
    fn short_ids_are_8_characters() {
        assert_eq!(short_id("1a2b3c4d-0000-4000-8000-000000000000"), "1a2b3c4d");
        assert_eq!(short_id("abc"), "abc");
    }
}
