//! `hostler unlock`, `lock` and `secret …` (plan P2, Tasks 8–9).

pub mod import;
pub mod request;
pub mod secret;
pub mod unlock;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use serde::de::DeserializeOwned;

use crate::cli::output;
use crate::contract::secrets::{ProjectInfo, ProjectRef};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};

/// Refusals and `--help` of the person-only commands.
pub(crate) const PERSON_ONLY: &str = "A person runs this; an agent asks the operator.";

pub(crate) const INVALID_SECRET_NAME: &str =
    "invalid secret name (NAME must match ^[A-Z][A-Z0-9_]{0,63}$)";

const VALUES_ARE_ARGUMENTS: &str = "values are never passed as arguments: a person types them at the prompt, or pipe them on stdin";

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct SecretArgs {
    #[command(subcommand)]
    command: SecretCommand,
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// The secrets database: locked, unlocked or missing.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// This project's secrets: declared, stored and shared in.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Store a secret; the value comes from a prompt or stdin, never an argument.
    Set {
        name: String,
        #[arg(hide = true, allow_hyphen_values = true, num_args = 0..)]
        extra: Vec<OsString>,
    },
    /// Remove a secret from this project.
    Remove {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Move an .env file's values into this project.
    Import {
        file: PathBuf,
        /// Replace secrets that are already set.
        #[arg(long)]
        overwrite: bool,
        /// Delete the file after a successful import.
        #[arg(long)]
        delete: bool,
        #[arg(hide = true, allow_hyphen_values = true, num_args = 0..)]
        extra: Vec<OsString>,
    },
    /// Share one of this project's secrets with another project.
    #[command(after_help = PERSON_ONLY)]
    Share {
        name: String,
        /// The target's project name, or a path inside its checkout.
        #[arg(long, value_name = "PROJECT")]
        to: String,
    },
    /// Stop sharing a secret with a project.
    #[command(after_help = PERSON_ONLY)]
    Unshare {
        name: String,
        /// The target's project name, or a path inside its checkout.
        #[arg(long, value_name = "PROJECT")]
        to: String,
    },
    /// Ask a person for a secret this project needs, and wait for the answer.
    Request {
        name: String,
        /// Why the secret is needed (shown to the person).
        #[arg(long, allow_hyphen_values = true)]
        reason: String,
        /// Minutes to wait (fractions allowed; 0 = no limit). Default 10.
        #[arg(long, value_name = "MIN", value_parser = request::parse_minutes)]
        max_wait: Option<f64>,
        #[arg(long)]
        json: bool,
    },
    /// Pending secret requests of all projects.
    Requests {
        #[arg(long)]
        json: bool,
    },
    /// Decline a pending request by its id (8+ characters), or every pending request for NAME.
    Decline {
        #[arg(value_name = "ID|NAME")]
        target: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn main(args: SecretArgs) -> ExitCode {
    match args.command {
        SecretCommand::Status { json } => secret::main_status(json),
        SecretCommand::List { json } => secret::main_list(json),
        SecretCommand::Set { name, extra } => secret::main_set(name, extra),
        SecretCommand::Remove { name, json } => secret::main_remove(name, json),
        SecretCommand::Import {
            file,
            overwrite,
            delete,
            extra,
        } => import::main(file, overwrite, delete, extra),
        SecretCommand::Share { name, to } => secret::main_share(name, to),
        SecretCommand::Unshare { name, to } => secret::main_unshare(name, to),
        SecretCommand::Request {
            name,
            reason,
            max_wait,
            json,
        } => request::main_request(name, reason, max_wait, json),
        SecretCommand::Requests { json } => request::main_requests(json),
        SecretCommand::Decline { target, json } => request::main_decline(target, json),
    }
}

/// One service call. It creates its own runtime and drops it before the caller prompts again,
/// so a person prompt never blocks a runtime thread.
pub(crate) fn call<T: DeserializeOwned>(request: Request) -> Result<T, HostlerError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| internal(format!("cannot start the async runtime: {e}")))?;
    runtime.block_on(crate::cli::output::call(request))
}

pub(crate) fn internal(message: impl Into<String>) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: message.into(),
        details: serde_json::json!({}),
        recovery: None,
    }
}

pub(crate) fn report_error(error: &HostlerError) -> ExitCode {
    output::print_error(error);
    ExitCode::FAILURE
}

/// A value reached the command line: refuse without echoing it.
pub(crate) fn values_are_arguments() -> ExitCode {
    eprintln!("[hostler] {VALUES_ARE_ARGUMENTS}");
    ExitCode::from(2)
}

/// An invalid secret name is never echoed.
pub(crate) fn invalid_name() -> ExitCode {
    eprintln!("[hostler] {INVALID_SECRET_NAME}");
    ExitCode::from(2)
}

pub(crate) fn checked_name(name: &str) -> Result<(), ExitCode> {
    if crate::secrets::valid_secret_name(name) {
        Ok(())
    } else {
        Err(invalid_name())
    }
}

pub(crate) fn cwd() -> Result<PathBuf, HostlerError> {
    std::env::current_dir().map_err(|e| internal(format!("cannot read the current directory: {e}")))
}

pub(crate) fn current_project() -> Result<ProjectRef, HostlerError> {
    project_at(&cwd()?)
}

pub(crate) fn project_at(path: &Path) -> Result<ProjectRef, HostlerError> {
    crate::project::current(path).map(|current| current.project)
}

/// A target given by name (unique in `project list`) or by a path inside its checkout.
pub(crate) fn target_project(value: &str) -> Result<ProjectInfo, HostlerError> {
    let is_path = Path::new(value).is_absolute()
        || matches!(value, "." | "..")
        || value.contains(['/', '\\']);
    if is_path {
        let project = project_at(Path::new(value))?;
        return call(Request::ProjectResolve(project));
    }

    let mut named = call::<Vec<ProjectInfo>>(Request::ProjectList)?
        .into_iter()
        .filter(|project| project.name == value);
    let found = named.next().ok_or_else(|| {
        crate::contract::secrets::errors::bad_request(format!(
            "no project named {}",
            crate::secrets::sanitize_for_terminal(value)
        ))
    })?;
    if named.next().is_some() {
        return Err(crate::contract::secrets::errors::bad_request(format!(
            "more than one project is named {}; pass a path inside its checkout",
            crate::secrets::sanitize_for_terminal(value)
        )));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn a_value_after_the_name_lands_in_the_hidden_extra() {
        let cli = crate::cli::Cli::try_parse_from([
            "hostler",
            "secret",
            "set",
            "API_KEY",
            "--hunter2-canary",
        ])
        .unwrap();
        let crate::cli::Command::Secret(SecretArgs { command }) = cli.command else {
            panic!("not a secret command");
        };
        match command {
            SecretCommand::Set { name, extra } => {
                assert_eq!(name, "API_KEY");
                assert_eq!(extra, ["--hunter2-canary"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn known_flags_still_parse_after_a_positional() {
        let cli = crate::cli::Cli::try_parse_from([
            "hostler",
            "secret",
            "import",
            "f.env",
            "--delete",
            "--overwrite",
        ])
        .unwrap();
        let crate::cli::Command::Secret(SecretArgs { command }) = cli.command else {
            panic!("not a secret command");
        };
        match command {
            SecretCommand::Import {
                file,
                overwrite,
                delete,
                extra,
            } => {
                assert_eq!(file, std::path::PathBuf::from("f.env"));
                assert!(overwrite && delete);
                assert!(extra.is_empty());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
