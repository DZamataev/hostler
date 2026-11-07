//! The versioned contract shared by the CLI, the service, MCP and the app (spec §12, plan Task 4).

pub mod heavy;
pub mod wire;

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 1;

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
}
