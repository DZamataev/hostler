//! `hostler run --heavy` (plan Task 8): take a slot in the machine-wide heavy queue, run the
//! command in its own run group, record its exit. The heavy queue fails open: when the service
//! cannot be reached, started or answered in time, the command runs unqueued after one warning.
//! `--secret` / `--secret-file` (P2 Task 10, `secret_run`) inject values first and mask the
//! command's output; they fail closed (exit 78) and compose with `--heavy`.
//!
//! Debug-only test hooks read here: `HOSTLER_TEST_KILL_BEFORE_GATE=1` (die at once after spawning
//! the gate launcher, before registering; a secret-file run has recorded the command),
//! `HOSTLER_TEST_KILL_BEFORE_RECORD=1` (an unqueued or re-entrant secret-file run dies between
//! the launch and recording the command), `HOSTLER_TEST_KILL_AFTER_SPAWN=1` (a secret-file run
//! dies right after the gate opened), `HOSTLER_TEST_DELAY_RELEASE_MS=<ms>` (sleep before the
//! heavy release),
//! `HOSTLER_TEST_DELAY_REATTACH_MS=<ms>` (sleep before every reattach),
//! `HOSTLER_TEST_PAUSE_BEFORE_SPAWN_UNTIL=<path>` (after the last stop check, before a granted
//! command is spawned: create `<path>.reached`, then block until `<path>` exists, ≤ 30 s),
//! `HOSTLER_TEST_DELAY_STARTED_MS=<ms>` (sleep after the
//! spawn, before `heavy.started`), in the Windows gate launcher
//! `HOSTLER_TEST_GATE_INTERRUPT_AFTER_MS=<ms>` (record a Ctrl+C that long after the command
//! started, as the console handler would) and, in `spawn`, `HOSTLER_TEST_NO_PROCESS_TABLE=1`
//! (process identities are unreadable).

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use serde_json::{Value, json};
use tokio::time::Instant as TokioInstant;

use super::secret_run::{self, ChildIo, EXIT_REFUSED};
use super::spawn::{self, Group, TOKEN_ENV};
use super::wait::{self, Holder, WaitStatus};
use crate::client::Client;
use crate::contract::heavy::{
    AcquireOutcome, HeavyAcquire, HeavyEntry, HeavyOutcome, HeavyRelease, HeavyStarted,
    QueueSnapshot,
};
use crate::contract::wire::{Event, Request};
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::{
    Clock, MAX_COMMAND_BYTES, MAX_PATH_BYTES, SystemClock, json_len, truncate_to_json_len,
};
use crate::heavy::queue;
use crate::paths::Paths;
use crate::procs::{current_identity, identity_of};
use crate::secrets::rundir::RunDir;
use crate::service::manager::OsManager;

/// Timed out, refused by `--no-wait`, or withdrawn before it ran (rn-looky's convention).
const EXIT_QUEUE: i32 = 75;
const EXIT_INTERRUPTED: i32 = 130;
const EXIT_CANNOT_RUN: i32 = 127;
const EXIT_USAGE: i32 = 2;
const DEFAULT_MAX_WAIT_MIN: f64 = 60.0;
const MAX_WAIT_ENV: &str = "HOSTLER_HEAVY_MAX_WAIT";
/// Reconnect-and-retry attempts for one request (or one recovery) give up after this, in total.
const RETRY_BUDGET: Duration = Duration::from_secs(10);
const RETRY_PAUSE: Duration = Duration::from_millis(200);
/// A withdrawal on Ctrl+C or timeout is not worth a long wait: unwithdrawn, the entry is reaped.
const WITHDRAW_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("resource")
        .required(true)
        .multiple(true)
        .args(["heavy", "secret", "secret_file"])
))]
pub struct RunArgs {
    /// Wait for a slot in the machine-wide heavy queue first; KIND defaults to `other`.
    #[arg(
        long,
        value_name = "KIND",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "other",
        value_parser = parse_kind
    )]
    heavy: Option<String>,
    /// Minutes to wait for a slot (fractions allowed; 0 = no limit). Default:
    /// $HOSTLER_HEAVY_MAX_WAIT, else 60.
    #[arg(long, value_name = "MIN", value_parser = parse_minutes, requires = "heavy")]
    max_wait: Option<f64>,
    /// Exit 75 at once instead of waiting when no slot is free.
    #[arg(long, requires = "heavy")]
    no_wait: bool,
    /// Put the project's secret NAME into the environment variable ENV (default: its `env` in
    /// hostler.toml, else NAME). The command's stdout and stderr are masked. Repeatable.
    // Hyphen values too: whatever follows reaches the non-echoing check (T10-1).
    #[arg(long, value_name = "NAME[=ENV]", allow_hyphen_values = true)]
    secret: Vec<String>,
    /// Write the project's secret NAME to a private file, removed after the command, and put
    /// the file's path into ENV (default as for --secret). Repeatable.
    #[arg(long, value_name = "NAME[=ENV]", allow_hyphen_values = true)]
    secret_file: Vec<String>,
    /// One argument is a shell line (`sh -c` / `cmd /C`); several are a program and its arguments.
    #[arg(last = true, required = true, value_name = "CMD")]
    command: Vec<OsString>,
}

fn parse_kind(s: &str) -> Result<String, String> {
    let ok = (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(s.to_owned())
    } else {
        Err("a kind matches ^[a-z0-9-]{1,32}$".into())
    }
}

fn parse_minutes(s: &str) -> Result<f64, String> {
    match s.trim().parse::<f64>() {
        Ok(m) if m.is_finite() && m >= 0.0 => Ok(m),
        _ => Err(format!("{s:?} is not a number of minutes (0 = no limit)")),
    }
}

/// `None` = no limit.
fn max_wait(cli: Option<f64>) -> Result<Option<Duration>, String> {
    let minutes = match cli {
        Some(m) => m,
        None => match std::env::var(MAX_WAIT_ENV) {
            Ok(v) => parse_minutes(&v).map_err(|e| format!("{MAX_WAIT_ENV}: {e}"))?,
            Err(std::env::VarError::NotPresent) => DEFAULT_MAX_WAIT_MIN,
            Err(e) => return Err(format!("{MAX_WAIT_ENV}: {e}")),
        },
    };
    // Too large to represent counts as no limit.
    Ok((minutes > 0.0)
        .then(|| Duration::try_from_secs_f64(minutes * 60.0).ok())
        .flatten())
}

fn command_argv(mut command: Vec<OsString>) -> Vec<OsString> {
    if command.len() != 1 {
        return command;
    }
    let line = command.remove(0);
    #[cfg(unix)]
    return vec!["sh".into(), "-c".into(), line];
    #[cfg(windows)]
    return vec!["cmd".into(), "/C".into(), line];
}

/// Marks a cut display string.
const CUT: &str = "…";

/// Keeps the head within `max` JSON bytes (the service's measure); a cut is marked with `…`.
fn keep_head(s: &str, max: usize) -> String {
    if json_len(s) <= max {
        return s.to_owned();
    }
    format!("{}{CUT}", truncate_to_json_len(s, max - json_len(CUT)))
}

/// Keeps the tail within `max` JSON bytes; a cut is marked with `…`.
fn keep_tail(s: &str, max: usize) -> String {
    if json_len(s) <= max {
        return s.to_owned();
    }
    let budget = max - json_len(CUT);
    let (mut used, mut start) = (0, s.len());
    for (i, c) in s.char_indices().rev() {
        used += json_len(c.encode_utf8(&mut [0; 4]));
        if used > budget {
            break;
        }
        start = i;
    }
    format!("{CUT}{}", &s[start..])
}

pub fn main(args: RunArgs) -> i32 {
    if !args.secret.is_empty() || !args.secret_file.is_empty() {
        crate::service::harden::harden_value_holder();
    }
    let max_wait = match max_wait(args.max_wait) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[hostler] {e}");
            return EXIT_USAGE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("[hostler] cannot start the async runtime: {e}");
            return 1;
        }
    };
    let code = runtime.block_on(run(args, max_wait));
    runtime.shutdown_background();
    code
}

/// The signals registered first by a secret run, else a fresh registration.
fn signals_now(
    early: &mut Option<std::io::Result<spawn::Signals>>,
) -> std::io::Result<spawn::Signals> {
    early.take().unwrap_or_else(spawn::Signals::register)
}

async fn run(args: RunArgs, max_wait: Option<Duration>) -> i32 {
    let display = args
        .command
        .iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let argv = command_argv(args.command);
    // Inside a secret run the command line may hold inherited values this run cannot mask:
    // only a placeholder leaves the process (heavy queue, history, materialization; CR-1).
    let display = if secret_run::inside_secret_run() {
        secret_run::INSIDE_SECRET_RUN.to_owned()
    } else {
        display
    };
    // Before any request names this process as the holder (and so a non-elevated service can
    // judge a secret run's liveness when it sweeps `runs/`, W-3).
    spawn::let_service_observe_self();
    // Generated once and reused for every retry: a known request is never re-acquired under a
    // new id. A secret run uses it as its run id too.
    let id = uuid::Uuid::new_v4().to_string();
    let secrets = !args.secret.is_empty() || !args.secret_file.is_empty();
    // Before any value is held: from here a signal is seen (and the files removed), not fatal.
    let mut early = secrets.then(spawn::Signals::register);
    let (io, files, display) = if secrets {
        match secret_run::setup(&args.secret, &args.secret_file, &id, &display).await {
            Ok(prepared) => (prepared.io, prepared.files, prepared.display),
            Err(code) => return code,
        }
    } else {
        (ChildIo::default(), None, display)
    };
    // Owned by the spawn path that runs the command (removed once its output is drained), else
    // dropped when `run` returns: the secret files are removed on every path.
    let Some(kind) = args.heavy else {
        return run_unqueued(argv, signals_now(&mut early), &io, files, Group::Own).await;
    };
    let mut heavy = match Heavy::prepare(id, kind, &display, args.no_wait) {
        Ok(heavy) => heavy,
        Err(e) => {
            warn_unavailable(&format!("{e:#}"));
            return run_unqueued(argv, signals_now(&mut early), &io, files, Group::Own).await;
        }
    };
    match Client::connect_or_start(&heavy.paths, &OsManager).await {
        Ok(client) => heavy.client = Some(client),
        Err(e) => {
            warn_fail_open(&e);
            return run_unqueued(argv, signals_now(&mut early), &io, files, Group::Own).await;
        }
    }
    let mut outcome = heavy
        .outcome(Request::HeavyAcquire(heavy.req.clone()))
        .await;
    // Armed at the first wait; from then on Ctrl+C and --max-wait win over every answer.
    let mut stop: Option<Stop> = None;
    let outcome = loop {
        match outcome {
            Ok(AcquireOutcome::Queued { position, of, .. }) => {
                let stop = stop.get_or_insert_with(|| Stop::new(max_wait, early.take()));
                outcome = match wait_for_slot(&mut heavy, stop, position, of).await {
                    ControlFlow::Continue(next) => next,
                    ControlFlow::Break(stopped) => {
                        return give_up(&mut heavy, stopped, max_wait).await;
                    }
                };
            }
            decided => break decided,
        }
    };
    // Registered before the last stop check and owned until the command ends: an interrupt at
    // any instant stops the request here, keeps the gate shut, or reaches the command.
    // A secret run's early signals were registered before all of this and are kept throughout.
    let reentrant = matches!(outcome, Ok(AcquireOutcome::Reentrant));
    let interrupts = (reentrant && !secrets).then(spawn::ignore_interrupts);
    let signals = (!reentrant && !secrets).then(spawn::Signals::register);
    if let Some(stop) = stop.as_mut()
        && let Some(stopped) = stop.now().await
    {
        return give_up(&mut heavy, stopped, max_wait).await;
    }
    if let Some(held) = stop.as_mut().and_then(Stop::take_signals) {
        early = Some(held);
    }
    let signals = || signals.or(early).unwrap_or_else(spawn::Signals::register);
    match outcome {
        Err(e) if e.code == ErrorCode::QueueBusy => {
            eprintln!("[hostler] {}", e.message);
            EXIT_QUEUE
        }
        Err(e) => {
            warn_fail_open(&e);
            run_unqueued(argv, signals(), &io, files, Group::Own).await
        }
        Ok(AcquireOutcome::Granted { .. }) => {
            if let Some(stop) = &stop {
                let waited = Instant::now().saturating_duration_since(stop.since);
                eprintln!(
                    "[hostler] Slot granted after {}.",
                    wait::duration_text(waited)
                );
            }
            run_granted(&mut heavy, argv, signals(), &io, files).await
        }
        // A secret run: behind a gate in the enclosing group, recorded before it can run, its
        // early signals kept (T10-2/3/4).
        Ok(AcquireOutcome::Reentrant) if secrets => {
            run_unqueued(argv, signals(), &io, files, Group::Enclosing).await
        }
        Ok(AcquireOutcome::Reentrant) => {
            run_direct(argv, interrupts.unwrap_or_else(spawn::ignore_interrupts)).await
        }
        Ok(AcquireOutcome::Busy { position, of }) => {
            eprintln!(
                "[hostler] The heavy slot is busy (you would be {position} of {of}); not \
                 waiting (--no-wait)."
            );
            EXIT_QUEUE
        }
        Ok(AcquireOutcome::Finished { .. }) => {
            eprintln!("[hostler] The request was withdrawn by an operator; not running.");
            EXIT_QUEUE
        }
        Ok(AcquireOutcome::Queued { .. }) => {
            unreachable!("the wait loop decides every queued request")
        }
    }
}

fn warn_unavailable(reason: &str) {
    eprintln!(
        "[hostler] Warning: the hostler service is unavailable ({reason}); running without the \
         heavy queue."
    );
}

fn warn_fail_open(e: &HostlerError) {
    if e.code == ErrorCode::ContractMismatch {
        eprintln!(
            "[hostler] Warning: {}; running without the heavy queue.",
            e.message
        );
        if let Some(recovery) = &e.recovery {
            eprintln!("[hostler] To fix: {recovery}");
        }
    } else {
        warn_unavailable(&e.message);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stopped {
    Interrupted,
    TimedOut,
}

/// Ctrl+C and the `--max-wait` deadline, armed from the first wait until the request is decided.
/// One listener for the whole wait: a SIGINT between two waits is never lost. A secret run's
/// listener is its early `Signals` (all four signals, registered before materialization), so
/// an interrupt that came before the wait is seen too; `take_signals` hands them on.
struct Stop {
    since: Instant,
    max_wait: Option<Duration>,
    interrupt: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>,
    can_interrupt: bool,
    signals: Option<std::io::Result<spawn::Signals>>,
}

impl Stop {
    fn new(max_wait: Option<Duration>, signals: Option<std::io::Result<spawn::Signals>>) -> Stop {
        let interrupt: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>> =
            if matches!(signals, Some(Ok(_))) {
                Box::pin(std::future::pending())
            } else {
                Box::pin(tokio::signal::ctrl_c())
            };
        Stop {
            since: Instant::now(),
            max_wait,
            interrupt,
            can_interrupt: true,
            signals,
        }
    }

    fn take_signals(&mut self) -> Option<std::io::Result<spawn::Signals>> {
        self.signals.take()
    }

    /// Resolves at Ctrl+C (any of a secret run's signals) or at the deadline. Cancel-safe.
    async fn wait(&mut self) -> Stopped {
        // An unrepresentable deadline is no deadline.
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
                () = early_signal(&mut self.signals) => return Stopped::Interrupted,
                () = &mut expired => return Stopped::TimedOut,
            }
        }
    }

    /// A stop that has already happened; never waits.
    async fn now(&mut self) -> Option<Stopped> {
        tokio::select! {
            biased;
            result = &mut self.interrupt, if self.can_interrupt => {
                if result.is_ok() {
                    return Some(Stopped::Interrupted);
                }
                self.can_interrupt = false;
            }
            () = early_signal(&mut self.signals) => return Some(Stopped::Interrupted),
            () = std::future::ready(()) => {}
        }
        wait::deadline_reached(self.since, Instant::now(), self.max_wait)
            .then_some(Stopped::TimedOut)
    }
}

/// Resolves at the next signal of a secret run's early `Signals`; never without them.
async fn early_signal(signals: &mut Option<std::io::Result<spawn::Signals>>) {
    if let Some(Ok(signals)) = signals
        && signals.recv().await.is_some()
    {
        return;
    }
    std::future::pending().await
}

/// Withdraws the request (briefly: if the service is gone the entry is reaped after we exit).
async fn give_up(heavy: &mut Heavy, stopped: Stopped, max_wait: Option<Duration>) -> i32 {
    let (timed_out, code) = match stopped {
        Stopped::TimedOut => {
            let limit = max_wait.map_or_else(String::new, wait::duration_text);
            eprintln!(
                "[hostler] Gave up after {limit} in the heavy queue; the request was withdrawn."
            );
            (true, EXIT_QUEUE)
        }
        Stopped::Interrupted => {
            eprintln!("[hostler] Interrupted; the request was withdrawn.");
            (false, EXIT_INTERRUPTED)
        }
    };
    let withdraw = Request::HeavyWithdraw {
        id: heavy.req.id.clone(),
        timed_out,
    };
    let _ = heavy
        .call_until(withdraw, TokioInstant::now() + WITHDRAW_BUDGET)
        .await;
    code
}

/// Sends one request until `deadline`: `Heavy` over the socket, a fake in tests.
trait Outcomes {
    async fn outcome_until(
        &mut self,
        request: Request,
        deadline: TokioInstant,
    ) -> Result<AcquireOutcome, HostlerError>;
}

/// The request's current state, asked again under the same id if the service lost it. One
/// 10 s deadline covers the whole recovery.
async fn recover(
    to: &mut impl Outcomes,
    req: &HeavyAcquire,
) -> Result<AcquireOutcome, HostlerError> {
    let deadline = TokioInstant::now() + RETRY_BUDGET;
    let attach = Request::HeavyAttach { id: req.id.clone() };
    match to.outcome_until(attach, deadline).await {
        Err(e) if e.code == ErrorCode::UnknownEntry => {
            to.outcome_until(Request::HeavyAcquire(req.clone()), deadline)
                .await
        }
        other => other,
    }
}

/// One heavy request and the connection it is made over.
struct Heavy {
    paths: Paths,
    client: Option<Client>,
    req: HeavyAcquire,
}

impl Outcomes for Heavy {
    async fn outcome_until(
        &mut self,
        request: Request,
        deadline: TokioInstant,
    ) -> Result<AcquireOutcome, HostlerError> {
        let value = self.call_until(request, deadline).await?;
        serde_json::from_value(value).map_err(|e| HostlerError {
            code: ErrorCode::Internal,
            message: format!("unreadable heavy-queue answer: {e}"),
            details: json!({}),
            recovery: None,
        })
    }
}

impl Heavy {
    fn prepare(id: String, kind: String, display: &str, no_wait: bool) -> anyhow::Result<Heavy> {
        let paths = Paths::resolve()?;
        let holder = current_identity(&spawn::process_table()?)?;
        let cwd = std::env::current_dir()?;
        Ok(Heavy {
            paths,
            client: None,
            req: HeavyAcquire {
                id,
                kind,
                command: keep_head(display, MAX_COMMAND_BYTES),
                cwd: keep_tail(&cwd.to_string_lossy(), MAX_PATH_BYTES),
                workspace_root: workspace_root().map(|r| keep_tail(&r, MAX_PATH_BYTES)),
                holder,
                reentrant_token: std::env::var(TOKEN_ENV).ok(),
                no_wait,
            },
        })
    }

    /// `call_until` with a fresh 10 s budget.
    async fn call(&mut self, request: Request) -> Result<Value, HostlerError> {
        self.call_until(request, TokioInstant::now() + RETRY_BUDGET)
            .await
    }

    /// Sends `request`; on unavailability reconnects (starting the service if needed) and sends
    /// it again, until `deadline`. Every heavy request is idempotent.
    async fn call_until(
        &mut self,
        request: Request,
        deadline: TokioInstant,
    ) -> Result<Value, HostlerError> {
        let mut last = None;
        loop {
            let (paths, slot) = (&self.paths, &mut self.client);
            let attempt = async {
                let client = match slot {
                    Some(client) => client,
                    None => slot.insert(Client::connect_or_start(paths, &OsManager).await?),
                };
                client.call(request.clone()).await
            };
            match tokio::time::timeout_at(deadline, attempt).await {
                Ok(Err(e)) if e.code == ErrorCode::ServiceUnavailable => last = Some(e),
                Ok(result) => return result,
                Err(_) => {}
            }
            self.client = None;
            if TokioInstant::now() + RETRY_PAUSE >= deadline {
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

    async fn outcome(&mut self, request: Request) -> Result<AcquireOutcome, HostlerError> {
        self.outcome_until(request, TokioInstant::now() + RETRY_BUDGET)
            .await
    }

    async fn reattach(&mut self) -> Result<AcquireOutcome, HostlerError> {
        #[cfg(debug_assertions)]
        if let Some(ms) = std::env::var("HOSTLER_TEST_DELAY_REATTACH_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        let req = self.req.clone();
        recover(self, &req).await
    }

    /// One attempt on the current connection; for the waiting display only.
    async fn snapshot(&mut self) -> Option<QueueSnapshot> {
        let value = self
            .client
            .as_mut()?
            .call(Request::QueueSnapshot)
            .await
            .ok()?;
        serde_json::from_value(value).ok()
    }

    /// Best effort: after 10 s the service reaps the entry once we are gone.
    async fn release(&mut self, exit_code: i32) {
        let outcome = if exit_code == 0 {
            HeavyOutcome::Ok
        } else {
            HeavyOutcome::Failed
        };
        let release = HeavyRelease {
            id: self.req.id.clone(),
            exit_code: Some(exit_code),
            outcome,
        };
        #[cfg(debug_assertions)]
        if let Some(ms) = std::env::var("HOSTLER_TEST_DELAY_RELEASE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        let _ = self.call(Request::HeavyRelease(release)).await;
    }
}

fn workspace_root() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let root = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    (out.status.success() && !root.is_empty()).then_some(root)
}

/// What the waiter shows; a change of it prints at once.
#[derive(Clone, PartialEq)]
struct View {
    capacity: u8,
    position: u32,
    of: u32,
    running: Vec<HeavyEntry>,
}

impl View {
    fn update(&mut self, snapshot: QueueSnapshot) {
        self.capacity = snapshot.capacity;
        self.running = snapshot.running;
        self.running.extend(snapshot.foreign_running);
    }

    fn status(&self, waited: Duration, max_wait: Option<Duration>) -> WaitStatus {
        let now_ms = SystemClock.now_ms();
        WaitStatus {
            capacity: self.capacity,
            position: self.position,
            of: self.of,
            running: self
                .running
                .iter()
                .map(|e| Holder {
                    command: e.command.clone(),
                    cwd: e.cwd.clone(),
                    pid: e.holder.pid,
                    held: Duration::from_millis(
                        (now_ms - e.granted_at_ms.unwrap_or(now_ms)).max(0) as u64,
                    ),
                })
                .collect(),
            waited,
            max_wait,
        }
    }
}

async fn next_event(client: &mut Option<Client>) -> Option<Event> {
    match client {
        Some(client) => client.next_event().await,
        None => None,
    }
}

/// Waits until our entry leaves the waiting list or the connection drops, then answers its state
/// (`Continue`). Every await, recovery included, races `stop` (`Break`).
async fn wait_for_slot(
    heavy: &mut Heavy,
    stop: &mut Stop,
    position: u32,
    of: u32,
) -> ControlFlow<Stopped, Result<AcquireOutcome, HostlerError>> {
    let mut view = View {
        capacity: 0,
        position,
        of,
        running: Vec::new(),
    };
    let snapshot = tokio::select! {
        snapshot = heavy.snapshot() => snapshot,
        stopped = stop.wait() => return ControlFlow::Break(stopped),
    };
    if let Some(snapshot) = snapshot {
        view.update(snapshot);
    }
    let mut printed: Option<(Instant, View)> = None;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        let now = Instant::now();
        let changed = printed.as_ref().is_none_or(|(_, shown)| *shown != view);
        if printed
            .as_ref()
            .is_none_or(|(at, _)| wait::should_print(*at, now, changed))
        {
            let waited = now.saturating_duration_since(stop.since);
            eprintln!("{}", wait::wait_line(&view.status(waited, stop.max_wait)));
            printed = Some((now, view.clone()));
        }
        let recover = tokio::select! {
            _ = ticker.tick() => false,
            stopped = stop.wait() => return ControlFlow::Break(stopped),
            event = next_event(&mut heavy.client) => match event {
                Some(Event::QueueChanged(snapshot)) => {
                    match queue::position(&snapshot.waiting, &heavy.req.id) {
                        Some((position, of)) => {
                            view.position = position;
                            view.of = of;
                            view.update(snapshot);
                            false
                        }
                        None => true,
                    }
                }
                Some(_) => false,
                None => true,
            },
        };
        if recover {
            return tokio::select! {
                outcome = heavy.reattach() => ControlFlow::Continue(outcome),
                stopped = stop.wait() => ControlFlow::Break(stopped),
            };
        }
    }
}

async fn wait_blocking(wait: impl FnOnce() -> anyhow::Result<ExitStatus> + Send + 'static) -> i32 {
    match tokio::task::spawn_blocking(wait).await {
        Ok(Ok(status)) => spawn::exit_code(status),
        Ok(Err(e)) => {
            eprintln!("[hostler] {e:#}");
            1
        }
        Err(e) => {
            eprintln!("[hostler] waiting for the command failed: {e}");
            1
        }
    }
}

async fn run_granted(
    heavy: &mut Heavy,
    argv: Vec<OsString>,
    signals: std::io::Result<spawn::Signals>,
    io: &ChildIo,
    files: Option<RunDir>,
) -> i32 {
    let program = io.mask(&spawn::program(&argv));
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("HOSTLER_TEST_PAUSE_BEFORE_SPAWN_UNTIL") {
        let path = std::path::PathBuf::from(path);
        let mut reached = path.clone().into_os_string();
        reached.push(".reached");
        let _ = std::fs::write(reached, "");
        let until = Instant::now() + Duration::from_secs(30);
        while !path.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let spawned = signals
        .context("cannot install signal forwarding")
        .and_then(|signals| spawn::spawn_in_group(argv, &heavy.req.id, signals, io));
    let mut spawned = match spawned {
        Ok(spawned) => spawned,
        Err(e) => {
            eprintln!("[hostler] cannot run {program}: {e:#}");
            heavy.release(EXIT_CANNOT_RUN).await;
            return EXIT_CANNOT_RUN;
        }
    };
    let pumps = secret_run::start_pumps(io, spawned.take_output());
    if let Some(files) = &files
        && let Err(e) = files.record_child(&spawned.identity())
    {
        eprintln!("[hostler] {e:#}");
        let _ = wait_blocking(move || spawned.abandon()).await;
        heavy.release(EXIT_REFUSED).await;
        return EXIT_REFUSED;
    }
    kill_self_if("HOSTLER_TEST_KILL_BEFORE_GATE");
    let started = HeavyStarted {
        id: heavy.req.id.clone(),
        child: spawned.identity(),
        group: spawned.group(),
    };
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_DELAY_STARTED_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    match heavy.call(Request::HeavyStarted(started)).await {
        // Withdrawn, timed out, force-released or reaped meanwhile: the command must not run.
        Err(e) if matches!(e.code, ErrorCode::Withdrawn | ErrorCode::UnknownEntry) => {
            let _ = wait_blocking(move || spawned.abandon()).await;
            eprintln!("[hostler] The request was withdrawn by an operator; not running.");
            return EXIT_QUEUE;
        }
        // Undelivered, the entry is reaped once we are gone and the group goes unseen:
        // accepted. Any other refusal is a defect of ours, not a reason to block the work.
        _ => {}
    }
    if let Err(e) = spawned.release_gate() {
        eprintln!("[hostler] {e:#}");
    }
    if files.is_some() {
        kill_self_if("HOSTLER_TEST_KILL_AFTER_SPAWN");
    }
    let code = wait_blocking(move || spawned.wait()).await;
    // The drain's deadline counts from the command's exit, never from the bookkeeping (T10-6).
    let exited = Instant::now();
    let drained = async move {
        secret_run::drain(pumps, exited).await;
        drop(files);
    };
    tokio::join!(heavy.release(code), drained);
    code
}

/// Fail-open, a secret run without `--heavy`, or (`Group::Enclosing`) a re-entrant secret run:
/// the command behind its gate, nothing registered; a process identity is needed only to record
/// a secret-file run's command, which happens before the gate opens.
async fn run_unqueued(
    argv: Vec<OsString>,
    signals: std::io::Result<spawn::Signals>,
    io: &ChildIo,
    files: Option<RunDir>,
    group: spawn::Group,
) -> i32 {
    let program = io.mask(&spawn::program(&argv));
    let launched = signals
        .context("cannot install signal forwarding")
        .and_then(|signals| spawn::launch(argv, None, signals, io, group));
    let mut launched = match launched {
        Ok(launched) => launched,
        Err(e) => {
            eprintln!("[hostler] cannot run {program}: {e:#}");
            return EXIT_CANNOT_RUN;
        }
    };
    let pumps = secret_run::start_pumps(io, launched.take_output());
    if let Some(files) = &files {
        kill_self_if("HOSTLER_TEST_KILL_BEFORE_RECORD");
        if let Err(e) = record_pid(files, launched.id()) {
            eprintln!("[hostler] {e:#}");
            // The gate closes unopened: the command never runs.
            let _ = wait_blocking(move || launched.wait()).await;
            return EXIT_REFUSED;
        }
        kill_self_if("HOSTLER_TEST_KILL_BEFORE_GATE");
    }
    if let Err(e) = launched.release_gate() {
        eprintln!("[hostler] {e:#}");
    }
    if files.is_some() {
        kill_self_if("HOSTLER_TEST_KILL_AFTER_SPAWN");
    }
    let code = wait_blocking(move || launched.wait()).await;
    secret_run::drain(pumps, Instant::now()).await;
    drop(files);
    code
}

/// Re-entrant without secrets: the enclosing heavy command already holds the slot; run in its
/// group as is.
async fn run_direct(argv: Vec<OsString>, mut interrupts: spawn::IgnoreInterrupts) -> i32 {
    let program = spawn::program(&argv);
    let Some((first, rest)) = argv.split_first() else {
        return EXIT_CANNOT_RUN;
    };
    let mut child = match Command::new(first).args(rest).spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[hostler] cannot run {program}: {e}");
            return EXIT_CANNOT_RUN;
        }
    };
    interrupts.pass_pending_to(&mut child);
    match tokio::task::spawn_blocking(move || child.wait()).await {
        Ok(Ok(status)) => spawn::exit_code(status),
        _ => {
            eprintln!("[hostler] waiting for {program} failed");
            1
        }
    }
}

/// Records the launcher `pid` (the command after `exec` on Unix, its supervisor on Windows) as a
/// secret run's command.
fn record_pid(files: &RunDir, pid: u32) -> anyhow::Result<()> {
    let table = spawn::process_table()?;
    let identity = identity_of(&table, pid)
        .with_context(|| format!("cannot read the start time of the launched process {pid}"))?;
    files.record_child(&identity)
}

/// Debug builds: this process dies at once (SIGKILL / TerminateProcess) when `hook` is `1`.
#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn kill_self_if(hook: &str) {
    #[cfg(debug_assertions)]
    if std::env::var(hook).as_deref() == Ok("1") {
        // SAFETY: plain process calls with no memory preconditions; this process ends here.
        #[cfg(unix)]
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL)
        };
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
            TerminateProcess(GetCurrentProcess(), 1)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_values_are_kept_whole() {
        assert_eq!(keep_head("cargo build", 1024), "cargo build");
        assert_eq!(keep_tail("/work/app", 2048), "/work/app");
        let exact = "a".repeat(1024);
        assert_eq!(keep_head(&exact, 1024), exact);
    }

    #[test]
    fn commands_keep_their_head_and_paths_their_tail() {
        let command = format!("./gradlew {}", "x".repeat(2000));
        let cut = keep_head(&command, 1024);
        assert_eq!(cut.len(), 1024);
        assert!(
            cut.starts_with("./gradlew xx") && cut.ends_with("x…"),
            "{cut}"
        );

        let path = format!("/{}/app", "d".repeat(3000));
        let cut = keep_tail(&path, 2048);
        assert_eq!(cut.len(), 2048);
        assert!(cut.starts_with("…ddd") && cut.ends_with("/app"), "{cut}");
    }

    #[test]
    fn truncation_cuts_at_char_boundaries() {
        // 'é' is two bytes: no cut may split it.
        let s = "é".repeat(1000);
        let head = keep_head(&s, 1024);
        assert!(head.len() <= 1024 && head.ends_with('…'));
        assert_eq!(head.trim_end_matches('…').chars().count(), 510);
        let tail = keep_tail(&s, 1023);
        assert!(tail.len() <= 1023 && tail.starts_with('…'));
        assert!(tail.trim_start_matches('…').chars().all(|c| c == 'é'));
    }

    #[test]
    fn truncation_measures_escaped_json_length() {
        // 600 quotes are 600 bytes raw but 1200 as JSON: the raw length fits, the wire one not.
        let quotes = "\"".repeat(600);
        let cut = keep_head(&quotes, 1024);
        assert!(
            json_len(&cut) <= 1024 && cut.ends_with('…'),
            "{}",
            json_len(&cut)
        );
        assert_eq!(cut.trim_end_matches('…').len(), 510);
        // Control characters escape to six bytes (\u0001); the tail must still fit.
        let path = format!("/{}/app", "\u{1}".repeat(400));
        let cut = keep_tail(&path, 2048);
        assert!(json_len(&cut) <= 2048 && cut.starts_with('…') && cut.ends_with("/app"));
        assert_eq!(cut.matches('\u{1}').count(), (2048 - 3 - 4) / 6);
        // A value whose escaped length fits is kept whole.
        let fits = "\\".repeat(512);
        assert_eq!(keep_head(&fits, 1024), fits);
    }

    /// Answers in order and records what was asked and until when.
    struct Recorded {
        answers: Vec<Result<AcquireOutcome, HostlerError>>,
        seen: Vec<(Request, TokioInstant)>,
    }

    impl Outcomes for Recorded {
        async fn outcome_until(
            &mut self,
            request: Request,
            deadline: TokioInstant,
        ) -> Result<AcquireOutcome, HostlerError> {
            self.seen.push((request, deadline));
            self.answers.remove(0)
        }
    }

    #[tokio::test]
    async fn recovery_shares_one_deadline_and_keeps_the_id() {
        let req = HeavyAcquire {
            id: uuid::Uuid::new_v4().to_string(),
            kind: "gradle".into(),
            command: "./gradlew assemble".into(),
            cwd: "/work/app".into(),
            workspace_root: None,
            holder: crate::procs::ProcessIdentity {
                pid: 42,
                boot_id: "b1".into(),
                start_time: 7,
            },
            reentrant_token: None,
            no_wait: false,
        };
        let unknown = HostlerError {
            code: ErrorCode::UnknownEntry,
            message: "no heavy entry".into(),
            details: json!({}),
            recovery: None,
        };
        let mut to = Recorded {
            answers: vec![Err(unknown), Ok(AcquireOutcome::Reentrant)],
            seen: Vec::new(),
        };
        let before = TokioInstant::now();
        assert_eq!(recover(&mut to, &req).await, Ok(AcquireOutcome::Reentrant));
        let [(attach, first), (acquire, second)] = to.seen.as_slice() else {
            panic!("{:?}", to.seen);
        };
        assert_eq!(attach, &Request::HeavyAttach { id: req.id.clone() });
        assert_eq!(acquire, &Request::HeavyAcquire(req.clone()));
        assert_eq!(first, second, "the re-acquire must not get a fresh budget");
        assert!(*first <= before + RETRY_BUDGET + Duration::from_secs(1));
    }

    #[test]
    fn gate_launcher_parses_on_every_platform() {
        use clap::Parser;
        // A Windows handle value can exceed any Unix descriptor.
        let cli = crate::cli::Cli::try_parse_from([
            "hostler",
            "__gate",
            "--fd",
            "1234567890123",
            "--job",
            r"Local\hostler-heavy-x",
            "--",
            "prog",
            "-x",
        ])
        .unwrap();
        match cli.command {
            crate::cli::Command::Gate { fd, job, command } => {
                assert_eq!(fd, 1_234_567_890_123);
                assert_eq!(job.as_deref(), Some(r"Local\hostler-heavy-x"));
                assert_eq!(command, [OsString::from("prog"), OsString::from("-x")]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn kind_and_minutes_are_validated() {
        assert!(parse_kind("gradle").is_ok());
        assert!(parse_kind("pod-install-2").is_ok());
        assert!(parse_kind("Gradle").is_err());
        assert!(parse_kind("").is_err());
        assert!(parse_kind(&"a".repeat(33)).is_err());
        assert_eq!(parse_minutes("0.05"), Ok(0.05));
        assert!(parse_minutes("-1").is_err());
        assert!(parse_minutes("NaN").is_err());
        assert_eq!(max_wait(Some(0.0)), Ok(None));
        assert_eq!(max_wait(Some(2.0)), Ok(Some(Duration::from_secs(120))));
    }

    #[test]
    fn one_argument_is_a_shell_line() {
        let argv = command_argv(vec!["echo a && echo b".into()]);
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[2], "echo a && echo b");
        let argv = command_argv(vec!["echo".into(), "a".into()]);
        assert_eq!(argv, vec![OsString::from("echo"), OsString::from("a")]);
    }
}
