//! iOS parsers: devicectl, simctl, ideviceinfo (P3b Task 4).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::contract::claims::ResourceId;
use crate::contract::devices::{AppState, Reading, Telemetry, TransportKind};

/// A physical iPhone, as `xcrun devicectl list devices --json-output` reports it. Its identity is
/// the hardware UDID, never the CoreDevice `identifier` (D2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalIos {
    pub udid: String,
    pub coredevice_id: String,
    pub name: String,
    pub model: String,
    pub os_version: Option<String>,
    pub transport: TransportKind,
    pub reachable: bool,
    pub paired: bool,
}

/// A simulator, as `xcrun simctl list devices --json` reports it. `last_booted_at` is part of its
/// boot identity (Task 7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Simulator {
    pub udid: String,
    pub name: String,
    pub runtime: String,
    pub booted: bool,
    pub last_booted_at: Option<String>,
}

#[derive(Deserialize)]
struct DevicectlList {
    info: Option<DevicectlInfo>,
    result: Option<DevicectlListResult>,
}

#[derive(Deserialize)]
struct DevicectlInfo {
    outcome: Option<String>,
}

fn succeeded(info: Option<DevicectlInfo>) -> bool {
    info.and_then(|info| info.outcome).as_deref() == Some("success")
}

#[derive(Deserialize)]
struct DevicectlListResult {
    devices: Option<Vec<DevicectlDevice>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevicectlDevice {
    identifier: Option<String>,
    connection_properties: Option<ConnectionProperties>,
    device_properties: Option<DeviceProperties>,
    hardware_properties: Option<HardwareProperties>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ConnectionProperties {
    transport_type: Option<String>,
    tunnel_state: Option<String>,
    pairing_state: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DeviceProperties {
    name: Option<String>,
    os_version_number: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HardwareProperties {
    udid: Option<String>,
    marketing_name: Option<String>,
    product_type: Option<String>,
    device_type: Option<String>,
    platform: Option<String>,
}

#[derive(Deserialize)]
struct DevicectlApps {
    info: Option<DevicectlInfo>,
    result: Option<DevicectlAppsResult>,
}

#[derive(Deserialize)]
struct DevicectlAppsResult {
    apps: Option<Vec<DevicectlApp>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevicectlApp {
    bundle_identifier: Option<String>,
    bundle_version: Option<String>,
    version: Option<String>,
    removable: Option<bool>,
    built_by_developer: Option<bool>,
}

#[derive(Deserialize)]
struct SimctlList {
    devices: Option<BTreeMap<String, Vec<SimctlDevice>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimctlDevice {
    udid: Option<String>,
    name: Option<String>,
    state: Option<String>,
    is_available: Option<bool>,
    last_booted_at: Option<String>,
}

#[derive(Deserialize)]
struct SimctlApp {
    #[serde(rename = "CFBundleIdentifier")]
    bundle_identifier: Option<String>,
    #[serde(rename = "CFBundleVersion")]
    bundle_version: Option<String>,
    #[serde(rename = "ApplicationType")]
    application_type: Option<String>,
    #[serde(rename = "Path")]
    path: Option<String>,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// `None` for broken or truncated JSON, never an empty listing: an unreadable listing must not
/// look like every app was removed.
pub fn parse_devicectl_list(json: &[u8]) -> Option<Vec<PhysicalIos>> {
    let list: DevicectlList = serde_json::from_slice(json).ok()?;
    if !succeeded(list.info) {
        return None;
    }
    let mut phones = Vec::new();
    for device in list.result?.devices? {
        let hardware = device.hardware_properties.unwrap_or_default();
        let ios = match hardware.platform.as_deref() {
            Some(platform) => platform == "iOS",
            None => matches!(
                hardware.device_type.as_deref(),
                Some("iPhone" | "iPad" | "iPod")
            ),
        };
        if !ios {
            continue;
        }
        let Some(udid) = non_empty(hardware.udid) else {
            continue;
        };
        let props = device.device_properties.unwrap_or_default();
        let connection = device.connection_properties.unwrap_or_default();
        phones.push(PhysicalIos {
            udid,
            coredevice_id: non_empty(device.identifier).unwrap_or_default(),
            name: non_empty(props.name).unwrap_or_default(),
            model: non_empty(hardware.marketing_name)
                .or_else(|| non_empty(hardware.product_type))
                .or_else(|| non_empty(hardware.device_type))
                .unwrap_or_default(),
            os_version: non_empty(props.os_version_number),
            transport: if connection.transport_type.as_deref() == Some("wired") {
                TransportKind::Wired
            } else {
                TransportKind::Network
            },
            reachable: connection.tunnel_state.as_deref() == Some("connected"),
            paired: connection.pairing_state.as_deref() == Some("paired"),
        });
    }
    Some(phones)
}

/// devicectl gives versions and codes, but no install times (D12).
pub fn parse_devicectl_apps(json: &[u8]) -> Option<Vec<AppState>> {
    let list: DevicectlApps = serde_json::from_slice(json).ok()?;
    if !succeeded(list.info) {
        return None;
    }
    let mut apps = Vec::new();
    for app in list.result?.apps? {
        if app.removable != Some(true) && app.built_by_developer != Some(true) {
            continue;
        }
        let Some(app_id) = non_empty(app.bundle_identifier) else {
            continue;
        };
        apps.push(AppState {
            app_id,
            version_name: non_empty(app.version),
            version_code: non_empty(app.bundle_version),
            installed: None,
            updated: None,
        });
    }
    Some(apps)
}

/// Only available simulators are resources; `last_booted_at` is kept for the boot identity.
pub fn parse_simctl_list(json: &[u8]) -> Option<Vec<Simulator>> {
    let list: SimctlList = serde_json::from_slice(json).ok()?;
    let mut sims = Vec::new();
    for (runtime, entries) in list.devices? {
        if !runtime.contains(".SimRuntime.iOS-") {
            continue;
        }
        for entry in entries {
            if entry.is_available == Some(false) {
                continue;
            }
            let Some(udid) = non_empty(entry.udid) else {
                continue;
            };
            sims.push(Simulator {
                udid,
                name: non_empty(entry.name).unwrap_or_default(),
                runtime: runtime.clone(),
                booted: entry.state.as_deref() == Some("Booted"),
                last_booted_at: non_empty(entry.last_booted_at),
            });
        }
    }
    Some(sims)
}

/// `simctl listapps`, already converted from the OpenStep plist. Only `User` apps count, and there
/// is no version name and no install time (verified).
pub fn parse_simctl_listapps(json: &[u8]) -> Option<Vec<(AppState, Option<PathBuf>)>> {
    let apps: BTreeMap<String, SimctlApp> = serde_json::from_slice(json).ok()?;
    let mut out = Vec::new();
    let mut saw_system = false;
    for (key, app) in apps {
        if app.application_type.as_deref() == Some("System") {
            saw_system = true;
        }
        if app.application_type.as_deref() != Some("User") {
            continue;
        }
        let app_id = non_empty(app.bundle_identifier).unwrap_or(key);
        out.push((
            AppState {
                app_id,
                version_name: None,
                version_code: non_empty(app.bundle_version),
                installed: None,
                updated: None,
            },
            non_empty(app.path).map(PathBuf::from),
        ));
    }
    // A booted simulator always lists its System apps; none means a failed or half-written read.
    saw_system.then_some(out)
}

/// The battery domain of `ideviceinfo`. Network is never reported for iOS: those stay unknown.
pub fn parse_ideviceinfo_battery(out: &str, now_ms: i64) -> Telemetry {
    let battery_percent = match field(out, "BatteryCurrentCapacity")
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|percent| *percent <= 100)
    {
        Some(percent) => Reading::Known(percent),
        None => Reading::Unknown,
    };
    let charging = match (
        field(out, "ExternalConnected"),
        field(out, "BatteryIsCharging"),
    ) {
        (Some("true"), _) | (_, Some("true")) => Reading::Known(true),
        (Some("false"), Some("false")) => Reading::Known(false),
        _ => Reading::Unknown,
    };
    Telemetry {
        battery_percent,
        charging,
        wifi: Reading::Unknown,
        wifi_name: None,
        cellular_data: Reading::Unknown,
        vpn: Reading::Unknown,
        at_ms: now_ms,
    }
}

fn field<'a>(out: &'a str, key: &str) -> Option<&'a str> {
    out.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.trim() == key).then(|| value.trim())
    })
}

pub fn resource_of_physical(p: &PhysicalIos) -> ResourceId {
    ResourceId(format!("device:ios:{}", p.udid))
}

pub fn resource_of_simulator(s: &Simulator) -> ResourceId {
    ResourceId(format!("device:ios:{}", s.udid))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::contract::claims::ResourceId;
    use crate::contract::devices::{Reading, Telemetry, TransportKind};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/devices")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn devicectl_udid_is_not_the_identifier() {
        let phones = parse_devicectl_list(&fixture("devicectl-list.json")).unwrap();
        assert_eq!(phones.len(), 2);
        let first = &phones[0];
        assert_eq!(first.udid, "00008101-000A00000000000E");
        assert_eq!(first.coredevice_id, "00000000-0000-4000-8000-0000000000C0");
        assert_ne!(first.udid, first.coredevice_id);
        assert_eq!(first.name, "Test iPhone 0");
        assert_eq!(first.model, "iPhone 13");
        assert_eq!(first.os_version.as_deref(), Some("18.7.8"));
        assert_eq!(
            resource_of_physical(first),
            ResourceId("device:ios:00008101-000A00000000000E".into())
        );
        assert_eq!(phones[1].udid, "00008101-000A00000000001E");
        assert_eq!(phones[1].model, "iPhone 12 Pro");
    }

    #[test]
    fn wired_and_network_transports() {
        let phones = parse_devicectl_list(&fixture("devicectl-list.json")).unwrap();
        assert_eq!(phones[0].transport, TransportKind::Network);
        assert_eq!(phones[1].transport, TransportKind::Wired);
    }

    #[test]
    fn unreachable_network_phone_is_not_reachable() {
        let phones = parse_devicectl_list(&fixture("devicectl-list.json")).unwrap();
        assert!(!phones[0].reachable, "localNetwork, tunnel disconnected");
        assert!(phones[1].reachable, "wired, tunnel connected");
    }

    #[test]
    fn pairing_state_parses() {
        let phones = parse_devicectl_list(&fixture("devicectl-list.json")).unwrap();
        assert!(phones.iter().all(|p| p.paired));

        let unpaired = br#"{"info":{"outcome":"success"},"result":{"devices":[{
            "identifier":"CORE-2",
            "connectionProperties":{"pairingState":"unpaired","tunnelState":"connected",
                "transportType":"wired"},
            "deviceProperties":{"name":"Test iPhone","osVersionNumber":"26.0"},
            "hardwareProperties":{"udid":"00000000-0000-4000-8000-0000000000E0",
                "marketingName":"iPhone 17","platform":"iOS"}
        }]}}"#;
        let phones = parse_devicectl_list(unpaired).unwrap();
        assert_eq!(phones.len(), 1);
        assert!(!phones[0].paired);
        assert!(phones[0].reachable);
        assert_eq!(phones[0].model, "iPhone 17");
    }

    #[test]
    fn devicectl_apps_have_versions_and_no_times() {
        let apps = parse_devicectl_apps(&fixture("devicectl-apps.json")).unwrap();
        assert_eq!(apps.len(), 3);
        assert_eq!(apps[0].app_id, "com.example.ios0");
        assert_eq!(apps[0].version_name.as_deref(), Some("1.0"));
        assert_eq!(apps[0].version_code.as_deref(), Some("1"));
        assert_eq!(apps[1].version_name.as_deref(), Some("1.2.4"));
        assert_eq!(apps[1].version_code.as_deref(), Some("2415643"));
        assert_eq!(apps[2].version_name.as_deref(), Some("4.12.1"));
        assert_eq!(apps[2].version_code.as_deref(), Some("2395694"));
        for app in &apps {
            assert_eq!(app.installed, None);
            assert_eq!(app.updated, None);
        }

        let filtered = br#"{"info":{"outcome":"success"},"result":{"apps":[
            {"bundleIdentifier":"com.example.keep","bundleVersion":"7","version":"1.7",
             "removable":true,"builtByDeveloper":false},
            {"bundleIdentifier":"com.example.dev","bundleVersion":"8","version":"1.8",
             "removable":false,"builtByDeveloper":true},
            {"bundleIdentifier":"com.example.system","bundleVersion":"9","version":"1.9",
             "removable":false,"builtByDeveloper":false},
            {"bundleIdentifier":"com.example.nover","removable":true}
        ]}}"#;
        let apps = parse_devicectl_apps(filtered).unwrap();
        let ids: Vec<&str> = apps.iter().map(|a| a.app_id.as_str()).collect();
        assert_eq!(
            ids,
            ["com.example.keep", "com.example.dev", "com.example.nover"]
        );
        assert_eq!(apps[2].version_name, None);
        assert_eq!(apps[2].version_code, None);
    }

    #[test]
    fn simctl_lists_available_simulators_with_state() {
        let sims = parse_simctl_list(&fixture("simctl-list-devices.json")).unwrap();
        assert_eq!(sims.len(), 4);
        assert_eq!(sims[0].udid, "00000000-0000-4000-8000-0000000000D0");
        assert_eq!(sims[0].name, "iPhone 17 Pro");
        assert_eq!(
            sims[0].runtime,
            "com.apple.CoreSimulator.SimRuntime.iOS-26-5"
        );
        assert!(sims[0].booted);
        assert_eq!(
            sims[0].last_booted_at.as_deref(),
            Some("2026-11-05T13:18:37Z")
        );
        assert!(!sims[1].booted);
        assert_eq!(
            sims[1].last_booted_at.as_deref(),
            Some("2026-10-28T12:08:21Z")
        );
        assert_eq!(
            resource_of_simulator(&sims[0]),
            ResourceId("device:ios:00000000-0000-4000-8000-0000000000D0".into())
        );

        let unavailable = br#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-26-5":[
            {"udid":"UNAVAILABLE","name":"Hidden","isAvailable":false,"state":"Shutdown"},
            {"udid":"AVAILABLE","name":"Shown","isAvailable":true,"state":"Booted"}
        ]}}"#;
        let sims = parse_simctl_list(unavailable).unwrap();
        assert_eq!(sims.len(), 1);
        assert_eq!(sims[0].udid, "AVAILABLE");
    }

    #[test]
    fn listapps_keeps_user_apps_only() {
        let apps = parse_simctl_listapps(&fixture("simctl-listapps.json")).unwrap();
        assert_eq!(apps.len(), 4);
        let ids: Vec<&str> = apps.iter().map(|(a, _)| a.app_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "com.example.sim1",
                "com.example.sim2",
                "com.example.sim3",
                "com.example.sim4"
            ]
        );
        for (app, _) in &apps {
            assert_eq!(app.version_name, None, "listapps has no version name");
            assert_eq!(app.installed, None);
            assert_eq!(app.updated, None);
        }
        assert_eq!(apps[0].0.version_code.as_deref(), Some("2395694"));
        assert_eq!(
            apps[0].1.as_deref(),
            Some(Path::new(
                "/Users/dev/Library/Developer/CoreSimulator/Devices/\
                 00000000-0000-4000-8000-00000000000A/data/Containers/Bundle/Application/\
                 00000000-0000-4000-8000-00000000000A/Example1.app"
            ))
        );
        assert_eq!(
            apps[1].1.as_deref().and_then(Path::file_name),
            Some(std::ffi::OsStr::new("AgentDeviceRunnerUITests-Runner.app"))
        );
    }

    #[test]
    fn ideviceinfo_battery_parses() {
        // L56: on external power reads as charging even when the battery is full.
        let text = String::from_utf8(fixture("ideviceinfo-battery.txt")).unwrap();
        assert_eq!(
            parse_ideviceinfo_battery(&text, 7_000),
            Telemetry {
                battery_percent: Reading::Known(100),
                charging: Reading::Known(true),
                wifi: Reading::Unknown,
                wifi_name: None,
                cellular_data: Reading::Unknown,
                vpn: Reading::Unknown,
                at_ms: 7_000,
            }
        );
        assert_eq!(
            parse_ideviceinfo_battery("BatteryIsCharging: true\n", 1).charging,
            Reading::Known(true)
        );
        assert_eq!(
            parse_ideviceinfo_battery("ExternalConnected: false\nBatteryIsCharging: false\n", 1)
                .charging,
            Reading::Known(false)
        );

        let empty = parse_ideviceinfo_battery("", 9_000);
        assert_eq!(empty.battery_percent, Reading::Unknown);
        assert_eq!(empty.charging, Reading::Unknown);
        assert_eq!(empty.at_ms, 9_000);
        let odd =
            parse_ideviceinfo_battery("BatteryCurrentCapacity: nope\nBatteryIsCharging: 1\n", 1);
        assert_eq!(odd.battery_percent, Reading::Unknown);
        assert_eq!(odd.charging, Reading::Unknown);
    }

    #[test]
    fn non_ios_runtimes_and_devices_are_skipped() {
        let list = br#"{"info":{"outcome":"success"},"result":{"devices":[
            {"identifier":"WATCH","hardwareProperties":{"udid":"WATCH-UDID","platform":"watchOS",
                "deviceType":"Watch"},"deviceProperties":{"name":"Test Watch"}},
            {"identifier":"TV","hardwareProperties":{"udid":"TV-UDID","platform":"tvOS",
                "deviceType":"Apple TV"},"deviceProperties":{"name":"Test TV"}},
            {"identifier":"VISION","hardwareProperties":{"udid":"VISION-UDID","platform":"xrOS",
                "deviceType":"Vision"},"deviceProperties":{"name":"Test Vision"}},
            {"identifier":"IPAD","hardwareProperties":{"udid":"IPAD-UDID","deviceType":"iPad"},
                "deviceProperties":{"name":"Test iPad"}},
            {"identifier":"IPHONE","hardwareProperties":{"udid":"IPHONE-UDID","platform":"iOS",
                "deviceType":"iPhone"},"deviceProperties":{"name":"Test iPhone"}}
        ]}}"#;
        let phones = parse_devicectl_list(list).unwrap();
        let udids: Vec<&str> = phones.iter().map(|p| p.udid.as_str()).collect();
        assert_eq!(udids, ["IPAD-UDID", "IPHONE-UDID"]);

        let sims = br#"{"devices":{
            "com.apple.CoreSimulator.SimRuntime.watchOS-26-0":[
                {"udid":"W1","name":"Watch","isAvailable":true,"state":"Booted"}],
            "com.apple.CoreSimulator.SimRuntime.tvOS-26-0":[
                {"udid":"T1","name":"TV","isAvailable":true,"state":"Shutdown"}],
            "com.apple.CoreSimulator.SimRuntime.xrOS-26-0":[
                {"udid":"V1","name":"Vision","isAvailable":true,"state":"Shutdown"}],
            "com.apple.CoreSimulator.SimRuntime.iOS-26-5":[
                {"udid":"I1","name":"iPhone","isAvailable":true,"state":"Booted",
                 "lastBootedAt":"2026-11-05T13:18:37Z"}]
        }}"#;
        let sims = parse_simctl_list(sims).unwrap();
        assert_eq!(sims.len(), 1);
        assert_eq!(sims[0].udid, "I1");
    }

    #[test]
    fn failed_devicectl_outcome_is_unreadable() {
        for name in ["devicectl-list.json", "devicectl-apps.json"] {
            let text = String::from_utf8(fixture(name)).unwrap();
            assert!(text.contains("\"outcome\": \"success\""), "{name}");
            let failed = text.replace("\"outcome\": \"success\"", "\"outcome\": \"failed\"");
            let parsed = if name == "devicectl-list.json" {
                parse_devicectl_list(failed.as_bytes()).is_none()
            } else {
                parse_devicectl_apps(failed.as_bytes()).is_none()
            };
            assert!(parsed, "{name} with a failed outcome must be unreadable");
        }
    }

    #[test]
    fn broken_json_is_none() {
        let bad: &[&[u8]] = &[b"{", b"", b"[]", b"{}"];
        for bytes in bad {
            assert!(parse_devicectl_list(bytes).is_none(), "{bytes:?}");
            assert!(parse_devicectl_apps(bytes).is_none(), "{bytes:?}");
            assert!(parse_simctl_list(bytes).is_none(), "{bytes:?}");
            assert!(parse_simctl_listapps(bytes).is_none(), "{bytes:?}");
        }
        let user_only = br#"{"com.example.sim1":{"CFBundleIdentifier":"com.example.sim1",
            "ApplicationType":"User","CFBundleVersion":"1"}}"#;
        assert!(
            parse_simctl_listapps(user_only).is_none(),
            "no System app means a failed listing, not an empty one"
        );
        for name in [
            "devicectl-list.json",
            "devicectl-apps.json",
            "simctl-list-devices.json",
            "simctl-listapps.json",
        ] {
            let bytes = fixture(name);
            let truncated = &bytes[..bytes.len() / 2];
            let parsed = match name {
                "devicectl-list.json" => parse_devicectl_list(truncated).is_none(),
                "devicectl-apps.json" => parse_devicectl_apps(truncated).is_none(),
                "simctl-list-devices.json" => parse_simctl_list(truncated).is_none(),
                _ => parse_simctl_listapps(truncated).is_none(),
            };
            assert!(parsed, "{name} truncated must be unreadable, not empty");
        }
    }

    #[test]
    fn fixtures_are_sanitized() {
        let files =
            files_under(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/devices"));
        assert!(files.len() >= 5, "only {} fixture files found", files.len());
        for path in files {
            let name = path.display();
            let text = std::fs::read_to_string(&path).unwrap();
            for uuid in uuids(&text) {
                assert!(
                    uuid.to_ascii_lowercase()
                        .starts_with("00000000-0000-4000-8000-"),
                    "{name}: UUID {uuid} is not an allowlisted fake id"
                );
            }
            for ip in ipv4s(&text) {
                assert!(
                    ipv4_allowed(&ip),
                    "{name}: IPv4 {ip} is not a documentation, loopback or placeholder address"
                );
            }
            for ip in ipv6s(&text) {
                assert!(
                    ipv6_allowed(&ip),
                    "{name}: IPv6 {ip} is outside 2001:db8::/32 and loopback"
                );
            }
            for mac in macs(&text) {
                assert!(
                    mac_allowed(&mac),
                    "{name}: MAC {mac} is not a 02:00:… placeholder"
                );
            }
            assert!(!text.contains("QrData"), "{name}: QrData");
            assert!(!text.contains("FirstUseDate"), "{name}: FirstUseDate");
            if let Some(host) = local_hosts(&text).first() {
                panic!("{name}: hostname {host} ends in .local");
            }
            for hash in cwd_hashes(&text) {
                assert!(
                    cwd_hash_allowed(&hash),
                    "{name}: agent-device workspace hash {hash} is not the fake value"
                );
            }
            let lower = text.to_ascii_lowercase();
            for app in ["looky", "pulsemessenger", "devicekit"] {
                assert!(!lower.contains(app), "{name}: owner app name {app}");
            }
        }
    }

    #[test]
    fn fixture_check_allowlists() {
        assert!(ipv4_allowed("10.0.0.7"));
        assert!(!ipv4_allowed("10.0.0.8"), "not all of 10.0.0.0/8");
        assert!(ipv4_allowed("192.0.2.10"));
        assert!(ipv4_allowed("127.0.0.1"));
        assert!(!ipv4_allowed("8.8.8.8"));
        assert!(ipv6_allowed("2001:db8::1"));
        assert!(ipv6_allowed("::1"));
        assert!(!ipv6_allowed("fdb7:7c06:6c51::1"));
        assert!(mac_allowed("02:00:00:00:00:00"));
        assert!(!mac_allowed("a4:83:e7:00:00:00"));
        assert_eq!(macs("BSSID: 02:00:00:00:00:00,"), ["02:00:00:00:00:00"]);
        assert_eq!(
            ipv6s("tunnel 2001:db8::1 and 13:18:37.000"),
            ["2001:db8::1"]
        );
        assert!(cwd_hash_allowed("cwd:0000000000000000"));
        assert!(!cwd_hash_allowed("cwd:2b2203d14065bfdf"));
        assert_eq!(
            cwd_hashes("session cwd:2b2203d14065bfdf:default"),
            ["cwd:2b2203d14065bfdf"]
        );
        assert_eq!(cwd_hashes("cwd:2b2203d14065bfdf00").len(), 0);
    }

    /// agent-device hashes the owner's workspace path into `cwd:<16 hex>`; only the fake value
    /// may appear in a fixture.
    fn cwd_hash_allowed(hash: &str) -> bool {
        hash == "cwd:0000000000000000"
    }

    fn cwd_hashes(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(pos) = text[from..].find("cwd:") {
            let start = from + pos;
            let digits = start + 4;
            let end = digits + 16;
            if end <= text.len()
                && text[digits..end].bytes().all(|b| b.is_ascii_hexdigit())
                && !text[end..].starts_with(|c: char| c.is_ascii_hexdigit())
            {
                out.push(text[start..end].to_owned());
            }
            from = start + 4;
        }
        out
    }

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut dirs = vec![dir.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    fn uuids(text: &str) -> Vec<String> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 36 <= bytes.len() {
            let shape = bytes[i..i + 36].iter().enumerate().all(|(j, byte)| {
                if matches!(j, 8 | 13 | 18 | 23) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            });
            if shape {
                out.push(text[i..i + 36].to_owned());
                i += 36;
            } else {
                i += 1;
            }
        }
        out
    }

    fn ipv4s(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() == 4
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.len() <= 3 && part.parse::<u8>().is_ok())
            {
                out.push(token.to_owned());
            }
        }
        out
    }

    /// `10.0.0.7` is the fixtures' adb-over-Wi-Fi placeholder, never all of 10.0.0.0/8.
    fn ipv4_allowed(ip: &str) -> bool {
        ip == "10.0.0.7" || ip.starts_with("192.0.2.") || ip.starts_with("127.")
    }

    fn ipv6_allowed(ip: &str) -> bool {
        let ip = ip.to_ascii_lowercase();
        ip == "::1" || ip.starts_with("2001:db8:")
    }

    /// The sanitizer's locally administered placeholders are `02:00:…`; any other MAC is real.
    fn mac_allowed(mac: &str) -> bool {
        mac.to_ascii_lowercase().starts_with("02:00:")
    }

    fn macs(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for raw in text.split(|c: char| !(c.is_ascii_hexdigit() || c == ':' || c == '.')) {
            let token = raw.trim_matches('.');
            let groups: Vec<&str> = token.split(':').collect();
            if groups.len() == 6
                && groups
                    .iter()
                    .all(|group| group.len() == 2 && group.chars().all(|c| c.is_ascii_hexdigit()))
            {
                out.push(token.to_owned());
            }
        }
        out
    }

    fn ipv6s(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for raw in text.split(|c: char| !(c.is_ascii_hexdigit() || c == ':' || c == '.')) {
            let token = raw.trim_matches('.');
            if ipv6_candidate(token) {
                out.push(token.to_owned());
            }
        }
        out
    }

    /// A colon-hex token that can be an IPv6 address: compressed, or exactly eight 1-4 digit
    /// groups. MAC addresses (six 2-digit groups) and ids with longer groups are not addresses.
    fn ipv6_candidate(token: &str) -> bool {
        let groups: Vec<&str> = token.split(':').collect();
        if groups.len() < 3
            || groups.iter().any(|group| group.len() > 4)
            || !groups
                .iter()
                .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return false;
        }
        if token.contains("::") {
            return groups.iter().any(|group| !group.is_empty());
        }
        if groups.len() == 6 && groups.iter().all(|group| group.len() == 2) {
            return false;
        }
        groups.len() == 8
    }

    fn local_hosts(text: &str) -> Vec<String> {
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut rest = lower.as_str();
        while let Some(pos) = rest.find(".local") {
            let before = &rest[..pos];
            if before
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                let start = before
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                    .map(|i| i + 1)
                    .unwrap_or(0);
                out.push(rest[start..pos + 6].to_owned());
            }
            rest = &rest[pos + 6..];
        }
        out
    }
}
