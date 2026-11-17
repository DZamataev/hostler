//! Newline-delimited JSON frames (plan Task 4; secrets methods P2 Task 1; workspaces, claims,
//! status and accounts P3a Task 1; devices P3b Task 1).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use super::HostlerError;
use super::accounts::{AccountMaterialize, AccountRemove, AccountSet};
use super::claims::{
    ClaimRelease, ClaimRequest, ClaimStarted, ClaimsSnapshot, ResourceId, SeizeBegin, SeizeCommit,
    SeizeProgress, SeizureRecord, WorkspaceResolve,
};
use super::devices::{DevicesSnapshot, InstallEvent, ToolPathsSet};
use super::heavy::{HeavyAcquire, HeavyRelease, HeavyStarted, HistoryQuery, QueueSnapshot};
use super::secrets::{
    DeclaredSecret, Decline, Materialize, ProjectRef, SecretImport, SecretRequest,
    SecretRequestInfo, SecretSet, SecretsStatus, Share, Unlock,
};
use crate::procs::ProcessIdentity;

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
    /// `refuse_if_unlocked` (P4, additive): a P4+ service refuses with `ServiceBusy` (details
    /// `{secrets: "unlocked"}`) while its secrets database is unlocked. Absent (every 0.0.3
    /// client) reads `false`; `false` is not sent, so the frame stays the 0.0.3 shape, and an
    /// older service ignores the field.
    #[serde(rename = "service.shutdown")]
    ServiceShutdown {
        if_idle: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refuse_if_unlocked: bool,
    },
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
    #[serde(rename = "workspace.resolve")]
    WorkspaceResolve(WorkspaceResolve),
    #[serde(rename = "workspace.list")]
    WorkspaceList,
    #[serde(rename = "claims.acquire")]
    ClaimsAcquire(ClaimRequest),
    #[serde(rename = "claims.attach")]
    ClaimsAttach { request_id: String },
    #[serde(rename = "claims.started")]
    ClaimsStarted(ClaimStarted),
    /// Answers `ReleaseOutcome`.
    #[serde(rename = "claims.release")]
    ClaimsRelease(ClaimRelease),
    /// Ends another workspace's claim without stopping anything (L23).
    #[serde(rename = "claims.force_release")]
    ClaimsForceRelease {
        claim_id: String,
        confirmation: String,
        workspace_id: String,
        requester: ProcessIdentity,
    },
    #[serde(rename = "claims.withdraw")]
    ClaimsWithdraw { request_id: String, timed_out: bool },
    #[serde(rename = "claims.renew")]
    ClaimsRenew {
        claim_id: String,
        workspace_id: String,
    },
    #[serde(rename = "claims.set_command")]
    ClaimsSetCommand {
        claim_id: String,
        workspace_id: String,
        command: String,
    },
    #[serde(rename = "claims.reserve")]
    ClaimsReserve {
        resource: ResourceId,
        workspace_id: String,
    },
    #[serde(rename = "claims.unreserve")]
    ClaimsUnreserve {
        resource: ResourceId,
        workspace_id: String,
    },
    #[serde(rename = "claims.snapshot")]
    ClaimsSnapshot,
    #[serde(rename = "claims.history")]
    ClaimsHistory(HistoryQuery),
    #[serde(rename = "claims.seize_begin")]
    ClaimsSeizeBegin(SeizeBegin),
    #[serde(rename = "claims.seize_commit")]
    ClaimsSeizeCommit(SeizeCommit),
    #[serde(rename = "claims.seize_abort")]
    ClaimsSeizeAbort { plan_id: String },
    /// L29: answers `{ "expires_at_ms": … }`.
    #[serde(rename = "claims.seize_progress")]
    ClaimsSeizeProgress(SeizeProgress),
    #[serde(rename = "status.overview")]
    StatusOverview,
    #[serde(rename = "account.list")]
    AccountList { project: ProjectRef },
    #[serde(rename = "account.set")]
    AccountSet(AccountSet),
    #[serde(rename = "account.remove")]
    AccountRemove(AccountRemove),
    #[serde(rename = "account.check")]
    AccountCheck { project: ProjectRef, name: String },
    #[serde(rename = "account.materialize")]
    AccountMaterialize(AccountMaterialize),
    /// Answers `DevicesSnapshot`; unregistered virtual devices and unidentified transports only
    /// with `all`.
    #[serde(rename = "devices.list")]
    DevicesList { all: bool },
    /// Answers `DeviceDetail`. `fresh_sessions`: wait up to 6 s for a sessions read that started
    /// after the call (L49, P2-15).
    #[serde(rename = "devices.show")]
    DevicesShow {
        device: String,
        fresh_sessions: bool,
    },
    /// Answers `DeviceInfo` of a registered device, with its transport fresh.
    #[serde(rename = "devices.resolve")]
    DevicesResolve { device: String },
    /// `person`: set only by a CLI that passed P2's person prompt (L49); the service refuses a
    /// physical device's `add` / `rename` / `remove` without it.
    #[serde(rename = "devices.add")]
    DevicesAdd {
        device: String,
        alias: Option<String>,
        person: bool,
    },
    #[serde(rename = "devices.rename")]
    DevicesRename {
        device: String,
        alias: String,
        person: bool,
    },
    #[serde(rename = "devices.note")]
    DevicesNote {
        device: String,
        note: Option<String>,
    },
    #[serde(rename = "devices.remove")]
    DevicesRemove { device: String, person: bool },
    /// Answers `Vec<InstallEvent>`, newest first.
    #[serde(rename = "devices.installs")]
    DevicesInstalls {
        device: Option<String>,
        query: HistoryQuery,
    },
    /// Boots a virtual device for a live lease or hold of `workspace_id` (D10).
    #[serde(rename = "devices.boot")]
    DevicesBoot {
        resource: ResourceId,
        claim_id: String,
        workspace_id: String,
    },
    /// Answers `ToolStatus`.
    #[serde(rename = "devices.tools")]
    DevicesTools(ToolPathsSet),
    /// L54: person-only; releases one device from the agent-device sessions wait, stopping
    /// nothing. Refused without `person`. `by_workspace_path` and `by_pid` are the person's
    /// checkout and process, recorded with the forget (L54's "who").
    #[serde(rename = "devices.forget_sessions")]
    DevicesForgetSessions {
        device: String,
        person: bool,
        #[serde(default)]
        by_workspace_path: Option<String>,
        #[serde(default)]
        by_pid: Option<u32>,
    },
}

/// Methods whose params may carry a value (or a command that may hold one, L12): a frame that
/// fails to parse gets a fixed error text, never serde's (which quotes the input).
pub fn sensitive_request(method: &str) -> bool {
    matches!(
        method,
        "secrets.unlock"
            | "secret.set"
            | "secret.import"
            | "secret.materialize"
            | "account.set"
            | "account.materialize"
    )
}

/// Methods whose answer carries values: never parsed into a `serde_json::Value`.
pub fn sensitive_answer(method: &str) -> bool {
    matches!(method, "secret.materialize" | "account.materialize")
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
    #[serde(rename = "claims.changed")]
    ClaimsChanged(ClaimsSnapshot),
    #[serde(rename = "claims.seized")]
    ClaimsSeized(SeizureRecord),
    // Boxed: the frame enums stay small; the wire shape is the payload's own.
    #[serde(rename = "devices.changed")]
    DevicesChanged(Box<DevicesSnapshot>),
    #[serde(rename = "devices.installed")]
    DevicesInstalled(Box<InstallEvent>),
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
            Request::ServiceShutdown {
                if_idle: true,
                refuse_if_unlocked: false,
            },
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

        let shutdown = serde_json::to_value(Request::ServiceShutdown {
            if_idle: true,
            refuse_if_unlocked: false,
        })
        .unwrap();
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
            Request::ServiceShutdown {
                if_idle: true,
                refuse_if_unlocked: false,
            }
        );
    }

    /// P4's additive lifecycle fields (R1-6): a 0.0.3 `service.status` answer (no
    /// `secrets_unlocked`) reads `None`, a 0.0.3 `service.shutdown` (no `refuse_if_unlocked`)
    /// reads `false`, and 0.0.3's shapes still read the P4 frames (no `deny_unknown_fields`).
    #[test]
    fn lifecycle_additions_read_both_ways() {
        let old_status = json!({
            "service_version": "0.0.3",
            "contract_version": 4,
            "pid": 42,
            "running_heavy": 1,
            "waiting_heavy": 0,
        });
        let status: ServiceStatus = serde_json::from_value(old_status.clone()).unwrap();
        assert_eq!(status.secrets_unlocked, None);
        // `None` is not sent: a P4 service that cannot say answers the 0.0.3 shape.
        assert_eq!(serde_json::to_value(&status).unwrap(), old_status);
        for unlocked in [false, true] {
            let status = ServiceStatus {
                secrets_unlocked: Some(unlocked),
                ..status.clone()
            };
            let value = serde_json::to_value(&status).unwrap();
            assert_eq!(value["secrets_unlocked"], unlocked);
            assert_eq!(
                serde_json::from_value::<ServiceStatus>(value.clone()).unwrap(),
                status
            );
            // 0.0.3's `ServiceStatus` reads a P4 answer.
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct OldStatus {
                service_version: String,
                contract_version: u32,
                pid: u32,
                running_heavy: u32,
                waiting_heavy: u32,
            }
            assert!(serde_json::from_value::<OldStatus>(value).is_ok());
        }

        let old_shutdown = json!({"method": "service.shutdown", "params": {"if_idle": true}});
        assert_eq!(
            serde_json::from_value::<Request>(old_shutdown.clone()).unwrap(),
            Request::ServiceShutdown {
                if_idle: true,
                refuse_if_unlocked: false,
            }
        );
        // `false` is not sent: an explicit stop is the 0.0.3 frame, byte for byte.
        assert_eq!(
            serde_json::to_value(Request::ServiceShutdown {
                if_idle: true,
                refuse_if_unlocked: false,
            })
            .unwrap(),
            old_shutdown
        );
        let refusing = Request::ServiceShutdown {
            if_idle: true,
            refuse_if_unlocked: true,
        };
        let value = serde_json::to_value(&refusing).unwrap();
        assert_eq!(
            value,
            json!({"method": "service.shutdown",
                   "params": {"if_idle": true, "refuse_if_unlocked": true}})
        );
        assert_eq!(
            serde_json::from_value::<Request>(value.clone()).unwrap(),
            refusing
        );
        // 0.0.3's `Request` reads it and ignores the field.
        #[derive(Debug, PartialEq, Deserialize)]
        #[serde(tag = "method", content = "params")]
        enum OldRequest {
            #[serde(rename = "service.shutdown")]
            ServiceShutdown { if_idle: bool },
        }
        assert_eq!(
            serde_json::from_value::<OldRequest>(value).unwrap(),
            OldRequest::ServiceShutdown { if_idle: true }
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
            secrets_unlocked: None,
            boot_id: None,
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
    fn sensitive_requests_are_exactly_six() {
        let sensitive: Vec<String> = secret_requests()
            .iter()
            .chain(&all_requests())
            .chain(&claims_requests())
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
                "secret.materialize",
                "account.set",
                "account.materialize"
            ]
        );
        assert!(!sensitive_request("secret.setx"));
        assert!(!sensitive_request(""));
    }

    #[test]
    fn sensitive_answer_only_materialize() {
        for request in secret_requests()
            .iter()
            .chain(&all_requests())
            .chain(&claims_requests())
        {
            let method = serde_json::to_value(request).unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                sensitive_answer(&method),
                method == "secret.materialize" || method == "account.materialize",
                "{method}"
            );
        }
    }

    #[test]
    fn sensitive_lists_include_accounts() {
        assert!(sensitive_request("account.set"));
        assert!(sensitive_request("account.materialize"));
        assert!(sensitive_answer("account.materialize"));
        for plain in [
            "account.list",
            "account.check",
            "account.remove",
            "claims.acquire",
        ] {
            assert!(!sensitive_request(plain), "{plain}");
            assert!(!sensitive_answer(plain), "{plain}");
        }
    }

    fn claims_requests() -> Vec<Request> {
        use crate::contract::accounts::{AccountMaterialize, AccountRemove, AccountSet};
        use crate::contract::claims::fixtures;
        use crate::contract::claims::*;

        let screen = ResourceId::screen();
        vec![
            Request::WorkspaceResolve(WorkspaceResolve {
                project: project(),
                root: "/work/app".into(),
            }),
            Request::WorkspaceList,
            Request::ClaimsAcquire(fixtures::request()),
            Request::ClaimsAttach {
                request_id: "r1".into(),
            },
            Request::ClaimsStarted(ClaimStarted {
                claim_id: "c1".into(),
                child: identity(43),
                group: group(),
            }),
            Request::ClaimsRelease(ClaimRelease {
                claim_id: "c1".into(),
                workspace_id: "ws-1".into(),
                requester: identity(42),
                exit_code: Some(3),
            }),
            Request::ClaimsForceRelease {
                claim_id: "c1".into(),
                confirmation: "screen".into(),
                workspace_id: "ws-2".into(),
                requester: identity(77),
            },
            Request::ClaimsWithdraw {
                request_id: "r1".into(),
                timed_out: true,
            },
            Request::ClaimsRenew {
                claim_id: "h1".into(),
                workspace_id: "ws-1".into(),
            },
            Request::ClaimsSetCommand {
                claim_id: "c1".into(),
                workspace_id: "ws-1".into(),
                command: "login ***".into(),
            },
            Request::ClaimsReserve {
                resource: screen.clone(),
                workspace_id: "ws-1".into(),
            },
            Request::ClaimsUnreserve {
                resource: screen.clone(),
                workspace_id: "ws-1".into(),
            },
            Request::ClaimsSnapshot,
            Request::ClaimsHistory(HistoryQuery {
                since_ms: Some(1_000),
                limit: 50,
            }),
            Request::ClaimsSeizeBegin(SeizeBegin {
                resource: screen,
                confirmation: "screen".into(),
                request: fixtures::request(),
            }),
            Request::ClaimsSeizeCommit(SeizeCommit {
                plan_id: "p1".into(),
                requester: identity(77),
                evidence: vec![
                    StopEvidence::Gone { target: 0 },
                    StopEvidence::JobGone {
                        target: 1,
                        session: 2,
                    },
                ],
            }),
            Request::ClaimsSeizeAbort {
                plan_id: "p1".into(),
            },
            Request::StatusOverview,
            Request::AccountList { project: project() },
            Request::AccountSet(AccountSet {
                project: project(),
                name: "qa-user".into(),
                field: "session".into(),
                value: SecretValue::new("session-value-1".into()),
            }),
            Request::AccountRemove(AccountRemove {
                project: project(),
                name: "qa-user".into(),
                field: Some("env".into()),
            }),
            Request::AccountCheck {
                project: project(),
                name: "qa-user".into(),
            },
            Request::AccountMaterialize(AccountMaterialize {
                run_id: "run-1".into(),
                claim_id: "c1".into(),
                requester: identity(42),
                project: project(),
                name: "qa-user".into(),
                command: RawCommand::new("login ***".into()),
                workspace: "/work/app".into(),
            }),
        ]
    }

    #[test]
    fn claims_requests_round_trip() {
        let methods = [
            "workspace.resolve",
            "workspace.list",
            "claims.acquire",
            "claims.attach",
            "claims.started",
            "claims.release",
            "claims.force_release",
            "claims.withdraw",
            "claims.renew",
            "claims.set_command",
            "claims.reserve",
            "claims.unreserve",
            "claims.snapshot",
            "claims.history",
            "claims.seize_begin",
            "claims.seize_commit",
            "claims.seize_abort",
            "status.overview",
            "account.list",
            "account.set",
            "account.remove",
            "account.check",
            "account.materialize",
        ];
        let requests = claims_requests();
        assert_eq!(requests.len(), methods.len());
        for (request, method) in requests.into_iter().zip(methods) {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["method"], method);
            let back: Request = serde_json::from_value(value).unwrap();
            assert_eq!(back, request, "{method}");
        }
    }

    #[test]
    fn claims_events_round_trip() {
        use crate::contract::claims::fixtures;

        let events = [
            Event::ClaimsChanged(fixtures::snapshot()),
            Event::ClaimsSeized(fixtures::seizure()),
        ];
        for (event, kind) in events.into_iter().zip(["claims.changed", "claims.seized"]) {
            let frame = ServerFrame::Event { event };
            let value = serde_json::to_value(&frame).unwrap();
            assert_eq!(value["event"]["type"], kind);
            assert_eq!(serde_json::from_value::<ServerFrame>(value).unwrap(), frame);
        }
    }

    fn device_requests() -> Vec<Request> {
        use crate::contract::devices::fixtures;

        vec![
            Request::DevicesList { all: true },
            Request::DevicesShow {
                device: "pixel-9".into(),
                fresh_sessions: true,
            },
            Request::DevicesResolve {
                device: "emulator-5554".into(),
            },
            Request::DevicesAdd {
                device: "SERIAL0001".into(),
                alias: Some("pixel-9".into()),
                person: true,
            },
            Request::DevicesRename {
                device: "pixel-9".into(),
                alias: "pixel-9-left".into(),
                person: false,
            },
            Request::DevicesNote {
                device: "pixel-9".into(),
                note: None,
            },
            Request::DevicesRemove {
                device: "pixel-9".into(),
                person: true,
            },
            Request::DevicesInstalls {
                device: None,
                query: HistoryQuery {
                    since_ms: Some(1_000),
                    limit: 20,
                },
            },
            Request::DevicesBoot {
                resource: ResourceId("device:android:avd:pixel_9".into()),
                claim_id: "c1".into(),
                workspace_id: "ws-1".into(),
            },
            Request::DevicesTools(fixtures::tool_paths()),
            Request::DevicesForgetSessions {
                device: "pixel-9".into(),
                person: true,
                by_workspace_path: Some("/work/app".into()),
                by_pid: Some(42),
            },
        ]
    }

    #[test]
    fn device_requests_round_trip() {
        let methods = [
            "devices.list",
            "devices.show",
            "devices.resolve",
            "devices.add",
            "devices.rename",
            "devices.note",
            "devices.remove",
            "devices.installs",
            "devices.boot",
            "devices.tools",
            "devices.forget_sessions",
        ];
        let requests = device_requests();
        assert_eq!(requests.len(), methods.len());
        for (request, method) in requests.into_iter().zip(methods) {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["method"], method);
            assert!(!sensitive_request(method), "{method}");
            assert!(!sensitive_answer(method), "{method}");
            let frame = ClientFrame { id: 3, request };
            let line = serde_json::to_string(&frame).unwrap();
            let back: ClientFrame = serde_json::from_str(&line).unwrap();
            assert_eq!(back, frame, "{method}");
        }
    }

    #[test]
    fn device_events_round_trip() {
        use crate::contract::devices::fixtures;

        let events = [
            Event::DevicesChanged(Box::new(fixtures::snapshot())),
            Event::DevicesInstalled(Box::new(fixtures::install_event())),
        ];
        for (event, kind) in events
            .into_iter()
            .zip(["devices.changed", "devices.installed"])
        {
            let frame = ServerFrame::Event { event };
            let value = serde_json::to_value(&frame).unwrap();
            assert_eq!(value["event"]["type"], kind);
            assert_eq!(serde_json::from_value::<ServerFrame>(value).unwrap(), frame);
        }
        // The box is not on the wire: the data is the snapshot itself.
        let value =
            serde_json::to_value(Event::DevicesChanged(Box::new(fixtures::snapshot()))).unwrap();
        assert_eq!(
            value["data"],
            serde_json::to_value(fixtures::snapshot()).unwrap()
        );
    }

    #[test]
    fn devices_list_frame_matches_wire_shape() {
        let frame = ClientFrame {
            id: 7,
            request: Request::DevicesList { all: false },
        };
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({ "id": 7, "method": "devices.list", "params": { "all": false } })
        );
        let boot = ClientFrame {
            id: 8,
            request: Request::DevicesBoot {
                resource: ResourceId("device:android:avd:pixel_9".into()),
                claim_id: "c1".into(),
                workspace_id: "ws-1".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&boot).unwrap(),
            json!({
                "id": 8,
                "method": "devices.boot",
                "params": {
                    "resource": "device:android:avd:pixel_9",
                    "claim_id": "c1",
                    "workspace_id": "ws-1"
                }
            })
        );
        let tools = serde_json::to_value(Request::DevicesTools(ToolPathsSet::default())).unwrap();
        assert_eq!(
            tools,
            json!({
                "method": "devices.tools",
                "params": {
                    "adb": null,
                    "agent_device": null,
                    "node": null,
                    "detected_script": null,
                    "explicit_agent_device": false,
                    "android_env": {}
                }
            })
        );
    }

    #[test]
    fn claims_acquire_frame_matches_wire_shape() {
        let frame = ClientFrame {
            id: 1,
            request: Request::ClaimsAcquire(crate::contract::claims::fixtures::request()),
        };
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({
                "id": 1,
                "method": "claims.acquire",
                "params": {
                    "id": "6f1e6a9a-2d0f-4b7c-9a41-6f0f2f7c1a11",
                    "selector": {"by": "exact", "resource": "screen"},
                    "workspace_id": "ws-1",
                    "requester": {"pid": 42, "boot_id": "boot-1", "start_time": 500},
                    "purpose": {"kind": "lease", "command": "make test"},
                    "reentrant_tokens": ["c0"],
                    "no_wait": false
                }
            })
        );
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
            request: Request::ServiceShutdown {
                if_idle: true,
                refuse_if_unlocked: false,
            },
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
