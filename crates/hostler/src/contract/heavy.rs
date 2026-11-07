//! Heavy-queue payloads and hostler-level status (plan Task 4).

use serde::{Deserialize, Serialize};

use crate::procs::{ProcessIdentity, RunGroup};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeavyState {
    Waiting,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeavyOutcome {
    Ok,
    Failed,
    Withdrawn,
    TimedOut,
    Stopped,
    Reaped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntrySource {
    Hostler,
    RnLookyLedger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct HeavyStarted {
    pub id: String,
    pub child: ProcessIdentity,
    pub group: RunGroup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavyRelease {
    pub id: String,
    pub exit_code: Option<i32>,
    pub outcome: HeavyOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub capacity: u8,
    pub running: Vec<HeavyEntry>,
    pub waiting: Vec<HeavyEntry>,
    pub foreign_running: Vec<HeavyEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct ServiceStatus {
    pub service_version: String,
    pub contract_version: u32,
    pub pid: u32,
    pub running_heavy: u32,
    pub waiting_heavy: u32,
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
