//! `hostler status`: the heavy queue, every claimed resource and the secrets database in one
//! overview (plan P3a, Task 10). A read: it never registers or re-identifies a checkout.

use std::process::ExitCode;
use std::time::Duration;

use clap::Args;

use crate::cli::devices::{device_status_line, print_problems};
use crate::cli::output::{print_error, report};
use crate::client::Client;
use crate::contract::claims::{
    Claim, ClaimKind, HoldBinding, Overview, ResourceKind, ResourceState,
};
use crate::contract::devices::DevicesSnapshot;
use crate::contract::secrets::LockState;
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::{Clock, SystemClock};
use crate::secrets::sanitize_for_terminal;
use crate::service::manager::OsManager;

/// The bound on the human "this workspace" line (L40): a hanging mount never holds up the overview.
const WORKSPACE_LINE_BOUND: Duration = Duration::from_secs(2);

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print the contract's JSON envelope.
    #[arg(long)]
    pub json: bool,
}

pub fn main(args: StatusArgs) -> ExitCode {
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
    runtime.block_on(async {
        let overview = collect().await;
        if args.json {
            // L40: the JSON answer is the contract's `Envelope<Overview>`; it never pays for the
            // workspace line, and a read never registers a checkout.
            return report(overview, true, |_: &Overview| {});
        }
        match overview {
            Err(e) => {
                print_error(&e);
                ExitCode::FAILURE
            }
            Ok(overview) => {
                // P3b Task 6: the device lines with their connection, telemetry and standing.
                // A read; a failed second read leaves the claims resource lines as they were.
                let devices = collect_devices().await.ok();
                print_overview(&overview, devices.as_ref());
                if let Some(line) = local_workspace_line().await {
                    println!("{line}");
                }
                ExitCode::SUCCESS
            }
        }
    })
}

async fn collect() -> Result<Overview, HostlerError> {
    // P4 R1-2: a denied state or socket directory is `sandbox_denied`.
    let paths = crate::client::resolve_paths()?;
    let mut client =
        Client::connect_or_start(&paths, &OsManager, crate::cli::REPLACE_ON_START).await?;
    let value = client.call(Request::StatusOverview).await?;
    serde_json::from_value(value)
        .map_err(|e| internal(format!("unreadable answer from the service: {e}")))
}

/// P3b Task 6: the devices for the resource lines. Never starts a service: `collect` already
/// reached one, and a failed read only loses the device detail.
async fn collect_devices() -> Result<DevicesSnapshot, HostlerError> {
    // P4 R1-2: a denied state or socket directory is `sandbox_denied`.
    let paths = crate::client::resolve_paths()?;
    let mut client = Client::connect(&paths).await?;
    let value = client.call(Request::DevicesList { all: false }).await?;
    serde_json::from_value(value)
        .map_err(|e| internal(format!("unreadable answer from the service: {e}")))
}

/// L40: this is a read. It reads the id file the service wrote, if any, and says when this
/// checkout has none; it never calls `workspace.resolve`. Bounded, because a dead mount hangs
/// `find_checkout` too.
async fn local_workspace_line() -> Option<String> {
    let task = tokio::task::spawn_blocking(local_workspace_info);
    match tokio::time::timeout(WORKSPACE_LINE_BOUND, task).await {
        Ok(Ok(line)) => line,
        _ => None,
    }
}

fn local_workspace_info() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let checkout = crate::project::find_checkout(&cwd).ok()?;
    let path = crate::project::display_path(&checkout.workspace_root);
    Some(match crate::workspace::read_id_file(&checkout.git_dir) {
        Some(file) => format!(
            "This workspace: {} ({})",
            sanitize_for_terminal(&path),
            sanitize_for_terminal(&file.id)
        ),
        None => format!(
            "This checkout ({}) is not registered yet; run `hostler workspace`.",
            sanitize_for_terminal(&path)
        ),
    })
}

fn internal(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message,
        details: serde_json::json!({}),
        recovery: None,
    }
}

fn print_overview(overview: &Overview, devices: Option<&DevicesSnapshot>) {
    let heavy = &overview.heavy;
    let running = heavy.running.len() + heavy.foreign_running.len();
    println!(
        "Heavy queue: capacity {}, {running} running, {} waiting.",
        heavy.capacity,
        heavy.waiting.len()
    );
    let now = SystemClock.now_ms();
    for state in &overview.claims.resources {
        if state.resource.kind == ResourceKind::Device
            && let Some(snapshot) = devices
        {
            match snapshot
                .devices
                .iter()
                .find(|d| d.resource == state.resource.id)
            {
                Some(info) if info.registered => {
                    println!(
                        "{}",
                        device_status_line(info, Some(state), now, &snapshot.tools)
                    );
                    continue;
                }
                // An unregistered phone is not a resource; the hint below counts it.
                Some(_) => continue,
                // The snapshot raced: keep the claims resource line.
                None => {}
            }
        }
        println!("{}", resource_line(state));
    }
    if let Some(snapshot) = devices {
        let new_physical = snapshot
            .devices
            .iter()
            .filter(|d| !d.registered && d.kind == crate::contract::devices::DeviceKind::Physical)
            .count();
        if new_physical == 1 {
            println!("1 new device: hostler devices list shows it");
        } else if new_physical > 1 {
            println!("{new_physical} new devices: hostler devices list shows them");
        }
        print_problems(snapshot);
    }
    println!("Secrets: {}.", lock_word(overview.secrets.state));
}

fn lock_word(state: LockState) -> &'static str {
    match state {
        LockState::Missing => "missing",
        LockState::Locked => "locked",
        LockState::Unlocked => "unlocked",
    }
}

/// `{display}: free`, `{display}: held by {path} ({what}, +{n} joined)`,
/// `…; being taken over by {path}` or `…; {n} waiting`.
fn resource_line(state: &ResourceState) -> String {
    let joined = state.claims.len().saturating_sub(1);
    let mut parts: Vec<String> = state
        .claims
        .first()
        .map(|claim| vec![held_part(claim, joined)])
        .unwrap_or_default();
    parts.extend(state.foreign.iter().map(|foreign| {
        format!(
            "{}: {}",
            sanitize_for_terminal(&foreign.source),
            sanitize_for_terminal(&foreign.description)
        )
    }));
    let held = if parts.is_empty() {
        "free".to_owned()
    } else {
        parts.join("; ")
    };
    let display = sanitize_for_terminal(&state.resource.display);
    let mut line = format!("{display}: {held}");
    if state.seizing {
        line.push_str(&match taker_of(state) {
            Some(path) => format!("; being taken over by {}", sanitize_for_terminal(path)),
            None => "; being taken over".to_owned(),
        });
    }
    let waiting = state.waiting.len();
    if waiting > 0 {
        line.push_str(&format!("; {waiting} waiting"));
    }
    line
}

/// A takeover's taker: the snapshot's `seizing_by`, or, from a service that does not send it, the
/// one queued request of another workspace (the taker of `run --force` / `hold --force`).
pub(crate) fn taker_of(state: &ResourceState) -> Option<&str> {
    if let Some(taker) = state.seizing_by.as_deref() {
        return Some(taker);
    }
    let holders: Vec<&str> = state
        .claims
        .iter()
        .map(|claim| claim.workspace.path.as_str())
        .collect();
    let mut waiting = state
        .waiting
        .iter()
        .map(|request| request.workspace_path.as_str())
        .filter(|path| !holders.contains(path));
    let first = waiting.next()?;
    waiting.all(|path| path == first).then_some(first)
}

pub(crate) fn held_part(claim: &Claim, joined: usize) -> String {
    let what = match claim.kind {
        ClaimKind::Lease => format!(
            "a run of {}, PID {}",
            claim.command.as_deref().unwrap_or("a command"),
            claim
                .holder
                .as_ref()
                .map_or_else(|| "?".to_owned(), |holder| holder.pid.to_string())
        ),
        ClaimKind::Hold => match &claim.binding {
            Some(HoldBinding::Ttl { expires_at_ms, .. }) => format!(
                "hold {} until {}",
                claim.id,
                crate::cli::claim_wait::hh_mm(*expires_at_ms)
            ),
            Some(HoldBinding::Process { holder }) => {
                format!("hold {} while PID {} lives", claim.id, holder.pid)
            }
            None => format!("hold {}", claim.id),
        },
        ClaimKind::Reservation => format!("reservation {}", claim.id),
    };
    let joined = if joined > 0 {
        format!(", +{joined} joined")
    } else {
        String::new()
    };
    format!(
        "held by {} ({}{joined})",
        sanitize_for_terminal(&claim.workspace.path),
        // Every other agent's text is sanitized before it reaches a person's terminal (L40).
        sanitize_for_terminal(&what)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::claims::fixtures;

    #[test]
    fn a_resource_line_names_the_holder_and_joined_runs() {
        let mut state = fixtures::snapshot().resources[0].clone();
        state.claims.clear();
        state.foreign.clear();
        state.waiting.clear();
        state.seizing = false;
        assert_eq!(resource_line(&state), "screen: free");

        state.claims = vec![fixtures::claim()];
        state.waiting = fixtures::snapshot().resources[0].waiting.clone();
        assert_eq!(
            resource_line(&state),
            "screen: held by /work/app (a run of make test, PID 42); 1 waiting"
        );

        state.claims.push(fixtures::claim());
        state.waiting.clear();
        assert_eq!(
            resource_line(&state),
            "screen: held by /work/app (a run of make test, PID 42, +1 joined)"
        );
    }

    #[test]
    fn a_resource_line_sanitizes_another_agents_command() {
        let mut state = fixtures::snapshot().resources[0].clone();
        state.foreign.clear();
        state.waiting.clear();
        state.seizing = false;
        state.claims[0].command = Some("x\u{1b}]0;PWNED\u{7}\u{1b}[2J".into());
        let line = resource_line(&state);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(!line.contains('\u{7}'), "{line:?}");
        assert!(line.contains('\u{FFFD}'), "{line:?}");
    }

    #[test]
    fn a_seizing_resource_names_the_taker() {
        let mut state = fixtures::snapshot().resources[0].clone();
        state.seizing = true;
        // An older service sends no `seizing_by`: the queued request of another workspace.
        state.seizing_by = None;
        assert!(
            resource_line(&state).ends_with("; being taken over by /work/other; 1 waiting"),
            "{}",
            resource_line(&state)
        );
        state.waiting.clear();
        assert!(
            resource_line(&state).ends_with("; being taken over"),
            "{}",
            resource_line(&state)
        );
        // A `release --force` takeover has no queued request: the snapshot names the taker.
        state.seizing_by = Some("/work/taker\u{1b}]0;x\u{7}".into());
        let line = resource_line(&state);
        assert!(line.contains("; being taken over by /work/taker"), "{line}");
        assert!(
            !line.contains('\u{1b}') && !line.contains('\u{7}'),
            "{line:?}"
        );
    }
}
