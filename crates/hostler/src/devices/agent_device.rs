//! agent-device sessions as foreign claims, `forget-sessions` and the stopper (P3b Task 9b).
//!
//! Only `agent-device device status --json` is read, never its claim files (D13). A session is a
//! foreign claim on the device it maps to, owned by the workspace its path is in (L50); the
//! engine resolves the owner. After every device claim end, no other workspace gets the device
//! before a good read that started after that end (L52, L54), ordered by the end counter, never
//! by clocks (P3-3). A read that fails keeps the last good sessions (L51).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::lanes::Lane;
use super::service::{DeviceCache, DevicesService, RawSession, SessionRead};
use super::tools::{self, AgentDeviceCmd, Call, Tool, ToolOutput};
use crate::claims::ForeignClaimSource;
use crate::claims::engine::{ClaimsEngine, ForgetOutcome};
use crate::cli::takeover::{HolderStopper, StopError};
use crate::contract::HostlerError;
use crate::contract::claims::{ForeignClaim, ForeignView, ResourceId, StopEvidence, StopTarget};
use crate::contract::devices::errors::person_required;
use crate::contract::devices::{ForgetRecord, Platform};
use crate::contract::secrets::errors::bad_request;
use crate::procs::ProcessTable;

/// The stopper's whole bound: `close`, then the status until the session is gone (D9).
pub const AGENT_DEVICE_STOP_BOUND: Duration = Duration::from_secs(20);
const STATUS_BOUND: Duration = Duration::from_secs(5);
const CLOSE_BOUND: Duration = Duration::from_secs(10);
const VERSION_BOUND: Duration = Duration::from_secs(5);
const VERSION_EVERY_MS: i64 = 60 * 60 * 1000;
const STOP_POLL: Duration = Duration::from_millis(500);
/// `canonical_workspace`'s bound (P3-9).
pub const CANONICAL_BOUND: Duration = Duration::from_secs(1);
/// The version hostler targets (D13); older ones only warn (P-3).
pub const TARGET_VERSION: (u32, u32, u32) = (0, 21, 36);
/// Debug builds: the directory standing in for `~/.agent-device`. With the fake tools and none
/// set, there is no claims dir (agent-device is not in use).
pub const TEST_AGENT_DEVICE_HOME_ENV: &str = "HOSTLER_TEST_AGENT_DEVICE_HOME";
/// The text every unreadable-status line ends with (L54).
pub const STATUS_FIX: &str =
    "fix: hostler devices tools --detect, or hostler devices tools --agent-device <path>";

/// What one sessions cycle did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionsPoll {
    /// A good read (L54): agent-device's status was read, or it is not in use here.
    pub good: bool,
    /// Why the read failed.
    pub reason: Option<String>,
}

/// The reader's own state across cycles.
#[derive(Debug, Default)]
pub struct ReaderState {
    version_checked_ms: Option<i64>,
    /// When agent-device was last searched for while none was found (P3bT9b-8).
    searched_ms: Option<i64>,
}

// ---------------------------------------------------------------------------------------------
// The status

/// One live claim of `device status --json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdClaim {
    pub device_id: String,
    /// `None`: a platform hostler does not know; a problem, never dropped silently (P3bT9b-6).
    pub platform: Option<Platform>,
    pub platform_text: String,
    /// `device` or `simulator` / `emulator`.
    pub kind: String,
    pub name: String,
    pub session: String,
    pub workspace: String,
    pub state_dir: String,
    pub pid: u32,
    pub start_time_text: String,
}

/// The live claims of `device status --json`, of any version's shape (`data.claims[]`, each
/// with `classification`, `device` and `owner`). `None` when the output is not that: the status
/// is unreadable. A claim without a session or device id is left out; one on a platform hostler
/// does not know is kept with `platform: None` (P3bT9b-6). `appleOs: "ios"` is iOS.
pub fn parse_status(json: &[u8]) -> Option<Vec<AdClaim>> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
        return None;
    }
    let claims = v.get("data")?.get("claims")?.as_array()?;
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    Some(
        claims
            .iter()
            .filter(|c| c.get("classification").and_then(|x| x.as_str()) == Some("live"))
            .filter_map(|c| {
                let device = c.get("device")?;
                let owner = c.get("owner")?;
                let platform_text = text(device, "platform");
                let platform = match (platform_text.as_str(), text(device, "appleOs").as_str()) {
                    ("ios", _) | (_, "ios") => Some(Platform::Ios),
                    ("android", _) => Some(Platform::Android),
                    _ => None,
                };
                let claim = AdClaim {
                    device_id: text(device, "id"),
                    platform,
                    platform_text,
                    kind: text(device, "kind"),
                    name: text(device, "name"),
                    session: text(owner, "session"),
                    workspace: text(owner, "workspace"),
                    state_dir: text(owner, "stateDir"),
                    pid: owner
                        .get("pid")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u32::try_from(p).ok())
                        .unwrap_or(0),
                    start_time_text: text(owner, "startTime"),
                };
                (!claim.device_id.is_empty() && !claim.session.is_empty()).then_some(claim)
            })
            .collect(),
    )
}

/// How long a canonical path is reused before it is asked again (a re-pointed symlink).
const CANONICAL_TTL: Duration = Duration::from_secs(60);
/// How long a path that could not be canonicalized is not asked again.
const CANONICAL_NEGATIVE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct CanonicalEntry {
    value: Option<String>,
    at: Instant,
    pending: bool,
}

fn canonical_cache() -> &'static Mutex<HashMap<String, CanonicalEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CanonicalEntry>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The workspace path as the registry stores paths (`project::display_path` of the canonical
/// path), so a symlinked or `/private/tmp` path maps to its workspace (L50). On its own thread,
/// outside the lanes, bounded (P3-9). `None` on a timeout or an error: such a session belongs to
/// no workspace and blocks every one; a lexical guess could make a nested worktree's session
/// look like its parent checkout's. At most one thread per path at a time, so a hung mount never
/// piles threads up; answers are reused for a minute, failures for 30 s (P3bT9b-7).
pub fn canonical_workspace(raw: &str, bound: Duration) -> Option<String> {
    canonical_with(raw, bound, |p| {
        std::fs::canonicalize(p)
            .ok()
            .map(|p| crate::project::display_path(&p))
    })
}

/// `canonical_workspace` with its canonicalizer given (tests).
pub fn canonical_with(
    raw: &str,
    bound: Duration,
    canonicalize: fn(&Path) -> Option<String>,
) -> Option<String> {
    if raw.is_empty() || !Path::new(raw).is_absolute() {
        return None;
    }
    let cache = canonical_cache();
    {
        let mut entries = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = entries.get(raw) {
            let ttl = if e.value.is_some() {
                CANONICAL_TTL
            } else {
                CANONICAL_NEGATIVE_TTL
            };
            if e.pending || e.at.elapsed() < ttl {
                return e.value.clone();
            }
        }
        let value = entries.get(raw).and_then(|e| e.value.clone());
        entries.insert(
            raw.to_owned(),
            CanonicalEntry {
                value,
                at: Instant::now(),
                pending: true,
            },
        );
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let path = PathBuf::from(raw);
    let key = raw.to_owned();
    // A plain thread: a stat stuck on a dead mount must never hold a pool thread for good. It
    // records its answer whenever that comes; until then the path is pending.
    let spawned = std::thread::Builder::new()
        .name("hostler-canonical".into())
        .spawn(move || {
            let value = canonicalize(&path);
            canonical_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    key,
                    CanonicalEntry {
                        value: value.clone(),
                        at: Instant::now(),
                        pending: false,
                    },
                );
            let _ = tx.send(value);
        });
    if spawned.is_err() {
        cache.lock().unwrap_or_else(|e| e.into_inner()).remove(raw);
        return None;
    }
    rx.recv_timeout(bound).ok().flatten()
}

/// At most 200 bytes of another program's text, fit for a terminal (L40).
pub(crate) fn sanitized(text: &str) -> String {
    let mut out = crate::secrets::sanitize_for_terminal(text);
    if out.len() > 200 {
        let mut cut = 200;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

/// The live claims as raw sessions, with their canonical workspace and the stopper's data. The
/// service maps them to devices in every rebuild (P3bT9b-1), never only here.
pub fn to_raw(
    claims: &[AdClaim],
    cmd: Option<&AgentDeviceCmd>,
    canonical: &dyn Fn(&str) -> Option<String>,
) -> Vec<RawSession> {
    claims
        .iter()
        .map(|c| {
            let platform = match c.platform {
                Some(Platform::Android) => "android".to_owned(),
                Some(Platform::Ios) => "ios".to_owned(),
                None => sanitized(&c.platform_text),
            };
            let mut stop = serde_json::json!({
                "session": c.session,
                "state_dir": c.state_dir,
                "platform": platform,
                "id": c.device_id,
                "pid": c.pid,
                "start_time": c.start_time_text,
            });
            if let Some(cmd) = cmd {
                stop["node"] = cmd.node.display().to_string().into();
                stop["script"] = cmd.script.display().to_string().into();
            }
            RawSession {
                platform: c.platform,
                platform_text: platform,
                device_id: c.device_id.clone(),
                name: sanitized(&c.name),
                session: c.session.clone(),
                workspace_path: canonical(&c.workspace),
                stop,
                since_ms: 0,
                mapped: Vec::new(),
                pinned: None,
                kept: false,
                forgotten: Vec::new(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The source

/// The engine's view of agent-device: the cache's sessions and their freshness from one read
/// guard (P3-2), and the end counter (P3-3). Runs under the claims lock; never waits on I/O.
pub struct AgentDeviceSource(pub Arc<DeviceCache>);

impl ForeignClaimSource for AgentDeviceSource {
    fn foreign(&self) -> Vec<ForeignClaim> {
        self.0.sessions()
    }

    fn collect(&self) -> ForeignView {
        self.0.collect_sessions()
    }

    /// The new count, after the increment; it wakes the sessions lane.
    fn claim_ended(&self, r: &ResourceId) -> u64 {
        self.0.claim_ended(r)
    }
}

// ---------------------------------------------------------------------------------------------
// The sessions cycle

/// Where agent-device's claims would be.
enum ClaimsDir {
    At(PathBuf),
    /// Debug builds with the fake tools and no test home: never in use.
    NotHere,
    /// No home to look in: whether it is in use cannot be told (P3bT9b-6).
    Unknown,
}

/// `~/.agent-device/device-claims`: agent-device is in use on this machine when it exists (P3-4).
fn claims_dir() -> ClaimsDir {
    if cfg!(debug_assertions) {
        if let Some(home) = std::env::var_os(TEST_AGENT_DEVICE_HOME_ENV) {
            return ClaimsDir::At(PathBuf::from(home).join("device-claims"));
        }
        if std::env::var_os(tools::FAKE_TOOLS_ENV).is_some() {
            return ClaimsDir::NotHere;
        }
    }
    match std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        Some(home) => ClaimsDir::At(
            PathBuf::from(home)
                .join(".agent-device")
                .join("device-claims"),
        ),
        None => ClaimsDir::Unknown,
    }
}

/// Whether agent-device is in use here; `Err` (a failed read, fail closed) when that cannot be
/// told: no home, or the directory cannot be checked (P3bT9b-6).
fn in_use_at(dir: ClaimsDir) -> Result<bool, String> {
    match dir {
        ClaimsDir::NotHere => Ok(false),
        ClaimsDir::Unknown => Err("no home directory to find agent-device's claims in".into()),
        ClaimsDir::At(dir) => dir
            .try_exists()
            .map_err(|_| "agent-device's claims directory cannot be checked".to_owned()),
    }
}

fn in_use() -> Result<bool, String> {
    in_use_at(claims_dir())
}

/// The claims dir check, then `device status --json` (Sessions lane). `Ok(None)`: agent-device is
/// not in use, a good read with no sessions.
fn read_status(svc: &DevicesService) -> Result<Option<Vec<AdClaim>>, String> {
    if !in_use()? {
        return Ok(None);
    }
    let mut cmd = svc.tools().agent_device;
    if needs_search(cmd.as_ref(), svc) {
        svc.search_agent_device();
        cmd = svc.tools().agent_device;
    }
    let Some(cmd) = cmd else {
        return Err("agent-device cannot be found".into());
    };
    let out = status(&cmd).map_err(|e| e.to_string())?;
    if out.timed_out {
        return Err("agent-device device status timed out".into());
    }
    if out.status != Some(0) || out.truncated {
        return Err(format!(
            "agent-device device status exited with {}",
            out.status
                .map_or_else(|| "a signal".to_owned(), |c| c.to_string())
        ));
    }
    parse_status(&out.stdout)
        .map(Some)
        .ok_or_else(|| "agent-device's status cannot be parsed".into())
}

/// How often agent-device is searched for while none is found (P3bT9b-8).
const SEARCH_EVERY_MS: i64 = 60_000;

/// L54: a recorded path that went away (nvm, a Node upgrade) is searched for again at once;
/// with none found, at most once a minute (P3bT9b-8).
fn needs_search(cmd: Option<&AgentDeviceCmd>, svc: &DevicesService) -> bool {
    match cmd {
        Some(c) => !c.node.is_file() || !c.script.is_file(),
        None => {
            let now = svc.now();
            let mut state = svc.agent.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .searched_ms
                .is_some_and(|at| now - at < SEARCH_EVERY_MS)
            {
                return false;
            }
            state.searched_ms = Some(now);
            true
        }
    }
}

fn run_node(
    cmd: &AgentDeviceCmd,
    args: &[&OsStr],
    env: &[(&str, &OsStr)],
    timeout: Duration,
) -> std::io::Result<ToolOutput> {
    let script = cmd.script.as_os_str();
    let mut all: Vec<&OsStr> = vec![script];
    all.extend_from_slice(args);
    tools::run_tool(&Call {
        tool: Tool::Node,
        path: &cmd.node,
        args: &all,
        env,
        stdin: None,
        timeout,
        adb_breakaway: false,
        adb_guard: None,
    })
}

fn status(cmd: &AgentDeviceCmd) -> std::io::Result<ToolOutput> {
    status_within(cmd, STATUS_BOUND)
}

fn status_within(cmd: &AgentDeviceCmd, bound: Duration) -> std::io::Result<ToolOutput> {
    run_node(
        cmd,
        &[
            OsStr::new("device"),
            OsStr::new("status"),
            OsStr::new("--json"),
        ],
        &[],
        bound,
    )
}

/// `1.0.N`-style versions; the first three numbers.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let word = text
        .split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = word.split(['.', '-', '+']).map(|p| p.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// One sessions cycle (P3-3): the end counter is captured first, then the claims dir and the
/// status are read; a good read sets `Since(captured)` in the cache under one write guard.
pub async fn poll(svc: &Arc<DevicesService>) -> SessionsPoll {
    let seq = svc.cache().ends();
    let started = svc.now();
    let reader = svc.clone();
    let read = svc
        .gate()
        .run(Lane::Sessions, None, move || read_status(&reader))
        .await;
    let read = match read {
        Ok(None) => SessionRead::Good {
            raw: Vec::new(),
            in_use: false,
        },
        Ok(Some(claims)) => {
            let cmd = svc.tools().agent_device;
            // Canonicalized on the blocking pool, outside every lane (P3-9).
            let raw = tokio::task::spawn_blocking(move || {
                to_raw(&claims, cmd.as_ref(), &|raw| {
                    canonical_workspace(raw, CANONICAL_BOUND)
                })
            })
            .await;
            read_of_mapping(raw)
        }
        Err(reason) => {
            // L53: a kept session whose owner daemon is certainly dead is dropped.
            let kept = svc.raw_sessions();
            let dead = tokio::task::spawn_blocking(move || dead_owners(&kept))
                .await
                .unwrap_or_default();
            SessionRead::Failed { reason, dead }
        }
    };
    svc.apply_session_read(read, seq, started)
}

/// A mapping that panicked is a failed read, never a good read with no sessions (P3bT9b-6).
pub fn read_of_mapping(raw: Result<Vec<RawSession>, tokio::task::JoinError>) -> SessionRead {
    match raw {
        Ok(raw) => SessionRead::Good { raw, in_use: true },
        Err(_) => SessionRead::Failed {
            reason: "agent-device's sessions could not be mapped".into(),
            dead: Vec::new(),
        },
    }
}

/// `--version` once an hour while agent-device is in use; an older one only warns (P-3). Runs
/// outside the sessions cycle, so `refresh_now` never waits for it (P3bT9b-4).
pub async fn check_version(svc: &Arc<DevicesService>) {
    let now = svc.now();
    let due = || {
        !svc.agent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .version_checked_ms
            .is_some_and(|at| now - at < VERSION_EVERY_MS)
    };
    if !due() {
        return;
    }
    let Some(cmd) = svc.tools().agent_device else {
        return;
    };
    if in_use() != Ok(true) {
        return;
    }
    // Marked only once it is in use, so a first use is checked at once (P3bT9b-11).
    svc.agent
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .version_checked_ms = Some(now);
    // On the Probe lane: it never holds the Sessions lane a takeover's fresh read needs
    // (P3bT9b-4).
    let out = svc
        .gate()
        .run(Lane::Probe, None, move || {
            run_node(&cmd, &[OsStr::new("--version")], &[], VERSION_BOUND).ok()
        })
        .await;
    let text = out
        .filter(|o| o.status == Some(0))
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let version = text.as_deref().and_then(parse_version);
    let warning = version.filter(|v| *v < TARGET_VERSION).map(|(a, b, c)| {
        format!(
            "agent-device {a}.{b}.{c} is older than {}.{}.{}, which hostler targets; it still \
             counts its sessions",
            TARGET_VERSION.0, TARGET_VERSION.1, TARGET_VERSION.2
        )
    });
    svc.set_agent_device_version(version.map(|(a, b, c)| format!("{a}.{b}.{c}")), warning);
}

/// The kept sessions whose owner daemon is certainly dead (L53), by `RawSession::key`.
fn dead_owners(kept: &[RawSession]) -> Vec<String> {
    let Ok(table) = crate::procs::SysProcessTable::new() else {
        return Vec::new();
    };
    dead_owners_in(&table, kept)
}

/// `dead_owners` with its process table given (tests).
fn dead_owners_in(table: &dyn ProcessTable, kept: &[RawSession]) -> Vec<String> {
    kept.iter()
        .filter_map(|r| {
            let pid = u32::try_from(r.stop.get("pid")?.as_u64()?).ok()?;
            let text = r.stop.get("start_time")?.as_str()?;
            (pid != 0 && owner_check(table, pid, text, Os::current()) == OwnerCheck::CertainlyDead)
                .then(|| r.key())
        })
        .collect()
}

/// The fresh read before a takeover commit (P-2): it waits for a cycle in flight, then runs its
/// own, so it started after the takeover's stops. Bounded at 12 s, inside the client's 15 s
/// `seize_commit` bound: a read that does not come in time refuses the commit, never a commit
/// after the taker gave up (P3bT9b-4).
pub async fn refresh_now(svc: &Arc<DevicesService>) -> Result<(), String> {
    refresh_within(svc, REFRESH_BOUND).await
}

/// `refresh_now`'s bound.
pub const REFRESH_BOUND: Duration = Duration::from_secs(12);

pub async fn refresh_within(svc: &Arc<DevicesService>, bound: Duration) -> Result<(), String> {
    if !super::service::tracks_sessions() {
        return Ok(());
    }
    // A cycle that starts after this call may answer it: it started after the stops.
    let ticket = svc.cycles_started();
    let Ok(poll) = tokio::time::timeout(bound, svc.sessions_round_after(ticket)).await else {
        return Err(format!(
            "no agent-device status read within {} s",
            bound.as_secs()
        ));
    };
    if poll.good {
        Ok(())
    } else {
        Err(poll
            .reason
            .unwrap_or_else(|| "agent-device's status cannot be read".into()))
    }
}

/// Whether a good sessions read that started at `t_ms` or later completed within `bound`
/// (`devices.show { fresh_sessions }`, L49). It asks for a read at once.
pub async fn read_started_after(svc: &DevicesService, t_ms: i64, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    svc.cache().wake_sessions();
    loop {
        if svc.last_good_read_started().is_some_and(|at| at >= t_ms) {
            return true;
        }
        // While reads fail no good one will come: answer at once (the caller says the sessions
        // are unknown, P3-14).
        if svc.sessions_unreadable().is_some() || Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How long `forget_sessions` waits for the claims lock.
const FORGET_LOCK_BOUND: Duration = Duration::from_secs(3);

/// L54: a person releases one device from the sessions wait while agent-device's status cannot be
/// read. It stops nothing and closes no session; it is recorded and logged.
pub async fn forget_sessions(
    svc: &DevicesService,
    engine: &tokio::sync::Mutex<ClaimsEngine>,
    device: &str,
    person: bool,
    by_workspace_path: Option<String>,
    by_pid: Option<u32>,
) -> Result<ForgetRecord, HostlerError> {
    if !person {
        return Err(person_required(&format!("forget-sessions {device}")));
    }
    let info = svc.resolve(device)?;
    let Some(reason) = svc.sessions_unreadable() else {
        return Err(bad_request(
            "nothing to forget: agent-device status reads fine".into(),
        ));
    };
    // The claims lock bounded, as `devices.remove` takes it, and only for CPU work (P3bT9b-5).
    let Ok(mut engine) = tokio::time::timeout(FORGET_LOCK_BOUND, engine.lock()).await else {
        return Err(bad_request(
            "the claims engine is busy; run forget-sessions again".into(),
        ));
    };
    // The kept sessions first, checked under the cache guard: only kept ones, and only while
    // the status is still unreadable; once it reads again nothing is dropped.
    let kept = svc.drop_sessions_on(&info.resource).map_err(bad_request)?;
    let outcome = engine.forget_device_end(&info.resource)?;
    drop(engine);
    if outcome == ForgetOutcome::NotClosed && kept.is_empty() {
        return Err(bad_request(format!(
            "nothing to forget: {device} is not closed"
        )));
    }
    let record = ForgetRecord {
        id: uuid::Uuid::new_v4().to_string(),
        resource: info.resource.clone(),
        sessions: kept.iter().map(|s| sanitized(s)).collect(),
        by_workspace_path,
        by_pid,
        reason: format!("agent-device status unreadable: {reason}"),
        at_ms: svc.now(),
    };
    svc.record_forget(&record);
    tracing::warn!(
        resource = %record.resource.0,
        sessions = record.sessions.len(),
        "forget-sessions: a person released the device from the agent-device wait"
    );
    Ok(record)
}

// ---------------------------------------------------------------------------------------------
// L53: is an owner daemon certainly dead?

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerCheck {
    CertainlyDead,
    Alive,
}

/// A host-local `ctime` time: `Thu Nov  5 23:29:41 2026`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub fn parse_ctime(text: &str) -> Option<Civil> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let [_, month, day, hms, year] = words.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS.iter().position(|m| m == month)? as u32 + 1;
    let mut hms = hms.split(':').map(|p| p.parse::<u32>().ok());
    let (hour, minute, second) = (hms.next()??, hms.next()??, hms.next()??);
    let civil = Civil {
        year: year.parse().ok()?,
        month,
        day: day.parse().ok()?,
        hour,
        minute,
        second,
    };
    (civil.day >= 1 && civil.day <= 31 && hour < 24 && minute < 60 && second < 61).then_some(civil)
}

/// The Unix seconds a host-local time can mean: one, or two in the DST fall-back hour, or none
/// in a skipped hour. Windows: none (its format is unverified, so nothing is ever dead by time).
pub fn local_readings(c: &Civil) -> Vec<i64> {
    #[cfg(unix)]
    {
        let mut out = Vec::new();
        for isdst in [0, 1] {
            // SAFETY: `tm` is a plain struct, fully initialised by zeroed() and the fields below;
            // mktime and localtime_r only read and write the structs passed.
            unsafe {
                let mut tm: libc::tm = std::mem::zeroed();
                tm.tm_year = c.year - 1900;
                tm.tm_mon = c.month as i32 - 1;
                tm.tm_mday = c.day as i32;
                tm.tm_hour = c.hour as i32;
                tm.tm_min = c.minute as i32;
                tm.tm_sec = c.second as i32;
                tm.tm_isdst = isdst;
                let t = libc::mktime(&mut tm);
                if t == -1 {
                    continue;
                }
                let mut back: libc::tm = std::mem::zeroed();
                if libc::localtime_r(&t, &mut back).is_null() {
                    continue;
                }
                let same = back.tm_year == c.year - 1900
                    && back.tm_mon == c.month as i32 - 1
                    && back.tm_mday == c.day as i32
                    && back.tm_hour == c.hour as i32
                    && back.tm_min == c.minute as i32
                    && back.tm_sec == c.second as i32;
                let t = t as i64;
                if same && !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }
    #[cfg(not(unix))]
    {
        let _ = c;
        Vec::new()
    }
}

/// Linux: `/proc/stat`'s `btime` and the clock tick, to read a start time in ticks since boot.
fn linux_boot() -> Option<(i64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let btime = stat
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        // SAFETY: sysconf has no preconditions.
        let tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        (tck > 0).then_some((btime, tck as u64))
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// The process's start in Unix seconds from `ProcInfo::start_time`: macOS µs since the epoch;
/// Linux ticks since boot (`btime + ticks / CLK_TCK`); Windows a FILETIME.
fn start_seconds(start: u64, os: Os, boot: Option<(i64, u64)>) -> Option<i64> {
    match os {
        Os::MacOs => Some((start / 1_000_000) as i64),
        Os::Linux => {
            let (btime, tck) = boot?;
            Some(btime + (start / tck.max(1)) as i64)
        }
        Os::Windows => Some((start / 10_000_000) as i64 - 11_644_473_600),
    }
}

/// The command line of `pid`, when it can be read: Linux `/proc/<pid>/cmdline`, macOS
/// `KERN_PROCARGS2` (same user). Windows: none here; the image name comes from the table.
fn command_line(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(String::from_utf8_lossy(&bytes).replace('\0', " "))
    }
    #[cfg(target_os = "macos")]
    {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut buf = vec![0u8; 256 * 1024];
        let mut size = buf.len();
        // SAFETY: `mib` names a readable sysctl; `buf` is writable for `size` bytes, and the
        // kernel writes at most that many.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size <= 4 {
            return None;
        }
        Some(String::from_utf8_lossy(&buf[4..size]).replace('\0', " "))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// L53, P3-7: `CertainlyDead` only when the OS says no process has `pid` (one it cannot read
/// exists, CRA-1), when it is a zombie, or when the process clearly is
/// another, newer program: its command line is readable and names no `agent-device` package,
/// **and** it started more than 5 s after `start_time_text` under every reading of that local
/// time. Anything uncertain is `Alive` (fail closed, L51).
pub fn owner_check(
    table: &dyn ProcessTable,
    pid: u32,
    start_time_text: &str,
    os: Os,
) -> OwnerCheck {
    owner_check_with(
        table,
        &command_line,
        &local_readings,
        linux_boot(),
        pid,
        start_time_text,
        os,
    )
}

/// `owner_check` with its OS reads given (tests).
pub fn owner_check_with(
    table: &dyn ProcessTable,
    command_line: &dyn Fn(u32) -> Option<String>,
    readings: &dyn Fn(&Civil) -> Vec<i64>,
    boot: Option<(i64, u64)>,
    pid: u32,
    start_time_text: &str,
    os: Os,
) -> OwnerCheck {
    let Some(info) = table.info(pid) else {
        // Unreadable is not gone (another user's or an elevated daemon, `hidepid`): dead only
        // when the OS says no process has the pid (CRA-1).
        return if table.exists(pid) == Some(false) {
            OwnerCheck::CertainlyDead
        } else {
            OwnerCheck::Alive
        };
    };
    if info.zombie {
        // An exited daemon serves no session.
        return OwnerCheck::CertainlyDead;
    }
    let other_program = match os {
        // The image name only: a `node` cannot be told from agent-device's daemon.
        Os::Windows => table.process_name(pid).is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            !name.contains("agent-device") && name != "node.exe" && name != "node"
        }),
        _ => command_line(pid).is_some_and(|cmd| !cmd.contains("agent-device")),
    };
    if !other_program {
        return OwnerCheck::Alive;
    }
    let Some(started) = info.start_time.and_then(|s| start_seconds(s, os, boot)) else {
        return OwnerCheck::Alive;
    };
    let Some(civil) = parse_ctime(start_time_text) else {
        return OwnerCheck::Alive;
    };
    let recorded = readings(&civil);
    if !recorded.is_empty() && recorded.iter().all(|&r| started > r + 5) {
        OwnerCheck::CertainlyDead
    } else {
        OwnerCheck::Alive
    }
}

// ---------------------------------------------------------------------------------------------
// The stopper (CLI side)

/// Closes the holder's agent-device session in a takeover the operator permitted (D9): `close`
/// with the service's own node and script, then the status until the session is gone, all within
/// 20 s. It never runs anything else.
pub struct AgentDeviceStopper;

struct Target {
    session: String,
    platform: String,
    id: String,
    state_dir: String,
    cmd: AgentDeviceCmd,
}

fn target_of(t: &StopTarget) -> Option<Target> {
    let StopTarget::Foreign { source, data, .. } = t else {
        return None;
    };
    if source != "agent-device" {
        return None;
    }
    let field = |k: &str| data.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    Some(Target {
        session: field("session")?,
        platform: field("platform")?,
        id: field("id")?,
        state_dir: field("state_dir").unwrap_or_default(),
        cmd: AgentDeviceCmd {
            node: PathBuf::from(field("node")?),
            script: PathBuf::from(field("script")?),
        },
    })
}

/// Whether the status still lists the session on that device, asked within `bound`.
fn still_listed(t: &Target, bound: Duration) -> Result<bool, StopError> {
    let out = status_within(&t.cmd, bound)
        .map_err(|e| StopError::Unknown(format!("agent-device could not run: {e}")))?;
    if out.timed_out || out.status != Some(0) {
        return Err(StopError::Unknown(
            "agent-device status unreadable, so the session cannot be seen gone".into(),
        ));
    }
    let claims = parse_status(&out.stdout)
        .ok_or_else(|| StopError::Unknown("agent-device's status cannot be parsed".into()))?;
    Ok(claims
        .iter()
        .any(|c| c.session == t.session && c.device_id == t.id))
}

/// A stopper call's bound: its own `cap`, or what is left until `deadline` when that is less;
/// `StillAlive` once the deadline has passed (P3bT9b-9).
pub(crate) fn call_bound(
    deadline: Instant,
    cap: Duration,
    now: Instant,
) -> Result<Duration, StopError> {
    let left = deadline.saturating_duration_since(now);
    if left.is_zero() {
        return Err(StopError::StillAlive);
    }
    Ok(left.min(cap))
}

fn gone() -> StopEvidence {
    StopEvidence::Gone { target: 0 }
}

impl HolderStopper for AgentDeviceStopper {
    fn handles(&self, t: &StopTarget) -> bool {
        matches!(t, StopTarget::Foreign { source, .. } if source == "agent-device")
    }

    fn stop(&self, t: &StopTarget, grace: Duration) -> Result<StopEvidence, StopError> {
        self.stop_counted(t, grace, &AtomicBool::new(false))
            .map(|(e, _)| e)
    }

    fn stop_unless(
        &self,
        t: &StopTarget,
        grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<StopEvidence, StopError> {
        self.stop_counted(t, grace, cancel).map(|(e, _)| e)
    }

    /// Not counted as stopped when the session was already closed.
    fn stop_counted(
        &self,
        t: &StopTarget,
        _grace: Duration,
        cancel: &AtomicBool,
    ) -> Result<(StopEvidence, bool), StopError> {
        let deadline = Instant::now() + AGENT_DEVICE_STOP_BOUND;
        // Every call within what is left of the 20 s (P3bT9b-9).
        let left = |cap: Duration| call_bound(deadline, cap, Instant::now());
        let target = target_of(t).ok_or(StopError::Unsupported)?;
        if !still_listed(&target, left(STATUS_BOUND)?)? {
            return Ok((gone(), false));
        }
        let cancelled = || cancel.load(std::sync::atomic::Ordering::SeqCst);
        if cancelled() {
            return Err(StopError::Unknown("the takeover was interrupted".into()));
        }
        let flag = if target.platform == "ios" {
            "--udid"
        } else {
            "--serial"
        };
        let args = [
            OsStr::new("close"),
            OsStr::new("--session"),
            OsStr::new(&target.session),
            OsStr::new("--platform"),
            OsStr::new(&target.platform),
            OsStr::new(flag),
            OsStr::new(&target.id),
            OsStr::new("--json"),
        ];
        let state_dir = OsStr::new(&target.state_dir);
        let env: Vec<(&str, &OsStr)> = if target.state_dir.is_empty() {
            Vec::new()
        } else {
            vec![("AGENT_DEVICE_STATE_DIR", state_dir)]
        };
        let out = run_node(&target.cmd, &args, &env, left(CLOSE_BOUND)?)
            .map_err(|e| StopError::Unknown(format!("agent-device close could not run: {e}")))?;
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if text.contains("registration ownership") {
            return Err(StopError::Unknown(
                "an older agent-device daemon owns this device; stop it with the agent-device \
                 version that started it"
                    .into(),
            ));
        }
        if out.timed_out || out.status != Some(0) {
            let first = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("no output");
            return Err(StopError::Unknown(format!(
                "agent-device close failed: {}",
                sanitized(first)
            )));
        }
        loop {
            if cancelled() {
                return Err(StopError::Unknown("the takeover was interrupted".into()));
            }
            match still_listed(&target, left(STATUS_BOUND)?) {
                Ok(false) => return Ok((gone(), true)),
                Ok(true) => {}
                // A status call cut short by the 20 s: the session was not seen gone in time.
                Err(_) if Instant::now() + STOP_POLL >= deadline => {
                    return Err(StopError::StillAlive);
                }
                Err(e) => return Err(e),
            }
            if Instant::now() + STOP_POLL >= deadline {
                return Err(StopError::StillAlive);
            }
            std::thread::sleep(STOP_POLL);
        }
    }
}

#[cfg(test)]
mod tests;
