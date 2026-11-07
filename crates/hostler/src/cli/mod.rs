pub mod guide;
pub mod output;
pub mod project;
pub mod queue;
pub mod run;
pub mod secret_run;
pub mod secrets;
pub mod service;
pub mod spawn;
pub mod tty;
pub mod wait;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Machine-wide resource coordination for parallel agents.
#[derive(Debug, Parser)]
#[command(name = "hostler", version, arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a command, optionally through the heavy queue and/or with secrets injected.
    Run(run::RunArgs),
    /// The machine-wide heavy queue: running and waiting tasks, history, operator actions.
    Queue(queue::QueueArgs),
    /// The per-user background service.
    #[command(subcommand)]
    Service(service::ServiceCommand),
    /// Explain how to use hostler from an agent; no topic lists the topics.
    Guide {
        /// Topic to print, e.g. `heavy`.
        topic: Option<String>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Unlock the secrets database for this service run (person-only).
    #[command(after_help = secrets::PERSON_ONLY)]
    Unlock(secrets::unlock::UnlockArgs),
    /// Lock the secrets database now.
    Lock {
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// This project: its name, directory and approval; approve it for secrets.
    Project(project::ProjectArgs),
    /// This project's secrets, and the secrets database's state.
    Secret(secrets::SecretArgs),
    /// Internal: the gate launcher of `run --heavy`.
    #[command(name = "__gate", hide = true)]
    Gate {
        /// The gate's read end: a descriptor (Unix) or an inherited handle (Windows).
        #[arg(long)]
        fd: u64,
        /// Windows: the job to terminate when an interrupted command will not stop.
        #[arg(long)]
        job: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
}

pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return parse_failure(error),
    };
    match cli.command {
        // Exact codes, also Windows codes above 255.
        Command::Run(args) => std::process::exit(run::main(args)),
        Command::Queue(args) => queue::main(args),
        Command::Service(command) => service::main(command),
        Command::Guide { topic, json } => guide::main(topic, json),
        Command::Unlock(args) => secrets::unlock::main(args),
        Command::Lock { json } => secrets::unlock::main_lock(json),
        Command::Project(args) => project::main(args),
        Command::Secret(args) => secrets::main(args),
        Command::Gate { fd, job, command } => {
            std::process::exit(spawn::gate_main(fd, job.as_deref(), &command))
        }
    }
}

/// A clap failure. Sensitive commands (`unlock`, `secret set`, `secret import`) may have been
/// given a value as an argument, so their parse errors print a fixed text that never quotes one.
fn parse_failure(error: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;

    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        return print_and_code(&error);
    }
    if let Some(command) = sensitive_command(std::env::args_os()) {
        eprintln!(
            "[hostler] invalid arguments for `hostler {command}`; values are never passed as \
             arguments — see `hostler {command} --help`"
        );
        return ExitCode::from(2);
    }
    print_and_code(&error)
}

fn print_and_code(error: &clap::Error) -> ExitCode {
    let code = error.exit_code();
    let _ = error.print();
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}

/// The sensitive command path from the raw arguments, not from clap's error text.
fn sensitive_command<I: IntoIterator<Item = OsString>>(args: I) -> Option<&'static str> {
    let words: Vec<String> = args
        .into_iter()
        .skip(1)
        .filter_map(|arg| arg.into_string().ok())
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["unlock", ..] => Some("unlock"),
        ["secret", "set", ..] => Some("secret set"),
        ["secret", "import", ..] => Some("secret import"),
        // A pasted value can follow either option, in any spelling, anywhere on the line.
        ["run", rest @ ..] if rest.iter().any(|word| secret_argument(word)) => Some("run"),
        _ => None,
    }
}

/// `--secret` or `--secret-file`, with or without `=VALUE`.
fn secret_argument(word: &str) -> bool {
    word == "--secret"
        || word.starts_with("--secret=")
        || word == "--secret-file"
        || word.starts_with("--secret-file=")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensitive(args: &[&str]) -> Option<&'static str> {
        sensitive_command(args.iter().map(OsString::from))
    }

    #[test]
    fn sensitive_commands_are_unlock_set_and_import() {
        assert_eq!(sensitive(&["hostler", "unlock"]), Some("unlock"));
        assert_eq!(
            sensitive(&["hostler", "secret", "set", "API_KEY"]),
            Some("secret set")
        );
        assert_eq!(
            sensitive(&["hostler", "secret", "import", "f.env"]),
            Some("secret import")
        );
        assert_eq!(sensitive(&["hostler", "unlocker"]), None);
        assert_eq!(sensitive(&["hostler", "secret", "list"]), None);
        assert_eq!(sensitive(&["hostler", "secret"]), None);
        assert_eq!(sensitive(&["hostler", "--help"]), None);
        assert_eq!(
            sensitive(&[
                "hostler",
                "run",
                "--secret",
                "API_KEY",
                "--bogus-hunter2-canary",
                "--",
                "true"
            ]),
            Some("run")
        );
        assert_eq!(
            sensitive(&["hostler", "run", "--secret=API_KEY", "--bogus"]),
            Some("run")
        );
        assert_eq!(
            sensitive(&["hostler", "run", "--secret-file", "API_KEY"]),
            Some("run")
        );
        assert_eq!(
            sensitive(&["hostler", "run", "--secret-file=API_KEY", "--bogus"]),
            Some("run")
        );
        assert_eq!(
            sensitive(&["hostler", "run", "--heavy", "--", "true"]),
            None
        );
    }

    #[test]
    fn help_requests_still_parse_as_help() {
        let error = Cli::try_parse_from(["hostler", "secret", "set", "--help"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let error = Cli::try_parse_from(["hostler", "unlock", "--help"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
