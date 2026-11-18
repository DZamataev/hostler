//! Contract types of the account methods (plan P3a, Task 1). Account field values travel only in
//! `account.set` (CLI→service) and `account.materialize` (service→CLI), always as `SecretValue`.

use serde::{Deserialize, Serialize};

use super::claims::ClaimSummary;
use super::secrets::{ImportEntry, MaterializedSecret, ProjectRef, RequestState};
use crate::contract::process::ProcessIdentity;
use crate::contract::values::{RawCommand, SecretValue};

/// An account's names, never its values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountInfo {
    pub name: String,
    pub fields: Vec<String>,
    pub holder: Option<ClaimSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountSet {
    pub project: ProjectRef,
    pub name: String,
    pub field: String,
    pub value: SecretValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountRemove {
    pub project: ProjectRef,
    pub name: String,
    /// `None` removes the whole account.
    pub field: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountMaterialize {
    pub run_id: String,
    pub claim_id: String,
    pub requester: ProcessIdentity,
    pub project: ProjectRef,
    pub name: String,
    /// The display already masked with the run's secret values.
    pub command: RawCommand,
    pub workspace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct MaterializedAccount {
    pub name: String,
    pub fields: Vec<MaterializedSecret>,
}

/// `account.set`'s answer: the names, never the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountFieldSet {
    pub name: String,
    pub field: String,
}

/// `account.request` (P5, spec §8.4): an agent asks a person for an account's fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountRequestNew {
    /// A UUID v4 made by the client; a re-sent id answers the recorded request.
    pub id: String,
    pub project: ProjectRef,
    pub name: String,
    pub fields: Vec<String>,
    pub reason: String,
    pub requester: ProcessIdentity,
}

/// An account request as recorded; names only, never a value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountRequestInfo {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub name: String,
    pub fields: Vec<String>,
    pub reason: String,
    pub state: RequestState,
    pub requester_pid: u32,
    pub created_at_ms: i64,
    pub answered_at_ms: Option<i64>,
}

/// `account.answer` (gated, spec §8.5): every field of one request, stored in one save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountAnswer {
    pub request_id: String,
    /// Each entry's `name` is a field name.
    pub fields: Vec<ImportEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AccountAnswerOutcome {
    pub name: String,
    pub stored_fields: Vec<String>,
    pub answered_requests: Vec<String>,
}

/// The fixed refusals of a bad name; never the name itself (it may be a pasted value).
pub const INVALID_ACCOUNT_NAME: &str =
    "invalid account name (NAME must match ^[a-z][a-z0-9._-]{0,63}$; `screen` is reserved)";
pub const INVALID_FIELD_NAME: &str =
    "invalid field name (FIELD must match ^[a-z][a-z0-9_]{0,31}$; `name` is reserved)";

/// `^[a-z][a-z0-9._-]{0,63}$`, except `screen` (L39): its `--confirmation` token would collide
/// with the screen's.
pub fn valid_account_name(s: &str) -> bool {
    let b = s.as_bytes();
    s != "screen"
        && (1..=64).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter().all(|&c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// `^[a-z][a-z0-9_]{0,31}$`, except `name` (L38): `HOSTLER_ACCOUNT_NAME` is the account's own
/// name, so a field called `name` would overwrite it. A KeePass custom field `name` is the
/// person's own data and is never injected.
pub fn valid_field_name(s: &str) -> bool {
    let b = s.as_bytes();
    s != "name"
        && (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        for ok in ["qa-user", "a.b_c-1", "a", &"a".repeat(64)] {
            assert!(valid_account_name(ok), "{ok}");
        }
        for bad in [
            "",
            "QA",
            "-a",
            ".a",
            "1qa",
            "a b",
            "a/b",
            "qa\u{1}",
            // L39: the screen's `--confirmation` token.
            "screen",
            &"a".repeat(65),
        ] {
            assert!(!valid_account_name(bad), "{bad:?}");
        }
        for ok in ["session", "env_2", "a", &"a".repeat(32)] {
            assert!(valid_field_name(ok), "{ok}");
        }
        for bad in ["", "Env", "a-b", "2env", "_env", "a.b", &"a".repeat(33)] {
            assert!(!valid_field_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn name_field_is_reserved() {
        // L38: `HOSTLER_ACCOUNT_NAME` is the account's name, so no field may be called `name`.
        assert!(!valid_field_name("name"));
        assert!(valid_field_name("name_2"));
        assert!(valid_field_name("username"));
    }

    fn project() -> ProjectRef {
        ProjectRef {
            common_dir: "/work/app/.git".into(),
            name: "app".into(),
        }
    }

    #[test]
    fn account_values_are_redacted_in_debug() {
        let set = AccountSet {
            project: project(),
            name: "qa-user".into(),
            field: "session".into(),
            value: SecretValue::new("hunter2-canary".into()),
        };
        let materialized = MaterializedAccount {
            name: "qa-user".into(),
            fields: vec![MaterializedSecret {
                name: "session".into(),
                value: SecretValue::new("hunter2-canary".into()),
            }],
        };
        let request = AccountMaterialize {
            run_id: "run-1".into(),
            claim_id: "c1".into(),
            requester: crate::contract::process::ProcessIdentity {
                pid: 42,
                boot_id: "b1".into(),
                start_time: 420,
            },
            project: project(),
            name: "qa-user".into(),
            command: RawCommand::new("login --token hunter2-canary".into()),
            workspace: "/work/app".into(),
        };
        for printed in [
            format!("{set:?}"),
            format!("{materialized:?}"),
            format!("{request:?}"),
        ] {
            assert!(!printed.contains("hunter2"), "{printed}");
        }
    }

    #[test]
    fn accounts_round_trip() {
        let info = AccountInfo {
            name: "qa-user".into(),
            fields: vec!["env".into(), "session".into()],
            holder: None,
        };
        let remove = AccountRemove {
            project: project(),
            name: "qa-user".into(),
            field: Some("env".into()),
        };
        assert_eq!(
            serde_json::from_value::<AccountInfo>(serde_json::to_value(&info).unwrap()).unwrap(),
            info
        );
        assert_eq!(
            serde_json::from_value::<AccountRemove>(serde_json::to_value(&remove).unwrap())
                .unwrap(),
            remove
        );
    }
}
