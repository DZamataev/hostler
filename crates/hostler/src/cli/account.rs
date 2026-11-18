//! `hostler account list | set | remove` (plan P3a, Task 9): a project's test accounts in the
//! secrets database. Values come from a hidden prompt or stdin, never an argument (P2's
//! `secret set` flow).

use std::ffi::OsString;
use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::Instant as TokioInstant;

use crate::cli::output;
use crate::cli::secrets::secret::read_stdin_value;
use crate::cli::secrets::{call, current_project, internal, report_error, values_are_arguments};
use crate::cli::tty::{self, Tty};
use crate::cli::{spawn, wait};
use crate::client::Client;
use crate::contract::HostlerError;
use crate::contract::accounts::{
    AccountInfo, AccountRemove, AccountRequestInfo, AccountRequestNew, AccountSet,
    INVALID_ACCOUNT_NAME, INVALID_FIELD_NAME, valid_account_name, valid_field_name,
};
use crate::contract::claims::ClaimKind;
use crate::contract::secrets::Prove;
use crate::contract::secrets::errors;
use crate::contract::wire::{Event, Request};
use crate::contract::{Envelope, ErrorCode};
use crate::paths::Paths;
use crate::procs::current_identity;
use crate::secrets::sanitize_for_terminal;
use crate::secrets::value::SecretValue;
use crate::service::harden::harden_value_holder;
use crate::service::manager::OsManager;

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub command: AccountCommand,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// This project's test accounts: names, field names and who holds them, never values.
    List {
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Set one field of an account, creating the account; the value comes from a prompt or
    /// stdin, never an argument.
    Set {
        /// The account's name.
        name: String,
        /// The field, e.g. `session`; it becomes HOSTLER_ACCOUNT_<FIELD> in a run.
        #[arg(long, value_name = "FIELD")]
        field: String,
        #[arg(hide = true, allow_hyphen_values = true, num_args = 0..)]
        extra: Vec<OsString>,
    },
    /// Remove an account, or one of its fields.
    ///
    /// hostler removes only its own data: the account's fields (or the one given with --field)
    /// and, once no field is left, the hostler-account tag. The KeePass entry itself is deleted
    /// only when nothing else is left in it; a password, user name, URL, notes, other custom
    /// fields or attachments a person keeps on the entry stay, and the entry stays with them.
    Remove {
        /// The account's name.
        name: String,
        /// Remove only this field.
        #[arg(long, value_name = "FIELD")]
        field: Option<String>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Ask a person for an account's fields and wait (spec §8.4).
    Request {
        /// The account's name.
        name: String,
        /// A field the run needs; repeat for each.
        #[arg(long = "field", value_name = "FIELD", required = true)]
        fields: Vec<String>,
        /// Why the account is needed (shown to the person).
        #[arg(long)]
        reason: String,
        /// Give up after this many minutes (default 10; 0 waits forever).
        #[arg(long, value_name = "MIN", value_parser = crate::cli::secrets::request::parse_minutes)]
        max_wait: Option<f64>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
        #[arg(hide = true, allow_hyphen_values = true, num_args = 0..)]
        extra: Vec<OsString>,
    },
    /// The pending account requests.
    Requests {
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Decline an account request (a person command).
    Decline {
        /// The request's id (or its first 8 characters).
        id: String,
    },
}

pub fn main(args: AccountArgs) -> ExitCode {
    match args.command {
        AccountCommand::List { json } => output::report(
            current_project()
                .and_then(|project| call::<Vec<AccountInfo>>(Request::AccountList { project })),
            json,
            |accounts| print_list(accounts),
        ),
        AccountCommand::Set { name, field, extra } => {
            if !extra.is_empty() {
                return values_are_arguments();
            }
            if let Err(code) = checked(&name, Some(&field)) {
                return code;
            }
            match set(name, field) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => report_error(&e),
            }
        }
        AccountCommand::Remove { name, field, json } => {
            if let Err(code) = checked(&name, field.as_deref()) {
                return code;
            }
            output::report(remove(name, field), json, print_removal)
        }
        AccountCommand::Request {
            name,
            fields,
            reason,
            max_wait,
            json,
            extra,
        } => {
            if !extra.is_empty() {
                return values_are_arguments();
            }
            main_request(name, fields, reason, max_wait, json)
        }
        AccountCommand::Requests { json } => main_requests(json),
        AccountCommand::Decline { id } => match decline(id) {
            Ok(info) => {
                println!(
                    "declined {} ({} for project {})",
                    short_id(&info.id),
                    info.name,
                    sanitize_for_terminal(&info.project_name)
                );
                ExitCode::SUCCESS
            }
            Err(e) => report_error(&e),
        },
    }
}

/// An invalid name is never echoed: it may be a pasted value.
fn checked(name: &str, field: Option<&str>) -> Result<(), ExitCode> {
    let refusal = if !valid_account_name(name) {
        INVALID_ACCOUNT_NAME
    } else if field.is_some_and(|f| !valid_field_name(f)) {
        INVALID_FIELD_NAME
    } else {
        return Ok(());
    };
    eprintln!("[hostler] {refusal}");
    Err(ExitCode::from(2))
}

fn set(name: String, field: String) -> Result<(), HostlerError> {
    let project = current_project()?;
    harden_value_holder();
    let value = if tty::interactive() {
        let mut tty = Tty::open("account set")?;
        let prompt = format!(
            "Value for {name}.{field} ({}):",
            sanitize_for_terminal(&project.name)
        );
        tty.hidden(&prompt)?
    } else if tty::stdin_is_terminal() {
        return Err(errors::not_interactive("account set"));
    } else {
        read_stdin_value()?
    };
    call::<Value>(Request::AccountSet(AccountSet {
        project,
        name: name.clone(),
        field: field.clone(),
        value,
    }))?;
    println!("stored {name}.{field}");
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct RemoveOutcome {
    removed: bool,
}

fn remove(name: String, field: Option<String>) -> Result<RemoveOutcome, HostlerError> {
    let project = current_project()?;
    let value = call::<Value>(Request::AccountRemove(AccountRemove {
        project,
        name,
        field,
    }))?;
    serde_json::from_value(value).map_err(|_| internal("unreadable answer from the service"))
}

fn print_list(accounts: &[AccountInfo]) {
    if accounts.is_empty() {
        println!("no accounts; a person adds one with `hostler account set NAME --field FIELD`");
        return;
    }
    let width = accounts
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(0)
        .max("NAME".len());
    let fields: Vec<String> = accounts.iter().map(|a| a.fields.join(", ")).collect();
    let fields_width = fields
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max("FIELDS".len());
    println!("{:<width$}  {:<fields_width$}  HOLDER", "NAME", "FIELDS");
    for (account, fields) in accounts.iter().zip(&fields) {
        let holder = account.holder.as_ref().map_or_else(
            || "-".to_string(),
            |h| {
                let kind = match h.kind {
                    ClaimKind::Lease => "run",
                    ClaimKind::Hold => "hold",
                    ClaimKind::Reservation => "reservation",
                };
                format!("{} ({kind})", sanitize_for_terminal(&h.workspace_path))
            },
        );
        println!(
            "{:<width$}  {fields:<fields_width$}  {holder}",
            account.name
        );
    }
}

fn print_removal(outcome: &RemoveOutcome) {
    println!(
        "{}",
        if outcome.removed {
            "removed"
        } else {
            "not set"
        }
    );
}

// ---------------------------------------------------------------------------------------------
// `hostler account request | requests | decline` (P5 Task 5, spec §8.4): the mirror of
// `hostler secret request`. A request waits visibly; a person answers in the app or with
// `hostler account set`, and declines with `hostler account decline` (behind the person gate).

const DEFAULT_MAX_WAIT_MIN: f64 = 10.0;
const POLL_EVERY: Duration = Duration::from_secs(5);
const RECONNECT_BUDGET: Duration = Duration::from_secs(10);
const RETRY_PAUSE: Duration = Duration::from_millis(200);
const WITHDRAW_BUDGET: Duration = Duration::from_secs(2);
const EXIT_TIMED_OUT: u8 = 75;
const EXIT_INTERRUPTED: u8 = 130;
/// The longest backoff of the service's password limit the `decline` prompt sits out itself.
const LIMIT_WAIT_MAX_MS: u64 = 30_000;

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// `None` = no limit; too large to represent counts as no limit.
fn account_max_wait(minutes: Option<f64>) -> Option<Duration> {
    let minutes = minutes.unwrap_or(DEFAULT_MAX_WAIT_MIN);
    (minutes > 0.0)
        .then(|| Duration::try_from_secs_f64(minutes * 60.0).ok())
        .flatten()
}

/// The waiting status line; `project` is agent-supplied, so it is sanitized here.
pub fn request_line(
    name: &str,
    fields: &[String],
    project: &str,
    short: &str,
    waited: Duration,
    max_wait: Option<Duration>,
    locked: bool,
) -> String {
    let limit = max_wait.map_or_else(|| "no limit".to_owned(), wait::duration_text);
    let runs = if locked {
        format!(
            "The database is locked: a person runs hostler unlock, then hostler account set \
             {name} --field <field> (one per field)"
        )
    } else {
        format!(
            "A person runs: hostler account set {name} --field <field> (one per field), or \
             answers in the hostler app"
        )
    };
    format!(
        "[hostler] Waiting for a person to provide {} of {name} for project {} (request \
         {short}). Waited {} of {limit} — this is waiting, not hanging; Ctrl+C withdraws the \
         request. {runs}   (or declines: hostler account decline {short})",
        fields.join(", "),
        sanitize_for_terminal(project),
        wait::duration_text(waited),
    )
}

pub fn main_request(
    name: String,
    fields: Vec<String>,
    reason: String,
    max_wait_min: Option<f64>,
    json: bool,
) -> ExitCode {
    if let Err(code) = checked(&name, None) {
        return code;
    }
    if fields.is_empty()
        || fields.len() > crate::secrets::engine::MAX_ACCOUNT_FIELDS
        || fields.iter().any(|f| !valid_field_name(f))
    {
        eprintln!("[hostler] {INVALID_FIELD_NAME}");
        return ExitCode::from(2);
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
    let code = runtime.block_on(request(
        name,
        fields,
        reason,
        account_max_wait(max_wait_min),
        json,
    ));
    runtime.shutdown_background();
    ExitCode::from(code)
}

/// Prints the outcome (`--json`: one envelope on stdout) and returns `code`.
fn finish(json: bool, result: Result<AccountRequestInfo, HostlerError>, code: u8) -> u8 {
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
fn decided(json: bool, info: AccountRequestInfo) -> u8 {
    let code = match info.state {
        crate::contract::secrets::RequestState::Stored => {
            if !json {
                println!("stored");
            }
            0
        }
        crate::contract::secrets::RequestState::Declined => {
            if !json {
                println!("declined");
            }
            1
        }
        crate::contract::secrets::RequestState::Withdrawn
        | crate::contract::secrets::RequestState::TimedOut => {
            // A status line: on stderr with `--json` too.
            eprintln!(
                "[hostler] The account request for {} was withdrawn by an operator, or because \
                 its requester was gone; not waiting any more.",
                info.name
            );
            EXIT_TIMED_OUT
        }
        crate::contract::secrets::RequestState::Pending => {
            unreachable!("a pending request is not decided")
        }
    };
    finish(json, Ok(info), code)
}

async fn request(
    name: String,
    fields: Vec<String>,
    reason: String,
    max_wait: Option<Duration>,
    json: bool,
) -> u8 {
    // Registered before anything is sent: a Ctrl+C from here on withdraws the request.
    let mut stop = Stop::new(max_wait);
    let _ = stop.now().await;
    let waiter = (|| {
        let project = current_project()?;
        // P4 R2-2: a denied state or socket directory is `sandbox_denied`.
        let paths = crate::client::resolve_paths()?;
        // Before the request names this process (W-3: a non-elevated service on Windows must be
        // able to judge an elevated requester's liveness).
        spawn::let_service_observe_self();
        let table = spawn::process_table().map_err(|e| crate::client::process_error(&e))?;
        let requester = current_identity(&table).map_err(|e| crate::client::process_error(&e))?;
        Ok::<_, HostlerError>(Waiter {
            paths,
            client: None,
            req: AccountRequestNew {
                // Made once: every re-send is the same request.
                id: uuid::Uuid::new_v4().to_string(),
                project,
                name,
                fields,
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
            .filter(|info| info.state != crate::contract::secrets::RequestState::Pending);
        return give_up(&mut waiter, stopped, max_wait, json, known).await;
    }
    let info = match first {
        Ok(info) => info,
        Err(e) => return finish(json, Err(e), 1),
    };
    if info.state != crate::contract::secrets::RequestState::Pending {
        return decided(json, info);
    }
    wait_for_answer(&mut waiter, &mut stop, info, max_wait, json).await
}

async fn wait_for_answer(
    waiter: &mut Waiter,
    stop: &mut Stop,
    info: AccountRequestInfo,
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
                    &waiter.req.fields,
                    &info.project_name,
                    &short,
                    waited,
                    max_wait,
                    locked
                )
            );
            printed = Some((now, locked));
        }
        // The poll comes before events: a stream of other events never starves it.
        let recheck = tokio::select! {
            biased;
            stopped = stop.wait() => return give_up(waiter, stopped, max_wait, json, None).await,
            _ = poll.tick() => true,
            event = next_event(&mut waiter.client) => match event {
                Some(Event::AccountRequestChanged(_) | Event::SecretsLockChanged(_)) => events_count,
                // Something was not delivered: ask again (spec §6.2).
                Some(Event::Resync { .. }) => events_count,
                Some(
                    Event::ProjectsChanged(_)
                    | Event::SecretRequestChanged(_)
                    | Event::QueueChanged(_)
                    | Event::ClaimsChanged(_)
                    | Event::ClaimsSeized(_)
                    | Event::DevicesChanged(_)
                    | Event::DevicesInstalled(_),
                ) => false,
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
                .filter(|info| info.state != crate::contract::secrets::RequestState::Pending);
            return give_up(waiter, stopped, max_wait, json, known).await;
        }
        match answer {
            Ok(answer) if answer.state == crate::contract::secrets::RequestState::Pending => {
                match locked_or_stop(waiter, stop).await {
                    Ok(Some(now_locked)) => locked = now_locked,
                    Ok(None) => {}
                    Err(stopped) => return give_up(waiter, stopped, max_wait, json, None).await,
                }
            }
            Ok(answer) => return decided(json, answer),
            Err(mut e) if e.code == ErrorCode::ServiceUnavailable => {
                e.message = "The hostler service is unavailable; the request may still be \
                             pending (hostler account requests)."
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
    known: Option<AccountRequestInfo>,
) -> u8 {
    let (timed_out, code) = match stopped {
        Stopped::TimedOut => (true, EXIT_TIMED_OUT),
        Stopped::Interrupted => (false, EXIT_INTERRUPTED),
    };
    let answer: Result<AccountRequestInfo, HostlerError> = match known {
        Some(info) => Ok(info),
        None => {
            let withdraw = Request::AccountRequestWithdraw {
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
        Ok(info)
            if matches!(
                info.state,
                crate::contract::secrets::RequestState::Withdrawn
                    | crate::contract::secrets::RequestState::TimedOut
            ) =>
        {
            "the request was withdrawn".to_owned()
        }
        Ok(info) => format!("the request was already {}", state_text(info.state)),
        Err(_) => "the request could not be withdrawn; it ends once the service sees this \
                   process gone (hostler account requests)"
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

fn state_text(state: crate::contract::secrets::RequestState) -> &'static str {
    use crate::contract::secrets::RequestState;
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
    req: AccountRequestNew,
}

impl Waiter {
    /// The first `account.request`: one connection (the service started if needed), no retry.
    async fn first(&mut self) -> Result<AccountRequestInfo, HostlerError> {
        let client =
            Client::connect_or_start(&self.paths, &OsManager, crate::cli::REPLACE_ON_START).await?;
        let client = self.client.insert(client);
        decode(
            client
                .call(Request::AccountRequest(self.req.clone()))
                .await?,
        )
    }

    /// The request's recorded state, re-sent under the same id; 10 s to reconnect.
    async fn ask(&mut self) -> Result<AccountRequestInfo, HostlerError> {
        self.call_until(
            Request::AccountRequest(self.req.clone()),
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

    /// Sends `request`; on unavailability reconnects and sends it again. A reconnect never
    /// starts a service (a person may have stopped it). Every request here is idempotent.
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
                // Never retried (P4 R1-3).
                Some(Err(e)) if crate::client::is_sandbox_denied(&e) => return Err(e),
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
                    details: serde_json::json!({ "reason": "timeout" }),
                    recovery: Some("hostler service start".into()),
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
    output::report(
        call::<Vec<AccountRequestInfo>>(Request::AccountRequests),
        json,
        print_requests,
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn print_requests(requests: &Vec<AccountRequestInfo>) {
    if requests.is_empty() {
        println!("no pending account requests");
        return;
    }
    let now = now_ms();
    for r in requests {
        let age = Duration::from_millis((now - r.created_at_ms).max(0) as u64);
        println!(
            "{}  {}  {}  {}  {} ago  {}",
            short_id(&r.id),
            sanitize_for_terminal(&r.project_name),
            r.name,
            r.fields.join(","),
            wait::duration_text(age),
            sanitize_for_terminal(&r.reason)
        );
    }
}

/// `account.decline` (spec §8.4/§8.5): a person command. The terminal check first, then the
/// master password at a hidden prompt, a proof on one connection, and the decline on it.
fn decline(id: String) -> Result<AccountRequestInfo, HostlerError> {
    if id.len() < 8 || id.len() > 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(crate::contract::secrets::errors::bad_request(
            "give a request id (at least 8 characters of it, see hostler account requests)".into(),
        ));
    }
    // Person-only: refuse before touching the service.
    let mut tty = Tty::open("account decline")?;
    harden_value_holder();
    let paths = crate::client::resolve_paths()?;
    let id = id.to_ascii_lowercase();
    let mut last = None;
    for _ in 0..3 {
        let password = tty.hidden("Master password:")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| internal(format!("cannot start the async runtime: {e}")))?;
        let result = runtime.block_on(async {
            let mut client =
                Client::connect_or_start(&paths, &OsManager, crate::cli::REPLACE_ON_START).await?;
            prove_waiting(&mut client, &password).await?;
            let value = client
                .call(Request::AccountDecline { id: id.clone() })
                .await?;
            decode::<AccountRequestInfo>(value)
        });
        runtime.shutdown_background();
        match result {
            Ok(info) => return Ok(info),
            Err(e) if e.code == ErrorCode::UnlockFailed => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(errors::unlock_failed))
}

/// One proof; a short wait of the service-wide limit on password checks is sat out and the same
/// password sent again (after a mistyped one, the next check waits 1, 2, 4 … s).
async fn prove_waiting(client: &mut Client, password: &SecretValue) -> Result<(), HostlerError> {
    loop {
        let prove = Request::SecretsProve(Prove {
            password: password.clone(),
            key_file: None,
        });
        match client.call(prove).await {
            Ok(_) => return Ok(()),
            Err(e) if e.code == ErrorCode::ProofLimited => {
                let wait = e.details["retry_after_ms"].as_u64().unwrap_or(1_000);
                if wait > LIMIT_WAIT_MAX_MS {
                    return Err(e);
                }
                eprintln!(
                    "[hostler] the service checks the next password in {} s; waiting",
                    wait.div_ceil(1000)
                );
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn a_value_after_the_flags_lands_in_the_hidden_extra() {
        let cli = crate::cli::Cli::try_parse_from([
            "hostler",
            "account",
            "request",
            "qa-user",
            "--field",
            "session",
            "--reason",
            "x",
            "--hunter2-canary",
        ])
        .unwrap();
        let crate::cli::Command::Account(AccountArgs { command }) = cli.command else {
            panic!("not an account command");
        };
        match command {
            AccountCommand::Request {
                name,
                fields,
                extra,
                ..
            } => {
                assert_eq!(name, "qa-user");
                assert_eq!(fields, ["session"]);
                assert_eq!(extra, ["--hunter2-canary"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn request_line_shapes() {
        let fields = vec!["session".to_string(), "env".to_string()];
        let line = request_line(
            "qa-user",
            &fields,
            "app",
            "1a2b3c4d",
            Duration::from_secs(42),
            Some(Duration::from_secs(600)),
            false,
        );
        assert_eq!(
            line,
            "[hostler] Waiting for a person to provide session, env of qa-user for project app \
             (request 1a2b3c4d). Waited 42 s of 10 min — this is waiting, not hanging; Ctrl+C \
             withdraws the request. A person runs: hostler account set qa-user --field <field> \
             (one per field), or answers in the hostler app   (or declines: hostler account \
             decline 1a2b3c4d)"
        );
        let locked = request_line(
            "qa-user",
            &fields,
            "app",
            "1a2b3c4d",
            Duration::ZERO,
            None,
            true,
        );
        assert!(locked.contains("Waited 0 s of no limit"), "{locked}");
        assert!(
            locked.contains("The database is locked: a person runs hostler unlock"),
            "{locked}"
        );
        assert!(!locked.contains("answers in the hostler app"), "{locked}");
        // The project name is agent-supplied.
        let hostile = request_line(
            "qa-user",
            &fields,
            "x\u{1b}[2Jy",
            "1a2b3c4d",
            Duration::ZERO,
            None,
            false,
        );
        assert!(
            hostile.contains("project x\u{FFFD}[2Jy (request"),
            "{hostile}"
        );
    }
}
