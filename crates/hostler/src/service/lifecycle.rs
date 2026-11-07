//! The frozen lifecycle protocol: `hello`, `service.status`, `service.shutdown` (plan Task 4/6).
//! These answers must stay readable by every past and future client.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::contract::heavy::HeavyEntry;
use crate::contract::{CONTRACT_VERSION, ErrorCode, HostlerError};

pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RESTART_RECOVERY: &str = "hostler service restart";

/// The answer to `hello`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub contract_version: u32,
    pub service_version: String,
}

/// The contract version this service speaks. Debug builds honour `HOSTLER_TEST_CONTRACT_VERSION`
/// so tests can run a "different" service.
pub fn service_contract_version() -> u32 {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var("HOSTLER_TEST_CONTRACT_VERSION")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return v;
    }
    CONTRACT_VERSION
}

/// `hello`: the service's versions, or `ContractMismatch` when the client speaks another version.
pub fn hello(client_contract: u32, service_contract: u32) -> Result<HelloReply, HostlerError> {
    if client_contract == service_contract {
        return Ok(HelloReply {
            contract_version: service_contract,
            service_version: SERVICE_VERSION.into(),
        });
    }
    Err(HostlerError {
        code: ErrorCode::ContractMismatch,
        message: format!(
            "the running hostler service speaks contract {service_contract} \
             (hostler {SERVICE_VERSION}); this client speaks contract {client_contract}"
        ),
        details: json!({
            "client_contract": client_contract,
            "service_contract": service_contract,
            "service_version": SERVICE_VERSION,
        }),
        recovery: Some(RESTART_RECOVERY.into()),
    })
}

/// A versioned method before a matching `hello` on this connection.
pub fn hello_required(service_contract: u32) -> HostlerError {
    HostlerError {
        code: ErrorCode::ContractMismatch,
        message: format!(
            "this connection has no matching hello; the service speaks contract {service_contract}"
        ),
        details: json!({ "service_contract": service_contract, "service_version": SERVICE_VERSION }),
        recovery: Some(RESTART_RECOVERY.into()),
    }
}

/// `service.shutdown { if_idle: true }` while heavy entries run.
pub fn busy(running: &[HeavyEntry]) -> HostlerError {
    HostlerError {
        code: ErrorCode::ServiceBusy,
        message: format!(
            "{} heavy task(s) are running; stopping the service now would not stop them, but \
             their slots would be unguarded until it restarts",
            running.len()
        ),
        details: json!({ "running": running }),
        recovery: Some("hostler service stop --force".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_matches_or_reports_both_versions() {
        let ok = hello(1, 1).unwrap();
        assert_eq!(ok.contract_version, 1);
        let err = hello(1, 999).unwrap_err();
        assert_eq!(err.code, ErrorCode::ContractMismatch);
        assert_eq!(err.details["client_contract"], 1);
        assert_eq!(err.details["service_contract"], 999);
        assert_eq!(err.details["service_version"], SERVICE_VERSION);
        assert_eq!(err.recovery.as_deref(), Some("hostler service restart"));
    }
}
