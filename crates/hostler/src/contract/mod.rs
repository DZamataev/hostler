//! The versioned contract shared by the CLI (and its `--json`), the service and a future app
//! (spec §12, plan Task 4).

pub mod accounts;
pub mod claims;
pub mod devices;
pub mod doctor;
pub mod heavy;
pub mod init;
pub mod lifecycle;
pub mod methods;
pub mod process;
pub mod projects;
pub mod secrets;
#[cfg(test)]
mod swift;
pub mod values;
pub mod wire;

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// P5 (spec §8.5): a gated method without a live proof on this connection. Recovery `type
    /// the master password in the hostler app, or rerun the command at a terminal`.
    ProofRequired,
    /// P5 (spec §8.5): the service-wide limit on password checks. Details `{retry_after_ms}`;
    /// recovery `wait <n> s, then try again`.
    ProofLimited,
    /// P5 (spec §11.1): `heavy.stop` could not verify that the task is gone. Details `{pids}`;
    /// recovery `the task still runs; stop it from its terminal, or: hostler queue release <id>
    /// --force`.
    StopFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HostlerError {
    pub code: ErrorCode,
    pub message: String,
    pub details: serde_json::Value,
    pub recovery: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    fn contract_version_is_5() {
        assert_eq!(CONTRACT_VERSION, 5);
    }

    /// P5 Task 1: every wire type is defined under `contract/` (Task 3's generator reads only
    /// this module): no contract file names a type from `procs`, `secrets::value` or `service`.
    /// `process.rs` calls `procs::local_boot`, a function, to fill an empty boot id.
    #[test]
    fn wire_types_live_in_contract() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contract");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let text = text.replace(&format!("crate::{}::local_boot(", "procs"), "");
            // Built here, so this file's own text does not match.
            for module in ["procs::", "secrets::value", "service::"] {
                let outside = format!("crate::{module}");
                assert!(
                    !text.contains(&outside),
                    "{} names {outside}",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked >= 14, "{checked} files");
    }

    #[test]
    fn p5_error_codes_on_the_wire() {
        for (code, wire) in [
            (ErrorCode::ProofRequired, "proof_required"),
            (ErrorCode::ProofLimited, "proof_limited"),
            (ErrorCode::StopFailed, "stop_failed"),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<ErrorCode>(json!(wire)).unwrap(),
                code
            );
        }
    }
}
