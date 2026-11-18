//! The frozen lifecycle's answers that are not in `heavy.rs` (moved from `service::lifecycle`,
//! P5 Task 1, so every wire type lives in `contract/`). `ServiceStatus` stays in `heavy.rs`.

use serde::{Deserialize, Serialize};

/// The answer to `hello`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HelloReply {
    pub contract_version: u32,
    pub service_version: String,
    /// The service's boot id (additive, P4 Task 9 F-1 r1): a client that may not read its own
    /// learns it here (`procs::learn_boot_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    /// P5 (additive, spec §6.2): this service run's id, a UUID made at start; the server
    /// fills it. A client that sees it change reads every snapshot again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}
