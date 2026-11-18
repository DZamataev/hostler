//! Projects as the app sees them (P5, spec §6.3): every project with its workspaces and the
//! declarations its checkouts' `hostler.toml` last sent, so the app lists a project's secrets,
//! accounts and app ids outside a checkout. A declaration holds names and purposes only.

use serde::{Deserialize, Serialize};

use super::claims::WorkspaceInfo;
use super::secrets::{DeclaredSecret, ProjectInfo};

/// At most this many entries of each kind in one declaration; `hostler.toml` allows fewer.
pub const MAX_DECLARED: usize = 256;
/// At most this many app ids per platform (`hostler.toml`'s own limit).
pub const MAX_DECLARED_APPS: usize = 32;

/// What one project's `hostler.toml` declares (`workspace.resolve`'s `declared`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectDecl {
    pub secrets: Vec<DeclaredSecret>,
    pub accounts: Vec<DeclaredAccount>,
    pub apps: DeclaredApps,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeclaredAccount {
    pub name: String,
    pub purpose: String,
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeclaredApps {
    pub android: Vec<String>,
    pub ios: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectView {
    pub info: ProjectInfo,
    pub workspaces: Vec<WorkspaceInfo>,
    /// `None` until a checkout with a valid `hostler.toml` resolved.
    pub declared: Option<ProjectDecl>,
    pub declared_at_ms: Option<i64>,
}

/// `projects.snapshot`'s answer and `projects.changed`'s data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectsSnapshot {
    pub projects: Vec<ProjectView>,
    pub at_ms: i64,
    /// The last event `seq` assigned when it was read; in a `projects.changed` event, that
    /// event's own `seq` (spec §6.2).
    pub as_of_seq: u64,
}

impl ProjectDecl {
    /// The service's check of a client-sent declaration: valid names, one-line purposes of at
    /// most 200 characters (`hostler.toml`'s rules), and bounded counts. Never echoes a value:
    /// a name or purpose may be a pasted secret.
    pub fn check(&self) -> Result<(), &'static str> {
        use super::accounts::{valid_account_name, valid_field_name};
        use crate::secrets::{valid_env_name, valid_secret_name};

        let purpose_ok =
            |p: &str| !p.is_empty() && p.chars().count() <= 200 && !p.chars().any(char::is_control);
        let app_ok = |id: &str| {
            (1..=255).contains(&id.len())
                && !id.chars().any(|c| c.is_control() || c.is_whitespace())
        };
        if self.secrets.len() > MAX_DECLARED || self.accounts.len() > MAX_DECLARED {
            return Err("a declaration holds at most 256 secrets and 256 accounts");
        }
        for s in &self.secrets {
            if !valid_secret_name(&s.name)
                || !purpose_ok(&s.purpose)
                || s.env.as_deref().is_some_and(|e| !valid_env_name(e))
            {
                return Err("a declared secret has an invalid name, purpose or env");
            }
        }
        for a in &self.accounts {
            if !valid_account_name(&a.name)
                || !purpose_ok(&a.purpose)
                || a.fields.len() > crate::secrets::engine::MAX_ACCOUNT_FIELDS
                || !a.fields.iter().all(|f| valid_field_name(f))
            {
                return Err("a declared account has an invalid name, purpose or field");
            }
        }
        if self.apps.android.len() > MAX_DECLARED_APPS
            || self.apps.ios.len() > MAX_DECLARED_APPS
            || !self
                .apps
                .android
                .iter()
                .chain(&self.apps.ios)
                .all(|id| app_ok(id))
        {
            return Err("declared app ids: at most 32 per platform, one word each");
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn decl() -> ProjectDecl {
        ProjectDecl {
            secrets: vec![DeclaredSecret {
                name: "API_KEY".into(),
                purpose: "the indexer".into(),
                env: Some("INDEXER_KEY".into()),
            }],
            accounts: vec![DeclaredAccount {
                name: "qa-user".into(),
                purpose: "the main e2e test user".into(),
                fields: vec!["session".into(), "env".into()],
            }],
            apps: DeclaredApps {
                android: vec!["com.example.app".into()],
                ios: vec!["com.example.App".into()],
            },
        }
    }

    pub fn snapshot() -> ProjectsSnapshot {
        use crate::contract::claims::WorkspaceOutcome;
        ProjectsSnapshot {
            projects: vec![ProjectView {
                info: ProjectInfo {
                    id: "p1".into(),
                    name: "app".into(),
                    common_dir: "/work/app/.git".into(),
                    approved: true,
                    group_uuid: None,
                    created_at_ms: 1_000,
                    approved_at_ms: Some(2_000),
                },
                workspaces: vec![WorkspaceInfo {
                    id: "ws-1".into(),
                    project_id: "p1".into(),
                    path: "/work/app".into(),
                    outcome: WorkspaceOutcome::Known,
                    project_repointed: false,
                    note: None,
                    last_seen_ms: 3_000,
                }],
                declared: Some(decl()),
                declared_at_ms: Some(3_000),
            }],
            at_ms: 4_000,
            as_of_seq: 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn decl_wire_shape() {
        assert_eq!(
            serde_json::to_value(fixtures::decl()).unwrap(),
            json!({
                "secrets": [{"name": "API_KEY", "purpose": "the indexer", "env": "INDEXER_KEY"}],
                "accounts": [{"name": "qa-user", "purpose": "the main e2e test user",
                              "fields": ["session", "env"]}],
                "apps": {"android": ["com.example.app"], "ios": ["com.example.App"]}
            })
        );
        let snapshot = fixtures::snapshot();
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["as_of_seq"], 7);
        assert_eq!(
            serde_json::from_value::<ProjectsSnapshot>(value).unwrap(),
            snapshot
        );
    }

    #[test]
    fn check_refuses_bad_declarations() {
        assert_eq!(fixtures::decl().check(), Ok(()));
        assert_eq!(ProjectDecl::default().check(), Ok(()));
        let mut bad = fixtures::decl();
        bad.secrets[0].name = "sk-live-value".into();
        assert!(bad.check().is_err());
        let mut bad = fixtures::decl();
        bad.secrets[0].purpose = "two\nlines".into();
        assert!(bad.check().is_err());
        let mut bad = fixtures::decl();
        bad.accounts[0].fields.push("name".into());
        assert!(bad.check().is_err());
        let mut bad = fixtures::decl();
        bad.apps.ios = vec!["x".into(); MAX_DECLARED_APPS + 1];
        assert!(bad.check().is_err());
        let mut bad = fixtures::decl();
        bad.apps.android = vec!["has space".into()];
        assert!(bad.check().is_err());
    }
}
