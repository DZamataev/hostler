//! The process identities on the wire (moved from `procs`, P5 Task 1, so every wire type lives
//! in `contract/`; `procs` re-exports them and keeps their behaviour). An empty `boot_id` read
//! here is filled with the local one (`procs::local_boot`).

use serde::{Deserialize, Serialize};

/// `pid`, `boot_id`, `start_time`. An **empty** `boot_id` means "this machine's current boot":
/// a client that may not read the boot id (an agent sandbox denies `sysctl
/// kern.bootsessionuuid`, P4 Task 9 F-1) and has not yet learned it from the service sends it
/// empty, since a client on the service's local socket always runs in the service's boot. The
/// service fills in its own boot id as it reads the identity (`set_local_boot_id`), so every
/// identity it keeps, reaps or binds has a real one; a client learns the service's from `hello`
/// or `service.status` (`learn_boot_id`) before it persists or compares anything. An empty boot
/// id never reaches a liveness comparison (`same_boot`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProcessIdentity {
    pub pid: u32,
    pub boot_id: String,
    pub start_time: u64,
}

impl<'de> Deserialize<'de> for ProcessIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            pid: u32,
            #[serde(default)]
            boot_id: String,
            start_time: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(ProcessIdentity {
            pid: wire.pid,
            boot_id: crate::procs::local_boot(wire.boot_id),
            start_time: wire.start_time,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunGroup {
    UnixGroup {
        pgid: u32,
        boot_id: String,
    },
    WindowsJob {
        name: String,
        boot_id: String,
        /// The Windows logon session the job was made in (P3a Task 6); absent in P1-era JSON.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<u32>,
    },
}

impl<'de> Deserialize<'de> for RunGroup {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire {
            UnixGroup {
                pgid: u32,
                #[serde(default)]
                boot_id: String,
            },
            WindowsJob {
                name: String,
                #[serde(default)]
                boot_id: String,
                #[serde(default)]
                session: Option<u32>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::UnixGroup { pgid, boot_id } => RunGroup::UnixGroup {
                pgid,
                boot_id: crate::procs::local_boot(boot_id),
            },
            Wire::WindowsJob {
                name,
                boot_id,
                session,
            } => RunGroup::WindowsJob {
                name,
                boot_id: crate::procs::local_boot(boot_id),
                session,
            },
        })
    }
}
