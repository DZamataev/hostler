//! Contract types of devices: inventory, telemetry, installs, tools (plan P3b, Task 1).
//!
//! Every type here is part of contract v4; P4 (MCP) and P5 (the app) read them as they are.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::claims::ResourceId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Android,
    Ios,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Physical,
    Emulator,
    Simulator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Usb,
    Wifi,
    Emulator,
    Simulator,
    Wired,
    Network,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transport {
    pub kind: TransportKind,
    /// The adb serial, `ip:port`, `emulator-N`, or the CoreDevice id.
    pub address: String,
    /// `device`, `unauthorized`, `offline`, `connected`, …
    pub state: String,
}

/// One telemetry reading: `{"known": v}`, `"emulated"` or `"unknown"`; never guessed (D11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reading<T> {
    Known(T),
    Emulated,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Telemetry {
    pub battery_percent: Reading<u8>,
    pub charging: Reading<bool>,
    pub wifi: Reading<bool>,
    /// Shown in `devices list` / `show` / `--json`, never logged.
    pub wifi_name: Option<String>,
    pub cellular_data: Reading<bool>,
    pub vpn: Reading<bool>,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDeviceSession {
    pub session: String,
    pub workspace_path: String,
    pub workspace_id: Option<String>,
    pub seen_since_ms: i64,
    /// L51: from the last good read; agent-device's status is unreadable now.
    pub kept: bool,
    pub platform: Platform,
    /// For the exact `agent-device close` command.
    pub agent_device_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub resource: ResourceId,
    pub hardware_id: String,
    pub platform: Platform,
    pub kind: DeviceKind,
    pub registered: bool,
    pub alias: Option<String>,
    pub alias_by_person: bool,
    pub model: String,
    pub name: Option<String>,
    pub os_version: Option<String>,
    pub note: Option<String>,
    pub connected: bool,
    pub ready: bool,
    /// The current transport first.
    pub transports: Vec<Transport>,
    pub coredevice_id: Option<String>,
    pub telemetry: Option<Telemetry>,
    pub booted_by_hostler: bool,
    pub shutting_down: bool,
    pub removing: bool,
    pub sessions: Vec<AgentDeviceSession>,
    /// L52: not grantable to another workspace until a fresh sessions read.
    pub checking_sessions: bool,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnidentifiedTransport {
    pub platform: Platform,
    pub transport: Transport,
    pub model: Option<String>,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdbState {
    Ours,
    Foreign { version: u32 },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStatus {
    pub adb: Option<String>,
    pub adb_state: AdbState,
    pub emulator: Option<String>,
    pub xcrun: bool,
    pub ideviceinfo: Option<String>,
    /// agent-device's version.
    pub agent_device: Option<String>,
    pub node: Option<String>,
    pub agent_device_in_use: bool,
    pub sessions_known: bool,
    pub sessions_last_good_ms: Option<i64>,
    /// Why agent-device's status cannot be read now (`None`: it reads fine, or it is not in use).
    #[serde(default)]
    pub sessions_reason: Option<String>,
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicesSnapshot {
    pub devices: Vec<DeviceInfo>,
    pub unidentified: Vec<UnidentifiedTransport>,
    /// Sanitized one-liners.
    pub unmapped_sessions: Vec<String>,
    pub tools: ToolStatus,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallTime {
    Device { at_ms: i64 },
    FirstSeen { at_ms: i64 },
    ChangedBetween { from_ms: i64, to_ms: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    pub app_id: String,
    pub version_name: Option<String>,
    pub version_code: Option<String>,
    pub installed: Option<InstallTime>,
    pub updated: Option<InstallTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallChange {
    Installed,
    Updated,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallSource {
    Claim {
        claim_ids: Vec<String>,
        workspace_path: String,
        branch: Option<String>,
        commit: Option<String>,
        command: Option<String>,
    },
    Around {
        claim_ids: Vec<String>,
        workspace_path: String,
    },
    NotThroughHostler,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallEvent {
    pub id: String,
    pub device: ResourceId,
    pub alias: Option<String>,
    pub app_id: String,
    pub change: InstallChange,
    pub version_name: Option<String>,
    pub version_code: Option<String>,
    pub when: InstallTime,
    pub source: InstallSource,
    pub recorded_at_ms: i64,
}

/// `devices.show { fresh_sessions: true }` adds this key beside `DeviceDetail`'s fields when no
/// good sessions read that started after the call completed in time (CRB-4): its value says
/// why, and the sessions in `info` are only the last ones known. Absent otherwise; older
/// clients ignore it.
pub const SESSIONS_UNKNOWN_KEY: &str = "sessions_unknown";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDetail {
    pub info: DeviceInfo,
    pub apps: Vec<AppState>,
    pub snapshot_at_ms: Option<i64>,
    /// L54: the person's `forget-sessions` records, newest first.
    #[serde(default)]
    pub forgets: Vec<ForgetRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPathsSet {
    pub adb: Option<String>,
    pub agent_device: Option<String>,
    pub node: Option<String>,
    pub detected_script: Option<String>,
    pub explicit_agent_device: bool,
    /// `ANDROID_*` from `devices tools --detect` (P2-18).
    pub android_env: BTreeMap<String, String>,
}

/// `devices.forget_sessions`'s answer, as recorded in `device_session_forgets` (L54): a person
/// released `resource` from the agent-device sessions wait; nothing was stopped. `sessions`: the
/// kept sessions it dropped, sanitized; `reason`: why the device was closed (the status's
/// unreadable reason, or `checking`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetRecord {
    pub id: String,
    pub resource: ResourceId,
    pub sessions: Vec<String>,
    pub by_workspace_path: Option<String>,
    pub by_pid: Option<u32>,
    pub reason: String,
    pub at_ms: i64,
}

/// `^[a-z0-9][a-z0-9._-]{0,31}$`. Whether an alias looks like a device id is the registry's
/// check (P-14), not this one.
pub fn valid_alias(s: &str) -> bool {
    let bytes = s.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    let plain = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    plain(first)
        && rest.len() <= 31
        && rest
            .iter()
            .all(|b| plain(b) || matches!(b, b'.' | b'_' | b'-'))
}

/// The device errors with their exact texts (the guide quotes them).
pub mod errors {
    use serde_json::json;

    use crate::contract::{ErrorCode, HostlerError};

    fn error(code: ErrorCode, message: String, recovery: String) -> HostlerError {
        HostlerError {
            code,
            message,
            details: json!({}),
            recovery: Some(recovery),
        }
    }

    pub fn device_not_found(device: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::DeviceNotFound,
            format!("no device known to hostler matches {device}"),
            "hostler devices list --all".into(),
        );
        e.details = json!({ "device": device });
        e
    }

    /// `holder`: the alias's current device, as `devices rename` accepts it.
    pub fn alias_taken(alias: &str, holder: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::AliasTaken,
            format!("the alias {alias} is taken by {holder}"),
            format!("choose another alias, or: hostler devices rename {holder} <other>"),
        );
        e.details = json!({ "alias": alias, "holder": holder });
        e
    }

    /// L49: a physical device is registered by a person; agents may add virtual ones.
    pub fn device_not_registered(name: &str, physical: bool) -> HostlerError {
        let recovery = if physical {
            format!(
                "ask the operator to register it: hostler devices add {name} (a person types the \
                 model)"
            )
        } else {
            format!("hostler devices add {name}")
        };
        let mut e = error(
            ErrorCode::DeviceNotRegistered,
            format!("{name} is not a registered device"),
            recovery,
        );
        e.details = json!({ "device": name, "physical": physical });
        e
    }

    pub fn device_not_ready(alias: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::DeviceNotReady,
            format!("{alias} is not booted"),
            "rerun with --boot".into(),
        );
        e.details = json!({ "device": alias });
        e
    }

    pub fn boot_failed(alias: &str, why: &str, log_path: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::BootFailed,
            format!("{alias} did not boot: {why}"),
            format!("hostler devices show {alias}; emulator output: {log_path}"),
        );
        e.details = json!({ "device": alias, "log": log_path });
        e
    }

    /// The tools `ToolMissing` names.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MissingTool {
        Adb,
        AgentDevice,
    }

    pub fn tool_missing(tool: MissingTool) -> HostlerError {
        let (name, recovery) = match tool {
            MissingTool::Adb => ("adb", "hostler devices tools --detect, or set ANDROID_HOME"),
            MissingTool::AgentDevice => (
                "agent-device",
                "install it globally (npm i -g agent-device@0.21.36), or point at a project's \
                 copy: hostler devices tools --agent-device \
                 <checkout>/node_modules/agent-device/bin/agent-device.mjs",
            ),
        };
        let mut e = error(
            ErrorCode::ToolMissing,
            format!("hostler cannot find {name}"),
            recovery.into(),
        );
        e.details = json!({ "tool": name });
        e
    }

    /// L49, L54: a physical device's `add` / `rename` / `remove`, and every `forget-sessions`,
    /// pass P2's person prompt in the CLI first; the service refuses them without it.
    /// `command`: the command after `hostler devices`, e.g. `add usb:1-1`.
    pub fn person_required(command: &str) -> HostlerError {
        let mut e = error(
            ErrorCode::PermissionDenied,
            format!(
                "`hostler devices {command}` needs a person at a terminal. A person runs this; an \
                 agent asks the operator."
            ),
            format!("hostler devices {command}"),
        );
        e.details = json!({ "command": command });
        e
    }

    /// Task 1's stubs: the method or flag exists in contract v4, its body comes later.
    pub fn not_available_yet(what: &str) -> HostlerError {
        HostlerError {
            code: ErrorCode::BadRequest,
            message: format!("{what} is not available in this hostler build yet"),
            details: json!({ "not_available_yet": what }),
            recovery: None,
        }
    }
}

/// Values for the contract tests (here and in `wire.rs`).
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn resource() -> ResourceId {
        ResourceId("device:android:SERIAL0001".into())
    }

    pub fn telemetry() -> Telemetry {
        Telemetry {
            battery_percent: Reading::Known(86),
            charging: Reading::Known(true),
            wifi: Reading::Known(true),
            wifi_name: Some("lab".into()),
            cellular_data: Reading::Unknown,
            vpn: Reading::Emulated,
            at_ms: 5_000,
        }
    }

    pub fn info() -> DeviceInfo {
        DeviceInfo {
            resource: resource(),
            hardware_id: "SERIAL0001".into(),
            platform: Platform::Android,
            kind: DeviceKind::Physical,
            registered: true,
            alias: Some("pixel-9".into()),
            alias_by_person: true,
            model: "Pixel 9".into(),
            name: None,
            os_version: Some("15".into()),
            note: Some("on the left".into()),
            connected: true,
            ready: true,
            transports: vec![
                Transport {
                    kind: TransportKind::Usb,
                    address: "SERIAL0001".into(),
                    state: "device".into(),
                },
                Transport {
                    kind: TransportKind::Wifi,
                    address: "192.0.2.10:5555".into(),
                    state: "device".into(),
                },
            ],
            coredevice_id: None,
            telemetry: Some(telemetry()),
            booted_by_hostler: false,
            shutting_down: false,
            removing: false,
            sessions: vec![AgentDeviceSession {
                session: "default".into(),
                workspace_path: "/work/app".into(),
                workspace_id: Some("ws-1".into()),
                seen_since_ms: 4_000,
                kept: false,
                platform: Platform::Android,
                agent_device_id: "SERIAL0001".into(),
            }],
            checking_sessions: true,
            first_seen_ms: 1_000,
            last_seen_ms: 5_000,
        }
    }

    pub fn snapshot() -> DevicesSnapshot {
        DevicesSnapshot {
            devices: vec![info()],
            unidentified: vec![UnidentifiedTransport {
                platform: Platform::Android,
                transport: Transport {
                    kind: TransportKind::Usb,
                    address: "SERIAL0002".into(),
                    state: "unauthorized".into(),
                },
                model: None,
                hint: Some("accept the prompt on the phone".into()),
            }],
            unmapped_sessions: vec!["agent-device session s2 on an unknown device".into()],
            tools: ToolStatus {
                adb: Some("/sdk/platform-tools/adb".into()),
                adb_state: AdbState::Foreign { version: 41 },
                emulator: None,
                xcrun: false,
                ideviceinfo: None,
                agent_device: Some("0.21.36".into()),
                node: Some("/usr/local/bin/node".into()),
                agent_device_in_use: true,
                sessions_known: true,
                sessions_last_good_ms: Some(4_500),
                sessions_reason: None,
                problems: vec!["another adb (version 41) owns the adb server".into()],
            },
            at_ms: 5_000,
        }
    }

    pub fn install_event() -> InstallEvent {
        InstallEvent {
            id: "i1".into(),
            device: resource(),
            alias: Some("pixel-9".into()),
            app_id: "com.example.app".into(),
            change: InstallChange::Updated,
            version_name: Some("1.2.0".into()),
            version_code: Some("120".into()),
            when: InstallTime::Device { at_ms: 4_200 },
            source: InstallSource::Claim {
                claim_ids: vec!["c1".into()],
                workspace_path: "/work/app".into(),
                branch: Some("main".into()),
                commit: Some("abc123".into()),
                command: Some("make e2e".into()),
            },
            recorded_at_ms: 4_300,
        }
    }

    pub fn tool_paths() -> ToolPathsSet {
        let mut android_env = BTreeMap::new();
        android_env.insert("ANDROID_HOME".to_owned(), "/sdk".to_owned());
        ToolPathsSet {
            adb: Some("/sdk/platform-tools/adb".into()),
            agent_device: Some("/usr/local/bin/agent-device".into()),
            node: Some("/usr/local/bin/node".into()),
            detected_script: Some(
                "/usr/local/lib/node_modules/agent-device/bin/agent-device.mjs".into(),
            ),
            explicit_agent_device: false,
            android_env,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::fixtures::*;
    use super::*;
    use crate::contract::ErrorCode;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T) {
        let encoded = serde_json::to_value(&value).unwrap();
        assert_eq!(serde_json::from_value::<T>(encoded).unwrap(), value);
    }

    #[test]
    fn aliases_are_validated() {
        for ok in [
            "a",
            "0",
            "pixel-9",
            "sm-s937b",
            "iphone-17-pro",
            "pixel_9",
            "a.b",
        ] {
            assert!(valid_alias(ok), "{ok}");
        }
        assert!(valid_alias(&format!("a{}", "b".repeat(31))));
        for bad in [
            "",
            "-a",
            ".a",
            "_a",
            "Pixel",
            "a b",
            "a/b",
            "a:b",
            "émulateur",
            "a\n",
        ] {
            assert!(!valid_alias(bad), "{bad:?}");
        }
        assert!(!valid_alias(&format!("a{}", "b".repeat(32))));
    }

    #[test]
    fn readings_have_the_wire_shape() {
        assert_eq!(
            serde_json::to_value(Reading::Known(86u8)).unwrap(),
            json!({ "known": 86 })
        );
        assert_eq!(
            serde_json::to_value(Reading::<bool>::Emulated).unwrap(),
            json!("emulated")
        );
        assert_eq!(
            serde_json::to_value(Reading::<bool>::Unknown).unwrap(),
            json!("unknown")
        );
        assert_eq!(
            serde_json::to_value(InstallTime::ChangedBetween {
                from_ms: 1,
                to_ms: 2
            })
            .unwrap(),
            json!({ "kind": "changed_between", "from_ms": 1, "to_ms": 2 })
        );
        assert_eq!(
            serde_json::to_value(InstallSource::NotThroughHostler).unwrap(),
            json!({ "kind": "not_through_hostler" })
        );
        assert_eq!(
            serde_json::to_value(AdbState::Foreign { version: 41 }).unwrap(),
            json!({ "foreign": { "version": 41 } })
        );
        assert_eq!(serde_json::to_value(AdbState::Ours).unwrap(), json!("ours"));
        assert_eq!(
            serde_json::to_value(TransportKind::Wifi).unwrap(),
            json!("wifi")
        );
    }

    #[test]
    fn device_types_round_trip() {
        round_trip(snapshot());
        round_trip(install_event());
        round_trip(tool_paths());
        round_trip(DeviceDetail {
            info: info(),
            apps: vec![AppState {
                app_id: "com.example.app".into(),
                version_name: Some("1.2.0".into()),
                version_code: None,
                installed: Some(InstallTime::FirstSeen { at_ms: 1 }),
                updated: None,
            }],
            snapshot_at_ms: Some(9),
            forgets: vec![ForgetRecord {
                id: "f1".into(),
                resource: resource(),
                sessions: vec!["default".into()],
                by_workspace_path: Some("/work/app".into()),
                by_pid: Some(42),
                reason: "agent-device status unreadable: node not found".into(),
                at_ms: 6_000,
            }],
        });
        // An older service's detail carries no forgets: the field defaults to empty.
        let old = serde_json::json!({
            "info": info(),
            "apps": [],
            "snapshot_at_ms": null,
        });
        let detail: DeviceDetail = serde_json::from_value(old).unwrap();
        assert!(detail.forgets.is_empty());
        let old_tools = serde_json::json!({
            "adb": null,
            "adb_state": "missing",
            "emulator": null,
            "xcrun": false,
            "ideviceinfo": null,
            "agent_device": null,
            "node": null,
            "agent_device_in_use": false,
            "sessions_known": true,
            "sessions_last_good_ms": null,
            "problems": [],
        });
        let tools: ToolStatus = serde_json::from_value(old_tools).unwrap();
        assert_eq!(tools.sessions_reason, None);
        for source in [
            InstallSource::Around {
                claim_ids: vec!["c1".into(), "c2".into()],
                workspace_path: "/work/app".into(),
            },
            InstallSource::NotThroughHostler,
        ] {
            round_trip(source);
        }
        for kind in [DeviceKind::Emulator, DeviceKind::Simulator] {
            round_trip(kind);
        }
        round_trip(ForgetRecord {
            id: "f1".into(),
            resource: resource(),
            sessions: vec!["default".into()],
            by_workspace_path: Some("/work/app".into()),
            by_pid: Some(42),
            reason: "agent-device status unreadable: node not found".into(),
            at_ms: 6_000,
        });
        round_trip(Platform::Ios);
        round_trip(InstallChange::Removed);
        round_trip(AdbState::Missing);
    }

    #[test]
    fn error_texts_are_exact() {
        let e = errors::device_not_found("pixel");
        assert_eq!(e.code, ErrorCode::DeviceNotFound);
        assert_eq!(e.recovery.as_deref(), Some("hostler devices list --all"));

        let e = errors::alias_taken("pixel-9", "SERIAL0001");
        assert_eq!(e.code, ErrorCode::AliasTaken);
        assert_eq!(
            e.recovery.as_deref(),
            Some("choose another alias, or: hostler devices rename SERIAL0001 <other>")
        );

        let e = errors::device_not_registered("usb:1-1", true);
        assert_eq!(e.code, ErrorCode::DeviceNotRegistered);
        assert_eq!(
            e.recovery.as_deref(),
            Some(
                "ask the operator to register it: hostler devices add usb:1-1 (a person types \
                 the model)"
            )
        );
        let e = errors::device_not_registered("pixel_9", false);
        assert_eq!(e.recovery.as_deref(), Some("hostler devices add pixel_9"));

        let e = errors::device_not_ready("pixel_9");
        assert_eq!(e.code, ErrorCode::DeviceNotReady);
        assert_eq!(e.recovery.as_deref(), Some("rerun with --boot"));

        let e = errors::boot_failed("pixel_9", "no boot within 180 s", "/h/logs/devices/x.log");
        assert_eq!(e.code, ErrorCode::BootFailed);
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler devices show pixel_9; emulator output: /h/logs/devices/x.log")
        );

        let e = errors::tool_missing(errors::MissingTool::Adb);
        assert_eq!(e.code, ErrorCode::ToolMissing);
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler devices tools --detect, or set ANDROID_HOME")
        );
        let e = errors::tool_missing(errors::MissingTool::AgentDevice);
        assert_eq!(
            e.recovery.as_deref(),
            Some(
                "install it globally (npm i -g agent-device@0.21.36), or point at a project's \
                 copy: hostler devices tools --agent-device \
                 <checkout>/node_modules/agent-device/bin/agent-device.mjs"
            )
        );

        let e = errors::person_required("forget-sessions pixel-9");
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        assert_eq!(
            e.message,
            "`hostler devices forget-sessions pixel-9` needs a person at a terminal. A person \
             runs this; an agent asks the operator."
        );

        let e = errors::not_available_yet("devices.list");
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert_eq!(
            e.message,
            "devices.list is not available in this hostler build yet"
        );
    }

    #[test]
    fn device_error_codes_serialize_snake_case() {
        for (code, text) in [
            (ErrorCode::DeviceNotFound, "device_not_found"),
            (ErrorCode::AliasTaken, "alias_taken"),
            (ErrorCode::DeviceNotRegistered, "device_not_registered"),
            (ErrorCode::DeviceNotReady, "device_not_ready"),
            (ErrorCode::BootFailed, "boot_failed"),
            (ErrorCode::ToolMissing, "tool_missing"),
            (ErrorCode::PermissionDenied, "permission_denied"),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(text));
        }
    }
}
