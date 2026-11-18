//! `hostler doctor`'s report (plan P4 Task 1; bodies in Task 6). A serialisable contract type
//! that no service method carries: it serves `doctor --json` and P5's
//! "config health". No text in it ever holds a secret value or a `hostler.toml` value.

use serde::{Deserialize, Serialize};

/// What `hostler doctor` found, and what `--fix` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DoctorReport {
    /// In the fixed check order.
    pub checks: Vec<Check>,
    /// One per fix `--fix` ran, in the order it ran them; empty without `--fix`.
    pub fixed: Vec<FixOutcome>,
}

/// One check's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Check {
    /// Stable and dotted, e.g. `service.registered` or `secrets.required.JACKETT_API_KEY`.
    pub id: String,
    pub status: CheckStatus,
    /// One line; never a value.
    pub message: String,
    /// What resolves a `warn` or a `problem`; `None` when nothing is to be done.
    pub fix: Option<Fix>,
    /// Machine-readable facts behind the message (never a value).
    pub details: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Problem,
    Skipped,
}

/// How a check's problem is resolved, and by whom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Fix {
    /// A runnable command when there is one (R1-13); `None` when only `text` applies.
    pub command: Option<String>,
    /// Always: what to do, e.g. "allow connecting to <dir> in the agent's sandbox settings".
    pub text: String,
    pub who: Who,
    /// Only with `Some(command)`: `--fix` runs only these.
    pub safe: bool,
}

/// Who acts on a step or a fix. Shared by `doctor` and `init` (`contract::init::Step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Who {
    Agent,
    Person,
}

/// What one `--fix` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct FixOutcome {
    /// The check id the fix belongs to.
    pub check: String,
    pub command: String,
    pub result: FixResult,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FixResult {
    /// The re-check passed.
    Fixed,
    /// The fix ran, but the re-check still fails.
    StillFailing,
    /// Not run, or refused by the service (e.g. busy); never forced.
    Refused,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn report() -> DoctorReport {
        DoctorReport {
            checks: vec![
                Check {
                    id: "service.reachable".into(),
                    status: CheckStatus::Problem,
                    message: "this process may not reach hostler's socket".into(),
                    fix: Some(Fix {
                        command: None,
                        text: "run hostler outside the agent sandbox, or allow its socket \
                               (hostler guide sandbox)"
                            .into(),
                        who: Who::Agent,
                        safe: false,
                    }),
                    details: json!({ "reason": "sandbox_denied" }),
                },
                Check {
                    id: "service.registered".into(),
                    status: CheckStatus::Ok,
                    message: "registered".into(),
                    fix: None,
                    details: json!({}),
                },
                Check {
                    id: "project.approved".into(),
                    status: CheckStatus::Warn,
                    message: "not approved".into(),
                    fix: Some(Fix {
                        command: Some("hostler project approve".into()),
                        text: "a person approves this project".into(),
                        who: Who::Person,
                        safe: false,
                    }),
                    details: json!({}),
                },
                Check {
                    id: "service.linux_session".into(),
                    status: CheckStatus::Skipped,
                    message: "not Linux".into(),
                    fix: None,
                    details: json!({}),
                },
            ],
            fixed: vec![
                FixOutcome {
                    check: "service.registered".into(),
                    command: "hostler service install".into(),
                    result: FixResult::Fixed,
                    message: "installed".into(),
                },
                FixOutcome {
                    check: "service.contract".into(),
                    command: "hostler service install".into(),
                    result: FixResult::Refused,
                    message: "1 heavy task(s) are running".into(),
                },
                FixOutcome {
                    check: "project.docs".into(),
                    command: "hostler init --write".into(),
                    result: FixResult::StillFailing,
                    message: "the block still differs".into(),
                },
            ],
        }
    }

    #[test]
    fn doctor_report_round_trips() {
        let report = report();
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            serde_json::from_value::<DoctorReport>(value.clone()).unwrap(),
            report
        );
        assert_eq!(
            serde_json::from_str::<DoctorReport>(&serde_json::to_string(&report).unwrap()).unwrap(),
            report
        );
        assert_eq!(value["checks"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn doctor_wire_is_snake_case() {
        let value = serde_json::to_value(report()).unwrap();
        let statuses: Vec<&str> = value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["problem", "ok", "warn", "skipped"]);
        let results: Vec<&str> = value["fixed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["result"].as_str().unwrap())
            .collect();
        assert_eq!(results, ["fixed", "refused", "still_failing"]);
        assert_eq!(value["checks"][2]["fix"]["who"], "person");
    }

    /// R1-13: a fix with nothing to run serialises its `command` as `null`, and a check without
    /// a fix serialises `fix: null`; neither is dropped.
    #[test]
    fn absent_command_and_fix_are_null() {
        let value = serde_json::to_value(report()).unwrap();
        assert_eq!(
            value["checks"][0]["fix"],
            json!({
                "command": null,
                "text": "run hostler outside the agent sandbox, or allow its socket \
                         (hostler guide sandbox)",
                "who": "agent",
                "safe": false,
            })
        );
        assert!(
            value["checks"][1]
                .as_object()
                .unwrap()
                .get("fix")
                .unwrap()
                .is_null()
        );
        assert_eq!(
            value["checks"][1],
            json!({
                "id": "service.registered",
                "status": "ok",
                "message": "registered",
                "fix": null,
                "details": {},
            })
        );
        assert_eq!(
            value["fixed"][0],
            json!({
                "check": "service.registered",
                "command": "hostler service install",
                "result": "fixed",
                "message": "installed",
            })
        );
    }
}
