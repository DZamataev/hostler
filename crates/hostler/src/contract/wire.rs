//! Newline-delimited JSON frames (plan Task 4).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::HostlerError;
use super::heavy::{HeavyAcquire, HeavyRelease, HeavyStarted, HistoryQuery, QueueSnapshot};

/// Lifecycle methods are frozen forever; the rest require a matching `hello` first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "hello")]
    Hello {
        contract_version: u32,
        client: String,
    },
    #[serde(rename = "service.status")]
    ServiceStatus,
    #[serde(rename = "service.shutdown")]
    ServiceShutdown { if_idle: bool },
    #[serde(rename = "heavy.acquire")]
    HeavyAcquire(HeavyAcquire),
    #[serde(rename = "heavy.attach")]
    HeavyAttach { id: String },
    #[serde(rename = "heavy.started")]
    HeavyStarted(HeavyStarted),
    #[serde(rename = "heavy.release")]
    HeavyRelease(HeavyRelease),
    #[serde(rename = "heavy.withdraw")]
    HeavyWithdraw { id: String, timed_out: bool },
    #[serde(rename = "heavy.force_release")]
    HeavyForceRelease { id: String },
    #[serde(rename = "queue.snapshot")]
    QueueSnapshot,
    #[serde(rename = "queue.history")]
    QueueHistory(HistoryQuery),
    #[serde(rename = "queue.set_capacity")]
    QueueSetCapacity { capacity: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFrame {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerFrame {
    Response { id: u64, result: Value },
    Error { id: u64, error: HostlerError },
    Event { event: Event },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Event {
    #[serde(rename = "queue.changed")]
    QueueChanged(QueueSnapshot),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::contract::CONTRACT_VERSION;
    use crate::contract::heavy::{
        AcquireOutcome, EntrySource, HeavyEntry, HeavyHistoryItem, HeavyOutcome, HeavyState,
        ServiceStatus,
    };
    use crate::procs::{ProcessIdentity, RunGroup};

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: "boot-1".into(),
            start_time: 500,
        }
    }

    fn group() -> RunGroup {
        RunGroup::UnixGroup {
            pgid: 42,
            boot_id: "boot-1".into(),
        }
    }

    fn entry(id: &str) -> HeavyEntry {
        HeavyEntry {
            id: id.into(),
            ticket: 3,
            kind: "gradle".into(),
            command: "./gradlew build".into(),
            cwd: "/work/app".into(),
            workspace_root: None,
            holder: identity(42),
            child: None,
            group: None,
            state: HeavyState::Waiting,
            enqueued_at_ms: 1_000,
            granted_at_ms: None,
            source: EntrySource::Hostler,
        }
    }

    fn history_item(id: &str) -> HeavyHistoryItem {
        HeavyHistoryItem {
            id: id.into(),
            ticket: 3,
            kind: "gradle".into(),
            command: "./gradlew build".into(),
            cwd: "/work/app".into(),
            workspace_root: None,
            enqueued_at_ms: 1_000,
            granted_at_ms: Some(1_100),
            finished_at_ms: 2_000,
            exit_code: Some(0),
            outcome: HeavyOutcome::Ok,
        }
    }

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Hello {
                contract_version: CONTRACT_VERSION,
                client: "cli".into(),
            },
            Request::ServiceStatus,
            Request::ServiceShutdown { if_idle: true },
            Request::HeavyAcquire(HeavyAcquire {
                id: "6f1e6a9a".into(),
                kind: "gradle".into(),
                command: "./gradlew build".into(),
                cwd: "/work/app".into(),
                workspace_root: Some("/work/app".into()),
                holder: identity(42),
                reentrant_token: Some("token-1".into()),
                no_wait: false,
            }),
            Request::HeavyAttach {
                id: "6f1e6a9a".into(),
            },
            Request::HeavyStarted(HeavyStarted {
                id: "6f1e6a9a".into(),
                child: identity(43),
                group: group(),
            }),
            Request::HeavyRelease(HeavyRelease {
                id: "6f1e6a9a".into(),
                exit_code: Some(1),
                outcome: HeavyOutcome::Failed,
            }),
            Request::HeavyWithdraw {
                id: "6f1e6a9a".into(),
                timed_out: true,
            },
            Request::HeavyForceRelease {
                id: "6f1e6a9a".into(),
            },
            Request::QueueSnapshot,
            Request::QueueHistory(HistoryQuery {
                since_ms: Some(1_000),
                limit: 50,
            }),
            Request::QueueSetCapacity { capacity: 2 },
        ]
    }

    #[test]
    fn requests_round_trip() {
        for request in all_requests() {
            let value = serde_json::to_value(&request).unwrap();
            assert!(value.get("method").is_some(), "{request:?} lost its method");
            let back: Request = serde_json::from_value(value).unwrap();
            assert_eq!(back, request);
        }
    }

    #[test]
    fn acquire_frame_matches_wire_shape() {
        let frame = ClientFrame {
            id: 1,
            request: Request::HeavyAcquire(HeavyAcquire {
                id: "6f1e6a9a-2d0f-4b7c-9a41-6f0f2f7c1a11".into(),
                kind: "gradle".into(),
                command: "./gradlew build".into(),
                cwd: "/work/app".into(),
                workspace_root: Some("/work/app".into()),
                holder: identity(42),
                reentrant_token: Some("token-1".into()),
                no_wait: false,
            }),
        };

        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({
                "id": 1,
                "method": "heavy.acquire",
                "params": {
                    "id": "6f1e6a9a-2d0f-4b7c-9a41-6f0f2f7c1a11",
                    "kind": "gradle",
                    "command": "./gradlew build",
                    "cwd": "/work/app",
                    "workspace_root": "/work/app",
                    "holder": {"pid": 42, "boot_id": "boot-1", "start_time": 500},
                    "reentrant_token": "token-1",
                    "no_wait": false
                }
            })
        );
    }

    #[test]
    fn lifecycle_method_names_are_frozen() {
        let hello = serde_json::to_value(Request::Hello {
            contract_version: CONTRACT_VERSION,
            client: "cli".into(),
        })
        .unwrap();
        assert_eq!(
            hello,
            json!({
                "method": "hello",
                "params": {"contract_version": 1, "client": "cli"}
            })
        );

        let status = serde_json::to_value(Request::ServiceStatus).unwrap();
        assert_eq!(status, json!({"method": "service.status"}));

        let shutdown = serde_json::to_value(Request::ServiceShutdown { if_idle: true }).unwrap();
        assert_eq!(
            shutdown,
            json!({"method": "service.shutdown", "params": {"if_idle": true}})
        );

        assert_eq!(
            serde_json::from_value::<Request>(hello).unwrap(),
            Request::Hello {
                contract_version: CONTRACT_VERSION,
                client: "cli".into()
            }
        );
        assert_eq!(
            serde_json::from_value::<Request>(status).unwrap(),
            Request::ServiceStatus
        );
        assert_eq!(
            serde_json::from_value::<Request>(shutdown).unwrap(),
            Request::ServiceShutdown { if_idle: true }
        );
    }

    #[test]
    fn server_frames_round_trip() {
        let response = ServerFrame::Response {
            id: 1,
            result: json!({"contract_version": 1}),
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({"id": 1, "result": {"contract_version": 1}})
        );
        assert_eq!(
            serde_json::from_value::<ServerFrame>(serde_json::to_value(&response).unwrap())
                .unwrap(),
            response
        );

        let event = ServerFrame::Event {
            event: Event::QueueChanged(QueueSnapshot {
                capacity: 1,
                running: vec![entry("running")],
                waiting: vec![entry("waiting")],
                foreign_running: Vec::new(),
            }),
        };
        assert_eq!(
            serde_json::from_value::<ServerFrame>(serde_json::to_value(&event).unwrap()).unwrap(),
            event
        );

        // `AcquireOutcome` is carried in a response's untyped result.
        let outcome = AcquireOutcome::Finished {
            item: history_item("6f1e6a9a"),
        };
        assert_eq!(
            serde_json::to_value(&outcome).unwrap()["outcome"],
            "finished"
        );

        // `ServiceStatus` is a frozen lifecycle response payload.
        let status = ServiceStatus {
            service_version: "0.0.1".into(),
            contract_version: CONTRACT_VERSION,
            pid: 42,
            running_heavy: 0,
            waiting_heavy: 1,
        };
        assert_eq!(serde_json::to_value(status).unwrap()["contract_version"], 1);
    }
}
