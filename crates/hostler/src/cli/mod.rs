pub mod account;
pub mod claim_wait;
pub mod devices;
pub mod doctor;
pub mod guide;
pub mod hold;
pub mod init;
pub mod output;
pub mod project;
pub mod queue;
pub mod resource_run;
pub mod run;
pub mod secret_run;
pub mod secrets;
pub mod service;
pub mod spawn;
pub mod status;
pub mod takeover;
pub mod tty;
pub mod wait;
pub mod workspace;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::client::Replace;

/// Whether ordinary commands replace an older service on a contract
/// mismatch (D-Q1, owner 2026-11-16: limited). `Never`: they return `ContractMismatch` with the
/// recovery `hostler service start`; only `hostler service start`, `hostler doctor --fix` and
/// `hostler init --write` pass `Replace::OnMismatch`.
pub const REPLACE_ON_START: Replace = Replace::Never;

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
    /// Hold a resource (the screen, a test account) for this workspace until it is released.
    Hold(hold::HoldArgs),
    /// Release a hold or another claim by its id.
    Release(hold::ReleaseArgs),
    /// This project's test accounts, kept in the secrets database.
    Account(account::AccountArgs),
    /// One overview: the heavy queue, every claimed resource, the secrets database.
    Status(status::StatusArgs),
    /// This checkout's workspace id; claims belong to workspaces.
    Workspace(workspace::WorkspaceArgs),
    /// Phones, emulators and simulators: register them, see who holds them, their installs.
    Devices(devices::DevicesArgs),
    /// Set this project up for hostler (preview; --write applies).
    Init(init::InitArgs),
    /// Check hostler and this project; --fix applies the safe fixes.
    Doctor(doctor::DoctorArgs),
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
        Command::Hold(args) => hold::main(args),
        Command::Release(args) => hold::main_release(args),
        Command::Account(args) => account::main(args),
        Command::Status(args) => status::main(args),
        Command::Workspace(args) => workspace::main(args),
        Command::Devices(args) => devices::main(args),
        Command::Init(args) => init::main(args),
        Command::Doctor(args) => doctor::main(args),
        Command::Gate { fd, job, command } => {
            std::process::exit(spawn::gate_main(fd, job.as_deref(), &command))
        }
    }
}

/// A clap failure. Sensitive commands (`unlock`, `secret set`, `secret import`, `account set`) may have been
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
        ["account", "set", ..] => Some("account set"),
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

    #[test]
    fn account_set_has_fixed_parse_errors() {
        assert_eq!(
            sensitive(&["hostler", "account", "set", "qa-user", "--field", "session"]),
            Some("account set")
        );
        assert_eq!(
            sensitive(&["hostler", "account", "set", "qa-user", "--hunter2-canary"]),
            Some("account set")
        );
        assert_eq!(sensitive(&["hostler", "account", "list"]), None);
        assert_eq!(
            sensitive(&["hostler", "account", "remove", "qa-user"]),
            None
        );
        // A value given as an argument lands in the hidden sink, which the command refuses.
        let cli = Cli::try_parse_from([
            "hostler",
            "account",
            "set",
            "qa-user",
            "--field",
            "session",
            "hunter2-canary",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::Account(_)));
        let error = Cli::try_parse_from(["hostler", "account", "set", "--help"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    /// Every `--force` flag's help carries the D9 sentence.
    #[test]
    fn force_help_has_the_sentence() {
        use clap::CommandFactory;

        let root = Cli::command();
        let force_help = |path: &[&str]| {
            let mut command = &root;
            for name in path {
                command = command.find_subcommand(name).unwrap();
            }
            command
                .get_arguments()
                .find(|a| a.get_id() == "force")
                .and_then(|a| a.get_help())
                .map(|h| h.to_string())
                .unwrap_or_default()
        };
        for path in [
            &["hold", "screen"][..],
            &["hold", "account"],
            &["hold", "device"],
            &["run"],
            &["release"],
            &["queue", "release"],
            &["service", "stop"],
            &["service", "restart"],
            &["service", "install"],
            &["service", "uninstall"],
        ] {
            assert!(
                force_help(path).contains(crate::contract::claims::FORCE_SENTENCE),
                "{path:?}: {}",
                force_help(path)
            );
        }
    }

    #[test]
    fn claim_commands_parse() {
        for args in [
            &[
                "hostler",
                "hold",
                "screen",
                "--ttl",
                "0.5",
                "--no-wait",
                "--json",
            ][..],
            &["hostler", "hold", "screen", "--while-parent", "--distinct"],
            &["hostler", "hold", "screen", "--ttl", "1440"],
            &[
                "hostler",
                "hold",
                "screen",
                "--ttl",
                "0.02",
                "--max-wait",
                "0",
            ],
            &[
                "hostler",
                "hold",
                "account",
                "qa-user",
                "--while-pid",
                "42",
                "--max-wait",
                "0",
            ],
            &[
                "hostler",
                "hold",
                "screen",
                "--force",
                "--confirmation",
                "screen",
            ],
            &["hostler", "hold", "renew", "h1", "--json"],
            &["hostler", "release", "h1"],
            &[
                "hostler",
                "release",
                "c1",
                "--force",
                "--confirmation",
                "screen",
                "--json",
            ],
            &["hostler", "account", "list", "--json"],
            &["hostler", "account", "remove", "qa-user", "--field", "env"],
            &["hostler", "status", "--json"],
            &["hostler", "workspace", "--json"],
            &["hostler", "workspace", "list", "--json"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["hostler", "hold", "screen", "--ttl", "5", "--while-parent"][..],
            &["hostler", "hold", "screen", "--force"],
            &["hostler", "hold", "screen", "--confirmation", "screen"],
            &["hostler", "release", "c1", "--force"],
            &["hostler", "hold", "screen", "--ttl", "-1"],
            &["hostler", "hold", "screen", "--ttl", "0"],
            &["hostler", "hold", "screen", "--ttl", "1440.5"],
            &["hostler", "hold", "account", "qa-user", "--ttl", "x"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn device_commands_parse() {
        for args in [
            &["hostler", "devices", "list"][..],
            &["hostler", "devices", "list", "--all", "--json"],
            &["hostler", "devices", "show", "pixel-9", "--json"],
            &["hostler", "devices", "add", "SERIAL0001"],
            &[
                "hostler",
                "devices",
                "add",
                "emulator-5554",
                "--alias",
                "pixel_9",
            ],
            &["hostler", "devices", "rename", "pixel-9", "left"],
            &["hostler", "devices", "note", "pixel-9", "on the left"],
            &["hostler", "devices", "note", "pixel-9", "--clear"],
            &["hostler", "devices", "remove", "pixel-9"],
            &["hostler", "devices", "reserve", "pixel-9"],
            &["hostler", "devices", "unreserve", "pixel-9"],
            &["hostler", "devices", "installs"],
            &[
                "hostler", "devices", "installs", "pixel-9", "--since", "1d", "--limit", "20",
                "--json",
            ],
            &["hostler", "devices", "forget-sessions", "pixel-9"],
            &["hostler", "devices", "tools", "--json"],
            &["hostler", "devices", "tools", "--detect"],
            &[
                "hostler",
                "devices",
                "tools",
                "--adb",
                "/sdk/platform-tools/adb",
                "--agent-device",
                "/x/agent-device.mjs",
                "--node",
                "/x/node",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["hostler", "devices"][..],
            &["hostler", "devices", "show"],
            &["hostler", "devices", "note", "pixel-9"],
            &["hostler", "devices", "note", "pixel-9", "x", "--clear"],
            &["hostler", "devices", "rename", "pixel-9"],
            &["hostler", "devices", "forget-sessions"],
            &["hostler", "devices", "forget_sessions", "pixel-9"],
            &["hostler", "devices", "installs", "--limit", "x"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
        // The person-only commands say so in their help: the registry for physical devices only
        // (L49), `forget-sessions` always (L54).
        use clap::CommandFactory;
        let root = Cli::command();
        let devices = root.find_subcommand("devices").unwrap();
        for (name, help) in [
            ("add", devices::DEVICE_PERSON_ONLY),
            ("rename", devices::DEVICE_PERSON_ONLY),
            ("remove", devices::DEVICE_PERSON_ONLY),
            ("forget-sessions", secrets::PERSON_ONLY),
        ] {
            let after = devices
                .find_subcommand(name)
                .unwrap()
                .get_after_help()
                .map(|h| h.to_string())
                .unwrap_or_default();
            assert_eq!(after, help, "{name}");
        }
        assert!(devices::DEVICE_PERSON_ONLY.contains("a person runs this"));
        assert!(devices::DEVICE_PERSON_ONLY.contains("emulators and simulators"));
    }

    #[test]
    fn device_flags_parse_and_refuse_for_now() {
        for args in [
            &["hostler", "hold", "device", "pixel-9"][..],
            &[
                "hostler",
                "hold",
                "device",
                "auto:android",
                "--boot",
                "--json",
            ],
            &[
                "hostler",
                "hold",
                "device",
                "auto",
                "--distinct",
                "--while-parent",
                "--json",
            ],
            &[
                "hostler",
                "hold",
                "device",
                "pixel-9",
                "--force",
                "--confirmation",
                "pixel-9",
            ],
            &["hostler", "run", "--device", "pixel-9", "--", "true"],
            &[
                "hostler",
                "run",
                "--device",
                "auto:ios:simulator",
                "--boot",
                "--max-wait",
                "0",
                "--",
                "true",
            ],
            &[
                "hostler", "run", "--device", "auto", "--screen", "--heavy", "--secret", "API_KEY",
                "--", "true",
            ],
            &[
                "hostler",
                "run",
                "--device",
                "pixel-9",
                "--force",
                "--confirmation",
                "pixel-9",
                "--",
                "true",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        for args in [
            &["hostler", "hold", "device"][..],
            &["hostler", "run", "--boot", "--", "true"],
            &["hostler", "run", "--boot", "--screen", "--", "true"],
            &["hostler", "run", "--device", "--", "true"],
            &["hostler", "hold", "screen", "--boot"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }

        // The bodies are tested where they run: tests/device_run.rs (Task 7) and
        // tests/devices_cli.rs (Task 6).
    }
}
