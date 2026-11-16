//! `hostler queue …` (plan Task 9): look at the heavy queue and its history, withdraw a waiting
//! request, force-release a slot, set the capacity.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::json;

use super::output::{call, print_error, report};
use super::wait::duration_text;
use crate::contract::heavy::{
    DEFAULT_HISTORY_LIMIT, EntrySource, HeavyEntry, HeavyHistoryItem, HistoryQuery, QueueSnapshot,
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
