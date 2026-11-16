//! The device service (P3b Task 5): the registry, the poller and the cache.
//!
//! Lock rules: the cache's `RwLock` is held only to copy or replace state, never across a tool
//! call, and never together with the claims or heavy lock. Every tool call runs in a `ToolGate`
//! lane on the blocking pool. The catalog and the sessions source answer from the cache without
//! I/O, so nothing device-related runs under the claims lock (T5-4).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::{Notify, broadcast};

use super::android::{self, AdbLine, Identity, Merged};
use super::catalog;
use super::ios::{self, PhysicalIos, Simulator};
use super::lanes::{Lane, ToolGate};
use super::probes::{self, AdbGuard, AdbServerAddr, AdbTarget, Refused, ServerSeen};
use super::tools::{self, Call, Tool, ToolOutput, ToolPaths, ToolSettings};
use crate::claims::engine::InUse;
use crate::contract::HostlerError;
use crate::contract::claims::{
    ClaimSummary, ClaimsSnapshot, ForeignClaim, ForeignView, Freshness, ResourceId, ResourceInfo,
    errors as claim_errors,
};
use crate::contract::devices::errors;
use crate::contract::devices::{
    AdbState, AgentDeviceSession, DeviceDetail, DeviceInfo, DeviceKind, DevicesSnapshot, Platform,
    Telemetry, ToolPathsSet, ToolStatus, Transport, TransportKind, UnidentifiedTransport,
    valid_alias,
};
use crate::contract::secrets::errors::bad_request;
use crate::contract::wire::Event;
use crate::heavy::engine::{Clock, SystemClock};
use crate::store::{DeviceRow, DeviceSeen, Store};

// ---------------------------------------------------------------------------------------------
// Bounds and intervals (Global Constraints)

const ADB_DEVICES_BOUND: Duration = Duration::from_secs(5);
const ADB_SHELL_BOUND: Duration = Duration::from_secs(10);
const ADB_EMU_BOUND: Duration = Duration::from_secs(5);
const LIST_AVDS_BOUND: Duration = Duration::from_secs(10);
const DEVICECTL_LIST_BOUND: Duration = Duration::from_secs(15);
const SIMCTL_LIST_BOUND: Duration = Duration::from_secs(10);

const ANDROID_EVERY: Duration = Duration::from_secs(3);
const AVDS_EVERY: Duration = Duration::from_secs(60);
const IOS_EVERY: Duration = Duration::from_secs(10);
const SESSIONS_EVERY: Duration = Duration::from_secs(5);
const SCHEDULE_EVERY: Duration = Duration::from_secs(10);
const ALIVE_EVERY: Duration = Duration::from_secs(60);
/// A tool that failed or timed out for this long marks its devices disconnected.
const STALE_MS: i64 = 30_000;
/// `last_seen` is written at most this often per device, or on a change (P2-19).
const SEEN_WRITE_MS: i64 = 60_000;
/// Telemetry and other volatile-only changes are broadcast at most this often (P3bT5-8).
const TELEMETRY_EVENT_MS: i64 = 10_000;
/// An identity that failed is asked again after this long.
const IDENTITY_RETRY_MS: i64 = 30_000;
/// A transport's identity is forgotten once it has not been listed for this long (P3bT5-13).
const IDENTITY_KEEP_MS: i64 = 60_000;
/// The first app snapshots after a service start begin this far apart (Task 8's schedule).
const SNAPSHOT_STAGGER: Duration = Duration::from_secs(10);

/// Debug builds: the agent-device sessions as a file (P3-6), so Tasks 6 and 7 test without 9b.
pub const TEST_FAKE_SESSIONS_ENV: &str = "HOSTLER_TEST_FAKE_SESSIONS";
/// Debug builds: run the iOS discovery off macOS (with the fake tools).
pub const TEST_PLATFORM_IOS_ENV: &str = "HOSTLER_TEST_PLATFORM_IOS";
/// Debug builds: every discovery interval, in ms (tests).
pub const TEST_POLL_MS_ENV: &str = "HOSTLER_TEST_DEVICES_POLL_MS";
/// Debug builds: the 30 s staleness rule, in ms (tests).
pub const TEST_STALE_MS_ENV: &str = "HOSTLER_TEST_DEVICES_STALE_MS";
/// Debug builds run the real device tools only with this set: the suite's services never touch
/// a phone that happens to be attached.
pub const TEST_REAL_TOOLS_ENV: &str = "HOSTLER_TEST_REAL_TOOLS";

pub(super) fn test_ms(name: &str) -> Option<u64> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name).ok()?.parse().ok().filter(|ms| *ms > 0)
}

pub(super) fn every(production: Duration) -> Duration {
    test_ms(TEST_POLL_MS_ENV).map_or(production, Duration::from_millis)
}

fn stale_ms() -> i64 {
    test_ms(TEST_STALE_MS_ENV).map_or(STALE_MS, |ms| ms as i64)
}

/// The settings keys (P2-18, P2-21).
pub mod keys {
    pub const ADB_PATH: &str = "devices.adb_path";
    pub const AGENT_DEVICE_PATH: &str = "devices.agent_device_path";
    pub const AGENT_DEVICE_SCRIPT: &str = "devices.agent_device_script";
    pub const NODE_PATH: &str = "devices.node_path";
    pub const ENV_PREFIX: &str = "devices.env.";
    pub const ALIVE_MS: &str = "devices.alive_ms";
    /// `devices.clock.<resource>`: the last good Android snapshot's time conversion (Task 8 F-2).
    pub const CLOCK_PREFIX: &str = "devices.clock.";
}

/// The Android variables `devices tools --detect` may record (the service keeps them, L44).
pub const RECORDED_ENV: [&str; 8] = [
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "ANDROID_SDK_HOME",
    "ANDROID_USER_HOME",
    "ANDROID_AVD_HOME",
    "ANDROID_EMULATOR_HOME",
    "ANDROID_ADB_SERVER_PORT",
    "ADB_SERVER_SOCKET",
];

/// The recorded names that hold a path; the adb server port and socket names do not.
pub fn path_valued_env(name: &str) -> bool {
    !matches!(name, "ANDROID_ADB_SERVER_PORT" | "ADB_SERVER_SOCKET")
}

/// The `devices.*` settings from the store.
pub fn read_settings(store: &Store) -> ToolSettings {
    let path = |key: &str| {
        store
            .setting(key)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let env = RECORDED_ENV
        .iter()
        .filter_map(|name| {
            let value = store
                .setting(&format!("{}{name}", keys::ENV_PREFIX))
                .ok()
                .flatten()?;
            Some(((*name).to_owned(), value))
        })
        .collect();
    ToolSettings {
        adb_path: path(keys::ADB_PATH),
        agent_device_path: path(keys::AGENT_DEVICE_PATH),
        agent_device_script: path(keys::AGENT_DEVICE_SCRIPT),
        node_path: path(keys::NODE_PATH),
        env,
    }
}

/// The tools the service runs: `find_tools` over the settings and its environment. Debug builds
/// find nothing unless the fake tools or `HOSTLER_TEST_REAL_TOOLS=1` are set.
pub fn find_service_tools(settings: &ToolSettings) -> ToolPaths {
    if cfg!(debug_assertions)
        && std::env::var_os(tools::FAKE_TOOLS_ENV).is_none()
        && std::env::var_os(TEST_REAL_TOOLS_ENV).is_none_or(|v| v != "1")
    {
        return ToolPaths::default();
    }
    let home = home_dir();
    tools::find_tools(settings, &|name| std::env::var_os(name), &home)
}

fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// The claims view

/// The claims on each device, from the last `ClaimsChanged` (P-17).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaimsView {
    pub on_device: BTreeMap<ResourceId, Vec<ClaimSummary>>,
}

impl ClaimsView {
    pub fn from_snapshot(snapshot: &ClaimsSnapshot) -> ClaimsView {
        let on_device = snapshot
            .resources
            .iter()
            .filter(|state| state.resource.kind == crate::contract::claims::ResourceKind::Device)
            .filter(|state| !state.claims.is_empty())
            .map(|state| {
                let claims = state
                    .claims
                    .iter()
                    .map(|c| crate::claims::rules::summary(c, &state.claims))
                    .collect();
                (state.resource.id.clone(), claims)
            })
            .collect();
        ClaimsView { on_device }
    }
}

// ---------------------------------------------------------------------------------------------
// The cache

/// What one Android discovery round saw, with the identities known by then.
#[derive(Clone, Debug, Default)]
struct AndroidSeen {
    merged: Vec<Merged>,
    unidentified: Vec<UnidentifiedTransport>,
    /// The emulators whose own transport read `sys.boot_completed=1` (P3bT5-2).
    ready: HashSet<ResourceId>,
}

/// Every tool's last good answer, and since when it fails.
#[derive(Clone, Debug, Default)]
struct Observed {
    android: Option<AndroidSeen>,
    /// The last good `adb devices -l`; each identity or boot read that finishes applies it
    /// again with what it learned (P3bT5-3).
    android_lines: Option<Vec<AdbLine>>,
    /// The newest Android recomputation installed (older ones that finish later are dropped).
    android_gen: u64,
    android_failing_since: Option<i64>,
    android_problem: Option<String>,
    adb_state: Option<AdbState>,
    /// `sys.boot_completed` per emulator transport (`seen_key`), dropped once that transport is
    /// no longer listed: a reused `emulator-5554` is a new transport (P3bT5-2).
    booted: HashMap<SeenKey, bool>,
    avds: Option<Vec<String>>,
    physical_ios: Option<Vec<PhysicalIos>>,
    devicectl_failing_since: Option<i64>,
    devicectl_problem: Option<String>,
    simulators: Option<Vec<Simulator>>,
    simctl_failing_since: Option<i64>,
    simctl_problem: Option<String>,
    /// A loop whose last round panicked, until a round of it completes (P3bT5-11).
    round_problems: BTreeMap<&'static str, String>,
}

impl Observed {
    /// Forgets what adb said: nothing of it may come back as connected (P3bT5-6).
    fn forget_android(&mut self) {
        self.android = None;
        self.android_lines = None;
        self.booted.clear();
    }
}

/// The agent-device sessions as the source answers them (Task 9b; `HOSTLER_TEST_FAKE_SESSIONS`
/// in tests). Nothing here is persisted but the last good read's sessions (L51): `freshness`
/// starts at `NotYet` in every process (P3-3).
#[derive(Clone, Debug)]
struct Sessions {
    /// The last good read's sessions with their raw device ids (P3bT9b-1); `claims` is derived
    /// from them in every `rebuild`, so a registration, a discovery or a transport change maps
    /// them at once.
    raw: Vec<RawSession>,
    /// Derived: one foreign claim per registered device a session maps to.
    claims: Vec<ForeignClaim>,
    freshness: Freshness,
    /// The status could not be read since this time (L51).
    unreadable_since: Option<i64>,
    reason: Option<String>,
    last_good_ms: Option<i64>,
    /// When the last good read started (`read_started_after`).
    last_good_started_ms: Option<i64>,
    /// When each session (`RawSession::key`) was first seen.
    first_seen: HashMap<String, i64>,
    /// The last claim end per device, as `claim_ended` counted it (for `checking_sessions`).
    ends: HashMap<ResourceId, u64>,
    /// Devices whose last end a person forgot (L54); the next end or a good read applies the
    /// normal rules again, as the engine's `forgotten` does.
    forgotten: HashSet<ResourceId>,
    /// Derived: live sessions that map to no device (P-10), and those on a platform hostler
    /// does not know (P3bT9b-6), sanitized.
    unmapped: Vec<String>,
    /// `~/.agent-device/device-claims` exists (P3-4).
    in_use: bool,
    /// agent-device's version, and a warning when it is older than the target.
    version: Option<String>,
    version_warning: Option<String>,
}

impl Default for Sessions {
    fn default() -> Self {
        Sessions {
            raw: Vec::new(),
            claims: Vec::new(),
            freshness: Freshness::NotTracked,
            unreadable_since: None,
            reason: None,
            last_good_ms: None,
            last_good_started_ms: None,
            first_seen: HashMap::new(),
            ends: HashMap::new(),
            forgotten: HashSet::new(),
            unmapped: Vec::new(),
            in_use: false,
            version: None,
            version_warning: None,
        }
    }
}

/// One live agent-device session as read, with its raw device id. It is mapped to devices in
/// every `rebuild` (P3bT9b-1), never only at the read.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RawSession {
    /// `None`: a platform hostler does not know (a problem, never silent).
    pub platform: Option<Platform>,
    pub platform_text: String,
    /// The adb serial, UDID or CoreDevice id agent-device names.
    pub device_id: String,
    pub name: String,
    pub session: String,
    /// The canonical workspace path; `None` blocks every workspace.
    pub workspace_path: Option<String>,
    /// For the stopper: session, state_dir, platform, id, pid, start_time, node, script.
    pub stop: serde_json::Value,
    #[serde(default)]
    pub since_ms: i64,
    /// Every resource it mapped to since the last good read: kept until the next one (fail
    /// closed), and stored with the read (P2-16).
    #[serde(default)]
    pub mapped: Vec<ResourceId>,
    /// The test hook names the resource itself.
    #[serde(default)]
    pub pinned: Option<ResourceId>,
    /// Kept from the last good read while the status cannot be read (L51).
    #[serde(default)]
    pub kept: bool,
    /// Resources a person released it from (`forget-sessions`, L54).
    #[serde(default)]
    pub forgotten: Vec<ResourceId>,
}

impl RawSession {
    /// The session's identity across reads.
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.platform_text, self.device_id, self.session)
    }
}

/// One sessions read (Task 9b's reader, or the test hook).
#[derive(Clone, Debug)]
pub enum SessionRead {
    /// A good read: the live sessions it lists.
    Good { raw: Vec<RawSession>, in_use: bool },
    /// The status could not be read (L51): the last good sessions stay, except the ones whose
    /// owner daemon is certainly dead (L53), by `RawSession::key`.
    Failed { reason: String, dead: Vec<String> },
}

/// The session name of an agent-device foreign claim (its `stop` data).
pub fn session_of(f: &ForeignClaim) -> Option<&str> {
    f.stop.as_ref()?.get("session")?.as_str()
}

#[derive(Clone, Debug, Default)]
struct CacheState {
    rows: Vec<DeviceRow>,
    observed: Observed,
    telemetry: HashMap<ResourceId, Telemetry>,
    /// Marks counted per holder, so one of two ends never clears the other's (P3bT5-5).
    shutting_down: HashMap<ResourceId, usize>,
    removing: HashMap<ResourceId, usize>,
    booted_by_hostler: HashSet<ResourceId>,
    sessions: Sessions,
    /// Workspace `(id, path)`s, for the sessions' `workspace_id` (P2-15).
    workspaces: Vec<(String, String)>,
    tool_paths: ToolPaths,
    /// Derived by `rebuild`.
    devices: Vec<DeviceInfo>,
    unidentified: Vec<UnidentifiedTransport>,
    tools: Option<ToolStatus>,
    at_ms: i64,
}

/// The poller's results, read by the catalog, the sessions source and every `devices.*` call.
pub struct DeviceCache {
    state: RwLock<CacheState>,
    /// Counts claim ends (P3-3); a sessions read captures it when it starts.
    ends: AtomicU64,
    /// Woken by every claim end: the sessions lane reads at once (L52).
    wake: Notify,
}

impl Default for DeviceCache {
    fn default() -> Self {
        DeviceCache {
            state: RwLock::new(CacheState::default()),
            ends: AtomicU64::new(1),
            wake: Notify::new(),
        }
    }
}

impl DeviceCache {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, CacheState> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, CacheState> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Every device and transport, as `devices list --all` shows them.
    pub fn snapshot(&self) -> DevicesSnapshot {
        let state = self.read();
        DevicesSnapshot {
            devices: state.devices.clone(),
            unidentified: state.unidentified.clone(),
            unmapped_sessions: state.sessions.unmapped.clone(),
            tools: state.tools.clone().unwrap_or_else(|| tool_status(&state)),
            at_ms: state.at_ms,
        }
    }

    /// The catalog's entries: registered devices only, in catalog order (no I/O).
    pub fn resources(&self) -> Vec<ResourceInfo> {
        catalog::entries(&self.read().devices)
    }

    /// The agent-device sessions as foreign claims (no I/O).
    pub fn sessions(&self) -> Vec<ForeignClaim> {
        self.read().sessions.claims.clone()
    }

    /// Sessions and freshness from one read guard (P3-2).
    pub fn collect_sessions(&self) -> ForeignView {
        let state = self.read();
        ForeignView {
            claims: state.sessions.claims.clone(),
            freshness: state.sessions.freshness,
            unreadable: state
                .sessions
                .unreadable_since
                .map(|_| state.sessions.reason.clone().unwrap_or_default()),
        }
    }

    /// The current adb transport serial of an Android device.
    pub fn current_serial(&self, r: &ResourceId) -> Option<String> {
        let state = self.read();
        let info = state.devices.iter().find(|d| d.resource == *r)?;
        (info.platform == Platform::Android && info.connected)
            .then(|| info.transports.first().map(|t| t.address.clone()))
            .flatten()
    }

    /// The device an adb serial belongs to: a current transport, then a stored one (P-10).
    pub fn resolve_serial(&self, serial: &str) -> Option<ResourceId> {
        if serial.is_empty() {
            return None;
        }
        let state = self.read();
        state
            .devices
            .iter()
            .filter(|d| d.platform == Platform::Android && d.connected)
            .find(|d| d.transports.iter().any(|t| t.address == serial))
            .map(|d| d.resource.clone())
            .or_else(|| {
                state
                    .rows
                    .iter()
                    .filter(|row| row.platform == Platform::Android)
                    .find(|row| row.serials.iter().any(|s| s.serial == serial))
                    .map(|row| row.resource.clone())
            })
    }

    /// The iOS device or simulator a UDID or CoreDevice id names.
    pub fn resolve_udid(&self, id: &str) -> Option<ResourceId> {
        if id.is_empty() {
            return None;
        }
        let state = self.read();
        state
            .devices
            .iter()
            .filter(|d| d.platform == Platform::Ios)
            .find(|d| d.hardware_id == id || d.coredevice_id.as_deref() == Some(id))
            .map(|d| d.resource.clone())
    }

    /// While hostler shuts a device down it is not available (P-6). Counted: every `true` needs
    /// its own `false`.
    pub fn set_shutting_down(&self, r: &ResourceId, on: bool) {
        let mut state = self.write();
        mark(&mut state.shutting_down, r, on);
        rebuild(&mut state);
    }

    /// While a `devices remove` checks the claims it is not available (P2-14). Counted, as
    /// `set_shutting_down`.
    pub fn set_removing(&self, r: &ResourceId, on: bool) {
        let mut state = self.write();
        mark(&mut state.removing, r, on);
        rebuild(&mut state);
    }

    /// Task 7: what hostler booted, shown as `booted_by_hostler`.
    pub fn set_booted_by_hostler(&self, r: &ResourceId, on: bool) {
        let mut state = self.write();
        if on {
            state.booted_by_hostler.insert(r.clone());
        } else {
            state.booted_by_hostler.remove(r);
        }
        rebuild(&mut state);
    }

    /// Task 8: a telemetry reading.
    pub fn set_telemetry(&self, r: &ResourceId, telemetry: Telemetry) {
        let mut state = self.write();
        state.telemetry.insert(r.clone(), telemetry);
        rebuild(&mut state);
    }

    /// Task 7: whether the last `simctl list` booted `udid`; `None` until a good list, and while
    /// it fails (P3bT7-1: a missing answer is never "shut down").
    pub(crate) fn simulator_booted(&self, udid: &str) -> Option<bool> {
        let state = self.read();
        if state.observed.simctl_failing_since.is_some() {
            return None;
        }
        let simulators = state.observed.simulators.as_ref()?;
        Some(simulators.iter().any(|s| s.udid == udid && s.booted))
    }

    /// Task 7: every `emulator-N` adb lists now, identified or not (P3bT7-4).
    pub(crate) fn emulator_serials(&self) -> Vec<String> {
        let state = self.read();
        let Some(android) = &state.observed.android else {
            return Vec::new();
        };
        android
            .merged
            .iter()
            .flat_map(|m| m.transports.iter().map(|t| t.address.clone()))
            .chain(
                android
                    .unidentified
                    .iter()
                    .map(|u| u.transport.address.clone()),
            )
            .filter(|a| a.starts_with("emulator-"))
            .collect()
    }

    pub(crate) fn info(&self, r: &ResourceId) -> Option<DeviceInfo> {
        self.read()
            .devices
            .iter()
            .find(|d| d.resource == *r)
            .cloned()
    }
}

impl DeviceCache {
    /// The claim-end counter now; a read captures it before its status call (P3-3).
    pub fn ends(&self) -> u64 {
        self.ends.load(Ordering::SeqCst)
    }

    /// A device claim on `r` ended (P3-3): count it, note it for `checking_sessions`, and wake
    /// the sessions lane (L52). Runs under the claims lock: an atomic, a short write guard (the
    /// poller holds it only to swap in a finished read) and a non-blocking `Notify`; never I/O.
    pub fn claim_ended(&self, r: &ResourceId) -> u64 {
        let seq = self.ends.fetch_add(1, Ordering::SeqCst) + 1;
        {
            // P3bT5-7: `checking_sessions` shows the end at once; CPU only under the guard.
            let mut state = self.write();
            state.sessions.ends.insert(r.clone(), seq);
            // A new end applies the normal rules again (L54).
            state.sessions.forgotten.remove(r);
            rebuild(&mut state);
        }
        self.wake.notify_one();
        seq
    }

    /// Asks the sessions lane for a read now.
    pub fn wake_sessions(&self) {
        self.wake.notify_one();
    }

    pub fn is_registered(&self, r: &ResourceId) -> bool {
        self.read()
            .devices
            .iter()
            .any(|d| d.resource == *r && d.registered)
    }
}

/// One more (`on`) or one fewer holder of a mark on `r`.
fn mark(marks: &mut HashMap<ResourceId, usize>, r: &ResourceId, on: bool) {
    if on {
        *marks.entry(r.clone()).or_default() += 1;
    } else if let Some(n) = marks.get_mut(r) {
        *n -= 1;
        if *n == 0 {
            marks.remove(r);
        }
    }
}

/// The derived view of the cache: one `DeviceInfo` per device known to the store or seen now.
fn rebuild(state: &mut CacheState) {
    let now = state.at_ms;
    let stale = stale_ms();
    let failing = |since: Option<i64>| since.is_some_and(|s| now - s >= stale);
    let mut seen: BTreeMap<ResourceId, DeviceInfo> = BTreeMap::new();

    // Android: what the last good round saw, unless adb has failed for 30 s.
    let android_live = !failing(state.observed.android_failing_since)
        && !matches!(state.observed.adb_state, Some(AdbState::Foreign { .. }));
    let mut unidentified = Vec::new();
    if let Some(android) = state.observed.android.as_ref().filter(|_| android_live) {
        for m in &android.merged {
            let emulator = m.identity.qemu;
            let hardware_id = if emulator {
                m.identity
                    .avd_name
                    .clone()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            } else {
                m.identity.serialno.trim().to_owned()
            };
            let ready = !emulator || android.ready.contains(&m.resource);
            seen.insert(
                m.resource.clone(),
                blank(
                    m.resource.clone(),
                    hardware_id,
                    Platform::Android,
                    if emulator {
                        DeviceKind::Emulator
                    } else {
                        DeviceKind::Physical
                    },
                    non_empty(&m.identity.model),
                    None,
                    non_empty(&m.identity.release),
                    m.transports.clone(),
                    true,
                    ready,
                ),
            );
        }
        unidentified.extend(android.unidentified.iter().cloned());
    }
    // AVDs that exist but do not run.
    for name in state.observed.avds.iter().flatten() {
        let r = avd_resource(name);
        seen.entry(r.clone()).or_insert_with(|| {
            blank(
                r,
                name.clone(),
                Platform::Android,
                DeviceKind::Emulator,
                None,
                None,
                None,
                Vec::new(),
                true,
                false,
            )
        });
    }
    // iOS physical: a wired, paired phone whose tunnel is down is still attached; an untrusted
    // one is not usable by agent-device or devicectl (P3bT5-9).
    if !failing(state.observed.devicectl_failing_since) {
        for p in state.observed.physical_ios.iter().flatten() {
            let r = ios::resource_of_physical(p);
            let wired = p.transport == TransportKind::Wired;
            let state_text = match (p.reachable, wired, p.paired) {
                (true, _, _) => "connected",
                (false, true, true) => "wired",
                (false, true, false) => "unpaired",
                (false, false, _) => "disconnected",
            };
            seen.insert(
                r.clone(),
                DeviceInfo {
                    coredevice_id: non_empty(&p.coredevice_id),
                    ..blank(
                        r,
                        p.udid.clone(),
                        Platform::Ios,
                        DeviceKind::Physical,
                        non_empty(&p.model),
                        non_empty(&p.name),
                        p.os_version.clone(),
                        vec![Transport {
                            kind: p.transport,
                            address: if p.coredevice_id.is_empty() {
                                p.udid.clone()
                            } else {
                                p.coredevice_id.clone()
                            },
                            state: state_text.into(),
                        }],
                        p.reachable || (wired && p.paired),
                        true,
                    )
                },
            );
        }
    }
    if !failing(state.observed.simctl_failing_since) {
        for s in state.observed.simulators.iter().flatten() {
            let r = ios::resource_of_simulator(s);
            seen.insert(
                r.clone(),
                blank(
                    r,
                    s.udid.clone(),
                    Platform::Ios,
                    DeviceKind::Simulator,
                    non_empty(&s.name),
                    non_empty(&s.name),
                    non_empty(&s.runtime),
                    vec![Transport {
                        kind: TransportKind::Simulator,
                        address: s.udid.clone(),
                        state: if s.booted { "booted" } else { "shutdown" }.into(),
                    }],
                    true,
                    s.booted,
                ),
            );
        }
    }

    let mut devices: Vec<DeviceInfo> = Vec::new();
    let rows: HashMap<&ResourceId, &DeviceRow> =
        state.rows.iter().map(|r| (&r.resource, r)).collect();
    for row in &state.rows {
        let mut info = match seen.remove(&row.resource) {
            Some(info) => info,
            // Known, not seen now: disconnected (a virtual device that is gone does not exist).
            None => blank(
                row.resource.clone(),
                hardware_of(&row.resource),
                row.platform,
                row.kind,
                None,
                None,
                None,
                Vec::new(),
                false,
                false,
            ),
        };
        apply_row(&mut info, row);
        devices.push(info);
    }
    // Seen but not stored yet (the poller stores them next): shown unregistered.
    devices.extend(
        seen.into_values()
            .filter(|d| !rows.contains_key(&d.resource)),
    );
    derive_sessions(&devices, &state.rows, &mut state.sessions);
    for d in &mut devices {
        d.shutting_down = state.shutting_down.contains_key(&d.resource);
        d.removing = state.removing.contains_key(&d.resource);
        d.booted_by_hostler = state.booted_by_hostler.contains(&d.resource);
        d.telemetry = state.telemetry.get(&d.resource).cloned();
        d.sessions = sessions_on(&d.resource, &state.sessions, &state.workspaces);
        d.checking_sessions = checking(&d.resource, &state.sessions);
    }
    devices.sort_by(|a, b| a.resource.cmp(&b.resource));
    state.devices = devices;
    state.unidentified = unidentified;
    state.tools = Some(tool_status(state));
}

#[allow(clippy::too_many_arguments)]
fn blank(
    resource: ResourceId,
    hardware_id: String,
    platform: Platform,
    kind: DeviceKind,
    model: Option<String>,
    name: Option<String>,
    os_version: Option<String>,
    transports: Vec<Transport>,
    connected: bool,
    ready: bool,
) -> DeviceInfo {
    DeviceInfo {
        resource,
        hardware_id,
        platform,
        kind,
        registered: false,
        alias: None,
        alias_by_person: false,
        model: model.unwrap_or_default(),
        name,
        os_version,
        note: None,
        connected,
        ready,
        transports,
        coredevice_id: None,
        telemetry: None,
        booted_by_hostler: false,
        shutting_down: false,
        removing: false,
        sessions: Vec::new(),
        checking_sessions: false,
        first_seen_ms: 0,
        last_seen_ms: 0,
    }
}

/// The store's facts where discovery has none, and the registry.
fn apply_row(info: &mut DeviceInfo, row: &DeviceRow) {
    info.registered = row.registered();
    info.alias = row.alias.clone();
    info.alias_by_person = row.alias_by_person;
    info.note = row.note.clone();
    if info.model.is_empty() {
        info.model = row.model.clone();
    }
    if info.name.is_none() {
        info.name = row.name.clone();
    }
    if info.os_version.is_none() {
        info.os_version = row.os_version.clone();
    }
    if info.coredevice_id.is_none() {
        info.coredevice_id = row.coredevice_id.clone();
    }
    info.first_seen_ms = row.first_seen_ms;
    info.last_seen_ms = row.last_seen_ms;
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// `device:android:avd:<name>`, as `android::resource_of` builds it for a running AVD.
pub fn avd_resource(name: &str) -> ResourceId {
    ResourceId(format!("device:android:avd:{}", name.trim()))
}

/// The hardware id part of a device resource id.
fn hardware_of(r: &ResourceId) -> String {
    let rest = r.0.strip_prefix("device:").unwrap_or(&r.0);
    let rest = rest.split_once(':').map_or(rest, |(_, h)| h);
    rest.strip_prefix("avd:").unwrap_or(rest).to_owned()
}

fn sessions_on(
    r: &ResourceId,
    sessions: &Sessions,
    workspaces: &[(String, String)],
) -> Vec<AgentDeviceSession> {
    sessions
        .claims
        .iter()
        .filter(|f| f.resource == *r)
        .map(|f| {
            let field = |k: &str| {
                f.stop
                    .as_ref()
                    .and_then(|s| s.get(k))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            };
            let path = f.workspace_path.clone().unwrap_or_default();
            AgentDeviceSession {
                session: field("session").unwrap_or_default(),
                workspace_id: crate::claims::workspace_match::owning_workspace(
                    workspaces,
                    &path,
                    cfg!(windows),
                )
                .map(str::to_owned),
                workspace_path: path,
                seen_since_ms: f.since_ms.unwrap_or(0),
                kept: f
                    .stop
                    .as_ref()
                    .and_then(|s| s.get("kept"))
                    .and_then(|k| k.as_bool())
                    .unwrap_or(false),
                platform: if field("platform").as_deref() == Some("ios") {
                    Platform::Ios
                } else {
                    Platform::Android
                },
                agent_device_id: field("id").unwrap_or_default(),
            }
        })
        .collect()
}

/// L52: whether a fresh sessions read is still owed for `r` (ignoring who ended its claim).
fn checking(r: &ResourceId, sessions: &Sessions) -> bool {
    if sessions.forgotten.contains(r) {
        return false;
    }
    match sessions.freshness {
        Freshness::NotTracked => false,
        Freshness::NotYet => true,
        Freshness::Since(seq) => sessions.ends.get(r).copied().unwrap_or(1) > seq,
    }
}

fn path_text(p: &Option<PathBuf>) -> Option<String> {
    p.as_ref().map(|p| p.display().to_string())
}

fn tool_status(state: &CacheState) -> ToolStatus {
    let paths = &state.tool_paths;
    let mut problems = paths.problems.clone();
    let observed = &state.observed;
    problems.extend(
        [
            &observed.android_problem,
            &observed.devicectl_problem,
            &observed.simctl_problem,
        ]
        .into_iter()
        .flatten()
        .cloned(),
    );
    problems.extend(observed.round_problems.values().cloned());
    // The contract has no "unknown" state: before the first check the state is `Missing`, never
    // `Ours` (P3bT5-6), and the problem says why.
    let adb_state = match (&paths.adb, &observed.adb_state) {
        (None, _) => AdbState::Missing,
        (Some(_), Some(s)) => s.clone(),
        (Some(_), None) => {
            problems.push("adb: not checked yet".into());
            AdbState::Missing
        }
    };
    if let AdbState::Foreign { version } = adb_state {
        // Version 0: another adb's client said so, without the version (P3bT5-13).
        let which = if version == 0 {
            "another adb".to_owned()
        } else {
            format!("another adb (version {version})")
        };
        problems.push(format!(
            "{which} owns the adb server; point hostler at it: hostler devices tools --adb <path>"
        ));
    }
    // From the first failed read (pallando, Task 7): the waiting line and every grant line
    // name the reason and the fix at once, not 30 s later.
    let unknown = state.sessions.unreadable_since.is_some();
    if unknown {
        problems.push(format!(
            "agent-device sessions unknown: {}; {}",
            state
                .sessions
                .reason
                .as_deref()
                .unwrap_or("its status cannot be read"),
            super::agent_device::STATUS_FIX
        ));
    }
    if let Some(warning) = &state.sessions.version_warning {
        problems.push(warning.clone());
    }
    problems.extend(state.sessions.unmapped.iter().cloned());
    ToolStatus {
        adb: path_text(&paths.adb),
        adb_state,
        emulator: path_text(&paths.emulator),
        xcrun: paths.xcrun.is_some(),
        ideviceinfo: path_text(&paths.ideviceinfo),
        agent_device: state.sessions.version.clone(),
        node: paths
            .agent_device
            .as_ref()
            .map(|a| a.node.display().to_string()),
        agent_device_in_use: state.sessions.in_use,
        sessions_known: !unknown,
        sessions_last_good_ms: state.sessions.last_good_ms,
        sessions_reason: state
            .sessions
            .unreadable_since
            .and(state.sessions.reason.clone()),
        problems,
    }
}

// ---------------------------------------------------------------------------------------------
// The service

pub struct DevicesService {
    /// The 4th connection; held only for one short statement at a time.
    store: Mutex<Store>,
    cache: Arc<DeviceCache>,
    settings: RwLock<ToolSettings>,
    guard: RwLock<Arc<AdbGuard>>,
    gate: ToolGate,
    events: broadcast::Sender<Event>,
    clock: Arc<dyn Clock>,
    claims_view: RwLock<ClaimsView>,
    /// The server's tick loop selects on it (P3-15).
    tick: Notify,
    /// A fresh devicectl output path per call, in this service's own directory (iOS only;
    /// removed when the service ends).
    json_dir: PathBuf,
    json_seq: AtomicU64,
    ios: bool,
    /// Identities by `seen_key`, and the keys being read now.
    identities: Mutex<HashMap<SeenKey, Known>>,
    in_flight: Mutex<HashSet<String>>,
    /// Telemetry and snapshot calls running or waiting, one per device and kind (P3bT5-4).
    jobs: Arc<Mutex<HashSet<(ResourceId, Job)>>>,
    /// Devices `add` registered whose first (seeding) snapshot is owed.
    seeds: Mutex<Vec<ResourceId>>,
    long_wake: Notify,
    long_passed: std::sync::atomic::AtomicBool,
    last_event: Mutex<(Option<DevicesSnapshot>, i64)>,
    /// The catalog as the claims engine last could have read it.
    last_catalog: Mutex<Vec<ResourceInfo>>,
    /// One sessions cycle at a time (the loop, `refresh_now`), with the last one's number and
    /// answer.
    sessions_cycle: tokio::sync::Mutex<Option<(u64, super::agent_device::SessionsPoll)>>,
    /// Sessions cycles started, counted under `sessions_cycle` as each starts.
    cycles: AtomicU64,
    /// The sessions JSON stored last (P2-19: written only when it changes).
    persisted: Mutex<Option<String>>,
    /// Task 9b's reader state (first-seen times, the version check).
    pub(crate) agent: Mutex<super::agent_device::ReaderState>,
    /// Task 7: boots in flight, shutdowns, and the engine's `device_in_use` hook.
    boots: super::boot::Boots,
    /// Task 8: the windows' snapshots and each device's last snapshot and telemetry attempt.
    book: super::windows::Book,
}

impl DevicesService {
    pub fn new(
        store: Store,
        tools: ToolPaths,
        events: broadcast::Sender<Event>,
    ) -> (Arc<Self>, Arc<DeviceCache>) {
        DevicesService::with_clock(store, tools, events, Arc::new(SystemClock))
    }

    pub fn with_clock(
        store: Store,
        tools: ToolPaths,
        events: broadcast::Sender<Event>,
        clock: Arc<dyn Clock>,
    ) -> (Arc<Self>, Arc<DeviceCache>) {
        let settings = read_settings(&store);
        let cache = Arc::new(DeviceCache::default());
        {
            let mut state = cache.write();
            state.rows = store.devices().unwrap_or_default();
            state.workspaces = workspaces_of(&store);
            state.tool_paths = tools.clone();
            state.at_ms = clock.now_ms();
            if tracks_sessions() {
                // L52: after a start, nothing is granted before the first good read. Never
                // restored from the store (P3-3).
                state.sessions.freshness = Freshness::NotYet;
                // L51: the last good read's sessions block again until a good read says
                // otherwise, even if the status cannot be read after a restart.
                state.sessions.raw = store
                    .agent_device_last()
                    .ok()
                    .flatten()
                    .and_then(|(json, _)| serde_json::from_str::<Vec<RawSession>>(&json).ok())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mut r| {
                        r.kept = true;
                        r
                    })
                    .collect();
            }
            rebuild(&mut state);
        }
        let guard = Arc::new(AdbGuard::new(adb_addr(&settings), tools.adb.clone()));
        let json_dir = std::env::temp_dir().join(format!(
            "hostler-devices-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let svc = Arc::new(DevicesService {
            store: Mutex::new(store),
            cache: cache.clone(),
            settings: RwLock::new(settings),
            guard: RwLock::new(guard),
            gate: ToolGate::new(),
            events,
            clock,
            claims_view: RwLock::new(ClaimsView::default()),
            tick: Notify::new(),
            json_dir,
            json_seq: AtomicU64::new(0),
            ios: cfg!(target_os = "macos")
                || (cfg!(debug_assertions)
                    && std::env::var_os(TEST_PLATFORM_IOS_ENV).is_some_and(|v| v == "1")),
            identities: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashSet::new()),
            jobs: Arc::new(Mutex::new(HashSet::new())),
            seeds: Mutex::new(Vec::new()),
            long_wake: Notify::new(),
            long_passed: std::sync::atomic::AtomicBool::new(false),
            last_event: Mutex::new((None, 0)),
            last_catalog: Mutex::new(Vec::new()),
            sessions_cycle: tokio::sync::Mutex::new(None),
            cycles: AtomicU64::new(0),
            persisted: Mutex::new(None),
            agent: Mutex::new(super::agent_device::ReaderState::default()),
            boots: super::boot::Boots::default(),
            book: super::windows::Book::default(),
        });
        (svc, cache)
    }

    pub fn cache(&self) -> &Arc<DeviceCache> {
        &self.cache
    }

    /// Task 8's memory of windows, snapshots and attempts.
    pub(crate) fn book(&self) -> &super::windows::Book {
        &self.book
    }

    /// Task 8: a snapshot of `r` at the next snapshot pass (a window opened or closed).
    pub(crate) fn snapshot_soon(&self, r: &ResourceId) {
        self.seeds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(r.clone());
        self.long_wake.notify_one();
    }

    /// Task 8: run the snapshot pass now; what it takes is what is due then (an iOS claim-open
    /// snapshot, F-3), so a request made while one is under way never adds a second.
    pub(crate) fn wake_snapshots(&self) {
        self.long_wake.notify_one();
    }

    /// Task 8: `devices.installed`.
    pub(crate) fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// A fresh path in this service's own directory for a tool's JSON output.
    pub(crate) fn json_path(&self, name: &str) -> PathBuf {
        let n = self.json_seq.fetch_add(1, Ordering::SeqCst);
        let _ = std::fs::create_dir_all(&self.json_dir);
        self.json_dir.join(format!("{name}-{n}.json"))
    }

    /// The tick loop selects on it: a sessions read cleared a device (P3-15).
    pub fn tick_notify(&self) -> &Notify {
        &self.tick
    }

    pub fn gate(&self) -> &ToolGate {
        &self.gate
    }

    pub fn adb_guard(&self) -> Arc<AdbGuard> {
        self.guard.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn tools(&self) -> ToolPaths {
        self.cache.read().tool_paths.clone()
    }

    pub fn settings(&self) -> ToolSettings {
        self.settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    pub fn boots(&self) -> &super::boot::Boots {
        &self.boots
    }

    pub(crate) fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn claims_view(&self) -> ClaimsView {
        self.claims_view
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Every `ClaimsChanged`, in order: the view at once, then Task 8's claim windows and Task
    /// 7's ended-hold lines on the blocking pool (store I/O never runs on a runtime worker, R1-4).
    /// Only the claims feed awaits it; the claims engine never does.
    pub async fn on_claims(self: &Arc<Self>, snapshot: &ClaimsSnapshot) {
        let view = ClaimsView::from_snapshot(snapshot);
        let before = std::mem::replace(
            &mut *self.claims_view.write().unwrap_or_else(|e| e.into_inner()),
            view.clone(),
        );
        let (svc, snapshot, now) = (self.clone(), snapshot.clone(), self.now());
        let pass = tokio::task::spawn_blocking(move || {
            super::windows::on_claims(&svc, &snapshot, now);
            // Task 7 (L49): an expired or reaped hold's open sessions go to the log and history.
            super::boot::claims_ended(&svc, &before, &view);
        });
        if let Err(e) = pass.await {
            tracing::error!("the claim windows pass failed: {e}");
        }
    }

    /// Each claims tick: Task 7's shutdown rule.
    pub fn on_tick(self: &Arc<Self>) {
        let view = self.claims_view();
        super::boot::tick(self, &view, self.now());
    }

    // ---- the registry ----

    /// `devices.list`: registered devices and new physical ones; with `all`, also unregistered
    /// virtual devices and unidentified transports.
    pub fn list(&self, all: bool) -> DevicesSnapshot {
        self.recheck_foreign();
        let mut snapshot = self.cache.snapshot();
        if !all {
            snapshot
                .devices
                .retain(|d| d.registered || d.kind == DeviceKind::Physical);
            snapshot.unidentified.clear();
        }
        snapshot
    }

    /// Resolution order (D2): alias, hardware id, the resource id, a current adb transport,
    /// UDID, CoreDevice identifier. Any known device.
    fn find(&self, device: &str) -> Option<DeviceInfo> {
        let state = self.cache.read();
        let devices = &state.devices;
        let by = |f: &dyn Fn(&DeviceInfo) -> bool| devices.iter().find(|d| f(d)).cloned();
        by(&|d| {
            d.registered
                && d.alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(device))
        })
        .or_else(|| by(&|d| d.hardware_id == device && !device.is_empty()))
        .or_else(|| by(&|d| d.resource.0 == device))
        .or_else(|| {
            by(&|d| {
                d.platform == Platform::Android && d.transports.iter().any(|t| t.address == device)
            })
        })
        .or_else(|| by(&|d| d.platform == Platform::Ios && d.hardware_id == device))
        .or_else(|| by(&|d| d.coredevice_id.as_deref() == Some(device) && !device.is_empty()))
    }

    fn found(&self, device: &str) -> Result<DeviceInfo, HostlerError> {
        self.find(device)
            .ok_or_else(|| errors::device_not_found(device))
    }

    fn registered(&self, device: &str) -> Result<DeviceInfo, HostlerError> {
        let info = self.found(device)?;
        if !info.registered {
            return Err(errors::device_not_registered(
                device,
                info.kind == DeviceKind::Physical,
            ));
        }
        Ok(info)
    }

    pub fn show(&self, device: &str) -> Result<DeviceDetail, HostlerError> {
        let info = self.found(device)?;
        let store = self.store();
        Ok(DeviceDetail {
            apps: store.device_apps(&info.resource).unwrap_or_default(),
            snapshot_at_ms: store.device_snapshot_at(&info.resource).ok().flatten(),
            forgets: store.session_forgets(&info.resource).unwrap_or_default(),
            info,
        })
    }

    /// `devices.installs`: the history, of one device (any resolution form) or all.
    pub fn installs(
        &self,
        device: Option<&str>,
        q: &crate::contract::heavy::HistoryQuery,
    ) -> Result<Vec<crate::contract::devices::InstallEvent>, HostlerError> {
        let resource = match device {
            Some(d) => Some(self.found(d)?.resource),
            None => None,
        };
        self.store()
            .device_installs(resource.as_ref(), q)
            .map_err(internal)
    }

    /// `devices.resolve`: a registered device with its transport as of now.
    pub fn resolve(&self, device: &str) -> Result<DeviceInfo, HostlerError> {
        self.registered(device)
    }

    pub fn add(
        &self,
        device: &str,
        alias: Option<&str>,
        person: bool,
    ) -> Result<DeviceInfo, HostlerError> {
        let info = self.found(device)?;
        if info.kind == DeviceKind::Physical && !person {
            return Err(errors::person_required(&format!("add {device}")));
        }
        if info.registered && alias.is_none_or(|a| info.alias.as_deref() == Some(a)) {
            return Ok(info);
        }
        let alias = match alias {
            Some(a) => {
                self.check_alias(a, &info.resource)?;
                a.to_owned()
            }
            None => self.default_alias(&info),
        };
        self.ensure_row(&info)?;
        let now = self.now();
        let stored = self
            .store()
            .register_device(&info.resource, &alias, person, now)
            .map_err(internal)?;
        if !stored {
            return Err(errors::device_not_found(device));
        }
        self.reload_rows();
        // The first snapshot seeds the apps without events (Task 8): on the Long lane, never
        // here on the request (P3bT5-4).
        self.seeds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(info.resource.clone());
        self.long_wake.notify_one();
        self.publish();
        self.cache
            .info(&info.resource)
            .ok_or_else(|| errors::device_not_found(device))
    }

    pub fn rename(
        &self,
        device: &str,
        alias: &str,
        person: bool,
    ) -> Result<DeviceInfo, HostlerError> {
        let info = self.registered(device)?;
        if info.kind == DeviceKind::Physical && !person {
            return Err(errors::person_required(&format!("rename {device} {alias}")));
        }
        if info.alias.as_deref() != Some(alias) {
            self.check_alias(alias, &info.resource)?;
        }
        self.store()
            .register_device(&info.resource, alias, true, self.now())
            .map_err(internal)?;
        self.reload_rows();
        self.publish();
        self.cache
            .info(&info.resource)
            .ok_or_else(|| errors::device_not_found(device))
    }

    pub fn note(&self, device: &str, note: Option<&str>) -> Result<DeviceInfo, HostlerError> {
        let info = self.found(device)?;
        if note.is_some_and(|n| n.len() > 1024 || n.chars().any(char::is_control)) {
            return Err(bad_request(
                "a device note is at most 1024 bytes of text, without control characters".into(),
            ));
        }
        self.ensure_row(&info)?;
        self.store()
            .set_device_note(&info.resource, note.filter(|n| !n.is_empty()))
            .map_err(internal)?;
        self.reload_rows();
        self.publish();
        self.cache
            .info(&info.resource)
            .ok_or_else(|| errors::device_not_found(device))
    }

    /// Step 1 of `devices remove` (P2-14): checks the device and the person, then marks it
    /// `removing` (the catalog lists it unavailable). The caller asks the engine's
    /// `device_in_use` next and passes it to `remove`.
    pub fn begin_remove(&self, device: &str, person: bool) -> Result<ResourceId, HostlerError> {
        let info = self.registered(device)?;
        if info.kind == DeviceKind::Physical && !person {
            return Err(errors::person_required(&format!("remove {device}")));
        }
        self.cache.set_removing(&info.resource, true);
        Ok(info.resource)
    }

    /// Step 3, on the device `begin_remove` marked and the engine was asked about (never the
    /// request's text again, which may name another device by now: P3bT5-5): refused while a
    /// claim or a waiting request is on it (P-14); else it is unregistered and stays known. That
    /// one `removing` mark ends either way.
    pub fn remove(&self, r: &ResourceId, person: bool, in_use: InUse) -> Result<(), HostlerError> {
        let result = match self.cache.info(r) {
            Some(info) if info.registered => self.unregister(&info, person, &in_use),
            Some(info) => Err(errors::device_not_registered(
                &r.0,
                info.kind == DeviceKind::Physical,
            )),
            None => Err(errors::device_not_found(&r.0)),
        };
        self.cache.set_removing(r, false);
        result?;
        self.reload_rows();
        self.publish();
        Ok(())
    }

    fn unregister(
        &self,
        info: &DeviceInfo,
        person: bool,
        in_use: &InUse,
    ) -> Result<(), HostlerError> {
        let device = info
            .alias
            .clone()
            .unwrap_or_else(|| info.resource.0.clone());
        if info.kind == DeviceKind::Physical && !person {
            return Err(errors::person_required(&format!("remove {device}")));
        }
        if !in_use.is_free() {
            let display = device.clone();
            let holder = in_use
                .claims
                .first()
                .map(|c| c.workspace_path.clone())
                .unwrap_or_else(|| format!("{} waiting request(s)", in_use.waiting));
            return Err(claim_errors::resource_in_use(&display, &holder));
        }
        self.store()
            .unregister_device(&info.resource)
            .map_err(internal)?;
        Ok(())
    }

    /// `devices.tools`: records what is given (a `None` keeps the setting), searches the tools
    /// again, and answers the status. Paths must be absolute.
    pub fn set_tools(&self, set: ToolPathsSet) -> Result<ToolStatus, HostlerError> {
        let paths = [
            (keys::ADB_PATH, &set.adb),
            (keys::AGENT_DEVICE_PATH, &set.agent_device),
            (keys::AGENT_DEVICE_SCRIPT, &set.detected_script),
            (keys::NODE_PATH, &set.node),
        ];
        for (_, value) in &paths {
            if let Some(v) = value
                && !v.is_empty()
                && !Path::new(v).is_absolute()
            {
                return Err(bad_request(format!("{v} is not an absolute path")));
            }
        }
        for (name, value) in &set.android_env {
            if !RECORDED_ENV.contains(&name.as_str()) {
                return Err(bad_request(format!(
                    "{name} is not an Android variable hostler records"
                )));
            }
            // A path-valued Android variable must be absolute; the port/socket names are not
            // paths. An empty value clears the setting.
            if !value.is_empty() && path_valued_env(name) && !Path::new(value).is_absolute() {
                return Err(bad_request(format!(
                    "{name} is not an absolute path: {value}"
                )));
            }
        }
        {
            let store = self.store();
            for (key, value) in paths {
                if let Some(v) = value {
                    store
                        .set_setting(key, Some(v.as_str()).filter(|v| !v.is_empty()))
                        .map_err(internal)?;
                }
            }
            for (name, value) in &set.android_env {
                store
                    .set_setting(
                        &format!("{}{name}", keys::ENV_PREFIX),
                        Some(value.as_str()).filter(|v| !v.is_empty()),
                    )
                    .map_err(internal)?;
            }
        }
        self.refresh_tools();
        Ok(self.cache.snapshot().tools)
    }

    /// Reads the settings and searches the tools again; a new adb path or address gets a new
    /// guard (P2-2). An unchanged adb path and address keep the observed adb state, so a settings
    /// write does not hide another adb that owns the server.
    pub fn refresh_tools(&self) {
        let settings = read_settings(&self.store());
        let paths = find_service_tools(&settings);
        let old_settings = self.settings();
        let same_adb = self.cache.read().tool_paths.adb == paths.adb
            && adb_addr(&old_settings) == adb_addr(&settings);
        *self.guard.write().unwrap_or_else(|e| e.into_inner()) =
            Arc::new(AdbGuard::new(adb_addr(&settings), paths.adb.clone()));
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = settings;
        let mut state = self.cache.write();
        state.tool_paths = paths;
        // An unchanged adb path and address keep the observed adb state (P3bT6-3) ...
        if !same_adb {
            state.observed.adb_state = None;
        }
        // ... but what adb listed before the change is never this tools state's answer: the
        // next discovery round lists them again (P3bT5-6).
        state.observed.forget_android();
        rebuild(&mut state);
        drop(state);
        // ... and a kept `Foreign` is asked again now, so this answer is current (P22-1).
        self.recheck_foreign();
    }

    /// P22-1: while adb reads `Foreign`, the server is asked again at once (one socket read,
    /// never a kill): `ours` when it answers as hostler's version, not checked yet when it is
    /// gone (the next round starts hostler's own), another version's state otherwise. Anything
    /// else the guard says is left to the next discovery round.
    fn recheck_foreign(&self) {
        let foreign = |s: &Option<AdbState>| matches!(s, Some(AdbState::Foreign { .. }));
        if !foreign(&self.cache.read().observed.adb_state) {
            return;
        }
        let guard = self.adb_guard();
        let now = match guard.recheck() {
            // The answer the recheck just cached: no second read.
            Ok(()) if guard.server() == ServerSeen::None => None,
            Ok(()) => Some(AdbState::Ours),
            Err(refused @ Refused::Foreign(_)) => Some(refused.state()),
            Err(_) => return,
        };
        let mut state = self.cache.write();
        if !foreign(&state.observed.adb_state) || state.observed.adb_state == now {
            return;
        }
        state.observed.adb_state = now;
        rebuild(&mut state);
        drop(state);
        self.publish();
    }

    /// Searches for agent-device alone (L54, P3bT9b-8): adb's guard and state stay as they are.
    pub fn search_agent_device(&self) {
        let found = find_service_tools(&self.settings()).agent_device;
        let mut state = self.cache.write();
        if state.tool_paths.agent_device != found {
            state.tool_paths.agent_device = found;
            rebuild(&mut state);
        }
    }

    fn ensure_row(&self, info: &DeviceInfo) -> Result<(), HostlerError> {
        if self
            .cache
            .read()
            .rows
            .iter()
            .any(|r| r.resource == info.resource)
        {
            return Ok(());
        }
        self.store()
            .upsert_device_seen(&seen_of(info), self.now())
            .map_err(internal)?;
        Ok(())
    }

    fn reload_rows(&self) {
        let rows = self.store().devices().unwrap_or_default();
        let mut state = self.cache.write();
        state.rows = rows;
        state.at_ms = self.now();
        rebuild(&mut state);
    }

    /// P-14: an alias is never one of the ids a device answers to, nor taken by another device.
    fn check_alias(&self, alias: &str, own: &ResourceId) -> Result<(), HostlerError> {
        if !valid_alias(alias) {
            return Err(bad_request(format!(
                "{alias} is not an alias: lowercase letters, digits, '.', '_' and '-', starting \
                 with a letter or digit, at most 32"
            )));
        }
        if self.id_like(alias, own) {
            return Err(bad_request(format!(
                "{alias} looks like a device id or transport; an alias must not (it is the \
                 --confirmation token)"
            )));
        }
        let state = self.cache.read();
        if let Some(holder) = state.rows.iter().find(|r| {
            r.resource != *own
                && r.alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(alias))
        }) {
            let name = holder.alias.clone().unwrap_or_default();
            return Err(errors::alias_taken(alias, &name));
        }
        Ok(())
    }

    /// P-14: the forms of `emulator-<n>` and `<host>:<port>`, and every id another device answers
    /// to. A device's own hardware id is no conflict (an AVD's default alias is its name).
    fn id_like(&self, alias: &str, own: &ResourceId) -> bool {
        if let Some(n) = alias.strip_prefix("emulator-")
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            return true;
        }
        if alias.contains(':') {
            return true;
        }
        let state = self.cache.read();
        let same = |id: &str| !id.is_empty() && id.eq_ignore_ascii_case(alias);
        state.devices.iter().any(|d| {
            let mine = d.resource == *own;
            (!mine && same(&d.hardware_id))
                || d.coredevice_id.as_deref().is_some_and(same)
                || d.transports
                    .iter()
                    .any(|t| same(&t.address) && !(mine && t.address == d.hardware_id))
        }) || state
            .rows
            .iter()
            .filter(|r| r.resource != *own)
            .any(|r| r.serials.iter().any(|s| same(&s.serial)))
            || state
                .unidentified
                .iter()
                .any(|u| same(&u.transport.address))
    }

    /// The model, AVD or simulator name slugged, with `-2`, `-3` … on a collision.
    fn default_alias(&self, info: &DeviceInfo) -> String {
        let base = match info.kind {
            DeviceKind::Emulator => info.hardware_id.clone(),
            DeviceKind::Simulator => info.name.clone().unwrap_or_else(|| info.model.clone()),
            DeviceKind::Physical => info.model.clone(),
        };
        let base = slug(&base);
        let taken = |a: &str| {
            self.id_like(a, &info.resource)
                || self.cache.read().rows.iter().any(|r| {
                    r.resource != info.resource
                        && r.alias
                            .as_deref()
                            .is_some_and(|x| x.eq_ignore_ascii_case(a))
                })
        };
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|n| {
                let suffix = format!("-{n}");
                let mut head = base.clone();
                head.truncate(32 - suffix.len());
                format!("{}{suffix}", head.trim_end_matches(['-', '.', '_']))
            })
            .find(|a| valid_alias(a) && !taken(a))
            .expect("some suffix is free")
    }

    // ---- the poller ----

    /// Runs every loop for the service's lifetime. The sessions loop reads once before
    /// anything else (L52).
    pub async fn run_poller(self: Arc<Self>) {
        if self.ios {
            let _ = std::fs::create_dir_all(&self.json_dir);
        }
        self.sessions_round().await;
        let mut loops = tokio::task::JoinSet::new();
        let svc = self.clone();
        loops.spawn(async move {
            svc.repeat(every(ANDROID_EVERY), Loop::Android, Self::android_round)
                .await
        });
        let svc = self.clone();
        loops.spawn(async move {
            svc.repeat(every(AVDS_EVERY), Loop::Avds, Self::avds_round)
                .await
        });
        if self.ios {
            let svc = self.clone();
            loops.spawn(async move {
                svc.repeat(every(IOS_EVERY), Loop::Ios, Self::ios_round)
                    .await
            });
        }
        let svc = self.clone();
        loops.spawn(async move { svc.sessions_loop().await });
        // Telemetry (Probe) and snapshots (Long) never wait on each other (P3bT5-4).
        let svc = self.clone();
        loops.spawn(async move {
            svc.repeat(every(SCHEDULE_EVERY), Loop::Telemetry, Self::probe_pass)
                .await
        });
        let svc = self.clone();
        loops.spawn(async move { svc.long_loop().await });
        let svc = self.clone();
        loops.spawn(async move {
            svc.repeat(every(ALIVE_EVERY), Loop::Alive, Self::alive_round)
                .await
        });
        while loops.join_next().await.is_some() {}
    }

    /// One round per `period`, each its own task: a panicking round counts as a failed call of
    /// its tool, and the loop goes on (P3bT5-11).
    async fn repeat<F, Fut>(self: Arc<Self>, period: Duration, which: Loop, round: F)
    where
        F: Fn(Arc<Self>) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut ticks = tokio::time::interval(period);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let done = tokio::spawn(round(self.clone())).await;
            self.round_ended(which, done.err());
        }
    }

    /// A round's end: a panic is logged and shown, and counts as a failed call; a completed
    /// round clears that.
    fn round_ended(&self, which: Loop, failed: Option<tokio::task::JoinError>) {
        let now = self.now();
        let mut state = self.cache.write();
        match failed {
            None => {
                if state.observed.round_problems.remove(which.name()).is_none() {
                    return;
                }
            }
            Some(e) => {
                tracing::error!("the device {} round failed: {e}", which.name());
                state.observed.round_problems.insert(
                    which.name(),
                    format!(
                        "the device {} round failed; see the service log",
                        which.name()
                    ),
                );
                match which {
                    Loop::Android => {
                        state.observed.android_failing_since.get_or_insert(now);
                    }
                    Loop::Ios => {
                        state.observed.devicectl_failing_since.get_or_insert(now);
                        state.observed.simctl_failing_since.get_or_insert(now);
                    }
                    _ => {}
                }
            }
        }
        state.at_ms = now;
        rebuild(&mut state);
        drop(state);
        self.publish();
    }

    fn env_pairs(&self) -> Vec<(String, OsString)> {
        self.settings()
            .env
            .into_iter()
            .map(|(k, v)| (k, OsString::from(v)))
            .collect()
    }

    /// One allowed call with the recorded Android variables; the adb guard is asked inside.
    pub(crate) fn call(
        &self,
        tool: Tool,
        path: &Path,
        args: &[&OsStr],
        timeout: Duration,
    ) -> Option<ToolOutput> {
        self.call_inner(tool, path, args, None, timeout)
    }

    /// `call`, writing `stdin` to the tool (Task 8: `plutil -convert … -`).
    pub(crate) fn call_with_stdin(
        &self,
        tool: Tool,
        path: &Path,
        args: &[&OsStr],
        stdin: &[u8],
        timeout: Duration,
    ) -> Option<ToolOutput> {
        self.call_inner(tool, path, args, Some(stdin), timeout)
    }

    fn call_inner(
        &self,
        tool: Tool,
        path: &Path,
        args: &[&OsStr],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Option<ToolOutput> {
        let env = self.env_pairs();
        // Only the names this tool may get (P3bT2-6): xcrun gets none.
        let env: Vec<(&str, &OsStr)> = env
            .iter()
            .filter(|(k, _)| tools::env_allowed(tool, k))
            .map(|(k, v)| (k.as_str(), v.as_os_str()))
            .collect();
        // A fresh pass right before each adb call (P3-10, P3bT2-8); a refused guard spawns nothing.
        let adb_guard = if tool == Tool::Adb {
            match super::probes::adb_guard(self) {
                Ok(pass) => Some(pass),
                Err(refused) => {
                    tracing::debug!("adb guard: {refused:?}");
                    return None;
                }
            }
        } else {
            None
        };
        let out = tools::run_tool(&Call {
            tool,
            path,
            args,
            env: &env,
            stdin,
            timeout,
            adb_breakaway: tool == Tool::Adb,
            adb_guard,
        });
        let out = out.ok()?;
        if tool == Tool::Adb
            && let Some(text) = tools::adb_mismatch(&out.stderr)
        {
            let version = text
                .split(['(', ')'])
                .nth(1)
                .and_then(|v| v.trim().parse().ok());
            self.adb_guard().saw_mismatch(version);
            return None;
        }
        Some(out)
    }

    /// `adb devices -l`, applied at once; identity and boot reads run on their own and apply
    /// the last answer again when they finish, so one hung phone never holds back another
    /// (P3bT5-3).
    async fn android_round(self: Arc<Self>) {
        let Some(adb) = self.tools().adb else {
            self.android_gone(self.now());
            return;
        };
        let svc = self.clone();
        let result = self
            .gate
            .run(Lane::DiscoveryAndroid, None, move || svc.adb_devices(&adb))
            .await;
        match result {
            AdbRound::Lines(lines) => {
                self.apply_android(&lines, self.now());
                self.spawn_identities(&lines);
                self.spawn_boot_reads();
            }
            AdbRound::Refused(refused) => self.android_refused(refused, self.now()),
            AdbRound::Failed(why) => self.android_failed(&why, self.now()),
        }
    }

    /// The transports with the identities known now.
    fn merged(&self, lines: &[AdbLine]) -> (Vec<Merged>, Vec<UnidentifiedTransport>) {
        let pairs: Vec<(AdbLine, Option<Identity>)> = {
            let ids = self.identities.lock().unwrap_or_else(|e| e.into_inner());
            lines
                .iter()
                .map(|l| {
                    let id = ids
                        .get(&android::seen_key(l))
                        .and_then(|known| known.id.clone());
                    (l.clone(), id.filter(|_| l.state == "device"))
                })
                .collect()
        };
        android::merge(&pairs)
    }

    /// A good `adb devices -l`: it is kept, the boot reads of transports that are gone are
    /// dropped, and the cache recomputed.
    fn apply_android(&self, lines: &[AdbLine], now: i64) {
        let keys: HashSet<SeenKey> = lines.iter().map(android::seen_key).collect();
        {
            let mut ids = self.identities.lock().unwrap_or_else(|e| e.into_inner());
            for key in &keys {
                if let Some(known) = ids.get_mut(key) {
                    known.listed_ms = now;
                }
            }
            ids.retain(|key, known| keys.contains(key) || now - known.listed_ms < IDENTITY_KEEP_MS);
        }
        {
            let mut state = self.cache.write();
            state.observed.android_lines = Some(lines.to_vec());
            state.observed.booted.retain(|key, _| keys.contains(key));
            state.observed.android_failing_since = None;
            state.observed.android_problem = None;
            state.observed.adb_state = Some(AdbState::Ours);
        }
        self.refresh_android(now);
    }

    /// The last good lines with the identities and boot reads known now: the devices are
    /// stored (unknown ones unregistered) with their transport serials, and the cache replaced.
    /// Nothing when adb's answer was forgotten meanwhile.
    fn refresh_android(&self, now: i64) {
        let (lines, generation) = {
            let mut state = self.cache.write();
            let Some(lines) = state.observed.android_lines.clone() else {
                return;
            };
            state.observed.android_gen += 1;
            (lines, state.observed.android_gen)
        };
        let (merged, unidentified) = self.merged(&lines);
        let ready: HashSet<ResourceId> = {
            let state = self.cache.read();
            merged
                .iter()
                .filter(|m| m.identity.qemu)
                .filter(|m| {
                    boot_key(m, &lines)
                        .is_some_and(|k| state.observed.booted.get(&k) == Some(&true))
                })
                .map(|m| m.resource.clone())
                .collect()
        };
        for m in &merged {
            let hardware = (!m.identity.qemu).then(|| m.identity.serialno.trim().to_owned());
            self.record_seen(&android_seen(m), now);
            for t in &m.transports {
                if let Err(e) = self.store().add_device_serial(
                    &m.resource,
                    hardware.as_deref(),
                    &t.address,
                    now,
                ) {
                    tracing::warn!("cannot record a transport serial: {e:#}");
                }
            }
        }
        let rows = self.store().devices().unwrap_or_default();
        {
            let mut state = self.cache.write();
            // Forgotten, or a newer recomputation is in: this one is stale.
            if state.observed.android_lines.is_none() || state.observed.android_gen != generation {
                return;
            }
            state.rows = rows;
            state.observed.android = Some(AndroidSeen {
                merged,
                unidentified,
                ready,
            });
            state.at_ms = now;
            rebuild(&mut state);
        }
        self.publish();
    }

    /// No adb now (a changed path that finds none): its old answer is forgotten (P3bT5-6).
    fn android_gone(&self, now: i64) {
        let mut state = self.cache.write();
        if state.observed.android_lines.is_none() && state.observed.adb_state.is_none() {
            return;
        }
        state.observed.forget_android();
        state.observed.adb_state = None;
        state.at_ms = now;
        rebuild(&mut state);
        drop(state);
        self.publish();
    }

    /// The guard refused adb (P2-2, P3bT5-1): no adb call ran. Another adb's server: its
    /// devices go at once. A server that cannot be asked, or no client version: a failed call
    /// for the 30 s rule, with a problem at once.
    fn android_refused(&self, refused: Refused, now: i64) {
        let mut state = self.cache.write();
        state.observed.adb_state = Some(refused.state());
        match refused {
            Refused::Foreign(_) => state.observed.forget_android(),
            Refused::ServerUnknown => {
                state.observed.android_failing_since.get_or_insert(now);
                state.observed.android_problem = Some(
                    "the adb server does not answer hostler's version check (host:version); \
                     hostler runs no adb call until it does"
                        .into(),
                );
            }
            Refused::NoClientVersion => {
                state.observed.android_failing_since.get_or_insert(now);
                state.observed.android_problem = Some(
                    "hostler cannot read its own adb's version (adb version); hostler runs no \
                     adb call"
                        .into(),
                );
            }
        }
        state.at_ms = now;
        rebuild(&mut state);
        drop(state);
        self.publish();
    }

    /// A failed or timed-out call; only after 30 s of them are the devices disconnected. A
    /// lane wait never gets here (P-3).
    fn android_failed(&self, why: &str, now: i64) {
        let mut state = self.cache.write();
        let since = *state.observed.android_failing_since.get_or_insert(now);
        if now - since >= stale_ms() {
            state.observed.android_problem = Some(format!("adb devices -l failed for 30 s: {why}"));
        }
        state.at_ms = now;
        rebuild(&mut state);
        drop(state);
        self.publish();
    }

    /// The discovery call (DiscoveryAndroid lane): hostler's own adb version first, then the
    /// guard's socket probe, `start-server` only when the port is closed (P2-2, P3bT5-1), then
    /// `adb devices -l`.
    fn adb_devices(&self, adb: &Path) -> AdbRound {
        let guard = self.adb_guard();
        if guard.client().is_none() {
            return AdbRound::Refused(Refused::NoClientVersion);
        }
        if guard.server() == ServerSeen::None {
            let env = self.env_pairs();
            let env: Vec<(&str, &OsStr)> = env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_os_str()))
                .collect();
            if let Err(e) = tools::ensure_adb_server(adb, &env) {
                tracing::warn!("adb start-server: {e}");
            }
        }
        if let Err(refused) = guard.check() {
            return AdbRound::Refused(refused);
        }
        let args = [OsStr::new("devices"), OsStr::new("-l")];
        let Some(out) = self.call(Tool::Adb, adb, &args, ADB_DEVICES_BOUND) else {
            return match guard.check() {
                Err(refused) => AdbRound::Refused(refused),
                Ok(()) => AdbRound::Failed("adb could not run".into()),
            };
        };
        let text = String::from_utf8_lossy(&out.stdout);
        if out.timed_out {
            return AdbRound::Failed("it timed out".into());
        }
        if out.status != Some(0) || out.truncated || !text.contains("List of devices attached") {
            return AdbRound::Failed(format!("it exited with {:?}", out.status));
        }
        AdbRound::Lines(android::parse_devices_l(&text))
    }

    /// The identity script for every `device` transport whose `seen_key` is new (P-13), with the
    /// `emu avd name` fallback; each its own task on the Probe lane. Its per-device key is the
    /// transport serial: the resource is what the read finds out (P3bT5-13).
    fn spawn_identities(self: &Arc<Self>, lines: &[AdbLine]) {
        let now = self.now();
        let wanted: Vec<AdbLine> = {
            let ids = self.identities.lock().unwrap_or_else(|e| e.into_inner());
            lines
                .iter()
                .filter(|l| l.state == "device")
                .filter(|l| match ids.get(&android::seen_key(l)) {
                    None => true,
                    Some(Known {
                        id: None, read_ms, ..
                    }) => now - read_ms >= IDENTITY_RETRY_MS,
                    Some(Known { id: Some(_), .. }) => false,
                })
                .cloned()
                .collect()
        };
        let Some(adb) = self.tools().adb else {
            return;
        };
        for line in wanted {
            if !self.claim_flight(&line.serial) {
                continue;
            }
            let svc = self.clone();
            let adb = adb.clone();
            tokio::spawn(async move {
                let flight = Flight(svc.clone(), line.serial.clone());
                let key = ResourceId(format!("serial:{}", line.serial));
                let serial = line.serial.clone();
                let id = svc
                    .gate
                    .run(Lane::Probe, Some(&key), {
                        let svc = svc.clone();
                        move || svc.identity(&adb, &serial)
                    })
                    .await;
                let now = svc.now();
                svc.identities
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        android::seen_key(&line),
                        Known {
                            id,
                            read_ms: now,
                            listed_ms: now,
                        },
                    );
                drop(flight);
                svc.refresh_android(now);
                svc.spawn_boot_reads();
            });
        }
    }

    fn claim_flight(&self, key: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_owned())
    }

    fn end_flight(&self, key: &str) {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    /// IDENTITY, never trimmed (a phone's last three lines are empty), then `emu avd name` for
    /// an emulator without `ro.boot.qemu.avd_name`.
    fn identity(&self, adb: &Path, serial: &str) -> Option<Identity> {
        let args = [
            OsStr::new("-s"),
            OsStr::new(serial),
            OsStr::new("shell"),
            OsStr::new(tools::IDENTITY),
        ];
        let out = self.call(Tool::Adb, adb, &args, ADB_SHELL_BOUND)?;
        if !ok(&out) {
            return None;
        }
        let mut id = android::parse_identity(&String::from_utf8_lossy(&out.stdout))?;
        if id.qemu && id.avd_name.is_none() {
            let args = [
                OsStr::new("-s"),
                OsStr::new(serial),
                OsStr::new("emu"),
                OsStr::new("avd"),
                OsStr::new("name"),
            ];
            if let Some(out) = self.call(Tool::Adb, adb, &args, ADB_EMU_BOUND)
                && ok(&out)
            {
                id.avd_name = android::parse_emu_avd_name(&String::from_utf8_lossy(&out.stdout));
            }
        }
        Some(id)
    }

    /// `sys.boot_completed` for every running emulator whose transport is not known ready, each
    /// its own task on the Probe lane.
    fn spawn_boot_reads(self: &Arc<Self>) {
        let Some(adb) = self.tools().adb else {
            return;
        };
        let pending: Vec<(ResourceId, SeenKey)> = {
            let state = self.cache.read();
            let (Some(android), Some(lines)) =
                (&state.observed.android, &state.observed.android_lines)
            else {
                return;
            };
            android
                .merged
                .iter()
                .filter(|m| m.identity.qemu)
                .filter_map(|m| Some((m.resource.clone(), boot_key(m, lines)?)))
                .filter(|(_, key)| state.observed.booted.get(key) != Some(&true))
                .collect()
        };
        for (r, key) in pending {
            let flight = format!("boot:{}:{:?}", key.0, key.1);
            if !self.claim_flight(&flight) {
                continue;
            }
            let svc = self.clone();
            let adb = adb.clone();
            tokio::spawn(async move {
                let done = Flight(svc.clone(), flight);
                let serial = key.0.clone();
                let booted = svc
                    .gate
                    .run(Lane::Probe, Some(&r), {
                        let svc = svc.clone();
                        move || {
                            let args = [
                                OsStr::new("-s"),
                                OsStr::new(&serial),
                                OsStr::new("shell"),
                                OsStr::new(tools::BOOT_COMPLETED),
                            ];
                            svc.call(Tool::Adb, &adb, &args, ADB_SHELL_BOUND)
                                .filter(ok)
                                .is_some_and(|out| {
                                    android::parse_boot_completed(&String::from_utf8_lossy(
                                        &out.stdout,
                                    ))
                                })
                        }
                    })
                    .await;
                {
                    let mut state = svc.cache.write();
                    // Only a transport still listed: a reused serial is a new key (P3bT5-2).
                    let listed = state
                        .observed
                        .android_lines
                        .iter()
                        .flatten()
                        .any(|l| android::seen_key(l) == key);
                    if listed {
                        state.observed.booted.insert(key, booted);
                    }
                }
                drop(done);
                svc.refresh_android(svc.now());
            });
        }
    }

    /// `emulator -list-avds` (every 60 s): a failed call keeps the last list.
    async fn avds_round(self: Arc<Self>) {
        let Some(emulator) = self.tools().emulator else {
            return;
        };
        let svc = self.clone();
        let out = self
            .gate
            .run(Lane::DiscoveryAndroid, None, move || {
                svc.call(
                    Tool::Emulator,
                    &emulator,
                    &[OsStr::new("-list-avds")],
                    LIST_AVDS_BOUND,
                )
            })
            .await;
        let Some(out) = out.filter(ok) else {
            return;
        };
        self.apply_avds(
            android::parse_avds(&String::from_utf8_lossy(&out.stdout)),
            self.now(),
        );
    }

    fn apply_avds(&self, avds: Vec<String>, now: i64) {
        for name in &avds {
            let resource = avd_resource(name);
            // A known AVD keeps what its running emulator reported (P3bT5-12).
            let known = self
                .cache
                .read()
                .rows
                .iter()
                .any(|r| r.resource == resource);
            self.record_seen(
                &DeviceSeen {
                    resource,
                    platform: Platform::Android,
                    kind: DeviceKind::Emulator,
                    model: if known { String::new() } else { name.clone() },
                    name: Some(name.clone()),
                    os_version: None,
                    coredevice_id: None,
                },
                now,
            );
        }
        let rows = self.store().devices().unwrap_or_default();
        {
            let mut state = self.cache.write();
            state.rows = rows;
            state.observed.avds = Some(avds);
            state.at_ms = now;
            rebuild(&mut state);
        }
        self.publish();
    }

    /// `devicectl list devices` and `simctl list devices` (every 10 s, macOS).
    async fn ios_round(self: Arc<Self>) {
        let Some(xcrun) = self.tools().xcrun else {
            return;
        };
        let svc = self.clone();
        let (physical, simulators) = self
            .gate
            .run(Lane::DiscoveryIos, None, move || {
                (svc.devicectl_list(&xcrun), svc.simctl_list(&xcrun))
            })
            .await;
        self.apply_ios(physical, simulators, self.now());
    }

    /// `Err(why)`: that call failed. It counts toward the 30 s rule, after which its devices
    /// are disconnected and the problem says so (P3bT5-10).
    fn apply_ios(
        &self,
        physical: Result<Vec<PhysicalIos>, String>,
        simulators: Result<Vec<Simulator>, String>,
        now: i64,
    ) {
        let seen: Vec<DeviceSeen> = physical
            .iter()
            .flatten()
            .map(|p| DeviceSeen {
                resource: ios::resource_of_physical(p),
                platform: Platform::Ios,
                kind: DeviceKind::Physical,
                model: p.model.clone(),
                name: non_empty(&p.name),
                os_version: p.os_version.clone(),
                coredevice_id: non_empty(&p.coredevice_id),
            })
            .chain(simulators.iter().flatten().map(|s| DeviceSeen {
                resource: ios::resource_of_simulator(s),
                platform: Platform::Ios,
                kind: DeviceKind::Simulator,
                model: s.name.clone(),
                name: non_empty(&s.name),
                os_version: non_empty(&s.runtime),
                coredevice_id: None,
            }))
            .collect();
        for d in &seen {
            self.record_seen(d, now);
        }
        let rows = self.store().devices().unwrap_or_default();
        let stale = stale_ms();
        let failed = |since: &mut Option<i64>, problem: &mut Option<String>, call: &str, why| {
            let at = *since.get_or_insert(now);
            if now - at >= stale {
                *problem = Some(format!("{call} failed for 30 s: {why}"));
            }
        };
        {
            let mut state = self.cache.write();
            state.rows = rows;
            let o = &mut state.observed;
            match physical {
                Ok(p) => {
                    o.physical_ios = Some(p);
                    o.devicectl_failing_since = None;
                    o.devicectl_problem = None;
                }
                Err(why) => failed(
                    &mut o.devicectl_failing_since,
                    &mut o.devicectl_problem,
                    "devicectl list devices",
                    why,
                ),
            }
            match simulators {
                Ok(s) => {
                    o.simulators = Some(s);
                    o.simctl_failing_since = None;
                    o.simctl_problem = None;
                }
                Err(why) => failed(
                    &mut o.simctl_failing_since,
                    &mut o.simctl_problem,
                    "simctl list devices",
                    why,
                ),
            }
            state.at_ms = now;
            rebuild(&mut state);
        }
        self.publish();
    }

    /// devicectl writes its JSON to a file: a fresh path per call, removed before and after; a
    /// missing file, a non-zero status or a timeout is a failed call. One UDID listed twice
    /// keeps its wired entry.
    fn devicectl_list(&self, xcrun: &Path) -> Result<Vec<PhysicalIos>, String> {
        let n = self.json_seq.fetch_add(1, Ordering::SeqCst);
        let path = self.json_dir.join(format!("devicectl-list-{n}.json"));
        let _ = std::fs::create_dir_all(&self.json_dir);
        let _ = std::fs::remove_file(&path);
        let args = [
            OsStr::new("devicectl"),
            OsStr::new("list"),
            OsStr::new("devices"),
            OsStr::new("--json-output"),
            path.as_os_str(),
        ];
        let out = self.call(Tool::Xcrun, xcrun, &args, DEVICECTL_LIST_BOUND);
        let bytes = std::fs::read(&path).ok();
        let _ = std::fs::remove_file(&path);
        failure(out.as_ref()).map_or(Ok(()), Err)?;
        let bytes = bytes.ok_or("it wrote no JSON")?;
        let mut list = ios::parse_devicectl_list(&bytes).ok_or("its JSON cannot be read")?;
        list.sort_by_key(|p| p.transport != TransportKind::Wired);
        let mut seen = HashSet::new();
        list.retain(|p| seen.insert(p.udid.clone()));
        Ok(list)
    }

    fn simctl_list(&self, xcrun: &Path) -> Result<Vec<Simulator>, String> {
        let args = [
            OsStr::new("simctl"),
            OsStr::new("list"),
            OsStr::new("devices"),
            OsStr::new("--json"),
        ];
        let out = self.call(Tool::Xcrun, xcrun, &args, SIMCTL_LIST_BOUND);
        failure(out.as_ref()).map_or(Ok(()), Err)?;
        let out = out.expect("checked by failure");
        ios::parse_simctl_list(&out.stdout).ok_or_else(|| "its JSON cannot be read".to_owned())
    }

    /// Writes a device's facts only when they change what the store has, else `last_seen` once
    /// a minute (P2-19, P3bT5-12). An empty model or a missing OS version never replaces a
    /// stored one (the store's upsert keeps them too).
    fn record_seen(&self, d: &DeviceSeen, now: i64) {
        let unchanged = self
            .cache
            .read()
            .rows
            .iter()
            .find(|row| row.resource == d.resource)
            .is_some_and(|row| {
                let model = if d.model.trim().is_empty() {
                    &row.model
                } else {
                    &d.model
                };
                row.platform == d.platform
                    && row.kind == d.kind
                    && *model == row.model
                    && d.name.as_ref().or(row.name.as_ref()) == row.name.as_ref()
                    && d.os_version.as_ref().or(row.os_version.as_ref()) == row.os_version.as_ref()
                    && d.coredevice_id.as_ref().or(row.coredevice_id.as_ref())
                        == row.coredevice_id.as_ref()
                    && now - row.last_seen_ms < SEEN_WRITE_MS
            });
        if unchanged {
            return;
        }
        if let Err(e) = self.store().upsert_device_seen(d, now) {
            tracing::warn!("cannot record a seen device: {e:#}");
        }
    }

    /// The sessions lane: every 5 s, and at once after a claim end (L52).
    async fn sessions_loop(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(every(SESSIONS_EVERY)) => {}
                _ = self.cache.wake.notified() => {}
            }
            self.sessions_round().await;
            // Outside the cycle, so `refresh_now` never waits for it (P3bT9b-4).
            if session_hook().is_none() {
                super::agent_device::check_version(&self).await;
            }
        }
    }

    /// One sessions cycle: the test hook's file, or agent-device itself (Task 9b); none when
    /// the service does not track sessions (debug builds without device tools).
    pub(crate) async fn sessions_round(self: &Arc<Self>) -> super::agent_device::SessionsPoll {
        let mut cycle = self.sessions_cycle.lock().await;
        let number = self.cycles.fetch_add(1, Ordering::SeqCst) + 1;
        let poll = self.one_cycle().await;
        *cycle = Some((number, poll.clone()));
        poll
    }

    /// A cycle that started after `ticket` (`cycles_started` taken earlier): the one that last
    /// finished when it did, else a new one (P3bT9b-4: a takeover's fresh read reuses a cycle
    /// that started after its stops).
    pub(crate) async fn sessions_round_after(
        self: &Arc<Self>,
        ticket: u64,
    ) -> super::agent_device::SessionsPoll {
        {
            let cycle = self.sessions_cycle.lock().await;
            if let Some((number, poll)) = cycle.as_ref()
                && *number > ticket
            {
                return poll.clone();
            }
        }
        self.sessions_round().await
    }

    /// How many sessions cycles have started.
    pub(crate) fn cycles_started(&self) -> u64 {
        self.cycles.load(Ordering::SeqCst)
    }

    async fn one_cycle(self: &Arc<Self>) -> super::agent_device::SessionsPoll {
        let workspaces = workspaces_of(&self.store());
        self.cache.write().workspaces = workspaces;
        match session_hook() {
            Some(path) => {
                let seq = self.cache.ends();
                let started = self.now();
                let read = self
                    .gate
                    .run(Lane::Sessions, None, move || read_fake_sessions(&path))
                    .await;
                let read = match read {
                    Ok(sessions) => SessionRead::Good {
                        raw: sessions.into_iter().map(fake_raw).collect(),
                        in_use: true,
                    },
                    Err(reason) => SessionRead::Failed {
                        reason,
                        dead: Vec::new(),
                    },
                };
                self.apply_session_read(read, seq, started)
            }
            None if tracks_sessions() => super::agent_device::poll(self).await,
            None => super::agent_device::SessionsPoll::default(),
        }
    }

    /// A finished sessions read, swapped in under one write guard with its freshness (P3-2): a
    /// good read sets `Since(seq)`, `seq` captured before its status call; a failed one changes
    /// neither the sessions nor `Since` (L51, L54).
    pub fn apply_session_read(
        &self,
        read: SessionRead,
        seq: u64,
        started_ms: i64,
    ) -> super::agent_device::SessionsPoll {
        let now = self.now();
        let mut state = self.cache.write();
        let before = state.devices.iter().filter(|d| d.checking_sessions).count();
        let poll = match read {
            SessionRead::Good { raw, in_use } => {
                let mut first_seen = HashMap::new();
                let raw: Vec<RawSession> = raw
                    .into_iter()
                    .map(|mut r| {
                        let key = r.key();
                        r.since_ms = state.sessions.first_seen.get(&key).copied().unwrap_or(now);
                        first_seen.insert(key, r.since_ms);
                        // A fresh read: mapped from now on against the devices of now.
                        r.mapped.clear();
                        r.kept = false;
                        r.forgotten.clear();
                        r
                    })
                    .collect();
                state.sessions.raw = raw;
                state.sessions.first_seen = first_seen;
                state.sessions.freshness = Freshness::Since(seq);
                state.sessions.unreadable_since = None;
                state.sessions.reason = None;
                state.sessions.last_good_ms = Some(now);
                state.sessions.last_good_started_ms = Some(started_ms);
                state.sessions.in_use = in_use;
                super::agent_device::SessionsPoll {
                    good: true,
                    reason: None,
                }
            }
            SessionRead::Failed { reason, dead } => {
                state.sessions.unreadable_since.get_or_insert(now);
                state.sessions.reason = Some(reason.clone());
                state.sessions.raw.retain(|r| !dead.contains(&r.key()));
                for r in &mut state.sessions.raw {
                    r.kept = true;
                }
                super::agent_device::SessionsPoll {
                    good: false,
                    reason: Some(reason),
                }
            }
        };
        state.at_ms = now;
        let old_raw = serde_json::to_string(&state.sessions.raw).ok();
        rebuild(&mut state);
        // Stored with the resources they mapped to (P2-16), only when that changed (P2-19).
        let persist = serde_json::to_string(&state.sessions.raw).ok();
        let persist = persist.filter(|now| self.last_persisted() != Some(now.clone()));
        let _ = old_raw;
        let after = state.devices.iter().filter(|d| d.checking_sessions).count();
        drop(state);
        if let Some(json) = persist {
            match self.store().set_agent_device_last(&json, now) {
                Ok(()) => *self.persisted.lock().unwrap_or_else(|e| e.into_inner()) = Some(json),
                Err(e) => tracing::warn!("cannot record the last agent-device read: {e:#}"),
            }
        }
        // A good read may clear a device a request waits for (P3-15).
        if poll.good || after < before {
            self.tick.notify_one();
        }
        self.publish();
        poll
    }

    fn last_persisted(&self) -> Option<String> {
        self.persisted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Why the status cannot be read now; `None` while it reads fine.
    pub fn sessions_unreadable(&self) -> Option<String> {
        let state = self.cache.read();
        state
            .sessions
            .unreadable_since
            .map(|_| state.sessions.reason.clone().unwrap_or_default())
    }

    /// The sessions as read, with their raw ids (the L53 check).
    pub fn raw_sessions(&self) -> Vec<RawSession> {
        self.cache.read().sessions.raw.clone()
    }

    /// Task 7 (CRB-4, L63): why nobody knows now what agent-device sessions are on the devices:
    /// the status cannot be read, or no good read came since the service started. `None` once a
    /// good read says (or sessions are not tracked).
    pub fn sessions_unknown(&self) -> Option<String> {
        let state = self.cache.read();
        if state.sessions.unreadable_since.is_some() {
            return Some(
                state
                    .sessions
                    .reason
                    .clone()
                    .unwrap_or_else(|| "agent-device status cannot be read".into()),
            );
        }
        (state.sessions.freshness == Freshness::NotYet)
            .then(|| "agent-device status not read yet since the service started".into())
    }

    /// When the last good sessions read started.
    pub fn last_good_read_started(&self) -> Option<i64> {
        self.cache.read().sessions.last_good_started_ms
    }

    /// The sessions on `r` now (kept ones included).
    pub fn sessions_on(&self, r: &ResourceId) -> Vec<String> {
        self.cache
            .read()
            .sessions
            .claims
            .iter()
            .filter(|f| f.resource == *r)
            .filter_map(|f| session_of(f).map(str::to_owned))
            .collect()
    }

    /// L54: `forget-sessions` releases `r` from its kept sessions and marks its last end checked;
    /// it stops nothing. Checked under the write guard (P3bT9b-5): only kept sessions, and only
    /// while the status is still unreadable; once it reads again, nothing is dropped. The next
    /// claim end applies the normal rules again. Answers the sessions released.
    pub fn drop_sessions_on(&self, r: &ResourceId) -> Result<Vec<String>, String> {
        let mut state = self.cache.write();
        if state.sessions.unreadable_since.is_none() {
            return Err("nothing to forget: agent-device status reads again".into());
        }
        let mut released = Vec::new();
        for raw in &mut state.sessions.raw {
            let on = raw.pinned.as_ref() == Some(r) || raw.mapped.contains(r);
            if raw.kept && on && !raw.forgotten.contains(r) {
                raw.forgotten.push(r.clone());
                released.push(raw.session.clone());
            }
        }
        state.sessions.forgotten.insert(r.clone());
        rebuild(&mut state);
        let json = serde_json::to_string(&state.sessions.raw).ok();
        drop(state);
        if let Some(json) = json {
            match self.store().set_agent_device_last(&json, self.now()) {
                Ok(()) => *self.persisted.lock().unwrap_or_else(|e| e.into_inner()) = Some(json),
                Err(e) => tracing::warn!("cannot record the last agent-device read: {e:#}"),
            }
        }
        self.tick.notify_one();
        self.publish();
        Ok(released)
    }

    pub fn record_forget(&self, record: &crate::contract::devices::ForgetRecord) {
        if let Err(e) = self.store().record_session_forget(record) {
            tracing::warn!("cannot record a forget-sessions: {e:#}");
        }
    }

    /// agent-device's version as `--version` printed it, and a warning below the target.
    pub fn set_agent_device_version(&self, version: Option<String>, warning: Option<String>) {
        let mut state = self.cache.write();
        state.sessions.version = version;
        state.sessions.version_warning = warning;
        rebuild(&mut state);
    }

    /// Telemetry (Task 8), for registered devices only (P-9): each call its own task on the
    /// Probe lane, so one hung phone delays no other (P3bT5-4).
    async fn probe_pass(self: Arc<Self>) {
        for r in super::telemetry::due(&self, self.now()) {
            if self.is_registered(&r) {
                self.spawn_job(Job::Telemetry, r, Duration::ZERO, |svc, r| {
                    super::telemetry::poll_one(svc, r)
                });
            }
        }
    }

    /// The snapshot loop: every period, and at once when `add` owes a seeding snapshot.
    async fn long_loop(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(every(SCHEDULE_EVERY)) => {}
                _ = self.long_wake.notified() => {}
            }
            let done = tokio::spawn(self.clone().long_pass()).await;
            self.round_ended(Loop::Snapshots, done.err());
        }
    }

    /// App snapshots (Task 8), registered devices only, each its own task on the Long lane: the
    /// seeds `add` owes, then the due ones. After a service start the first ones begin 10 s
    /// apart.
    async fn long_pass(self: Arc<Self>) {
        let mut seeds = std::mem::take(&mut *self.seeds.lock().unwrap_or_else(|e| e.into_inner()));
        let mut seen = HashSet::new();
        seeds.retain(|r| seen.insert(r.clone()));
        let mut busy = Vec::new();
        for r in seeds {
            // One already running started too early to be the one asked for: asked again at
            // the next pass (Task 8's close snapshots).
            if self.is_registered(&r)
                && !self.spawn_job(Job::Snapshot, r.clone(), Duration::ZERO, snapshot_job)
            {
                busy.push(r);
            }
        }
        self.seeds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(busy);
        let first = !self.long_passed.swap(true, Ordering::SeqCst);
        // `due` reads the store: on the blocking pool, never on a runtime worker (L71).
        let (svc, now) = (self.clone(), self.now());
        let due = tokio::task::spawn_blocking(move || super::installs::due(&svc, now))
            .await
            .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
        let mut k = 0u32;
        for r in due {
            if !self.is_registered(&r) {
                continue;
            }
            let delay = if first {
                SNAPSHOT_STAGGER * k
            } else {
                Duration::ZERO
            };
            if self.spawn_job(Job::Snapshot, r, delay, snapshot_job) {
                k += 1;
            }
        }
    }

    /// Runs `f` for `r` on `job`'s lane as its own task, after `delay`, unless the same job
    /// for `r` is running or waiting. A telemetry answer goes to the cache. The in-flight mark
    /// ends however the task ends, a panic too.
    fn spawn_job<F>(self: &Arc<Self>, job: Job, r: ResourceId, delay: Duration, f: F) -> bool
    where
        F: FnOnce(&DevicesService, &ResourceId) -> Option<Telemetry> + Send + 'static,
    {
        if !self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((r.clone(), job))
        {
            return false;
        }
        let svc = self.clone();
        tokio::spawn(async move {
            let _running = JobMark(svc.jobs.clone(), (r.clone(), job));
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let (s, target) = (svc.clone(), r.clone());
            let reading = svc
                .gate
                .run(job.lane(), Some(&r), move || f(&s, &target))
                .await;
            if let Some(t) = reading {
                svc.cache.set_telemetry(&r, t);
                svc.publish();
            }
        });
        true
    }

    fn is_registered(&self, r: &ResourceId) -> bool {
        self.cache
            .read()
            .devices
            .iter()
            .any(|d| d.resource == *r && d.registered)
    }

    /// P2-21: the startup pass closes windows of claims that ended meanwhile at this time.
    async fn alive_round(self: Arc<Self>) {
        let now = self.now().to_string();
        if let Err(e) = self.store().set_setting(keys::ALIVE_MS, Some(&now)) {
            tracing::warn!("cannot record devices.alive_ms: {e:#}");
        }
        // Task 8's hourly pruning (plan: installs after 90 days, closed windows after 30), on
        // the blocking pool.
        let now = self.now();
        if self.book.prune_due(now) {
            let svc = self.clone();
            let _ = tokio::task::spawn_blocking(move || super::windows::prune(&svc, now)).await;
        }
    }

    /// Broadcasts `devices.changed` when the snapshot differs; telemetry and other volatile-only
    /// changes at most once per 10 s.
    pub fn publish(&self) {
        // A catalog change (registered, connected, booted, removed) ticks the claims engine at
        // once, so waiting requests see it without the 2 s wait.
        let catalog = self.cache.resources();
        {
            let mut last = self.last_catalog.lock().unwrap_or_else(|e| e.into_inner());
            if *last != catalog {
                *last = catalog;
                self.tick.notify_one();
            }
        }
        let now = self.now();
        let mut snapshot = self.cache.snapshot();
        snapshot.at_ms = 0;
        let mut last = self.last_event.lock().unwrap_or_else(|e| e.into_inner());
        // What changes on its own every few seconds is not a change (P3bT5-8): it goes out
        // with the next real one, or at most once per 10 s.
        let structural = |s: &DevicesSnapshot| {
            let mut s = s.clone();
            s.tools.sessions_last_good_ms = None;
            for d in &mut s.devices {
                d.telemetry = None;
                d.last_seen_ms = 0;
                for session in &mut d.sessions {
                    session.seen_since_ms = 0;
                }
            }
            s
        };
        let send = match &last.0 {
            None => true,
            Some(prev) if structural(prev) != structural(&snapshot) => true,
            Some(prev) if *prev != snapshot => now - last.1 >= TELEMETRY_EVENT_MS,
            Some(_) => false,
        };
        if send {
            last.0 = Some(snapshot.clone());
            last.1 = now;
            snapshot.at_ms = now;
            let _ = self.events.send(Event::DevicesChanged(Box::new(snapshot)));
        }
    }

    /// iOS battery through `ideviceinfo` (Task 8 calls it from `poll_one`): only for a
    /// registered, wired, devicectl-paired phone, and only when the lockdown pair record exists
    /// at this moment: a stat right before the call, never cached (P2-3, P3-8). `None`: not
    /// asked; the reading stays unknown.
    pub fn ios_battery(&self, r: &ResourceId) -> Option<Telemetry> {
        self.ios_battery_in(r, &probes::lockdown_dir()?)
    }

    /// `ios_battery` with the lockdown directory given (tests).
    pub fn ios_battery_in(&self, r: &ResourceId, lockdown: &Path) -> Option<Telemetry> {
        let info = self.cache.info(r)?;
        let paired = {
            let state = self.cache.read();
            state
                .observed
                .physical_ios
                .iter()
                .flatten()
                .any(|p| ios::resource_of_physical(p) == *r && p.paired)
        };
        let wired = info
            .transports
            .first()
            .is_some_and(|t| t.kind == TransportKind::Wired);
        if !(info.registered && info.connected && wired && paired) {
            return None;
        }
        let tool = self.tools().ideviceinfo?;
        // Right before the call, every time: a record removed meanwhile stops it (P3-8).
        if probes::has_pair_record(lockdown, &info.hardware_id) != Some(true) {
            return None;
        }
        let args = [
            OsStr::new("-u"),
            OsStr::new(&info.hardware_id),
            OsStr::new("-q"),
            OsStr::new("com.apple.mobile.battery"),
        ];
        let out = self
            .call(Tool::Ideviceinfo, &tool, &args, Duration::from_secs(10))
            .filter(ok)?;
        Some(ios::parse_ideviceinfo_battery(
            &String::from_utf8_lossy(&out.stdout),
            self.now(),
        ))
    }
}

fn internal(e: anyhow::Error) -> HostlerError {
    HostlerError {
        code: crate::contract::ErrorCode::Internal,
        message: format!("{e:#}"),
        details: serde_json::json!({}),
        recovery: None,
    }
}

/// A call that ran to completion with status 0 and whole output.
fn ok(out: &ToolOutput) -> bool {
    out.status == Some(0) && !out.timed_out && !out.truncated
}

/// `android::seen_key`: a transport's `(serial, transport_id, state)` (P-13).
type SeenKey = (String, Option<u32>, String);

/// A transport's identity read: its answer (`None`: failed), when it was read, and when the
/// transport was last listed.
#[derive(Clone, Debug)]
struct Known {
    id: Option<Identity>,
    read_ms: i64,
    listed_ms: i64,
}

enum AdbRound {
    Lines(Vec<AdbLine>),
    Refused(Refused),
    Failed(String),
}

/// The poller's loops, as their problems name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loop {
    Android,
    Avds,
    Ios,
    Telemetry,
    Snapshots,
    Alive,
}

impl Loop {
    fn name(self) -> &'static str {
        match self {
            Loop::Android => "Android discovery",
            Loop::Avds => "AVD list",
            Loop::Ios => "iOS discovery",
            Loop::Telemetry => "telemetry",
            Loop::Snapshots => "app snapshot",
            Loop::Alive => "alive mark",
        }
    }
}

/// Task 8's per-device work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Job {
    Telemetry,
    Snapshot,
}

impl Job {
    fn lane(self) -> Lane {
        match self {
            Job::Telemetry => Lane::Probe,
            Job::Snapshot => Lane::Long,
        }
    }
}

fn snapshot_job(svc: &DevicesService, r: &ResourceId) -> Option<Telemetry> {
    super::installs::snapshot_and_diff(svc, r);
    None
}

/// Ends a job's in-flight mark when dropped.
struct JobMark(Arc<Mutex<HashSet<(ResourceId, Job)>>>, (ResourceId, Job));

impl Drop for JobMark {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

/// Ends an identity or boot read's in-flight key when dropped (a panic too).
struct Flight(Arc<DevicesService>, String);

impl Drop for Flight {
    fn drop(&mut self) {
        self.0.end_flight(&self.1);
    }
}

impl Drop for DevicesService {
    fn drop(&mut self) {
        if self.ios {
            let _ = std::fs::remove_dir_all(&self.json_dir);
        }
    }
}

/// An emulator's own transport key, from the lines it was merged from.
fn boot_key(m: &Merged, lines: &[AdbLine]) -> Option<SeenKey> {
    let serial = &m.transports.first()?.address;
    lines
        .iter()
        .find(|l| l.serial == *serial && l.state == "device")
        .map(android::seen_key)
}

/// Why a call failed (`None`: it ran to completion with status 0).
fn failure(out: Option<&ToolOutput>) -> Option<String> {
    let Some(out) = out else {
        return Some("it could not run".into());
    };
    if out.timed_out {
        Some("it timed out".into())
    } else if out.truncated {
        Some("its output was cut".into())
    } else if out.status != Some(0) {
        Some(format!("it exited with {:?}", out.status))
    } else {
        None
    }
}

fn android_seen(m: &Merged) -> DeviceSeen {
    let emulator = m.identity.qemu;
    DeviceSeen {
        resource: m.resource.clone(),
        platform: Platform::Android,
        kind: if emulator {
            DeviceKind::Emulator
        } else {
            DeviceKind::Physical
        },
        model: m.identity.model.trim().to_owned(),
        name: emulator.then(|| m.identity.avd_name.clone()).flatten(),
        os_version: non_empty(&m.identity.release),
        coredevice_id: None,
    }
}

fn seen_of(info: &DeviceInfo) -> DeviceSeen {
    DeviceSeen {
        resource: info.resource.clone(),
        platform: info.platform,
        kind: info.kind,
        model: info.model.clone(),
        name: info.name.clone(),
        os_version: info.os_version.clone(),
        coredevice_id: info.coredevice_id.clone(),
    }
}

fn workspaces_of(store: &Store) -> Vec<(String, String)> {
    store
        .workspaces()
        .map(|rows| rows.into_iter().map(|w| (w.id, w.path)).collect())
        .unwrap_or_default()
}

fn adb_addr(settings: &ToolSettings) -> AdbTarget {
    let env = settings.env.clone();
    AdbServerAddr::resolve(&move |name: &str| {
        env.get(name).cloned().or_else(|| std::env::var(name).ok())
    })
}

/// The model, AVD or simulator name as an alias: lowercase, runs of anything else as one `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let mut out: String = out.chars().take(32).collect();
    while out.ends_with(['-', '.', '_']) {
        out.pop();
    }
    if out.is_empty() { "device".into() } else { out }
}

// ---------------------------------------------------------------------------------------------
// HOSTLER_TEST_FAKE_SESSIONS

#[derive(Clone, Debug, serde::Deserialize)]
struct FakeSession {
    resource: String,
    session: String,
    workspace_path: String,
    #[serde(default = "android_text")]
    platform: String,
    #[serde(default)]
    agent_device_id: String,
}

fn android_text() -> String {
    "android".into()
}

#[derive(Debug, serde::Deserialize)]
struct FakeSessionsFile {
    #[serde(default = "yes")]
    readable: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    sessions: Vec<FakeSession>,
}

fn yes() -> bool {
    true
}

/// A hook session: it names its resource itself.
fn fake_raw(s: FakeSession) -> RawSession {
    RawSession {
        platform: Some(if s.platform == "ios" {
            Platform::Ios
        } else {
            Platform::Android
        }),
        platform_text: s.platform.clone(),
        device_id: s.agent_device_id.clone(),
        name: String::new(),
        session: s.session.clone(),
        workspace_path: Some(s.workspace_path),
        stop: serde_json::json!({
            "session": s.session,
            "platform": s.platform,
            "id": s.agent_device_id,
        }),
        since_ms: 0,
        mapped: Vec::new(),
        pinned: Some(ResourceId(s.resource)),
        kept: false,
        forgotten: Vec::new(),
    }
}

/// The devices a session is on now (P3bT9b-1): Android by a current transport, else by every
/// stored serial (an `emulator-N` port is reused across AVDs: all of them, never the first),
/// and an `emulator-N` no AVD is on now by every AVD not on a known port now (one whose
/// identity is not read yet, or whose port was never stored); iOS by UDID or CoreDevice id.
fn current_targets(
    raw: &RawSession,
    devices: &[DeviceInfo],
    rows: &[DeviceRow],
) -> Vec<ResourceId> {
    let id = raw.device_id.as_str();
    if id.is_empty() {
        return Vec::new();
    }
    match raw.platform {
        None => Vec::new(),
        Some(Platform::Android) => {
            let current: Vec<ResourceId> = devices
                .iter()
                .filter(|d| d.platform == Platform::Android && d.connected)
                .filter(|d| d.transports.iter().any(|t| t.address == id))
                .map(|d| d.resource.clone())
                .collect();
            if !current.is_empty() {
                return current;
            }
            let stored = rows
                .iter()
                .filter(|r| r.platform == Platform::Android)
                .filter(|r| r.serials.iter().any(|s| s.serial == id))
                .map(|r| r.resource.clone());
            if !id.starts_with("emulator-") {
                return stored.collect();
            }
            let unplaced = devices
                .iter()
                .filter(|d| d.platform == Platform::Android && d.kind == DeviceKind::Emulator)
                .filter(|d| {
                    !d.connected
                        || !d
                            .transports
                            .iter()
                            .any(|t| t.address.starts_with("emulator-"))
                })
                .map(|d| d.resource.clone());
            let mut all: Vec<ResourceId> = stored.chain(unplaced).collect();
            all.sort_by(|a, b| a.0.cmp(&b.0));
            all.dedup();
            all
        }
        Some(Platform::Ios) => devices
            .iter()
            .filter(|d| d.platform == Platform::Ios)
            .filter(|d| d.hardware_id == id || d.coredevice_id.as_deref() == Some(id))
            .map(|d| d.resource.clone())
            .chain(
                rows.iter()
                    .filter(|r| r.platform == Platform::Ios)
                    .filter(|r| {
                        hardware_of(&r.resource) == id || r.coredevice_id.as_deref() == Some(id)
                    })
                    .map(|r| r.resource.clone()),
            )
            .collect(),
    }
}

/// CPU only, under the write guard: the sessions' foreign claims on the registered devices they
/// are on, and the problems of the ones on no device.
fn derive_sessions(devices: &[DeviceInfo], rows: &[DeviceRow], sessions: &mut Sessions) {
    let registered: HashSet<&ResourceId> = devices
        .iter()
        .filter(|d| d.registered)
        .map(|d| &d.resource)
        .collect();
    let known: HashSet<&ResourceId> = devices.iter().map(|d| &d.resource).collect();
    let reason = sessions.reason.clone();
    let mut claims = Vec::new();
    let mut unmapped = Vec::new();
    for raw in &mut sessions.raw {
        // L61: kept sessions too are re-mapped, and the mapping only grows: the last good
        // mapping (P2-16) is never dropped, and a device its id resolves to now is the one
        // agent-device would address, so blocking it fails closed. Mapping kept sessions only
        // at their read would reopen P3bT9b-1 for those that mapped to nothing then.
        for r in raw
            .pinned
            .iter()
            .cloned()
            .chain(current_targets(raw, devices, rows))
        {
            if !raw.mapped.contains(&r) {
                raw.mapped.push(r);
            }
        }
        let on_known = raw.mapped.iter().any(|r| known.contains(r));
        if !on_known {
            unmapped.push(match raw.platform {
                Some(_) => super::agent_device::sanitized(&format!(
                    "agent-device session {} on the {} device {} ({}) maps to no device hostler \
                     knows",
                    raw.session, raw.platform_text, raw.name, raw.device_id
                )),
                None => super::agent_device::sanitized(&format!(
                    "agent-device session {} on a {} device {} ({}): hostler does not know that \
                     platform",
                    raw.session, raw.platform_text, raw.name, raw.device_id
                )),
            });
        }
        for r in &raw.mapped {
            if !registered.contains(r) || raw.forgotten.contains(r) {
                continue;
            }
            let mut stop = raw.stop.clone();
            // L40, L54: each piece of agent-device's own text is sanitized and clipped on its
            // own, never the composed line, so the reason and the fix stay whole (M-1).
            let session = super::agent_device::sanitized(&raw.session);
            let description = if raw.kept {
                if let serde_json::Value::Object(map) = &mut stop {
                    map.insert("kept".into(), serde_json::Value::Bool(true));
                }
                match &reason {
                    Some(why) => format!(
                        "agent-device session {session} (last known; agent-device status \
                         unreadable: {}; {})",
                        super::agent_device::sanitized(why),
                        super::agent_device::STATUS_FIX
                    ),
                    None => format!(
                        "agent-device session {session} (last known, before a restart; not read \
                         yet)"
                    ),
                }
            } else {
                format!("agent-device session {session}")
            };
            claims.push(ForeignClaim {
                resource: r.clone(),
                source: "agent-device".into(),
                description,
                workspace_path: raw.workspace_path.clone(),
                since_ms: Some(raw.since_ms),
                stop: Some(stop),
                workspace_id: None,
            });
        }
    }
    sessions.claims = claims;
    sessions.unmapped = unmapped;
}

/// Whether this service reads agent-device sessions: always in release builds; in debug builds
/// with the fake tools, `HOSTLER_TEST_REAL_TOOLS=1` or the sessions hook, so the suite's other
/// services never read a real `~/.agent-device`.
pub fn tracks_sessions() -> bool {
    if !cfg!(debug_assertions) {
        return true;
    }
    session_hook().is_some()
        || std::env::var_os(tools::FAKE_TOOLS_ENV).is_some()
        || std::env::var_os(TEST_REAL_TOOLS_ENV).is_some_and(|v| v == "1")
}

fn session_hook() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os(TEST_FAKE_SESSIONS_ENV).map(PathBuf::from)
}

/// `{"readable": bool, "reason": "…", "sessions": [{resource, session, workspace_path,
/// platform?, agent_device_id?}]}`. A missing file is a good read with no sessions (as a missing
/// claims dir is, P3-4); an unreadable or unparsable one, or `readable: false`, a failed read.
fn read_fake_sessions(path: &Path) -> Result<Vec<FakeSession>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("the status file cannot be read".into()),
    };
    let file: FakeSessionsFile =
        serde_json::from_slice(&bytes).map_err(|_| "the status cannot be parsed".to_owned())?;
    if !file.readable {
        return Err(file
            .reason
            .unwrap_or_else(|| "agent-device cannot run".into()));
    }
    Ok(file.sessions)
}

/// Test hooks for the sibling modules' tests (Task 9b): discovery results without tools.
#[cfg(test)]
impl DevicesService {
    pub(crate) fn test_android(&self, seen: &[(AdbLine, Identity)]) {
        {
            let mut ids = self.identities.lock().unwrap();
            for (line, id) in seen {
                ids.insert(
                    android::seen_key(line),
                    Known {
                        id: Some(id.clone()),
                        read_ms: self.now(),
                        listed_ms: self.now(),
                    },
                );
            }
        }
        let lines: Vec<AdbLine> = seen.iter().map(|(l, _)| l.clone()).collect();
        self.apply_android(&lines, self.now());
    }

    pub(crate) fn test_ios(&self, physical: Vec<PhysicalIos>, simulators: Vec<Simulator>) {
        self.apply_ios(Ok(physical), Ok(simulators), self.now());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI64;

    use super::*;
    use crate::contract::ErrorCode;

    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct World {
        svc: Arc<DevicesService>,
        cache: Arc<DeviceCache>,
        clock: Arc<TestClock>,
        events: broadcast::Receiver<Event>,
    }

    fn world_with(store: Store, tools: ToolPaths) -> World {
        let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
        let (tx, events) = broadcast::channel(256);
        let (svc, cache) = DevicesService::with_clock(store, tools, tx, clock.clone());
        World {
            svc,
            cache,
            clock,
            events,
        }
    }

    fn world() -> World {
        world_with(Store::open_in_memory().unwrap(), ToolPaths::default())
    }

    impl World {
        fn now(&self) -> i64 {
            self.clock.now_ms()
        }

        fn advance(&self, ms: i64) {
            self.clock.0.fetch_add(ms, Ordering::SeqCst);
        }

        fn learn(&self, line: &AdbLine, id: Identity) {
            self.svc.identities.lock().unwrap().insert(
                android::seen_key(line),
                Known {
                    id: Some(id),
                    read_ms: self.now(),
                    listed_ms: self.now(),
                },
            );
        }

        fn adb(&self, lines: &[AdbLine]) {
            self.svc.apply_android(lines, self.now());
        }

        fn info(&self, device: &str) -> DeviceInfo {
            self.svc
                .find(device)
                .unwrap_or_else(|| panic!("{device} not found"))
        }

        fn events(&mut self) -> usize {
            let mut n = 0;
            while let Ok(e) = self.events.try_recv() {
                if matches!(e, Event::DevicesChanged(_)) {
                    n += 1;
                }
            }
            n
        }
    }

    fn line(serial: &str, state: &str, usb: Option<&str>) -> AdbLine {
        AdbLine {
            serial: serial.into(),
            state: state.into(),
            usb: usb.map(Into::into),
            model: Some("Pixel_9".into()),
            product: None,
            device: None,
            transport_id: Some(1),
        }
    }

    fn phone(serialno: &str, model: &str) -> Identity {
        Identity {
            serialno: serialno.into(),
            model: model.into(),
            release: "15".into(),
            sdk: Some(35),
            avd_name: None,
            qemu: false,
        }
    }

    fn emulator(avd: &str) -> Identity {
        Identity {
            serialno: "EMULATOR35X1X1X0".into(),
            model: "sdk_gphone64_arm64".into(),
            release: "15".into(),
            sdk: Some(35),
            avd_name: Some(avd.into()),
            qemu: true,
        }
    }

    /// One USB phone (SERIAL0001, `Pixel 9`), seen and stored.
    fn with_phone() -> World {
        let w = world();
        let usb = line("SERIAL0001", "device", Some("1-1"));
        w.learn(&usb, phone("SERIAL0001", "Pixel 9"));
        w.adb(&[usb]);
        w
    }

    #[test]
    fn devices_seen_are_stored_unregistered_and_listed() {
        let w = with_phone();
        let info = w.info("SERIAL0001");
        assert_eq!(info.resource.0, "device:android:SERIAL0001");
        assert!(!info.registered && info.connected && info.ready);
        assert_eq!(info.model, "Pixel 9");
        assert_eq!(info.kind, DeviceKind::Physical);
        // New physical devices are listed by default; the catalog has none yet.
        assert_eq!(w.svc.list(false).devices.len(), 1);
        assert!(w.cache.resources().is_empty());
        let row = w.svc.store().device(&info.resource).unwrap().unwrap();
        assert!(!row.registered());
        assert_eq!(row.serials[0].serial, "SERIAL0001");
    }

    #[test]
    fn physical_add_needs_person_virtual_does_not() {
        let w = with_phone();
        let e = w.svc.add("SERIAL0001", None, false).unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        assert!(!w.info("SERIAL0001").registered);
        let info = w.svc.add("SERIAL0001", None, true).unwrap();
        assert!(info.registered);
        assert_eq!(info.alias.as_deref(), Some("pixel-9"));
        assert!(info.alias_by_person);
        // rename and remove: the same gate.
        assert_eq!(
            w.svc.rename("pixel-9", "left", false).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            w.svc.begin_remove("pixel-9", false).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        // An AVD: no person needed.
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        let avd = w.svc.add("Pixel_9", None, false).unwrap();
        assert!(avd.registered && !avd.alias_by_person);
        assert_eq!(avd.alias.as_deref(), Some("pixel_9"));
        let renamed = w.svc.rename("pixel_9", "avd-9", false).unwrap();
        assert_eq!(renamed.alias.as_deref(), Some("avd-9"));
        assert!(renamed.alias_by_person, "a rename is a chosen name");
        w.svc
            .begin_remove("avd-9", false)
            .and_then(|r| w.svc.remove(&r, false, InUse::default()))
            .unwrap();
        assert!(!w.info("Pixel_9").registered);
    }

    #[test]
    fn default_aliases_slug_and_dedupe() {
        let w = world();
        let a = line("SERIAL000A", "device", Some("1-1"));
        let b = line("SERIAL000B", "device", Some("1-2"));
        w.learn(&a, phone("SERIAL000A", "SM-S937B"));
        w.learn(&b, phone("SERIAL000B", "SM-S937B"));
        w.adb(&[a, b]);
        assert_eq!(
            w.svc
                .add("SERIAL000A", None, true)
                .unwrap()
                .alias
                .as_deref(),
            Some("sm-s937b")
        );
        assert_eq!(
            w.svc
                .add("SERIAL000B", None, true)
                .unwrap()
                .alias
                .as_deref(),
            Some("sm-s937b-2")
        );
        assert_eq!(slug("iPhone 17 Pro"), "iphone-17-pro");
        assert_eq!(slug("Pixel_9"), "pixel_9");
        assert_eq!(slug("  ***  "), "device");
        assert_eq!(slug(&"x".repeat(40)).len(), 32);
        // Adding again answers the same device.
        assert_eq!(
            w.svc.add("sm-s937b", None, true).unwrap().resource.0,
            "device:android:SERIAL000A"
        );
    }

    #[test]
    fn alias_never_reslugs() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let usb = line("SERIAL0001", "device", Some("1-1"));
        // A system update renames the model.
        w.svc.identities.lock().unwrap().clear();
        w.learn(&usb, phone("SERIAL0001", "Pixel 9 Pro XL"));
        w.adb(&[usb]);
        let info = w.info("pixel-9");
        assert_eq!(info.model, "Pixel 9 Pro XL");
        assert_eq!(info.alias.as_deref(), Some("pixel-9"));
    }

    #[test]
    fn id_like_aliases_are_refused() {
        let w = with_phone();
        let wifi = line("192.0.2.10:5555", "device", None);
        w.learn(&wifi, phone("SERIAL0001", "Pixel 9"));
        let usb = line("SERIAL0001", "device", Some("1-1"));
        w.adb(&[usb, wifi]);
        for alias in ["emulator-5554", "host:5555", "192.0.2.10:5555", "Pixel"] {
            let e = w.svc.add("SERIAL0001", Some(alias), true).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest, "{alias}");
        }
        w.svc.add("SERIAL0001", Some("left-phone"), true).unwrap();
        // Another device's hardware id is refused; a device's own is not (an AVD's name).
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        assert_eq!(
            w.svc
                .add("Pixel_9", Some("serial0001"), false)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        // Taken by another device, in any case: the holder is named.
        w.svc.rename("left-phone", "left", true).unwrap();
        w.svc.rename("left", "left-phone", true).unwrap();
        let e = w.svc.add("Pixel_9", Some("left-phone"), false).unwrap_err();
        assert_eq!(e.code, ErrorCode::AliasTaken);
        assert_eq!(
            e.recovery.as_deref(),
            Some("choose another alias, or: hostler devices rename left-phone <other>")
        );
    }

    #[test]
    fn usb_to_wifi_switch_keeps_one_resource_and_its_alias() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let wifi = line("192.0.2.10:5555", "device", None);
        w.learn(&wifi, phone("SERIAL0001", "Pixel 9"));
        w.adb(std::slice::from_ref(&wifi));
        let info = w.info("pixel-9");
        assert_eq!(info.resource.0, "device:android:SERIAL0001");
        assert_eq!(info.transports.len(), 1);
        assert_eq!(info.transports[0].kind, TransportKind::Wifi);
        let entry = &w.cache.resources()[0];
        assert_eq!(entry.display, "pixel-9");
        assert_eq!(entry.attributes["transport"], "wifi");
        assert!(entry.available);
        assert_eq!(w.svc.store().devices().unwrap().len(), 1);
    }

    #[test]
    fn serial_attribute_follows_the_current_transport() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let usb = line("SERIAL0001", "device", Some("1-1"));
        let wifi = line("192.0.2.10:5555", "device", None);
        w.learn(&wifi, phone("SERIAL0001", "Pixel 9"));
        w.adb(&[wifi.clone(), usb]);
        assert_eq!(
            w.cache.resources()[0].attributes["serial"],
            "SERIAL0001",
            "USB first"
        );
        assert_eq!(
            w.cache
                .current_serial(&ResourceId("device:android:SERIAL0001".into()))
                .as_deref(),
            Some("SERIAL0001")
        );
        w.adb(&[wifi]);
        assert_eq!(
            w.cache.resources()[0].attributes["serial"],
            "192.0.2.10:5555"
        );
        assert_eq!(
            w.cache.resolve_serial("192.0.2.10:5555").unwrap().0,
            "device:android:SERIAL0001"
        );
    }

    /// P-10: an agent-device claim on a serial maps after a restart through the stored ones.
    #[test]
    fn stored_serials_resolve_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let w = world_with(Store::open(&path).unwrap(), ToolPaths::default());
            let wifi = line("192.0.2.10:5555", "device", None);
            w.learn(&wifi, phone("SERIAL0001", "Pixel 9"));
            w.adb(&[wifi]);
        }
        let w = world_with(Store::open(&path).unwrap(), ToolPaths::default());
        let r = w.cache.resolve_serial("192.0.2.10:5555").unwrap();
        assert_eq!(r.0, "device:android:SERIAL0001");
        assert_eq!(w.cache.resolve_serial("SERIAL0001"), Some(r));
        assert_eq!(w.cache.resolve_serial("192.0.2.99:5555"), None);
        assert_eq!(w.cache.resolve_serial(""), None);
        assert!(!w.info("SERIAL0001").connected, "known, not seen yet");
    }

    #[test]
    fn remove_refused_while_claimed_or_waited_for() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let mut held = crate::contract::claims::fixtures::summary();
        held.workspace_path = "/work/a".into();
        for in_use in [
            InUse {
                claims: vec![held],
                waiting: 0,
            },
            InUse {
                claims: Vec::new(),
                waiting: 1,
            },
        ] {
            let r = w.svc.begin_remove("pixel-9", true).unwrap();
            let e = w.svc.remove(&r, true, in_use.clone()).unwrap_err();
            assert_eq!(e.code, ErrorCode::ResourceInUse, "{in_use:?}");
            assert!(w.info("pixel-9").registered);
        }
        let r = w.svc.begin_remove("pixel-9", true).unwrap();
        w.svc.remove(&r, true, InUse::default()).unwrap();
        let info = w.info("SERIAL0001");
        assert!(!info.registered && info.alias.is_none());
        assert!(
            w.svc.store().device(&info.resource).unwrap().is_some(),
            "kept as seen"
        );
        // No longer registered: refused, and the call leaves no mark behind.
        assert_eq!(
            w.svc.remove(&r, true, InUse::default()).unwrap_err().code,
            ErrorCode::DeviceNotRegistered
        );
        assert!(!w.info("SERIAL0001").removing);
    }

    /// P2-14: the device is unavailable from before the engine is asked until the answer.
    #[test]
    fn remove_marks_removing_before_the_check() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        assert!(w.cache.resources()[0].available);
        let r = w.svc.begin_remove("pixel-9", true).unwrap();
        let entry = &w.cache.resources()[0];
        assert_eq!(entry.id, r);
        assert!(!entry.available, "removing");
        assert!(w.info("pixel-9").removing);
        let refused = InUse {
            claims: Vec::new(),
            waiting: 1,
        };
        assert!(w.svc.remove(&r, true, refused).is_err());
        assert!(w.cache.resources()[0].available, "unmarked after a refusal");
        assert!(!w.info("pixel-9").removing);
    }

    #[test]
    fn failed_tool_marks_disconnected_after_30_s() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        w.advance(1_000);
        w.svc.android_failed("it timed out", w.now());
        assert!(
            w.info("pixel-9").connected,
            "one failure keeps the last answer"
        );
        w.advance(29_999);
        w.svc.android_failed("it timed out", w.now());
        assert!(
            w.info("pixel-9").connected,
            "30 s from the first failure, not yet"
        );
        w.advance(1);
        w.svc.android_failed("it timed out", w.now());
        let info = w.info("pixel-9");
        assert!(!info.connected && !w.cache.resources()[0].available);
        assert!(
            w.svc
                .list(false)
                .tools
                .problems
                .iter()
                .any(|p| p == "adb devices -l failed for 30 s: it timed out"),
            "{:?}",
            w.svc.list(false).tools.problems
        );
        // One good answer restores it.
        w.adb(&[line("SERIAL0001", "device", Some("1-1"))]);
        assert!(w.info("pixel-9").connected);
        assert!(w.svc.list(false).tools.problems.is_empty());
    }

    /// P-3: only failed or timed-out calls count; time spent waiting for a lane is none.
    #[test]
    fn lane_waits_never_count_as_failure() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        // A minute with no answer at all (the lane was busy): nothing failed.
        w.advance(60_000);
        w.svc.publish();
        let state_now = w.now();
        {
            let mut state = w.cache.write();
            state.at_ms = state_now;
            rebuild(&mut state);
        }
        assert!(w.info("pixel-9").connected);
        assert!(w.svc.list(false).tools.problems.is_empty());
    }

    #[test]
    fn unauthorized_transport_is_listed_with_hint() {
        let w = world();
        w.adb(&[line("SERIAL0009", "unauthorized", Some("1-3"))]);
        let all = w.svc.list(true);
        assert!(all.devices.is_empty());
        assert_eq!(all.unidentified.len(), 1);
        assert_eq!(
            all.unidentified[0].hint.as_deref(),
            Some("accept the prompt on the phone")
        );
        assert!(w.svc.list(false).unidentified.is_empty(), "only with --all");
        assert!(
            w.svc.store().devices().unwrap().is_empty(),
            "never a resource"
        );
    }

    #[test]
    fn placeholder_and_duplicate_usb_serials_are_unidentified() {
        let w = world();
        let a = line("0123456789ABCDEF", "device", Some("1-1"));
        w.learn(&a, phone("0123456789ABCDEF", "Cheap Phone"));
        let b = line("DUPSERIAL", "device", Some("1-2"));
        let c = line("DUPSERIAL2", "device", Some("1-3"));
        w.learn(&b, phone("DUPSERIAL", "Clone"));
        w.learn(&c, phone("DUPSERIAL", "Clone"));
        w.adb(&[a, b, c]);
        let all = w.svc.list(true);
        assert!(all.devices.is_empty(), "{:?}", all.devices);
        assert_eq!(all.unidentified.len(), 3);
        assert!(
            all.unidentified
                .iter()
                .all(|u| u.hint.as_deref() == Some("this phone reports no unique serial"))
        );
    }

    #[test]
    fn not_ready_virtual_devices_are_available_with_ready_false() {
        let w = world();
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        w.svc.add("Pixel_9", None, false).unwrap();
        let entry = &w.cache.resources()[0];
        assert!(entry.available);
        assert_eq!(entry.attributes["ready"], "false");
        assert_eq!(entry.attributes["kind"], "emulator");
        assert!(!w.svc.list(false).devices[0].ready);
        // An AVD that is gone (deleted) is unavailable.
        w.svc.apply_avds(Vec::new(), w.now());
        assert!(!w.cache.resources()[0].available);
    }

    #[test]
    fn booting_emulator_is_not_ready() {
        let w = world();
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        w.svc.add("Pixel_9", None, false).unwrap();
        let emu = line("emulator-5554", "device", None);
        w.learn(&emu, emulator("Pixel_9"));
        w.adb(std::slice::from_ref(&emu));
        let info = w.info("pixel_9");
        assert_eq!(info.resource.0, "device:android:avd:Pixel_9");
        assert!(info.connected && !info.ready);
        assert_eq!(w.cache.resources()[0].attributes["ready"], "false");
        assert_eq!(w.cache.resources()[0].attributes["serial"], "emulator-5554");
        w.cache
            .write()
            .observed
            .booted
            .insert(android::seen_key(&emu), true);
        w.adb(&[emu]);
        assert!(w.info("pixel_9").ready);
        assert_eq!(w.cache.resources()[0].attributes["ready"], "true");
    }

    #[test]
    fn catalog_answers_from_the_cache_without_io() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let catalog = super::catalog::DeviceCatalog(w.cache.clone());
        // The service's store is busy: the catalog never touches it.
        let _store = w.svc.store();
        let start = std::time::Instant::now();
        let entries = crate::claims::ResourceCatalog::resources(&catalog);
        assert!(start.elapsed() < Duration::from_millis(50));
        assert_eq!(entries, catalog::entries(&w.cache.snapshot().devices));
        assert_eq!(entries.len(), 1);
    }

    /// P2-15: the sessions' workspaces come from the engine's own function.
    #[test]
    fn session_workspace_ids_resolved_by_the_shared_function() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let r = ResourceId("device:android:SERIAL0001".into());
        {
            let mut state = w.cache.write();
            state.workspaces = vec![
                ("outer".into(), "/work/app".into()),
                ("inner".into(), "/work/app/.worktrees/x".into()),
            ];
            let mut s1 = session(&r, "s1");
            s1.workspace_path = Some("/work/app/.worktrees/x/e2e".into());
            s1.since_ms = 5;
            state.sessions.raw = vec![s1];
            rebuild(&mut state);
        }
        let session = &w.info("pixel-9").sessions[0];
        assert_eq!(session.workspace_id.as_deref(), Some("inner"));
        assert_eq!(session.session, "s1");
        assert_eq!(session.agent_device_id, "SERIAL0001");
        assert_eq!(session.seen_since_ms, 5);
        assert!(!session.kept);
    }

    #[test]
    fn devices_changed_is_coalesced() {
        let mut w = with_phone();
        assert_eq!(w.events(), 1, "the first snapshot");
        w.adb(&[line("SERIAL0001", "device", Some("1-1"))]);
        assert_eq!(w.events(), 0, "nothing changed");
        w.svc.add("SERIAL0001", None, true).unwrap();
        assert_eq!(w.events(), 1);
        let r = ResourceId("device:android:SERIAL0001".into());
        let reading = |pct| Telemetry {
            battery_percent: crate::contract::devices::Reading::Known(pct),
            charging: crate::contract::devices::Reading::Unknown,
            wifi: crate::contract::devices::Reading::Unknown,
            wifi_name: None,
            cellular_data: crate::contract::devices::Reading::Unknown,
            vpn: crate::contract::devices::Reading::Unknown,
            at_ms: 0,
        };
        w.advance(TELEMETRY_EVENT_MS);
        w.cache.set_telemetry(&r, reading(80));
        w.svc.publish();
        assert_eq!(w.events(), 1);
        w.advance(1_000);
        w.cache.set_telemetry(&r, reading(79));
        w.svc.publish();
        assert_eq!(w.events(), 0, "telemetry only, within 10 s");
        w.advance(TELEMETRY_EVENT_MS);
        w.svc.publish();
        assert_eq!(w.events(), 1);
    }

    #[test]
    fn missing_sdk_reports_the_fix() {
        let tools = ToolPaths {
            problems: vec![
                "adb not found in the configured Android SDK; fix: hostler devices tools \
                 --detect, or set ANDROID_HOME"
                    .into(),
            ],
            ..ToolPaths::default()
        };
        let w = world_with(Store::open_in_memory().unwrap(), tools);
        let status = w.svc.list(false).tools;
        assert_eq!(status.adb_state, AdbState::Missing);
        assert!(status.problems[0].contains("hostler devices tools --detect"));
    }

    #[test]
    fn tools_setting_points_at_a_custom_adb() {
        let w = world();
        let abs = if cfg!(windows) {
            r"C:\sdk\platform-tools\adb.exe"
        } else {
            "/opt/sdk/platform-tools/adb"
        };
        let mut env = BTreeMap::new();
        env.insert("ANDROID_HOME".to_owned(), "/opt/sdk".to_owned());
        w.svc
            .set_tools(ToolPathsSet {
                adb: Some(abs.into()),
                android_env: env,
                ..ToolPathsSet::default()
            })
            .unwrap();
        let settings = read_settings(&w.svc.store());
        assert_eq!(settings.adb_path, Some(PathBuf::from(abs)));
        assert_eq!(settings.env["ANDROID_HOME"], "/opt/sdk");
        assert_eq!(w.svc.settings(), settings);
        // A `None` keeps a setting; an empty value clears it.
        w.svc.set_tools(ToolPathsSet::default()).unwrap();
        assert_eq!(
            read_settings(&w.svc.store()).adb_path,
            Some(PathBuf::from(abs))
        );
        w.svc
            .set_tools(ToolPathsSet {
                adb: Some(String::new()),
                ..ToolPathsSet::default()
            })
            .unwrap();
        assert_eq!(read_settings(&w.svc.store()).adb_path, None);
        for bad in [
            ToolPathsSet {
                adb: Some("relative/adb".into()),
                ..ToolPathsSet::default()
            },
            ToolPathsSet {
                android_env: BTreeMap::from([("PATH".to_owned(), "/x".to_owned())]),
                ..ToolPathsSet::default()
            },
        ] {
            assert_eq!(
                w.svc.set_tools(bad).unwrap_err().code,
                ErrorCode::BadRequest
            );
        }
    }

    #[test]
    fn resolution_order_and_registered_only_resolve() {
        let w = with_phone();
        let r = "device:android:SERIAL0001";
        for form in ["SERIAL0001", r] {
            assert_eq!(w.svc.show(form).unwrap().info.resource.0, r);
        }
        let e = w.svc.resolve("SERIAL0001").unwrap_err();
        assert_eq!(e.code, ErrorCode::DeviceNotRegistered);
        assert_eq!(
            e.recovery.as_deref(),
            Some(
                "ask the operator to register it: hostler devices add SERIAL0001 (a person types \
                 the model)"
            )
        );
        w.svc.add("SERIAL0001", None, true).unwrap();
        assert_eq!(
            w.svc.resolve("PIXEL-9").unwrap().resource.0,
            r,
            "alias, any case"
        );
        assert_eq!(
            w.svc.show("nothing").unwrap_err().code,
            ErrorCode::DeviceNotFound
        );
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        assert_eq!(
            w.svc.resolve("Pixel_9").unwrap_err().recovery.as_deref(),
            Some("hostler devices add Pixel_9")
        );
    }

    #[test]
    fn notes_are_kept_and_checked() {
        let w = with_phone();
        assert_eq!(
            w.svc
                .note("SERIAL0001", Some("on the left"))
                .unwrap()
                .note
                .as_deref(),
            Some("on the left")
        );
        assert_eq!(w.svc.note("SERIAL0001", None).unwrap().note, None);
        assert_eq!(
            w.svc.note("SERIAL0001", Some("a\u{1b}b")).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    fn session(r: &ResourceId, name: &str) -> RawSession {
        fake_raw(FakeSession {
            resource: r.0.clone(),
            session: name.into(),
            workspace_path: "/work/app".into(),
            platform: "android".into(),
            agent_device_id: "SERIAL0001".into(),
        })
    }

    fn good(raw: Vec<RawSession>) -> SessionRead {
        SessionRead::Good { raw, in_use: true }
    }

    /// The description a session gets in the three shapes of `derive_sessions` (M-1).
    fn session_claim(session_name: &str, kept: bool, reason: Option<&str>) -> String {
        let w = with_phone();
        let device = w.svc.add("SERIAL0001", None, true).unwrap();
        let mut raw = session(
            &ResourceId("device:android:SERIAL0001".into()),
            session_name,
        );
        raw.kept = kept;
        let mut sessions = Sessions {
            raw: vec![raw],
            reason: reason.map(str::to_owned),
            ..Sessions::default()
        };
        derive_sessions(&[device], &[], &mut sessions);
        sessions.claims[0].description.clone()
    }

    /// M-1 (L40, L54): a long, control-laden reason is sanitized and clipped on its own, so the
    /// line still ends with the whole fix.
    #[test]
    fn a_long_unreadable_reason_keeps_the_fix_whole() {
        let why = format!("node not found\u{1b}[2J{}", "x".repeat(300));
        let sanitized = crate::devices::agent_device::sanitized(&why);
        assert!(sanitized.len() <= 200, "{} bytes", sanitized.len());
        let line = session_claim("s1", true, Some(&why));
        assert_eq!(
            line,
            format!(
                "agent-device session s1 (last known; agent-device status unreadable: \
                 {sanitized}; {})",
                crate::devices::agent_device::STATUS_FIX
            )
        );
        assert!(
            line.ends_with(&format!("; {})", crate::devices::agent_device::STATUS_FIX)),
            "{line}"
        );
        // The control characters were replaced, not carried into the terminal.
        assert!(line.contains('\u{FFFD}'), "{line}");
        assert!(!line.contains('\u{1b}'), "{line}");
    }

    /// M-1: a long session name is clipped on its own; the fixed text of all three shapes stays
    /// whole.
    #[test]
    fn a_long_session_name_keeps_the_fixed_text_whole() {
        let name = "s".repeat(300);
        let clipped = crate::devices::agent_device::sanitized(&name);
        assert!(clipped.len() <= 200, "{} bytes", clipped.len());
        assert_eq!(
            session_claim(&name, true, Some("node not found")),
            format!(
                "agent-device session {clipped} (last known; agent-device status unreadable: \
                 node not found; {})",
                crate::devices::agent_device::STATUS_FIX
            )
        );
        assert_eq!(
            session_claim(&name, true, None),
            format!("agent-device session {clipped} (last known, before a restart; not read yet)")
        );
        assert_eq!(
            session_claim(&name, false, None),
            format!("agent-device session {clipped}")
        );
    }

    /// M-1: with nothing to clip, the three shapes are byte-identical to the text before it.
    #[test]
    fn short_session_descriptions_are_unchanged() {
        assert_eq!(
            session_claim("s1", true, Some("node not found")),
            "agent-device session s1 (last known; agent-device status unreadable: node not \
             found; fix: hostler devices tools --detect, or hostler devices tools --agent-device \
             <path>)"
        );
        assert_eq!(
            session_claim("s1", true, None),
            "agent-device session s1 (last known, before a restart; not read yet)"
        );
        assert_eq!(session_claim("s1", false, None), "agent-device session s1");
    }

    /// The source answers the cache's sessions and freshness from one read, counts claim ends
    /// from 1, and a failed read keeps both (L51).
    #[test]
    fn sessions_source_counts_ends_and_answers_one_read() {
        use crate::claims::ForeignClaimSource;
        use crate::devices::agent_device::AgentDeviceSource;

        let w = with_phone();
        let source = AgentDeviceSource(w.cache.clone());
        assert_eq!(source.collect().freshness, Freshness::NotTracked);
        let r = ResourceId("device:android:SERIAL0001".into());
        assert_eq!(w.cache.ends(), 1, "the counter starts at 1");
        assert_eq!(source.claim_ended(&r), 2);
        assert_eq!(source.claim_ended(&r), 3);
        w.svc
            .apply_session_read(good(vec![session(&r, "s1")]), 2, w.now());
        // Unregistered devices are skipped.
        assert!(source.collect().claims.is_empty());
        assert_eq!(source.collect().freshness, Freshness::Since(2));
        assert!(
            w.info("SERIAL0001").checking_sessions,
            "end 3 is newer than read 2"
        );
        w.svc.add("SERIAL0001", None, true).unwrap();
        let poll = w
            .svc
            .apply_session_read(good(vec![session(&r, "s1")]), 3, w.now());
        assert!(poll.good);
        let view = source.collect();
        assert_eq!(view.freshness, Freshness::Since(3));
        assert_eq!(view.claims.len(), 1);
        assert_eq!(view.claims[0].since_ms, Some(w.now()));
        assert!(!w.info("pixel-9").checking_sessions);
        // The last good read is stored (L51).
        let stored = w.svc.store().agent_device_last().unwrap().unwrap();
        assert!(stored.0.contains("\"s1\""));
        // A failed read keeps the sessions (kept) and `Since` (L51).
        let failed = |w: &World| {
            w.svc.apply_session_read(
                SessionRead::Failed {
                    reason: "node not found".into(),
                    dead: Vec::new(),
                },
                9,
                w.now(),
            )
        };
        assert_eq!(failed(&w).reason.as_deref(), Some("node not found"));
        let view = source.collect();
        assert_eq!(view.freshness, Freshness::Since(3));
        assert_eq!(view.claims.len(), 1);
        assert!(view.claims[0].description.contains("last known"));
        assert!(w.info("pixel-9").sessions[0].kept);
        assert_eq!(
            w.svc.sessions_unreadable().as_deref(),
            Some("node not found")
        );
        w.advance(STALE_MS);
        failed(&w);
        let tools = w.svc.list(false).tools;
        assert!(!tools.sessions_known);
        assert!(tools.problems.contains(&format!(
            "agent-device sessions unknown: node not found; {}",
            crate::devices::agent_device::STATUS_FIX
        )));
        // A dead owner's kept session goes (L53).
        w.svc.apply_session_read(
            SessionRead::Failed {
                reason: "node not found".into(),
                dead: vec!["android|SERIAL0001|s1".into()],
            },
            9,
            w.now(),
        );
        assert!(source.collect().claims.is_empty());
        assert_eq!(w.svc.store().agent_device_last().unwrap().unwrap().0, "[]");
    }

    /// L51: after a restart the last good read's sessions block again, and `Since` is never
    /// restored: nothing is granted before the first good read.
    #[test]
    fn kept_sessions_reload_by_resource_not_serial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        let r = ResourceId("device:android:SERIAL0001".into());
        {
            let w = world_with(Store::open(&path).unwrap(), ToolPaths::default());
            let usb = line("SERIAL0001", "device", Some("1-1"));
            w.learn(&usb, phone("SERIAL0001", "Pixel 9"));
            w.adb(&[usb]);
            w.svc.add("SERIAL0001", None, true).unwrap();
            let mut s1 = session(&r, "s1");
            // Mapped by its serial, not pinned: the kept session stays on the resource it
            // mapped to, stored with the read (P2-16).
            s1.pinned = None;
            w.svc.apply_session_read(good(vec![s1]), 1, w.now());
        }
        let w = world_with(Store::open(&path).unwrap(), ToolPaths::default());
        let source = crate::devices::agent_device::AgentDeviceSource(w.cache.clone());
        let view = crate::claims::ForeignClaimSource::collect(&source);
        if tracks_sessions() {
            assert_eq!(view.freshness, Freshness::NotYet);
            assert_eq!(view.claims.len(), 1);
            assert_eq!(view.claims[0].resource, r);
            assert!(view.claims[0].description.contains("last known"));
        } else {
            // This test process tracks nothing (no device tools): the store still holds them.
            assert!(
                w.svc
                    .store()
                    .agent_device_last()
                    .unwrap()
                    .unwrap()
                    .0
                    .contains("s1")
            );
        }
    }

    /// The counter starts at 1: a fresh source and one good read open a device never claimed.
    #[test]
    fn a_fresh_source_and_one_good_read_open_an_unclaimed_device() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let seq = w.cache.ends();
        assert_eq!(seq, 1);
        w.svc.apply_session_read(good(Vec::new()), seq, w.now());
        let state = crate::claims::rules::DeviceEnd::at_start(ResourceId(
            "device:android:SERIAL0001".into(),
        ));
        // The engine's start entry is seq 1; the read captured 1: open.
        assert_eq!(
            crate::claims::ForeignClaimSource::collect(
                &crate::devices::agent_device::AgentDeviceSource(w.cache.clone())
            )
            .freshness,
            Freshness::Since(state.seq)
        );
        assert!(!w.info("pixel-9").checking_sessions);
    }

    #[test]
    fn fake_sessions_file_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        assert!(
            read_fake_sessions(&path).unwrap().is_empty(),
            "missing: no sessions"
        );
        std::fs::write(&path, r#"{"readable": false, "reason": "node not found"}"#).unwrap();
        assert_eq!(read_fake_sessions(&path).unwrap_err(), "node not found");
        std::fs::write(&path, "not json").unwrap();
        assert!(read_fake_sessions(&path).is_err());
        std::fs::write(
            &path,
            r#"{"sessions": [{"resource": "device:android:S", "session": "s", "workspace_path": "/w"}]}"#,
        )
        .unwrap();
        let s = read_fake_sessions(&path).unwrap();
        assert_eq!(
            (s[0].platform.as_str(), s[0].agent_device_id.as_str()),
            ("android", "")
        );
    }

    // ---- Task 5 R1 (P3bT5-*) ----

    fn emulator_line(tid: u32) -> AdbLine {
        AdbLine {
            transport_id: Some(tid),
            ..line("emulator-5554", "device", None)
        }
    }

    /// P3bT5-2: `emulator-5554` once booted says nothing about the next emulator on it.
    #[test]
    fn reused_emulator_serial_is_not_ready_until_boot_completed() {
        let w = world();
        w.svc
            .apply_avds(vec!["Pixel_9".into(), "Tablet".into()], w.now());
        let first = emulator_line(2);
        w.learn(&first, emulator("Pixel_9"));
        w.adb(std::slice::from_ref(&first));
        w.cache
            .write()
            .observed
            .booted
            .insert(android::seen_key(&first), true);
        w.adb(std::slice::from_ref(&first));
        assert!(w.info("Pixel_9").ready);
        // Pixel_9 stops; Tablet starts on the same serial with a new transport.
        w.adb(&[]);
        assert!(
            w.cache.read().observed.booted.is_empty(),
            "gone with its transport"
        );
        let second = emulator_line(3);
        w.learn(&second, emulator("Tablet"));
        w.adb(std::slice::from_ref(&second));
        let tablet = w.info("Tablet");
        assert!(tablet.connected && !tablet.ready, "{tablet:?}");
        // The same AVD cold-booted again on the old serial is not ready either.
        w.adb(&[]);
        let third = emulator_line(4);
        w.learn(&third, emulator("Pixel_9"));
        w.adb(std::slice::from_ref(&third));
        assert!(!w.info("Pixel_9").ready);
    }

    /// P3bT5-5: `remove` acts on the device `begin_remove` marked, whatever its text names now.
    #[test]
    fn remove_acts_on_the_marked_device() {
        let w = world();
        w.svc
            .apply_avds(vec!["Pixel_9".into(), "Tablet".into()], w.now());
        let first = emulator_line(2);
        w.learn(&first, emulator("Pixel_9"));
        w.adb(std::slice::from_ref(&first));
        w.svc.add("Pixel_9", None, false).unwrap();
        w.svc.add("Tablet", None, false).unwrap();
        let marked = w.svc.begin_remove("emulator-5554", false).unwrap();
        assert_eq!(marked.0, "device:android:avd:Pixel_9");
        // Meanwhile Tablet takes emulator-5554.
        let second = emulator_line(3);
        w.learn(&second, emulator("Tablet"));
        w.adb(std::slice::from_ref(&second));
        w.svc.remove(&marked, false, InUse::default()).unwrap();
        assert!(!w.info("Pixel_9").registered && !w.info("Pixel_9").removing);
        assert!(w.info("Tablet").registered, "never checked, never removed");
    }

    /// P3bT5-5: one of two removes ending never clears the other's mark.
    #[test]
    fn concurrent_removes_keep_the_mark() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let busy = InUse {
            claims: Vec::new(),
            waiting: 1,
        };
        let r = w.svc.begin_remove("pixel-9", true).unwrap();
        w.svc.begin_remove("pixel-9", true).unwrap();
        assert!(w.svc.remove(&r, true, busy.clone()).is_err());
        assert!(w.info("pixel-9").removing, "the other remove still checks");
        assert!(!w.cache.resources()[0].available);
        assert!(w.svc.remove(&r, true, busy).is_err());
        assert!(!w.info("pixel-9").removing);
        assert!(w.cache.resources()[0].available);
    }

    fn with_adb_path() -> World {
        world_with(
            Store::open_in_memory().unwrap(),
            ToolPaths {
                adb: Some(PathBuf::from("/sdk/platform-tools/adb")),
                ..ToolPaths::default()
            },
        )
    }

    /// P3bT5-6: what adb listed before another adb took the server, or before the tools
    /// changed, never comes back as connected.
    #[test]
    fn tools_change_never_revives_old_android_devices() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        w.svc.android_refused(Refused::Foreign(Some(40)), w.now());
        assert!(!w.info("pixel-9").connected);
        w.advance(10 * 60_000);
        // `devices tools --adb <path>`, the Foreign recovery, finds no adb here.
        w.svc.refresh_tools();
        let info = w.info("pixel-9");
        assert!(!info.connected && info.transports.is_empty(), "{info:?}");
        assert!(!w.cache.resources()[0].available);
        // A tools change alone forgets adb's old answer too.
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        w.svc.refresh_tools();
        assert!(!w.info("pixel-9").connected);
        // The same when a round finds no adb at all.
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        w.svc.android_gone(w.now());
        assert!(!w.info("pixel-9").connected);
    }

    /// P3bT5-6: `ours` only once adb answered; an unreadable client version is `missing`.
    #[test]
    fn unreadable_client_version_reads_missing() {
        let w = with_adb_path();
        let tools = w.svc.list(false).tools;
        assert_eq!(tools.adb_state, AdbState::Missing, "not checked yet");
        assert!(tools.problems.contains(&"adb: not checked yet".to_owned()));
        w.adb(&[]);
        assert_eq!(w.svc.list(false).tools.adb_state, AdbState::Ours);
        w.svc.android_refused(Refused::NoClientVersion, w.now());
        let tools = w.svc.list(false).tools;
        assert_eq!(tools.adb_state, AdbState::Missing);
        assert!(
            tools
                .problems
                .iter()
                .any(|p| p.starts_with("hostler cannot read its own adb's version")),
            "{:?}",
            tools.problems
        );
        // A server that does not answer its version: refused, shown, and the 30 s rule runs.
        w.adb(&[]);
        w.svc.android_refused(Refused::ServerUnknown, w.now());
        let tools = w.svc.list(false).tools;
        assert_eq!(tools.adb_state, AdbState::Missing);
        assert!(tools.problems.iter().any(|p| p.contains("host:version")));
        assert!(w.cache.read().observed.android_failing_since.is_some());
        // Another adb's client that named no version: no "version 0" in the text.
        w.svc.android_refused(Refused::Foreign(None), w.now());
        let tools = w.svc.list(false).tools;
        assert!(
            tools
                .problems
                .iter()
                .any(|p| p.starts_with("another adb owns the adb server")),
            "{:?}",
            tools.problems
        );
    }

    /// P3bT5-8: a sessions read or a `last_seen` that changes nothing else is no event; at most
    /// one per 10 s. A real change still goes out at once.
    #[test]
    fn volatile_changes_are_coalesced() {
        let mut w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        w.events();
        for _ in 0..10 {
            w.advance(1_000);
            w.svc.apply_session_read(good(Vec::new()), 1, w.now());
        }
        assert!(w.events() <= 1, "ten quiet reads in 10 s");
        w.advance(SEEN_WRITE_MS);
        w.adb(&[line("SERIAL0001", "device", Some("1-1"))]);
        w.svc.apply_session_read(good(Vec::new()), 1, w.now());
        assert!(w.events() <= 1);
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        assert_eq!(w.events(), 1, "a new device at once");
    }

    /// P3bT5-9: a wired iPhone that was never trusted is not available.
    #[test]
    fn wired_unpaired_iphone_is_not_available() {
        let w = world();
        w.svc.apply_ios(
            Ok(vec![
                iphone(
                    "00008101-000A00000000001A",
                    TransportKind::Wired,
                    false,
                    false,
                ),
                iphone(
                    "00008101-000A00000000002B",
                    TransportKind::Wired,
                    false,
                    true,
                ),
            ]),
            Ok(Vec::new()),
            w.now(),
        );
        for udid in ["00008101-000A00000000001A", "00008101-000A00000000002B"] {
            w.svc
                .add(udid, Some(&udid[15..].to_lowercase()), true)
                .unwrap();
        }
        let untrusted = w.info("00008101-000A00000000001A");
        assert!(!untrusted.connected);
        assert_eq!(untrusted.transports[0].state, "unpaired");
        let paired = w.info("00008101-000A00000000002B");
        assert!(paired.connected);
        assert_eq!(paired.transports[0].state, "wired");
        let available = |r: &ResourceId| {
            w.cache
                .resources()
                .iter()
                .find(|e| e.id == *r)
                .is_some_and(|e| e.available)
        };
        assert!(!available(&untrusted.resource));
        assert!(available(&paired.resource));
    }

    /// P3bT5-11: a round that panics is a failed call, said so, and its loop goes on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_round_counts_as_a_failure_and_the_loop_goes_on() {
        let w = with_phone();
        let rounds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (count, problems) = (rounds.clone(), seen.clone());
        let looping = tokio::spawn(w.svc.clone().repeat(
            Duration::from_millis(20),
            Loop::Android,
            move |svc: Arc<DevicesService>| {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let problems = problems.clone();
                async move {
                    problems
                        .lock()
                        .unwrap()
                        .push(svc.list(false).tools.problems.clone());
                    assert!(n != 0, "the first round panics");
                }
            },
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        looping.abort();
        assert!(rounds.load(Ordering::SeqCst) >= 3);
        // The round after the panic saw the problem; a completed one cleared it.
        let seen = seen.lock().unwrap().clone();
        assert!(
            seen[1]
                .iter()
                .any(|p| p == "the device Android discovery round failed; see the service log"),
            "{seen:?}"
        );
        assert!(w.cache.read().observed.round_problems.is_empty());
        assert!(
            w.cache.read().observed.android_failing_since.is_some(),
            "a failed call for the 30 s rule"
        );
    }

    /// P3bT5-12: unchanged rounds within a minute write nothing.
    #[test]
    fn quiet_rounds_write_nothing() {
        let w = with_phone();
        let usb = line("SERIAL0001", "device", Some("1-1"));
        let before = w.svc.store().total_changes();
        for _ in 0..10 {
            w.advance(3_000);
            w.adb(std::slice::from_ref(&usb));
        }
        assert_eq!(w.svc.store().total_changes(), before);
        // A minute on: `last_seen` once.
        w.advance(SEEN_WRITE_MS);
        w.adb(std::slice::from_ref(&usb));
        assert!(w.svc.store().total_changes() > before);
    }

    /// P3bT5-12: the AVD list never overwrites what the running emulator reported, and the two
    /// rounds do not rewrite the row in turn.
    #[test]
    fn avd_round_keeps_model_and_os() {
        let w = world();
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        let emu = emulator_line(2);
        w.learn(&emu, emulator("Pixel_9"));
        w.adb(std::slice::from_ref(&emu));
        let r = avd_resource("Pixel_9");
        let row = || w.svc.store().device(&r).unwrap().unwrap();
        assert_eq!(
            (row().model.as_str(), row().os_version.as_deref()),
            ("sdk_gphone64_arm64", Some("15"))
        );
        let before = w.svc.store().total_changes();
        for _ in 0..5 {
            w.advance(3_000);
            w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
            w.adb(std::slice::from_ref(&emu));
        }
        assert_eq!(w.svc.store().total_changes(), before);
        // The emulator stops: the stopped AVD keeps its OS version.
        w.advance(SEEN_WRITE_MS);
        w.adb(&[]);
        w.svc.apply_avds(vec!["Pixel_9".into()], w.now());
        assert_eq!(row().os_version.as_deref(), Some("15"));
        assert_eq!(row().model, "sdk_gphone64_arm64");
        // An iPhone that briefly reports no model keeps its stored one.
        let mut phone = iphone(
            "00008101-000A00000000001A",
            TransportKind::Wired,
            true,
            true,
        );
        w.svc
            .apply_ios(Ok(vec![phone.clone()]), Ok(Vec::new()), w.now());
        phone.model = String::new();
        w.advance(SEEN_WRITE_MS);
        w.svc.apply_ios(Ok(vec![phone]), Ok(Vec::new()), w.now());
        let ios = w
            .svc
            .store()
            .device(&ResourceId("device:ios:00008101-000A00000000001A".into()))
            .unwrap()
            .unwrap();
        assert_eq!(ios.model, "iPhone 13");
    }

    /// P3bT5-13: a transport's identity is forgotten once it has not been listed for a minute.
    #[test]
    fn identities_of_gone_transports_are_pruned() {
        let w = with_phone();
        let gone = emulator_line(7);
        w.learn(&gone, emulator("Pixel_9"));
        w.adb(&[line("SERIAL0001", "device", Some("1-1"))]);
        assert!(
            w.svc
                .identities
                .lock()
                .unwrap()
                .contains_key(&android::seen_key(&gone))
        );
        w.advance(IDENTITY_KEEP_MS);
        w.adb(&[line("SERIAL0001", "device", Some("1-1"))]);
        let ids = w.svc.identities.lock().unwrap();
        assert!(!ids.contains_key(&android::seen_key(&gone)));
        assert_eq!(ids.len(), 1, "the listed phone stays");
    }

    /// P3bT5-13: the devicectl JSON directory is the service's own, made only for iOS, and
    /// removed when the service ends.
    #[test]
    fn the_json_dir_is_per_service_and_removed() {
        let a = world();
        let b = world();
        assert_ne!(a.svc.json_dir, b.svc.json_dir);
        assert!(!a.svc.json_dir.exists(), "not made before a poller runs");
        let dir = a.svc.json_dir.clone();
        std::fs::create_dir_all(&dir).unwrap();
        let ios = a.svc.ios;
        drop(a);
        assert_eq!(dir.exists(), !ios);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3bT5-4: per-device work runs side by side as its own task, never twice at once, and its
    /// mark ends however it ends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_run_side_by_side_and_never_twice() {
        let w = world();
        let start = std::time::Instant::now();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let slow = |ran: Arc<std::sync::atomic::AtomicUsize>| {
            move |_: &DevicesService, _: &ResourceId| {
                std::thread::sleep(Duration::from_millis(300));
                ran.fetch_add(1, Ordering::SeqCst);
                None
            }
        };
        let (a, b) = (
            ResourceId("device:test:a".into()),
            ResourceId("device:test:b".into()),
        );
        assert!(
            w.svc
                .spawn_job(Job::Snapshot, a.clone(), Duration::ZERO, slow(ran.clone()))
        );
        assert!(
            w.svc
                .spawn_job(Job::Snapshot, b.clone(), Duration::ZERO, slow(ran.clone()))
        );
        assert!(
            !w.svc
                .spawn_job(Job::Snapshot, a.clone(), Duration::ZERO, slow(ran.clone())),
            "one per device and kind"
        );
        assert!(
            w.svc
                .spawn_job(Job::Telemetry, a.clone(), Duration::ZERO, |_, _| None),
            "another kind is its own"
        );
        while !w.svc.jobs.lock().unwrap().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(ran.load(Ordering::SeqCst), 2);
        assert!(
            start.elapsed() < Duration::from_millis(550),
            "the Long lane's two slots run together: {:?}",
            start.elapsed()
        );
        // A job that panics leaves no mark behind.
        assert!(
            w.svc
                .spawn_job(Job::Snapshot, a.clone(), Duration::ZERO, |_, _| panic!(
                    "boom"
                ))
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !w.svc.jobs.lock().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// P3bT5-4: `add` never runs its seeding snapshot on the request; the Long pass does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn add_returns_before_its_snapshot() {
        let w = with_phone();
        w.svc.add("SERIAL0001", None, true).unwrap();
        let r = ResourceId("device:android:SERIAL0001".into());
        assert_eq!(
            *w.svc.seeds.lock().unwrap(),
            vec![r.clone()],
            "owed, not run"
        );
        w.svc.clone().long_pass().await;
        assert!(w.svc.seeds.lock().unwrap().is_empty());
    }

    // ---- iOS ----

    fn iphone(udid: &str, transport: TransportKind, reachable: bool, paired: bool) -> PhysicalIos {
        PhysicalIos {
            udid: udid.into(),
            coredevice_id: format!("CD-{udid}"),
            name: "Test iPhone".into(),
            model: "iPhone 13".into(),
            os_version: Some("18.7".into()),
            transport,
            reachable,
            paired,
        }
    }

    #[test]
    fn ios_devices_and_simulators_are_listed() {
        let w = world();
        let sim = Simulator {
            udid: "SIM-0001".into(),
            name: "iPhone 17 Pro".into(),
            runtime: "iOS 26.5".into(),
            booted: true,
            last_booted_at: None,
        };
        w.svc.apply_ios(
            Ok(vec![
                iphone("UDID-W", TransportKind::Wired, false, true),
                iphone("UDID-N", TransportKind::Network, false, true),
            ]),
            Ok(vec![sim]),
            w.now(),
        );
        let wired = w.info("UDID-W");
        assert!(
            wired.connected,
            "a wired phone with its tunnel down is still attached"
        );
        assert_eq!(wired.coredevice_id.as_deref(), Some("CD-UDID-W"));
        assert_eq!(w.svc.find("CD-UDID-W").unwrap().resource, wired.resource);
        assert!(!w.info("UDID-N").connected, "unreachable over the network");
        assert_eq!(
            w.cache.resolve_udid("CD-UDID-N").unwrap().0,
            "device:ios:UDID-N"
        );
        assert_eq!(w.cache.resolve_udid(""), None);
        let sim = w.svc.add("SIM-0001", None, false).unwrap();
        assert_eq!(sim.alias.as_deref(), Some("iphone-17-pro"));
        assert!(sim.ready);
        let entry = &w.cache.resources()[0];
        assert_eq!(entry.attributes["udid"], "SIM-0001");
        assert_eq!(entry.attributes["kind"], "simulator");
        // devicectl fails for 30 s: the phones are disconnected; simctl's answer stays.
        let failed = || Err::<Vec<PhysicalIos>, String>("it timed out".into());
        let sims_failed = || Err::<Vec<Simulator>, String>("it exited with Some(1)".into());
        w.svc.apply_ios(failed(), sims_failed(), w.now());
        assert!(w.svc.list(false).tools.problems.is_empty(), "not yet");
        w.advance(STALE_MS);
        w.svc.apply_ios(failed(), sims_failed(), w.now());
        assert!(!w.info("UDID-W").connected);
        assert!(!w.info("SIM-0001").ready);
        // P3bT5-10: the person sees why.
        let problems = w.svc.list(false).tools.problems;
        assert!(
            problems.contains(&"devicectl list devices failed for 30 s: it timed out".to_owned())
                && problems.contains(
                    &"simctl list devices failed for 30 s: it exited with Some(1)".to_owned()
                ),
            "{problems:?}"
        );
        // One good answer each clears it.
        w.svc.apply_ios(Ok(Vec::new()), Ok(Vec::new()), w.now());
        assert!(w.svc.list(false).tools.problems.is_empty());
    }

    /// The fake `ideviceinfo` (the `fake_tool` example), its call log, and a lockdown directory.
    fn fake_ideviceinfo() -> (tempfile::TempDir, PathBuf) {
        let exe = escargot::CargoBuild::new()
            .example("fake_tool")
            .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .run()
            .expect("building the fake_tool example")
            .path()
            .to_path_buf();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let tool = dir.path().join("bin").join(Tool::Ideviceinfo.file_name());
        std::fs::copy(exe, &tool).unwrap();
        std::fs::write(
            dir.path().join("script.json"),
            r#"[{"tool": "ideviceinfo", "args_prefix": ["-u"], "stdout": "BatteryCurrentCapacity: 81\n"}]"#,
        )
        .unwrap();
        (dir, tool)
    }

    fn ideviceinfo_calls(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// P2-3, P3-8: `ideviceinfo` only for a registered, wired, paired phone, and only while its
    /// pair record exists, checked right before each call.
    #[test]
    fn ideviceinfo_only_for_paired_registered_phones_with_a_lockdown_record() {
        let (dir, tool) = fake_ideviceinfo();
        let w = world_with(
            Store::open_in_memory().unwrap(),
            ToolPaths {
                ideviceinfo: Some(tool),
                ..ToolPaths::default()
            },
        );
        let lockdown = tempfile::tempdir().unwrap();
        let record = |udid: &str| lockdown.path().join(format!("{udid}.plist"));
        w.svc.apply_ios(
            Ok(vec![
                iphone(
                    "00008101-000A00000000001A",
                    TransportKind::Wired,
                    true,
                    true,
                ),
                iphone(
                    "00008101-000A00000000002B",
                    TransportKind::Wired,
                    true,
                    false,
                ),
                iphone(
                    "00008101-000A00000000003C",
                    TransportKind::Network,
                    true,
                    true,
                ),
            ]),
            Ok(Vec::new()),
            w.now(),
        );
        for udid in [
            "00008101-000A00000000001A",
            "00008101-000A00000000002B",
            "00008101-000A00000000003C",
        ] {
            std::fs::write(record(udid), b"").unwrap();
        }
        let r1 = ResourceId("device:ios:00008101-000A00000000001A".into());
        // Unregistered: never asked.
        assert!(w.svc.ios_battery_in(&r1, lockdown.path()).is_none());
        assert_eq!(ideviceinfo_calls(dir.path()), 0);
        for udid in [
            "00008101-000A00000000001A",
            "00008101-000A00000000002B",
            "00008101-000A00000000003C",
        ] {
            w.svc.add(udid, Some(&udid.to_lowercase()), true).unwrap();
        }
        // Not paired, or over the network: never asked.
        for udid in ["00008101-000A00000000002B", "00008101-000A00000000003C"] {
            let r = ResourceId(format!("device:ios:{udid}"));
            assert!(w.svc.ios_battery_in(&r, lockdown.path()).is_none());
        }
        assert_eq!(ideviceinfo_calls(dir.path()), 0);
        let reading = w.svc.ios_battery_in(&r1, lockdown.path()).unwrap();
        assert_eq!(
            reading.battery_percent,
            crate::contract::devices::Reading::Known(81)
        );
        assert_eq!(ideviceinfo_calls(dir.path()), 1);
        // The record is removed between two calls: the second never runs.
        std::fs::remove_file(record("00008101-000A00000000001A")).unwrap();
        assert!(w.svc.ios_battery_in(&r1, lockdown.path()).is_none());
        assert_eq!(ideviceinfo_calls(dir.path()), 1);
        // No lockdown directory at all: never asked.
        let missing = lockdown.path().join("nope");
        std::fs::write(record("00008101-000A00000000001A"), b"").unwrap();
        assert!(w.svc.ios_battery_in(&r1, &missing).is_none());
        assert_eq!(ideviceinfo_calls(dir.path()), 1);
    }
}
