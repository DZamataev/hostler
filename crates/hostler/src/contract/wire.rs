//! Newline-delimited JSON frames (plan Task 4; secrets methods P2 Task 1).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use super::HostlerError;
use super::heavy::{HeavyAcquire, HeavyRelease, HeavyStarted, HistoryQuery, QueueSnapshot};
use super::secrets::{
    DeclaredSecret, Decline, Materialize, ProjectRef, SecretImport, SecretRequest,
    SecretRequestInfo, SecretSet, SecretsStatus, Share, Unlock,
};

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
    #[serde(rename = "secrets.status")]
    SecretsStatus,
    #[serde(rename = "secrets.unlock")]
    SecretsUnlock(Unlock),
    #[serde(rename = "secrets.lock")]
    SecretsLock,
    #[serde(rename = "project.resolve")]
    ProjectResolve(ProjectRef),
    #[serde(rename = "project.list")]
    ProjectList,
    #[serde(rename = "project.approve")]
    ProjectApprove(ProjectRef),
    #[serde(rename = "secret.list")]
    SecretList {
        project: ProjectRef,
        declared: Vec<DeclaredSecret>,
    },
    #[serde(rename = "secret.set")]
    SecretSet(SecretSet),
    #[serde(rename = "secret.remove")]
    SecretRemove { project: ProjectRef, name: String },
    #[serde(rename = "secret.import")]
    SecretImport(SecretImport),
    #[serde(rename = "secret.request")]
    SecretRequest(SecretRequest),
    #[serde(rename = "secret.request_withdraw")]
    SecretRequestWithdraw { id: String, timed_out: bool },
    #[serde(rename = "secret.requests")]
    SecretRequests,
    #[serde(rename = "secret.decline")]
    SecretDecline(Decline),
    #[serde(rename = "secret.share")]
    SecretShare(Share),
    #[serde(rename = "secret.unshare")]
    SecretUnshare(Share),
    #[serde(rename = "secret.materialize")]
    SecretMaterialize(Materialize),
}

/// Methods whose params may carry a value (or a command that may hold one, L12): a frame that
/// fails to parse gets a fixed error text, never serde's (which quotes the input).
pub fn sensitive_request(method: &str) -> bool {
    matches!(
        method,
        "secrets.unlock" | "secret.set" | "secret.import" | "secret.materialize"
    )
}

/// Methods whose answer carries values: never parsed into a `serde_json::Value`.
pub fn sensitive_answer(method: &str) -> bool {
    method == "secret.materialize"
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFrame {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(rename = "secrets.request_changed")]
    SecretRequestChanged(SecretRequestInfo),
    #[serde(rename = "secrets.lock_changed")]
    SecretsLockChanged(SecretsStatus),
}

/// The tag of an adjacently tagged value (`{"<tag>":"…","<content>":…}`) and the byte length
/// of its content, read back from an encoding that is erased afterwards.
fn tag_and_content_len<T: Serialize>(value: &T, tag: &str, content: &str) -> (String, usize) {
    let Ok(encoded) = serde_json::to_vec(value).map(Zeroizing::new) else {
        return ("<unencodable>".into(), 0);
    };
    #[derive(Deserialize)]
    struct Tags {
        method: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
    }
    let Ok(Tags { method, kind }) = serde_json::from_slice::<Tags>(&encoded) else {
        return ("<unencodable>".into(), 0);
    };
    let name = method.or(kind).unwrap_or_default();
    // `{"tag":"name"}` without content; `,"content":` comes before any content.
    let bare = tag.len() + name.len() + 7;
    let len = encoded.len().saturating_sub(bare + content.len() + 4);
    (name, len)
}

struct Bytes(usize);

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// Never prints the params: they may carry values.
impl fmt::Debug for ClientFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (method, len) = tag_and_content_len(&self.request, "method", "params");
        f.debug_struct("ClientFrame")
            .field("id", &self.id)
            .field("method", &method)
            .field("params", &Bytes(len))
            .finish()
    }
}

/// Never prints a result or an event's data: a result may carry values.
impl fmt::Debug for ServerFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoded_len = |v: &Value| {
            serde_json::to_vec(v)
                .map(Zeroizing::new)
                .map_or(0, |b| b.len())
        };
        match self {
            ServerFrame::Response { id, result } => f
                .debug_struct("Response")
                .field("id", id)
                .field("result", &Bytes(encoded_len(result)))
                .finish(),
            ServerFrame::Error { id, error } => f
                .debug_struct("Error")
                .field("id", id)
                .field("code", &error.code)
                .finish(),
            ServerFrame::Event { event } => {
                let (kind, len) = tag_and_content_len(event, "type", "data");
                f.debug_struct("Event")
                    .field("type", &kind)
                    .field("data", &Bytes(len))
                    .finish()
            }
        }
    }
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
    use crate::contract::secrets::*;
    use crate::procs::{ProcessIdentity, RunGroup};
    use crate::secrets::value::{RawCommand, SecretValue};

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
                "params": {"contract_version": CONTRACT_VERSION, "client": "cli"}
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
        assert_eq!(
            serde_json::to_value(status).unwrap()["contract_version"],
            CONTRACT_VERSION
        );
    }

    fn project() -> ProjectRef {
        ProjectRef {
            common_dir: "/work/app/.git".into(),
            name: "app".into(),
        }
    }

    fn secret_requests() -> Vec<Request> {
        let share = Share {
            from: project(),
            name: "API_KEY".into(),
            to_project_id: "p2".into(),
        };
        vec![
            Request::SecretsStatus,
            Request::SecretsUnlock(Unlock {
                password: SecretValue::new("master-pass".into()),
                key_file: Some("/keys/k.keyx".into()),
                forget_key_file: false,
                create: true,
            }),
            Request::SecretsLock,
            Request::ProjectResolve(project()),
            Request::ProjectList,
            Request::ProjectApprove(project()),
            Request::SecretList {
                project: project(),
                declared: vec![DeclaredSecret {
                    name: "API_KEY".into(),
                    purpose: "the indexer".into(),
                    env: Some("INDEXER_KEY".into()),
                }],
            },
            Request::SecretSet(SecretSet {
                project: project(),
                name: "API_KEY".into(),
                value: SecretValue::new("sk-value-1".into()),
            }),
            Request::SecretRemove {
                project: project(),
                name: "API_KEY".into(),
            },
            Request::SecretImport(SecretImport {
                project: project(),
                entries: vec![ImportEntry {
                    name: "API_KEY".into(),
                    value: SecretValue::new("sk-value-2".into()),
                }],
                overwrite: true,
            }),
            Request::SecretRequest(SecretRequest {
                id: "6f1e6a9a-2d0f-4b7c-9a41-6f0f2f7c1a11".into(),
                project: project(),
                name: "API_KEY".into(),
                reason: "run the indexer tests".into(),
                requester: identity(42),
            }),
            Request::SecretRequestWithdraw {
                id: "6f1e6a9a".into(),
                timed_out: true,
            },
            Request::SecretRequests,
            Request::SecretDecline(Decline {
                id: None,
                project: Some(project()),
                name: Some("API_KEY".into()),
            }),
            Request::SecretShare(share.clone()),
            Request::SecretUnshare(share),
            Request::SecretMaterialize(materialize("curl -H \"X: sk-value-3\"")),
        ]
    }

    fn materialize(command: &str) -> Materialize {
        Materialize {
            run_id: "run-1".into(),
            project: project(),
            names: vec!["API_KEY".into()],
            command: RawCommand::new(command.into()),
            workspace: "/work/app".into(),
        }
    }

    #[test]
    fn secret_requests_round_trip() {
        let methods = [
            "secrets.status",
            "secrets.unlock",
            "secrets.lock",
            "project.resolve",
            "project.list",
            "project.approve",
            "secret.list",
            "secret.set",
            "secret.remove",
            "secret.import",
            "secret.request",
            "secret.request_withdraw",
            "secret.requests",
            "secret.decline",
            "secret.share",
            "secret.unshare",
            "secret.materialize",
        ];
        let requests = secret_requests();
        assert_eq!(requests.len(), methods.len());
        for (request, method) in requests.into_iter().zip(methods) {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["method"], method);
            let back: Request = serde_json::from_value(value).unwrap();
            assert_eq!(back, request, "{method}");
        }
    }

    #[test]
    fn secret_events_round_trip() {
        let events = [
            Event::SecretRequestChanged(SecretRequestInfo {
                id: "6f1e6a9a".into(),
                project_id: "p1".into(),
                project_name: "app".into(),
                name: "API_KEY".into(),
                reason: "tests".into(),
                state: RequestState::TimedOut,
                requester_pid: 42,
                created_at_ms: 1_000,
                answered_at_ms: Some(2_000),
            }),
            Event::SecretsLockChanged(SecretsStatus {
                state: LockState::Unlocked,
                database: "/state/hostler.kdbx".into(),
                key_file: None,
                pending_requests: 1,
                preserved_files: vec!["hostler.kdbx.prev-1".into()],
            }),
        ];
        let types = ["secrets.request_changed", "secrets.lock_changed"];
        for (event, kind) in events.into_iter().zip(types) {
            let frame = ServerFrame::Event { event };
            let value = serde_json::to_value(&frame).unwrap();
            assert_eq!(value["event"]["type"], kind);
            assert_eq!(serde_json::from_value::<ServerFrame>(value).unwrap(), frame);
        }
    }

    #[test]
    fn materialize_frame_matches_wire_shape() {
        let frame = ClientFrame {
            id: 1,
            request: Request::SecretMaterialize(materialize("make test")),
        };
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({
                "id": 1,
                "method": "secret.materialize",
                "params": {
                    "run_id": "run-1",
                    "project": {"common_dir": "/work/app/.git", "name": "app"},
                    "names": ["API_KEY"],
                    "command": "make test",
                    "workspace": "/work/app"
                }
            })
        );
    }

    #[test]
    fn sensitive_requests_are_exactly_four() {
        let sensitive: Vec<String> = secret_requests()
            .iter()
            .chain(&all_requests())
            .map(|r| {
                serde_json::to_value(r).unwrap()["method"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .filter(|m| sensitive_request(m))
            .collect();
        assert_eq!(
            sensitive,
            [
                "secrets.unlock",
                "secret.set",
                "secret.import",
                "secret.materialize"
            ]
        );
        assert!(!sensitive_request("secret.setx"));
        assert!(!sensitive_request(""));
    }

    #[test]
    fn sensitive_answer_only_materialize() {
        for request in secret_requests().iter().chain(&all_requests()) {
            let method = serde_json::to_value(request).unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(sensitive_answer(&method), method == "secret.materialize");
        }
    }

    #[test]
    fn frame_debug_never_prints_payloads() {
        let response = ServerFrame::Response {
            id: 9,
            result: json!([{"name": "API_KEY", "value": "hunter2-canary"}]),
        };
        let printed = format!("{response:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(!printed.contains("API_KEY"), "{printed}");
        assert!(printed.contains("id: 9"), "{printed}");

        let import = ClientFrame {
            id: 3,
            request: Request::SecretImport(SecretImport {
                project: project(),
                entries: vec![ImportEntry {
                    name: "API_KEY".into(),
                    value: SecretValue::new("hunter2-canary".into()),
                }],
                overwrite: false,
            }),
        };
        let printed = format!("{import:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(!printed.contains("API_KEY"), "{printed}");
        assert!(printed.contains("id: 3"), "{printed}");
        assert!(printed.contains("secret.import"), "{printed}");

        let error = ServerFrame::Error {
            id: 4,
            error: crate::contract::secrets::errors::unlock_failed(),
        };
        assert!(format!("{error:?}").contains("id: 4"));
        let event = ServerFrame::Event {
            event: Event::SecretsLockChanged(SecretsStatus {
                state: LockState::Locked,
                database: "/state/hunter2-dir/hostler.kdbx".into(),
                key_file: None,
                pending_requests: 0,
                preserved_files: Vec::new(),
            }),
        };
        let printed = format!("{event:?}");
        assert!(printed.contains("secrets.lock_changed"), "{printed}");
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    #[test]
    fn frame_debug_counts_the_payload_bytes() {
        let shutdown = ClientFrame {
            id: 1,
            request: Request::ServiceShutdown { if_idle: true },
        };
        assert_eq!(
            format!("{shutdown:?}"),
            "ClientFrame { id: 1, method: \"service.shutdown\", params: <16 bytes> }"
        );
        let status = ClientFrame {
            id: 2,
            request: Request::ServiceStatus,
        };
        assert_eq!(
            format!("{status:?}"),
            "ClientFrame { id: 2, method: \"service.status\", params: <0 bytes> }"
        );
        let response = ServerFrame::Response {
            id: 5,
            result: json!({"a": 1}),
        };
        assert_eq!(
            format!("{response:?}"),
            "Response { id: 5, result: <7 bytes> }"
        );
    }
}
