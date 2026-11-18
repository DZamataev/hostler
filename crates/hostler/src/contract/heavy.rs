//! Heavy-queue payloads and hostler-level status (plan Task 4).

use serde::{Deserialize, Serialize};

use crate::contract::process::{ProcessIdentity, RunGroup};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HeavyState {
    Waiting,
    Running,
    /// P5 (spec §11.1): `heavy.stop` is stopping it. It still occupies its slot, is listed
    /// under the snapshot's `running`, and is never granted, reaped, withdrawn or
    /// force-released meanwhile. Only Task 6's stop sets it.
    Stopping,
}

impl HeavyState {
    /// Running or stopping: the entry holds a slot.
    pub fn occupies_slot(self) -> bool {
        matches!(self, HeavyState::Running | HeavyState::Stopping)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HeavyOutcome {
    Ok,
    Failed,
    Withdrawn,
    TimedOut,
    /// Only a verified `heavy.stop` that signalled the task (spec §11.1, P5-L10).
    Stopped,
    Reaped,
    /// `heavy.force_release` (`hostler queue release --force`): the slot was freed and nothing
    /// was stopped (spec §11.1, P5-L10).
    ForceReleased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntrySource {
    Hostler,
    RnLookyLedger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HeavyEntry {
    pub id: String,
    pub ticket: u64,
    pub kind: String,
    pub command: String,
    pub cwd: String,
    pub workspace_root: Option<String>,
    pub holder: ProcessIdentity,
    pub child: Option<ProcessIdentity>,
    pub group: Option<RunGroup>,
    pub state: HeavyState,
    pub enqueued_at_ms: i64,
    pub granted_at_ms: Option<i64>,
    pub source: EntrySource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HeavyAcquire {
    /// Client-generated UUID v4; makes acquire idempotent.
    pub id: String,
    pub kind: String,
    pub command: String,
    pub cwd: String,
    pub workspace_root: Option<String>,
    pub holder: ProcessIdentity,
    pub reentrant_token: Option<String>,
    pub no_wait: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AcquireOutcome {
    Granted {
        entry: HeavyEntry,
    },
    Queued {
        entry: HeavyEntry,
        position: u32,
        of: u32,
    },
    Reentrant,
    Busy {
        position: u32,
        of: u32,
    },
    Finished {
        item: HeavyHistoryItem,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HeavyStarted {
    pub id: String,
    pub child: ProcessIdentity,
    pub group: RunGroup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HeavyRelease {
    pub id: String,
    pub exit_code: Option<i32>,
    pub outcome: HeavyOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct QueueSnapshot {
    pub capacity: u8,
    pub running: Vec<HeavyEntry>,
    pub waiting: Vec<HeavyEntry>,
    pub foreign_running: Vec<HeavyEntry>,
    /// P5 (spec §6.2): the last event `seq` assigned when this snapshot was taken; in a
    /// `queue.changed` event, that event's own `seq`.
    pub as_of_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HeavyHistoryItem {
    pub id: String,
    pub ticket: u64,
    pub kind: String,
    pub command: String,
    pub cwd: String,
    pub workspace_root: Option<String>,
    pub enqueued_at_ms: i64,
    pub granted_at_ms: Option<i64>,
    pub finished_at_ms: i64,
    pub exit_code: Option<i32>,
    pub outcome: HeavyOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(default)]
pub struct HistoryQuery {
    pub since_ms: Option<i64>,
    pub limit: u32,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        HistoryQuery {
            since_ms: None,
            limit: DEFAULT_HISTORY_LIMIT,
        }
    }
}

pub const DEFAULT_HISTORY_LIMIT: u32 = 100;
pub const MAX_HISTORY_LIMIT: u32 = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ServiceStatus {
    pub service_version: String,
    pub contract_version: u32,
    pub pid: u32,
    pub running_heavy: u32,
    pub waiting_heavy: u32,
    /// P4 (additive): whether the secrets database is unlocked. A P4+ service fills it; a
    /// 0.0.3 service omits it, which reads `None` ("cannot say", treated as live work).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_unlocked: Option<bool>,
    /// P4 (additive, Task 9 F-1 r1): the service's boot id, for a client that may not read its
    /// own (`procs::learn_boot_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    /// P5 (additive, spec §6.2): this service run's id, a UUID made at start; the same as the
    /// `hello` reply's. A client that sees it change reads every snapshot again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// P5 (additive, spec §9.7): the service's own binary, canonicalized
    /// (`std::env::current_exe()`); the app runs the CLI from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
}

/// An accepted `service.shutdown`'s answer (`{"stopping": true}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ShutdownAccepted {
    pub stopping: bool,
}

/// `heavy.stop`'s answer (P5, spec §11.1): `stopped` only when the service verified that the
/// holder, its child and their group are gone; `pids` are the processes it signalled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct QueueStopOutcome {
    pub id: String,
    pub stopped: bool,
    pub pids: Vec<u32>,
}

/// The heavy-queue errors of contract 5 with their exact texts.
pub mod errors {
    use serde_json::json;

    use crate::contract::{ErrorCode, HostlerError};

    /// `heavy.stop` could not verify that the task is gone; it keeps its slot (spec §11.1). The
    /// recovery names the CLI's real form, `queue release --id <id> --force` (the plan's text
    /// has a positional id, which that command does not take).
    pub fn stop_failed(id: &str, pids: &[u32]) -> HostlerError {
        HostlerError {
            code: ErrorCode::StopFailed,
            message: format!("hostler could not stop heavy task {id}; it keeps its slot"),
            details: json!({ "id": id, "pids": pids }),
            recovery: Some(format!(
                "the task still runs; stop it from its terminal, or: hostler queue release --id \
                 {id} --force"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(id: &str, state: HeavyState) -> HeavyEntry {
        HeavyEntry {
            id: id.into(),
            ticket: 7,
            kind: "gradle".into(),
            command: "./gradlew build".into(),
            cwd: "/work/app".into(),
            workspace_root: Some("/work/app".into()),
            holder: ProcessIdentity {
                pid: 42,
                boot_id: "boot-1".into(),
                start_time: 500,
            },
            child: None,
            group: Some(RunGroup::UnixGroup {
                pgid: 42,
                boot_id: "boot-1".into(),
            }),
            state,
            enqueued_at_ms: 1_000,
            granted_at_ms: Some(1_100),
            source: EntrySource::Hostler,
        }
    }

    #[test]
    fn heavy_entry_round_trip() {
        let original = entry("6f1e6a9a", HeavyState::Running);
        let value = serde_json::to_value(&original).unwrap();
        assert_eq!(value["state"], "running");
        assert_eq!(value["source"], "hostler");
        assert_eq!(
            value["group"],
            json!({"kind": "unix_group", "pgid": 42, "boot_id": "boot-1"})
        );
        let back: HeavyEntry = serde_json::from_value(value).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn history_query_defaults_on_the_wire() {
        let query: HistoryQuery = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            query,
            HistoryQuery {
                since_ms: None,
                limit: 100
            }
        );
    }

    #[test]
    fn acquire_outcome_json_shapes() {
        assert_eq!(
            serde_json::to_value(AcquireOutcome::Reentrant).unwrap(),
            json!({"outcome": "reentrant"})
        );
        assert_eq!(
            serde_json::to_value(AcquireOutcome::Busy { position: 2, of: 3 }).unwrap(),
            json!({"outcome": "busy", "position": 2, "of": 3})
        );

        let granted = AcquireOutcome::Granted {
            entry: entry("6f1e6a9a", HeavyState::Waiting),
        };
        let value = serde_json::to_value(&granted).unwrap();
        assert_eq!(value["outcome"], "granted");
        assert_eq!(value["entry"]["state"], "waiting");
        let back: AcquireOutcome = serde_json::from_value(value).unwrap();
        assert_eq!(back, granted);
    }
}
