//! `hostler project …` (plan P2, Task 8).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::cli::output::report;
use crate::cli::secrets::{PERSON_ONLY, call, current_project, report_error};
use crate::cli::tty::Tty;
use crate::contract::HostlerError;
use crate::contract::secrets::ProjectInfo;
use crate::contract::wire::Request;
use crate::secrets::sanitize_for_terminal;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: Option<ProjectCommand>,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Every recorded project, by name.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Approve this project (or one at <path>) for secrets; a person types its name.
    #[command(after_help = PERSON_ONLY)]
    Approve {
        /// A path inside the checkout; defaults to the current directory.
        path: Option<PathBuf>,
    },
}

pub fn main(args: ProjectArgs) -> ExitCode {
    match args.command {
        None => resolve(args.json),
        Some(ProjectCommand::List { json }) => list(json),
        Some(ProjectCommand::Approve { path }) => approve(path),
    }
}

fn resolve(json: bool) -> ExitCode {
    let result = (|| {
        let project = current_project()?;
        call::<ProjectInfo>(Request::ProjectResolve(project))
    })();
    report(result, json, print_project)
}

fn list(json: bool) -> ExitCode {
    report(
        call::<Vec<ProjectInfo>>(Request::ProjectList),
        json,
        |projects| print_projects(projects),
    )
}

fn approve(path: Option<PathBuf>) -> ExitCode {
    match run_approve(path.unwrap_or_else(|| PathBuf::from("."))) {
        Ok(code) => code,
        Err(e) => report_error(&e),
    }
}

fn run_approve(path: PathBuf) -> Result<ExitCode, HostlerError> {
    // Person-only: refuse before touching the project or the service.
    let mut tty = Tty::open("project approve")?;
    let project_ref = crate::project::current(&path)?.project;
    let project = call::<ProjectInfo>(Request::ProjectResolve(project_ref.clone()))?;
    let name = sanitize_for_terminal(&project.name);
    println!("{name}");
    println!("  dir: {}", sanitize_for_terminal(&project.common_dir));

    let prompt = format!("Type the project name ({name}) to approve it:");
    if !tty.confirm_typed(&prompt, &project.name)? {
        eprintln!("[hostler] the name did not match; the project is not approved");
        return Ok(ExitCode::FAILURE);
    }
    let approved = call::<ProjectInfo>(Request::ProjectApprove(project_ref))?;
    println!("approved {}", sanitize_for_terminal(&approved.name));
    Ok(ExitCode::SUCCESS)
}

fn print_project(project: &ProjectInfo) {
    println!("{}", sanitize_for_terminal(&project.name));
    println!("  dir: {}", sanitize_for_terminal(&project.common_dir));
    println!(
        "  approved: {}",
        if project.approved { "yes" } else { "no" }
    );
}

fn print_projects(projects: &[ProjectInfo]) {
    if projects.is_empty() {
        println!("no projects");
        return;
    }
    for project in projects {
        println!(
            "{} — {} ({})",
            sanitize_for_terminal(&project.name),
            sanitize_for_terminal(&project.common_dir),
            if project.approved {
                "approved"
            } else {
                "not approved"
            }
        );
    }
}
