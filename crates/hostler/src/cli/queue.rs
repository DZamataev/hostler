//! `hostler queue …` (plan Task 9): look at the heavy queue and its history, withdraw a waiting
//! request, force-release a slot, set the capacity; and (P5 Task 6) a person's stop of a running
//! task, which the service itself performs (spec §11.1).

use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::json;

use super::output::{call, print_error, report};
use super::wait::duration_text;
use crate::contract::heavy::{
    DEFAULT_HISTORY_LIMIT, EntrySource, HeavyEntry, HeavyHistoryItem, HistoryQuery, QueueSnapshot,
    QueueStopOutcome,
};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::{Clock, SystemClock};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct QueueArgs {
    #[command(subcommand)]
    command: Option<QueueCommand>,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum QueueCommand {
    /// Finished heavy tasks, newest first.
    History {
        /// Only tasks finished within this long, e.g. 30m, 3h, 1d.
        #[arg(long, value_name = "DUR", value_parser = parse_since)]
        since: Option<Duration>,
        #[arg(long, value_name = "N", default_value_t = DEFAULT_HISTORY_LIMIT)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Withdraw a request that has not started its command.
    Withdraw {
        #[arg(long)]
        id: String,
    },
    /// Free a slot while its command may still run (agents: only with the operator's explicit
    /// permission). The process itself is not stopped.
    Release {
        #[arg(long)]
        id: String,
        #[arg(
            long,
            help = "Free the slot while its command may still run; the process is not stopped. \
                    An agent uses --force only with the operator's explicit permission."
        )]
        force: bool,
    },
    /// The number of heavy tasks that may run at once.
    #[command(subcommand)]
    Capacity(CapacityCommand),
    /// Stop a running heavy task and free its slot: the service stops its processes, checks
    /// that they are gone, and records it as stopped (a person command: the master password is
    /// asked).
    #[command(after_help = crate::cli::secrets::PERSON_ONLY)]
    Stop {
        /// The task's id.
        id: String,
        // An explicit help: clap would drop the D9 sentence's final period from a doc comment.
        #[arg(
            long,
            help = "Required: the task's processes are stopped. An agent uses --force only with \
                    the operator's explicit permission."
        )]
        force: bool,
        /// The task's id again, to confirm.
        #[arg(long, value_name = "ID")]
        confirmation: Option<String>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CapacityCommand {
    /// Set the capacity (1–16).
    Set { capacity: u8 },
}

/// `<n>s`, `<n>m`, `<n>h` or `<n>d`.
fn parse_since(s: &str) -> Result<Duration, String> {
    let error = || format!("{s:?} is not a duration like 30m, 3h or 1d");
    let unit = s.chars().last().ok_or_else(error)?;
    let n: u64 = s[..s.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| error())?;
    let secs = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(error()),
    };
    n.checked_mul(secs)
        .map(Duration::from_secs)
        .ok_or_else(error)
}

pub fn main(args: QueueArgs) -> ExitCode {
    // A person command: it prompts, so it runs outside the async runtime below.
    if let Some(QueueCommand::Stop {
        id,
        force,
        confirmation,
        json,
    }) = args.command
    {
        return stop::main(id, force, confirmation, json);
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("[hostler] cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(args))
}

async fn run(args: QueueArgs) -> ExitCode {
    match args.command {
        None => {
            let result = call::<QueueSnapshot>(Request::QueueSnapshot).await;
            report(result, args.json, print_snapshot)
        }
        Some(QueueCommand::History { since, limit, json }) => {
            let since_ms = since.map(|d| SystemClock.now_ms() - d.as_millis() as i64);
            let query = HistoryQuery { since_ms, limit };
            let result = call::<Vec<HeavyHistoryItem>>(Request::QueueHistory(query)).await;
            report(result, json, print_history)
        }
        Some(QueueCommand::Withdraw { id }) => {
            let request = Request::HeavyWithdraw {
                id: id.clone(),
                timed_out: false,
            };
            done(call::<()>(request).await, &format!("Withdrew {id}."))
        }
        Some(QueueCommand::Release { id, force: false }) => {
            print_error(&force_required(&id));
            ExitCode::FAILURE
        }
        Some(QueueCommand::Release { id, force: true }) => done(
            call::<()>(Request::HeavyForceRelease { id: id.clone() }).await,
            &format!("Released {id}; its processes were not stopped."),
        ),
        Some(QueueCommand::Stop { .. }) => unreachable!("answered before the runtime starts"),
        Some(QueueCommand::Capacity(CapacityCommand::Set { capacity })) => done(
            call::<()>(Request::QueueSetCapacity { capacity }).await,
            &format!("Heavy-queue capacity set to {capacity}."),
        ),
    }
}

fn force_required(id: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::ForceRequired,
        message: format!(
            "releasing {id} frees its slot while its command may still run; agents use --force \
             only with the operator's explicit permission"
        ),
        details: json!({ "id": id }),
        recovery: Some(format!("hostler queue release --id {id} --force")),
    }
}

/// `hostler queue stop` (P5 Task 6, spec §11.1): the CLI's terminal check, the master password
/// proven on one connection (the person gate, §8.5), then the service's `heavy.stop` on it.
mod stop {
    use super::*;
    use crate::cli::tty::Tty;
    use crate::client::Client;
    use crate::contract::secrets::{ProofGrant, Prove};
    use crate::secrets::value::SecretValue;

    /// Typed passwords before giving up, as `hostler unlock`.
    const TRIES: usize = 3;
    /// The longest wait of the service's limit on password checks that this sits out.
    const LIMIT_WAIT_MAX_MS: u64 = 30_000;

    pub(super) fn main(
        id: String,
        force: bool,
        confirmation: Option<String>,
        json: bool,
    ) -> ExitCode {
        let result = run(&id, force, confirmation.as_deref());
        let code = match &result {
            Ok(_) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
        report(result, json, print_stopped);
        code
    }

    fn run(
        id: &str,
        force: bool,
        confirmation: Option<&str>,
    ) -> Result<QueueStopOutcome, HostlerError> {
        if !force {
            return Err(force_required(id));
        }
        if confirmation != Some(id) {
            return Err(confirm_with_id(id));
        }
        crate::service::harden::harden_value_holder();
        let mut tty = Tty::open("queue stop")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                crate::cli::secrets::internal(format!("cannot start the async runtime: {e}"))
            })?;
        let paths = crate::client::resolve_paths()?;
        // One connection: the proof grants only the connection it was made on.
        let mut client = runtime.block_on(Client::connect(&paths))?;
        let mut last = None;
        let mut proven = false;
        for _ in 0..TRIES {
            let password = tty.hidden(&format!("Master password, to stop heavy task {id}:"))?;
            match runtime.block_on(prove(&mut client, password)) {
                Ok(()) => {
                    proven = true;
                    break;
                }
                Err(e) if e.code == ErrorCode::UnlockFailed => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        if !proven {
            return Err(last.unwrap_or_else(crate::contract::secrets::errors::unlock_failed));
        }
        let answer = runtime.block_on(client.call(Request::HeavyStop {
            id: id.to_owned(),
            confirmation: id.to_owned(),
        }))?;
        serde_json::from_value(answer).map_err(|e| {
            crate::cli::secrets::internal(format!("unreadable answer from the service: {e}"))
        })
    }

    /// `secrets.prove` of one typed password. A short `proof_limited` (the next check after a
    /// mistyped password waits 1, 2, 4 … s) is waited out and the same password sent again; a
    /// longer one is the error.
    async fn prove(client: &mut Client, password: SecretValue) -> Result<(), HostlerError> {
        loop {
            let request = Request::SecretsProve(Prove {
                password: password.clone(),
                key_file: None,
            });
            match client.call(request).await {
                Ok(answer) => {
                    return serde_json::from_value::<ProofGrant>(answer)
                        .map(drop)
                        .map_err(|e| {
                            crate::cli::secrets::internal(format!(
                                "unreadable answer from the service: {e}"
                            ))
                        });
                }
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

    pub(super) fn force_required(id: &str) -> HostlerError {
        HostlerError {
            code: ErrorCode::ForceRequired,
            message: format!(
                "stopping {id} ends its processes; {}",
                crate::contract::claims::FORCE_SENTENCE
            ),
            details: json!({ "id": id }),
            recovery: Some(format!(
                "hostler queue stop {id} --force --confirmation {id}"
            )),
        }
    }

    /// The service's own text for a confirmation that is not the id (it checks too).
    pub(super) fn confirm_with_id(id: &str) -> HostlerError {
        HostlerError {
            code: ErrorCode::BadRequest,
            message: "type the task's id to confirm".into(),
            details: json!({ "id": id }),
            recovery: Some(format!(
                "hostler queue stop {id} --force --confirmation {id}"
            )),
        }
    }

    fn print_stopped(o: &QueueStopOutcome) {
        let id = crate::secrets::sanitize_for_terminal(&o.id);
        if o.stopped {
            println!("Stopped heavy task {id}; its slot is free.");
        } else {
            println!("Heavy task {id} had already ended; nothing was stopped, its slot is free.");
        }
    }
}

fn done(result: Result<(), HostlerError>, message: &str) -> ExitCode {
    match result {
        Ok(()) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

fn since(now_ms: i64, then_ms: i64) -> String {
    duration_text(Duration::from_millis((now_ms - then_ms).max(0) as u64))
}

fn entry_line(e: &HeavyEntry, now_ms: i64, prefix: &str) -> String {
    let time = match e.granted_at_ms {
        Some(at) => format!("held {}", since(now_ms, at)),
        None => format!("waiting {}", since(now_ms, e.enqueued_at_ms)),
    };
    let source = match e.source {
        EntrySource::Hostler => String::new(),
        EntrySource::RnLookyLedger => " [rn-looky ledger]".into(),
    };
    // Another agent's command, kind, directory and (an rn-looky ledger's) id are text, not
    // terminal control (FR-5).
    format!(
        "  {prefix}{}  {}  PID {}  {time}  {}  ({}){source}",
        crate::secrets::sanitize_for_terminal(&e.id),
        crate::secrets::sanitize_for_terminal(&e.kind),
        e.holder.pid,
        crate::secrets::sanitize_for_terminal(&e.command),
        crate::secrets::sanitize_for_terminal(&e.cwd)
    )
}

fn print_snapshot(s: &QueueSnapshot) {
    let now_ms = SystemClock.now_ms();
    let running = s.running.len() + s.foreign_running.len();
    println!(
        "Heavy queue: capacity {}, {running} running, {} waiting",
        s.capacity,
        s.waiting.len()
    );
    if running > 0 {
        println!("Running:");
        for e in s.running.iter().chain(&s.foreign_running) {
            println!("{}", entry_line(e, now_ms, ""));
        }
    }
    if !s.waiting.is_empty() {
        println!("Waiting:");
        for (i, e) in s.waiting.iter().enumerate() {
            println!("{}", entry_line(e, now_ms, &format!("{}. ", i + 1)));
        }
    }
}

fn print_history(items: &Vec<HeavyHistoryItem>) {
    if items.is_empty() {
        println!("No finished heavy tasks.");
        return;
    }
    let now_ms = SystemClock.now_ms();
    for i in items {
        println!("{}", history_line(i, now_ms));
    }
}

fn history_line(i: &HeavyHistoryItem, now_ms: i64) -> String {
    let outcome = serde_json::to_value(i.outcome)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let exit = i.exit_code.map_or_else(|| "-".into(), |c| c.to_string());
    format!(
        "{} ago  {outcome:<9}  exit {exit:<3}  {}  {}  ({})  {}",
        since(now_ms, i.finished_at_ms),
        crate::secrets::sanitize_for_terminal(&i.kind),
        crate::secrets::sanitize_for_terminal(&i.command),
        crate::secrets::sanitize_for_terminal(&i.cwd),
        crate::secrets::sanitize_for_terminal(&i.id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::heavy::{HeavyOutcome, HeavyState};

    /// P5 Task 6: `queue stop` is listed, takes the id and its confirmation, and its `--force`
    /// help carries the D9 sentence.
    #[test]
    fn queue_stop_is_listed_with_the_sentence() {
        use clap::{CommandFactory, Parser};

        let root = crate::cli::Cli::command();
        let stop = root
            .find_subcommand("queue")
            .and_then(|q| q.find_subcommand("stop"))
            .unwrap();
        assert!(!stop.is_hide_set());
        let force = stop
            .get_arguments()
            .find(|a| a.get_id() == "force")
            .and_then(|a| a.get_help())
            .map(|h| h.to_string())
            .unwrap();
        assert!(
            force.contains(crate::contract::claims::FORCE_SENTENCE),
            "{force}"
        );
        let cli = crate::cli::Cli::try_parse_from([
            "hostler",
            "queue",
            "stop",
            "e1",
            "--force",
            "--confirmation",
            "e1",
            "--json",
        ])
        .unwrap();
        let crate::cli::Command::Queue(args) = cli.command else {
            panic!("not a queue command");
        };
        assert!(matches!(
            args.command,
            Some(QueueCommand::Stop { ref id, force: true, confirmation: Some(ref c), json: true })
                if id == "e1" && c == "e1"
        ));
    }

    #[test]
    fn queue_stop_refusals_name_the_full_command() {
        let e = stop::force_required("e1");
        assert_eq!(e.code, ErrorCode::ForceRequired);
        assert_eq!(
            e.message,
            "stopping e1 ends its processes; An agent uses --force only with the operator's \
             explicit permission."
        );
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler queue stop e1 --force --confirmation e1")
        );
        let e = stop::confirm_with_id("e1");
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::BadRequest, "type the task's id to confirm")
        );
    }

    #[test]
    fn since_accepts_units_and_rejects_the_rest() {
        assert_eq!(parse_since("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_since("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_since("3h"), Ok(Duration::from_secs(10_800)));
        assert_eq!(parse_since("1d"), Ok(Duration::from_secs(86_400)));
        for bad in ["", "d", "1w", "-1h", "1.5h", "h1"] {
            assert!(parse_since(bad).is_err(), "{bad}");
        }
    }

    fn holder() -> crate::procs::ProcessIdentity {
        crate::procs::ProcessIdentity {
            pid: 42,
            boot_id: "b1".into(),
            start_time: 1,
        }
    }

    #[test]
    fn queue_lines_sanitize_another_agents_command_and_cwd() {
        // FR-5: an rn-looky ledger's id is another agent's text too.
        let entry = HeavyEntry {
            id: "looky:x\u{1b}]0;PWNED\u{7}\u{1b}[2J".into(),
            ticket: 1,
            kind: "other".into(),
            command: "make \u{1b}]0;PWNED\u{7}".into(),
            cwd: "/work/d\u{1b}[2J".into(),
            workspace_root: None,
            holder: holder(),
            child: None,
            group: None,
            state: HeavyState::Running,
            enqueued_at_ms: 0,
            granted_at_ms: Some(0),
            source: EntrySource::Hostler,
        };
        let line = entry_line(&entry, 1_000, "");
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(!line.contains('\u{7}'), "{line:?}");
        assert!(line.contains('\u{FFFD}'), "{line:?}");

        let item = HeavyHistoryItem {
            id: entry.id.clone(),
            ticket: entry.ticket,
            kind: entry.kind.clone(),
            command: entry.command.clone(),
            cwd: entry.cwd.clone(),
            workspace_root: None,
            enqueued_at_ms: 0,
            granted_at_ms: Some(0),
            finished_at_ms: 1_000,
            exit_code: Some(0),
            outcome: HeavyOutcome::Ok,
        };
        let line = history_line(&item, 2_000);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(!line.contains('\u{7}'), "{line:?}");
    }
}
