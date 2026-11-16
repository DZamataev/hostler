//! The versioned contract shared by the CLI, the service, MCP and the app (spec §12, plan Task 4).

pub mod accounts;
pub mod claims;
pub mod devices;
pub mod heavy;
pub mod secrets;
pub mod wire;

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ServiceUnavailable,
    ContractMismatch,
    ServiceBusy,
    BadRequest,
    UnknownEntry,
    QueueTimeout,
    QueueBusy,
    Withdrawn,
    CapacityOutOfRange,
    ForceRequired,
    Internal,
    NotInteractive,
    NotInProject,
    ConfigInvalid,
    ProjectNotApproved,
    SecretsLocked,
    UnlockFailed,
    SecretsConflict,
    SecretMissing,
    SecretNotInProject,
    SecretExists,
    SecretTooShort,
    RequestTimeout,
    UnknownResource,
    ResourceInUse,
    ClaimNotHeld,
    SeizureFailed,
    AccountMissing,
    ClaimDeadlock,
    /// L33: refused because the run it comes from (or the claim itself) is being taken over;
    /// every wrapper treats it as "not running", never as a service fault.
    TakenOver,
    /// P3b: recovery `hostler devices list --all`.
    DeviceNotFound,
    /// P3b: recovery `choose another alias, or: hostler devices rename {holder} <other>`.
    AliasTaken,
    /// P3b, L49: a physical device is registered by a person, a virtual one by anyone.
    DeviceNotRegistered,
    /// P3b: recovery `rerun with --boot`.
    DeviceNotReady,
    /// P3b: recovery `hostler devices show {alias}; emulator output: {log path}`.
    BootFailed,
    /// P3b: adb or agent-device cannot be found; the recovery names the fix.
    ToolMissing,
    /// P3b, L49, L54: a person-only device call without the person prompt.
    PermissionDenied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostlerError {
    pub code: ErrorCode,
    pub message: String,
    pub details: serde_json::Value,
    pub recovery: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<HostlerError>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn envelope_error_shape() {
        let envelope: Envelope<serde_json::Value> = Envelope {
            ok: false,
            data: None,
            error: Some(HostlerError {
                code: ErrorCode::QueueTimeout,
                message: "gave up after 60 min in the heavy queue".into(),
                details: json!({}),
                recovery: Some("hostler queue".into()),
            }),
        };

        assert_eq!(
            serde_json::to_value(&envelope).unwrap(),
            json!({
                "ok": false,
                "data": null,
                "error": {
                    "code": "queue_timeout",
                    "message": "gave up after 60 min in the heavy queue",
                    "details": {},
                    "recovery": "hostler queue"
                }
            })
        );
    }

    #[test]
    fn contract_version_is_4() {
        assert_eq!(CONTRACT_VERSION, 4);
    }
}
