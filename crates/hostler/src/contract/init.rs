//! `hostler init`'s report (plan P4 Task 1; bodies in Tasks 3 and 5). A serialisable contract
//! type that no service method carries: it serves `init --json` and P5. It never holds a secret
//! value: env files contribute names only.
//!
//! Every enum that carries data is internally tagged with `kind`, in snake_case. An
//! `AgentDoc` flattens its `AgentDocKind`, so it has one discriminator:
//! `{"path": "CLAUDE.md", "kind": "symlink_to", "target": "AGENTS.md"}`.

use serde::{Deserialize, Serialize};

use crate::contract::devices::Platform;
use crate::contract::doctor::Who;

/// What `hostler init` found, proposes and (with `--write`) did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitReport {
    /// The checkout root init ran for.
    pub root: String,
    pub detected: Detected,
    pub config: ConfigState,
    /// The proposed `hostler.toml`, only when the checkout has none.
    pub proposed_toml: Option<String>,
    pub docs: Vec<DocsChange>,
    pub steps: Vec<Step>,
    /// The paths written; empty without `--write`.
    pub written: Vec<String>,
    pub service: ServiceAction,
}

/// The state of the checkout's `hostler.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigState {
    Absent,
    Valid,
    Invalid { problems: Vec<LineProblem> },
}

/// One problem of a file, by line; the message never quotes a value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineProblem {
    pub line: usize,
    pub message: String,
}

/// What init does (or would do) to one agent-docs file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocsChange {
    /// Relative to the checkout root.
    pub path: String,
    pub action: DocsAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocsAction {
    /// The file is absent and is created holding only the block.
    Create,
    /// The file has no block; the block is appended.
    Insert,
    /// The block differs and is replaced in place.
    Update,
    /// The block is already current.
    Unchanged,
    /// No block: the file links to or imports `path`, which has it.
    CoveredBy { path: String },
}

/// One step for the agent or the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Stable, e.g. `init.device_rules`, `init.secrets.JACKETT_API_KEY`.
    pub id: String,
    pub who: Who,
    pub text: String,
    /// The command as a person or an agent types it (paths quoted).
    pub command: Option<String>,
    /// The same command as an argument vector, so nothing need parse `command`.
    pub argv: Option<Vec<String>>,
}

/// What init did, or would do, about the service (R1-11, R2-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceAction {
    /// Reachable with a matching contract; nothing to do.
    Running,
    /// Preview (or `--write` with a dev build, D-Q2): an absent or outdated registration.
    WouldInstall {
        /// The binary running init is a dev build, so init lists `hostler service install`
        /// instead of installing (D-Q2).
        #[serde(default)]
        dev_build: bool,
    },
    /// Preview: a Current registration whose service is down.
    WouldStart,
    /// Preview: a Current registration and an older, locked running service.
    WouldReplace,
    /// `--write`: the older service was stopped atomically and the registered one started.
    Replaced,
    /// A contract mismatch where nothing was stopped; `direction` is `newer`, `older` or
    /// `unknown` (this client against the service).
    MismatchKept {
        direction: String,
        message: String,
    },
    /// `--write` installed the registration (and the manager started the service).
    Installed,
    /// `--write` started a Current registration through the manager.
    Started,
    StartFailed {
        message: String,
    },
    /// Busy, unlocked or unknown lock state, a newer service, a custom home.
    InstallRefused {
        message: String,
    },
    /// E.g. `sandbox_denied`: nothing was attempted.
    Unreachable {
        reason: String,
    },
}

/// What init detected in the checkout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detected {
    pub toolchains: Vec<Toolchain>,
    pub apps: Vec<DetectedApp>,
    /// Names only, never values.
    pub env_files: Vec<EnvFile>,
    pub agent_docs: Vec<AgentDoc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    pub kind: ToolchainKind,
    /// The file that shows it, relative to the root.
    pub evidence: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainKind {
    Gradle,
    KotlinMultiplatform,
    AndroidApp,
    ReactNative,
    Xcode,
    CocoaPods,
    Cargo,
    Node,
}

/// An app id found in the build files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedApp {
    pub platform: Platform,
    pub id: String,
    /// Relative to the root.
    pub evidence: String,
}

/// An env file and the names it defines; never a value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvFile {
    /// Relative to the root.
    pub path: String,
    pub names: Vec<String>,
}

/// An agent-docs file at the root. On the wire its kind is flattened into it, one
/// discriminator: `{"path": "AGENTS.md", "kind": "file"}`, `{"path": "CLAUDE.md", "kind":
/// "symlink_to", "target": "AGENTS.md"}`, `{"path": "CLAUDE.md", "kind": "imports", "target":
/// "AGENTS.md"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDoc {
    /// Relative to the root.
    pub path: String,
    #[serde(flatten)]
    pub kind: AgentDocKind,
}

/// The `kind` of an `AgentDoc`. The other file is `target` (not `path`, which is the doc's
/// own path once flattened), relative to the root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDocKind {
    File,
    /// A symlink to another agent-docs file inside the root.
    SymlinkTo {
        target: String,
    },
    /// A line `@AGENTS.md` (Claude Code's import).
    Imports {
        target: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn every_service_action() -> Vec<ServiceAction> {
        vec![
            ServiceAction::Running,
            ServiceAction::WouldInstall { dev_build: true },
            ServiceAction::WouldStart,
            ServiceAction::WouldReplace,
            ServiceAction::Replaced,
            ServiceAction::MismatchKept {
                direction: "older".into(),
                message: "this hostler is older than the running service".into(),
            },
            ServiceAction::Installed,
            ServiceAction::Started,
            ServiceAction::StartFailed {
                message: "no answer".into(),
            },
            ServiceAction::InstallRefused {
                message: "custom home".into(),
            },
            ServiceAction::Unreachable {
                reason: "sandbox_denied".into(),
            },
        ]
    }

    fn report(service: ServiceAction, config: ConfigState) -> InitReport {
        InitReport {
            root: "/work/muse".into(),
            detected: Detected {
                toolchains: vec![
                    Toolchain {
                        kind: ToolchainKind::Gradle,
                        evidence: "settings.gradle.kts".into(),
                    },
                    Toolchain {
                        kind: ToolchainKind::KotlinMultiplatform,
                        evidence: "build.gradle.kts".into(),
                    },
                ],
                apps: vec![DetectedApp {
                    platform: Platform::Android,
                    id: "com.example.muse".into(),
                    evidence: "androidApp/build.gradle.kts".into(),
                }],
                env_files: vec![EnvFile {
                    path: ".env.local".into(),
                    names: vec!["API_KEY".into(), "TOKEN".into()],
                }],
                agent_docs: vec![
                    AgentDoc {
                        path: "AGENTS.md".into(),
                        kind: AgentDocKind::File,
                    },
                    AgentDoc {
                        path: "CLAUDE.md".into(),
                        kind: AgentDocKind::SymlinkTo {
                            target: "AGENTS.md".into(),
                        },
                    },
                ],
            },
            config,
            proposed_toml: Some("[apps]\nandroid = [\"com.example.muse\"]\n".into()),
            docs: vec![
                DocsChange {
                    path: "AGENTS.md".into(),
                    action: DocsAction::Insert,
                },
                DocsChange {
                    path: "CLAUDE.md".into(),
                    action: DocsAction::CoveredBy {
                        path: "AGENTS.md".into(),
                    },
                },
            ],
            steps: vec![Step {
                id: "init.service".into(),
                who: Who::Agent,
                text: "register this hostler as the machine's service".into(),
                command: Some("'/a b/hostler' service install".into()),
                argv: Some(
                    ["/a b/hostler", "service", "install"]
                        .map(String::from)
                        .to_vec(),
                ),
            }],
            written: vec![],
            service,
        }
    }

    #[test]
    fn init_report_round_trips_with_every_service_action() {
        for service in every_service_action() {
            for config in [
                ConfigState::Absent,
                ConfigState::Valid,
                ConfigState::Invalid {
                    problems: vec![LineProblem {
                        line: 3,
                        message: "unknown table [x]".into(),
                    }],
                },
            ] {
                let report = report(service.clone(), config);
                let text = serde_json::to_string(&report).unwrap();
                assert_eq!(
                    serde_json::from_str::<InitReport>(&text).unwrap(),
                    report,
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn service_actions_are_snake_case_and_tagged() {
        let kinds: Vec<String> = every_service_action()
            .iter()
            .map(|a| {
                serde_json::to_value(a).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "running",
                "would_install",
                "would_start",
                "would_replace",
                "replaced",
                "mismatch_kept",
                "installed",
                "started",
                "start_failed",
                "install_refused",
                "unreachable",
            ]
        );
        assert_eq!(
            serde_json::to_value(ServiceAction::MismatchKept {
                direction: "newer".into(),
                message: "m".into(),
            })
            .unwrap(),
            json!({"kind": "mismatch_kept", "direction": "newer", "message": "m"})
        );
        assert_eq!(
            serde_json::to_value(ServiceAction::Unreachable {
                reason: "sandbox_denied".into()
            })
            .unwrap(),
            json!({"kind": "unreachable", "reason": "sandbox_denied"})
        );
        // `dev_build` defaults to false when absent.
        assert_eq!(
            serde_json::from_value::<ServiceAction>(json!({"kind": "would_install"})).unwrap(),
            ServiceAction::WouldInstall { dev_build: false }
        );
    }

    #[test]
    fn init_wire_shapes_are_snake_case() {
        let value = serde_json::to_value(report(
            ServiceAction::Running,
            ConfigState::Invalid {
                problems: vec![LineProblem {
                    line: 7,
                    message: "apps.android[2]: not an application id".into(),
                }],
            },
        ))
        .unwrap();
        assert_eq!(
            value["config"],
            json!({"kind": "invalid", "problems": [
                {"line": 7, "message": "apps.android[2]: not an application id"}
            ]})
        );
        assert_eq!(
            serde_json::to_value(ConfigState::Absent).unwrap(),
            json!({"kind": "absent"})
        );
        assert_eq!(
            value["detected"]["toolchains"][1],
            json!({"kind": "kotlin_multiplatform", "evidence": "build.gradle.kts"})
        );
        assert_eq!(
            value["detected"]["apps"][0],
            json!({"platform": "android", "id": "com.example.muse",
                   "evidence": "androidApp/build.gradle.kts"})
        );
        assert_eq!(
            value["detected"]["agent_docs"][1],
            json!({"path": "CLAUDE.md", "kind": "symlink_to", "target": "AGENTS.md"})
        );
        assert_eq!(
            value["docs"][0],
            json!({"path": "AGENTS.md", "action": {"kind": "insert"}})
        );
        assert_eq!(
            value["docs"][1]["action"],
            json!({"kind": "covered_by", "path": "AGENTS.md"})
        );
        assert_eq!(value["steps"][0]["who"], "agent");
        assert_eq!(value["steps"][0]["argv"][0], "/a b/hostler");
        assert_eq!(value["service"], json!({"kind": "running"}));
        let kinds: Vec<Value> = [
            ToolchainKind::Gradle,
            ToolchainKind::KotlinMultiplatform,
            ToolchainKind::AndroidApp,
            ToolchainKind::ReactNative,
            ToolchainKind::Xcode,
            ToolchainKind::CocoaPods,
            ToolchainKind::Cargo,
            ToolchainKind::Node,
        ]
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
        assert_eq!(
            kinds,
            [
                "gradle",
                "kotlin_multiplatform",
                "android_app",
                "react_native",
                "xcode",
                "cocoa_pods",
                "cargo",
                "node",
            ]
        );
        for action in [
            DocsAction::Create,
            DocsAction::Update,
            DocsAction::Unchanged,
        ] {
            let value = serde_json::to_value(&action).unwrap();
            assert_eq!(serde_json::from_value::<DocsAction>(value).unwrap(), action);
        }
        // One discriminator per agent doc (fix round 1, R1-1): every kind round-trips.
        for (doc, wire) in [
            (
                AgentDoc {
                    path: "AGENTS.md".into(),
                    kind: AgentDocKind::File,
                },
                json!({"path": "AGENTS.md", "kind": "file"}),
            ),
            (
                AgentDoc {
                    path: "CLAUDE.md".into(),
                    kind: AgentDocKind::SymlinkTo {
                        target: "AGENTS.md".into(),
                    },
                },
                json!({"path": "CLAUDE.md", "kind": "symlink_to", "target": "AGENTS.md"}),
            ),
            (
                AgentDoc {
                    path: "CLAUDE.md".into(),
                    kind: AgentDocKind::Imports {
                        target: "AGENTS.md".into(),
                    },
                },
                json!({"path": "CLAUDE.md", "kind": "imports", "target": "AGENTS.md"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&doc).unwrap(), wire);
            assert_eq!(serde_json::from_value::<AgentDoc>(wire).unwrap(), doc);
        }
        // No nested object anywhere in the detected agent docs.
        for doc in value["detected"]["agent_docs"].as_array().unwrap() {
            assert!(doc["kind"].is_string(), "{doc}");
        }
    }
}
