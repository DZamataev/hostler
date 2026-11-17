//! `hostler init [--write] [--json]` (plan P4 Task 5): the report, printed for a person or as
//! the contract's JSON envelope.

use std::process::ExitCode;

use clap::Args;

use crate::contract::doctor::Who;
use crate::contract::init::{ConfigState, InitReport, ServiceAction, Step};
use crate::contract::{Envelope, HostlerError};
use crate::init::{InitOptions, run};
use crate::project::config::ConfigProblem;
use crate::project::config_invalid;
use crate::secrets::sanitize_for_terminal;

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Write `hostler.toml` (only when absent) and the docs block, and install or start the
    /// service; without it, init only previews.
    #[arg(long)]
    pub write: bool,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    pub json: bool,
}

pub fn main(args: InitArgs) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            let error = HostlerError {
                code: crate::contract::ErrorCode::Internal,
                message: format!("cannot read the current directory: {e}"),
                details: serde_json::json!({}),
                recovery: None,
            };
            return crate::cli::output::report::<InitReport>(Err(error), args.json, |_| {});
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let error = HostlerError {
                code: crate::contract::ErrorCode::Internal,
                message: format!("cannot start the async runtime: {e}"),
                details: serde_json::json!({}),
                recovery: None,
            };
            return crate::cli::output::report::<InitReport>(Err(error), args.json, |_| {});
        }
    };
    let result = runtime.block_on(run(InitOptions {
        cwd,
        write: args.write,
    }));
    match result {
        Err(error) => crate::cli::output::report::<InitReport>(Err(error), args.json, |_| {}),
        Ok(report) => {
            // An invalid `hostler.toml` writes nothing and exits 1; the report still says what
            // the file's problems are, so a person and an agent both see why.
            let problems = match &report.config {
                ConfigState::Invalid { problems } => Some(to_problems(problems)),
                _ => None,
            };
            let invalid = problems.is_some();
            if args.json {
                let envelope = match problems {
                    None => Envelope {
                        ok: true,
                        data: Some(report),
                        error: None,
                    },
                    Some(problems) => Envelope {
                        ok: false,
                        data: Some(report),
                        error: Some(config_invalid(&problems)),
                    },
                };
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("an envelope always encodes")
                );
            } else {
                print_report(&report);
            }
            if invalid {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

fn to_problems(problems: &[crate::contract::init::LineProblem]) -> Vec<ConfigProblem> {
    problems
        .iter()
        .map(|problem| ConfigProblem {
            line: problem.line,
            message: problem.message.clone(),
        })
        .collect()
}

fn print_report(report: &InitReport) {
    println!("{}", sanitize_for_terminal(&report.root));
    for toolchain in &report.detected.toolchains {
        println!(
            "  toolchain: {} ({})",
            toolchain_label(toolchain.kind),
            sanitize_for_terminal(&toolchain.evidence)
        );
    }
    if !report.detected.apps.is_empty() {
        let apps: Vec<String> = report
            .detected
            .apps
            .iter()
            .map(|app| {
                format!(
                    "{} {}",
                    platform_label(app.platform),
                    sanitize_for_terminal(&app.id)
                )
            })
            .collect();
        println!("  apps: {}", apps.join(", "));
    }
    if !report.detected.env_files.is_empty() {
        let files: Vec<String> = report
            .detected
            .env_files
            .iter()
            .map(|file| {
                format!(
                    "{} ({} names, no values read)",
                    sanitize_for_terminal(&file.path),
                    file.names.len()
                )
            })
            .collect();
        println!("  env files: {}", files.join(", "));
    }
    match &report.config {
        ConfigState::Absent => match &report.proposed_toml {
            Some(toml) => {
                println!("  hostler.toml: absent; --write would create it with:");
                for line in toml.lines() {
                    println!("    {line}");
                }
            }
            None => println!("  hostler.toml: absent"),
        },
        ConfigState::Valid => println!("  hostler.toml: valid (never rewritten)"),
        ConfigState::Invalid { problems } => {
            println!("  hostler.toml: invalid; nothing was written:");
            for problem in problems {
                println!(
                    "    line {}: {}",
                    problem.line,
                    sanitize_for_terminal(&problem.message)
                );
            }
        }
    }
    if !report.docs.is_empty() {
        let docs: Vec<String> = report
            .docs
            .iter()
            .map(|change| {
                let action = match &change.action {
                    crate::contract::init::DocsAction::Create => "create".to_string(),
                    crate::contract::init::DocsAction::Insert => "append the block".to_string(),
                    crate::contract::init::DocsAction::Update => "update the block".to_string(),
                    crate::contract::init::DocsAction::Unchanged => "unchanged".to_string(),
                    crate::contract::init::DocsAction::CoveredBy { path } => {
                        format!("covered by {}", sanitize_for_terminal(path))
                    }
                };
                format!("{}: {action}", sanitize_for_terminal(&change.path))
            })
            .collect();
        println!("  docs: {}", docs.join("; "));
    }
    println!("  service: {}", service_line(&report.service));
    if !report.written.is_empty() {
        println!("  written: {}", report.written.join(", "));
    }
    print_steps(report.steps.iter(), "for you (agent)", Who::Agent);
    print_steps(report.steps.iter(), "for the person", Who::Person);
}

fn toolchain_label(kind: crate::contract::init::ToolchainKind) -> &'static str {
    use crate::contract::init::ToolchainKind as Kind;
    match kind {
        Kind::Gradle => "gradle",
        Kind::KotlinMultiplatform => "kotlin multiplatform",
        Kind::AndroidApp => "android app",
        Kind::ReactNative => "react native",
        Kind::Xcode => "xcode",
        Kind::CocoaPods => "cocoa pods",
        Kind::Cargo => "cargo",
        Kind::Node => "node",
    }
}

fn platform_label(platform: crate::contract::devices::Platform) -> &'static str {
    match platform {
        crate::contract::devices::Platform::Android => "android",
        crate::contract::devices::Platform::Ios => "ios",
    }
}

fn service_line(action: &ServiceAction) -> String {
    match action {
        ServiceAction::Running => "running".into(),
        ServiceAction::WouldInstall { dev_build } => {
            if *dev_build {
                "would install (a dev build: not automatically)".into()
            } else {
                "would install".into()
            }
        }
        ServiceAction::WouldStart => "would start".into(),
        ServiceAction::WouldReplace => "would replace the older running service".into(),
        ServiceAction::Replaced => "replaced the older running service".into(),
        ServiceAction::MismatchKept { direction, .. } => match &direction[..] {
            "newer" => "the running service is older and was kept".into(),
            "older" => "the running service is newer and was kept".into(),
            _ => "the running service's version cannot be compared; it was kept".into(),
        },
        ServiceAction::Installed => "installed and started".into(),
        ServiceAction::Started => "started".into(),
        ServiceAction::StartFailed { message } => {
            format!("the start failed: {}", sanitize_for_terminal(message))
        }
        ServiceAction::InstallRefused { message } => {
            format!("not installed: {}", sanitize_for_terminal(message))
        }
        ServiceAction::Unreachable { reason } => format!("unreachable ({reason})"),
    }
}

fn print_steps<'a>(steps: impl Iterator<Item = &'a Step>, heading: &str, who: Who) {
    let selected: Vec<&Step> = steps.filter(|step| step.who == who).collect();
    if selected.is_empty() {
        return;
    }
    println!("{heading}:");
    for (index, step) in selected.iter().enumerate() {
        println!("  {}. [{}]", index + 1, step.id);
        print_text(&step.text, "     ");
        if let Some(command) = &step.command {
            print_text(command, "     $ ");
        }
    }
}

/// One text for the terminal: each line is sanitized on its own, so a step's line breaks
/// survive while agent-chosen text (a path, a purpose) cannot reach it raw.
fn print_text(text: &str, prefix: &str) {
    for line in text.split('\n') {
        println!("{prefix}{}", sanitize_for_terminal(line));
    }
}
