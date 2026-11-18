//! `--boot` and the shutdown rule for what hostler booted (P3b Task 7; D10, L49, P-6, P2-14).
//!
//! `boot` starts an existing AVD or simulator for a live claim. Only a boot from the shut-down
//! state is recorded (`device_boots`), with the identity a later shutdown must find again: the
//! emulator's pid, start time and `emulator-N`, or the simulator's `lastBootedAt`. One boot per
//! resource is in flight; a second caller joins it. The boot runs in its own task under one
//! deadline, so a client that goes away (Ctrl+C) never cancels it: the shutdown rule cleans up.
//!
//! `tick` (every claims tick) drops a record once the device certainly stopped (the emulator's
//! process is gone; a good `simctl list` shows the simulator shut down), and shuts a device down
//! after 2 minutes without claims or sessions: mark it `shutting_down` (the catalog lists it
//! unavailable), ask the engine `device_in_use` under the claims lock (a holder or a pending
//! Exact request cancels), then, in one Long-lane call, verify the identity, re-read the
//! sessions and run `emu kill` / `simctl shutdown`. Nothing here runs under the claims lock but
//! that one question.
//!
//! `claims_ended` writes L49's open-session line for a device hold that expired or was reaped:
//! no CLI is there to print it.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};

use super::lanes::Lane;
use super::service::{ClaimsView, DevicesService};
use super::tools::{self, Tool, ToolOutput};
use crate::claims::engine::InUse;
use crate::contract::claims::{ClaimKind, ClaimsSnapshot, EndReason, ResourceId};
use crate::contract::devices::{DeviceInfo, DeviceKind, Platform, errors};
use crate::contract::secrets::errors::bad_request;
use crate::contract::{ErrorCode, HostlerError};
use crate::procs::{ProcessTable, SysProcessTable};
use crate::secrets::sanitize_for_terminal;

/// A boot is ready within this (Global Constraints: 180 s), inside the client's 200 s bound;
/// every step, lane waits included, is held to it (P3bT7-5).
const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
/// No claim and no session for this long: hostler shuts down what it booted (L51).
const SHUTDOWN_GRACE_MS: i64 = 120_000;
/// A failed shutdown is tried again this often, for this long, then reported.
const RETRY_EVERY_MS: i64 = 10_000;
const RETRY_FOR_MS: i64 = 600_000;
/// After a good shutdown call the device stays marked until discovery sees it gone, at most this.
const GONE_WITHIN_MS: i64 = 60_000;
/// How often a boot looks at the cache for the device being ready.
const READY_POLL: Duration = Duration::from_millis(200);

const SIMCTL_BOOT_BOUND: Duration = Duration::from_secs(30);
const SIMCTL_SHUTDOWN_BOUND: Duration = Duration::from_secs(30);
const SIMCTL_LIST_BOUND: Duration = Duration::from_secs(10);
const ADB_EMU_BOUND: Duration = Duration::from_secs(5);

/// Debug builds: the 180 s boot bound and the 2 min grace, in ms (tests).
pub const TEST_BOOT_TIMEOUT_ENV: &str = "HOSTLER_TEST_BOOT_TIMEOUT_MS";
pub const TEST_SHUTDOWN_GRACE_ENV: &str = "HOSTLER_TEST_SHUTDOWN_GRACE_MS";

fn test_ms(name: &str) -> Option<u64> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name).ok()?.parse().ok().filter(|ms| *ms > 0)
}

/// One `device_boots` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootRow {
    pub resource: ResourceId,
    pub booted_at_ms: i64,
    /// A `BootIdentity`.
    pub identity_json: String,
    pub claim_id: String,
    pub idle_since_ms: Option<i64>,
}

/// What a shutdown must find again (P-6, P2-14).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BootIdentity {
    /// `serial`: the `emulator-N` it came up as; `None` until discovery saw it. `not`: the
    /// `emulator-N`s adb listed before the spawn, never ours (P3bT7-4).
    Emulator {
        avd: String,
        pid: u32,
        start_time: u64,
        serial: Option<String>,
        #[serde(default)]
        not: Vec<String>,
    },
    Simulator {
        udid: String,
        last_booted_at: Option<String>,
    },
}

impl BootRow {
    fn identity(&self) -> Option<BootIdentity> {
        serde_json::from_str(&self.identity_json).ok()
    }

    fn with_identity(&self, identity: &BootIdentity) -> BootRow {
        BootRow {
            identity_json: serde_json::to_string(identity).expect("an identity encodes"),
            ..self.clone()
        }
    }
}

/// `simctl boot`'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimBoot {
    Booted,
    /// "Unable to boot device in current state: Booted": someone else booted it.
    AlreadyBooted,
    Failed(String),
}

/// What the one Long-lane call of a shutdown did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// A session, or no single transport of ours, right before the kill: not now.
    Busy,
    /// Not the instance hostler booted: the record goes, nothing is shut down.
    Mismatch,
    Done,
    Failed,
}

pub type InUseFuture = Pin<Box<dyn Future<Output = Option<InUse>> + Send>>;
/// The engine's `device_in_use` under the claims lock; `None`: no answer (the device is kept).
pub type InUseCheck = Box<dyn Fn(ResourceId) -> InUseFuture + Send + Sync>;

type BootFuture = Shared<BoxFuture<'static, Result<DeviceInfo, HostlerError>>>;

/// The service's boot state: in-flight boots, running shutdowns, retries (`DevicesService` holds
/// one).
#[derive(Default)]
pub struct Boots {
    in_flight: Mutex<HashMap<ResourceId, BootFuture>>,
    /// A shutdown task runs for these.
    shutting: Mutex<HashSet<ResourceId>>,
    /// The first and the last failed shutdown.
    failed: Mutex<HashMap<ResourceId, (i64, i64)>>,
    /// Shut down at: the mark stays until discovery no longer sees the device.
    stopping: Mutex<HashMap<ResourceId, i64>>,
    in_use: OnceLock<InUseCheck>,
    ticking: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Boots {
    /// The server sets it once (P2-14).
    pub fn set_in_use(&self, check: InUseCheck) {
        let _ = self.in_use.set(check);
    }

    fn booting(&self, r: &ResourceId) -> bool {
        lock(&self.in_flight).contains_key(r)
    }
}

/// The calls of a shutdown's one Long-lane step, synchronous (P3bT7-6); the unit tests fake it.
pub trait Tools {
    fn device(&self, r: &ResourceId) -> Option<DeviceInfo>;
    /// Whether a good agent-device read says what sessions are on the devices (L63).
    fn sessions_known(&self) -> bool;
    /// Whether the process `pid` that started at `start_time` still runs.
    fn alive(&self, pid: u32, start_time: u64) -> bool;
    /// `adb -s emulator-N emu avd name`.
    fn emulator_avd(&self, serial: &str) -> Option<String>;
    fn emu_kill(&self, serial: &str) -> bool;
    /// `lastBootedAt` from a fresh `simctl list devices --json`.
    fn last_booted_at(&self, udid: &str) -> Option<String>;
    fn simctl_shutdown(&self, udid: &str) -> bool;
}

/// What boot and the shutdown rule need from the service; the unit tests give a fake.
pub trait Host: Clone + Send + Sync + 'static {
    fn now(&self) -> i64;
    fn boots(&self) -> &Boots;
    fn device(&self, r: &ResourceId) -> Option<DeviceInfo>;
    fn rows(&self) -> Vec<BootRow>;
    fn put_row(&self, row: &BootRow);
    fn set_idle(&self, r: &ResourceId, since: Option<i64>);
    fn drop_row(&self, r: &ResourceId);
    fn mark_shutting_down(&self, r: &ResourceId, on: bool);
    fn mark_booted(&self, r: &ResourceId, on: bool);
    fn alive(&self, pid: u32, start_time: u64) -> bool;
    /// Whether a good agent-device read says what sessions are on the devices (L63).
    fn sessions_known(&self) -> bool;
    /// Every `emulator-N` adb lists now, identified or not.
    fn emulator_serials(&self) -> Vec<String>;
    /// A good `simctl list`'s answer for `udid`; `None` while there is none.
    fn simulator_booted(&self, udid: &str) -> Option<bool>;
    fn boot_timeout(&self) -> Duration;
    fn grace_ms(&self) -> i64;
    fn log_path(&self, avd: &str) -> PathBuf;
    /// The device this call booted is ready (F-3): the snapshot pass runs now, so a claim window
    /// that waited for it gets its claim-open snapshot. Nothing waits on that snapshot.
    fn booted_ready(&self, _r: &ResourceId) {}
    /// Spawns nothing once `deadline` passed (a lane wait may have eaten the bound).
    fn spawn_emulator(
        &self,
        r: &ResourceId,
        avd: &str,
        log: PathBuf,
        deadline: Instant,
    ) -> impl Future<Output = Result<(u32, u64), String>> + Send;
    fn simctl_boot(
        &self,
        r: &ResourceId,
        udid: &str,
        deadline: Instant,
    ) -> impl Future<Output = SimBoot> + Send;
    fn bootstatus(
        &self,
        r: &ResourceId,
        udid: &str,
        deadline: Instant,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn last_booted_at(
        &self,
        r: &ResourceId,
        udid: &str,
        deadline: Instant,
    ) -> impl Future<Output = Option<String>> + Send;
    /// `verify_and_stop` in one Long-lane call.
    fn verify_and_stop(
        &self,
        r: &ResourceId,
        identity: BootIdentity,
    ) -> impl Future<Output = Stopped> + Send;
    fn in_use(&self, r: &ResourceId) -> impl Future<Output = Option<InUse>> + Send;
}

/// What is left of `deadline`, at most `bound`; `None` once it passed.
fn within(deadline: Instant, bound: Duration) -> Option<Duration> {
    let left = deadline.checked_duration_since(Instant::now())?;
    (!left.is_zero()).then(|| left.min(bound))
}

// ---------------------------------------------------------------------------------------------
// devices.boot

/// P2-19: `claim_id` must be a live lease or hold of `workspace_id` on `resource`. The server
/// reads `snapshot` under the claims lock.
pub fn check_claim(
    snapshot: &ClaimsSnapshot,
    resource: &ResourceId,
    claim_id: &str,
    workspace_id: &str,
) -> Result<(), HostlerError> {
    let ours = snapshot
        .resources
        .iter()
        .filter(|s| s.resource.id == *resource)
        .flat_map(|s| &s.claims)
        .any(|c| {
            c.id == claim_id
                && c.workspace.id == workspace_id
                && matches!(c.kind, ClaimKind::Lease | ClaimKind::Hold)
        });
    if ours {
        Ok(())
    } else {
        Err(bad_request(format!(
            "{claim_id} is not a live lease or hold of this workspace on {}; hostler boots a \
             device only for its claim",
            resource.0
        )))
    }
}

/// `devices.boot`, after `check_claim`: boots `r` (joining a boot in flight) and answers the
/// device once it is ready.
pub async fn boot(
    svc: &Arc<DevicesService>,
    r: &ResourceId,
    claim_id: &str,
) -> Result<DeviceInfo, HostlerError> {
    boot_with(Real(svc.clone()), r, claim_id).await
}

async fn boot_with<H: Host>(
    h: H,
    r: &ResourceId,
    claim_id: &str,
) -> Result<DeviceInfo, HostlerError> {
    let joined = {
        let mut in_flight = lock(&h.boots().in_flight);
        match in_flight.get(r) {
            Some(f) => f.clone(),
            None => {
                let (h2, r2, claim) = (h.clone(), r.clone(), claim_id.to_owned());
                // Its own task: a caller that goes away never cancels a boot (P2-19).
                let task = tokio::spawn(async move { boot_once(&h2, &r2, &claim).await });
                let (h3, r3) = (h.clone(), r.clone());
                let f = async move {
                    let result = task.await;
                    // Also after a panic, so the next boot is a new one (P3bT7-9).
                    lock(&h3.boots().in_flight).remove(&r3);
                    result.unwrap_or_else(|e| {
                        Err(HostlerError {
                            code: ErrorCode::Internal,
                            message: format!("the boot task failed: {e}"),
                            details: serde_json::json!({}),
                            recovery: None,
                        })
                    })
                }
                .boxed()
                .shared();
                in_flight.insert(r.clone(), f.clone());
                f
            }
        }
    };
    joined.await
}

/// An emulator runs while it has an adb transport (an AVD that exists but does not run has
/// none); a simulator while it is booted.
fn running(d: &DeviceInfo) -> bool {
    match d.kind {
        DeviceKind::Emulator => !d.transports.is_empty(),
        _ => d.ready,
    }
}

fn alias_of(info: &DeviceInfo) -> String {
    info.alias
        .clone()
        .unwrap_or_else(|| info.resource.0.clone())
}

/// The one transport a booted emulator may be learnt by: exactly one, and none adb listed
/// before the spawn (a person's instance of the same AVD never becomes ours, P3bT7-4).
fn learnable(d: &DeviceInfo, not: &[String]) -> Option<String> {
    match d.transports.as_slice() {
        [t] if !not.contains(&t.address) => Some(t.address.clone()),
        _ => None,
    }
}

async fn boot_once<H: Host>(
    h: &H,
    r: &ResourceId,
    claim_id: &str,
) -> Result<DeviceInfo, HostlerError> {
    let Some(info) = h.device(r) else {
        return Err(errors::device_not_found(&r.0));
    };
    let alias = alias_of(&info);
    if info.kind == DeviceKind::Physical {
        return Err(bad_request(format!(
            "{alias} is a physical device; hostler boots only emulators and simulators"
        )));
    }
    // Already booted (by anyone): no record.
    if info.ready {
        return Ok(info);
    }
    let bound = h.boot_timeout();
    let deadline = Instant::now() + bound;
    let late = || format!("not ready within {} s", bound.as_secs());
    let started = h.now();
    let mut log = "simctl".to_owned();
    let identity = match info.kind {
        DeviceKind::Emulator => {
            // Running but not ready yet: someone else started it; wait without a record.
            if running(&info) {
                None
            } else {
                let path = h.log_path(&info.hardware_id);
                log = path.display().to_string();
                let not = h.emulator_serials();
                let (pid, start_time) = h
                    .spawn_emulator(r, &info.hardware_id, path, deadline)
                    .await
                    .map_err(|why| errors::boot_failed(&alias, &why, &log))?;
                Some(BootIdentity::Emulator {
                    avd: info.hardware_id.clone(),
                    pid,
                    start_time,
                    serial: None,
                    not,
                })
            }
        }
        _ => match h.simctl_boot(r, &info.hardware_id, deadline).await {
            SimBoot::AlreadyBooted => None,
            SimBoot::Failed(why) => return Err(errors::boot_failed(&alias, &why, &log)),
            SimBoot::Booted => {
                let waited = h.bootstatus(r, &info.hardware_id, deadline).await;
                let last_booted_at = h.last_booted_at(r, &info.hardware_id, deadline).await;
                let identity = BootIdentity::Simulator {
                    udid: info.hardware_id.clone(),
                    last_booted_at,
                };
                if let Err(why) = waited {
                    // Booted by us, so recorded anyway: the shutdown rule cleans it up.
                    record(h, r, claim_id, &identity, started);
                    return Err(errors::boot_failed(&alias, &why, &log));
                }
                Some(identity)
            }
        },
    };
    // Recorded at once: a boot that times out is still cleaned up (P-6).
    if let Some(identity) = &identity {
        record(h, r, claim_id, identity, started);
    }
    loop {
        if let Some(now) = h.device(r).filter(|d| d.ready && running(d)) {
            // The `emulator-N` it came up as, before anyone could stop and restart it.
            if let Some(BootIdentity::Emulator {
                avd,
                pid,
                start_time,
                serial: None,
                not,
            }) = &identity
                && let Some(serial) = learnable(&now, not)
            {
                let seen = BootIdentity::Emulator {
                    avd: avd.clone(),
                    pid: *pid,
                    start_time: *start_time,
                    serial: Some(serial),
                    not: not.clone(),
                };
                record(h, r, claim_id, &seen, started);
            }
            h.booted_ready(r);
            return Ok(now);
        }
        if Instant::now() >= deadline {
            return Err(errors::boot_failed(&alias, &late(), &log));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

fn record<H: Host>(h: &H, r: &ResourceId, claim_id: &str, identity: &BootIdentity, at: i64) {
    let row = BootRow {
        resource: r.clone(),
        booted_at_ms: at,
        identity_json: String::new(),
        claim_id: claim_id.to_owned(),
        idle_since_ms: None,
    }
    .with_identity(identity);
    h.put_row(&row);
    h.mark_booted(r, true);
}

// ---------------------------------------------------------------------------------------------
// The shutdown rule

/// What one tick does with one record.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Keep,
    /// The device certainly stopped: the record goes.
    Drop,
    /// The emulator's own `emulator-N` is known now: remember it.
    Learn(BootIdentity),
    SetIdle(i64),
    ClearIdle,
    ShutDown,
}

/// What the tick knows about one record's device.
struct Seen<'a> {
    info: Option<&'a DeviceInfo>,
    /// Claims or sessions on it.
    busy: bool,
    /// A good `simctl list`'s answer, for a simulator.
    simulator_booted: Option<bool>,
}

/// The tick's decision for `row`. "Stopped" is never read from a cache that has no answer yet
/// (a service start, adb foreign or failing, a failing `simctl list`; P3bT7-1): an emulator
/// stopped only when its process is gone, a simulator only when a good list says so after the
/// boot's own window.
fn step(
    row: &BootRow,
    seen: &Seen<'_>,
    alive: impl Fn(u32, u64) -> bool,
    now: i64,
    grace_ms: i64,
    boot_window_ms: i64,
) -> Step {
    let Some(identity) = row.identity() else {
        return Step::Drop;
    };
    match &identity {
        BootIdentity::Emulator {
            pid,
            start_time,
            serial,
            not,
            ..
        } => {
            if !alive(*pid, *start_time) {
                return Step::Drop;
            }
            let info = seen.info.filter(|d| running(d));
            match serial {
                None => {
                    let learnt = info.and_then(|d| learnable(d, not));
                    return match learnt {
                        Some(s) => {
                            let mut identity = identity.clone();
                            if let BootIdentity::Emulator { serial, .. } = &mut identity {
                                *serial = Some(s);
                            }
                            Step::Learn(identity)
                        }
                        None => Step::Keep,
                    };
                }
                // Not visible as exactly our transport now (adb foreign or failing, a second
                // instance beside it): nothing to decide.
                Some(s) => {
                    let only_ours = info
                        .is_some_and(|d| matches!(d.transports.as_slice(), [t] if t.address == *s));
                    if !only_ours {
                        return Step::Keep;
                    }
                }
            }
        }
        BootIdentity::Simulator { .. } => match seen.simulator_booted {
            None => return Step::Keep,
            Some(false) if now - row.booted_at_ms < boot_window_ms => return Step::Keep,
            Some(false) => return Step::Drop,
            Some(true) => {}
        },
    }
    if seen.busy {
        return if row.idle_since_ms.is_some() {
            Step::ClearIdle
        } else {
            Step::Keep
        };
    }
    match row.idle_since_ms {
        None => Step::SetIdle(now),
        Some(since) if now - since >= grace_ms => Step::ShutDown,
        Some(_) => Step::Keep,
    }
}

/// The shutdown rule, every claims tick; synchronous, it spawns its work (store calls on the
/// blocking pool, tool calls on the Long lane).
pub fn tick(svc: &Arc<DevicesService>, claims: &ClaimsView, now_ms: i64) {
    tick_with(Real(svc.clone()), claims, now_ms);
}

fn tick_with<H: Host>(h: H, claims: &ClaimsView, now_ms: i64) {
    let ticking = h.boots().ticking.clone();
    if ticking.swap(true, Ordering::SeqCst) {
        return;
    }
    let claims = claims.clone();
    tokio::task::spawn_blocking(move || {
        tick_once(&h, &claims, now_ms);
        ticking.store(false, Ordering::SeqCst);
    });
}

fn tick_once<H: Host>(h: &H, claims: &ClaimsView, now: i64) {
    let rows = h.rows();
    let recorded: HashSet<&ResourceId> = rows.iter().map(|r| &r.resource).collect();
    // A shut-down device keeps its mark until discovery no longer sees it running.
    let gone: Vec<ResourceId> = lock(&h.boots().stopping)
        .iter()
        .filter(|(r, at)| now - **at >= GONE_WITHIN_MS || !h.device(r).is_some_and(|d| running(&d)))
        .map(|(r, _)| r.clone())
        .collect();
    for r in gone {
        lock(&h.boots().stopping).remove(&r);
        if !recorded.contains(&r) {
            h.mark_shutting_down(&r, false);
        }
    }
    let boot_window_ms = h.boot_timeout().as_millis() as i64;
    for row in &rows {
        let r = &row.resource;
        if h.boots().booting(r) || lock(&h.boots().shutting).contains(r) {
            continue;
        }
        let info = h.device(r);
        // After a service start the cache knows nothing of the record yet.
        if info.as_ref().is_some_and(|d| !d.booted_by_hostler) {
            h.mark_booted(r, true);
        }
        let simulator_booted = match row.identity() {
            Some(BootIdentity::Simulator { udid, .. }) => h.simulator_booted(&udid),
            _ => None,
        };
        let seen = Seen {
            info: info.as_ref(),
            // Sessions not known (unreadable, or not read since the start) count as a session:
            // an empty cache then says nothing (CRB-4, L63).
            busy: claims.on_device.get(r).is_some_and(|c| !c.is_empty())
                || info.as_ref().is_some_and(|d| !d.sessions.is_empty())
                || !h.sessions_known(),
            simulator_booted,
        };
        match step(
            row,
            &seen,
            |p, s| h.alive(p, s),
            now,
            h.grace_ms(),
            boot_window_ms,
        ) {
            Step::Keep => {}
            Step::Drop => forget(h, r),
            Step::Learn(identity) => h.put_row(&row.with_identity(&identity)),
            Step::SetIdle(at) => h.set_idle(r, Some(at)),
            Step::ClearIdle => h.set_idle(r, None),
            Step::ShutDown => {
                let retry = lock(&h.boots().failed).get(r).copied();
                if retry.is_some_and(|(_, last)| now - last < RETRY_EVERY_MS) {
                    continue;
                }
                lock(&h.boots().shutting).insert(r.clone());
                let (h, row) = (h.clone(), row.clone());
                tokio::spawn(async move {
                    shut_down(&h, &row).await;
                    lock(&h.boots().shutting).remove(&row.resource);
                });
            }
        }
    }
}

fn forget<H: Host>(h: &H, r: &ResourceId) {
    h.drop_row(r);
    h.mark_booted(r, false);
    lock(&h.boots().failed).remove(r);
}

/// The outcome of one shutdown attempt, for the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shutdown {
    /// A holder, a waiting Exact request, a session (or no engine answer): the device stays.
    InUse,
    /// The identity no longer matches: the record went, nothing was shut down.
    Mismatch,
    Done,
    Failed,
}

/// P-6 / P2-14: mark, ask the engine, then verify and shut down in one Long-lane call.
async fn shut_down<H: Host>(h: &H, row: &BootRow) -> Shutdown {
    let r = &row.resource;
    let Some(identity) = row.identity() else {
        forget(h, r);
        return Shutdown::Mismatch;
    };
    // 1. Every later acquire and tick sees the device unavailable.
    h.mark_shutting_down(r, true);
    // 2. Whatever was granted or queued before the mark is in the engine's answer.
    let free = h.in_use(r).await.is_some_and(|u| u.is_free());
    if !free {
        h.mark_shutting_down(r, false);
        h.set_idle(r, None);
        return Shutdown::InUse;
    }
    // 3–4. Identity, sessions and the one state change, with no lane wait between them.
    let now_stopped = h.verify_and_stop(r, identity).await;
    let now = h.now();
    match now_stopped {
        Stopped::Busy => {
            h.mark_shutting_down(r, false);
            h.set_idle(r, None);
            return Shutdown::InUse;
        }
        Stopped::Mismatch => {
            forget(h, r);
            h.mark_shutting_down(r, false);
            tracing::info!(resource = %r.0, "a device hostler booted changed; not shutting it down");
            return Shutdown::Mismatch;
        }
        Stopped::Done => {
            forget(h, r);
            lock(&h.boots().stopping).insert(r.clone(), now);
            tracing::info!(resource = %r.0, "shut down a device hostler booted");
            return Shutdown::Done;
        }
        Stopped::Failed => {}
    }
    h.mark_shutting_down(r, false);
    let first = {
        let mut failed = lock(&h.boots().failed);
        let entry = failed.entry(r.clone()).or_insert((now, now));
        entry.1 = now;
        entry.0
    };
    if now - first >= RETRY_FOR_MS {
        tracing::error!(
            resource = %r.0,
            "cannot shut down a device hostler booted after 10 minutes of tries; leaving it running"
        );
        forget(h, r);
    } else {
        tracing::warn!(resource = %r.0, "shutting down a device hostler booted failed; retrying");
    }
    Shutdown::Failed
}

/// Steps 3 and 4 of a shutdown, inside one lane call: the recorded process and AVD (or
/// `lastBootedAt`), then, right before the kill, the device read again: sessions known and none
/// on it, and still exactly our transport (P3bT7-6, L63).
pub fn verify_and_stop(t: &impl Tools, r: &ResourceId, identity: &BootIdentity) -> Stopped {
    let quiet = |transport: &str| {
        t.sessions_known()
            && t.device(r).is_some_and(|d| {
                d.sessions.is_empty()
                    && matches!(d.transports.as_slice(), [only] if only.address == transport)
            })
    };
    match identity {
        BootIdentity::Emulator {
            avd,
            pid,
            start_time,
            serial: Some(serial),
            ..
        } => {
            if !t.alive(*pid, *start_time)
                || t.emulator_avd(serial).as_deref() != Some(avd.as_str())
            {
                return Stopped::Mismatch;
            }
            if !quiet(serial) {
                return Stopped::Busy;
            }
            if t.emu_kill(serial) {
                Stopped::Done
            } else {
                Stopped::Failed
            }
        }
        BootIdentity::Emulator { serial: None, .. } => Stopped::Busy,
        BootIdentity::Simulator {
            udid,
            last_booted_at,
        } => {
            if last_booted_at.is_none() || t.last_booted_at(udid) != *last_booted_at {
                return Stopped::Mismatch;
            }
            if !quiet(udid) {
                return Stopped::Busy;
            }
            if t.simctl_shutdown(udid) {
                Stopped::Done
            } else {
                Stopped::Failed
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// L49: the end of a device hold with no CLI to print it

/// L49, P2-15: a line for every live session on the device that belongs to this workspace or
/// was first seen inside the claim's `window` (an agent that `cd`'d elsewhere), with the exact
/// `agent-device close` command.
pub fn session_warnings(
    device: &DeviceInfo,
    workspace_id: &str,
    window: (i64, i64),
) -> Vec<String> {
    let alias = device
        .alias
        .clone()
        .unwrap_or_else(|| device.resource.0.clone());
    device
        .sessions
        .iter()
        .filter(|s| {
            s.workspace_id.as_deref() == Some(workspace_id)
                || (window.0..=window.1).contains(&s.seen_since_ms)
        })
        .map(|s| {
            let (platform, flag) = match s.platform {
                Platform::Android => ("android", "--serial"),
                Platform::Ios => ("ios", "--udid"),
            };
            sanitize_for_terminal(&format!(
                "[hostler] agent-device session {} is still open on {alias}: it blocks other \
                 workspaces until closed: agent-device close --session {} --platform {platform} \
                 {flag} {}",
                s.session, s.session, s.agent_device_id
            ))
        })
        .collect()
}

/// Every `ClaimsChanged`: a device hold that left the view because it expired or was reaped
/// gets its open-session lines in the log and in its history row's `detail` (P3bT7-7).
pub fn claims_ended(svc: &DevicesService, before: &ClaimsView, now: &ClaimsView) {
    for (r, claims) in &before.on_device {
        for c in claims.iter().filter(|c| c.kind == ClaimKind::Hold) {
            let still = now
                .on_device
                .get(r)
                .is_some_and(|l| l.iter().any(|n| n.id == c.id));
            if still {
                continue;
            }
            // Never the store and the cache guard at once.
            let Ok(Some(item)) = svc.store().history_item_of(&c.id) else {
                continue;
            };
            if !matches!(item.reason, EndReason::Expired | EndReason::Reaped) {
                continue;
            }
            let Some(info) = svc.cache().info(r) else {
                continue;
            };
            let workspace_id = svc
                .store()
                .workspaces()
                .ok()
                .and_then(|w| w.into_iter().find(|w| w.path == item.workspace_path))
                .map(|w| w.id)
                .unwrap_or_default();
            let window = (item.granted_at_ms.unwrap_or(c.since_ms), item.ended_at_ms);
            let lines = session_warnings(&info, &workspace_id, window);
            if lines.is_empty() {
                continue;
            }
            for line in &lines {
                tracing::warn!(claim = %c.id, "{line}");
            }
            let detail = lines
                .iter()
                .map(|l| l.trim_start_matches("[hostler] "))
                .collect::<Vec<_>>()
                .join("; ");
            if let Err(e) = svc.store().set_claim_history_detail(&c.id, &detail) {
                tracing::warn!("cannot record the open sessions of an ended hold: {e:#}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The service as the host

#[derive(Clone)]
struct Real(Arc<DevicesService>);

fn ok(out: &ToolOutput) -> bool {
    out.status == Some(0) && !out.timed_out && !out.truncated
}

impl Real {
    /// One call on the Long lane (one per device at a time).
    async fn long<T: Send + 'static>(
        &self,
        r: &ResourceId,
        f: impl FnOnce(&DevicesService) -> T + Send + 'static,
    ) -> T {
        let svc = self.0.clone();
        self.0
            .gate()
            .run(Lane::Long, Some(r), move || f(&svc))
            .await
    }
}

fn os(args: &[&str]) -> Vec<std::ffi::OsString> {
    args.iter().map(Into::into).collect()
}

fn call(svc: &DevicesService, tool: Tool, args: &[&str], bound: Duration) -> Option<ToolOutput> {
    let path = match tool {
        Tool::Adb => svc.tools().adb?,
        Tool::Xcrun => svc.tools().xcrun?,
        _ => return None,
    };
    let args = os(args);
    let args: Vec<&OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    svc.call(tool, &path, &args, bound)
}

fn alive(pid: u32, start_time: u64) -> bool {
    SysProcessTable::new()
        .ok()
        .and_then(|t| t.info(pid))
        .and_then(|i| i.start_time)
        == Some(start_time)
}

fn simctl_last_booted_at(svc: &DevicesService, udid: &str, bound: Duration) -> Option<String> {
    let out = call(
        svc,
        Tool::Xcrun,
        &["simctl", "list", "devices", "--json"],
        bound,
    )
    .filter(ok)?;
    super::ios::parse_simctl_list(&out.stdout)?
        .into_iter()
        .find(|s| s.udid == udid)?
        .last_booted_at
}

/// The service's tools, inside one Long-lane call.
struct SvcTools<'a>(&'a DevicesService);

impl Tools for SvcTools<'_> {
    fn device(&self, r: &ResourceId) -> Option<DeviceInfo> {
        self.0.cache().info(r)
    }

    fn sessions_known(&self) -> bool {
        self.0.sessions_unknown().is_none()
    }

    fn alive(&self, pid: u32, start_time: u64) -> bool {
        alive(pid, start_time)
    }

    fn emulator_avd(&self, serial: &str) -> Option<String> {
        let args = ["-s", serial, "emu", "avd", "name"];
        let out = call(self.0, Tool::Adb, &args, ADB_EMU_BOUND).filter(ok)?;
        super::android::parse_emu_avd_name(&String::from_utf8_lossy(&out.stdout))
    }

    fn emu_kill(&self, serial: &str) -> bool {
        call(
            self.0,
            Tool::Adb,
            &["-s", serial, "emu", "kill"],
            ADB_EMU_BOUND,
        )
        .is_some_and(|o| ok(&o))
    }

    fn last_booted_at(&self, udid: &str) -> Option<String> {
        simctl_last_booted_at(self.0, udid, SIMCTL_LIST_BOUND)
    }

    fn simctl_shutdown(&self, udid: &str) -> bool {
        let args = ["simctl", "shutdown", udid];
        call(self.0, Tool::Xcrun, &args, SIMCTL_SHUTDOWN_BOUND).is_some_and(|o| ok(&o))
    }
}

impl Host for Real {
    fn now(&self) -> i64 {
        self.0.now()
    }

    fn boots(&self) -> &Boots {
        self.0.boots()
    }

    fn device(&self, r: &ResourceId) -> Option<DeviceInfo> {
        self.0.cache().info(r)
    }

    fn rows(&self) -> Vec<BootRow> {
        self.0.store().device_boots().unwrap_or_else(|e| {
            tracing::warn!("cannot read device_boots: {e:#}");
            Vec::new()
        })
    }

    fn put_row(&self, row: &BootRow) {
        if let Err(e) = self.0.store().put_device_boot(row) {
            tracing::warn!("cannot record a boot: {e:#}");
        }
    }

    fn set_idle(&self, r: &ResourceId, since: Option<i64>) {
        if let Err(e) = self.0.store().set_device_boot_idle(r, since) {
            tracing::warn!("cannot record a boot's idle time: {e:#}");
        }
    }

    fn drop_row(&self, r: &ResourceId) {
        if let Err(e) = self.0.store().delete_device_boot(r) {
            tracing::warn!("cannot drop a boot record: {e:#}");
        }
    }

    fn mark_shutting_down(&self, r: &ResourceId, on: bool) {
        self.0.cache().set_shutting_down(r, on);
        self.0.publish();
    }

    fn mark_booted(&self, r: &ResourceId, on: bool) {
        self.0.cache().set_booted_by_hostler(r, on);
        self.0.publish();
    }

    fn booted_ready(&self, _r: &ResourceId) {
        self.0.wake_snapshots();
    }

    fn alive(&self, pid: u32, start_time: u64) -> bool {
        alive(pid, start_time)
    }

    fn sessions_known(&self) -> bool {
        self.0.sessions_unknown().is_none()
    }

    fn emulator_serials(&self) -> Vec<String> {
        self.0.cache().emulator_serials()
    }

    fn simulator_booted(&self, udid: &str) -> Option<bool> {
        self.0.cache().simulator_booted(udid)
    }

    fn boot_timeout(&self) -> Duration {
        test_ms(TEST_BOOT_TIMEOUT_ENV).map_or(BOOT_TIMEOUT, Duration::from_millis)
    }

    fn grace_ms(&self) -> i64 {
        test_ms(TEST_SHUTDOWN_GRACE_ENV).map_or(SHUTDOWN_GRACE_MS, |ms| ms as i64)
    }

    fn log_path(&self, avd: &str) -> PathBuf {
        let dir = crate::paths::Paths::resolve()
            .map(|p| p.log_dir)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("devices");
        dir.join(format!("emulator-{avd}.log"))
    }

    async fn spawn_emulator(
        &self,
        r: &ResourceId,
        avd: &str,
        log: PathBuf,
        deadline: Instant,
    ) -> Result<(u32, u64), String> {
        let avd = avd.to_owned();
        self.long(r, move |svc| {
            // Checked inside the lane: what the lane wait ate is not spent twice (P3bT7-5).
            if within(deadline, BOOT_TIMEOUT).is_none() {
                return Err("the boot did not start within its bound".to_owned());
            }
            let emulator = svc
                .tools()
                .emulator
                .ok_or_else(|| "the Android emulator was not found".to_owned())?;
            if let Some(dir) = log.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let env: Vec<(String, std::ffi::OsString)> = svc
                .settings()
                .env
                .into_iter()
                .filter(|(k, _)| tools::env_allowed(Tool::Emulator, k))
                .map(|(k, v)| (k, v.into()))
                .collect();
            let env: Vec<(&str, &OsStr)> = env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_os_str()))
                .collect();
            let args = os(&[
                "-avd",
                &avd,
                "-no-window",
                "-no-audio",
                "-no-boot-anim",
                "-no-snapshot-save",
            ]);
            let args: Vec<&OsStr> = args.iter().map(|a| a.as_os_str()).collect();
            tools::spawn_detached(&emulator, &args, &env, &log)
                .map_err(|e| format!("the emulator did not start: {e}"))
        })
        .await
    }

    async fn simctl_boot(&self, r: &ResourceId, udid: &str, deadline: Instant) -> SimBoot {
        let udid = udid.to_owned();
        self.long(r, move |svc| {
            let Some(bound) = within(deadline, SIMCTL_BOOT_BOUND) else {
                return SimBoot::Failed("the boot did not start within its bound".into());
            };
            let Some(out) = call(svc, Tool::Xcrun, &["simctl", "boot", &udid], bound) else {
                return SimBoot::Failed("simctl could not run".into());
            };
            if ok(&out) {
                return SimBoot::Booted;
            }
            let text = String::from_utf8_lossy(&out.stderr);
            if text.contains("current state: Booted") {
                SimBoot::AlreadyBooted
            } else if out.timed_out {
                SimBoot::Failed("simctl boot timed out".into())
            } else {
                SimBoot::Failed(format!("simctl boot exited with {:?}", out.status))
            }
        })
        .await
    }

    async fn bootstatus(
        &self,
        r: &ResourceId,
        udid: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        let udid = udid.to_owned();
        self.long(r, move |svc| {
            let bound = within(deadline, BOOT_TIMEOUT).ok_or("not booted within the bound")?;
            match call(
                svc,
                Tool::Xcrun,
                &["simctl", "bootstatus", &udid, "-b"],
                bound,
            ) {
                Some(out) if ok(&out) => Ok(()),
                Some(out) if out.timed_out => Err("not booted within the bound".into()),
                Some(out) => Err(format!("simctl bootstatus exited with {:?}", out.status)),
                None => Err("simctl could not run".into()),
            }
        })
        .await
    }

    async fn last_booted_at(
        &self,
        r: &ResourceId,
        udid: &str,
        deadline: Instant,
    ) -> Option<String> {
        let udid = udid.to_owned();
        self.long(r, move |svc| {
            let bound = within(deadline, SIMCTL_LIST_BOUND)?;
            simctl_last_booted_at(svc, &udid, bound)
        })
        .await
    }

    async fn verify_and_stop(&self, r: &ResourceId, identity: BootIdentity) -> Stopped {
        let r2 = r.clone();
        self.long(r, move |svc| {
            verify_and_stop(&SvcTools(svc), &r2, &identity)
        })
        .await
    }

    async fn in_use(&self, r: &ResourceId) -> Option<InUse> {
        let check = self.0.boots().in_use.get()?;
        check(r.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicI64;

    use super::*;
    use crate::contract::claims::fixtures as claim_fixtures;
    use crate::contract::claims::{ResourceKind, ResourceState};
    use crate::contract::devices::{AgentDeviceSession, Transport, TransportKind, fixtures};

    #[derive(Default)]
    struct State {
        boots: Boots,
        now: AtomicI64,
        devices: Mutex<HashMap<ResourceId, DeviceInfo>>,
        rows: Mutex<BTreeMap<ResourceId, BootRow>>,
        calls: Mutex<Vec<String>>,
        alive: Mutex<HashSet<(u32, u64)>>,
        in_use: Mutex<Option<InUse>>,
        /// Whether the device was marked shutting down when the engine was asked.
        marked_at_ask: Mutex<Option<bool>>,
        marked: Mutex<HashSet<ResourceId>>,
        booted: Mutex<HashSet<ResourceId>>,
        last_booted_at: Mutex<Option<String>>,
        emulator_avd: Mutex<Option<String>>,
        sim_boot: Mutex<Option<SimBoot>>,
        sim_listed: Mutex<Option<bool>>,
        serials: Mutex<Vec<String>>,
        /// What the device looks like once a boot call ran (`None`: it never comes up).
        after_boot: Mutex<Option<DeviceInfo>>,
        kill_fails: AtomicBool,
        bootstatus_fails: AtomicBool,
        spawn_panics: AtomicBool,
        /// `emu avd name` opens an agent-device session (the window before the kill).
        session_during_check: AtomicBool,
        /// Agent-device's status cannot be read (or was not read yet).
        sessions_unknown: AtomicBool,
        /// `emu avd name` makes the status unreadable (the window before the kill).
        unknown_during_check: AtomicBool,
    }

    #[derive(Clone, Default)]
    struct Fake(Arc<State>);

    impl Fake {
        fn call(&self, what: String) {
            lock(&self.0.calls).push(what);
        }

        fn calls(&self) -> Vec<String> {
            lock(&self.0.calls).clone()
        }

        fn booted(&self, r: &ResourceId) {
            if let Some(d) = lock(&self.0.after_boot).clone() {
                lock(&self.0.devices).insert(r.clone(), d);
            }
        }
    }

    fn session() -> AgentDeviceSession {
        fixtures::info().sessions[0].clone()
    }

    impl Tools for Fake {
        fn device(&self, r: &ResourceId) -> Option<DeviceInfo> {
            lock(&self.0.devices).get(r).cloned()
        }
        fn sessions_known(&self) -> bool {
            !self.0.sessions_unknown.load(Ordering::SeqCst)
        }
        fn alive(&self, pid: u32, start_time: u64) -> bool {
            lock(&self.0.alive).contains(&(pid, start_time))
        }
        fn emulator_avd(&self, _serial: &str) -> Option<String> {
            if self.0.unknown_during_check.load(Ordering::SeqCst) {
                self.0.sessions_unknown.store(true, Ordering::SeqCst);
            }
            if self.0.session_during_check.load(Ordering::SeqCst)
                && let Some(d) = lock(&self.0.devices).get_mut(&avd())
            {
                d.sessions.push(session());
            }
            lock(&self.0.emulator_avd).clone()
        }
        fn emu_kill(&self, serial: &str) -> bool {
            self.call(format!("emu kill {serial}"));
            !self.0.kill_fails.load(Ordering::SeqCst)
        }
        fn last_booted_at(&self, _udid: &str) -> Option<String> {
            lock(&self.0.last_booted_at).clone()
        }
        fn simctl_shutdown(&self, udid: &str) -> bool {
            self.call(format!("simctl shutdown {udid}"));
            true
        }
    }

    impl Host for Fake {
        fn now(&self) -> i64 {
            self.0.now.load(Ordering::SeqCst)
        }
        fn boots(&self) -> &Boots {
            &self.0.boots
        }
        fn device(&self, r: &ResourceId) -> Option<DeviceInfo> {
            lock(&self.0.devices).get(r).cloned()
        }
        fn rows(&self) -> Vec<BootRow> {
            lock(&self.0.rows).values().cloned().collect()
        }
        fn put_row(&self, row: &BootRow) {
            lock(&self.0.rows).insert(row.resource.clone(), row.clone());
        }
        fn set_idle(&self, r: &ResourceId, since: Option<i64>) {
            if let Some(row) = lock(&self.0.rows).get_mut(r) {
                row.idle_since_ms = since;
            }
        }
        fn drop_row(&self, r: &ResourceId) {
            lock(&self.0.rows).remove(r);
        }
        fn mark_shutting_down(&self, r: &ResourceId, on: bool) {
            if on {
                lock(&self.0.marked).insert(r.clone());
            } else {
                lock(&self.0.marked).remove(r);
            }
        }
        fn mark_booted(&self, r: &ResourceId, on: bool) {
            if on {
                lock(&self.0.booted).insert(r.clone());
            } else {
                lock(&self.0.booted).remove(r);
            }
        }
        fn alive(&self, pid: u32, start_time: u64) -> bool {
            lock(&self.0.alive).contains(&(pid, start_time))
        }
        fn sessions_known(&self) -> bool {
            !self.0.sessions_unknown.load(Ordering::SeqCst)
        }
        fn emulator_serials(&self) -> Vec<String> {
            lock(&self.0.serials).clone()
        }
        fn simulator_booted(&self, _udid: &str) -> Option<bool> {
            *lock(&self.0.sim_listed)
        }
        fn boot_timeout(&self) -> Duration {
            Duration::from_millis(600)
        }
        fn grace_ms(&self) -> i64 {
            SHUTDOWN_GRACE_MS
        }
        fn log_path(&self, avd: &str) -> PathBuf {
            PathBuf::from(format!("/logs/devices/emulator-{avd}.log"))
        }
        async fn spawn_emulator(
            &self,
            r: &ResourceId,
            avd: &str,
            _log: PathBuf,
            _deadline: Instant,
        ) -> Result<(u32, u64), String> {
            self.call(format!("emulator -avd {avd}"));
            assert!(
                !self.0.spawn_panics.load(Ordering::SeqCst),
                "a boot that panics"
            );
            // A second caller would have had time to start a second boot.
            tokio::time::sleep(Duration::from_millis(50)).await;
            lock(&self.0.alive).insert((4242, 77));
            self.booted(r);
            Ok((4242, 77))
        }
        async fn simctl_boot(&self, r: &ResourceId, udid: &str, _deadline: Instant) -> SimBoot {
            self.call(format!("simctl boot {udid}"));
            let answer = lock(&self.0.sim_boot).clone().unwrap_or(SimBoot::Booted);
            if answer == SimBoot::Booted {
                self.booted(r);
            }
            answer
        }
        async fn bootstatus(
            &self,
            _r: &ResourceId,
            udid: &str,
            _deadline: Instant,
        ) -> Result<(), String> {
            self.call(format!("simctl bootstatus {udid}"));
            if self.0.bootstatus_fails.load(Ordering::SeqCst) {
                return Err("not booted within the bound".into());
            }
            Ok(())
        }
        async fn last_booted_at(
            &self,
            _r: &ResourceId,
            _udid: &str,
            _deadline: Instant,
        ) -> Option<String> {
            lock(&self.0.last_booted_at).clone()
        }
        async fn verify_and_stop(&self, r: &ResourceId, identity: BootIdentity) -> Stopped {
            verify_and_stop(self, r, &identity)
        }
        async fn in_use(&self, r: &ResourceId) -> Option<InUse> {
            *lock(&self.0.marked_at_ask) = Some(lock(&self.0.marked).contains(r));
            lock(&self.0.in_use).clone()
        }
    }

    const UDID: &str = "00000000-0000-4000-8000-000000000001";

    fn avd() -> ResourceId {
        ResourceId("device:android:avd:Pixel_9".into())
    }

    fn sim() -> ResourceId {
        ResourceId(format!("device:ios:{UDID}"))
    }

    fn transport(serial: &str) -> Transport {
        Transport {
            kind: TransportKind::Emulator,
            address: serial.into(),
            state: "device".into(),
        }
    }

    /// The AVD, shut down (`running`: as `emulator-5554`, booted).
    fn emulator(running: bool) -> DeviceInfo {
        DeviceInfo {
            resource: avd(),
            hardware_id: "Pixel_9".into(),
            kind: DeviceKind::Emulator,
            alias: Some("pixel_9".into()),
            ready: running,
            connected: true,
            transports: if running {
                vec![transport("emulator-5554")]
            } else {
                Vec::new()
            },
            sessions: Vec::new(),
            ..fixtures::info()
        }
    }

    fn simulator(booted: bool) -> DeviceInfo {
        DeviceInfo {
            resource: sim(),
            hardware_id: UDID.into(),
            platform: Platform::Ios,
            kind: DeviceKind::Simulator,
            alias: Some("iphone-17".into()),
            ready: booted,
            transports: vec![Transport {
                kind: TransportKind::Simulator,
                address: UDID.into(),
                state: if booted { "booted" } else { "shutdown" }.into(),
            }],
            sessions: Vec::new(),
            ..fixtures::info()
        }
    }

    fn with(device: DeviceInfo) -> Fake {
        let fake = Fake::default();
        fake.0.now.store(1_000_000, Ordering::SeqCst);
        lock(&fake.0.devices).insert(device.resource.clone(), device);
        fake
    }

    fn emulator_row(serial: Option<&str>, idle: Option<i64>) -> BootRow {
        BootRow {
            resource: avd(),
            booted_at_ms: 0,
            identity_json: String::new(),
            claim_id: "c1".into(),
            idle_since_ms: idle,
        }
        .with_identity(&BootIdentity::Emulator {
            avd: "Pixel_9".into(),
            pid: 4242,
            start_time: 77,
            serial: serial.map(Into::into),
            not: Vec::new(),
        })
    }

    fn simulator_row(last: &str) -> BootRow {
        BootRow {
            resource: sim(),
            booted_at_ms: 0,
            identity_json: String::new(),
            claim_id: "c1".into(),
            idle_since_ms: Some(0),
        }
        .with_identity(&BootIdentity::Simulator {
            udid: UDID.into(),
            last_booted_at: Some(last.into()),
        })
    }

    /// A running emulator hostler booted, idle past its grace.
    fn idle_emulator() -> (Fake, BootRow) {
        let fake = with(emulator(true));
        lock(&fake.0.alive).insert((4242, 77));
        *lock(&fake.0.emulator_avd) = Some("Pixel_9".into());
        *lock(&fake.0.in_use) = Some(InUse::default());
        let row = emulator_row(Some("emulator-5554"), Some(0));
        fake.put_row(&row);
        fake.mark_booted(&avd(), true);
        (fake, row)
    }

    fn seen(info: Option<&DeviceInfo>, busy: bool, simulator_booted: Option<bool>) -> Seen<'_> {
        Seen {
            info,
            busy,
            simulator_booted,
        }
    }

    const ALIVE: fn(u32, u64) -> bool = |_, _| true;
    const GONE: fn(u32, u64) -> bool = |_, _| false;

    #[tokio::test]
    async fn boot_is_idempotent_per_resource() {
        let fake = with(emulator(false));
        *lock(&fake.0.after_boot) = Some(emulator(true));
        let r = avd();
        let (a, b) = tokio::join!(
            boot_with(fake.clone(), &r, "c1"),
            boot_with(fake.clone(), &r, "c2")
        );
        assert!(a.unwrap().ready && b.unwrap().ready);
        assert_eq!(fake.calls(), ["emulator -avd Pixel_9"]);
        let rows = fake.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].claim_id, "c1");
        // The instance it came up as is known at once.
        assert!(
            matches!(rows[0].identity(), Some(BootIdentity::Emulator { serial: Some(s), .. }) if s == "emulator-5554")
        );
        assert!(lock(&fake.0.booted).contains(&avd()));
        // Done: the next boot is a new one (and finds it booted).
        assert!(!fake.boots().booting(&avd()));
    }

    #[tokio::test]
    async fn a_panicking_boot_never_blocks_the_next() {
        let fake = with(emulator(false));
        fake.0.spawn_panics.store(true, Ordering::SeqCst);
        let e = boot_with(fake.clone(), &avd(), "c1").await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal);
        assert!(!fake.boots().booting(&avd()), "the in-flight entry is gone");
        fake.0.spawn_panics.store(false, Ordering::SeqCst);
        *lock(&fake.0.after_boot) = Some(emulator(true));
        assert!(boot_with(fake.clone(), &avd(), "c2").await.unwrap().ready);
    }

    #[tokio::test]
    async fn already_booted_is_not_recorded() {
        let fake = with(emulator(true));
        assert!(boot_with(fake.clone(), &avd(), "c1").await.unwrap().ready);
        assert!(fake.calls().is_empty() && fake.rows().is_empty());
        // A simulator someone else booted meanwhile: "already booted" is not ours.
        let fake = with(simulator(false));
        *lock(&fake.0.sim_boot) = Some(SimBoot::AlreadyBooted);
        lock(&fake.0.devices).insert(sim(), simulator(true));
        boot_with(fake.clone(), &sim(), "c1").await.unwrap();
        assert!(fake.rows().is_empty());
        // An emulator another process is booting: waited for, not started, not recorded.
        let fake = with(DeviceInfo {
            ready: false,
            ..emulator(true)
        });
        let e = boot_with(fake.clone(), &avd(), "c1").await.unwrap_err();
        assert_eq!(e.code, ErrorCode::BootFailed);
        assert!(fake.calls().is_empty() && fake.rows().is_empty());
    }

    #[tokio::test]
    async fn boot_times_out_and_is_still_cleaned_up() {
        let fake = with(emulator(false));
        let started = Instant::now();
        let e = boot_with(fake.clone(), &avd(), "c1").await.unwrap_err();
        // One deadline for the whole boot (P3bT7-5).
        assert!(
            started.elapsed() < Duration::from_millis(600 + 400),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(e.code, ErrorCode::BootFailed);
        assert_eq!(
            e.recovery.as_deref(),
            Some(
                "hostler devices show pixel_9; emulator output: \
                 /logs/devices/emulator-Pixel_9.log"
            )
        );
        // Recorded: the shutdown rule finds it once it comes up, or drops it once it is gone.
        let rows = fake.rows();
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0].identity(),
            Some(BootIdentity::Emulator {
                pid: 4242,
                start_time: 77,
                serial: None,
                ..
            })
        ));
        let view = ClaimsView::default();
        tick_once(&fake, &view, fake.now());
        assert_eq!(fake.rows().len(), 1, "kept while the process lives");
        lock(&fake.0.devices).insert(avd(), emulator(true));
        tick_once(&fake, &view, fake.now());
        let learnt = fake.rows()[0].identity();
        assert!(
            matches!(&learnt, Some(BootIdentity::Emulator { serial: Some(s), .. }) if s == "emulator-5554"),
            "{learnt:?}"
        );
    }

    #[tokio::test]
    async fn failed_bootstatus_is_still_cleaned_up() {
        let fake = with(simulator(false));
        fake.0.bootstatus_fails.store(true, Ordering::SeqCst);
        *lock(&fake.0.last_booted_at) = Some("t1".into());
        let e = boot_with(fake.clone(), &sim(), "c1").await.unwrap_err();
        assert_eq!(e.code, ErrorCode::BootFailed);
        let rows = fake.rows();
        assert_eq!(rows.len(), 1, "booted by us: recorded");
        // Still booting in the list (not booted) within the boot window: kept.
        *lock(&fake.0.sim_listed) = Some(false);
        tick_once(&fake, &ClaimsView::default(), rows[0].booted_at_ms + 100);
        assert_eq!(fake.rows().len(), 1);
        // Booted later: it idles and is shut down like any.
        *lock(&fake.0.sim_listed) = Some(true);
        lock(&fake.0.devices).insert(sim(), simulator(true));
        tick_once(&fake, &ClaimsView::default(), rows[0].booted_at_ms + 200);
        assert!(fake.rows()[0].idle_since_ms.is_some());
    }

    #[test]
    fn shutdown_waits_for_the_grace_step() {
        let info = emulator(true);
        let row = emulator_row(Some("emulator-5554"), None);
        let s = seen(Some(&info), false, None);
        assert_eq!(step(&row, &s, ALIVE, 100, 120_000, 0), Step::SetIdle(100));
        let row = emulator_row(Some("emulator-5554"), Some(100));
        assert_eq!(step(&row, &s, ALIVE, 120_099, 120_000, 0), Step::Keep);
        assert_eq!(step(&row, &s, ALIVE, 120_100, 120_000, 0), Step::ShutDown);
        // A claim or a session on it: the idle time starts again.
        let busy = seen(Some(&info), true, None);
        assert_eq!(
            step(&row, &busy, ALIVE, 999_999, 120_000, 0),
            Step::ClearIdle
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_grace_and_marks_first() {
        let (fake, row) = idle_emulator();
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Done);
        assert_eq!(
            *lock(&fake.0.marked_at_ask),
            Some(true),
            "marked before the engine"
        );
        assert_eq!(fake.calls(), ["emu kill emulator-5554"]);
        assert!(fake.rows().is_empty() && !lock(&fake.0.booted).contains(&avd()));
        // The mark stays until discovery no longer sees it running.
        assert!(lock(&fake.0.marked).contains(&avd()));
        tick_once(&fake, &ClaimsView::default(), fake.now());
        assert!(lock(&fake.0.marked).contains(&avd()));
        lock(&fake.0.devices).insert(avd(), emulator(false));
        tick_once(&fake, &ClaimsView::default(), fake.now());
        assert!(!lock(&fake.0.marked).contains(&avd()));
    }

    #[tokio::test]
    async fn tick_starts_the_shutdown_only_after_the_grace() {
        let (fake, mut row) = idle_emulator();
        row.idle_since_ms = None;
        fake.put_row(&row);
        let view = ClaimsView::default();
        let t0 = fake.now();
        tick_once(&fake, &view, t0);
        assert_eq!(fake.rows()[0].idle_since_ms, Some(t0));
        tick_once(&fake, &view, t0 + SHUTDOWN_GRACE_MS - 1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(fake.calls().is_empty());
        tick_once(&fake, &view, t0 + SHUTDOWN_GRACE_MS);
        for _ in 0..100 {
            if !fake.calls().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(fake.calls(), ["emu kill emulator-5554"]);
        // Claims on it: no idle time at all.
        let (fake, row) = idle_emulator();
        let mut view = ClaimsView::default();
        view.on_device
            .insert(avd(), vec![claim_fixtures::summary()]);
        tick_once(
            &fake,
            &view,
            row.idle_since_ms.unwrap() + 10 * SHUTDOWN_GRACE_MS,
        );
        assert_eq!(fake.rows()[0].idle_since_ms, None);
    }

    #[tokio::test]
    async fn grant_during_shutdown_mark_cancels_it() {
        let (fake, row) = idle_emulator();
        // The engine, asked under the claims lock after the mark: a holder.
        *lock(&fake.0.in_use) = Some(InUse {
            claims: vec![claim_fixtures::summary()],
            waiting: 0,
        });
        assert_eq!(shut_down(&fake, &row).await, Shutdown::InUse);
        assert!(fake.calls().is_empty());
        assert!(!lock(&fake.0.marked).contains(&avd()), "the mark is lifted");
        assert_eq!(fake.rows()[0].idle_since_ms, None, "idle starts again");
        assert!(lock(&fake.0.booted).contains(&avd()));
        // No engine answer: kept (fail closed).
        *lock(&fake.0.in_use) = None;
        assert_eq!(shut_down(&fake, &row).await, Shutdown::InUse);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn waiting_exact_request_cancels_the_shutdown() {
        let (fake, row) = idle_emulator();
        *lock(&fake.0.in_use) = Some(InUse {
            claims: Vec::new(),
            waiting: 1,
        });
        assert_eq!(shut_down(&fake, &row).await, Shutdown::InUse);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn session_opened_during_the_shutdown_cancels_it() {
        // An agent-device session opened after the tick, while the identity is checked
        // (P3bT7-6): re-read right before the kill.
        let (fake, row) = idle_emulator();
        fake.0.session_during_check.store(true, Ordering::SeqCst);
        assert_eq!(shut_down(&fake, &row).await, Shutdown::InUse);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        assert!(!lock(&fake.0.marked).contains(&avd()));
        assert_eq!(fake.rows().len(), 1, "still hostler's");
    }

    #[tokio::test]
    async fn simulator_reboot_by_hand_changes_last_booted_at_and_is_not_shut_down() {
        let fake = with(simulator(true));
        *lock(&fake.0.in_use) = Some(InUse::default());
        let row = simulator_row("2026-11-08T10:00:00Z");
        fake.put_row(&row);
        *lock(&fake.0.last_booted_at) = Some("2026-11-08T10:05:00Z".into());
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Mismatch);
        assert!(fake.calls().is_empty() && fake.rows().is_empty());
        // Unchanged: shut down.
        fake.put_row(&row);
        *lock(&fake.0.last_booted_at) = Some("2026-11-08T10:00:00Z".into());
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Done);
        assert_eq!(fake.calls(), [format!("simctl shutdown {UDID}")]);
    }

    #[tokio::test]
    async fn boot_refuses_a_foreign_claim_id_and_physical_devices() {
        let mut claim = claim_fixtures::claim();
        claim.resource.id = avd();
        claim.resource.kind = ResourceKind::Device;
        let snapshot = ClaimsSnapshot {
            as_of_seq: 0,
            resources: vec![ResourceState {
                resource: claim.resource.clone(),
                claims: vec![claim.clone()],
                foreign: Vec::new(),
                waiting: Vec::new(),
                seizing: false,
                seizing_by: None,
            }],
        };
        assert!(check_claim(&snapshot, &avd(), "c1", "ws-1").is_ok());
        for (r, id, ws) in [
            (avd(), "c1", "ws-2"),
            (avd(), "c9", "ws-1"),
            (sim(), "c1", "ws-1"),
        ] {
            let e = check_claim(&snapshot, &r, id, ws).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest, "{r:?} {id} {ws}");
        }
        let mut reservation = snapshot.clone();
        reservation.resources[0].claims[0].kind = ClaimKind::Reservation;
        assert!(check_claim(&reservation, &avd(), "c1", "ws-1").is_err());

        let fake = with(fixtures::info());
        let e = boot_with(fake.clone(), &fixtures::resource(), "c1")
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn identity_mismatch_drops_the_record_without_shutdown() {
        // Another AVD now answers on emulator-5554.
        let (fake, row) = idle_emulator();
        *lock(&fake.0.emulator_avd) = Some("Pixel_8".into());
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Mismatch);
        assert!(fake.calls().is_empty() && fake.rows().is_empty());
        assert!(!lock(&fake.0.booted).contains(&avd()));
        assert!(!lock(&fake.0.marked).contains(&avd()));
        // Our process is gone (a new one runs the AVD).
        let (fake, row) = idle_emulator();
        lock(&fake.0.alive).clear();
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Mismatch);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn record_dropped_when_the_process_is_gone() {
        let row = emulator_row(Some("emulator-5554"), Some(0));
        let up = emulator(true);
        let down = emulator(false);
        assert_eq!(
            step(&row, &seen(Some(&up), false, None), GONE, 1, 1, 0),
            Step::Drop
        );
        assert_eq!(
            step(&row, &seen(None, false, None), GONE, 1, 1, 0),
            Step::Drop
        );
        let never = emulator_row(None, None);
        assert_eq!(
            step(&never, &seen(Some(&down), false, None), GONE, 1, 1, 0),
            Step::Drop
        );
        // A good list says the simulator is shut down, after its boot window.
        let row = simulator_row("t");
        let off = simulator(false);
        assert_eq!(
            step(
                &row,
                &seen(Some(&off), false, Some(false)),
                ALIVE,
                50,
                1,
                10
            ),
            Step::Drop
        );

        let fake = with(emulator(false));
        fake.put_row(&emulator_row(Some("emulator-5554"), None));
        fake.mark_booted(&avd(), true);
        tick_once(&fake, &ClaimsView::default(), 5);
        assert!(fake.rows().is_empty());
        assert!(!lock(&fake.0.booted).contains(&avd()));
    }

    #[test]
    fn record_kept_while_discovery_has_not_run() {
        // After a service start: the cache has no device yet, or the AVD without transports.
        let row = emulator_row(Some("emulator-5554"), Some(0));
        assert_eq!(
            step(&row, &seen(None, false, None), ALIVE, 1 << 40, 1, 0),
            Step::Keep
        );
        let down = emulator(false);
        assert_eq!(
            step(&row, &seen(Some(&down), false, None), ALIVE, 1 << 40, 1, 0),
            Step::Keep
        );
        // The tick also says again that it is hostler's.
        let fake = with(down);
        lock(&fake.0.alive).insert((4242, 77));
        fake.put_row(&row);
        tick_once(&fake, &ClaimsView::default(), 1 << 40);
        assert_eq!(fake.rows(), vec![row]);
        assert!(lock(&fake.0.booted).contains(&avd()));
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn record_kept_while_adb_is_foreign() {
        // adb foreign or failing: the cache lists no transport for a running emulator.
        let (fake, row) = idle_emulator();
        lock(&fake.0.devices).insert(avd(), emulator(false));
        for t in 0..5 {
            tick_once(
                &fake,
                &ClaimsView::default(),
                fake.now() + t * SHUTDOWN_GRACE_MS,
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(fake.rows(), vec![row], "neither dropped nor shut down");
        assert!(fake.calls().is_empty());
        // adb is back: the shutdown follows its grace.
        lock(&fake.0.devices).insert(avd(), emulator(true));
        tick_once(
            &fake,
            &ClaimsView::default(),
            fake.now() + 10 * SHUTDOWN_GRACE_MS,
        );
        for _ in 0..100 {
            if !fake.calls().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(fake.calls(), ["emu kill emulator-5554"]);
    }

    #[test]
    fn simulator_record_kept_until_a_good_list_shows_it_shut() {
        let row = simulator_row("t");
        let off = simulator(false);
        // No good `simctl list` (not yet, or failing): kept.
        assert_eq!(
            step(&row, &seen(Some(&off), false, None), ALIVE, 1 << 40, 1, 10),
            Step::Keep
        );
        assert_eq!(
            step(&row, &seen(None, false, None), ALIVE, 1 << 40, 1, 10),
            Step::Keep
        );
        // Inside the boot's window it may still be booting.
        assert_eq!(
            step(&row, &seen(Some(&off), false, Some(false)), ALIVE, 5, 1, 10),
            Step::Keep
        );
        assert_eq!(
            step(
                &row,
                &seen(Some(&off), false, Some(false)),
                ALIVE,
                1 << 40,
                1,
                10
            ),
            Step::Drop
        );
    }

    #[tokio::test]
    async fn second_instance_of_the_avd_is_never_learnt() {
        // P3bT7-4: the person's instance of the same AVD beside ours.
        let mut two = emulator(true);
        two.transports.push(transport("emulator-5556"));
        let never = emulator_row(None, None);
        assert_eq!(
            step(&never, &seen(Some(&two), false, None), ALIVE, 1, 1, 0),
            Step::Keep
        );
        // Learnt already, but no longer alone: no idle time, no shutdown.
        let ours = emulator_row(Some("emulator-5554"), Some(0));
        assert_eq!(
            step(&ours, &seen(Some(&two), false, None), ALIVE, 1 << 40, 1, 0),
            Step::Keep
        );
        // A serial adb listed before the spawn is never ours.
        let before = BootRow {
            identity_json: serde_json::to_string(&BootIdentity::Emulator {
                avd: "Pixel_9".into(),
                pid: 4242,
                start_time: 77,
                serial: None,
                not: vec!["emulator-5554".into()],
            })
            .unwrap(),
            ..never.clone()
        };
        let one = emulator(true);
        assert_eq!(
            step(&before, &seen(Some(&one), false, None), ALIVE, 1, 1, 0),
            Step::Keep
        );
        // And right before the kill: still exactly our one transport.
        let (fake, row) = idle_emulator();
        lock(&fake.0.devices).insert(avd(), two);
        assert_eq!(shut_down(&fake, &row).await, Shutdown::InUse);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn the_boot_learns_only_a_new_serial() {
        let fake = with(emulator(false));
        *lock(&fake.0.serials) = vec!["emulator-5554".into()];
        *lock(&fake.0.after_boot) = Some(emulator(true));
        boot_with(fake.clone(), &avd(), "c1").await.unwrap();
        let identity = fake.rows()[0].identity();
        assert!(
            matches!(&identity, Some(BootIdentity::Emulator { serial: None, not, .. }) if not == &["emulator-5554"]),
            "{identity:?}"
        );
    }

    #[test]
    fn within_clips_to_the_deadline() {
        let deadline = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            within(deadline, Duration::from_secs(10)),
            Some(Duration::from_secs(10))
        );
        assert!(within(deadline, Duration::from_secs(600)).unwrap() <= Duration::from_secs(60));
        assert_eq!(within(Instant::now(), Duration::from_secs(10)), None);
    }

    #[tokio::test]
    async fn a_failed_shutdown_is_retried_then_given_up() {
        let (fake, row) = idle_emulator();
        fake.0.kill_fails.store(true, Ordering::SeqCst);
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Failed);
        assert_eq!(fake.rows(), vec![row.clone()], "kept for the retry");
        assert!(
            !lock(&fake.0.marked).contains(&avd()),
            "available again meanwhile"
        );
        // Within RETRY_EVERY the tick does not try again.
        let now = fake.now();
        tick_once(&fake, &ClaimsView::default(), now + 1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(fake.calls().len(), 1);
        // After 10 minutes of failures: reported and given up.
        fake.0.now.store(now + RETRY_FOR_MS, Ordering::SeqCst);
        assert_eq!(shut_down(&fake, &row).await, Shutdown::Failed);
        assert!(fake.rows().is_empty());
        assert!(!lock(&fake.0.booted).contains(&avd()));
    }

    #[test]
    fn identity_round_trips() {
        let row = simulator_row("t");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.identity_json).unwrap()["kind"],
            "simulator"
        );
        let identity = row.identity().unwrap();
        assert_eq!(row.with_identity(&identity), row);
        // A record written before `not` existed still reads.
        let old = r#"{"kind":"emulator","avd":"Pixel_9","pid":1,"start_time":2,"serial":null}"#;
        let old = BootRow {
            identity_json: old.into(),
            ..emulator_row(None, None)
        };
        assert!(old.identity().is_some());
        // An unreadable identity drops the record (never a guessed shutdown).
        let broken = BootRow {
            identity_json: "{".into(),
            ..row
        };
        assert_eq!(
            step(&broken, &seen(None, false, None), ALIVE, 0, 0, 0),
            Step::Drop
        );
    }

    #[tokio::test]
    async fn unknown_sessions_keep_the_device_running() {
        // CRB-4: after a good empty read the status turns unreadable; the cache is empty but
        // says nothing, so no idle time starts and nothing is shut down.
        let (fake, mut row) = idle_emulator();
        row.idle_since_ms = None;
        fake.put_row(&row);
        fake.0.sessions_unknown.store(true, Ordering::SeqCst);
        let view = ClaimsView::default();
        let t0 = fake.now();
        for t in 0..5 {
            tick_once(&fake, &view, t0 + t * SHUTDOWN_GRACE_MS);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(fake.rows()[0].idle_since_ms, None);
        assert!(fake.calls().is_empty());
        // An idle time already running is cleared.
        fake.set_idle(&avd(), Some(t0));
        tick_once(&fake, &view, t0 + 10 * SHUTDOWN_GRACE_MS);
        assert_eq!(fake.rows()[0].idle_since_ms, None);
        // A good read: the rule goes on as before.
        fake.0.sessions_unknown.store(false, Ordering::SeqCst);
        let t1 = t0 + 20 * SHUTDOWN_GRACE_MS;
        tick_once(&fake, &view, t1);
        assert_eq!(fake.rows()[0].idle_since_ms, Some(t1));
        tick_once(&fake, &view, t1 + SHUTDOWN_GRACE_MS);
        for _ in 0..100 {
            if !fake.calls().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(fake.calls(), ["emu kill emulator-5554"]);
    }

    #[tokio::test]
    async fn sessions_unreadable_right_before_the_kill_cancel_it() {
        let (fake, row) = idle_emulator();
        fake.0.unknown_during_check.store(true, Ordering::SeqCst);
        assert_eq!(shut_down(&fake, &row).await, Shutdown::InUse);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        assert!(!lock(&fake.0.marked).contains(&avd()));
        assert_eq!(fake.rows().len(), 1, "still hostler's");
    }
}
