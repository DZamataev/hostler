//! Contract types of workspaces, claims, takeovers and the status overview (plan P3a, Task 1).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::accounts::valid_account_name;
use super::heavy::QueueSnapshot;
use super::secrets::{ProjectRef, SecretsStatus};
use crate::procs::{ProcessIdentity, RunGroup};

/// Every `--help`, guide paragraph and refusal that mentions `--force` carries it (D9).
pub const FORCE_SENTENCE: &str =
    "An agent uses --force only with the operator's explicit permission.";

/// `screen`, `account:<project_id>/<name>`, or `device:<platform>:<hardware id>` (P3b).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Screen,
    Account,
    /// P3b.
    Device,
}

/// A project id as it appears in an account resource id (a UUID in practice).
fn valid_project_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl ResourceId {
    pub fn screen() -> Self {
        ResourceId("screen".into())
    }

    pub fn account(project_id: &str, name: &str) -> Self {
        ResourceId(format!("account:{project_id}/{name}"))
    }

    fn account_parts(&self) -> Option<(&str, &str)> {
        let (project, name) = self.0.strip_prefix("account:")?.split_once('/')?;
        (valid_project_id(project) && valid_account_name(name)).then_some((project, name))
    }

    /// `(platform, hardware id)`; the hardware id may hold colons (`adb` over the network).
    fn device_parts(&self) -> Option<(&str, &str)> {
        let (platform, hardware) = self.0.strip_prefix("device:")?.split_once(':')?;
        let platform_ok = !platform.is_empty()
            && platform
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        let hardware_ok = !hardware.is_empty()
            && !hardware
                .chars()
                .any(|c| c.is_control() || c.is_whitespace());
        (platform_ok && hardware_ok).then_some((platform, hardware))
    }

    /// `None` for an id of no known form.
    pub fn kind(&self) -> Option<ResourceKind> {
        if self.0 == "screen" {
            Some(ResourceKind::Screen)
        } else if self.account_parts().is_some() {
            Some(ResourceKind::Account)
        } else if self.device_parts().is_some() {
            Some(ResourceKind::Device)
        } else {
            None
        }
    }

    /// What `--confirmation` must equal: `screen` or the account name. A device's alias comes
    /// from its catalog (P3b); until then a device's token is its id.
    pub fn token(&self) -> String {
        match self.account_parts() {
            Some((_, name)) => name.to_owned(),
            None => self.0.clone(),
        }
    }

    pub fn account_project(&self) -> Option<&str> {
        self.account_parts().map(|(project, _)| project)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceInfo {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub display: String,
    pub attributes: BTreeMap<String, String>,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Selector {
    Exact {
        resource: ResourceId,
    },
    /// Any resource of `kind` whose attributes include these (exact string equality per key).
    Any {
        kind: ResourceKind,
        attributes: BTreeMap<String, String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Lease,
    Hold,
    Reservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HoldBinding {
    Ttl {
        ttl_ms: u64,
        expires_at_ms: i64,
    },
    /// Lives while this exact process lives; no TTL.
    Process {
        holder: ProcessIdentity,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Purpose {
    /// A command-scoped lease; `command` is already masked.
    Lease { command: String },
    Hold {
        ttl_ms: Option<u64>,
        bound_to: Option<ProcessIdentity>,
        distinct: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub id: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub resource: ResourceInfo,
    pub kind: ClaimKind,
    /// The claim this one joined.
    pub parent: Option<String>,
    pub workspace: WorkspaceRef,
    pub holder: Option<ProcessIdentity>,
    pub child: Option<ProcessIdentity>,
    pub group: Option<RunGroup>,
    pub binding: Option<HoldBinding>,
    pub command: Option<String>,
    pub granted_at_ms: i64,
}

/// A claim outside hostler's own (P3b: agent-device sessions).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignClaim {
    pub resource: ResourceId,
    pub source: String,
    pub description: String,
    pub workspace_path: Option<String>,
    pub since_ms: Option<i64>,
    /// `Some`: stoppable; handed to `StopTarget::Foreign`.
    pub stop: Option<serde_json::Value>,
    /// The registered workspace that owns `workspace_path` (P3b, L50). Filled by the engine
    /// when it collects its sources, never by a source.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// How fresh a source's foreign claims are (P3b, L52, L54). A read counter, never a clock
/// (P3-3): `Since(seq)` = the last good read started after `seq` claim ends had been counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// The source does not track claim ends: nothing waits for it.
    NotTracked,
    /// No good read yet since the service started.
    NotYet,
    Since(u64),
}

/// One collection of a source: its claims and their freshness, from the same read (P3-2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignView {
    pub claims: Vec<ForeignClaim>,
    pub freshness: Freshness,
    /// Why the source cannot be read now, from the first failed read (L54): the `Checking` text
    /// names the fix and `forget-sessions` with it.
    #[serde(default)]
    pub unreadable: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    /// A UUID v4 made by the client.
    pub id: String,
    pub selector: Selector,
    pub workspace_id: String,
    pub requester: ProcessIdentity,
    pub purpose: Purpose,
    pub reentrant_tokens: Vec<String>,
    pub no_wait: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "blocker", rename_all = "snake_case")]
pub enum Blocker {
    Claim {
        summary: ClaimSummary,
    },
    Foreign {
        claim: ForeignClaim,
    },
    Seizing {
        by_workspace: String,
    },
    /// L52, L54: the device waits for a fresh agent-device sessions read; `text` names the fix
    /// when the status cannot be read.
    Checking {
        /// The resource's display.
        resource: String,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSummary {
    pub id: String,
    /// The resource's display.
    pub resource: String,
    pub kind: ClaimKind,
    pub workspace_path: String,
    pub pid: Option<u32>,
    pub command: Option<String>,
    pub since_ms: i64,
    pub expires_at_ms: Option<i64>,
    /// Claims linked to this one.
    pub linked: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ClaimOutcome {
    Granted {
        claim: Claim,
    },
    /// The new linked claim.
    Joined {
        claim: Claim,
    },
    Reentrant {
        claim_id: String,
    },
    Queued {
        request_id: String,
        position: u32,
        of: u32,
        blockers: Vec<Blocker>,
    },
    Busy {
        position: u32,
        of: u32,
        blockers: Vec<Blocker>,
    },
    Finished {
        item: ClaimHistoryItem,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimStarted {
    pub claim_id: String,
    pub child: ProcessIdentity,
    pub group: RunGroup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Released,
    Expired,
    Reaped,
    Stranded,
    Seized,
    Withdrawn,
    TimedOut,
    Deadlock,
    /// Refused on a tick, its text in the history `detail` (CR-A-4: a queued `--distinct` hold
    /// whose workspace got the resource meanwhile). It answers as `bad_request(detail)`.
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRelease {
    pub claim_id: String,
    pub workspace_id: String,
    /// A lease is ended only by its holder (the run's wrapper); a hold or a reservation by any
    /// process of its workspace (T5-5).
    pub requester: ProcessIdentity,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseOutcome {
    /// The joined hold, if it still lives.
    pub parent: Option<Claim>,
    /// The claim had already ended by a takeover: the taker's workspace path (the run's exit
    /// summary, Task 8). `None` for every other release.
    #[serde(default)]
    pub seized_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimHistoryItem {
    pub id: String,
    pub resource: String,
    pub kind: ClaimKind,
    pub workspace_path: String,
    pub command: Option<String>,
    pub granted_at_ms: Option<i64>,
    pub ended_at_ms: i64,
    pub reason: EndReason,
    pub exit_code: Option<i32>,
    /// A deadlock: the cycle, as text.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingRequest {
    pub request_id: String,
    pub ticket: u64,
    pub workspace_path: String,
    pub kind: ClaimKind,
    pub since_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceState {
    pub resource: ResourceInfo,
    pub claims: Vec<Claim>,
    pub foreign: Vec<ForeignClaim>,
    pub waiting: Vec<WaitingRequest>,
    pub seizing: bool,
    /// While `seizing`: the taker's workspace path (`status` names it, `release --force`
    /// takeovers included).
    #[serde(default)]
    pub seizing_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimsSnapshot {
    pub resources: Vec<ResourceState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeizeBegin {
    pub resource: ResourceId,
    pub confirmation: String,
    /// The taker's own request, possibly already queued.
    pub request: ClaimRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopTarget {
    RunGroup {
        claim_id: String,
        holder: ProcessIdentity,
        child: Option<ProcessIdentity>,
        group: Option<RunGroup>,
    },
    Process {
        claim_id: String,
        process: ProcessIdentity,
    },
    Foreign {
        source: String,
        resource: ResourceId,
        data: serde_json::Value,
    },
}

/// `target` indexes `SeizePlan.stops`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopEvidence {
    Gone {
        target: usize,
    },
    /// A Windows job the taker saw end in its own logon session.
    JobGone {
        target: usize,
        session: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeizePlan {
    pub plan_id: String,
    pub claims: Vec<Claim>,
    pub foreign: Vec<ForeignClaim>,
    pub stops: Vec<StopTarget>,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeizeCommit {
    pub plan_id: String,
    pub requester: ProcessIdentity,
    pub evidence: Vec<StopEvidence>,
}

/// `claims.seize_progress` (L29): the taker extends its plan before each target it stops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeizeProgress {
    pub plan_id: String,
    pub requester: ProcessIdentity,
}

/// `claims.seize_commit`'s answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeizeOutcome {
    pub outcome: ClaimOutcome,
    /// A target passed only on the taker's `JobGone` evidence (L30): the seizure is recorded
    /// `forced`, as the taker's assertion.
    pub attested: bool,
}

/// What a seizure took the resource from: one of hostler's claim kinds, or a foreign claim
/// (P3b: an agent-device session). The claim kinds serialize as `ClaimKind` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeizedFrom {
    Lease,
    Hold,
    Reservation,
    Foreign,
}

impl From<ClaimKind> for SeizedFrom {
    fn from(kind: ClaimKind) -> SeizedFrom {
        match kind {
            ClaimKind::Lease => SeizedFrom::Lease,
            ClaimKind::Hold => SeizedFrom::Hold,
            ClaimKind::Reservation => SeizedFrom::Reservation,
        }
    }
}

impl PartialEq<ClaimKind> for SeizedFrom {
    fn eq(&self, kind: &ClaimKind) -> bool {
        *self == SeizedFrom::from(*kind)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeizureRecord {
    pub id: String,
    pub resource: String,
    pub from_workspace_path: String,
    pub from_kind: SeizedFrom,
    /// The foreign source a `SeizedFrom::Foreign` seizure ended (`agent-device`); `None` for
    /// hostler's own claims (P2-12).
    #[serde(default)]
    pub from_source: Option<String>,
    pub from_pid: Option<u32>,
    pub by_workspace_path: String,
    pub by_pid: u32,
    pub at_ms: i64,
    pub stranded: bool,
    /// `release --force`: ended without stopping anything (L23).
    pub forced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceResolve {
    pub project: ProjectRef,
    /// The checkout's root as the CLI displays it (`project::display_path`: on Windows without
    /// the `\\?\` prefix). The service canonicalizes it before comparing it with anything.
    pub root: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOutcome {
    Registered,
    Known,
    Moved,
    Reidentified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub outcome: WorkspaceOutcome,
    pub project_repointed: bool,
    pub note: Option<String>,
    pub last_seen_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub service_version: String,
    pub heavy: QueueSnapshot,
    pub claims: ClaimsSnapshot,
    pub secrets: SecretsStatus,
}

/// The claims errors with their exact texts (the guide quotes them).
pub mod errors {
    use serde_json::json;

    use super::FORCE_SENTENCE;
    use crate::contract::{ErrorCode, HostlerError};

    fn error(code: ErrorCode, message: String, recovery: Option<&str>) -> HostlerError {
        HostlerError {
            code,
            message,
            details: json!({}),
            recovery: recovery.map(str::to_owned),
        }
    }

    pub fn unknown_resource(id: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::UnknownResource,
            format!("{id} is not a resource this hostler knows"),
            None,
        );
        e.details = json!({ "resource": id });
        e
    }

    pub fn resource_in_use(display: &str, holder_path: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::ResourceInUse,
            format!("{display} is claimed by {holder_path}"),
            Some("hostler status"),
        );
        e.details = json!({ "resource": display, "holder_path": holder_path });
        e
    }

    pub fn claim_not_held(id: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::ClaimNotHeld,
            format!("claim {id} is not held by this workspace"),
            Some("hostler status"),
        );
        e.details = json!({ "claim_id": id });
        e
    }

    pub fn seizure_failed(display: &str, why: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::SeizureFailed,
            format!("{display} stays with its holder: {why}"),
            Some("hostler status"),
        );
        e.details = json!({ "resource": display });
        e
    }

    /// No recovery: the takeover command is in the message, with the D9 sentence.
    pub fn force_required(
        display: &str,
        holder_path: &str,
        cmd: &str,
        token: &str,
    ) -> HostlerError {
        let mut e = error(
            ErrorCode::ForceRequired,
            format!(
                "{display} is held by {holder_path}. To take it over: {cmd} --force \
                 --confirmation {token}. {FORCE_SENTENCE}"
            ),
            None,
        );
        e.details = json!({ "resource": display, "holder_path": holder_path, "token": token });
        e
    }

    pub fn account_missing(name: &str, project: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::AccountMissing,
            format!("account {name} does not exist in project {project}"),
            None,
        );
        e.recovery = Some(format!("hostler account set {name} --field <field>"));
        e.details = json!({ "name": name, "project": project });
        e
    }

    pub fn claim_deadlock(display: &str, cycle: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::ClaimDeadlock,
            format!(
                "waiting for {display} would never end: {cycle}; this request was refused so \
                 the others can finish; if a nested run was started in the background, take every \
                 resource in the outer run"
            ),
            Some("hostler status"),
        );
        e.details = json!({ "resource": display, "cycle": cycle });
        e
    }

    pub fn bad_binding() -> HostlerError {
        error(
            ErrorCode::BadRequest,
            "--while-pid and --while-parent must name a process that runs this hostler command \
             (an ancestor), not pid 1, a service manager or the desktop shell"
                .into(),
            None,
        )
    }
}

/// Values for the contract tests (here and in `wire.rs`).
#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::BTreeMap;

    use super::*;
    use crate::procs::RunGroup;

    pub fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: "boot-1".into(),
            start_time: 500,
        }
    }

    pub fn group() -> RunGroup {
        RunGroup::UnixGroup {
            pgid: 42,
            boot_id: "boot-1".into(),
        }
    }

    pub fn screen() -> ResourceInfo {
        ResourceInfo {
            id: ResourceId::screen(),
            kind: ResourceKind::Screen,
            display: "screen".into(),
            attributes: BTreeMap::new(),
            available: true,
        }
    }

    pub fn claim() -> Claim {
        Claim {
            id: "c1".into(),
            resource: screen(),
            kind: ClaimKind::Lease,
            parent: Some("h1".into()),
            workspace: WorkspaceRef {
                id: "ws-1".into(),
                path: "/work/app".into(),
            },
            holder: Some(identity(42)),
            child: Some(identity(43)),
            group: Some(group()),
            binding: Some(HoldBinding::Ttl {
                ttl_ms: 1_800_000,
                expires_at_ms: 9_000,
            }),
            command: Some("make test".into()),
            granted_at_ms: 1_000,
        }
    }

    pub fn foreign() -> ForeignClaim {
        ForeignClaim {
            resource: ResourceId("device:test:0".into()),
            source: "fake".into(),
            description: "agent-device session".into(),
            workspace_path: Some("/work/other".into()),
            since_ms: Some(500),
            stop: Some(serde_json::json!({ "session": "s1" })),
            workspace_id: None,
        }
    }

    pub fn summary() -> ClaimSummary {
        ClaimSummary {
            id: "c1".into(),
            resource: "screen".into(),
            kind: ClaimKind::Hold,
            workspace_path: "/work/app".into(),
            pid: Some(42),
            command: None,
            since_ms: 1_000,
            expires_at_ms: Some(9_000),
            linked: 2,
        }
    }

    pub fn request() -> ClaimRequest {
        ClaimRequest {
            id: "6f1e6a9a-2d0f-4b7c-9a41-6f0f2f7c1a11".into(),
            selector: Selector::Exact {
                resource: ResourceId::screen(),
            },
            workspace_id: "ws-1".into(),
            requester: identity(42),
            purpose: Purpose::Lease {
                command: "make test".into(),
            },
            reentrant_tokens: vec!["c0".into()],
            no_wait: false,
        }
    }

    pub fn history_item() -> ClaimHistoryItem {
        ClaimHistoryItem {
            id: "c1".into(),
            resource: "screen".into(),
            kind: ClaimKind::Lease,
            workspace_path: "/work/app".into(),
            command: Some("make test".into()),
            granted_at_ms: None,
            ended_at_ms: 2_000,
            reason: EndReason::Deadlock,
            exit_code: None,
            detail: Some("/work/app (PID 42) holds screen and waits for heavy".into()),
        }
    }

    pub fn snapshot() -> ClaimsSnapshot {
        ClaimsSnapshot {
            resources: vec![ResourceState {
                resource: screen(),
                claims: vec![claim()],
                foreign: vec![foreign()],
                waiting: vec![WaitingRequest {
                    request_id: "r2".into(),
                    ticket: 7,
                    workspace_path: "/work/other".into(),
                    kind: ClaimKind::Hold,
                    since_ms: 1_500,
                }],
                seizing: true,
                seizing_by: Some("/work/taker".into()),
            }],
        }
    }

    pub fn seizure() -> SeizureRecord {
        SeizureRecord {
            id: "s1".into(),
            resource: "screen".into(),
            from_workspace_path: "/work/app".into(),
            from_kind: ClaimKind::Lease.into(),
            from_source: None,
            from_pid: Some(42),
            by_workspace_path: "/work/other".into(),
            by_pid: 77,
            at_ms: 3_000,
            stranded: false,
            forced: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::fixtures::*;
    use super::*;
    use crate::contract::ErrorCode;
    use crate::procs::RunGroup;

    const UUID: &str = "0b7e7c55-7a3e-4b0e-9c2b-1f2d3e4f5a6b";

    #[test]
    fn resource_ids_parse_and_print() {
        let screen = ResourceId::screen();
        assert_eq!(screen.0, "screen");
        assert_eq!(screen.kind(), Some(ResourceKind::Screen));
        assert_eq!(screen.token(), "screen");
        assert_eq!(screen.account_project(), None);
        assert_eq!(serde_json::to_value(&screen).unwrap(), json!("screen"));

        let account = ResourceId::account(UUID, "qa-user");
        assert_eq!(account.0, format!("account:{UUID}/qa-user"));
        assert_eq!(account.kind(), Some(ResourceKind::Account));
        assert_eq!(account.token(), "qa-user");
        assert_eq!(account.account_project(), Some(UUID));

        let device = ResourceId("device:android:R5CT1234".into());
        assert_eq!(device.kind(), Some(ResourceKind::Device));
        assert_eq!(device.account_project(), None);

        for bad in [
            "account:x/BAD",
            "account:/qa-user",
            "account:x",
            "account:x/",
            "device:",
            "device:android",
            "device:android:",
            "screen2",
            "",
        ] {
            assert_eq!(ResourceId(bad.into()).kind(), None, "{bad:?}");
            assert_eq!(ResourceId(bad.into()).account_project(), None, "{bad:?}");
        }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T) {
        let encoded = serde_json::to_value(&value).unwrap();
        assert_eq!(serde_json::from_value::<T>(encoded).unwrap(), value);
    }

    #[test]
    fn answers_round_trip() {
        let mut attributes = BTreeMap::new();
        attributes.insert("test".to_owned(), "1".to_owned());
        round_trip(Selector::Any {
            kind: ResourceKind::Device,
            attributes,
        });
        round_trip(HoldBinding::Process {
            holder: identity(9),
        });
        round_trip(Purpose::Hold {
            ttl_ms: None,
            bound_to: Some(identity(9)),
            distinct: true,
        });
        for outcome in [
            ClaimOutcome::Granted { claim: claim() },
            ClaimOutcome::Joined { claim: claim() },
            ClaimOutcome::Reentrant {
                claim_id: "c0".into(),
            },
            ClaimOutcome::Queued {
                request_id: "r1".into(),
                position: 2,
                of: 3,
                blockers: vec![
                    Blocker::Claim { summary: summary() },
                    Blocker::Foreign { claim: foreign() },
                    Blocker::Seizing {
                        by_workspace: "/work/other".into(),
                    },
                ],
            },
            ClaimOutcome::Busy {
                position: 1,
                of: 1,
                blockers: Vec::new(),
            },
            ClaimOutcome::Finished {
                item: history_item(),
            },
        ] {
            round_trip(outcome);
        }
        round_trip(ReleaseOutcome {
            parent: Some(claim()),
            seized_by: None,
        });
        round_trip(ReleaseOutcome {
            parent: None,
            seized_by: Some("/work/b".into()),
        });
        round_trip(SeizePlan {
            plan_id: "p1".into(),
            claims: vec![claim()],
            foreign: vec![foreign()],
            stops: vec![
                StopTarget::RunGroup {
                    claim_id: "c1".into(),
                    holder: identity(42),
                    child: Some(identity(43)),
                    group: Some(group()),
                },
                StopTarget::Process {
                    claim_id: "h1".into(),
                    process: identity(40),
                },
                StopTarget::Foreign {
                    source: "fake".into(),
                    resource: ResourceId("device:test:0".into()),
                    data: json!({ "session": "s1" }),
                },
            ],
            expires_at_ms: 61_000,
        });
        round_trip(WorkspaceInfo {
            id: "ws-1".into(),
            project_id: "p1".into(),
            path: "/work/app".into(),
            outcome: WorkspaceOutcome::Reidentified,
            project_repointed: false,
            note: Some("a copy".into()),
            last_seen_ms: 4_000,
        });
        round_trip(seizure());
        round_trip(snapshot());
        assert_eq!(
            serde_json::to_value(EndReason::TimedOut).unwrap(),
            json!("timed_out")
        );
        assert_eq!(
            serde_json::to_value(StopEvidence::JobGone {
                target: 1,
                session: 2
            })
            .unwrap(),
            json!({ "kind": "job_gone", "target": 1, "session": 2 })
        );
        round_trip(StopEvidence::Gone { target: 0 });
    }

    #[test]
    fn windows_job_without_session_still_parses() {
        let p1 = json!({ "kind": "windows_job", "name": "Local\\hostler-heavy-1", "boot_id": "b" });
        let group: RunGroup = serde_json::from_value(p1.clone()).unwrap();
        assert_eq!(
            group,
            RunGroup::WindowsJob {
                name: "Local\\hostler-heavy-1".into(),
                boot_id: "b".into(),
                session: None,
            }
        );
        // Without a session the JSON stays exactly P1's.
        assert_eq!(serde_json::to_value(&group).unwrap(), p1);
        let with = RunGroup::WindowsJob {
            name: "n".into(),
            boot_id: "b".into(),
            session: Some(2),
        };
        assert_eq!(serde_json::to_value(&with).unwrap()["session"], 2);
        round_trip(with);
    }

    #[test]
    fn error_texts_are_exact() {
        let e = errors::unknown_resource("device:x:y");
        assert_eq!(e.code, ErrorCode::UnknownResource);
        assert_eq!(e.message, "device:x:y is not a resource this hostler knows");

        let e = errors::resource_in_use("screen", "/work/app");
        assert_eq!(e.code, ErrorCode::ResourceInUse);
        assert_eq!(e.message, "screen is claimed by /work/app");
        assert_eq!(e.recovery.as_deref(), Some("hostler status"));

        let e = errors::claim_not_held("c1");
        assert_eq!(e.code, ErrorCode::ClaimNotHeld);
        assert_eq!(e.message, "claim c1 is not held by this workspace");
        assert_eq!(e.recovery.as_deref(), Some("hostler status"));

        let e = errors::seizure_failed("screen", "PID 42 is still running");
        assert_eq!(e.code, ErrorCode::SeizureFailed);
        assert_eq!(
            e.message,
            "screen stays with its holder: PID 42 is still running"
        );
        assert_eq!(e.recovery.as_deref(), Some("hostler status"));

        let e = errors::account_missing("qa-user", "app");
        assert_eq!(e.code, ErrorCode::AccountMissing);
        assert_eq!(e.message, "account qa-user does not exist in project app");
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler account set qa-user --field <field>")
        );

        let e = errors::claim_deadlock("screen", "/a (PID 1) holds x and waits for screen");
        assert_eq!(e.code, ErrorCode::ClaimDeadlock);
        assert_eq!(
            e.message,
            "waiting for screen would never end: /a (PID 1) holds x and waits for screen; this \
             request was refused so the others can finish; if a nested run was started in the \
             background, take every resource in the outer run"
        );
        assert_eq!(e.recovery.as_deref(), Some("hostler status"));

        let e = errors::bad_binding();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert_eq!(
            e.message,
            "--while-pid and --while-parent must name a process that runs this hostler command \
             (an ancestor), not pid 1, a service manager or the desktop shell"
        );
    }

    #[test]
    fn seized_from_serializes_like_claim_kind_and_adds_foreign() {
        for kind in [ClaimKind::Lease, ClaimKind::Hold, ClaimKind::Reservation] {
            let from = SeizedFrom::from(kind);
            assert_eq!(
                serde_json::to_value(from).unwrap(),
                serde_json::to_value(kind).unwrap(),
                "{kind:?}"
            );
            assert_eq!(from, kind);
            round_trip(from);
        }
        assert_eq!(
            serde_json::to_value(SeizedFrom::Lease).unwrap(),
            json!("lease")
        );
        assert_eq!(
            serde_json::to_value(SeizedFrom::Hold).unwrap(),
            json!("hold")
        );
        assert_eq!(
            serde_json::to_value(SeizedFrom::Reservation).unwrap(),
            json!("reservation")
        );
        assert_eq!(
            serde_json::to_value(SeizedFrom::Foreign).unwrap(),
            json!("foreign")
        );
        assert_ne!(SeizedFrom::Foreign, ClaimKind::Lease);
        let foreign = SeizureRecord {
            from_kind: SeizedFrom::Foreign,
            from_source: Some("agent-device".into()),
            ..seizure()
        };
        let value = serde_json::to_value(&foreign).unwrap();
        assert_eq!(value["from_kind"], "foreign");
        assert_eq!(value["from_source"], "agent-device");
        round_trip(foreign);
        // A record without `from_source` (a hostler claim) still reads.
        let mut old = serde_json::to_value(seizure()).unwrap();
        old.as_object_mut().unwrap().remove("from_source");
        let back: SeizureRecord = serde_json::from_value(old).unwrap();
        assert_eq!(back, seizure());
    }

    #[test]
    fn foreign_claim_workspace_id_defaults_to_none() {
        let mut value = serde_json::to_value(foreign()).unwrap();
        assert_eq!(value["workspace_id"], json!(null));
        value.as_object_mut().unwrap().remove("workspace_id");
        let back: ForeignClaim = serde_json::from_value(value).unwrap();
        assert_eq!(back.workspace_id, None);
        assert_eq!(back, foreign());
        round_trip(ForeignClaim {
            workspace_id: Some("ws-2".into()),
            ..foreign()
        });
    }

    #[test]
    fn blocker_checking_round_trips() {
        let checking = Blocker::Checking {
            resource: "pixel-9".into(),
            text: "checking for agent-device sessions".into(),
        };
        assert_eq!(
            serde_json::to_value(&checking).unwrap(),
            json!({
                "blocker": "checking",
                "resource": "pixel-9",
                "text": "checking for agent-device sessions"
            })
        );
        round_trip(checking.clone());
        round_trip(ClaimOutcome::Queued {
            request_id: "r1".into(),
            position: 1,
            of: 1,
            blockers: vec![checking],
        });
    }

    #[test]
    fn freshness_and_foreign_view_have_the_wire_shape() {
        assert_eq!(
            serde_json::to_value(Freshness::NotTracked).unwrap(),
            json!("not_tracked")
        );
        assert_eq!(
            serde_json::to_value(Freshness::NotYet).unwrap(),
            json!("not_yet")
        );
        assert_eq!(
            serde_json::to_value(Freshness::Since(7)).unwrap(),
            json!({ "since": 7 })
        );
        round_trip(ForeignView {
            claims: vec![foreign()],
            freshness: Freshness::Since(u64::MAX),
            unreadable: Some("node not found".into()),
        });
    }

    #[test]
    fn force_text_has_the_sentence() {
        let e = errors::force_required("screen", "/work/app", "hostler run --screen", "screen");
        assert_eq!(e.code, ErrorCode::ForceRequired);
        assert_eq!(
            e.message,
            "screen is held by /work/app. To take it over: hostler run --screen --force \
             --confirmation screen. An agent uses --force only with the operator's explicit \
             permission."
        );
        assert!(e.message.contains(FORCE_SENTENCE));
    }
}
