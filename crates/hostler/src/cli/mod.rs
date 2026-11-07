pub mod guide;
pub mod queue;
pub mod run;
pub mod service;
pub mod spawn;
pub mod wait;

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
    /// Run a command once a slot in the machine-wide heavy queue is free.
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
    match Cli::parse().command {
        // Exact codes, also Windows codes above 255.
        Command::Run(args) => std::process::exit(run::main(args)),
        Command::Queue(args) => queue::main(args),
        Command::Service(command) => service::main(command),
        Command::Guide { topic, json } => guide::main(topic, json),
        Command::Gate { fd, job, command } => {
            std::process::exit(spawn::gate_main(fd, job.as_deref(), &command))
        }
    }
}
