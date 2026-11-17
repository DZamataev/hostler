//! `hostler doctor [--fix] [--json]` (plan P4 Task 6).
//!
//! Human output: one line per check (`ok|warn|FAIL|skip  <id>  <message>`), its fix
//! (`  fix: …` for the agent, `  for the person: …`), what `--fix` did, then a summary.
//! `--json`: `Envelope<DoctorReport>`. Exit codes: 0 when no `problem` remains, 1 otherwise; 78
//! when doctor cannot run at all (no home).

use std::process::ExitCode;

use clap::Args;

use crate::cli::output;
use crate::contract::Envelope;
use crate::contract::doctor::{Check, CheckStatus, DoctorReport, FixResult, Who};
use crate::doctor::{DoctorOptions, run};
use crate::secrets::sanitize_for_terminal;

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Apply the safe fixes, then check again.
    #[arg(long)]
    pub fix: bool,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    pub json: bool,
}

/// P1's setup-failure exit: doctor could not run at all.
const SETUP_FAILURE: u8 = 78;

pub fn main(args: DoctorArgs) -> ExitCode {
    let DoctorArgs { fix, json } = args;
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("[hostler] doctor cannot read the current directory: {e}");
            return ExitCode::from(SETUP_FAILURE);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("[hostler] cannot start the async runtime: {e}");
            return ExitCode::from(SETUP_FAILURE);
        }
    };
    let result = runtime.block_on(run(DoctorOptions { cwd, fix }));
    // A start or a lookup left on a blocking worker never holds the exit (R1-1, R1-4).
    runtime.shutdown_background();
    let report = match result {
        Ok(report) => report,
        Err(e) => {
            if json {
                let envelope: Envelope<DoctorReport> = Envelope {
                    ok: false,
                    data: None,
                    error: Some(e),
                };
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("an envelope always encodes")
                );
            } else {
                output::print_error(&e);
            }
            return ExitCode::from(SETUP_FAILURE);
        }
    };
    let code = exit_code(&report);
    if json {
        let envelope = Envelope {
            ok: true,
            data: Some(report),
            error: None,
        };
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("an envelope always encodes")
        );
    } else {
        print!("{}", human(&report));
    }
    code
}

fn exit_code(report: &DoctorReport) -> ExitCode {
    if report
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::Problem)
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn tag(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Ok => "ok  ",
        CheckStatus::Warn => "warn",
        CheckStatus::Problem => "FAIL",
        CheckStatus::Skipped => "skip",
    }
}

fn check_lines(check: &Check, out: &mut String) {
    let clean = sanitize_for_terminal;
    out.push_str(&format!(
        "{}  {}  {}\n",
        tag(check.status),
        clean(&check.id),
        clean(&check.message)
    ));
    if !matches!(check.status, CheckStatus::Problem | CheckStatus::Warn) {
        return;
    }
    let Some(fix) = &check.fix else {
        return;
    };
    let label = match fix.who {
        Who::Agent => "fix",
        Who::Person => "for the person",
    };
    match &fix.command {
        Some(command) => {
            let safe = if fix.safe {
                " (doctor --fix runs it)"
            } else {
                ""
            };
            out.push_str(&format!("  {label}: {}{safe}\n", clean(command)));
            out.push_str(&format!("      {}\n", clean(&fix.text)));
        }
        None => out.push_str(&format!("  {label}: {}\n", clean(&fix.text))),
    }
    if let Some(person) = check.details.get("person_fix")
        && let Some(command) = person["command"].as_str()
    {
        out.push_str(&format!("  for the person: {}\n", clean(command)));
        if let Some(text) = person["text"].as_str() {
            out.push_str(&format!("      {}\n", clean(text)));
        }
    }
}

/// The human report (pure, for tests).
pub fn human(report: &DoctorReport) -> String {
    let mut out = String::new();
    for check in &report.checks {
        check_lines(check, &mut out);
    }
    if !report.fixed.is_empty() {
        out.push('\n');
        for outcome in &report.fixed {
            let result = match outcome.result {
                FixResult::Fixed => "fixed",
                FixResult::StillFailing => "still failing",
                FixResult::Refused => "refused",
            };
            out.push_str(&format!(
                "{result}: {} ({}): {}\n",
                sanitize_for_terminal(&outcome.check),
                sanitize_for_terminal(&outcome.command),
                sanitize_for_terminal(&outcome.message)
            ));
        }
    }
    let count = |status| {
        report
            .checks
            .iter()
            .filter(|check| check.status == status)
            .count()
    };
    let fixed = report
        .fixed
        .iter()
        .filter(|outcome| outcome.result == FixResult::Fixed)
        .count();
    out.push_str(&format!(
        "\n{} problems, {} warnings; {fixed} fixed\n",
        count(CheckStatus::Problem),
        count(CheckStatus::Warn)
    ));
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::contract::doctor::{Fix, FixOutcome};

    #[test]
    fn human_lines_name_status_id_fix_and_summary() {
        let report = DoctorReport {
            checks: vec![
                Check {
                    id: "service.reachable".into(),
                    status: CheckStatus::Problem,
                    message: "not running".into(),
                    fix: Some(Fix {
                        command: Some("hostler service start".into()),
                        text: "starts it".into(),
                        who: Who::Agent,
                        safe: true,
                    }),
                    details: json!({}),
                },
                Check {
                    id: "project.approved".into(),
                    status: CheckStatus::Warn,
                    message: "not approved".into(),
                    fix: Some(Fix {
                        command: None,
                        text: "a person approves it".into(),
                        who: Who::Person,
                        safe: false,
                    }),
                    details: json!({}),
                },
                Check {
                    id: "tools.xcrun".into(),
                    status: CheckStatus::Skipped,
                    message: "not macOS".into(),
                    fix: None,
                    details: json!({}),
                },
            ],
            fixed: vec![FixOutcome {
                check: "service.reachable".into(),
                command: "hostler service start".into(),
                result: FixResult::Fixed,
                message: "the service answers".into(),
            }],
        };
        let text = human(&report);
        assert!(
            text.contains("FAIL  service.reachable  not running\n"),
            "{text}"
        );
        assert!(
            text.contains("  fix: hostler service start (doctor --fix runs it)\n"),
            "{text}"
        );
        assert!(
            text.contains("  for the person: a person approves it\n"),
            "{text}"
        );
        assert!(text.contains("skip  tools.xcrun  not macOS\n"), "{text}");
        assert!(
            text.contains("fixed: service.reachable (hostler service start)"),
            "{text}"
        );
        assert!(
            text.ends_with("\n1 problems, 1 warnings; 1 fixed\n"),
            "{text}"
        );
    }
}
