//! Contract types of the secrets methods (plan P2, Task 1). No type here holds a value except
//! through `SecretValue` / `RawCommand`.

use serde::{Deserialize, Serialize};

use crate::contract::process::ProcessIdentity;
use crate::contract::values::{RawCommand, SecretValue};

/// A project as the CLI resolved it from the cwd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectRef {
    pub common_dir: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub common_dir: String,
    pub approved: bool,
    pub group_uuid: Option<String>,
    pub created_at_ms: i64,
    pub approved_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LockState {
    Missing,
    Locked,
    Unlocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretsStatus {
    pub state: LockState,
    pub database: String,
    pub key_file: Option<String>,
    pub pending_requests: u32,
    /// `hostler.kdbx.prev-*` files kept after a conflict.
    pub preserved_files: Vec<String>,
    /// P5 (spec §6.2): the last event `seq` assigned when this status was read; in a
    /// `secrets.lock_changed` event, that event's own `seq`.
    pub as_of_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Unlock {
    pub password: SecretValue,
    pub key_file: Option<String>,
    pub forget_key_file: bool,
    pub create: bool,
}

/// `secret.remove`'s and `account.remove`'s answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Removed {
    pub removed: bool,
}

/// `secret.share`'s answer: the name and the two project ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ShareOutcome {
    pub name: String,
    pub from: String,
    pub to: String,
}

/// `secret.unshare`'s answer (always `true`: a missing share is refused).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Unshared {
    pub unshared: bool,
}

/// `secrets.prove` (P5, spec §8.5): checks a master password against the database on disk,
/// changing nothing; success grants this connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Prove {
    pub password: SecretValue,
    /// `None`: the remembered key file, if any.
    pub key_file: Option<String>,
}

/// A live proof on one connection (spec §8.5): gated methods are admitted until it expires, the
/// database is locked or unlocked, or the connection ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProofGrant {
    pub expires_at_ms: i64,
}

/// `secrets.unlock`'s answer (contract 5): a successful unlock grants its connection too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UnlockOutcome {
    pub status: SecretsStatus,
    pub grant: ProofGrant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeclaredSecret {
    pub name: String,
    pub purpose: String,
    pub env: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretSource {
    Own,
    SharedFrom { project_id: String, project: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SecretState {
    Set,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretInfo {
    pub name: String,
    pub purpose: Option<String>,
    pub declared: bool,
    pub state: SecretState,
    pub source: Option<SecretSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretList {
    pub project: ProjectInfo,
    pub secrets: Vec<SecretInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretSet {
    pub project: ProjectRef,
    pub name: String,
    pub value: SecretValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SetOutcome {
    pub name: String,
    pub answered_requests: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ImportEntry {
    pub name: String,
    pub value: SecretValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretImport {
    pub project: ProjectRef,
    pub entries: Vec<ImportEntry>,
    pub overwrite: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ImportOutcome {
    pub stored: Vec<String>,
    pub answered_requests: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Stored,
    Declined,
    Withdrawn,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretRequest {
    /// A UUID v4 made by the client.
    pub id: String,
    pub project: ProjectRef,
    pub name: String,
    pub reason: String,
    pub requester: ProcessIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SecretRequestInfo {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub name: String,
    pub reason: String,
    pub state: RequestState,
    pub requester_pid: u32,
    pub created_at_ms: i64,
    pub answered_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Decline {
    pub id: Option<String>,
    pub project: Option<ProjectRef>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Share {
    pub from: ProjectRef,
    pub name: String,
    pub to_project_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Materialize {
    pub run_id: String,
    pub project: ProjectRef,
    pub names: Vec<String>,
    /// The raw display up to 16 KiB, else `<command of N bytes>` (L13); the service masks it
    /// before recording it (L6).
    pub command: RawCommand,
    pub workspace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct MaterializedSecret {
    pub name: String,
    pub value: SecretValue,
}

/// The secrets errors with their exact texts (the guide quotes them).
pub mod errors {
    use serde_json::json;

    use crate::contract::{ErrorCode, HostlerError};

    fn error(code: ErrorCode, message: String, recovery: Option<String>) -> HostlerError {
        HostlerError {
            code,
            message,
            details: json!({}),
            recovery,
        }
    }

    pub fn bad_request(message: String) -> HostlerError {
        error(ErrorCode::BadRequest, message, None)
    }

    pub fn secrets_locked(missing: bool) -> HostlerError {
        let what = if missing {
            "there is no hostler secrets database yet"
        } else {
            "the hostler secrets database is locked"
        };
        let mut e = error(
            ErrorCode::SecretsLocked,
            format!(
                "{what}; a person runs `hostler unlock` in a terminal (agents ask the operator)"
            ),
            Some("hostler unlock".into()),
        );
        e.details = json!({ "state": if missing { "missing" } else { "locked" } });
        e
    }

    pub fn not_interactive(cmd: &str) -> HostlerError {
        error(
            ErrorCode::NotInteractive,
            format!(
                "`hostler {cmd}` needs a person at a terminal. A person runs this; an agent asks \
                 the operator."
            ),
            Some(format!("hostler {cmd}")),
        )
    }

    pub fn project_not_approved(name: &str, common_dir: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::ProjectNotApproved,
            format!(
                "project {name} ({common_dir}) is not approved for secrets; a person approves it \
                 once"
            ),
            Some("hostler project approve".into()),
        );
        e.details = json!({ "project": name, "common_dir": common_dir });
        e
    }

    pub fn secret_missing(name: &str, project: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::SecretMissing,
            format!("secret {name} is not set for project {project}"),
            Some(format!(
                "hostler secret request {name} --reason \"<why you need it>\""
            )),
        );
        e.details = json!({ "name": name, "project": project });
        e
    }

    pub fn secret_not_in_project(name: &str, owner: &str, project: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::SecretNotInProject,
            format!(
                "secret {name} belongs to project {owner}, not {project}; a person can share it \
                 from a checkout of {owner}"
            ),
            Some(format!("hostler secret share {name} --to {project}")),
        );
        e.details = json!({ "name": name, "owner": owner, "project": project });
        e
    }

    pub fn secret_too_short() -> HostlerError {
        error(
            ErrorCode::SecretTooShort,
            "secret values must be at least 4 bytes (shorter values would garble masked output)"
                .into(),
            None,
        )
    }

    pub fn secret_exists(names: &[String]) -> HostlerError {
        let mut e = error(
            ErrorCode::SecretExists,
            format!(
                "already set: {}; add --overwrite to replace",
                names.join(", ")
            ),
            None,
        );
        e.details = json!({ "names": names });
        e
    }

    pub fn secrets_conflict(reason: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::SecretsConflict,
            "hostler.kdbx was changed outside hostler and cannot be read with the key hostler \
             holds (changed master password, key file, or a damaged file); nothing was written"
                .into(),
            Some("hostler unlock".into()),
        );
        e.details = json!({ "reason": reason });
        e
    }

    pub fn unlock_failed() -> HostlerError {
        error(
            ErrorCode::UnlockFailed,
            "wrong master password or key file".into(),
            Some("hostler unlock".into()),
        )
    }

    /// P5 (spec §8.5): a gated method without a live proof on its connection.
    pub fn proof_required() -> HostlerError {
        error(
            ErrorCode::ProofRequired,
            "this needs the master password: a person proves it on this connection first".into(),
            Some(
                "type the master password in the hostler app, or rerun the command at a terminal"
                    .into(),
            ),
        )
    }

    /// P5 (spec §8.5): the limit on password checks, after wrong passwords; `retry_after_ms`
    /// is at least 1. The text names the next try and says that another process of the user may
    /// be guessing (the accepted lockout risk, P5-L4).
    pub fn proof_limited(retry_after_ms: u64) -> HostlerError {
        limited("too many wrong master passwords", retry_after_ms)
    }

    /// P5 (spec §8.5): another master-password check is running in the service (at most one
    /// at a time, service-wide); same code and details as `proof_limited`.
    pub fn proof_busy(retry_after_ms: u64) -> HostlerError {
        limited("another master-password check is running", retry_after_ms)
    }

    fn limited(what: &str, retry_after_ms: u64) -> HostlerError {
        let retry_after_ms = retry_after_ms.max(1);
        let secs = retry_after_ms.div_ceil(1000);
        let mut e = error(
            ErrorCode::ProofLimited,
            format!(
                "{what}; next try in {secs} s. Another process of your user may be guessing \
                 (the service log names each wrong password's process)"
            ),
            Some(format!("wait {secs} s, then try again")),
        );
        e.details = json!({ "retry_after_ms": retry_after_ms });
        e
    }

    pub fn service_stopping() -> HostlerError {
        let mut e = error(
            ErrorCode::ServiceUnavailable,
            "the hostler service is stopping".into(),
            Some("hostler service start".into()),
        );
        e.details = json!({ "reason": "stopping" });
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ErrorCode;
    use crate::contract::wire::Request;

    #[test]
    fn materialize_debug_hides_the_command() {
        let m = Materialize {
            run_id: "run-1".into(),
            project: ProjectRef {
                common_dir: "/work/app/.git".into(),
                name: "app".into(),
            },
            names: vec!["API_KEY".into()],
            command: RawCommand::new("deploy --token hunter2-canary".into()),
            workspace: "/work/app".into(),
        };
        let request = Request::SecretMaterialize(m.clone());
        for printed in [format!("{m:?}"), format!("{request:?}")] {
            for part in ["hunter2", "canary", "deploy", "--token"] {
                assert!(!printed.contains(part), "{part} in {printed}");
            }
            assert!(printed.contains("<command of 29 bytes>"), "{printed}");
        }
    }

    #[test]
    fn materialized_secret_debug_is_redacted() {
        let m = MaterializedSecret {
            name: "API_KEY".into(),
            value: SecretValue::new("hunter2-canary".into()),
        };
        assert!(!format!("{m:?}").contains("hunter2"));
    }

    #[test]
    fn enums_are_snake_case_and_sources_are_tagged() {
        assert_eq!(
            serde_json::to_value(RequestState::TimedOut).unwrap(),
            "timed_out"
        );
        assert_eq!(serde_json::to_value(LockState::Missing).unwrap(), "missing");
        assert_eq!(serde_json::to_value(SecretState::Set).unwrap(), "set");
        assert_eq!(
            serde_json::to_value(SecretSource::Own).unwrap(),
            serde_json::json!({"kind": "own"})
        );
        assert_eq!(
            serde_json::to_value(SecretSource::SharedFrom {
                project_id: "p1".into(),
                project: "lib".into()
            })
            .unwrap(),
            serde_json::json!({"kind": "shared_from", "project_id": "p1", "project": "lib"})
        );
    }

    #[test]
    fn error_texts_are_exact() {
        let e = errors::secrets_locked(false);
        assert_eq!(e.code, ErrorCode::SecretsLocked);
        assert_eq!(
            e.message,
            "the hostler secrets database is locked; a person runs `hostler unlock` in a \
             terminal (agents ask the operator)"
        );
        assert_eq!(e.recovery.as_deref(), Some("hostler unlock"));
        assert_eq!(
            errors::secrets_locked(true).message,
            "there is no hostler secrets database yet; a person runs `hostler unlock` in a \
             terminal (agents ask the operator)"
        );

        let e = errors::not_interactive("unlock");
        assert_eq!(e.code, ErrorCode::NotInteractive);
        assert_eq!(
            e.message,
            "`hostler unlock` needs a person at a terminal. A person runs this; an agent asks \
             the operator."
        );
        assert_eq!(e.recovery.as_deref(), Some("hostler unlock"));

        let e = errors::project_not_approved("app", "/work/app/.git");
        assert_eq!(e.code, ErrorCode::ProjectNotApproved);
        assert_eq!(
            e.message,
            "project app (/work/app/.git) is not approved for secrets; a person approves it once"
        );
        assert_eq!(e.recovery.as_deref(), Some("hostler project approve"));

        let e = errors::secret_missing("API_KEY", "app");
        assert_eq!(e.code, ErrorCode::SecretMissing);
        assert_eq!(e.message, "secret API_KEY is not set for project app");
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler secret request API_KEY --reason \"<why you need it>\"")
        );

        let e = errors::secret_not_in_project("API_KEY", "lib", "app");
        assert_eq!(e.code, ErrorCode::SecretNotInProject);
        assert_eq!(
            e.message,
            "secret API_KEY belongs to project lib, not app; a person can share it from a \
             checkout of lib"
        );
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler secret share API_KEY --to app")
        );

        let e = errors::secret_too_short();
        assert_eq!(e.code, ErrorCode::SecretTooShort);
        assert_eq!(
            e.message,
            "secret values must be at least 4 bytes (shorter values would garble masked output)"
        );

        let e = errors::secret_exists(&["A".into(), "B".into()]);
        assert_eq!(e.code, ErrorCode::SecretExists);
        assert_eq!(e.message, "already set: A, B; add --overwrite to replace");

        let e = errors::secrets_conflict("unreadable");
        assert_eq!(e.code, ErrorCode::SecretsConflict);
        assert_eq!(
            e.message,
            "hostler.kdbx was changed outside hostler and cannot be read with the key hostler \
             holds (changed master password, key file, or a damaged file); nothing was written"
        );
        assert_eq!(e.details["reason"], "unreadable");
        assert_eq!(e.recovery.as_deref(), Some("hostler unlock"));

        let e = errors::unlock_failed();
        assert_eq!(e.code, ErrorCode::UnlockFailed);
        assert_eq!(e.message, "wrong master password or key file");

        let e = errors::service_stopping();
        assert_eq!(e.code, ErrorCode::ServiceUnavailable);
        assert_eq!(e.message, "the hostler service is stopping");
        assert_eq!(e.details["reason"], "stopping");
        assert_eq!(e.recovery.as_deref(), Some("hostler service start"));
    }
}
