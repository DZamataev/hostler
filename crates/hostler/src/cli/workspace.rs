//! `hostler workspace [list]`: this checkout's workspace identity (plan P3a, Task 2).

use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::cli::output::report;
use crate::cli::secrets::{call, internal};
use crate::client::Client;
use crate::contract::HostlerError;
use crate::contract::claims::{WorkspaceInfo, WorkspaceOutcome, WorkspaceResolve};
use crate::contract::wire::Request;
use crate::project::{Current, display_path};
use crate::secrets::sanitize_for_terminal;
use crate::service::manager::OsManager;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: Option<WorkspaceCommand>,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Every recorded workspace.
    List {
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
}

pub fn main(args: WorkspaceArgs) -> ExitCode {
    match args.command {
        Some(WorkspaceCommand::List { json }) => report(
            call::<Vec<WorkspaceInfo>>(Request::WorkspaceList),
            json,
            |workspaces| print_workspaces(workspaces),
        ),
        None => report(resolve_here(), args.json, print_workspace),
    }
}

/// Sends `workspace.resolve` for this checkout; P2's commands call it before `project.resolve`,
/// so a moved repository's project follows on the first of them (L15).
pub async fn resolve(
    client: &mut Client,
    current: &Current,
) -> Result<WorkspaceInfo, HostlerError> {
    let value = client.call(resolve_request(current)).await?;
    serde_json::from_value(value)
        .map_err(|e| internal(format!("unreadable answer from the service: {e}")))
}

/// The one `workspace.resolve` frame of a checkout (P5): with the declarations of its
/// `hostler.toml` (names and purposes only), or `None` without one. A config with problems
/// never gets here: `project::current` refuses it first.
pub fn resolve_request(current: &Current) -> Request {
    Request::WorkspaceResolve(WorkspaceResolve {
        project: current.project.clone(),
        root: display_path(&current.checkout.workspace_root),
        declared: current.config.as_ref().map(|c| c.declaration()),
    })
}

fn resolve_here() -> Result<WorkspaceInfo, HostlerError> {
    let cwd = std::env::current_dir()
        .map_err(|e| internal(format!("cannot read the current directory: {e}")))?;
    let current = crate::project::current(&cwd)?;
    // P4 R2-2: a denied state or socket directory is `sandbox_denied`.
    let paths = crate::client::resolve_paths()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| internal(format!("cannot start the async runtime: {e}")))?;
    runtime.block_on(async {
        let mut client =
            Client::connect_or_start(&paths, &OsManager, crate::cli::REPLACE_ON_START).await?;
        resolve(&mut client, &current).await
    })
}

fn print_workspace(info: &WorkspaceInfo) {
    for line in describe(info) {
        println!("{line}");
    }
}

fn print_workspaces(workspaces: &[WorkspaceInfo]) {
    for (index, info) in workspaces.iter().enumerate() {
        if index > 0 {
            println!();
        }
        for line in describe(info) {
            println!("{line}");
        }
    }
}

fn describe(info: &WorkspaceInfo) -> Vec<String> {
    let mut lines = vec![
        sanitize_for_terminal(&info.path),
        format!("  id: {}", sanitize_for_terminal(&info.id)),
        format!("  project: {}", sanitize_for_terminal(&info.project_id)),
        format!("  status: {}", outcome_label(info.outcome)),
    ];
    if info.project_repointed {
        lines.push("  the project moved here".into());
    }
    if let Some(note) = &info.note {
        lines.push(format!("  note: {}", sanitize_for_terminal(note)));
    }
    lines
}

fn outcome_label(outcome: WorkspaceOutcome) -> &'static str {
    match outcome {
        WorkspaceOutcome::Registered => "registered",
        WorkspaceOutcome::Known => "known",
        WorkspaceOutcome::Moved => "moved",
        WorkspaceOutcome::Reidentified => "reidentified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(outcome: WorkspaceOutcome, project_repointed: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
            project_id: "p".into(),
            path: "/w/repo".into(),
            outcome,
            project_repointed,
            note: Some("the old project waits elsewhere".into()),
            last_seen_ms: 1,
        }
    }

    #[test]
    fn workspace_output_names_the_outcome_and_a_project_move() {
        let lines = describe(&info(WorkspaceOutcome::Reidentified, true));
        assert!(lines.iter().any(|line| line == "  status: reidentified"));
        assert!(
            lines
                .iter()
                .any(|line| line.trim() == "the project moved here")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "  note: the old project waits elsewhere")
        );

        let known = describe(&info(WorkspaceOutcome::Known, false));
        assert!(known.iter().any(|line| line == "  status: known"));
        assert!(!known.iter().any(|line| line.contains("moved here")));
    }

    #[test]
    fn every_outcome_has_its_word() {
        assert_eq!(outcome_label(WorkspaceOutcome::Registered), "registered");
        assert_eq!(outcome_label(WorkspaceOutcome::Known), "known");
        assert_eq!(outcome_label(WorkspaceOutcome::Moved), "moved");
        assert_eq!(
            outcome_label(WorkspaceOutcome::Reidentified),
            "reidentified"
        );
    }
}
