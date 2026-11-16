//! Android parsers: `adb devices -l`, identity, telemetry, apps (P3b Task 3).
//!
//! Pure functions over the tools' text output; nothing here runs a tool. The output shapes are the
//! Android scripts' (Task 2) and are quoted in each parser's comment.

use std::collections::HashMap;

use crate::contract::claims::ResourceId;
use crate::contract::devices::{
    AppState, InstallTime, Platform, Reading, Telemetry, Transport, TransportKind,
    UnidentifiedTransport,
};

/// One `adb devices -l` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbLine {
    pub serial: String,
    pub state: String,
    pub usb: Option<String>,
    pub model: Option<String>,
    pub product: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<u32>,
}

/// `adb devices -l` output. The first token is the transport serial, the second its state; a
/// `no permissions` state spans the rest of the line.
pub fn parse_devices_l(out: &str) -> Vec<AdbLine> {
    let mut lines = Vec::new();
    for text in out.lines() {
        let text = text.trim();
        if text.is_empty() || text.starts_with('*') || text == "List of devices attached" {
            continue;
        }
        let mut parts = text.split_whitespace();
        let (Some(serial), Some(first)) = (parts.next(), parts.next()) else {
            continue;
        };
        let state = if first == "no" && parts.next() == Some("permissions") {
            "no permissions"
        } else {
            first
        };
        let mut line = AdbLine {
            serial: serial.to_owned(),
            state: state.to_owned(),
            usb: None,
            model: None,
            product: None,
            device: None,
            transport_id: None,
        };
        for field in parts {
            let Some((key, value)) = field.split_once(':') else {
                continue;
            };
            match key {
                "usb" => line.usb = Some(value.to_owned()),
                "model" => line.model = Some(value.to_owned()),
                "product" => line.product = Some(value.to_owned()),
                "device" => line.device = Some(value.to_owned()),
                "transport_id" => line.transport_id = value.parse().ok(),
                _ => {}
            }
        }
        lines.push(line);
    }
    lines
}

/// `usb:` wins; `emulator-N` is the emulator; every other serial (`ip:port`, an `adb-<id>._adb-
/// tls-connect._tcp` wireless-debugging serial) is Wi-Fi.
pub fn transport_kind(line: &AdbLine) -> TransportKind {
    if line.usb.is_some() {
        TransportKind::Usb
    } else if line.serial.starts_with("emulator-") {
        TransportKind::Emulator
    } else {
        TransportKind::Wifi
    }
}

/// The identity is re-read when any of these changes (P-13).
pub fn seen_key(line: &AdbLine) -> (String, Option<u32>, String) {
    (line.serial.clone(), line.transport_id, line.state.clone())
}

/// `ro.serialno`, `ro.product.model`, `ro.build.version.release`, `ro.build.version.sdk`,
/// `ro.boot.qemu.avd_name`, `ro.kernel.qemu`, `ro.boot.qemu`: one `getprop` line each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub serialno: String,
    pub model: String,
    pub release: String,
    pub sdk: Option<u32>,
    pub avd_name: Option<String>,
    pub qemu: bool,
}

pub fn parse_identity(out: &str) -> Option<Identity> {
    let values: Vec<&str> = out.lines().map(str::trim).collect();
    if values.len() < 7 {
        return None;
    }
    let avd_name = match values[4] {
        "" => None,
        name => Some(name.to_owned()),
    };
    Some(Identity {
        serialno: values[0].to_owned(),
        model: values[1].to_owned(),
        release: values[2].to_owned(),
        sdk: values[3].parse().ok(),
        avd_name,
        qemu: values[5] == "1" || values[6] == "1",
    })
}

/// `adb -s <serial> emu avd name`: `<name>\nOK`.
pub fn parse_emu_avd_name(out: &str) -> Option<String> {
    let name = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    if name == "OK" || name.starts_with("KO") {
        None
    } else {
        Some(name.to_owned())
    }
}

/// A placeholder (P2-17): empty, `unknown`, a known build constant, or all one character.
fn placeholder_serial(serial: &str) -> bool {
    let serial = serial.trim();
    if serial.is_empty()
        || serial.eq_ignore_ascii_case("unknown")
        || serial.eq_ignore_ascii_case("0123456789ABCDEF")
    {
        return true;
    }
    let mut chars = serial.chars();
    let first = chars.next().expect("not empty");
    chars.all(|c| c == first)
}

pub fn resource_of(id: &Identity) -> Option<ResourceId> {
    if id.qemu {
        let name = id.avd_name.as_deref()?.trim();
        if name.is_empty() {
            return None;
        }
        Some(ResourceId(format!("device:android:avd:{name}")))
    } else {
        let serial = id.serialno.trim();
        if placeholder_serial(serial) {
            None
        } else {
            Some(ResourceId(format!("device:android:{serial}")))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub resource: ResourceId,
    pub identity: Identity,
    /// USB before Wi-Fi.
    pub transports: Vec<Transport>,
}

/// Joins transports by resource. Non-`device` states, failed identities, placeholder serials and a
/// serial seen on two simultaneous `usb:` transports are unidentified (P2-17).
pub fn merge(lines: &[(AdbLine, Option<Identity>)]) -> (Vec<Merged>, Vec<UnidentifiedTransport>) {
    struct Group {
        resource: ResourceId,
        identity: Option<Identity>,
        usb_identity: Option<Identity>,
        transports: Vec<(Transport, Option<String>)>,
        usb: usize,
    }

    enum Slot {
        Group(usize),
        Unidentified(UnidentifiedTransport),
    }

    fn unidentified(
        transport: Transport,
        model: Option<String>,
        hint: Option<&'static str>,
    ) -> UnidentifiedTransport {
        UnidentifiedTransport {
            platform: Platform::Android,
            transport,
            model,
            hint: hint.map(str::to_owned),
        }
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut by_resource: HashMap<ResourceId, usize> = HashMap::new();
    let mut slots = Vec::with_capacity(lines.len());

    for (line, identity) in lines {
        let transport = Transport {
            kind: transport_kind(line),
            address: line.serial.clone(),
            state: line.state.clone(),
        };
        if line.state != "device" {
            let un = unidentified(transport, line.model.clone(), state_hint(&line.state));
            slots.push(Slot::Unidentified(un));
            continue;
        }
        let Some(identity) = identity else {
            let un = unidentified(transport, line.model.clone(), None);
            slots.push(Slot::Unidentified(un));
            continue;
        };
        let Some(resource) = resource_of(identity) else {
            let hint = if identity.qemu {
                "this emulator reports no AVD name"
            } else {
                "this phone reports no unique serial"
            };
            let un = unidentified(transport, line.model.clone(), Some(hint));
            slots.push(Slot::Unidentified(un));
            continue;
        };
        let usb = transport.kind == TransportKind::Usb;
        let index = *by_resource.entry(resource.clone()).or_insert_with(|| {
            groups.push(Group {
                resource,
                identity: None,
                usb_identity: None,
                transports: Vec::new(),
                usb: 0,
            });
            groups.len() - 1
        });
        let group = &mut groups[index];
        if usb {
            group.usb += 1;
            group.usb_identity.get_or_insert_with(|| identity.clone());
        }
        group.identity.get_or_insert_with(|| identity.clone());
        group.transports.push((transport, line.model.clone()));
        slots.push(Slot::Group(index));
    }

    enum Output {
        Merged(Merged),
        Duplicate(Vec<UnidentifiedTransport>),
        Taken,
    }

    let mut outputs: Vec<Output> = groups
        .into_iter()
        .map(|group| {
            if group.usb > 1 {
                return Output::Duplicate(
                    group
                        .transports
                        .into_iter()
                        .map(|(transport, model)| {
                            unidentified(
                                transport,
                                model,
                                Some("this phone reports no unique serial"),
                            )
                        })
                        .collect(),
                );
            }
            let mut transports: Vec<Transport> = group
                .transports
                .into_iter()
                .map(|(transport, _)| transport)
                .collect();
            transports.sort_by_key(|transport| match transport.kind {
                TransportKind::Usb => 0,
                TransportKind::Emulator => 1,
                _ => 2,
            });
            Output::Merged(Merged {
                resource: group.resource,
                identity: group
                    .usb_identity
                    .or(group.identity)
                    .expect("an identified group has an identity"),
                transports,
            })
        })
        .collect();

    let mut merged = Vec::new();
    let mut unidentified_transports = Vec::new();
    for slot in slots {
        match slot {
            Slot::Group(index) => match std::mem::replace(&mut outputs[index], Output::Taken) {
                Output::Merged(one) => merged.push(one),
                Output::Duplicate(mut many) => unidentified_transports.append(&mut many),
                Output::Taken => {}
            },
            Slot::Unidentified(one) => unidentified_transports.push(one),
        }
    }

    (merged, unidentified_transports)
}

fn state_hint(state: &str) -> Option<&'static str> {
    match state {
        "unauthorized" => Some("accept the prompt on the phone"),
        "no permissions" => Some("udev rules (Linux)"),
        _ => None,
    }
}

pub fn parse_boot_completed(out: &str) -> bool {
    out.lines().any(|line| line.trim() == "1")
}

pub fn parse_telemetry(out: &str, emulator: bool, now_ms: i64) -> Telemetry {
    if emulator {
        return Telemetry {
            battery_percent: Reading::Emulated,
            charging: Reading::Emulated,
            wifi: Reading::Emulated,
            wifi_name: None,
            cellular_data: Reading::Emulated,
            vpn: Reading::Emulated,
            at_ms: now_ms,
        };
    }

    let mut level = None;
    let mut scale = None;
    let mut status = None;
    let mut present = None;
    let mut plugged = false;
    let mut wifi_name = None;
    let mut wifi_line_seen = false;
    let mut agents = 0;
    let mut wifi = false;
    let mut cellular = false;
    let mut vpn = false;

    for line in out.lines() {
        let text = line.trim_start();
        if let Some(value) = text.strip_prefix("level:") {
            level = value.trim().parse::<u32>().ok();
        } else if let Some(value) = text.strip_prefix("scale:") {
            scale = value.trim().parse::<u32>().ok();
        } else if let Some(value) = text.strip_prefix("status:") {
            status = value.trim().parse::<u32>().ok();
        } else if let Some(value) = text.strip_prefix("present:") {
            present = match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
        } else if [
            "AC powered:",
            "USB powered:",
            "Wireless powered:",
            "Dock powered:",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
        {
            plugged |= text.ends_with("true");
        }
        if !wifi_line_seen && let Some(name) = wifi_name_of(line) {
            wifi_line_seen = true;
            wifi_name = name;
        }
        if !line.contains("NetworkAgentInfo{") {
            continue;
        }
        agents += 1;
        if !agent_connected(line) {
            continue;
        }
        let transports = agent_transports(line);
        let capabilities = agent_capabilities(line);
        // Global Constraints: a transport is on only with INTERNET and without VPN.
        let on_transport = |kind: &str| {
            transports.contains(&kind)
                && capabilities.contains(&"INTERNET")
                && !transports.contains(&"VPN")
        };
        if on_transport("WIFI") {
            wifi = true;
        }
        if transports.contains(&"VPN") {
            vpn = true;
        }
        if on_transport("CELLULAR") {
            cellular = true;
        }
    }

    let battery_percent = match (present, level, scale) {
        (Some(false), _, _) => Reading::Unknown,
        (_, Some(level), Some(scale)) if scale > 0 && level <= scale => {
            Reading::Known((level * 100 / scale).min(100) as u8)
        }
        _ => Reading::Unknown,
    };
    // L56, L67: on external power. CHARGING always; NOT_CHARGING (an Android 10 phone on USB)
    // and FULL while plugged; DISCHARGING never.
    let charging = match status {
        Some(2) => Reading::Known(true),
        Some(4 | 5) => Reading::Known(plugged),
        Some(3) => Reading::Known(false),
        _ => Reading::Unknown,
    };
    let (wifi, cellular_data, vpn) = if agents == 0 {
        (Reading::Unknown, Reading::Unknown, Reading::Unknown)
    } else {
        (
            Reading::Known(wifi),
            Reading::Known(cellular),
            Reading::Known(vpn),
        )
    };
    let wifi_name = if wifi == Reading::Known(true) {
        wifi_name
    } else {
        None
    };

    Telemetry {
        battery_percent,
        charging,
        wifi,
        wifi_name,
        cellular_data,
        vpn,
        at_ms: now_ms,
    }
}

/// Only `NetworkAgentInfo{` lines describe networks; requests also print `Transports:` (P-16).
/// `DISCONNECTED`/`NOT_CONNECTED` must not match `CONNECTED`.
fn agent_connected(line: &str) -> bool {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token == "CONNECTED")
}

fn agent_transports(line: &str) -> Vec<&str> {
    let Some((_, rest)) = line.split_once("Transports:") else {
        return Vec::new();
    };
    rest.trim_start()
        .split(|c: char| c.is_whitespace() || c == ']')
        .next()
        .unwrap_or_default()
        .split('|')
        .filter(|t| !t.is_empty())
        .collect()
}

fn agent_capabilities(line: &str) -> Vec<&str> {
    let Some((_, rest)) = line.split_once("Capabilities:") else {
        return Vec::new();
    };
    rest.trim_start()
        .split(|c: char| c.is_whitespace() || c == ']')
        .next()
        .unwrap_or_default()
        .split('&')
        .filter(|t| !t.is_empty())
        .collect()
}

/// What follows the SSID in `WifiInfo.toString()` (Android 10–17, plus vendor builds that drop
/// `BSSID`, Task 13 F-1): an unquoted name ends at the first of them.
const WIFI_FIELDS: &[&str] = &[
    "BSSID",
    "MAC",
    "IP",
    "Security type",
    "Supplicant state",
    "Wi-Fi standard",
    "RSSI",
    "Link speed",
    "Tx Link speed",
    "Max Supported Tx Link speed",
    "Rx Link speed",
    "Max Supported Rx Link speed",
    "Frequency",
    "Net ID",
    "Metered hint",
    "score",
    "isUsable",
    "CarrierMerged",
    "SubscriptionId",
    "IsPrimary",
    "Trusted",
    "Restricted",
    "Ephemeral",
    "OEM paid",
    "OEM private",
    "OSU AP",
    "FQDN",
    "Provider friendly name",
    "Requesting package name",
    "MLO Information",
    "Vendor Data",
];

/// The first `mWifiInfo SSID:` line; quoted on 14+, unquoted on 10; `<unknown ssid>` is no name.
fn wifi_name_of(line: &str) -> Option<Option<String>> {
    let (_, rest) = line.split_once("mWifiInfo SSID:")?;
    let rest = rest.trim_start();
    let value = match rest.strip_prefix('"') {
        // Quoted: everything to the closing quote, commas too.
        Some(quoted) => quoted.split_once('"').map_or(quoted, |(name, _)| name),
        // Unquoted: an SSID may contain a comma, so it ends only at a known next field. A name
        // that itself holds `, <field>:` cannot be told from that field (Android 10 does not
        // quote): the earliest known field wins, the chosen reading (L69).
        None => {
            let end = WIFI_FIELDS
                .iter()
                .filter_map(|field| rest.find(&format!(", {field}:")))
                .min()
                .unwrap_or(rest.len());
            &rest[..end]
        }
    };
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("<unknown ssid>") {
        Some(None)
    } else {
        Some(Some(value.to_owned()))
    }
}

/// `date +%s` and `date +%z` (either order). `skew_ms` is the host's clock minus the device's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceClock {
    pub utc_offset_s: i32,
    pub skew_ms: i64,
}

impl DeviceClock {
    /// What converting a device-local time adds: the skew minus the UTC offset. Two snapshots'
    /// values differ by exactly how much their converted times of one device value differ
    /// (Task 13 F-2).
    pub fn conversion_ms(&self) -> i64 {
        self.skew_ms - i64::from(self.utc_offset_s) * 1_000
    }
}

pub fn parse_clock(out: &str, host_now_ms: i64) -> Option<DeviceClock> {
    let mut offset = None;
    let mut epoch_s = None;
    let mut seen = 0;
    for line in out.lines().map(str::trim) {
        // Only the clock's own lines: SNAPSHOT puts them before `pm` and millions of dumpsys bytes.
        if line.starts_with("package:") || line == "===" {
            break;
        }
        if line.is_empty() {
            continue;
        }
        seen += 1;
        if seen > 2 {
            break;
        }
        if offset.is_none() {
            offset = parse_utc_offset(line);
        }
        if epoch_s.is_none() && line.bytes().all(|b| b.is_ascii_digit()) {
            epoch_s = line.parse::<i64>().ok();
        }
    }
    let epoch_ms = epoch_s?.checked_mul(1_000)?;
    Some(DeviceClock {
        utc_offset_s: offset?,
        skew_ms: host_now_ms.saturating_sub(epoch_ms),
    })
}

/// SNAPSHOT's order: `date +%s`, `date +%z`, then `getprop persist.sys.timezone` (L69). The
/// zone line's place is fixed, so whatever it holds (or an empty line) is never read as `pm`
/// output.
const ZONE_LINE: usize = 2;

/// How many lines of `out` are the clock and zone lines: up to three, ending early at the first
/// `package:` or `===` line (an output without a zone line).
fn head_len(out: &str) -> usize {
    out.lines()
        .take(ZONE_LINE + 1)
        .map(str::trim)
        .take_while(|line| !line.starts_with("package:") && *line != "===")
        .count()
}

/// The time zone id the snapshot's `getprop persist.sys.timezone` printed (`Europe/Berlin`,
/// `GMT+05:30`): the zone line, whatever its characters, unless it is empty, missing or has
/// whitespace; then `None`, unknown (L69).
pub fn parse_zone(out: &str) -> Option<String> {
    if head_len(out) <= ZONE_LINE {
        return None;
    }
    let line = out.lines().nth(ZONE_LINE)?.trim();
    (!line.is_empty() && !line.contains(char::is_whitespace) && !is_clock_line(line))
        .then(|| line.to_owned())
}

/// Either line `date +%s` / `date +%z` prints; the `pm` part of a snapshot may hold them.
fn is_clock_line(text: &str) -> bool {
    let text = text.trim();
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        || parse_utc_offset(text).is_some()
}

fn parse_utc_offset(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, text.get(1..)?),
        b'-' => (-1, text.get(1..)?),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits.get(0..2)?.parse().ok()?;
    let minutes: i32 = digits.get(2..4)?.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// The app snapshot: two `date` lines, `pm list packages -3 --show-versioncode`, a `===` line,
/// then `dumpsys package packages`. A truncated output, a failed or partial `pm` list, a missing
/// marker or a dump without a `Package [` block is unreadable, never empty.
pub fn parse_apps(
    out: &str,
    truncated: bool,
    clock: Option<&DeviceClock>,
) -> Option<Vec<AppState>> {
    if truncated {
        return None;
    }
    let marker = out.lines().position(|line| line.trim() == "===")?;

    // The two clock lines, then the zone line: whatever that holds never fails the snapshot.
    let head = head_len(out);
    for line in out.lines().take(head.min(ZONE_LINE)) {
        let text = line.trim();
        if !text.is_empty() && !is_clock_line(text) {
            return None;
        }
    }
    let mut pm_packages = Vec::new();
    for line in out.lines().take(marker).skip(head) {
        let text = line.trim();
        if text.is_empty() || is_clock_line(text) {
            continue;
        }
        let rest = text.strip_prefix("package:")?;
        let mut parts = rest.split_whitespace();
        let app_id = parts.next()?.to_owned();
        let version_code = parts
            .find_map(|field| field.strip_prefix("versionCode:"))
            .map(str::to_owned);
        pm_packages.push((app_id, version_code));
    }

    let packages = parse_package_blocks(out.lines().skip(marker + 1))?;

    let mut apps = Vec::new();
    for (app_id, pm_version_code) in pm_packages {
        let block = packages.get(&app_id)?;
        // A missing clock gives no times; a present one converts device-local time to UTC and
        // corrects it by the device's skew against the host. Epoch (or earlier) is no time.
        let at = |ms: Option<i64>| -> Option<InstallTime> {
            let clock = clock?;
            let at_ms = ms? + clock.conversion_ms();
            (at_ms > 0).then_some(InstallTime::Device { at_ms })
        };
        apps.push(AppState {
            app_id,
            version_name: block.version_name.clone(),
            version_code: block.version_code.clone().or(pm_version_code),
            installed: at(block.first),
            updated: at(block.last),
        });
    }
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    Some(apps)
}

#[derive(Default)]
struct PackageBlock {
    version_code: Option<String>,
    version_name: Option<String>,
    /// Device-local epoch ms, before the offset and skew.
    first: Option<i64>,
    last: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Package,
    User0,
    Other,
}

impl PackageBlock {
    fn fill_from(&mut self, other: PackageBlock) {
        self.version_code = self.version_code.take().or(other.version_code);
        self.version_name = self.version_name.take().or(other.version_name);
        self.first = self.first.take().or(other.first);
        self.last = self.last.take().or(other.last);
    }
}

fn parse_package_blocks<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> Option<HashMap<String, PackageBlock>> {
    let mut blocks: HashMap<String, PackageBlock> = HashMap::new();
    let mut current: Option<(String, PackageBlock)> = None;
    let mut section = Section::Package;
    let mut saw_block = false;
    for line in lines {
        let text = line.trim_start();
        if let Some(rest) = text.strip_prefix("Package [") {
            saw_block = true;
            store_block(&mut blocks, current.take());
            current = rest
                .split_once(']')
                .map(|(id, _)| (id.to_owned(), PackageBlock::default()));
            section = Section::Package;
            continue;
        }
        let Some((_, block)) = current.as_mut() else {
            continue;
        };
        if text.starts_with("User 0:") {
            section = Section::User0;
        } else if text.starts_with("User ") {
            section = Section::Other;
        } else if let Some(value) = text.strip_prefix("versionCode=") {
            let value = value.split_whitespace().next().unwrap_or_default();
            block.version_code = Some(value.to_owned());
        } else if let Some(value) = text.strip_prefix("versionName=") {
            block.version_name = Some(value.trim().to_owned());
        } else if let Some(value) = text.strip_prefix("firstInstallTime=") {
            let parsed = parse_device_time(value);
            match section {
                Section::Package => block.first = parsed,
                Section::User0 => block.first = block.first.or(parsed),
                Section::Other => {}
            }
        } else if let Some(value) = text.strip_prefix("lastUpdateTime=") {
            let parsed = parse_device_time(value);
            match section {
                Section::Package => block.last = parsed,
                Section::User0 => block.last = block.last.or(parsed),
                Section::Other => {}
            }
        }
    }
    store_block(&mut blocks, current.take());
    saw_block.then_some(blocks)
}

fn store_block(blocks: &mut HashMap<String, PackageBlock>, block: Option<(String, PackageBlock)>) {
    let Some((id, block)) = block else {
        return;
    };
    match blocks.entry(id) {
        std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().fill_from(block),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(block);
        }
    }
}

/// `YYYY-MM-DD HH:MM:SS`, device-local.
fn parse_device_time(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    let mut time = time.split(':');
    let hour: u32 = time.next()?.parse().ok()?;
    let minute: u32 = time.next()?.parse().ok()?;
    let second: u32 = time.next()?.parse().ok()?;
    if date.next().is_some()
        || time.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(civil_ms(year, month, day, hour, minute, second))
}

/// A civil date as Unix milliseconds (Hinnant's `days_from_civil`).
fn civil_ms(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * i64::from(shifted_month) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    (days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second)) * 1_000
}

/// `emulator -list-avds`: one name per line. Some releases print `INFO    | …` on stdout too.
pub fn parse_avds(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/devices")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn line(serial: &str, state: &str, usb: Option<&str>) -> AdbLine {
        AdbLine {
            serial: serial.into(),
            state: state.into(),
            usb: usb.map(Into::into),
            model: None,
            product: None,
            device: None,
            transport_id: None,
        }
    }

    fn id(serialno: &str) -> Identity {
        Identity {
            serialno: serialno.into(),
            model: "SM-S937B".into(),
            release: "14".into(),
            sdk: Some(34),
            avd_name: None,
            qemu: false,
        }
    }

    fn emu(serialno: &str, avd: &str) -> Identity {
        Identity {
            serialno: serialno.into(),
            model: "sdk_gphone64_arm64".into(),
            release: "17".into(),
            sdk: Some(37),
            avd_name: Some(avd.into()),
            qemu: true,
        }
    }

    fn identity_text(serialno: &str, model: &str, sdk: &str, avd: &str, qemu: &[&str]) -> String {
        format!(
            "{serialno}\n{model}\n17\n{sdk}\n{avd}\n{}\n{}\n",
            qemu[0], qemu[1]
        )
    }

    fn snapshot(pm: &str, packages: &str) -> String {
        format!("{pm}===\n{packages}")
    }

    fn app<'a>(apps: &'a [AppState], id: &str) -> &'a AppState {
        apps.iter()
            .find(|a| a.app_id == id)
            .unwrap_or_else(|| panic!("no app {id}"))
    }

    #[test]
    fn devices_l_parses_usb_wifi_emulator_and_unauthorized() {
        let lines = parse_devices_l(&fixture("adb-devices-l.txt"));
        assert_eq!(lines.len(), 7, "{lines:#?}");
        assert_eq!(lines[0].serial, "FAKE0TECNO01");
        assert_eq!(lines[0].state, "device");
        assert_eq!(lines[0].usb.as_deref(), Some("0-1.2.2"));
        assert_eq!(lines[0].product.as_deref(), Some("LH7n-RU"));
        assert_eq!(lines[0].model.as_deref(), Some("TECNO_LH7n"));
        assert_eq!(lines[0].device.as_deref(), Some("TECNO-LH7n"));
        assert_eq!(lines[0].transport_id, Some(10));
        assert_eq!(lines[3].serial, "FAKE0SAMS01");
        assert_eq!(lines[4].serial, "10.0.0.7:5555");
        assert_eq!(lines[4].usb, None);
        assert_eq!(lines[5].serial, "emulator-5554");
        assert_eq!(lines[6].state, "unauthorized");
        assert_eq!(lines[6].transport_id, Some(15));
        assert!(lines.iter().all(|l| l.serial != "List"));
        assert!(lines.iter().all(|l| !l.serial.is_empty()));
    }

    #[test]
    fn no_permissions_is_one_state_not_a_dozen_tokens() {
        let out = "List of devices attached\n\
                   FAKE0000 no permissions (missing udev rules? user is in the plugdev group); \
                   see [http://developer.android.com/tools/device.html] usb:1-1 transport_id:3\n";
        let lines = parse_devices_l(out);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].state, "no permissions");
        assert_eq!(transport_kind(&lines[0]), TransportKind::Usb);
        assert_eq!(lines[0].transport_id, Some(3));
        let (merged, unidentified) = merge(&[(lines[0].clone(), None)]);
        assert!(merged.is_empty());
        assert_eq!(unidentified[0].hint.as_deref(), Some("udev rules (Linux)"));
    }

    #[test]
    fn tls_connect_serial_is_wifi() {
        let usb = line("FAKE0SAMS01", "device", Some("0-2.3.1"));
        let ip = line("10.0.0.7:5555", "device", None);
        let tls = line("adb-EMULATOR01-abc._adb-tls-connect._tcp", "device", None);
        let emulator = line("emulator-5554", "device", None);
        assert_eq!(transport_kind(&usb), TransportKind::Usb);
        assert_eq!(transport_kind(&ip), TransportKind::Wifi);
        assert_eq!(transport_kind(&tls), TransportKind::Wifi);
        assert_eq!(transport_kind(&emulator), TransportKind::Emulator);
    }

    #[test]
    fn wifi_and_usb_of_one_phone_merge_usb_first() {
        let usb = line("FAKE0SAMS01", "device", Some("0-2.3.1"));
        let wifi = line("10.0.0.7:5555", "device", None);
        let (merged, unidentified) = merge(&[
            (wifi.clone(), Some(id("FAKE0SAMS01"))),
            (usb, Some(id("FAKE0SAMS01"))),
        ]);
        assert!(unidentified.is_empty());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].resource.0, "device:android:FAKE0SAMS01");
        let kinds: Vec<_> = merged[0].transports.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![TransportKind::Usb, TransportKind::Wifi]);
        assert_eq!(merged[0].transports[0].address, "FAKE0SAMS01");
        assert_eq!(merged[0].transports[1].address, "10.0.0.7:5555");
    }

    #[test]
    fn emulators_with_the_same_serialno_stay_apart() {
        let first = line("emulator-5554", "device", None);
        let second = line("emulator-5556", "device", None);
        let (merged, unidentified) = merge(&[
            (first, Some(emu("EMULATORSHARED", "Pixel_9"))),
            (second, Some(emu("EMULATORSHARED", "Pixel_8"))),
        ]);
        assert!(unidentified.is_empty());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].resource.0, "device:android:avd:Pixel_9");
        assert_eq!(merged[1].resource.0, "device:android:avd:Pixel_8");
        assert_eq!(merged[0].transports[0].kind, TransportKind::Emulator);
    }

    #[test]
    fn boot_qemu_also_marks_an_emulator() {
        let out = identity_text(
            "EMULATORSHARED",
            "sdk_gphone64_arm64",
            "37",
            "Pixel_9",
            &["", "1"],
        );
        let identity = parse_identity(&out).unwrap();
        assert!(identity.qemu);
        assert_eq!(identity.sdk, Some(37));
        assert_eq!(
            resource_of(&identity).unwrap().0,
            "device:android:avd:Pixel_9"
        );

        let out = identity_text(
            "EMULATORSHARED",
            "sdk_gphone64_arm64",
            "37",
            "Pixel_9",
            &["1", ""],
        );
        assert!(parse_identity(&out).unwrap().qemu);

        let out = identity_text("FAKE0SAMS01", "SM-S937B", "34", "", &["", ""]);
        let physical = parse_identity(&out).unwrap();
        assert!(!physical.qemu);
        assert_eq!(
            resource_of(&physical).unwrap().0,
            "device:android:FAKE0SAMS01"
        );
    }

    #[test]
    fn emulator_without_avd_name_is_unidentified() {
        let mut identity = emu("EMULATORSHARED", "Pixel_9");
        identity.avd_name = None;
        assert!(resource_of(&identity).is_none());
        identity.avd_name = Some(String::new());
        assert!(resource_of(&identity).is_none());

        let mut identity = emu("EMULATORSHARED", "Pixel_9");
        identity.avd_name = None;
        let (merged, unidentified) =
            merge(&[(line("emulator-5554", "device", None), Some(identity))]);
        assert!(merged.is_empty());
        assert_eq!(unidentified.len(), 1);
        assert_eq!(unidentified[0].transport.address, "emulator-5554");
        assert!(unidentified[0].hint.is_some());
    }

    #[test]
    fn unauthorized_becoming_device_is_a_new_seen_key() {
        let mut unauthorized = line("FAKE0SAMS01", "unauthorized", Some("0-2.3.1"));
        unauthorized.transport_id = Some(15);
        let mut device = unauthorized.clone();
        device.state = "device".into();
        assert_ne!(seen_key(&unauthorized), seen_key(&device));
        assert_eq!(
            seen_key(&unauthorized),
            ("FAKE0SAMS01".into(), Some(15), "unauthorized".into())
        );

        let mut moved = device.clone();
        moved.transport_id = Some(16);
        assert_ne!(seen_key(&device), seen_key(&moved));

        let (_, unidentified) = merge(&[(unauthorized, None)]);
        assert_eq!(
            unidentified[0].hint.as_deref(),
            Some("accept the prompt on the phone")
        );
    }

    #[test]
    fn placeholder_and_duplicate_serials_are_unidentified() {
        for serial in [
            "",
            "unknown",
            "UNKNOWN",
            "0123456789ABCDEF",
            "0000000000000000",
            "11111111",
            "x",
        ] {
            let identity = id(serial);
            assert!(resource_of(&identity).is_none(), "{serial:?}");
        }
        let (merged, unidentified) =
            merge(&[(line("FAKE0SAMS01", "device", None), Some(id("FAKE0SAMS01")))]);
        assert_eq!(merged.len(), 1);
        assert!(unidentified.is_empty());

        let one = line("FAKE0SAMS01", "device", Some("0-1"));
        let two = line("FAKE0SAMS01", "device", Some("0-2"));
        let (merged, unidentified) = merge(&[
            (one, Some(id("FAKE0SAMS01"))),
            (two, Some(id("FAKE0SAMS01"))),
        ]);
        assert!(merged.is_empty());
        assert_eq!(unidentified.len(), 2);
        assert!(
            unidentified
                .iter()
                .all(|u| u.hint.as_deref() == Some("this phone reports no unique serial"))
        );
    }

    #[test]
    fn emu_avd_name_parses_the_first_line() {
        assert_eq!(
            parse_emu_avd_name("Pixel_9\nOK\n").as_deref(),
            Some("Pixel_9")
        );
        assert_eq!(parse_emu_avd_name("OK\n"), None);
        assert_eq!(parse_emu_avd_name(""), None);
    }

    #[test]
    fn battery_charging_comes_from_status() {
        let a10_text = fixture("battery-a10.txt");
        let a10 = parse_telemetry(&a10_text, false, 1_000);
        assert!(a10_text.contains("USB powered: true"));
        assert!(a10_text.contains("status: 4"));
        assert_eq!(a10.battery_percent, Reading::Known(81));
        // L67: NOT_CHARGING on USB power is on external power.
        assert_eq!(a10.charging, Reading::Known(true));
        assert_eq!(a10.at_ms, 1_000);

        let a14_text = fixture("battery-a14.txt");
        let a14 = parse_telemetry(&a14_text, false, 0);
        assert_eq!(a14.battery_percent, Reading::Known(100));
        assert_eq!(a14.charging, Reading::Known(true));

        let a17_text = fixture("battery-a17.txt");
        assert!(!a17_text.contains("ACTION_POWER"));
        let a17 = parse_telemetry(&a17_text, false, 0);
        assert_eq!(a17.charging, Reading::Known(true));
        // A distractor `Sending ACTION_BATTERY_CHANGED` line is not at a line start.
        assert_eq!(a17.battery_percent, Reading::Known(100));

        let full_unplugged = a14_text.replace("AC powered: true", "AC powered: false");
        let full_unplugged = full_unplugged.replace("status: 2", "status: 5");
        assert_eq!(
            parse_telemetry(&full_unplugged, false, 0).charging,
            Reading::Known(false)
        );

        // NOT_CHARGING unplugged, DISCHARGING even while plugged, and no status at all.
        let unplugged = a10_text.replace("USB powered: true", "USB powered: false");
        assert_eq!(
            parse_telemetry(&unplugged, false, 0).charging,
            Reading::Known(false)
        );
        let discharging = a14_text.replace("status: 2", "status: 3");
        assert_eq!(
            parse_telemetry(&discharging, false, 0).charging,
            Reading::Known(false)
        );
        let no_status = a14_text.replace("status: 2", "");
        assert_eq!(
            parse_telemetry(&no_status, false, 0).charging,
            Reading::Unknown
        );

        let no_battery = a14_text.replace("present: true", "present: false");
        assert_eq!(
            parse_telemetry(&no_battery, false, 0).battery_percent,
            Reading::Unknown
        );
    }

    #[test]
    fn network_agents_only_connected() {
        let battery = fixture("battery-a14.txt");
        let wifi_agent = fixture("network-agents-a10.txt");
        let out = format!("{battery}{wifi_agent}{}", fixture("wifi-a14.txt"));
        let telemetry = parse_telemetry(&out, false, 0);
        assert_eq!(telemetry.wifi, Reading::Known(true));
        assert_eq!(telemetry.cellular_data, Reading::Known(false));

        // Request-shaped lines are not NetworkAgentInfo lines; a disconnected agent counts as a
        // known "no mobile data", a NOT_CONNECTED one must not match the CONNECTED token.
        let out = format!(
            "{battery}\
               NetworkRequestInfo{{ni{{MOBILE[LTE]}} nc{{[ Transports: CELLULAR Capabilities: INTERNET ]}}}}\n\
               NetworkAgentInfo{{ni{{WIFI DISCONNECTED extra: }} nc{{[ Transports: WIFI Capabilities: INTERNET ]}}}}\n\
               NetworkAgentInfo{{ni{{WIFI NOT_CONNECTED extra: }} nc{{[ Transports: WIFI Capabilities: INTERNET ]}}}}\n"
        );
        let telemetry = parse_telemetry(&out, false, 0);
        assert_eq!(telemetry.wifi, Reading::Known(false));
        assert_eq!(telemetry.cellular_data, Reading::Known(false));

        let mobile = "NetworkAgentInfo{ni{MOBILE[LTE] CONNECTED extra: } \
                      nc{[ Transports: CELLULAR Capabilities: INTERNET&NOT_METERED&TRUSTED&NOT_VPN ]}}\n";
        let telemetry = parse_telemetry(&format!("{battery}{mobile}"), false, 0);
        assert_eq!(telemetry.cellular_data, Reading::Known(true));
        assert_eq!(telemetry.vpn, Reading::Known(false));

        // A VPN over cellular is the VPN, not mobile data; the IMS-only agent has no INTERNET.
        let vpn = "NetworkAgentInfo{ni{MOBILE[LTE] CONNECTED extra: } nc{[ Transports: CELLULAR|VPN Capabilities: INTERNET&TRUSTED ]}}\n";
        let telemetry = parse_telemetry(&format!("{battery}{vpn}"), false, 0);
        assert_eq!(telemetry.cellular_data, Reading::Known(false));
        assert_eq!(telemetry.vpn, Reading::Known(true));

        let ims = fixture("network-agents-a14-vpn-ims.txt");
        let telemetry = parse_telemetry(&format!("{battery}{ims}"), false, 0);
        assert_eq!(telemetry.cellular_data, Reading::Known(false));
        assert_eq!(telemetry.vpn, Reading::Known(true));
        assert_eq!(telemetry.wifi, Reading::Known(true));

        let unknown = parse_telemetry(&battery, false, 0);
        assert_eq!(unknown.wifi, Reading::Unknown);
        assert_eq!(unknown.cellular_data, Reading::Unknown);
        assert_eq!(unknown.vpn, Reading::Unknown);

        // Wi-Fi counts only with INTERNET and without VPN (P3bT3-3).
        let local_only = "NetworkAgentInfo{ni{WIFI CONNECTED extra: } \
                          nc{[ Transports: WIFI Capabilities: NOT_METERED&TRUSTED ]}}\n";
        let telemetry = parse_telemetry(&format!("{battery}{local_only}"), false, 0);
        assert_eq!(telemetry.wifi, Reading::Known(false));

        let vpn_over_wifi = "NetworkAgentInfo{ni{VPN CONNECTED extra: } \
                             nc{[ Transports: WIFI|VPN Capabilities: INTERNET&TRUSTED ]}}\n";
        let telemetry = parse_telemetry(&format!("{battery}{vpn_over_wifi}"), false, 0);
        assert_eq!(telemetry.wifi, Reading::Known(false));
        assert_eq!(telemetry.vpn, Reading::Known(true));
    }

    #[test]
    fn unknown_ssid_is_none() {
        let out = format!(
            "{}{}mWifiInfo SSID: <unknown ssid>, BSSID: 02:00:00:00:00:00, \
             MAC: 02:00:00:00:00:00\n",
            fixture("battery-a14.txt"),
            fixture("network-agents-a14.txt")
        );
        let telemetry = parse_telemetry(&out, false, 0);
        assert_eq!(telemetry.wifi, Reading::Known(true));
        assert_eq!(telemetry.wifi_name, None);
        let out = format!(
            "{}{}mWifiInfo SSID: \"\", BSSID: 02:00:00:00:00:00\n",
            fixture("battery-a14.txt"),
            fixture("network-agents-a14.txt")
        );
        assert_eq!(parse_telemetry(&out, false, 0).wifi_name, None);
    }

    /// L69 F-R2: the zone line is positional and tolerant: a fixed-offset id with `:`, an
    /// empty `getprop` line, an Olson id; whatever it holds, the snapshot still parses.
    #[test]
    fn zone_line_never_fails_a_snapshot() {
        let snapshot = |zone_line: &str| {
            format!(
                "1800000000\n+0530\n{zone_line}\npackage:com.example.a versionCode:7\n===\n\
                 Package [com.example.a] (1):\n  versionCode=7\n  \
                 firstInstallTime=2026-01-01 10:00:00\n  lastUpdateTime=2026-01-01 10:00:00\n"
            )
        };
        for (line, zone) in [
            ("GMT+05:30", Some("GMT+05:30")),
            ("Asia/Kolkata", Some("Asia/Kolkata")),
            ("", None),
            ("not a zone", None),
        ] {
            let out = snapshot(line);
            assert_eq!(parse_zone(&out).as_deref(), zone, "{line:?}");
            let clock = parse_clock(&out, 1_800_000_000_000).unwrap();
            assert_eq!(clock.utc_offset_s, 19_800, "{line:?}");
            let apps = parse_apps(&out, false, Some(&clock)).unwrap_or_else(|| panic!("{line:?}"));
            assert_eq!(apps.len(), 1, "{line:?}");
        }
        // No zone line at all, and no packages: still whole.
        let bare = "1800000000\n+0000\n===\nPackage [x] (1):\n";
        assert_eq!(parse_zone(bare), None);
        assert_eq!(parse_apps(bare, false, None), Some(Vec::new()));
        // After the first package line, a stray line still makes the list unreadable.
        let stray = snapshot("UTC").replace("===", "oops\n===");
        assert_eq!(parse_apps(&stray, false, None), None);
    }

    #[test]
    fn ssid_quoted_and_unquoted() {
        for name in [
            "wifi-a10.txt",
            "wifi-a10-no-bssid.txt",
            "wifi-a14.txt",
            "wifi-a17.txt",
        ] {
            let out = format!(
                "{}{}{}",
                fixture("battery-a14.txt"),
                fixture("network-agents-a14.txt"),
                fixture(name)
            );
            assert_eq!(
                parse_telemetry(&out, false, 0).wifi_name.as_deref(),
                Some("TestNet"),
                "{name}"
            );
        }

        let out = format!(
            "{}{}mWifiInfo SSID: \"Cafe, Free\", BSSID: 02:00:00:00:00:00\n",
            fixture("battery-a14.txt"),
            fixture("network-agents-a14.txt")
        );
        assert_eq!(
            parse_telemetry(&out, false, 0).wifi_name.as_deref(),
            Some("Cafe, Free")
        );

        // Task 13 F-1: unquoted with a comma, no BSSID field; and the unknown name.
        for (line, want) in [
            (
                "mWifiInfo SSID: Cafe, Free, Supplicant state: COMPLETED, RSSI: -47, score: 60\n",
                Some("Cafe, Free"),
            ),
            (
                "mWifiInfo SSID: Cafe, Free, BSSID: 02:00:00:00:00:00, MAC: 02:00:00:00:00:00\n",
                Some("Cafe, Free"),
            ),
            ("mWifiInfo SSID: TestNet\n", Some("TestNet")),
            (
                "mWifiInfo SSID: <unknown ssid>, Supplicant state: DISCONNECTED, RSSI: -127\n",
                None,
            ),
        ] {
            let out = format!(
                "{}{}{line}",
                fixture("battery-a14.txt"),
                fixture("network-agents-a14.txt")
            );
            assert_eq!(
                parse_telemetry(&out, false, 0).wifi_name.as_deref(),
                want,
                "{line}"
            );
        }

        // No connected Wi-Fi agent: no name, even when the SSID line is there.
        let out = format!("{}{}", fixture("battery-a14.txt"), fixture("wifi-a14.txt"));
        assert_eq!(parse_telemetry(&out, false, 0).wifi_name, None);
    }

    #[test]
    fn emulator_telemetry_is_emulated() {
        let out = format!(
            "{}{}{}",
            fixture("battery-a14.txt"),
            fixture("network-agents-a14.txt"),
            fixture("wifi-a14.txt")
        );
        let telemetry = parse_telemetry(&out, true, 42);
        assert_eq!(telemetry.battery_percent, Reading::Emulated);
        assert_eq!(telemetry.charging, Reading::Emulated);
        assert_eq!(telemetry.wifi, Reading::Emulated);
        assert_eq!(telemetry.wifi_name, None);
        assert_eq!(telemetry.cellular_data, Reading::Emulated);
        assert_eq!(telemetry.vpn, Reading::Emulated);
        assert_eq!(telemetry.at_ms, 42);
    }

    #[test]
    fn apps_android_10_and_17_install_times() {
        let zero = DeviceClock {
            utc_offset_s: 0,
            skew_ms: 0,
        };
        let a10 = snapshot(
            &fixture("pm-list-3-a10.txt"),
            &fixture("dumpsys-package-a10.txt"),
        );
        let apps = parse_apps(&a10, false, Some(&zero)).unwrap();
        assert_eq!(apps.len(), 3);
        let app1 = app(&apps, "com.example.app1");
        assert_eq!(app1.version_name.as_deref(), Some("1.22.5"));
        assert_eq!(app1.version_code.as_deref(), Some("498232"));
        assert_eq!(
            app1.installed,
            Some(InstallTime::Device {
                at_ms: 1_714_746_887_000
            })
        );
        assert_eq!(
            app1.updated,
            Some(InstallTime::Device {
                at_ms: 1_793_760_873_000
            })
        );

        let a17 = snapshot(
            &fixture("pm-list-3-a17.txt"),
            &fixture("dumpsys-package-a17.txt"),
        );
        let apps = parse_apps(&a17, false, Some(&zero)).unwrap();
        assert_eq!(apps.len(), 3);
        let app1 = app(&apps, "com.example.app1");
        assert_eq!(app1.version_name.as_deref(), Some("236.6"));
        assert_eq!(app1.version_code.as_deref(), Some("3615"));
        assert_eq!(
            app1.installed,
            Some(InstallTime::Device {
                at_ms: 1_791_580_742_000
            })
        );
        assert_eq!(
            app1.updated,
            Some(InstallTime::Device {
                at_ms: 1_794_013_523_000
            })
        );
        let app2 = app(&apps, "com.example.app2");
        // User 0's time; User 10's 1970 and User 150's other time are never taken.
        assert_eq!(
            app2.installed,
            Some(InstallTime::Device {
                at_ms: 1_782_824_799_000
            })
        );

        let epoch = fixture("dumpsys-package-a10.txt").replace(
            "firstInstallTime=2024-05-03 14:34:47",
            "firstInstallTime=1970-01-01 00:00:00",
        );
        let apps = parse_apps(
            &snapshot(&fixture("pm-list-3-a10.txt"), &epoch),
            false,
            Some(&zero),
        )
        .unwrap();
        assert_eq!(app(&apps, "com.example.app1").installed, None);
    }

    #[test]
    fn times_use_the_device_offset_and_skew() {
        let clock = parse_clock("1714746887\n+0200\n", 1_714_746_887_000 + 5_000).unwrap();
        assert_eq!(
            clock,
            DeviceClock {
                utc_offset_s: 7_200,
                skew_ms: 5_000
            }
        );
        let a10 = snapshot(
            &fixture("pm-list-3-a10.txt"),
            &fixture("dumpsys-package-a10.txt"),
        );
        let apps = parse_apps(&a10, false, Some(&clock)).unwrap();
        // 14:34:47 local at +02:00 is 12:34:47 UTC; the device was 5 s behind the host.
        assert_eq!(
            app(&apps, "com.example.app1").installed,
            Some(InstallTime::Device {
                at_ms: 1_714_739_692_000
            })
        );

        assert_eq!(
            parse_clock("1714746887\n-0530\n", 0),
            Some(DeviceClock {
                utc_offset_s: -19_800,
                skew_ms: -1_714_746_887_000
            })
        );
        assert!(parse_clock("+0200\n", 0).is_none());
        assert!(parse_clock("1714746887\n", 0).is_none());
        assert!(parse_clock("1714746887\n+0270\n", 0).is_none());
        assert!(parse_clock("", 0).is_none());
        // Only the clock's own lines: nothing after the first `===` or `package:` counts.
        assert!(parse_clock("Error\n===\n1714746887\n+0200\n", 0).is_none());
        assert!(parse_clock("Error\npackage:x\n1714746887\n+0200\n", 0).is_none());
    }

    #[test]
    fn apps_without_a_clock_have_no_times() {
        let a10 = snapshot(
            &fixture("pm-list-3-a10.txt"),
            &fixture("dumpsys-package-a10.txt"),
        );
        let apps = parse_apps(&a10, false, None).unwrap();
        assert!(
            apps.iter()
                .all(|a| a.installed.is_none() && a.updated.is_none())
        );
    }

    #[test]
    fn truncated_or_incomplete_snapshot_is_none() {
        let pm = fixture("pm-list-3-a10.txt");
        let packages = fixture("dumpsys-package-a10.txt");
        let complete = snapshot(&pm, &packages);
        assert!(parse_apps(&complete, true, None).is_none());
        assert!(parse_apps(&format!("{pm}{packages}"), false, None).is_none());
        let missing = packages.replace("Package [com.example.app2]", "Package [com.example.other]");
        assert!(parse_apps(&snapshot(&pm, &missing), false, None).is_none());
        assert!(parse_apps(&snapshot(&pm, ""), false, None).is_none());
    }

    #[test]
    fn failed_or_partial_pm_is_none() {
        let packages = fixture("dumpsys-package-a10.txt");
        let out = format!(
            "1714746887\n+0200\nEurope/Berlin\nError: Can't find service: package\n===\n{packages}"
        );
        assert!(parse_apps(&out, false, None).is_none());
        // Before the zone line (an output without it), too.
        let out = format!("1714746887\nError: Can't find service: package\n===\n{packages}");
        assert!(parse_apps(&out, false, None).is_none());

        let out = format!(
            "1714746887\n+0200\npackage:com.example.app1 versionCode:498232\n\
             Failure calling service package: Broken pipe (32)\n===\n{packages}"
        );
        assert!(parse_apps(&out, false, None).is_none());

        let out = "1714746887\n+0200\npackage:com.example.app1 versionCode:498232\n===\n";
        assert!(parse_apps(out, false, None).is_none());

        // The marker is a whole line, not any `===` substring.
        let out = format!("{}x===\n{packages}", fixture("pm-list-3-a10.txt"));
        assert!(parse_apps(&out, false, None).is_none());

        // A real zero-app list: the clock lines, no `pm` package, a dump with system packages.
        let system = "  Package [com.android.systemui] (abc):\n\
                      \x20   versionCode=1 minSdk=1 targetSdk=1\n\
                      \x20   versionName=1\n";
        let out = format!("1714746887\n+0200\n===\n{system}");
        assert_eq!(parse_apps(&out, false, None), Some(vec![]));

        // The clock lines in front of a good `pm` list are skipped, not a failure.
        let out = format!(
            "1714746887\n+0200\n{}===\n{packages}",
            fixture("pm-list-3-a10.txt")
        );
        assert_eq!(parse_apps(&out, false, None).unwrap().len(), 3);
    }

    #[test]
    fn boot_completed_parses() {
        assert!(parse_boot_completed("1\n"));
        assert!(parse_boot_completed("1\r\n"));
        assert!(!parse_boot_completed("0\n"));
        assert!(!parse_boot_completed(""));
        assert!(!parse_boot_completed("getprop: not found\n"));
    }

    #[test]
    fn avds_parse() {
        assert_eq!(
            parse_avds("Pixel_9\nPixel_8\n\n"),
            vec!["Pixel_9", "Pixel_8"]
        );
        assert_eq!(parse_avds("  Pixel_9  \n"), vec!["Pixel_9"]);
        assert!(parse_avds("\n").is_empty());
        assert_eq!(
            parse_avds("INFO    | Storing crashdata in: /tmp/android-user/crashpad\nPixel_9\n"),
            vec!["Pixel_9"]
        );
    }

    #[test]
    fn identity_parses_props_in_order() {
        let out = identity_text("FAKE0SAMS01", "SM-S937B", "34", "", &["", ""]);
        let identity = parse_identity(&out).unwrap();
        assert_eq!(identity.serialno, "FAKE0SAMS01");
        assert_eq!(identity.model, "SM-S937B");
        assert_eq!(identity.release, "17");
        assert_eq!(identity.sdk, Some(34));
        assert_eq!(identity.avd_name, None);
        assert!(!identity.qemu);
        assert!(parse_identity("FAKE0SAMS01\nSM-S937B\n").is_none());
    }

    #[test]
    fn identified_transports_carry_the_platform_and_model() {
        let mut wifi = line("10.0.0.7:5555", "device", None);
        wifi.model = Some("SM_S937B".into());
        let (_, unidentified) = merge(&[(wifi, None)]);
        assert_eq!(unidentified[0].platform, Platform::Android);
        assert_eq!(unidentified[0].model.as_deref(), Some("SM_S937B"));
        assert_eq!(unidentified[0].transport.kind, TransportKind::Wifi);
    }
}
