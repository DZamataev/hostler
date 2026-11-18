//! The claims engine in the service (plan P3a, Task 5): workspaces, FIFO claims on exclusive
//! resources, linked claims within a workspace, holds, reaping and deadlock refusals.
//!
//! The decisions are `claims::rules`; this engine loads the state from the store, applies them
//! and answers the contract. It runs under the server's claims lock, which is taken after the
//! heavy lock and never held across a secrets call or the KDF.

mod seize;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use super::rules::{self, Admission, DeviceEnd, HeavyRunning, HeavyView, Pending, Seizing, State};
use super::{ForeignClaimSource, ResourceCatalog};
use crate::contract::claims::{
    Claim, ClaimHistoryItem, ClaimKind, ClaimOutcome, ClaimRelease, ClaimRequest, ClaimStarted,
    ClaimSummary, ClaimsSnapshot, EndReason, ForeignClaim, ForeignView, Freshness, HoldBinding,
    Purpose, ReleaseOutcome, ResourceId, ResourceInfo, ResourceKind, ResourceState, SeizureRecord,
    Selector, WaitingRequest, WorkspaceInfo, WorkspaceOutcome, WorkspaceRef, WorkspaceResolve,
    errors,
};
use crate::contract::heavy::{EntrySource, HistoryQuery, QueueSnapshot};
use crate::contract::secrets::errors::{bad_request, service_stopping};
use crate::contract::wire::Event;
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::Clock;
use crate::procs::{self, Liveness, ProcessIdentity, ProcessTable};
use crate::project::display_path;
use crate::store::ProjectResolution;
use crate::store::Store;
use crate::store::{ClaimRow, PendingRow};
use crate::workspace::{self, Identification, Known};

/// At most this many live claims plus waiting requests.
pub const MAX_LIVE_ROWS: usize = 512;
/// A lease's command, already masked by the CLI.
pub const MAX_COMMAND_BYTES: usize = 1024;
const MAX_PATH_BYTES: usize = 2048;
const MAX_TOKENS: usize = 64;
/// The default and the bounds of a TTL hold.
const DEFAULT_TTL_MS: u64 = 30 * 60 * 1000;
const MAX_TTL_MS: u64 = 1440 * 60 * 1000;
/// An abandoned takeover plan.
pub const PLAN_TTL_MS: i64 = 60_000;
const PRUNE_EVERY_MS: i64 = 60 * 60 * 1000;
const HISTORY_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// A takeover in progress: its resource counts as seizing, and its taker's request is not
/// granted elsewhere (R2-8), until commit, abort or expiry (`seize.rs`).
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub resource: ResourceId,
    /// The taker's workspace path (`Blocker::Seizing`).
    pub by_workspace: String,
    pub request: ClaimRequest,
    pub expires_at_ms: i64,
    /// What `seize_begin` found and `seize_commit` verifies.
    pub detail: seize::Detail,
}

pub struct ClaimsEngine {
    store: Store,
    table: Arc<dyn ProcessTable>,
    clock: Arc<dyn Clock>,
    catalogs: Vec<Box<dyn ResourceCatalog>>,
    foreign: Vec<Box<dyn ForeignClaimSource>>,
    events: crate::service::events::EventBus,
    plans: Vec<Plan>,
    stopping: bool,
    last_snapshot: Option<ClaimsSnapshot>,
    /// The projects as `projects.changed` last published them (P5), compared without times.
    last_projects: Option<Vec<crate::contract::projects::ProjectView>>,
    last_prune_ms: Option<i64>,
    /// What the catalogs and foreign sources said last (T5-4).
    sources: std::cell::RefCell<Option<Sources>>,
    /// Workspace paths for the stranding check, probed off the claims lock (T5-1, T5-2).
    paths_probe: PathProbe,
    /// L41: since when (engine clock) each workspace path has been seen gone, in a row.
    path_gone_since: HashMap<String, i64>,
    /// The last claim end per device (P3b, L52, P3-3), in memory only: a device with no entry is
    /// as at start (`DeviceEnd::at_start`, seq 1). Written only by `end_claims` (P3-16) and
    /// `forget_device_end` (L54).
    device_ends: HashMap<ResourceId, DeviceEnd>,
}

/// The catalogs' resources and the foreign sources' claims, collected together. The tick and
/// the calls that decide on resources (`acquire`, `seize_begin`, `reserve`) collect them, once
/// each; every other call reads what was collected last (T5-4: P3b's sources run commands).
#[derive(Clone)]
struct Sources {
    catalog: Vec<ResourceInfo>,
    /// Each with its `workspace_id` resolved by the engine (L50).
    foreign: Vec<ForeignClaim>,
    /// From the same collections as `foreign` (P3-2).
    freshness: Freshness,
    /// Why a source cannot be read now (L54), for the `Checking` text.
    unreadable: Option<String>,
}

/// What the gate reads of several sources: an untracked one adds nothing; any source without a
/// good read yet closes every device (`NotYet`); else the oldest read decides. P3b has one
/// tracking source (agent-device), whose counter both `Since` and `end_seq` come from.
fn combined(views: &[ForeignView]) -> Freshness {
    views
        .iter()
        .fold(Freshness::NotTracked, |out, v| match (out, v.freshness) {
            (out, Freshness::NotTracked) => out,
            (Freshness::NotYet, _) | (_, Freshness::NotYet) => Freshness::NotYet,
            (Freshness::NotTracked, since) => since,
            (Freshness::Since(a), Freshness::Since(b)) => Freshness::Since(a.min(b)),
        })
}

/// `forget_device_end`'s answer (L54).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgetOutcome {
    /// The gate closed the device to other workspaces; it is open now, until its next claim end.
    Forgotten {
        /// The end this marked as checked.
        seq: u64,
    },
    /// The gate does not close it: nothing to forget.
    NotClosed,
}

/// L41: how long a workspace path must have been gone, in a row, before it strands a claim.
pub const PATH_GONE_MS: i64 = 2 * 60 * 1000;

/// `PATH_GONE_MS`; `HOSTLER_TEST_PATH_GONE_MS` in debug builds.
fn path_gone_ms() -> i64 {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_PATH_GONE_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
    {
        return ms;
    }
    PATH_GONE_MS
}

/// How often the service re-probes the workspace paths the tick asked about.
pub const PATH_PROBE_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
/// How long one probe may take before its path counts as unknown.
const PATH_PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);
/// A path the tick has not asked about for this long is no longer probed.
const PATH_FORGET: std::time::Duration = std::time::Duration::from_secs(600);

/// What a probe found at a workspace path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathState {
    Exists,
    /// Not found while its parent directory exists: the checkout itself was removed.
    Gone,
    /// Not probed yet, slow (a dead network mount), unreadable, or its parent is missing too
    /// (an unmounted volume, an offline share): counts as existing, so nothing is stranded.
    Unknown,
}

#[derive(Clone, Copy)]
struct PathEntry {
    state: PathState,
    asked: std::time::Instant,
    probing: bool,
}

/// A path, with the device its checkout root had at its last resolve (T5-R4): two rows at one
/// path keep their own entries (T6-R2-5).
type PathKey = (String, Option<u64>);

/// The stranding check's view of workspace paths (T5-1, T5-2). The tick reads it and never
/// blocks; `run` probes each path the tick asked about on its own thread, off the claims lock,
/// with a time bound. A probe stuck on a dead mount keeps its path unknown and is not repeated
/// while it hangs.
#[derive(Clone, Default)]
pub struct PathProbe(Arc<std::sync::Mutex<HashMap<PathKey, PathEntry>>>);

impl PathProbe {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<PathKey, PathEntry>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// False only when a probe saw the path gone; asks for it to be probed. `dev` is the
    /// checkout root's device recorded at its last resolve (T5-R4).
    pub fn exists(&self, path: &str, dev: Option<u64>) -> bool {
        self.state(path, dev) != PathState::Gone
    }

    pub fn state(&self, path: &str, dev: Option<u64>) -> PathState {
        let now = std::time::Instant::now();
        let mut entries = self.entries();
        let entry = entries.entry((path.to_owned(), dev)).or_insert(PathEntry {
            state: PathState::Unknown,
            asked: now,
            probing: false,
        });
        entry.asked = now;
        entry.state
    }

    fn finish(&self, key: &PathKey, state: PathState) {
        if let Some(entry) = self.entries().get_mut(key) {
            entry.state = state;
            entry.probing = false;
        }
    }

    /// One round: every wanted path not still being probed, each on its own thread, waited for
    /// at most `PATH_PROBE_LIMIT`.
    pub async fn refresh(&self) {
        let due: Vec<PathKey> = {
            let mut entries = self.entries();
            entries.retain(|_, e| e.probing || e.asked.elapsed() < PATH_FORGET);
            entries
                .iter_mut()
                .filter(|(_, e)| !e.probing)
                .map(|(key, e)| {
                    e.probing = true;
                    key.clone()
                })
                .collect()
        };
        let mut waits = Vec::new();
        for key in due {
            let (done, finished) = tokio::sync::oneshot::channel();
            let probe = self.clone();
            let probed = key.clone();
            // A plain thread, not the blocking pool: a probe stuck in the kernel must not hold
            // up the runtime's shutdown.
            let spawned = std::thread::Builder::new()
                .name("hostler-path-probe".into())
                .spawn(move || {
                    let state = probe_path(&probed.0, probed.1);
                    probe.finish(&probed, state);
                    let _ = done.send(());
                });
            if spawned.is_err() {
                self.finish(&key, PathState::Unknown);
                continue;
            }
            waits.push((key, finished));
        }
        let deadline = tokio::time::Instant::now() + PATH_PROBE_LIMIT;
        for (key, finished) in waits {
            if tokio::time::timeout_at(deadline, finished).await.is_err()
                && let Some(entry) = self.entries().get_mut(&key)
            {
                // Still probing: unknown until the probe returns.
                entry.state = PathState::Unknown;
            }
        }
    }

    /// Refreshes every `PATH_PROBE_EVERY`, for the service's lifetime.
    pub async fn run(self) {
        let mut every = tokio::time::interval(path_probe_every());
        loop {
            every.tick().await;
            self.refresh().await;
        }
    }

    /// Probes every wanted path now, on this thread (tests).
    #[cfg(test)]
    pub(crate) fn probe_now(&self) {
        let keys: Vec<PathKey> = self.entries().keys().cloned().collect();
        for key in keys {
            let state = probe_path(&key.0, key.1);
            self.finish(&key, state);
        }
    }
}

fn path_probe_every() -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_CLAIMS_TICK_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        return std::time::Duration::from_millis(ms);
    }
    PATH_PROBE_EVERY
}

/// `Gone` only for "not found" under a parent that exists — on Unix, on the device the checkout
/// root had at its last resolve (T5-R4: a Linux mount point stays as an empty directory after an
/// unmount, and a checkout may itself be a mount root); anything else is `Unknown`.
/// Debug builds: `HOSTLER_TEST_PATH_PROBE_BLOCK=<text>` makes a probe of a path containing it
/// hang for an hour, like a dead network mount.
fn probe_path(path: &str, dev: Option<u64>) -> PathState {
    #[cfg(debug_assertions)]
    if let Ok(text) = std::env::var("HOSTLER_TEST_PATH_PROBE_BLOCK")
        && !text.is_empty()
        && path.contains(&text)
    {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
    let path = Path::new(path);
    match std::fs::metadata(path) {
        Ok(_) => PathState::Exists,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match path.parent().map(std::fs::metadata) {
                Some(Ok(parent)) if parent.is_dir() && same_device(&parent, dev) => PathState::Gone,
                _ => PathState::Unknown,
            }
        }
        Err(_) => PathState::Unknown,
    }
}

/// Whether the parent directory is on the checkout root's recorded device (Unix). Without a
/// recorded device the parent rule alone decides, as before T5-R4 (T6-R2-5); Windows keeps
/// that rule.
#[cfg(unix)]
fn same_device(parent: &std::fs::Metadata, dev: Option<u64>) -> bool {
    use std::os::unix::fs::MetadataExt;
    dev.is_none_or(|dev| dev == parent.dev())
}

#[cfg(not(unix))]
fn same_device(_parent: &std::fs::Metadata, _dev: Option<u64>) -> bool {
    true
}

/// Process ancestries (the pid first) for one call, through at most ONE `parent_lookup`, made
/// on first use (R2-7, T5-3): on Windows each lookup is a full process snapshot.
pub(crate) struct Ancestry<'a> {
    table: &'a dyn ProcessTable,
    chains: HashMap<u32, Vec<u32>>,
    /// Parents and start times (on Windows one system snapshot, L46), made on first use.
    lookup: Option<procs::LineageLookup<'a>>,
}

impl<'a> Ancestry<'a> {
    pub(crate) fn new(table: &'a dyn ProcessTable) -> Ancestry<'a> {
        Ancestry {
            table,
            chains: HashMap::new(),
            lookup: None,
        }
    }

    /// `pid` and its ancestors, cut at a parent that started after its child (CR-A-1).
    pub(crate) fn chain(&mut self, pid: u32) -> Vec<u32> {
        if let Some(chain) = self.chains.get(&pid) {
            return chain.clone();
        }
        let table = self.table;
        let lookup = self.lookup.get_or_insert_with(|| table.lineage_lookup());
        let mut chain = vec![pid];
        chain.extend(procs::ancestors_checked(pid, lookup));
        self.chains.insert(pid, chain.clone());
        chain
    }

    fn live(&self, p: &ProcessIdentity) -> bool {
        procs::liveness(p, self.table) == Liveness::Alive
    }

    /// The heavy view with every ancestry the server left as the holder alone filled in — from
    /// live identities only (L47): a running entry whose holder died stands for its live child,
    /// one with neither is left out (it is reaped; leaving it out only makes a deadlock less
    /// likely), and a waiting entry whose holder (`waiting_holders`, from the store) died is
    /// left out.
    fn heavy(
        &mut self,
        heavy: &HeavyView,
        waiting_holders: &HashMap<String, ProcessIdentity>,
    ) -> HeavyView {
        let mut out = heavy.clone();
        out.running.retain_mut(|r| {
            if !self.live(&r.holder) {
                match r.child.clone().filter(|c| self.live(c)) {
                    Some(child) => {
                        r.holder = child;
                        r.holder_ancestry.clear();
                    }
                    None => return false,
                }
            }
            if r.holder_ancestry.len() <= 1 {
                r.holder_ancestry = self.chain(r.holder.pid);
            }
            true
        });
        out.waiting.retain_mut(|(id, chain)| {
            if waiting_holders.get(id).is_some_and(|h| !self.live(h)) {
                return false;
            }
            if let [pid] = chain[..] {
                *chain = self.chain(pid);
            }
            true
        });
        out
    }
}

fn internal(e: anyhow::Error) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: format!("the claims store failed: {e:#}"),
        details: json!({}),
        recovery: None,
    }
}

fn queue_busy() -> HostlerError {
    HostlerError {
        code: ErrorCode::QueueBusy,
        message: format!(
            "{MAX_LIVE_ROWS} claims and waiting requests are live on this machine; release some \
             first"
        ),
        details: json!({ "limit": MAX_LIVE_ROWS }),
        recovery: Some("hostler status".into()),
    }
}

/// The resource as the engine shows it: the catalog's entry, an account's synthesized one, or —
/// for a resource that left the catalog while claimed — an unavailable placeholder, so its
/// claims are kept.
fn info_for(id: &ResourceId, catalog: &[ResourceInfo]) -> ResourceInfo {
    rules::resource_info(id, catalog).unwrap_or_else(|| ResourceInfo {
        id: id.clone(),
        kind: id.kind().unwrap_or(ResourceKind::Device),
        display: id.0.clone(),
        attributes: Default::default(),
        available: false,
    })
}

/// What `--confirmation` must equal: a device's catalog display (its alias, P3b), else the id's
/// token (`screen`, the account name) — T1-3.
pub(crate) fn token_of(info: &ResourceInfo) -> String {
    match info.kind {
        ResourceKind::Device => info.display.clone(),
        _ => info.id.token(),
    }
}

fn kind_name(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Screen => "screen",
        ResourceKind::Account => "account",
        ResourceKind::Device => "device",
    }
}

fn selector_display(sel: &Selector, catalog: &[ResourceInfo]) -> String {
    match sel {
        Selector::Exact { resource } => info_for(resource, catalog).display,
        Selector::Any { kind, .. } => format!("any {}", kind_name(*kind)),
    }
}

/// A stored resource string (an id, or `any:<kind>` for a request that never got one) as people
/// read it (T3-5).
fn stored_display(stored: &str, catalog: &[ResourceInfo]) -> String {
    match stored.strip_prefix("any:") {
        Some(kind) => format!("any {kind}"),
        None => info_for(&ResourceId(stored.to_owned()), catalog).display,
    }
}

fn claim_kind(purpose: &Purpose) -> ClaimKind {
    match purpose {
        Purpose::Lease { .. } => ClaimKind::Lease,
        Purpose::Hold { .. } => ClaimKind::Hold,
    }
}

/// `ClaimsEngine::device_in_use`'s answer (P3-6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InUse {
    pub claims: Vec<ClaimSummary>,
    /// Pending requests for exactly this device, plus live takeovers of it.
    pub waiting: usize,
}

impl InUse {
    pub fn is_free(&self) -> bool {
        self.claims.is_empty() && self.waiting == 0
    }
}

/// The binding a granted or joined hold gets; a lease has none.
fn binding_for(purpose: &Purpose, now_ms: i64) -> Option<HoldBinding> {
    match purpose {
        Purpose::Lease { .. } => None,
        Purpose::Hold {
            bound_to: Some(holder),
            ..
        } => Some(HoldBinding::Process {
            holder: holder.clone(),
        }),
        Purpose::Hold { ttl_ms, .. } => {
            let ttl_ms = ttl_ms.unwrap_or(DEFAULT_TTL_MS);
            Some(HoldBinding::Ttl {
                ttl_ms,
                expires_at_ms: now_ms + ttl_ms as i64,
            })
        }
    }
}

fn is_uuid_v4(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|u| u.get_version_num() == 4)
}

/// Everything the rules read, loaded once per call.
struct Data {
    /// Workspace id → path.
    paths: HashMap<String, String>,
    claims: Vec<Claim>,
    pending: Vec<Pending>,
    created: HashMap<String, i64>,
    foreign: Vec<ForeignClaim>,
    catalog: Vec<ResourceInfo>,
    seizing: Vec<Seizing>,
    planned: Vec<String>,
    /// Claim id or `heavy:<id>` → every enclosing run recorded at acquire, nearest first
    /// (L28, T6-R2-4).
    nesting: HashMap<String, Vec<String>>,
    /// `claims` as the rules see them (L47): a holder or child that is not alive is left out, so
    /// a dead wrapper's pid — maybe a stranger's now — counts in no deadlock rule.
    state_claims: Vec<Claim>,
    /// Workspace path → the devices its rows' checkout roots had at their last resolve (T5-R4),
    /// one per workspace row (T6-R2-5).
    root_devs: HashMap<String, Vec<Option<u64>>>,
    device_ends: Vec<DeviceEnd>,
    freshness: Freshness,
    unreadable: Option<String>,
}

impl Data {
    fn state<'a>(&'a self, heavy: &'a HeavyView) -> State<'a> {
        State {
            claims: &self.state_claims,
            pending: &self.pending,
            foreign: &self.foreign,
            catalog: &self.catalog,
            seizing: &self.seizing,
            planned_requests: &self.planned,
            heavy,
            device_ends: &self.device_ends,
            freshness: self.freshness,
            sessions_unreadable: self.unreadable.as_deref(),
        }
    }
}

/// The heavy queue as the deadlock check needs it. Each ancestry is the holder alone: the
/// engine fills them in with the same parent lookup as its own requests' (T5-3).
pub fn heavy_view(snapshot: &QueueSnapshot) -> HeavyView {
    let ancestry = |pid: u32| vec![pid];
    let own = |e: &&crate::contract::heavy::HeavyEntry| e.source == EntrySource::Hostler;
    let running: Vec<HeavyRunning> = snapshot
        .running
        .iter()
        .filter(own)
        .filter(|e| e.state.occupies_slot())
        .map(|e| HeavyRunning {
            id: e.id.clone(),
            holder: e.holder.clone(),
            child: e.child.clone(),
            group: e.group.clone(),
            holder_ancestry: ancestry(e.holder.pid),
        })
        .collect();
    let waiting = snapshot
        .waiting
        .iter()
        .map(|e| (e.id.clone(), ancestry(e.holder.pid)))
        .collect();
    HeavyView {
        // Foreign heavy work holds slots hostler's entries cannot use; it progresses on its own.
        capacity: snapshot
            .capacity
            .saturating_sub(snapshot.foreign_running.len().min(255) as u8),
        running,
        waiting,
    }
}

/// How long the checkout's own filesystem work may take before `workspace.resolve` gives up.
const CHECKOUT_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a moved project's old path may take to answer, under the claims lock.
const OLD_PATH_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

/// What `workspace.resolve` learns from the checkout before it takes the claims lock.
#[derive(Clone)]
struct Prep {
    name: String,
    /// The canonical checkout root.
    root: std::path::PathBuf,
    git_dir: std::path::PathBuf,
    common: String,
    identity: Option<String>,
    /// The root's device (Unix `st_dev`), for the stranding check (T5-R4).
    root_dev: Option<u64>,
}

/// What the first store step decided.
#[derive(Clone)]
struct Resolved {
    project_id: String,
    repointed: bool,
    note: Option<String>,
    row: Option<Known>,
    row_project_dir: Option<String>,
    /// The id file's id, and its row's path and git dir as read (T5-R1).
    file_id: Option<String>,
    row_was: Option<(String, String)>,
}

/// How many times `workspace.resolve` redoes its decision when another copy changed the row.
const RESOLVE_ROUNDS: usize = 3;

/// How many calls per checkout or path outlived their limit and still run (T5-R3, T6-R2-2).
static HUNG: std::sync::LazyLock<std::sync::Mutex<HashMap<String, usize>>> =
    std::sync::LazyLock::new(Default::default);

fn hung() -> std::sync::MutexGuard<'static, HashMap<String, usize>> {
    HUNG.lock().unwrap_or_else(|e| e.into_inner())
}

/// One call, as its thread and its waiter share it.
#[derive(Default)]
struct CallState {
    done: bool,
    hung: bool,
}

#[derive(Clone)]
struct Call {
    key: String,
    state: Arc<std::sync::Mutex<CallState>>,
}

impl Call {
    fn state(&self) -> std::sync::MutexGuard<'_, CallState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The waiter gave up: until the thread returns, its key counts as hung.
    fn give_up(&self) {
        let mut state = self.state();
        if !state.done && !state.hung {
            state.hung = true;
            *hung().entry(self.key.clone()).or_default() += 1;
        }
    }
}

/// Dropped on the call's thread when `f` returns, or unwinds: a panic never leaves its key
/// hung.
struct Finished(Call);

impl Drop for Finished {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.done = true;
        if state.hung {
            let mut hung = hung();
            if let Some(n) = hung.get_mut(&self.0.key) {
                *n -= 1;
                if *n == 0 {
                    hung.remove(&self.0.key);
                }
            }
        }
    }
}

/// Runs `f` on its own thread for `key` (a checkout root or a path) and hands its result to
/// `send`. `None`, at once, while an earlier call for `key` has outlived its limit and still
/// runs: one stuck on a dead mount cannot be cancelled, so no more threads pile up behind it.
/// Healthy calls for one key run side by side (T6-R2-2).
fn spawn_for<T: Send + 'static>(
    key: &str,
    f: impl FnOnce() -> T + Send + 'static,
    send: impl FnOnce(T) + Send + 'static,
) -> Option<Call> {
    if hung().contains_key(key) {
        return None;
    }
    let call = Call {
        key: key.to_owned(),
        state: Default::default(),
    };
    let finished = Finished(call.clone());
    std::thread::Builder::new()
        .name("hostler-fs".into())
        .spawn(move || {
            let _finished = finished;
            send(f());
        })
        .ok()?;
    Some(call)
}

/// `f` for `key`, waited for at most `limit` on this thread; `None` past it (the key is then
/// hung until `f` returns), or while an earlier call for `key` is hung (`spawn_for`).
fn bounded<T: Send + 'static>(
    key: &str,
    limit: std::time::Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let call = spawn_for(key, f, move |out| {
        let _ = tx.send(out);
    })?;
    match rx.recv_timeout(limit) {
        Ok(out) => Some(out),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            call.give_up();
            None
        }
        // `f` panicked.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// `bounded`, awaited: the runtime thread is free meanwhile.
async fn off_lock<T: Send + 'static>(
    key: &str,
    limit: std::time::Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let call = spawn_for(key, f, move |out| {
        let _ = tx.send(out);
    })?;
    match tokio::time::timeout(limit, rx).await {
        Ok(out) => out.ok(),
        Err(_) => {
            call.give_up();
            None
        }
    }
}

fn slow_checkout(root: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: format!(
            "the checkout at {root} did not answer within {} s (a network mount that is gone?)",
            CHECKOUT_LIMIT.as_secs()
        ),
        details: json!({}),
        recovery: None,
    }
}

/// One `workspace.resolve` at a time per git dir: the id file is read, decided on and written
/// as one step for each checkout, without the claims lock.
async fn checkout_turn(git_dir: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    static TURNS: std::sync::LazyLock<
        std::sync::Mutex<HashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::LazyLock::new(Default::default);
    let turn = {
        let mut turns = TURNS.lock().unwrap_or_else(|e| e.into_inner());
        turns.retain(|_, t| Arc::strong_count(t) > 1);
        turns.entry(git_dir.to_owned()).or_default().clone()
    };
    turn.lock_owned().await
}

/// The checkout facts, off the lock.
fn prepare(r: WorkspaceResolve) -> Result<Prep, HostlerError> {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_RESOLVE_PAUSE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    // `root` is a display path (T1-2); `git_dir_for` canonicalizes it.
    let (checkout, git_dir) = workspace::git_dir_for(Path::new(&r.root))?;
    let common = display_path(&checkout.common_dir);
    let claimed = std::fs::canonicalize(&r.project.common_dir)
        .map(|d| display_path(&d))
        .ok();
    if claimed.as_deref() != Some(common.as_str()) {
        return Err(bad_request(format!(
            "the project's git directory is not this checkout's ({common})"
        )));
    }
    #[cfg(unix)]
    let root_dev = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&checkout.workspace_root)
            .ok()
            .map(|m| m.dev())
    };
    #[cfg(not(unix))]
    let root_dev = None;
    Ok(Prep {
        name: r.project.name,
        root_dev,
        identity: workspace::dir_identity(&checkout.common_dir),
        root: checkout.workspace_root,
        git_dir,
        common,
    })
}

/// `workspace.resolve` (T5-1). Every filesystem call runs off the claims lock — the checkout
/// and its id file before the first store step, the identity decision between the two store
/// steps, the id file's write after them — so a checkout on a dead mount stalls only its own
/// resolve, never the claims engine.
pub async fn resolve_workspace(
    claims: &tokio::sync::Mutex<ClaimsEngine>,
    mut r: WorkspaceResolve,
) -> Result<WorkspaceInfo, HostlerError> {
    // P5: the declaration is checked first (never echoed) and stored once the workspace is.
    let declared = r.declared.take();
    if let Some(decl) = &declared
        && let Err(why) = decl.check()
    {
        return Err(bad_request(format!(
            "invalid hostler.toml declaration: {why}"
        )));
    }
    let chars = r.project.name.chars().count();
    if !(1..=64).contains(&chars) || r.project.name.chars().any(char::is_control) {
        return Err(bad_request(
            "invalid project name (1 to 64 characters, no control characters)".into(),
        ));
    }
    if r.root.len() > MAX_PATH_BYTES {
        return Err(bad_request(format!(
            "a workspace path is at most {MAX_PATH_BYTES} bytes"
        )));
    }
    let root = r.root.clone();
    let prep = off_lock(&root, CHECKOUT_LIMIT, move || prepare(r))
        .await
        .ok_or_else(|| slow_checkout(&root))??;
    let shown = display_path(&prep.root);
    let _turn = checkout_turn(&prep.git_dir).await;
    let git_dir = prep.git_dir.clone();
    let file = off_lock(&shown, CHECKOUT_LIMIT, move || {
        workspace::read_id_file(&git_dir)
    })
    .await
    .ok_or_else(|| slow_checkout(&shown))?;

    // T5-R1: redone when another copy changed the row meanwhile.
    let mut decided = None;
    for _ in 0..RESOLVE_ROUNDS {
        let resolved = claims.lock().await.resolve_step(&prep, file.as_ref())?;
        let (p, res, f) = (prep.clone(), resolved.clone(), file.clone());
        let identification = off_lock(&shown, CHECKOUT_LIMIT, move || {
            #[cfg(debug_assertions)]
            if let Some(ms) = std::env::var("HOSTLER_TEST_RESOLVE_STEP_PAUSE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
            {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            // The OLD project's directory decides whether that project may follow the move.
            let row = res.row.clone().map(|mut row| {
                row.project_dir_exists = res
                    .row_project_dir
                    .as_ref()
                    .is_some_and(|d| Path::new(d).exists());
                row
            });
            let known = |id: &str| row.clone().filter(|r| r.id == id);
            workspace::identify(
                f.as_ref(),
                &p.root,
                &p.git_dir,
                &res.project_id,
                &known,
                &|path| path.exists(),
            )
        })
        .await
        .ok_or_else(|| slow_checkout(&shown))?;
        // Two checkouts sharing one git dir (T2-1): nothing is written.
        if let Identification::Refused(e) = identification {
            return Err(e);
        }
        if let Some(info) = claims
            .lock()
            .await
            .record_step(&prep, resolved, &identification)?
        {
            decided = Some((info, identification));
            break;
        }
    }
    let Some((info, identification)) = decided else {
        return Err(HostlerError {
            code: ErrorCode::Internal,
            message: format!(
                "the workspace record of {shown} kept changing while it was resolved; try again"
            ),
            details: json!({}),
            recovery: None,
        });
    };

    let git_dir = prep.git_dir.clone();
    let root = prep.root.clone();
    let fresh = matches!(identification, Identification::New { .. });
    let written = off_lock(&shown, CHECKOUT_LIMIT, move || {
        workspace::write_identification(&git_dir, &root, &identification).is_ok()
    })
    .await;
    if written != Some(true) {
        // A new id must not stay stored without its id file (T6-R2-2): the next resolve would
        // mint another and leave this row behind.
        if fresh {
            claims.lock().await.forget_new_workspace(&info.id);
        }
        return Err(HostlerError {
            code: ErrorCode::Internal,
            message: format!(
                "cannot record the workspace id in {}",
                display_path(&prep.git_dir)
            ),
            details: json!({}),
            recovery: None,
        });
    }
    // `None` (no hostler.toml) leaves the stored declaration alone.
    if let Some(decl) = &declared {
        claims.lock().await.store_declared(&info.project_id, decl)?;
    }
    Ok(info)
}

impl ClaimsEngine {
    pub fn new(
        store: Store,
        table: Arc<dyn ProcessTable>,
        clock: Arc<dyn Clock>,
        catalogs: Vec<Box<dyn ResourceCatalog>>,
        foreign: Vec<Box<dyn ForeignClaimSource>>,
        events: crate::service::events::EventBus,
    ) -> ClaimsEngine {
        ClaimsEngine {
            store,
            table,
            clock,
            catalogs,
            foreign,
            events,
            plans: Vec::new(),
            stopping: false,
            last_snapshot: None,
            last_projects: None,
            last_prune_ms: None,
            sources: std::cell::RefCell::new(None),
            paths_probe: PathProbe::default(),
            path_gone_since: HashMap::new(),
            device_ends: HashMap::new(),
        }
    }

    /// The holders of the view's waiting heavy entries, from the store (L47); empty without
    /// waiting entries.
    fn waiting_heavy_holders(&self, heavy: &HeavyView) -> HashMap<String, ProcessIdentity> {
        if heavy.waiting.is_empty() {
            return HashMap::new();
        }
        self.store
            .entries()
            .map(|entries| entries.into_iter().map(|e| (e.id, e.holder)).collect())
            .unwrap_or_default()
    }

    /// The probe the service refreshes in the background (`PathProbe::run`).
    pub fn path_probe(&self) -> PathProbe {
        self.paths_probe.clone()
    }

    /// Collects the catalogs and the foreign sources now: each source's claims and freshness in
    /// one `collect` (P3-2). Every foreign claim's `workspace_id` is the registered workspace that
    /// owns its path (L50; a source's own value is ignored); the registry is read here, so a
    /// workspace registered since the last collection counts at once. Unreadable, nothing owns
    /// any path, and every session blocks everyone.
    fn refresh_sources(&self) -> Sources {
        let mut seen = HashSet::new();
        let catalog = self
            .catalogs
            .iter()
            .flat_map(|c| c.resources())
            .filter(|r| seen.insert(r.id.clone()))
            .collect();
        let views: Vec<ForeignView> = self.foreign.iter().map(|f| f.collect()).collect();
        // `record_device_ends` keeps the largest `claim_ended` answer and the gate the smallest
        // `Since`: sound for one counter. Two tracking sources on different counters would keep
        // a device closed for good (P3bT9a-4).
        debug_assert!(
            views
                .iter()
                .filter(|v| !matches!(v.freshness, Freshness::NotTracked))
                .count()
                <= 1,
            "at most one foreign source tracks claim ends"
        );
        let freshness = combined(&views);
        let unreadable = views.iter().find_map(|v| v.unreadable.clone());
        let workspaces: Vec<(String, String)> = match self.store.workspaces() {
            Ok(rows) => rows.into_iter().map(|w| (w.id, w.path)).collect(),
            Err(e) => {
                tracing::warn!("cannot read the workspaces for foreign claims: {e:#}");
                Vec::new()
            }
        };
        let foreign = views
            .into_iter()
            .flat_map(|v| v.claims)
            .map(|mut f| {
                f.workspace_id = f
                    .workspace_path
                    .as_deref()
                    .and_then(|path| {
                        super::workspace_match::owning_workspace(&workspaces, path, cfg!(windows))
                    })
                    .map(str::to_owned);
                f
            })
            .collect();
        let sources = Sources {
            catalog,
            foreign,
            freshness,
            unreadable,
        };
        *self.sources.borrow_mut() = Some(sources.clone());
        sources
    }

    /// What was collected last (collecting it if nothing was yet).
    fn sources(&self) -> Sources {
        if let Some(sources) = self.sources.borrow().as_ref() {
            return sources.clone();
        }
        self.refresh_sources()
    }

    pub fn table(&self) -> &dyn ProcessTable {
        self.table.as_ref()
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    fn live(&self, p: &ProcessIdentity) -> bool {
        procs::liveness(p, self.table.as_ref()) == Liveness::Alive
    }

    fn lease_live(&self, c: &Claim) -> bool {
        rules::lease_alive(c, &|p| self.live(p), &|g| {
            procs::group_liveness(g, self.table.as_ref())
        })
    }

    fn catalog(&self) -> Vec<ResourceInfo> {
        self.sources().catalog
    }

    /// The live state, without process ancestries.
    fn load(&self) -> Result<Data, HostlerError> {
        self.load_for(None, None, &no_heavy())
            .map(|(data, _, _)| data)
    }

    /// The live state with every waiting request's ancestry, plus `requester`'s and the heavy
    /// view's, all through `ancestry`'s one parent lookup (R2-7, T5-3).
    fn load_for(
        &self,
        mut ancestry: Option<&mut Ancestry<'_>>,
        requester: Option<u32>,
        heavy: &HeavyView,
    ) -> Result<(Data, Vec<u32>, HeavyView), HostlerError> {
        let sources = self.sources();
        let catalog = sources.catalog;
        let rows = self.store.workspaces().map_err(internal)?;
        let mut root_devs: HashMap<String, Vec<Option<u64>>> = HashMap::new();
        for w in &rows {
            root_devs
                .entry(w.path.clone())
                .or_default()
                .push(w.root_dev);
        }
        let paths: HashMap<String, String> = rows.into_iter().map(|w| (w.id, w.path)).collect();
        let claims: Vec<Claim> = self
            .store
            .claims()
            .map_err(internal)?
            .into_iter()
            .map(|row| claim_of(row, &catalog, &paths))
            .collect();
        let rows = self.store.claim_requests().map_err(internal)?;
        let mut own = Vec::new();
        let mut heavy = heavy.clone();
        if let Some(ancestry) = ancestry.as_deref_mut() {
            if let Some(pid) = requester {
                own = ancestry.chain(pid);
            }
            heavy = ancestry.heavy(&heavy, &self.waiting_heavy_holders(&heavy));
        }
        let mut created = HashMap::new();
        let pending = rows
            .into_iter()
            .map(|row: PendingRow| {
                created.insert(row.id.clone(), row.created_at_ms);
                let chain = match ancestry.as_deref_mut() {
                    Some(ancestry) => ancestry.chain(row.requester.pid),
                    None => vec![row.requester.pid],
                };
                Pending {
                    id: row.id,
                    ticket: row.ticket,
                    selector: row.selector,
                    workspace_id: row.workspace_id,
                    purpose: row.purpose,
                    requester: row.requester,
                    ancestry: chain,
                }
            })
            .collect();
        let foreign = sources.foreign;
        let freshness = sources.freshness;
        let unreadable = sources.unreadable;
        let now = self.now();
        let live_plans = self.plans.iter().filter(|p| p.expires_at_ms > now);
        // L47: the rules see live identities only.
        let state_claims = claims
            .iter()
            .map(|c: &Claim| {
                let mut c = c.clone();
                c.holder = c.holder.filter(|h| self.live(h));
                c.child = c.child.filter(|h| self.live(h));
                c
            })
            .collect();
        let data = Data {
            paths,
            state_claims,
            claims,
            pending,
            created,
            foreign,
            catalog,
            seizing: live_plans
                .clone()
                .map(|p| Seizing {
                    resource: p.resource.clone(),
                    by_workspace: p.by_workspace.clone(),
                })
                .collect(),
            planned: live_plans.map(|p| p.request.id.clone()).collect(),
            nesting: self
                .store
                .nestings()
                .map_err(internal)?
                .into_iter()
                .map(|(id, outer)| (id, parse_nesting(&outer)))
                .collect(),
            root_devs,
            device_ends: self.device_ends.values().cloned().collect(),
            freshness,
            unreadable,
        };
        Ok((data, own, heavy))
    }

    // ---- workspaces ----

    /// The store step of `workspace.resolve` before the identity decision: the project (L21)
    /// and the row of the workspace the id file names. Its only filesystem calls are
    /// `resolve_project`'s checks of a moved project's old path, which happen only during a
    /// move and are bounded (unknown keeps the project where it is).
    fn resolve_step(
        &mut self,
        prep: &Prep,
        file: Option<&workspace::IdFile>,
    ) -> Result<Resolved, HostlerError> {
        let now = self.now();
        let identity = prep.identity.clone();
        let old_path = |dir: &str| {
            let dir = dir.to_owned();
            bounded(&dir.clone(), OLD_PATH_LIMIT, move || {
                workspace::dir_identity(Path::new(&dir))
            })
            .unwrap_or_else(|| identity.clone())
        };
        let (project, resolution) = self
            .store
            .resolve_project(
                &prep.common,
                &prep.name,
                prep.identity.as_deref(),
                &old_path,
                now,
            )
            .map_err(internal)?;
        if resolution != ProjectResolution::Existing {
            self.events.mark_projects_dirty();
        }
        let row = match file {
            Some(f) => self.store.workspace(&f.id).map_err(internal)?,
            None => None,
        };
        let row_was = row.as_ref().map(|r| (r.path.clone(), r.git_dir.clone()));
        let row_project_dir = match &row {
            Some(row) => self
                .store
                .project(&row.project_id)
                .map_err(internal)?
                .map(|p| p.common_dir),
            None => None,
        };
        Ok(Resolved {
            project_id: project.id,
            repointed: matches!(resolution, ProjectResolution::Repointed { .. }),
            note: matches!(resolution, ProjectResolution::MovedAway { .. }).then(|| {
                format!(
                    "{} now holds another repository; the old project waits for it at its new \
                     place",
                    display_path(&prep.root)
                )
            }),
            row: row.map(|row| Known {
                id: row.id,
                project_id: row.project_id,
                path: row.path,
                git_dir: row.git_dir,
                // Filled in off the lock.
                project_dir_exists: true,
            }),
            row_project_dir,
            file_id: file.map(|f| f.id.clone()),
            row_was,
        })
    }

    /// Removes a workspace row `record_step` just created when its id file could not be written.
    fn forget_new_workspace(&self, id: &str) {
        self.events.mark_projects_dirty();
        if let Err(e) = self.store.remove_unused_workspace(id) {
            tracing::warn!("cannot remove workspace {id} without its id file: {e:#}");
        }
    }

    /// The store step after the identity decision: re-point the old project (L15) and record
    /// the workspace. No filesystem call. `None` when the id file's row changed since
    /// `resolve_step` read it — another copy resolved meanwhile — so the decision is redone
    /// (T5-R1: two copies of a moved checkout never both keep its id).
    fn record_step(
        &mut self,
        prep: &Prep,
        resolved: Resolved,
        identification: &Identification,
    ) -> Result<Option<WorkspaceInfo>, HostlerError> {
        if let Some(id) = &resolved.file_id {
            let now_row = self
                .store
                .workspace(id)
                .map_err(internal)?
                .map(|r| (r.path, r.git_dir));
            if now_row != resolved.row_was {
                return Ok(None);
            }
        }
        let now = self.now();
        let (id, outcome, repoint_from) = match identification {
            Identification::Keep {
                id,
                outcome,
                repoint_from,
            } => (id.clone(), *outcome, repoint_from.clone()),
            Identification::New { id, outcome } => (id.clone(), *outcome, None),
            Identification::Refused(e) => return Err(e.clone()),
        };
        let (mut project_id, mut repointed, mut note) =
            (resolved.project_id, resolved.repointed, resolved.note);
        if let Some(old) = repoint_from {
            if self
                .store
                .repoint_project(&old, &prep.common)
                .map_err(internal)?
            {
                project_id = old;
                repointed = true;
            } else {
                note = Some(format!(
                    "a project for {} already exists; the old project stays at its old path",
                    prep.common
                ));
            }
        }
        let path = display_path(&prep.root);
        if outcome != WorkspaceOutcome::Known || repointed {
            self.events.mark_projects_dirty();
        }
        self.store
            .upsert_workspace(&id, &project_id, &path, &display_path(&prep.git_dir), now)
            .map_err(internal)?;
        self.store
            .set_workspace_root_dev(&id, prep.root_dev)
            .map_err(internal)?;
        Ok(Some(WorkspaceInfo {
            id,
            project_id,
            path,
            outcome,
            project_repointed: repointed,
            note,
            last_seen_ms: now,
        }))
    }

    /// P5: stores a checkout's declaration for its project when it differs from the stored
    /// one; a change is owed a `projects.changed`.
    fn store_declared(
        &self,
        project_id: &str,
        decl: &crate::contract::projects::ProjectDecl,
    ) -> Result<(), HostlerError> {
        if self
            .store
            .set_declared(project_id, decl, self.now())
            .map_err(internal)?
        {
            self.events.mark_projects_dirty();
        }
        Ok(())
    }

    /// Every project with its workspaces and stored declaration, by name then id.
    fn projects_view(&self) -> Result<Vec<crate::contract::projects::ProjectView>, HostlerError> {
        let mut declared = self.store.declarations().map_err(internal)?;
        let workspaces = self.workspaces()?;
        Ok(self
            .store
            .projects()
            .map_err(internal)?
            .into_iter()
            .map(|info| {
                let (declared, declared_at_ms) = match declared.remove(&info.id) {
                    Some((decl, at)) => (Some(decl), Some(at)),
                    None => (None, None),
                };
                crate::contract::projects::ProjectView {
                    workspaces: workspaces
                        .iter()
                        .filter(|w| w.project_id == info.id)
                        .cloned()
                        .collect(),
                    info,
                    declared,
                    declared_at_ms,
                }
            })
            .collect())
    }

    /// `projects.snapshot`'s answer, under the claims lock (`&self`), read at one point of the
    /// event order (C-A1, spec §6.2).
    pub fn projects_snapshot(
        &self,
    ) -> Result<crate::contract::projects::ProjectsSnapshot, HostlerError> {
        let projects = self.projects_view()?;
        Ok(crate::contract::projects::ProjectsSnapshot {
            projects,
            at_ms: self.now(),
            // Read after the capture, under the claims lock where `projects.changed` is
            // committed (R-1).
            as_of_seq: self.events.current_seq(),
        })
    }

    /// The projects without the times that change on every resolve.
    fn projects_key(
        projects: &[crate::contract::projects::ProjectView],
    ) -> Vec<crate::contract::projects::ProjectView> {
        let mut key = projects.to_vec();
        for p in &mut key {
            for w in &mut p.workspaces {
                w.last_seen_ms = 0;
            }
        }
        key
    }

    /// The projects as they are now become the baseline of `publish_projects` (at start).
    pub fn remember_projects(&mut self) {
        match self.projects_view() {
            Ok(view) => self.last_projects = Some(Self::projects_key(&view)),
            Err(e) => tracing::warn!("cannot read the projects: {}", e.message),
        }
    }

    /// Publishes `projects.changed` when the projects differ from the last published ones;
    /// read and numbered in one step (C-A1) under the claims lock (spec §6.2).
    pub fn publish_projects(&mut self) {
        let view = match self.projects_view() {
            Ok(view) => view,
            Err(e) => {
                tracing::warn!("cannot read the projects: {}", e.message);
                return;
            }
        };
        let key = Self::projects_key(&view);
        if self.last_projects.as_ref() == Some(&key) {
            return;
        }
        self.last_projects = Some(key);
        self.events
            .publish_snapshot(crate::contract::projects::ProjectsSnapshot {
                projects: view,
                at_ms: self.now(),
                as_of_seq: 0,
            });
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceInfo>, HostlerError> {
        Ok(self
            .store
            .workspaces()
            .map_err(internal)?
            .into_iter()
            .map(|w| WorkspaceInfo {
                id: w.id,
                project_id: w.project_id,
                path: w.path,
                outcome: WorkspaceOutcome::Known,
                project_repointed: false,
                note: None,
                last_seen_ms: w.last_seen_ms,
            })
            .collect())
    }

    // ---- claims ----

    /// The answer for an id the engine already knows: a live claim, a waiting request or history.
    fn known(&self, id: &str) -> Result<Option<ClaimOutcome>, HostlerError> {
        let data = self.load()?;
        if let Some(claim) = data.claims.iter().find(|c| c.id == id) {
            return Ok(Some(if claim.parent.is_some() {
                ClaimOutcome::Joined {
                    claim: claim.clone(),
                }
            } else {
                ClaimOutcome::Granted {
                    claim: claim.clone(),
                }
            }));
        }
        if data.pending.iter().any(|p| p.id == id) {
            let heavy = no_heavy();
            let state = data.state(&heavy);
            let (position, of) = rules::position(id, &state).unwrap_or((1, 1));
            return Ok(Some(ClaimOutcome::Queued {
                request_id: id.to_owned(),
                position,
                of,
                blockers: rules::blockers(id, &state),
            }));
        }
        Ok(self
            .store
            .history_item_of(id)
            .map_err(internal)?
            .map(|item| ClaimOutcome::Finished {
                item: self.history_display(item, &data.catalog),
            }))
    }

    fn history_display(
        &self,
        mut item: ClaimHistoryItem,
        catalog: &[ResourceInfo],
    ) -> ClaimHistoryItem {
        item.resource = stored_display(&item.resource, catalog);
        item
    }

    fn check_request(
        &self,
        r: &ClaimRequest,
        catalog: &[ResourceInfo],
    ) -> Result<(), HostlerError> {
        if !is_uuid_v4(&r.id) {
            return Err(bad_request("a claim request id is a UUID v4".into()));
        }
        if r.reentrant_tokens.len() > MAX_TOKENS {
            return Err(bad_request(format!(
                "at most {MAX_TOKENS} re-entrant claim tokens"
            )));
        }
        match &r.purpose {
            Purpose::Lease { command } if command.len() > MAX_COMMAND_BYTES => {
                return Err(bad_request(format!(
                    "a claim's command is at most {MAX_COMMAND_BYTES} bytes"
                )));
            }
            Purpose::Hold {
                ttl_ms: Some(_),
                bound_to: Some(_),
                ..
            } => {
                return Err(bad_request(
                    "a hold has a TTL or a bound process, not both".into(),
                ));
            }
            Purpose::Hold {
                ttl_ms: Some(ttl), ..
            } if *ttl == 0 || *ttl > MAX_TTL_MS => {
                return Err(bad_request(
                    "a TTL is more than 0 and at most 1440 minutes".into(),
                ));
            }
            Purpose::Hold {
                bound_to: Some(process),
                ..
            } if !self.live(process) => return Err(errors::bad_binding()),
            _ => {}
        }
        let workspace = self
            .store
            .workspace(&r.workspace_id)
            .map_err(internal)?
            .ok_or_else(|| {
                bad_request("unknown workspace; resolve it with workspace.resolve first".into())
            })?;
        match &r.selector {
            Selector::Exact { resource } => match resource.kind() {
                Some(ResourceKind::Screen) => {}
                Some(ResourceKind::Account) => {
                    if resource.account_project() != Some(workspace.project_id.as_str()) {
                        return Err(bad_request(
                            "an account claim names an account of this workspace's own project"
                                .into(),
                        ));
                    }
                }
                Some(ResourceKind::Device) if catalog.iter().any(|i| i.id == *resource) => {}
                _ => return Err(errors::unknown_resource(&resource.0)),
            },
            Selector::Any {
                kind: ResourceKind::Account,
                ..
            } => {
                return Err(bad_request(
                    "an account is claimed by name, not as any account".into(),
                ));
            }
            Selector::Any { .. } => {}
        }
        Ok(())
    }

    fn live_rows(&self) -> Result<usize, HostlerError> {
        Ok(self.store.claims().map_err(internal)?.len()
            + self.store.claim_requests().map_err(internal)?.len())
    }

    /// Idempotent by request id.
    pub fn acquire(
        &mut self,
        r: ClaimRequest,
        heavy: &HeavyView,
    ) -> Result<ClaimOutcome, HostlerError> {
        if let Some(known) = self.known(&r.id)? {
            return Ok(known);
        }
        if self.stopping {
            return Err(service_stopping());
        }
        let catalog = self.refresh_sources().catalog;
        self.check_request(&r, &catalog)?;
        let now = self.now();
        let table = self.table.clone();
        let mut lookup = Ancestry::new(table.as_ref());
        let (mut data, ancestry, heavy) =
            self.load_for(Some(&mut lookup), Some(r.requester.pid), heavy)?;
        let heavy = &heavy;
        // L28: nothing new starts under a run that is being taken over.
        if let Some(target) = self.planned_ancestor(&ancestry) {
            return Err(being_taken_over(&format!(
                "the run this request comes from ({target})"
            )));
        }
        let nested_in = enclosing(&ancestry, &data, heavy, &|p| self.live(p));
        let candidates = rules::candidates(&r.selector, &data.catalog);

        // 1. A re-entrant ancestor's lease on a candidate; never for a distinct hold, which
        // takes a resource its workspace does not use (P2-6, L16).
        let distinct = matches!(r.purpose, Purpose::Hold { distinct: true, .. });
        let reentrant = data.claims.iter().find(|c| {
            !distinct
                && c.kind == ClaimKind::Lease
                && r.reentrant_tokens.contains(&c.id)
                && candidates.contains(&c.resource.id)
                && [c.holder.as_ref(), c.child.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|p| ancestry[1..].contains(&p.pid) && self.live(p))
        });
        if let Some(c) = reentrant {
            return Ok(ClaimOutcome::Reentrant {
                claim_id: c.id.clone(),
            });
        }

        let ticket = data.pending.iter().map(|p| p.ticket).max().unwrap_or(0) + 1;
        let pending = Pending {
            id: r.id.clone(),
            ticket,
            selector: r.selector.clone(),
            workspace_id: r.workspace_id.clone(),
            purpose: r.purpose.clone(),
            requester: r.requester.clone(),
            ancestry,
        };
        // 2. Join this workspace's claim.
        match rules::admit(&pending, &data.state(heavy)) {
            Admission::Refuse(e) => return Err(e),
            Admission::Join { parent, resource } => {
                if self.live_rows()? >= MAX_LIVE_ROWS {
                    return Err(queue_busy());
                }
                let row = ClaimRow {
                    id: r.id.clone(),
                    resource,
                    kind: claim_kind(&r.purpose),
                    parent: Some(parent.clone()),
                    workspace_id: r.workspace_id.clone(),
                    holder: Some(r.requester.clone()),
                    child: None,
                    group: None,
                    binding: binding_for(&r.purpose, now),
                    command: match &r.purpose {
                        Purpose::Lease { command } => Some(command.clone()),
                        Purpose::Hold { .. } => None,
                    },
                    granted_at_ms: now,
                };
                self.store.insert_claim(&row).map_err(internal)?;
                if let Some(outer) = &nested_in {
                    self.store.set_nested_in(&r.id, outer).map_err(internal)?;
                }
                self.renew_parent(Some(&parent))?;
                self.publish();
                return match self.known(&r.id)? {
                    Some(outcome) => Ok(outcome),
                    None => Err(internal(anyhow::anyhow!("the joined claim is missing"))),
                };
            }
            Admission::Queue => {}
        }

        // 3–5 happen in `grants`; `--no-wait` asks it without queueing.
        data.pending.push(pending);
        if r.no_wait {
            let state = data.state(heavy);
            if !rules::grants(&state).iter().any(|(id, _)| *id == r.id) {
                let (position, of) = rules::position(&r.id, &state).unwrap_or((1, 1));
                return Ok(ClaimOutcome::Busy {
                    position,
                    of,
                    blockers: rules::blockers(&r.id, &state),
                });
            }
        }
        if self.live_rows()? >= MAX_LIVE_ROWS {
            return Err(queue_busy());
        }
        self.store.insert_claim_request(&r, now).map_err(internal)?;
        if let Some(outer) = &nested_in {
            self.store.set_nested_in(&r.id, outer).map_err(internal)?;
        }
        // A request that closes a cycle, as one of its victims, is refused at once (L25).
        let state = data.state(heavy);
        if victims(&state).contains(&r.id) {
            let cycle = cycle_text(&data, &state);
            self.store
                .end_request(&r.id, EndReason::Deadlock, Some(&cycle), now)
                .map_err(internal)?;
            self.publish();
            return Err(errors::claim_deadlock(
                &selector_display(&r.selector, &data.catalog),
                &cycle,
            ));
        }
        // The same ancestries: no second parent lookup (T5-3).
        self.settle_in(heavy, &mut lookup);
        match self.known(&r.id)? {
            Some(outcome) => Ok(outcome),
            None => Err(internal(anyhow::anyhow!("the request is missing"))),
        }
    }

    pub fn attach(&mut self, request_id: &str) -> Result<ClaimOutcome, HostlerError> {
        self.known(request_id)?.ok_or_else(|| HostlerError {
            code: ErrorCode::UnknownEntry,
            message: format!("no claim or request {request_id}"),
            details: json!({ "request_id": request_id }),
            recovery: Some("hostler status".into()),
        })
    }

    /// `ClaimNotHeld` when the claim ended (seized, reaped, released) before its command started.
    pub fn started(&mut self, s: ClaimStarted) -> Result<(), HostlerError> {
        // L27: a claim being taken over never starts its command; its wrapper abandons it.
        if self.start_refused(&s.claim_id, &s.child) {
            return Err(being_taken_over(&format!("claim {}", s.claim_id)));
        }
        if !self
            .store
            .set_claim_started(&s.claim_id, &s.child, &s.group)
            .map_err(internal)?
        {
            return Err(errors::claim_not_held(&s.claim_id));
        }
        // L31: the service keeps the run's job findable while the claim lives.
        self.table.hold_group("claims", &s.group);
        let parent = self
            .store
            .claim(&s.claim_id)
            .map_err(internal)?
            .and_then(|c| c.parent);
        self.renew_parent(parent.as_deref())?;
        self.publish();
        Ok(())
    }

    /// Idempotent: a claim that already ended answers again.
    pub fn release(&mut self, r: ClaimRelease) -> Result<ReleaseOutcome, HostlerError> {
        let parent = match self.store.claim(&r.claim_id).map_err(internal)? {
            // A run's lease is its wrapper's to end: a sibling's buggy release must not free the
            // resource under a running command (T5-5).
            Some(row)
                if row.workspace_id == r.workspace_id
                    && row.kind == ClaimKind::Lease
                    && row.holder.as_ref() != Some(&r.requester) =>
            {
                return Err(HostlerError {
                    code: ErrorCode::ClaimNotHeld,
                    message: format!(
                        "claim {} is a run of another process{}; only that run releases it",
                        r.claim_id,
                        row.holder
                            .as_ref()
                            .map(|h| format!(" (PID {})", h.pid))
                            .unwrap_or_default()
                    ),
                    details: json!({ "claim_id": r.claim_id }),
                    recovery: Some("hostler status".into()),
                });
            }
            Some(row) if row.workspace_id == r.workspace_id => {
                self.end_claims_as(
                    std::slice::from_ref(&r.claim_id),
                    EndReason::Released,
                    r.exit_code,
                    None,
                )
                .map_err(internal)?;
                // A linked claim's end renews its parent hold.
                self.renew_parent(row.parent.as_deref())?;
                self.settle(&no_heavy());
                row.parent
            }
            Some(_) => return Err(errors::claim_not_held(&r.claim_id)),
            None => {
                let Some(item) = self.store.history_item_of(&r.claim_id).map_err(internal)? else {
                    return Err(errors::claim_not_held(&r.claim_id));
                };
                // Ended by a takeover: its history row names the taker (`seized_by`).
                if item.reason == EndReason::Seized {
                    return Ok(ReleaseOutcome {
                        parent: None,
                        seized_by: item.detail,
                    });
                }
                None
            }
        };
        let parent = match parent {
            Some(id) => self.load()?.claims.into_iter().find(|c| c.id == id),
            None => None,
        };
        Ok(ReleaseOutcome {
            parent,
            seized_by: None,
        })
    }

    /// L23: ends another workspace's claim and every claim linked to it without stopping
    /// anything, recorded as a forced seizure.
    pub fn force_release(
        &mut self,
        claim_id: &str,
        confirmation: &str,
        workspace_id: &str,
        requester: &ProcessIdentity,
    ) -> Result<SeizureRecord, HostlerError> {
        let data = self.load()?;
        let claim = data
            .claims
            .iter()
            .find(|c| c.id == claim_id)
            .ok_or_else(|| errors::claim_not_held(claim_id))?;
        if confirmation != token_of(&claim.resource) {
            return Err(bad_request(format!(
                "--confirmation must be {}",
                token_of(&claim.resource)
            )));
        }
        let by = self
            .store
            .workspace(workspace_id)
            .map_err(internal)?
            .ok_or_else(|| {
                bad_request("unknown workspace; resolve it with workspace.resolve first".into())
            })?;
        let now = self.now();
        // A seized claim's history `detail` is the taker's workspace path (`seized_by`).
        self.end_claims_as(
            &with_linked(claim_id, &data.claims),
            EndReason::Seized,
            None,
            Some(&by.path),
        )
        .map_err(internal)?;
        let record = SeizureRecord {
            id: uuid::Uuid::new_v4().to_string(),
            resource: claim.resource.id.0.clone(),
            from_workspace_path: claim.workspace.path.clone(),
            from_kind: claim.kind.into(),
            from_source: None,
            from_pid: rules::summary(claim, &data.claims).pid,
            by_workspace_path: by.path,
            by_pid: requester.pid,
            at_ms: now,
            stranded: false,
            forced: true,
        };
        self.store.record_seizure(&record).map_err(internal)?;
        let shown = SeizureRecord {
            resource: claim.resource.display.clone(),
            ..record
        };
        self.events.publish(Event::ClaimsSeized(shown.clone()));
        self.settle(&no_heavy());
        Ok(shown)
    }

    /// Idempotent; a request granted meanwhile is the caller's to release.
    pub fn withdraw(&mut self, request_id: &str, timed_out: bool) -> Result<(), HostlerError> {
        let reason = if timed_out {
            EndReason::TimedOut
        } else {
            EndReason::Withdrawn
        };
        if self
            .store
            .end_request(request_id, reason, None, self.now())
            .map_err(internal)?
        {
            self.settle(&no_heavy());
        }
        Ok(())
    }

    fn own_claim(&self, claim_id: &str, workspace_id: &str) -> Result<ClaimRow, HostlerError> {
        match self.store.claim(claim_id).map_err(internal)? {
            Some(row) if row.workspace_id == workspace_id => Ok(row),
            _ => Err(errors::claim_not_held(claim_id)),
        }
    }

    /// A TTL hold's expiry becomes now + its TTL; other claims answer unchanged.
    pub fn renew(&mut self, claim_id: &str, workspace_id: &str) -> Result<Claim, HostlerError> {
        let row = self.own_claim(claim_id, workspace_id)?;
        if let Some(HoldBinding::Ttl { ttl_ms, .. }) = row.binding {
            self.store
                .set_binding(
                    claim_id,
                    &HoldBinding::Ttl {
                        ttl_ms,
                        expires_at_ms: self.now() + ttl_ms as i64,
                    },
                )
                .map_err(internal)?;
            self.publish();
        }
        self.load()?
            .claims
            .into_iter()
            .find(|c| c.id == claim_id)
            .ok_or_else(|| errors::claim_not_held(claim_id))
    }

    pub fn set_command(
        &mut self,
        claim_id: &str,
        workspace_id: &str,
        command: &str,
    ) -> Result<(), HostlerError> {
        if command.len() > MAX_COMMAND_BYTES {
            return Err(bad_request(format!(
                "a claim's command is at most {MAX_COMMAND_BYTES} bytes"
            )));
        }
        self.own_claim(claim_id, workspace_id)?;
        if !self
            .store
            .set_claim_command(claim_id, command)
            .map_err(internal)?
        {
            return Err(errors::claim_not_held(claim_id));
        }
        self.publish();
        Ok(())
    }

    /// A reservation gives precedence (step 3) and blocks other workspaces; it is refused while
    /// another workspace claims the resource.
    pub fn reserve(&mut self, r: &ResourceId, workspace_id: &str) -> Result<Claim, HostlerError> {
        self.refresh_sources();
        let data = self.load()?;
        match r.kind() {
            Some(ResourceKind::Screen) => {}
            Some(ResourceKind::Device) if data.catalog.iter().any(|i| i.id == *r) => {}
            Some(ResourceKind::Account) => {
                let workspace = self.store.workspace(workspace_id).map_err(internal)?;
                if r.account_project() != workspace.as_ref().map(|w| w.project_id.as_str()) {
                    return Err(bad_request(
                        "an account reservation names an account of this workspace's own \
                         project"
                            .into(),
                    ));
                }
            }
            _ => return Err(errors::unknown_resource(&r.0)),
        }
        if self
            .store
            .workspace(workspace_id)
            .map_err(internal)?
            .is_none()
        {
            return Err(bad_request(
                "unknown workspace; resolve it with workspace.resolve first".into(),
            ));
        }
        if let Some(other) = data
            .claims
            .iter()
            .find(|c| c.resource.id == *r && c.workspace.id != workspace_id)
        {
            return Err(errors::resource_in_use(
                &other.resource.display,
                &other.workspace.path,
            ));
        }
        if let Some(own) = data.claims.iter().find(|c| {
            c.resource.id == *r
                && c.workspace.id == workspace_id
                && c.kind == ClaimKind::Reservation
        }) {
            return Ok(own.clone());
        }
        if self.live_rows()? >= MAX_LIVE_ROWS {
            return Err(queue_busy());
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.store
            .insert_claim(&ClaimRow {
                id: id.clone(),
                resource: r.clone(),
                kind: ClaimKind::Reservation,
                parent: None,
                workspace_id: workspace_id.to_owned(),
                holder: None,
                child: None,
                group: None,
                binding: None,
                command: None,
                granted_at_ms: self.now(),
            })
            .map_err(internal)?;
        self.publish();
        self.load()?
            .claims
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| internal(anyhow::anyhow!("the reservation is missing")))
    }

    pub fn unreserve(&mut self, r: &ResourceId, workspace_id: &str) -> Result<(), HostlerError> {
        let data = self.load()?;
        let Some(own) = data.claims.iter().find(|c| {
            c.resource.id == *r
                && c.workspace.id == workspace_id
                && c.kind == ClaimKind::Reservation
        }) else {
            return Err(errors::claim_not_held(&r.0));
        };
        let id = own.id.clone();
        self.end_claims(&[id], EndReason::Released)
            .map_err(internal)?;
        self.settle(&no_heavy());
        Ok(())
    }

    /// The one way a claim ends (P3-16): every end site calls this, never the store's
    /// `end_claim*`. Ends each claim (the caller lists the linked ones it cascades to) and
    /// answers the ones that were still live.
    pub(crate) fn end_claims(
        &mut self,
        ids: &[String],
        reason: EndReason,
    ) -> anyhow::Result<Vec<Claim>> {
        self.end_claims_as(ids, reason, None, None)
    }

    /// `end_claims` with the history row's exit code (a run's release) and `detail` (a seizure's
    /// taker path). For every device it ended a claim on, it tells each foreign source
    /// (`claim_ended`) and keeps the answer as the device's end with the ending workspace (L52,
    /// P3-3), also when a later store write fails, so an end never goes unrecorded.
    pub(crate) fn end_claims_as(
        &mut self,
        ids: &[String],
        reason: EndReason,
        exit_code: Option<i32>,
        detail: Option<&str>,
    ) -> anyhow::Result<Vec<Claim>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.now();
        let catalog = self.sources().catalog;
        let paths: HashMap<String, String> = self
            .store
            .workspaces()?
            .into_iter()
            .map(|w| (w.id, w.path))
            .collect();
        let rows: HashMap<String, ClaimRow> = self
            .store
            .claims()?
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        let mut ended = Vec::new();
        let mut failed = None;
        for id in ids {
            let Some(row) = rows.get(id) else {
                continue;
            };
            match self
                .store
                .end_claim_with(id, reason, exit_code, detail, now)
            {
                Ok(true) => ended.push(claim_of(row.clone(), &catalog, &paths)),
                Ok(false) => {}
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        self.record_device_ends(&ended, reason);
        match failed {
            Some(e) => Err(e),
            None => Ok(ended),
        }
    }

    /// One `claim_ended` per device per call. The ending workspace is the claims' own when they
    /// share one, else none (closed to every workspace). A seizure's victim is none either
    /// (P3bT9a-2): it waits for the read like the forcer, and FIFO gives the forcer the device.
    /// An end never drops an earlier one that is still unverified (P3bT9a-1): the ender keeps
    /// its exemption only when that earlier end was its own too.
    fn record_device_ends(&mut self, ended: &[Claim], reason: EndReason) {
        let mut devices: Vec<(&ResourceId, Option<&str>)> = Vec::new();
        for c in ended
            .iter()
            .filter(|c| c.resource.id.kind() == Some(ResourceKind::Device))
        {
            let ws = (reason != EndReason::Seized).then_some(c.workspace.id.as_str());
            match devices.iter_mut().find(|(r, _)| **r == c.resource.id) {
                Some((_, owner)) if *owner != ws => *owner = None,
                Some(_) => {}
                None => devices.push((&c.resource.id, ws)),
            }
        }
        if devices.is_empty() {
            return;
        }
        // The last collection's freshness: older than now, so it errs towards "unverified".
        let freshness = self.sources().freshness;
        for (r, ws) in devices {
            let ws = match self.device_ends.get(r) {
                Some(old)
                    if old.workspace_id.as_deref() != ws
                        && !old.forgotten
                        && match freshness {
                            Freshness::NotTracked => false,
                            Freshness::NotYet => true,
                            Freshness::Since(read) => old.seq > read,
                        } =>
                {
                    None
                }
                _ => ws,
            };
            let seq = self
                .foreign
                .iter()
                .map(|f| f.claim_ended(r))
                .max()
                .unwrap_or(0);
            self.device_ends.insert(
                r.clone(),
                DeviceEnd {
                    resource: r.clone(),
                    seq,
                    workspace_id: ws.map(str::to_owned),
                    forgotten: false,
                },
            );
        }
    }

    /// L54 (`devices forget-sessions`, after the person's prompt): marks the device's last end
    /// as checked, so the freshness gate no longer keeps it from other workspaces, until its next
    /// claim end. It stops nothing; waiting requests are granted at once. `NotClosed` when the
    /// gate does not close it.
    pub fn forget_device_end(&mut self, r: &ResourceId) -> Result<ForgetOutcome, HostlerError> {
        if r.kind() != Some(ResourceKind::Device) {
            return Err(errors::unknown_resource(&r.0));
        }
        self.refresh_sources();
        let data = self.load()?;
        if !rules::closed_to_others(r, &data.state(&no_heavy())) {
            return Ok(ForgetOutcome::NotClosed);
        }
        let end = self
            .device_ends
            .entry(r.clone())
            .or_insert_with(|| DeviceEnd::at_start(r.clone()));
        end.forgotten = true;
        let seq = end.seq;
        self.settle(&no_heavy());
        Ok(ForgetOutcome::Forgotten { seq })
    }

    /// Under the claims lock (`&self`), read at one point of the event order (C-A1), which
    /// `as_of_seq` names (spec §6.2).
    pub fn snapshot(&self) -> Result<ClaimsSnapshot, HostlerError> {
        let mut snapshot = snapshot_of(&self.load()?);
        // Read after the capture, under the claims lock where `claims.changed` is committed.
        snapshot.as_of_seq = self.events.current_seq();
        Ok(snapshot)
    }

    pub fn history(&self, q: HistoryQuery) -> Result<Vec<ClaimHistoryItem>, HostlerError> {
        let catalog = self.catalog();
        Ok(self
            .store
            .claim_history(&q)
            .map_err(internal)?
            .into_iter()
            .map(|item| self.history_display(item, &catalog))
            .collect())
    }

    /// The root claim on `r`, for `account.list` and `account.remove`.
    pub fn holder_of(&self, r: &ResourceId) -> Option<ClaimSummary> {
        let data = self.load().ok()?;
        let on: Vec<&Claim> = data.claims.iter().filter(|c| c.resource.id == *r).collect();
        on.iter()
            .find(|c| c.parent.is_none())
            .or_else(|| on.first())
            .map(|c| rules::summary(c, &data.claims))
    }

    /// What keeps a device in use (P3-6): every claim on `r`, its root first (as `holder_of`
    /// answers), and the pending requests for exactly `r`. An `Any` request can take another
    /// device, so it does not count. Asked under the claims lock by `devices remove` and the
    /// boot shutdown rule; a store that cannot be read counts as one waiting request, so
    /// nothing is removed or shut down on a guess.
    pub fn device_in_use(&self, r: &ResourceId) -> InUse {
        let Ok(data) = self.load() else {
            return InUse {
                claims: Vec::new(),
                waiting: 1,
            };
        };
        let mut on: Vec<&Claim> = data.claims.iter().filter(|c| c.resource.id == *r).collect();
        on.sort_by_key(|c| c.parent.is_some());
        InUse {
            claims: on
                .into_iter()
                .map(|c| rules::summary(c, &data.claims))
                .collect(),
            // A takeover's taker may never have queued, or queued with `auto` (P3bT1-1): its live
            // plan keeps the device as a waiting request would, until commit, abort or expiry.
            waiting: data
                .pending
                .iter()
                .filter(|p| matches!(&p.selector, Selector::Exact { resource } if resource == r))
                .count()
                + data.seizing.iter().filter(|z| z.resource == *r).count(),
        }
    }

    /// `account.materialize`: the run's own live claim on `r`.
    pub fn check_holder(
        &self,
        claim_id: &str,
        r: &ResourceId,
        requester: &ProcessIdentity,
    ) -> Result<(), HostlerError> {
        match self.store.claim(claim_id).map_err(internal)? {
            Some(row) if row.resource == *r && row.holder.as_ref() == Some(requester) => Ok(()),
            _ => Err(errors::claim_not_held(claim_id)),
        }
    }

    /// Leases (linked ones included) whose holder, child or group lives: they block an idle stop.
    pub fn live_leases(&self) -> Vec<ClaimSummary> {
        let Ok(data) = self.load() else {
            return Vec::new();
        };
        data.claims
            .iter()
            .filter(|c| c.kind == ClaimKind::Lease && self.lease_live(c))
            .map(|c| rules::summary(c, &data.claims))
            .collect()
    }

    /// Grants nothing more; new acquires answer `service_stopping`.
    pub fn begin_stopping(&mut self) {
        self.stopping = true;
    }

    /// A TTL parent hold lives at least another TTL from now.
    fn renew_parent(&self, parent: Option<&str>) -> Result<(), HostlerError> {
        let Some(parent) = parent else {
            return Ok(());
        };
        let Some(row) = self.store.claim(parent).map_err(internal)? else {
            return Ok(());
        };
        if let Some(HoldBinding::Ttl {
            ttl_ms,
            expires_at_ms,
        }) = row.binding
        {
            let renewed = expires_at_ms.max(self.now() + ttl_ms as i64);
            if renewed != expires_at_ms {
                self.store
                    .set_binding(
                        parent,
                        &HoldBinding::Ttl {
                            ttl_ms,
                            expires_at_ms: renewed,
                        },
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    /// Every 2 s and after every mutation: reap, expire, drop old plans, refuse a deadlock's
    /// youngest victim, join and grant, prune, announce.
    pub fn tick(&mut self, heavy: &HeavyView) {
        self.refresh_sources();
        self.settle(heavy);
    }

    /// The tick's work after a mutation, on the sources collected last.
    fn settle(&mut self, heavy: &HeavyView) {
        let table = self.table.clone();
        self.settle_in(heavy, &mut Ancestry::new(table.as_ref()));
    }

    fn settle_in(&mut self, heavy: &HeavyView, ancestry: &mut Ancestry<'_>) {
        if let Err(e) = self.try_tick(heavy, ancestry) {
            tracing::error!("claims tick failed: {}", e.message);
        }
        self.publish();
    }

    fn try_tick(
        &mut self,
        heavy: &HeavyView,
        ancestry: &mut Ancestry<'_>,
    ) -> Result<(), HostlerError> {
        let now = self.now();
        let (mut data, _, heavy) = self.load_for(Some(ancestry), None, heavy)?;
        let heavy = &heavy;

        // Reap, then expire what is left (T4-2): a TTL hold whose last linked lease is reaped
        // now expires now.
        // Never a filesystem call under the claims lock: unknown counts as existing (T5-1). A path
        // shared by rows of several devices is gone only when gone for each of them. L41: a path
        // strands only after it has been gone for 2 minutes in a row (a moved checkout
        // re-resolves first).
        let mut gone_long: HashSet<String> = HashSet::new();
        let mut seen: HashSet<String> = HashSet::new();
        for c in &data.claims {
            let path = &c.workspace.path;
            if !matches!(c.kind, ClaimKind::Hold | ClaimKind::Reservation)
                || !seen.insert(path.clone())
            {
                continue;
            }
            let exists = match data.root_devs.get(path) {
                Some(devs) => devs.iter().any(|dev| self.paths_probe.exists(path, *dev)),
                None => self.paths_probe.exists(path, None),
            };
            if exists {
                self.path_gone_since.remove(path);
            } else {
                let since = *self.path_gone_since.entry(path.clone()).or_insert(now);
                if now - since >= path_gone_ms() {
                    gone_long.insert(path.clone());
                }
            }
        }
        self.path_gone_since.retain(|path, _| seen.contains(path));
        let reaped = rules::reapable(
            &data.claims,
            &data.pending,
            &|p| self.live(p),
            &|c| self.lease_live(c),
            &|path| !gone_long.contains(path),
        );
        let mut ended: HashSet<String> = HashSet::new();
        // Claims through `end_claims`, one call per reason, in `reapable`'s order; requests never
        // had the resource.
        let mut by_reason: Vec<(EndReason, Vec<String>)> = Vec::new();
        for (id, reason) in &reaped {
            if data.claims.iter().any(|c| c.id == *id) {
                match by_reason.iter_mut().find(|(r, _)| r == reason) {
                    Some((_, ids)) => ids.push(id.clone()),
                    None => by_reason.push((*reason, vec![id.clone()])),
                }
            } else if self
                .store
                .end_request(id, *reason, None, now)
                .map_err(internal)?
            {
                ended.insert(id.clone());
            }
        }
        for (reason, ids) in by_reason {
            for claim in self.end_claims(&ids, reason).map_err(internal)? {
                ended.insert(claim.id);
            }
        }
        // A reaped linked lease is a dead run: it does not renew its parent hold, which may now
        // expire in this same tick.
        data.claims.retain(|c| !ended.contains(&c.id));
        data.state_claims.retain(|c| !ended.contains(&c.id));
        data.pending.retain(|p| !ended.contains(&p.id));
        let expired = rules::expired(&data.claims, now);
        for claim in self
            .end_claims(&expired, EndReason::Expired)
            .map_err(internal)?
        {
            ended.insert(claim.id);
        }
        data.claims.retain(|c| !ended.contains(&c.id));
        data.state_claims.retain(|c| !ended.contains(&c.id));

        // L31: handles only for live claims' jobs (reopened after a restart).
        let groups: Vec<procs::RunGroup> =
            data.claims.iter().filter_map(|c| c.group.clone()).collect();
        self.table.hold_only("claims", &groups);

        // Abandoned plans.
        self.plans.retain(|p| p.expires_at_ms > now);

        // The youngest victim of a deadlock, one per tick (L25).
        let state = data.state(heavy);
        if let Some(victim) = victims(&state).first() {
            let cycle = cycle_text(&data, &state);
            self.store
                .end_request(victim, EndReason::Deadlock, Some(&cycle), now)
                .map_err(internal)?;
            let victim = victim.clone();
            data.pending.retain(|p| p.id != victim);
        }

        if !self.stopping {
            // A request queued before its workspace got the resource joins it now.
            let mut joined = false;
            for p in data.pending.clone() {
                if data.planned.contains(&p.id) {
                    continue;
                }
                match rules::admit(&p, &data.state(heavy)) {
                    Admission::Join { parent, resource } => {
                        self.store
                            .grant(
                                &p.id,
                                &resource,
                                claim_kind(&p.purpose),
                                Some(&parent),
                                binding_for(&p.purpose, now).as_ref(),
                                now,
                            )
                            .map_err(internal)?;
                        self.renew_parent(Some(&parent))?;
                        joined = true;
                        data.pending.retain(|q| q.id != p.id);
                    }
                    // With its own reason and text: nobody withdrew it (CR-A-4).
                    Admission::Refuse(e) => {
                        self.store
                            .end_request(&p.id, EndReason::Refused, Some(&e.message), now)
                            .map_err(internal)?;
                        data.pending.retain(|q| q.id != p.id);
                    }
                    Admission::Queue => {}
                }
            }
            if joined {
                let fresh = self.load()?;
                data.claims = fresh.claims;
                data.state_claims = fresh.state_claims;
            }
            for (id, resource) in rules::grants(&data.state(heavy)) {
                let Some(p) = data.pending.iter().find(|p| p.id == id) else {
                    continue;
                };
                self.store
                    .grant(
                        &id,
                        &resource,
                        claim_kind(&p.purpose),
                        None,
                        binding_for(&p.purpose, now).as_ref(),
                        now,
                    )
                    .map_err(internal)?;
            }
        }

        if self
            .last_prune_ms
            .is_none_or(|last| now - last >= PRUNE_EVERY_MS)
        {
            self.last_prune_ms = Some(now);
            if let Err(e) = self.store.prune_claim_history(now - HISTORY_RETENTION_MS) {
                tracing::warn!("cannot prune the claims history: {e:#}");
            }
        }
        Ok(())
    }

    /// Broadcasts `claims.changed` when the snapshot differs from the last one sent.
    /// Read and numbered in one step (C-A1).
    fn publish(&mut self) {
        let Ok(data) = self.load() else {
            return;
        };
        let snapshot = snapshot_of(&data);
        if self.last_snapshot.as_ref() != Some(&snapshot) {
            self.last_snapshot = Some(snapshot.clone());
            self.events.publish_snapshot(snapshot);
        }
    }
}

/// An empty heavy queue, for calls that do not look at it.
pub fn no_heavy() -> HeavyView {
    HeavyView {
        capacity: 1,
        running: Vec::new(),
        waiting: Vec::new(),
    }
}

/// L27/L28: refused because a takeover of `what` is in progress (L33: its own code).
fn being_taken_over(what: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::TakenOver,
        message: format!("{what} is being taken over; nothing may start under it now"),
        details: json!({}),
        recovery: Some("hostler status".into()),
    }
}

/// L28, T6-R2-4: every run enclosing a process with this ancestry (pid first), nearest first —
/// each live lease (its claim id) or running heavy entry (`heavy:<id>`) whose holder or child is
/// an ancestor, and the runs each of those was recorded under — as the JSON list stored in
/// `nested_in`; `None` when there is none. A chain stays whole when a middle run ends first.
fn enclosing(
    ancestry: &[u32],
    data: &Data,
    heavy: &HeavyView,
    live: &dyn Fn(&ProcessIdentity) -> bool,
) -> Option<String> {
    let mut outer: Vec<String> = Vec::new();
    let add = |id: String, outer: &mut Vec<String>| {
        if !outer.contains(&id) {
            outer.push(id);
        }
    };
    for pid in ancestry.iter().skip(1) {
        // A live identity only (CR-A-1): a dead holder's pid may be a stranger's now.
        let runs = |holder: Option<&ProcessIdentity>, child: Option<&ProcessIdentity>| {
            [holder, child]
                .into_iter()
                .flatten()
                .any(|p| p.pid == *pid && live(p))
        };
        let found = data
            .claims
            .iter()
            .filter(|c| c.kind == ClaimKind::Lease && runs(c.holder.as_ref(), c.child.as_ref()))
            .map(|c| c.id.clone())
            .chain(
                heavy
                    .running
                    .iter()
                    .filter(|h| runs(Some(&h.holder), h.child.as_ref()))
                    .map(|h| format!("heavy:{}", h.id)),
            )
            .collect::<Vec<_>>();
        for id in found {
            let recorded = data.nesting.get(&id).cloned().unwrap_or_default();
            add(id, &mut outer);
            for up in recorded {
                add(up, &mut outer);
            }
        }
    }
    (!outer.is_empty()).then(|| serde_json::to_string(&outer).expect("a list of strings"))
}

/// A stored `nested_in`: a JSON list (T6-R2-4), or a single id.
fn parse_nesting(stored: &str) -> Vec<String> {
    serde_json::from_str(stored).unwrap_or_else(|_| vec![stored.to_owned()])
}

/// The requests a deadlock refusal may pick, youngest first (L25: victims, never bystanders;
/// none for a heavy-only deadlock).
fn victims(s: &State) -> Vec<String> {
    rules::deadlocked(s).victims
}

fn path_in(paths: &HashMap<String, String>, workspace_id: &str) -> String {
    paths
        .get(workspace_id)
        .cloned()
        .unwrap_or_else(|| workspace_id.to_owned())
}

/// `{workspace path} (PID {pid}) holds {display} and waits for {display}`, for every claims
/// request in the cycle, joined with `; `.
fn cycle_text(data: &Data, s: &State) -> String {
    rules::deadlocked(s)
        .stuck
        .iter()
        .filter_map(|id| s.pending.iter().find(|p| p.id == *id))
        .map(|p| {
            let path = path_in(&data.paths, &p.workspace_id);
            let holds: Vec<String> = s
                .claims
                .iter()
                .filter(|c| {
                    [c.holder.as_ref(), c.child.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|h| p.ancestry.contains(&h.pid))
                })
                .map(|c| c.resource.display.clone())
                .collect();
            let wants = selector_display(&p.selector, s.catalog);
            if holds.is_empty() {
                format!("{path} (PID {}) waits for {wants}", p.requester.pid)
            } else {
                format!(
                    "{path} (PID {}) holds {} and waits for {wants}",
                    p.requester.pid,
                    holds.join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `id` and every claim linked to it, directly or through other linked claims.
/// A stored claim as the rules and the contract see it.
fn claim_of(row: ClaimRow, catalog: &[ResourceInfo], paths: &HashMap<String, String>) -> Claim {
    Claim {
        resource: info_for(&row.resource, catalog),
        workspace: WorkspaceRef {
            path: path_in(paths, &row.workspace_id),
            id: row.workspace_id,
        },
        id: row.id,
        kind: row.kind,
        parent: row.parent,
        holder: row.holder,
        child: row.child,
        group: row.group,
        binding: row.binding,
        command: row.command,
        granted_at_ms: row.granted_at_ms,
    }
}

fn with_linked(id: &str, claims: &[Claim]) -> Vec<String> {
    let mut out = vec![id.to_owned()];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].clone();
        out.extend(
            claims
                .iter()
                .filter(|c| c.parent.as_deref() == Some(parent.as_str()))
                .map(|c| c.id.clone()),
        );
        i += 1;
    }
    out
}

/// One state per resource in catalog order, then every other claimed or requested resource.
fn snapshot_of(data: &Data) -> ClaimsSnapshot {
    let mut ids: Vec<ResourceId> = data.catalog.iter().map(|i| i.id.clone()).collect();
    let mut extra: Vec<ResourceId> = data
        .claims
        .iter()
        .map(|c| c.resource.id.clone())
        .chain(data.pending.iter().filter_map(|p| match &p.selector {
            Selector::Exact { resource } => Some(resource.clone()),
            Selector::Any { .. } => None,
        }))
        .chain(data.foreign.iter().map(|f| f.resource.clone()))
        .filter(|id| !ids.contains(id))
        .collect();
    extra.sort();
    extra.dedup();
    ids.extend(extra);
    let resources = ids
        .into_iter()
        .map(|id| {
            let waiting = data
                .pending
                .iter()
                .filter(|p| rules::candidates(&p.selector, &data.catalog).contains(&id))
                .map(|p| WaitingRequest {
                    request_id: p.id.clone(),
                    ticket: p.ticket,
                    workspace_path: path_in(&data.paths, &p.workspace_id),
                    kind: claim_kind(&p.purpose),
                    since_ms: data.created.get(&p.id).copied().unwrap_or(0),
                })
                .collect();
            ResourceState {
                resource: info_for(&id, &data.catalog),
                claims: data
                    .claims
                    .iter()
                    .filter(|c| c.resource.id == id)
                    .cloned()
                    .collect(),
                foreign: data
                    .foreign
                    .iter()
                    .filter(|f| f.resource == id)
                    .cloned()
                    .collect(),
                seizing: data.seizing.iter().any(|z| z.resource == id),
                seizing_by: data
                    .seizing
                    .iter()
                    .find(|z| z.resource == id)
                    .map(|z| z.by_workspace.clone()),
                waiting,
            }
        })
        .collect();
    // The engine's own comparisons see 0; answers and events fill it (spec §6.2).
    ClaimsSnapshot {
        resources,
        as_of_seq: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::claims::ScreenCatalog;
    use crate::contract::claims::{Blocker, SeizeBegin, SeizeCommit};
    use crate::procs::RunGroup;
    use crate::procs::fake::FakeTable;

    const BOOT: &str = "b1";
    const MIN: i64 = 60_000;

    struct TestClock(AtomicI64);

    #[cfg(unix)]
    fn root_dev(root: &Path) -> Option<u64> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(root).ok().map(|m| m.dev())
    }

    #[cfg(not(unix))]
    fn root_dev(_root: &Path) -> Option<u64> {
        None
    }

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Devices(u32);

    impl ResourceCatalog for Devices {
        fn resources(&self) -> Vec<ResourceInfo> {
            (0..self.0)
                .map(|i| ResourceInfo {
                    id: dev(i),
                    kind: ResourceKind::Device,
                    display: format!("fake-{i}"),
                    attributes: BTreeMap::from([("test".to_owned(), "1".to_owned())]),
                    available: true,
                })
                .collect()
        }
    }

    fn dev(i: u32) -> ResourceId {
        ResourceId(format!("device:test:{i}"))
    }

    fn screen() -> ResourceId {
        ResourceId::screen()
    }

    struct World {
        engine: ClaimsEngine,
        table: Arc<FakeTable>,
        clock: Arc<TestClock>,
        dir: tempfile::TempDir,
        events: crate::service::events::TestEvents,
    }

    fn world(devices: u32) -> World {
        world_with(devices, Vec::new())
    }

    fn world_with(devices: u32, sources: Vec<Box<dyn ForeignClaimSource>>) -> World {
        let table = Arc::new(FakeTable::new(BOOT));
        let clock = Arc::new(TestClock(AtomicI64::new(100 * MIN)));
        let (tx, events) = crate::service::events::EventBus::channel(4096);
        let engine = ClaimsEngine::new(
            Store::open_in_memory().unwrap(),
            table.clone(),
            clock.clone(),
            vec![Box::new(ScreenCatalog), Box::new(Devices(devices))],
            sources,
            tx,
        );
        World {
            engine,
            table,
            clock,
            dir: tempfile::tempdir().unwrap(),
            events,
        }
    }

    impl World {
        fn advance(&self, ms: i64) {
            self.clock.0.fetch_add(ms, Ordering::SeqCst);
        }

        fn now(&self) -> i64 {
            self.clock.now_ms()
        }

        fn proc(&self, pid: u32, parent: Option<u32>) -> ProcessIdentity {
            self.table.insert(pid, parent, Some(u64::from(pid) * 10));
            ProcessIdentity {
                pid,
                boot_id: BOOT.into(),
                start_time: u64::from(pid) * 10,
            }
        }

        /// A checkout with its own project.
        fn workspace(&self, name: &str) -> String {
            let root = self.dir.path().join(name);
            std::fs::create_dir_all(root.join(".git")).unwrap();
            let common = display_path(&root.join(".git"));
            let project = self
                .engine
                .store
                .upsert_project(&common, name, self.now())
                .unwrap();
            self.checkout_of(name, &project.id)
        }

        /// A checkout (a worktree, say) of an existing project.
        fn checkout_of(&self, name: &str, project_id: &str) -> String {
            let root = self.dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            self.engine
                .store
                .upsert_workspace(
                    &id,
                    project_id,
                    &display_path(&root),
                    &display_path(&root.join(".git")),
                    self.now(),
                )
                .unwrap();
            // As a resolve records it (T5-R4).
            self.engine
                .store
                .set_workspace_root_dev(&id, root_dev(&root))
                .unwrap();
            id
        }

        fn project_of(&self, ws: &str) -> String {
            self.engine.store.workspace(ws).unwrap().unwrap().project_id
        }

        fn request(
            &self,
            ws: &str,
            requester: &ProcessIdentity,
            sel: Selector,
            purpose: Purpose,
        ) -> ClaimRequest {
            ClaimRequest {
                id: uuid::Uuid::new_v4().to_string(),
                selector: sel,
                workspace_id: ws.into(),
                requester: requester.clone(),
                purpose,
                reentrant_tokens: Vec::new(),
                no_wait: false,
            }
        }

        fn lease(&self, ws: &str, pid: u32, r: ResourceId) -> ClaimRequest {
            let me = self.proc(pid, None);
            self.request(
                ws,
                &me,
                Selector::Exact { resource: r },
                Purpose::Lease {
                    command: "make test".into(),
                },
            )
        }

        fn hold(&self, ws: &str, r: ResourceId, ttl_ms: Option<u64>) -> ClaimRequest {
            let cli = self.proc(900, None);
            self.request(
                ws,
                &cli,
                Selector::Exact { resource: r },
                Purpose::Hold {
                    ttl_ms,
                    bound_to: None,
                    distinct: false,
                },
            )
        }

        fn acquire(&mut self, r: &ClaimRequest) -> Result<ClaimOutcome, HostlerError> {
            self.engine.acquire(r.clone(), &no_heavy())
        }

        fn tick(&mut self) {
            self.engine.tick(&no_heavy());
        }

        fn live(&self, id: &str) -> Option<ClaimRow> {
            self.engine.store.claim(id).unwrap()
        }

        fn ended(&self, id: &str) -> Option<EndReason> {
            self.engine
                .store
                .history_item_of(id)
                .unwrap()
                .map(|i| i.reason)
        }

        fn state_of(&self, id: &str) -> ClaimOutcome {
            self.engine.known(id).unwrap().expect("known")
        }

        /// As the claim's own holder (a lease's wrapper); any process for an ended claim.
        fn release(&mut self, id: &str, ws: &str) -> ReleaseOutcome {
            let requester = self
                .live(id)
                .and_then(|row| row.holder)
                .unwrap_or_else(|| self.proc(900, None));
            self.engine
                .release(ClaimRelease {
                    claim_id: id.into(),
                    workspace_id: ws.into(),
                    requester,
                    exit_code: Some(0),
                })
                .unwrap()
        }

        fn drain_events(&mut self) -> Vec<Event> {
            let mut out = Vec::new();
            while let Ok(e) = self.events.try_recv() {
                out.push(e);
            }
            out
        }
    }

    fn granted(o: ClaimOutcome) -> Claim {
        match o {
            ClaimOutcome::Granted { claim } => claim,
            other => panic!("not granted: {other:?}"),
        }
    }

    fn joined(o: ClaimOutcome) -> Claim {
        match o {
            ClaimOutcome::Joined { claim } => claim,
            other => panic!("not joined: {other:?}"),
        }
    }

    fn is_queued(o: &ClaimOutcome) -> bool {
        matches!(o, ClaimOutcome::Queued { .. })
    }

    fn expires(row: &ClaimRow) -> i64 {
        match row.binding {
            Some(HoldBinding::Ttl { expires_at_ms, .. }) => expires_at_ms,
            ref other => panic!("no TTL: {other:?}"),
        }
    }

    // ---- queue basics ----

    /// T5-5: a run's lease is released only by its holder; a hold by any process of its
    /// workspace.
    #[test]
    fn release_of_a_lease_needs_its_holder() {
        let mut w = world(0);
        let a = w.workspace("a");
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let sibling = w.proc(101, None);
        let release = |w: &mut World, id: &str, requester: &ProcessIdentity| {
            w.engine.release(ClaimRelease {
                claim_id: id.into(),
                workspace_id: a.clone(),
                requester: requester.clone(),
                exit_code: None,
            })
        };
        let e = release(&mut w, &run.id, &sibling).unwrap_err();
        assert_eq!(e.code, ErrorCode::ClaimNotHeld);
        assert!(
            e.message.contains("is a run of another process (PID 100)"),
            "{}",
            e.message
        );
        assert!(w.live(&run.id).is_some());
        release(&mut w, &run.id, &run.requester).unwrap();
        assert_eq!(w.ended(&run.id), Some(EndReason::Released));
        // A hold: any process of the workspace (`hostler release` from another shell).
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        release(&mut w, &hold.id, &sibling).unwrap();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Released));
    }

    #[test]
    fn second_screen_lease_queues_then_gets_it_on_release() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let r1 = w.lease(&a, 100, screen());
        let r2 = w.lease(&b, 200, screen());
        granted(w.acquire(&r1).unwrap());
        match w.acquire(&r2).unwrap() {
            ClaimOutcome::Queued {
                request_id,
                position,
                of,
                blockers,
            } => {
                assert_eq!((request_id.as_str(), position, of), (r2.id.as_str(), 1, 1));
                assert!(
                    matches!(&blockers[..], [crate::contract::claims::Blocker::Claim { summary }] if summary.id == r1.id)
                );
            }
            other => panic!("{other:?}"),
        }
        w.release(&r1.id, &a);
        let claim = granted(w.state_of(&r2.id));
        assert_eq!(claim.workspace.id, b);
        assert_eq!(w.ended(&r1.id), Some(EndReason::Released));
    }

    #[test]
    fn acquire_same_id_twice_returns_same_state() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let r1 = w.lease(&a, 100, screen());
        let r2 = w.lease(&b, 200, screen());
        let first = granted(w.acquire(&r1).unwrap());
        assert_eq!(granted(w.acquire(&r1).unwrap()), first);
        assert!(is_queued(&w.acquire(&r2).unwrap()));
        assert!(is_queued(&w.acquire(&r2).unwrap()));
        assert_eq!(w.engine.store.claim_requests().unwrap().len(), 1);
    }

    #[test]
    fn finished_id_returns_finished() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        let r2 = w.lease(&b, 200, screen());
        w.acquire(&r2).unwrap();
        w.engine.withdraw(&r2.id, false).unwrap();
        match w.acquire(&r2).unwrap() {
            ClaimOutcome::Finished { item } => {
                assert_eq!(item.reason, EndReason::Withdrawn);
                assert_eq!(item.resource, "screen");
            }
            other => panic!("{other:?}"),
        }
    }

    // ---- re-entrancy ----

    /// L47 (FR-1): the heavy view the rules see names live identities only — a waiting entry
    /// whose holder died is left out (its pid may be a stranger's), a running one whose holder
    /// died stands for its live child, and one with neither is left out.
    #[test]
    fn the_heavy_view_keeps_live_identities_only() {
        let w = world(0);
        let live = w.proc(500, None);
        let dead = ProcessIdentity {
            pid: 600,
            boot_id: BOOT.into(),
            start_time: 1,
        };
        // 600 is somebody else now.
        w.proc(600, None);
        let child = w.proc(701, Some(1));
        let heavy = HeavyView {
            capacity: 2,
            running: vec![
                HeavyRunning {
                    id: "r-dead-holder".into(),
                    holder: dead.clone(),
                    child: Some(child.clone()),
                    group: None,
                    holder_ancestry: vec![600],
                },
                HeavyRunning {
                    id: "r-all-dead".into(),
                    holder: dead.clone(),
                    child: None,
                    group: None,
                    holder_ancestry: vec![600],
                },
            ],
            waiting: vec![("w-live".into(), vec![500]), ("w-dead".into(), vec![600])],
        };
        let holders = HashMap::from([("w-live".to_owned(), live), ("w-dead".to_owned(), dead)]);
        let table = w.table.clone();
        let view = Ancestry::new(table.as_ref()).heavy(&heavy, &holders);
        assert_eq!(view.running.len(), 1);
        assert_eq!(view.running[0].holder, child);
        assert_eq!(view.running[0].holder_ancestry[0], 701);
        let waiting: Vec<&str> = view.waiting.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(waiting, ["w-live"]);
    }

    /// L46 (FR-2): a re-entrant nested run under a process the service cannot open (an
    /// elevated shell on Windows) is still re-entrant: the chain goes through it.
    #[test]
    fn reentrant_through_an_unopenable_shell() {
        let mut w = world(0);
        let a = w.workspace("a");
        let outer = w.lease(&a, 100, screen());
        let claim = granted(w.acquire(&outer).unwrap());
        w.proc(150, Some(100));
        w.table.set_unopenable(150, true);
        let inner_process = w.proc(160, Some(150));
        let mut inner = w.request(
            &a,
            &inner_process,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "inner".into(),
            },
        );
        inner.reentrant_tokens = vec![claim.id.clone()];
        let got = w.acquire(&inner).unwrap();
        assert!(matches!(got, ClaimOutcome::Reentrant { .. }), "{got:?}");
    }

    #[test]
    fn reentrant_nested_lease_is_noop() {
        let mut w = world(0);
        let a = w.workspace("a");
        let outer = w.lease(&a, 100, screen());
        granted(w.acquire(&outer).unwrap());
        let child = w.proc(101, Some(100));
        w.engine
            .started(ClaimStarted {
                claim_id: outer.id.clone(),
                child: child.clone(),
                group: RunGroup::UnixGroup {
                    pgid: 101,
                    boot_id: BOOT.into(),
                },
            })
            .unwrap();
        let nested_proc = w.proc(102, Some(101));
        let mut nested = w.request(
            &a,
            &nested_proc,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "inner".into(),
            },
        );
        nested.reentrant_tokens = vec![outer.id.clone()];
        assert_eq!(
            w.acquire(&nested).unwrap(),
            ClaimOutcome::Reentrant {
                claim_id: outer.id.clone()
            }
        );
        assert_eq!(w.engine.store.claims().unwrap().len(), 1);
        assert!(w.engine.store.claim_requests().unwrap().is_empty());
    }

    #[test]
    fn reentrant_through_a_linked_lease() {
        let mut w = world(0);
        let a = w.workspace("a");
        granted(w.acquire(&w.hold(&a, screen(), None)).unwrap());
        let run = w.lease(&a, 100, screen());
        joined(w.acquire(&run).unwrap());
        let inner = w.proc(101, Some(100));
        let mut nested = w.request(
            &a,
            &inner,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "inner".into(),
            },
        );
        nested.reentrant_tokens = vec![run.id.clone()];
        assert_eq!(
            w.acquire(&nested).unwrap(),
            ClaimOutcome::Reentrant { claim_id: run.id }
        );
    }

    #[test]
    fn token_without_ancestor_is_ignored() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let outer = w.lease(&a, 100, screen());
        granted(w.acquire(&outer).unwrap());
        let mut stranger = w.lease(&b, 300, screen());
        stranger.reentrant_tokens = vec![outer.id.clone()];
        assert!(is_queued(&w.acquire(&stranger).unwrap()));
    }

    // ---- joining ----

    #[test]
    fn run_joins_own_hold_with_its_own_row_and_renews_it() {
        let mut w = world(0);
        let a = w.workspace("a");
        let hold = w.hold(&a, screen(), Some(10 * MIN as u64));
        granted(w.acquire(&hold).unwrap());
        w.advance(9 * MIN);
        let run = w.lease(&a, 100, screen());
        let claim = joined(w.acquire(&run).unwrap());
        assert_eq!(claim.parent.as_deref(), Some(hold.id.as_str()));
        assert_eq!(claim.kind, ClaimKind::Lease);
        assert_eq!(claim.holder.as_ref().map(|h| h.pid), Some(100));
        assert_eq!(w.engine.store.claims().unwrap().len(), 2);
        assert_eq!(expires(&w.live(&hold.id).unwrap()), w.now() + 10 * MIN);
    }

    #[test]
    fn second_device_run_of_a_workspace_joins() {
        let mut w = world(1);
        let a = w.workspace("a");
        let first = w.lease(&a, 100, dev(0));
        granted(w.acquire(&first).unwrap());
        let second = joined(w.acquire(&w.lease(&a, 200, dev(0))).unwrap());
        assert_eq!(second.parent.as_deref(), Some(first.id.as_str()));
    }

    #[test]
    fn device_in_use_counts_holders_and_pending_exact_requests() {
        let mut w = world(2);
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        assert_eq!(w.engine.device_in_use(&dev(0)), InUse::default());
        assert!(w.engine.device_in_use(&dev(0)).is_free());

        let hold = w.hold(&a, dev(0), None);
        granted(w.acquire(&hold).unwrap());
        let run = joined(w.acquire(&w.lease(&a, 100, dev(0))).unwrap());
        let in_use = w.engine.device_in_use(&dev(0));
        let ids: Vec<&str> = in_use.claims.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, [hold.id.as_str(), run.id.as_str()], "the root first");
        assert_eq!(in_use.claims[0], w.engine.holder_of(&dev(0)).unwrap());
        assert_eq!(in_use.waiting, 0);
        assert!(!in_use.is_free());

        // B waits for exactly dev 0, then takes dev 1 with `auto`; C's `auto` waits for either
        // device, so it may take another one and counts for neither.
        let exact = w.lease(&b, 200, dev(0));
        assert!(is_queued(&w.acquire(&exact).unwrap()));
        let any = |w: &World, ws: &str, pid: u32| {
            let me = w.proc(pid, None);
            w.request(
                ws,
                &me,
                Selector::Any {
                    kind: ResourceKind::Device,
                    attributes: BTreeMap::new(),
                },
                Purpose::Lease {
                    command: "make test".into(),
                },
            )
        };
        let b_auto = any(&w, &b, 300);
        let on_dev1 = granted(w.acquire(&b_auto).unwrap());
        assert_eq!(on_dev1.resource.id, dev(1));
        let c_auto = any(&w, &c, 400);
        assert!(is_queued(&w.acquire(&c_auto).unwrap()));
        let in_use = w.engine.device_in_use(&dev(0));
        assert_eq!((in_use.claims.len(), in_use.waiting), (2, 1));
        let in_use = w.engine.device_in_use(&dev(1));
        assert_eq!((in_use.claims.len(), in_use.waiting), (1, 0));

        // A releases: B's exact request goes first, C's `auto` still counts for nothing.
        w.release(&run.id, &a);
        w.release(&hold.id, &a);
        w.tick();
        granted(w.state_of(&exact.id));
        assert!(is_queued(&w.state_of(&c_auto.id)));
        let in_use = w.engine.device_in_use(&dev(0));
        assert_eq!(in_use.waiting, 0);
        let ids: Vec<&str> = in_use.claims.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, [exact.id.as_str()]);
    }

    // ---- P3b Task 9a: own sessions, the freshness gate, one end path ----

    /// A source that answers scripted reads and counts claim ends, as agent-device's will (9b):
    /// `collect` answers the current read whole; `foreign` answers it and then lets the next
    /// scripted read complete, the race P3-2 is about.
    #[derive(Clone, Default)]
    struct FakeSource(Arc<FakeState>);

    #[derive(Default)]
    struct FakeState {
        read: std::sync::Mutex<(Vec<ForeignClaim>, Option<Freshness>)>,
        next: std::sync::Mutex<Option<(Vec<ForeignClaim>, Freshness)>>,
        ends: std::sync::atomic::AtomicU64,
        ended: std::sync::Mutex<Vec<ResourceId>>,
        foreign_calls: std::sync::atomic::AtomicU32,
    }

    impl FakeSource {
        /// A source as at service start: one end counted, no good read yet.
        fn new() -> FakeSource {
            let fake = FakeSource::default();
            fake.0.ends.store(1, Ordering::SeqCst);
            fake.0.read.lock().unwrap().1 = Some(Freshness::NotYet);
            fake
        }

        /// A good read that started now (captured the current count) and lists `claims`.
        fn good_read(&self, claims: Vec<ForeignClaim>) {
            let seq = self.0.ends.load(Ordering::SeqCst);
            *self.0.read.lock().unwrap() = (claims, Some(Freshness::Since(seq)));
        }

        fn set(&self, claims: Vec<ForeignClaim>, freshness: Freshness) {
            *self.0.read.lock().unwrap() = (claims, Some(freshness));
        }

        fn ended(&self) -> Vec<ResourceId> {
            self.0.ended.lock().unwrap().clone()
        }
    }

    impl ForeignClaimSource for FakeSource {
        fn foreign(&self) -> Vec<ForeignClaim> {
            self.0
                .foreign_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let claims = self.0.read.lock().unwrap().0.clone();
            if let Some((next, freshness)) = self.0.next.lock().unwrap().take() {
                *self.0.read.lock().unwrap() = (next, Some(freshness));
            }
            claims
        }

        fn collect(&self) -> ForeignView {
            let read = self.0.read.lock().unwrap();
            ForeignView {
                claims: read.0.clone(),
                freshness: read.1.unwrap_or(Freshness::NotTracked),
                unreadable: None,
            }
        }

        fn claim_ended(&self, r: &ResourceId) -> u64 {
            self.0.ended.lock().unwrap().push(r.clone());
            self.0.ends.fetch_add(1, Ordering::SeqCst) + 1
        }
    }

    fn gated(devices: u32) -> (World, FakeSource) {
        let fake = FakeSource::new();
        (world_with(devices, vec![Box::new(fake.clone())]), fake)
    }

    /// A session as agent-device's source reports it: a path, never a workspace id.
    fn agent_session(r: ResourceId, path: &str, name: &str) -> ForeignClaim {
        ForeignClaim {
            resource: r,
            source: "agent-device".into(),
            description: format!("agent-device session {name}"),
            workspace_path: Some(path.to_owned()),
            since_ms: Some(1),
            stop: Some(serde_json::json!({ "session": name })),
            workspace_id: Some("set-by-the-source".into()),
        }
    }

    fn path_of(w: &World, ws: &str) -> String {
        w.engine.store.workspace(ws).unwrap().unwrap().path
    }

    fn checking(o: &ClaimOutcome) -> bool {
        matches!(o, ClaimOutcome::Queued { blockers, .. }
            if blockers.iter().any(|b| matches!(b, Blocker::Checking { .. })))
    }

    fn end_of(w: &World, r: &ResourceId) -> Option<DeviceEnd> {
        w.engine.device_ends.get(r).cloned()
    }

    #[test]
    fn foreign_claims_get_their_owning_workspace() {
        let (mut w, fake) = gated(1);
        let a = w.workspace("a");
        let inner = w.checkout_of("a/.worktrees/x", &w.project_of(&a));
        let b = w.workspace("b");
        let inner_path = path_of(&w, &inner);
        fake.good_read(vec![agent_session(
            dev(0),
            &format!("{inner_path}/e2e"),
            "s1",
        )]);
        w.tick();
        let snapshot = w.engine.snapshot().unwrap();
        let session = &snapshot.resources[1].foreign[0];
        assert_eq!(session.workspace_id.as_deref(), Some(inner.as_str()));
        // The nested worktree's own session lets it in; its outer checkout and B wait.
        assert!(is_queued(&w.acquire(&w.lease(&a, 100, dev(0))).unwrap()));
        assert!(is_queued(&w.acquire(&w.lease(&b, 200, dev(0))).unwrap()));
        granted(w.acquire(&w.lease(&inner, 300, dev(0))).unwrap());
        // A path no workspace owns blocks everyone, whatever the source said.
        fake.good_read(vec![agent_session(dev(0), "/elsewhere", "s2")]);
        w.tick();
        let snapshot = w.engine.snapshot().unwrap();
        assert_eq!(snapshot.resources[1].foreign[0].workspace_id, None);
        assert_eq!(
            fake.0.foreign_calls.load(Ordering::SeqCst),
            0,
            "collect only"
        );
    }

    #[test]
    fn startup_sets_every_device_end_seq_to_one() {
        let (mut w, fake) = gated(2);
        let a = w.workspace("a");
        // No good read yet: no device for anyone; the screen is not gated.
        let first = w.lease(&a, 100, dev(0));
        assert!(checking(&w.acquire(&first).unwrap()));
        granted(w.acquire(&w.lease(&a, 101, screen())).unwrap());
        // A read that captured 0 (impossible after start, ends starts at 1) still closes them.
        fake.set(Vec::new(), Freshness::Since(0));
        w.tick();
        assert!(is_queued(&w.state_of(&first.id)));
        assert!(
            w.engine.device_ends.is_empty(),
            "start is the default, not a row"
        );
        // The first good read captured 1: every device opens.
        fake.good_read(Vec::new());
        w.tick();
        granted(w.state_of(&first.id));
        granted(w.acquire(&w.lease(&a, 102, dev(1))).unwrap());
    }

    #[test]
    fn freshness_gate_opens_after_a_good_read_that_started_after_the_end() {
        let (mut w, fake) = gated(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        let run = w.lease(&a, 100, dev(0));
        granted(w.acquire(&run).unwrap());
        w.release(&run.id, &a);
        assert_eq!(end_of(&w, &dev(0)).unwrap().seq, 2);
        // B waits: the last good read started before the end.
        let theirs = w.lease(&b, 200, dev(0));
        let queued = w.acquire(&theirs).unwrap();
        assert!(checking(&queued), "{queued:?}");
        // A comes straight back (L50, L52).
        granted(w.acquire(&w.lease(&a, 101, dev(0))).unwrap());
        w.release(
            &w.engine
                .holder_of(&dev(0))
                .map(|c| c.id)
                .unwrap_or_default(),
            &a,
        );
        // A failed read changes nothing; a good one that started after both ends opens it.
        w.tick();
        assert!(is_queued(&w.state_of(&theirs.id)));
        fake.good_read(Vec::new());
        w.tick();
        granted(w.state_of(&theirs.id));
    }

    /// P3-2: the engine takes a source's claims and freshness from one `collect`. A read that
    /// completes between two accessors would pair the old claims with the new freshness.
    #[test]
    fn sessions_and_freshness_come_from_one_read() {
        let (mut w, fake) = gated(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        let run = w.lease(&a, 100, dev(0));
        granted(w.acquire(&run).unwrap());
        w.release(&run.id, &a);
        // A read in flight (started before the end) lists nothing; the next one, completing
        // during a collection, lists the session A's run opened.
        let a_path = path_of(&w, &a);
        let seq = fake.0.ends.load(Ordering::SeqCst);
        *fake.0.next.lock().unwrap() = Some((
            vec![agent_session(dev(0), &a_path, "left-open")],
            Freshness::Since(seq),
        ));
        let theirs = w.lease(&b, 200, dev(0));
        assert!(checking(&w.acquire(&theirs).unwrap()));
        w.tick();
        assert!(is_queued(&w.state_of(&theirs.id)));
        // The new read whole: fresh, and A's session keeps B out.
        fake.set(
            vec![agent_session(dev(0), &a_path, "left-open")],
            Freshness::Since(seq),
        );
        w.tick();
        let ClaimOutcome::Queued { blockers, .. } = w.state_of(&theirs.id) else {
            panic!("B must wait");
        };
        assert!(matches!(blockers.as_slice(), [Blocker::Foreign { .. }]));
        assert_eq!(fake.0.foreign_calls.load(Ordering::SeqCst), 0);
    }

    /// P3-3: the gate counts ends, never compares clocks.
    #[test]
    fn gate_holds_across_a_backward_clock_step() {
        let (mut w, fake) = gated(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        let run = w.lease(&a, 100, dev(0));
        granted(w.acquire(&run).unwrap());
        // A read starts (captures the count), the clock steps back an hour, then the run ends.
        let captured = fake.0.ends.load(Ordering::SeqCst);
        w.clock.0.fetch_sub(60 * MIN, Ordering::SeqCst);
        w.release(&run.id, &a);
        // The read completes: it started before the end, so B still waits.
        fake.set(Vec::new(), Freshness::Since(captured));
        let theirs = w.lease(&b, 200, dev(0));
        assert!(checking(&w.acquire(&theirs).unwrap()));
        // Forward again, far: time alone opens nothing.
        w.advance(10 * 60 * MIN);
        w.tick();
        assert!(is_queued(&w.state_of(&theirs.id)));
        fake.good_read(Vec::new());
        w.tick();
        granted(w.state_of(&theirs.id));
    }

    /// P3-16: every way a device claim ends records the device's end, once per device per end.
    #[test]
    fn every_end_path_records_the_device_end() {
        let (mut w, fake) = gated(7);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        let mut expected = Vec::new();
        // A seizure's end has no ending workspace (P3bT9a-2).
        let mut check = |w: &World, fake: &FakeSource, d: u32, ws: Option<&str>, what: &str| {
            expected.push(dev(d));
            assert_eq!(fake.ended(), expected, "{what}");
            let end = end_of(w, &dev(d)).unwrap_or_else(|| panic!("{what}: no end"));
            assert_eq!(end.seq, fake.0.ends.load(Ordering::SeqCst), "{what}");
            assert_eq!(end.workspace_id.as_deref(), ws, "{what}");
            assert!(!end.forgotten);
        };

        // release
        let run = w.lease(&a, 100, dev(0));
        granted(w.acquire(&run).unwrap());
        w.release(&run.id, &a);
        check(&w, &fake, 0, Some(&a), "release");

        // force_release
        let hold = w.hold(&a, dev(1), None);
        granted(w.acquire(&hold).unwrap());
        let by = w.proc(801, None);
        w.engine.force_release(&hold.id, "fake-1", &b, &by).unwrap();
        check(&w, &fake, 1, None, "force_release");

        // unreserve
        w.engine.reserve(&dev(2), &a).unwrap();
        w.engine.unreserve(&dev(2), &a).unwrap();
        check(&w, &fake, 2, Some(&a), "unreserve");

        // reap: the run's holder dies.
        let run = w.lease(&a, 300, dev(3));
        granted(w.acquire(&run).unwrap());
        w.table.remove(300);
        w.tick();
        assert_eq!(w.ended(&run.id), Some(EndReason::Reaped));
        check(&w, &fake, 3, Some(&a), "reap");

        // expiry
        let hold = w.hold(&a, dev(4), Some(MIN as u64));
        granted(w.acquire(&hold).unwrap());
        w.advance(MIN);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Expired));
        check(&w, &fake, 4, Some(&a), "expiry");

        // seize commit: a TTL hold has nothing to stop.
        let hold = w.hold(&a, dev(5), None);
        granted(w.acquire(&hold).unwrap());
        let taker = w.lease(&b, 500, dev(5));
        let plan = w
            .engine
            .seize_begin(
                SeizeBegin {
                    resource: dev(5),
                    confirmation: "fake-5".into(),
                    request: taker.clone(),
                },
                &no_heavy(),
            )
            .unwrap();
        let outcome = w
            .engine
            .seize_commit(
                SeizeCommit {
                    plan_id: plan.plan_id,
                    requester: taker.requester.clone(),
                    evidence: Vec::new(),
                },
                &no_heavy(),
            )
            .unwrap();
        granted(outcome.outcome);
        assert_eq!(w.ended(&hold.id), Some(EndReason::Seized));
        check(&w, &fake, 5, None, "seize commit");

        // A linked cascade: one end for the device, however many claims.
        let hold = w.hold(&a, dev(6), None);
        granted(w.acquire(&hold).unwrap());
        let run = joined(w.acquire(&w.lease(&a, 600, dev(6))).unwrap());
        w.engine.force_release(&hold.id, "fake-6", &b, &by).unwrap();
        assert_eq!(w.ended(&run.id), Some(EndReason::Seized));
        check(&w, &fake, 6, None, "linked cascade");

        // Ending a screen claim or a request tells no source.
        let screen_run = w.lease(&a, 700, screen());
        granted(w.acquire(&screen_run).unwrap());
        w.release(&screen_run.id, &a);
        let waiting = w.lease(&b, 701, dev(0));
        let busy = w.lease(&a, 702, dev(0));
        granted(w.acquire(&busy).unwrap());
        assert!(is_queued(&w.acquire(&waiting).unwrap()));
        w.engine.withdraw(&waiting.id, false).unwrap();
        assert_eq!(fake.ended(), expected);
    }

    /// P3bT9a-1: an end of A's own (a reservation it makes while the gate keeps it out) never
    /// drops B's unverified end: A stays out until a good read that started after B's end.
    #[test]
    fn reservation_end_keeps_another_workspaces_unverified_end() {
        let (mut w, fake) = gated(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        let run = w.lease(&b, 100, dev(0));
        granted(w.acquire(&run).unwrap());
        w.release(&run.id, &b);
        let mine = w.lease(&a, 200, dev(0));
        assert!(checking(&w.acquire(&mine).unwrap()));
        w.engine.reserve(&dev(0), &a).unwrap();
        w.engine.unreserve(&dev(0), &a).unwrap();
        assert_eq!(end_of(&w, &dev(0)).unwrap().workspace_id, None);
        w.tick();
        assert!(
            checking(&w.state_of(&mine.id)),
            "{:?}",
            w.state_of(&mine.id)
        );
        fake.good_read(Vec::new());
        w.tick();
        granted(w.state_of(&mine.id));
        // Once B's end is verified, A's own end lets A straight back again.
        w.release(&mine.id, &a);
        assert_eq!(
            end_of(&w, &dev(0)).unwrap().workspace_id.as_deref(),
            Some(a.as_str())
        );
        granted(w.acquire(&w.lease(&a, 201, dev(0))).unwrap());
    }

    /// P3bT9a-1, a seizure: the taker that releases before any read is not exempt from the
    /// victim's unverified end, and a third workspace stays out too.
    #[test]
    fn seized_then_released_before_a_read_keeps_the_device_closed() {
        let (mut w, fake) = gated(1);
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        fake.good_read(Vec::new());
        let hold = w.hold(&a, dev(0), None);
        granted(w.acquire(&hold).unwrap());
        let taker = w.lease(&b, 500, dev(0));
        let plan = w
            .engine
            .seize_begin(
                SeizeBegin {
                    resource: dev(0),
                    confirmation: "fake-0".into(),
                    request: taker.clone(),
                },
                &no_heavy(),
            )
            .unwrap();
        let outcome = w
            .engine
            .seize_commit(
                SeizeCommit {
                    plan_id: plan.plan_id,
                    requester: taker.requester.clone(),
                    evidence: Vec::new(),
                },
                &no_heavy(),
            )
            .unwrap();
        granted(outcome.outcome);
        w.release(&taker.id, &b);
        assert_eq!(end_of(&w, &dev(0)).unwrap().workspace_id, None);
        let again = w.lease(&b, 501, dev(0));
        assert!(checking(&w.acquire(&again).unwrap()));
        let third = w.lease(&c, 600, dev(0));
        assert!(checking(&w.acquire(&third).unwrap()));
        fake.good_read(Vec::new());
        w.tick();
        granted(w.state_of(&again.id));
        assert!(is_queued(&w.state_of(&third.id)));
    }

    /// P3bT9a-2: after `release --force`, the victim waits for the read like the forcer, and
    /// the forcer, first in line, gets the device.
    #[test]
    fn forced_release_victim_does_not_jump_the_forcer() {
        let (mut w, fake) = gated(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        let hold = w.hold(&a, dev(0), None);
        granted(w.acquire(&hold).unwrap());
        let theirs = w.lease(&b, 200, dev(0));
        assert!(is_queued(&w.acquire(&theirs).unwrap()));
        let by = w.proc(801, None);
        w.engine.force_release(&hold.id, "fake-0", &b, &by).unwrap();
        w.tick();
        assert!(
            checking(&w.state_of(&theirs.id)),
            "{:?}",
            w.state_of(&theirs.id)
        );
        let again = w.lease(&a, 100, dev(0));
        assert!(checking(&w.acquire(&again).unwrap()));
        fake.good_read(Vec::new());
        w.tick();
        granted(w.state_of(&theirs.id));
        assert!(is_queued(&w.state_of(&again.id)));
    }

    #[test]
    fn forget_device_end_opens_only_that_device() {
        let (mut w, fake) = gated(2);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        fake.good_read(Vec::new());
        for (pid, d) in [(100, 0), (101, 1)] {
            let run = w.lease(&a, pid, dev(d));
            granted(w.acquire(&run).unwrap());
            w.release(&run.id, &a);
        }
        let wait0 = w.lease(&b, 200, dev(0));
        let wait1 = w.lease(&b, 201, dev(1));
        assert!(checking(&w.acquire(&wait0).unwrap()));
        assert!(checking(&w.acquire(&wait1).unwrap()));
        let seq = end_of(&w, &dev(0)).unwrap().seq;
        assert_eq!(
            w.engine.forget_device_end(&dev(0)).unwrap(),
            ForgetOutcome::Forgotten { seq }
        );
        // Granted at once, without a tick; the other device stays closed.
        granted(w.state_of(&wait0.id));
        assert!(checking(&w.state_of(&wait1.id)));
        // Forgetting again, or an open device, has nothing to forget.
        assert_eq!(
            w.engine.forget_device_end(&dev(0)).unwrap(),
            ForgetOutcome::NotClosed
        );
        // Not a device.
        assert_eq!(
            w.engine.forget_device_end(&screen()).unwrap_err().code,
            ErrorCode::UnknownResource
        );
        // The next end applies the rules again. It is B's own and the forgotten end is not
        // merged into it (P3bT9a-1): B comes straight back, A waits for a read.
        w.release(&wait0.id, &b);
        let back = w.lease(&b, 202, dev(0));
        granted(w.acquire(&back).unwrap());
        w.release(&back.id, &b);
        let again = w.lease(&a, 300, dev(0));
        assert!(checking(&w.acquire(&again).unwrap()));
        // Before the first good read too (L54: agent-device unreadable since the start).
        let (mut w, _fake) = gated(1);
        let a = w.workspace("a");
        let first = w.lease(&a, 100, dev(0));
        assert!(checking(&w.acquire(&first).unwrap()));
        assert_eq!(
            w.engine.forget_device_end(&dev(0)).unwrap(),
            ForgetOutcome::Forgotten { seq: 1 }
        );
        granted(w.state_of(&first.id));
    }

    /// P2-6, L16: a distinct hold is never re-entrant; it takes another device.
    #[test]
    fn distinct_re_entrancy_is_skipped() {
        let mut w = world(2);
        let a = w.workspace("a");
        let outer = w.lease(&a, 100, dev(0));
        granted(w.acquire(&outer).unwrap());
        start(&mut w, &outer.id, 101, 100);
        let inner = w.proc(102, Some(101));
        let mut slot = w.request(
            &a,
            &inner,
            Selector::Any {
                kind: ResourceKind::Device,
                attributes: BTreeMap::new(),
            },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: None,
                distinct: true,
            },
        );
        slot.reentrant_tokens = vec![outer.id.clone()];
        let claim = granted(w.acquire(&slot).unwrap());
        assert_eq!(claim.resource.id, dev(1));
        assert_eq!(claim.parent, None);
    }

    /// The run inside a device run stays re-entrant: it is a lease, not a distinct hold.
    #[test]
    fn nested_run_device_stays_re_entrant() {
        let mut w = world(2);
        let a = w.workspace("a");
        let outer = w.lease(&a, 100, dev(0));
        granted(w.acquire(&outer).unwrap());
        start(&mut w, &outer.id, 101, 100);
        let inner = w.proc(102, Some(101));
        for sel in [
            Selector::Exact { resource: dev(0) },
            Selector::Any {
                kind: ResourceKind::Device,
                attributes: BTreeMap::new(),
            },
        ] {
            let mut nested = w.request(
                &a,
                &inner,
                sel,
                Purpose::Lease {
                    command: "inner".into(),
                },
            );
            nested.reentrant_tokens = vec![outer.id.clone()];
            assert_eq!(
                w.acquire(&nested).unwrap(),
                ClaimOutcome::Reentrant {
                    claim_id: outer.id.clone()
                }
            );
        }
    }

    /// P3-12: on the only device, a distinct hold inside the run that holds it can never be
    /// granted: refused at once as a deadlock, not left to time out.
    #[test]
    fn distinct_hold_inside_a_run_on_the_only_device_is_refused_as_deadlock() {
        let mut w = world(1);
        let a = w.workspace("a");
        let outer = w.lease(&a, 100, dev(0));
        granted(w.acquire(&outer).unwrap());
        start(&mut w, &outer.id, 101, 100);
        let inner = w.proc(102, Some(101));
        let mut slot = w.request(
            &a,
            &inner,
            Selector::Any {
                kind: ResourceKind::Device,
                attributes: BTreeMap::new(),
            },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: None,
                distinct: true,
            },
        );
        slot.reentrant_tokens = vec![outer.id.clone()];
        let e = w.acquire(&slot).unwrap_err();
        assert_eq!(e.code, ErrorCode::ClaimDeadlock, "{}", e.message);
        assert!(w.engine.store.claim_requests().unwrap().is_empty());
    }

    /// P3bT1-1: a takeover in flight keeps the device in use, also once the holder released it
    /// and when its taker never queued.
    #[test]
    fn device_in_use_counts_a_takeover_in_flight() {
        let mut w = world(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.hold(&a, dev(0), None);
        granted(w.acquire(&hold).unwrap());
        // B's taker request was never acquired.
        let taker = w.lease(&b, 500, dev(0));
        let plan = w
            .engine
            .seize_begin(
                SeizeBegin {
                    resource: dev(0),
                    confirmation: "fake-0".into(),
                    request: taker,
                },
                &no_heavy(),
            )
            .unwrap();
        assert!(w.engine.store.claim_requests().unwrap().is_empty());
        w.release(&hold.id, &a);
        let in_use = w.engine.device_in_use(&dev(0));
        assert_eq!(
            in_use,
            InUse {
                claims: Vec::new(),
                waiting: 1,
            }
        );
        assert!(!in_use.is_free());
        w.engine.seize_abort(&plan.plan_id).unwrap();
        assert!(w.engine.device_in_use(&dev(0)).is_free());
    }

    /// P3bT1-2: a store that cannot be read counts as in use: nothing is removed or shut down on
    /// a guess.
    #[test]
    fn device_in_use_fails_closed_when_the_store_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        let (tx, _events) = crate::service::events::EventBus::channel(16);
        let engine = ClaimsEngine::new(
            Store::open(&path).unwrap(),
            Arc::new(FakeTable::new(BOOT)),
            Arc::new(TestClock(AtomicI64::new(100 * MIN))),
            vec![Box::new(Devices(1))],
            Vec::new(),
            tx,
        );
        assert!(engine.device_in_use(&dev(0)).is_free());
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE claims;")
            .unwrap();
        let in_use = engine.device_in_use(&dev(0));
        assert_eq!(
            in_use,
            InUse {
                claims: Vec::new(),
                waiting: 1,
            }
        );
        assert!(!in_use.is_free());
    }

    #[test]
    fn second_screen_run_of_a_workspace_queues() {
        let mut w = world(0);
        let a = w.workspace("a");
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        assert!(is_queued(&w.acquire(&w.lease(&a, 200, screen())).unwrap()));
    }

    /// Task 4 handoff: a request that queued before its workspace got the resource joins once
    /// it has it, instead of waiting behind its sibling.
    #[test]
    fn queued_request_joins_once_its_workspace_gets_the_resource() {
        let mut w = world(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let theirs = w.lease(&a, 100, dev(0));
        granted(w.acquire(&theirs).unwrap());
        let first = w.lease(&b, 200, dev(0));
        let second = w.lease(&b, 300, dev(0));
        assert!(is_queued(&w.acquire(&first).unwrap()));
        assert!(is_queued(&w.acquire(&second).unwrap()));
        w.release(&theirs.id, &a);
        granted(w.state_of(&first.id));
        w.tick();
        let claim = joined(w.state_of(&second.id));
        assert_eq!(claim.parent.as_deref(), Some(first.id.as_str()));
    }

    // ---- holds and leases ----

    #[test]
    fn hold_expires_and_next_waiter_is_granted() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.hold(&a, screen(), Some(MIN as u64));
        granted(w.acquire(&hold).unwrap());
        let waiter = w.lease(&b, 200, screen());
        assert!(is_queued(&w.acquire(&waiter).unwrap()));
        w.advance(MIN + 1_000);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Expired));
        granted(w.state_of(&waiter.id));
    }

    /// T4-2: a TTL hold never expires under a live linked lease, and expires in the very tick its
    /// last linked lease is reaped.
    #[test]
    fn hold_never_expires_under_a_linked_lease() {
        let mut w = world(0);
        let a = w.workspace("a");
        let hold = w.hold(&a, screen(), Some(MIN as u64));
        granted(w.acquire(&hold).unwrap());
        let run = w.lease(&a, 100, screen());
        joined(w.acquire(&run).unwrap());
        w.advance(5 * MIN);
        w.tick();
        assert!(w.live(&hold.id).is_some());
        w.table.remove(100);
        w.tick();
        assert_eq!(w.ended(&run.id), Some(EndReason::Reaped));
        assert_eq!(w.ended(&hold.id), Some(EndReason::Expired));
    }

    #[test]
    fn renew_extends() {
        let mut w = world(0);
        let a = w.workspace("a");
        let hold = w.hold(&a, screen(), Some(10 * MIN as u64));
        granted(w.acquire(&hold).unwrap());
        w.advance(5 * MIN);
        let claim = w.engine.renew(&hold.id, &a).unwrap();
        assert_eq!(
            claim.binding,
            Some(HoldBinding::Ttl {
                ttl_ms: 10 * MIN as u64,
                expires_at_ms: w.now() + 10 * MIN
            })
        );
        let other = w.workspace("b");
        assert_eq!(
            w.engine.renew(&hold.id, &other).unwrap_err().code,
            ErrorCode::ClaimNotHeld
        );
    }

    #[test]
    fn process_bound_hold_ends_with_its_process() {
        let mut w = world(0);
        let a = w.workspace("a");
        let parent = w.proc(500, None);
        let cli = w.proc(501, Some(500));
        let hold = w.request(
            &a,
            &cli,
            Selector::Exact { resource: screen() },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: Some(parent),
                distinct: false,
            },
        );
        let claim = granted(w.acquire(&hold).unwrap());
        assert!(matches!(claim.binding, Some(HoldBinding::Process { .. })));
        w.advance(48 * 60 * MIN);
        w.tick();
        assert!(w.live(&hold.id).is_some(), "no TTL");
        w.table.remove(500);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Reaped));
    }

    fn start(w: &mut World, claim_id: &str, child_pid: u32, parent: u32) -> RunGroup {
        let child = w.proc(child_pid, Some(parent));
        let group = RunGroup::UnixGroup {
            pgid: child_pid,
            boot_id: BOOT.into(),
        };
        w.engine
            .started(ClaimStarted {
                claim_id: claim_id.into(),
                child,
                group: group.clone(),
            })
            .unwrap();
        group
    }

    #[test]
    fn dead_holder_lease_reaped_group_survivor_keeps_it() {
        let mut w = world(0);
        let a = w.workspace("a");
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let group = start(&mut w, &run.id, 101, 100);
        w.table.set_group_alive(&group, true);
        w.table.remove(100);
        w.table.remove(101);
        w.tick();
        assert!(w.live(&run.id).is_some(), "a group survivor keeps it");
        w.table.set_group_alive(&group, false);
        w.tick();
        assert_eq!(w.ended(&run.id), Some(EndReason::Reaped));
    }

    #[test]
    fn explicit_release_frees_despite_group_survivors() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let group = start(&mut w, &run.id, 101, 100);
        w.table.set_group_alive(&group, true);
        let waiter = w.lease(&b, 200, screen());
        assert!(is_queued(&w.acquire(&waiter).unwrap()));
        w.release(&run.id, &a);
        granted(w.state_of(&waiter.id));
    }

    // ---- reaping and admission ----

    #[test]
    fn dead_waiter_reaped_before_its_turn() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let waiter_cli = w.proc(200, None);
        let waiter = w.request(
            &b,
            &waiter_cli,
            Selector::Exact { resource: screen() },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: None,
                distinct: false,
            },
        );
        assert!(is_queued(&w.acquire(&waiter).unwrap()));
        w.table.remove(200);
        w.release(&run.id, &a);
        assert_eq!(w.ended(&waiter.id), Some(EndReason::Reaped));
        assert!(w.live(&waiter.id).is_none());
        assert!(w.engine.store.claims().unwrap().is_empty());
    }

    #[test]
    fn stranded_hold_reaped_when_path_gone() {
        let mut w = world(0);
        let a = w.workspace("a");
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        std::fs::remove_dir_all(w.dir.path().join("a")).unwrap();
        // The tick never probes the filesystem itself: unknown until a probe saw it (T5-1).
        w.tick();
        assert_eq!(w.ended(&hold.id), None);
        w.engine.path_probe().probe_now();
        w.tick();
        // L41: gone for 2 minutes in a row first.
        assert_eq!(w.ended(&hold.id), None);
        w.advance(PATH_GONE_MS);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Stranded));
    }

    /// L41: a TTL hold whose path has been gone for less than 2 minutes is kept; a path that comes
    /// back starts the count again.
    #[test]
    fn a_gone_path_strands_only_after_two_minutes() {
        let mut w = world(0);
        let a = w.workspace("a");
        let hold = w.hold(&a, screen(), Some(60 * MIN as u64));
        granted(w.acquire(&hold).unwrap());
        let root = w.dir.path().join("a");
        let moved = w.dir.path().join("a-moved");
        std::fs::rename(&root, &moved).unwrap();
        w.tick();
        w.engine.path_probe().probe_now();
        w.tick();
        w.advance(PATH_GONE_MS - 1);
        w.tick();
        assert_eq!(w.ended(&hold.id), None);
        // Back in place: the count starts again.
        std::fs::rename(&moved, &root).unwrap();
        w.engine.path_probe().probe_now();
        w.tick();
        std::fs::rename(&root, &moved).unwrap();
        w.engine.path_probe().probe_now();
        w.tick();
        w.advance(PATH_GONE_MS - 1);
        w.tick();
        assert_eq!(w.ended(&hold.id), None);
        w.advance(1);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Stranded));
    }

    /// L41: a process-bound hold whose process lives is never stranded by its path (a moved
    /// checkout), however long the path is gone.
    #[test]
    fn a_moved_checkout_keeps_its_live_bound_hold() {
        let mut w = world(0);
        let a = w.workspace("a");
        let parent = w.proc(500, None);
        let cli = w.proc(501, Some(500));
        let hold = w.request(
            &a,
            &cli,
            Selector::Exact { resource: screen() },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: Some(parent),
                distinct: false,
            },
        );
        granted(w.acquire(&hold).unwrap());
        std::fs::rename(w.dir.path().join("a"), w.dir.path().join("a-moved")).unwrap();
        w.tick();
        w.engine.path_probe().probe_now();
        for _ in 0..3 {
            w.advance(PATH_GONE_MS);
            w.tick();
        }
        assert!(w.live(&hold.id).is_some());
        // It ends with its process, as ever.
        w.table.remove(500);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Reaped));
    }

    /// T5-R4: "gone" needs the parent on the checkout root's recorded device; a parent on
    /// another one (a Linux mount point left as an empty directory) is unknown.
    #[cfg(unix)]
    #[test]
    fn a_parent_on_another_device_strands_nothing() {
        let mut w = world(0);
        let a = w.workspace("mnt/a");
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        let row = w.engine.store.workspace(&a).unwrap().unwrap();
        let dev = row.root_dev.unwrap();
        // As if `a` had been on its own mount, now unmounted.
        w.engine
            .store
            .set_workspace_root_dev(&a, Some(dev + 1))
            .unwrap();
        std::fs::remove_dir_all(w.dir.path().join("mnt/a")).unwrap();
        w.tick();
        w.engine.path_probe().probe_now();
        w.tick();
        assert_eq!(w.ended(&hold.id), None);
        assert_eq!(
            w.engine.path_probe().state(&row.path, Some(dev + 1)),
            PathState::Unknown
        );
        // On its own device the same removal is gone.
        assert_eq!(probe_path(&row.path, Some(dev)), PathState::Gone);
    }

    /// T6-R2-5: a row without a recorded device follows the parent rule (gone when its parent
    /// exists); rows of several devices at one path keep their own probe entries.
    #[test]
    fn no_recorded_device_strands_by_the_parent_rule() {
        let mut w = world(0);
        let a = w.workspace("a");
        w.engine.store.set_workspace_root_dev(&a, None).unwrap();
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        std::fs::remove_dir_all(w.dir.path().join("a")).unwrap();
        w.tick();
        w.engine.path_probe().probe_now();
        w.tick();
        w.advance(PATH_GONE_MS);
        w.tick();
        assert_eq!(w.ended(&hold.id), Some(EndReason::Stranded));

        let probe = PathProbe::default();
        let gone = w.dir.path().join("b-gone");
        let path = display_path(&gone);
        let dev = root_dev(w.dir.path());
        let other = dev.map(|d| d + 1);
        probe.state(&path, dev);
        probe.state(&path, other);
        probe.probe_now();
        for _ in 0..3 {
            assert_eq!(probe.state(&path, dev), PathState::Gone);
            #[cfg(unix)]
            assert_eq!(probe.state(&path, other), PathState::Unknown);
        }
    }

    /// T5-R3: while a filesystem call for a checkout hangs, the next one for it fails at once
    /// instead of adding a thread; other checkouts are not affected.
    #[tokio::test]
    async fn a_hanging_checkout_call_is_not_joined_by_more() {
        use std::sync::atomic::AtomicUsize;

        let key = format!("/test/hang-{}", uuid::Uuid::new_v4());
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let first = off_lock(&key, std::time::Duration::from_millis(50), move || {
            let _ = wait.recv();
        })
        .await;
        assert!(first.is_none(), "timed out");
        let ran = Arc::new(AtomicUsize::new(0));
        let counted = ran.clone();
        let started = std::time::Instant::now();
        let second = off_lock(&key, std::time::Duration::from_secs(5), move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert!(second.is_none());
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(bounded(&key, std::time::Duration::from_secs(5), || 1), None);
        assert_eq!(
            off_lock(
                &format!("{key}-other"),
                std::time::Duration::from_secs(5),
                || 2
            )
            .await,
            Some(2)
        );
        // Once the hung call returns, the checkout is served again.
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(v) = off_lock(&key, std::time::Duration::from_secs(5), || 3).await {
                assert_eq!(v, 3);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// T6-R2-2: healthy calls for one checkout run side by side; only a hung one refuses.
    #[tokio::test]
    async fn overlapping_calls_for_one_checkout_both_answer() {
        let key = format!("/test/both-{}", uuid::Uuid::new_v4());
        let slow = |v: u32| {
            move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                v
            }
        };
        let limit = std::time::Duration::from_secs(5);
        let (one, two) = tokio::join!(
            off_lock(&key, limit, slow(1)),
            off_lock(&key, limit, slow(2))
        );
        assert_eq!((one, two), (Some(1), Some(2)));
        let k2 = key.clone();
        let other = std::thread::spawn(move || bounded(&k2, limit, slow(3)));
        assert_eq!(bounded(&key, limit, slow(4)), Some(4));
        assert_eq!(other.join().unwrap(), Some(3));
    }

    /// T6-R2-2: a panicking call never leaves its checkout refused.
    #[tokio::test]
    async fn a_panicking_call_does_not_block_the_next() {
        let key = format!("/test/panic-{}", uuid::Uuid::new_v4());
        let limit = std::time::Duration::from_secs(5);
        let panicked: Option<u32> = off_lock(&key, limit, || panic!("a test panic")).await;
        assert_eq!(panicked, None);
        assert_eq!(off_lock(&key, limit, || 7).await, Some(7));
        // Also when it panics after its waiter gave up.
        let late: Option<u32> = off_lock(&key, std::time::Duration::from_millis(20), || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            panic!("a late test panic")
        })
        .await;
        assert_eq!(late, None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while off_lock(&key, limit, || 8).await != Some(8) {
            assert!(std::time::Instant::now() < deadline, "the key stayed hung");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// L31: the engine holds a handle on a run's group from `claims.started` until the claim
    /// ends.
    #[test]
    fn run_groups_are_held_while_their_claims_live() {
        let mut w = world(0);
        let a = w.workspace("a");
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let child = w.proc(101, Some(100));
        let group = RunGroup::WindowsJob {
            name: "Local\\hostler-heavy-r".into(),
            boot_id: BOOT.into(),
            session: Some(1),
        };
        w.engine
            .started(ClaimStarted {
                claim_id: run.id.clone(),
                child,
                group: group.clone(),
            })
            .unwrap();
        assert_eq!(w.table.held("claims"), vec![group.clone()]);
        w.tick();
        assert_eq!(w.table.held("claims"), vec![group]);
        w.release(&run.id, &a);
        assert!(w.table.held("claims").is_empty());
    }

    /// T5-2: a workspace whose parent directory is gone too (an unmounted volume, an offline
    /// share) is unknown, not gone: its hold and reservation stay.
    #[test]
    fn unmounted_volume_strands_nothing() {
        let mut w = world(1);
        let a = w.workspace("volume/a");
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        let reservation = w.engine.reserve(&dev(0), &a).unwrap();
        std::fs::remove_dir_all(w.dir.path().join("volume")).unwrap();
        w.tick();
        w.engine.path_probe().probe_now();
        w.tick();
        assert_eq!(w.ended(&hold.id), None);
        let row = w.engine.store.workspace(&a).unwrap().unwrap();
        assert_eq!(
            w.engine.path_probe().state(&row.path, row.root_dev),
            PathState::Unknown
        );
        assert!(w.live(&reservation.id).is_some());
    }

    /// T5-4: catalogs and foreign sources are collected by the tick and by the calls that decide
    /// on resources, once each; every other call reads the last collection.
    #[test]
    fn sources_are_collected_once_per_tick_and_deciding_call() {
        use std::sync::atomic::AtomicUsize;

        struct Counted(Arc<AtomicUsize>);
        impl ResourceCatalog for Counted {
            fn resources(&self) -> Vec<ResourceInfo> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            }
        }
        impl ForeignClaimSource for Counted {
            fn foreign(&self) -> Vec<ForeignClaim> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            }
        }
        let mut w = world(0);
        let (catalog, foreign) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        w.engine.catalogs.push(Box::new(Counted(catalog.clone())));
        w.engine.foreign.push(Box::new(Counted(foreign.clone())));
        let a = w.workspace("a");
        let calls = || {
            (
                catalog.load(Ordering::SeqCst),
                foreign.load(Ordering::SeqCst),
            )
        };
        let step = |w: &mut World, what: &str, expected: usize, f: &dyn Fn(&mut World)| {
            let before = calls();
            f(w);
            let after = calls();
            assert_eq!(
                (after.0 - before.0, after.1 - before.1),
                (expected, expected),
                "{what}"
            );
        };
        step(&mut w, "tick", 1, &|w| w.tick());
        step(&mut w, "reads", 0, &|w| {
            w.engine.snapshot().unwrap();
            w.engine.live_leases();
            w.engine.history(HistoryQuery::default()).unwrap();
        });
        let run = w.lease(&a, 100, screen());
        step(&mut w, "acquire and its tick", 1, &|w| {
            granted(w.acquire(&run).unwrap());
        });
        step(&mut w, "release and its tick", 0, &|w| {
            w.release(&run.id, &a);
        });
        step(&mut w, "reserve", 1, &|w| {
            w.engine.reserve(&screen(), &a).unwrap();
        });
    }

    #[test]
    fn reserve_refused_while_another_workspace_holds() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        let e = w.engine.reserve(&screen(), &b).unwrap_err();
        assert_eq!(e.code, ErrorCode::ResourceInUse);
        let mine = w.engine.reserve(&screen(), &a).unwrap();
        assert_eq!(mine.kind, ClaimKind::Reservation);
        assert_eq!(w.engine.reserve(&screen(), &a).unwrap().id, mine.id);
        w.engine.unreserve(&screen(), &a).unwrap();
        assert_eq!(
            w.engine.unreserve(&screen(), &a).unwrap_err().code,
            ErrorCode::ClaimNotHeld
        );
    }

    #[test]
    fn own_reservation_is_preferred_and_blocks_others() {
        let mut w = world(2);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        w.engine.reserve(&dev(1), &a).unwrap();
        let any = |w: &World, ws: &str, pid: u32| {
            let me = w.proc(pid, None);
            w.request(
                ws,
                &me,
                Selector::Any {
                    kind: ResourceKind::Device,
                    attributes: BTreeMap::new(),
                },
                Purpose::Lease {
                    command: "e2e".into(),
                },
            )
        };
        let mine = granted(w.acquire(&any(&w, &a, 100)).unwrap());
        assert_eq!(mine.resource.id, dev(1));
        let theirs = granted(w.acquire(&any(&w, &b, 200)).unwrap());
        assert_eq!(theirs.resource.id, dev(0));
    }

    #[test]
    fn no_wait_busy_has_position_and_blockers() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let mut asking = w.lease(&b, 200, screen());
        asking.no_wait = true;
        match w.acquire(&asking).unwrap() {
            ClaimOutcome::Busy {
                position,
                of,
                blockers,
            } => {
                assert_eq!((position, of), (1, 1));
                assert_eq!(blockers.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        assert!(w.engine.store.claim_requests().unwrap().is_empty());
        // Free: --no-wait gets it.
        w.release(&run.id, &a);
        granted(w.acquire(&asking).unwrap());
    }

    #[test]
    fn uncatalogued_device_ids_are_unknown() {
        let mut w = world(1);
        let a = w.workspace("a");
        let e = w.acquire(&w.lease(&a, 100, dev(5))).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownResource);
        let e = w
            .acquire(&w.lease(&a, 101, ResourceId("printer:x".into())))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownResource);
        granted(w.acquire(&w.lease(&a, 102, dev(0))).unwrap());
    }

    #[test]
    fn account_of_another_project_is_refused() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let theirs = ResourceId::account(&w.project_of(&b), "qa-user");
        let e = w.acquire(&w.lease(&a, 100, theirs)).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        let mine = ResourceId::account(&w.project_of(&a), "qa-user");
        granted(w.acquire(&w.lease(&a, 101, mine)).unwrap());
    }

    #[test]
    fn two_checkouts_of_one_project_share_nothing_but_queue() {
        let mut w = world(0);
        let a = w.workspace("a");
        let a2 = w.checkout_of("a-worktree", &w.project_of(&a));
        let account = ResourceId::account(&w.project_of(&a), "qa-user");
        granted(w.acquire(&w.lease(&a, 100, account.clone())).unwrap());
        // Another workspace of the same project waits (L13 shares within a workspace only).
        assert!(is_queued(&w.acquire(&w.lease(&a2, 200, account)).unwrap()));
    }

    // ---- deadlocks ----

    #[test]
    fn deadlock_refused_at_acquire_when_the_new_request_closes_it() {
        let mut w = world(2);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        granted(w.acquire(&w.lease(&a, 100, dev(0))).unwrap());
        granted(w.acquire(&w.lease(&b, 200, dev(1))).unwrap());
        // A nested run of a's run waits for b's device…
        let inner_a = w.proc(101, Some(100));
        let wants_1 = w.request(
            &a,
            &inner_a,
            Selector::Exact { resource: dev(1) },
            Purpose::Lease {
                command: "inner a".into(),
            },
        );
        assert!(is_queued(&w.acquire(&wants_1).unwrap()));
        // …and b's nested run asking for a's device closes the cycle: refused at once.
        let inner_b = w.proc(201, Some(200));
        let wants_0 = w.request(
            &b,
            &inner_b,
            Selector::Exact { resource: dev(0) },
            Purpose::Lease {
                command: "inner b".into(),
            },
        );
        let e = w.acquire(&wants_0).unwrap_err();
        assert_eq!(e.code, ErrorCode::ClaimDeadlock);
        assert!(
            e.message
                .starts_with("waiting for fake-0 would never end: "),
            "{}",
            e.message
        );
        assert!(
            e.message
                .contains("(PID 201) holds fake-1 and waits for fake-0"),
            "{}",
            e.message
        );
        let item = w
            .engine
            .store
            .history_item_of(&wants_0.id)
            .unwrap()
            .unwrap();
        assert_eq!(item.reason, EndReason::Deadlock);
        assert!(item.detail.is_some());
        // The other side keeps waiting (it is granted once b's run ends).
        assert!(is_queued(&w.state_of(&wants_1.id)));
    }

    #[test]
    fn deadlock_with_heavy_refused_on_tick() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        // a's run holds the screen.
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        // A heavy task (holder 300) runs; its nested run waits for the screen.
        w.proc(300, None);
        let inner = w.proc(301, Some(300));
        let wants_screen = w.request(
            &b,
            &inner,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "inner".into(),
            },
        );
        assert!(is_queued(&w.acquire(&wants_screen).unwrap()));
        // Then a's run waits for the heavy slot: a cycle across both queues.
        w.proc(101, Some(100));
        let heavy = HeavyView {
            capacity: 1,
            running: vec![HeavyRunning {
                id: "h1".into(),
                holder: w.proc(300, None),
                child: None,
                group: None,
                holder_ancestry: vec![300],
            }],
            waiting: vec![("h2".into(), vec![101, 100])],
        };
        w.engine.tick(&heavy);
        assert_eq!(w.ended(&wants_screen.id), Some(EndReason::Deadlock));
    }

    /// L25: a request that only queues behind a cycle is stuck but no victim — at acquire it
    /// queues; the tick refuses the cycle's own member, after which the bystander just waits.
    #[test]
    fn bystander_behind_a_cycle_queues_and_the_victim_is_refused() {
        let mut w = world(0);
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        w.proc(300, None);
        let inner = w.proc(301, Some(300));
        let wants_screen = w.request(
            &b,
            &inner,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "inner".into(),
            },
        );
        assert!(is_queued(&w.acquire(&wants_screen).unwrap()));
        w.proc(101, Some(100));
        let heavy = HeavyView {
            capacity: 1,
            running: vec![HeavyRunning {
                id: "h1".into(),
                holder: w.proc(300, None),
                child: None,
                group: None,
                holder_ancestry: vec![300],
            }],
            waiting: vec![("h2".into(), vec![101, 100])],
        };
        // c's run waits for the screen behind the cycle; nothing waits for c.
        let bystander = w.lease(&c, 400, screen());
        let outcome = w.engine.acquire(bystander.clone(), &heavy).unwrap();
        assert!(is_queued(&outcome), "{outcome:?}");
        assert_eq!(w.ended(&wants_screen.id), Some(EndReason::Deadlock));
        assert_eq!(w.ended(&bystander.id), None);
        w.engine.tick(&heavy);
        assert!(is_queued(&w.state_of(&bystander.id)));
    }

    // ---- seizing, distinct, forced release ----

    #[test]
    fn no_join_while_seizing() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        granted(w.acquire(&w.hold(&a, screen(), None)).unwrap());
        let taker = w.lease(&b, 700, screen());
        w.engine.plans.push(Plan {
            resource: screen(),
            by_workspace: "/work/b".into(),
            request: taker,
            expires_at_ms: w.now() + PLAN_TTL_MS,
            detail: Default::default(),
        });
        let run = w.lease(&a, 100, screen());
        match w.acquire(&run).unwrap() {
            ClaimOutcome::Queued { blockers, .. } => assert!(
                blockers
                    .iter()
                    .any(|b| matches!(b, crate::contract::claims::Blocker::Seizing { .. }))
            ),
            other => panic!("{other:?}"),
        }
        // The plan expires after 60 s; then the run joins.
        w.advance(PLAN_TTL_MS + 1);
        w.tick();
        assert!(w.engine.plans.is_empty());
        joined(w.state_of(&run.id));
    }

    #[test]
    fn distinct_exact_on_own_claim_is_refused_at_once() {
        let mut w = world(1);
        let a = w.workspace("a");
        granted(w.acquire(&w.lease(&a, 100, dev(0))).unwrap());
        let cli = w.proc(900, None);
        let distinct = w.request(
            &a,
            &cli,
            Selector::Exact { resource: dev(0) },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: None,
                distinct: true,
            },
        );
        let e = w.acquire(&distinct).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(w.engine.store.claim_requests().unwrap().is_empty());
    }

    /// CR-A-4: a queued `--distinct` hold whose workspace gets the resource meanwhile ends with
    /// the refusal itself — its own reason and text — not as withdrawn.
    #[test]
    fn a_queued_distinct_hold_ends_with_its_refusal() {
        let mut w = world(1);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let other = w.lease(&b, 200, dev(0));
        granted(w.acquire(&other).unwrap());
        let cli = w.proc(900, None);
        let distinct = w.request(
            &a,
            &cli,
            Selector::Exact { resource: dev(0) },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: None,
                distinct: true,
            },
        );
        // a's own run waits ahead of it, so it gets the device first.
        let own = w.lease(&a, 100, dev(0));
        assert!(is_queued(&w.acquire(&own).unwrap()));
        assert!(is_queued(&w.acquire(&distinct).unwrap()));
        w.release(&other.id, &b);
        assert!(w.live(&own.id).is_some());
        // The next tick (every 2 s) re-admits it: refused.
        w.tick();
        let refused = match w.state_of(&distinct.id) {
            ClaimOutcome::Finished { item } => item,
            other => panic!("{other:?}"),
        };
        assert_eq!(refused.reason, EndReason::Refused);
        assert!(refused.detail.as_deref().is_some_and(|d| !d.is_empty()));
    }

    #[test]
    fn force_release_ends_a_stuck_lease_and_records_forced() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        let run = w.lease(&a, 100, screen());
        joined(w.acquire(&run).unwrap());
        let me = w.proc(800, None);
        assert_eq!(
            w.engine
                .force_release(&hold.id, "Screen", &b, &me)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        w.drain_events();
        let record = w.engine.force_release(&hold.id, "screen", &b, &me).unwrap();
        assert!(record.forced && !record.stranded);
        assert_eq!(record.resource, "screen");
        assert_eq!(record.by_pid, 800);
        assert_eq!(w.ended(&hold.id), Some(EndReason::Seized));
        assert_eq!(
            w.ended(&run.id),
            Some(EndReason::Seized),
            "linked claims end too"
        );
        assert!(
            w.drain_events()
                .iter()
                .any(|e| matches!(e, Event::ClaimsSeized(s) if s.forced))
        );
        // The run's release learns who ended its claim.
        let b_path = w.engine.store.workspace(&b).unwrap().unwrap().path;
        assert_eq!(w.release(&run.id, &a).seized_by, Some(b_path));
    }

    #[test]
    fn release_reports_whether_the_parent_hold_lives() {
        let mut w = world(0);
        let a = w.workspace("a");
        let hold = w.hold(&a, screen(), None);
        granted(w.acquire(&hold).unwrap());
        let run = w.lease(&a, 100, screen());
        joined(w.acquire(&run).unwrap());
        let out = w.release(&run.id, &a);
        assert_eq!(out.parent.map(|p| p.id), Some(hold.id.clone()));
        // The hold ends while a second run is joined: that run's release reports no parent.
        let run2 = w.lease(&a, 200, screen());
        joined(w.acquire(&run2).unwrap());
        w.release(&hold.id, &a);
        assert_eq!(w.release(&run2.id, &a).parent, None);
        // Released twice: the same answer, not an error.
        assert_eq!(w.release(&run2.id, &a).parent, None);
    }

    #[test]
    fn one_parent_lookup_per_tick() {
        let mut w = world(0);
        let (a, b, c) = (w.workspace("a"), w.workspace("b"), w.workspace("c"));
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        w.acquire(&w.lease(&b, 200, screen())).unwrap();
        w.acquire(&w.lease(&c, 300, screen())).unwrap();
        let before = w.table.parent_lookups();
        w.tick();
        assert_eq!(w.table.parent_lookups() - before, 1);
        let before = w.table.parent_lookups();
        w.acquire(&w.lease(&c, 301, screen())).unwrap();
        // The acquire's decision and the tick that follows it share one (T5-3).
        assert_eq!(w.table.parent_lookups() - before, 1);
    }

    /// T5-3: the server's heavy view carries each holder alone; the engine fills in the
    /// ancestries with its own one lookup, so a cycle across both queues is still found.
    #[test]
    fn heavy_ancestries_are_filled_by_the_engines_one_lookup() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        w.proc(300, None);
        let inner = w.proc(301, Some(300));
        let wants_screen = w.request(
            &b,
            &inner,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "inner".into(),
            },
        );
        assert!(is_queued(&w.acquire(&wants_screen).unwrap()));
        w.proc(101, Some(100));
        let heavy = HeavyView {
            capacity: 1,
            running: vec![HeavyRunning {
                id: "h1".into(),
                holder: w.proc(300, None),
                child: None,
                group: None,
                holder_ancestry: vec![300],
            }],
            // As `heavy_view` makes it: the waiting entry's holder alone.
            waiting: vec![("h2".into(), vec![101])],
        };
        let before = w.table.parent_lookups();
        w.engine.tick(&heavy);
        assert_eq!(w.table.parent_lookups() - before, 1);
        assert_eq!(w.ended(&wants_screen.id), Some(EndReason::Deadlock));
    }

    // ---- holder checks and commands ----

    #[test]
    fn check_holder_needs_the_requesters_own_row() {
        let mut w = world(0);
        let a = w.workspace("a");
        let account = ResourceId::account(&w.project_of(&a), "qa-user");
        let run = w.lease(&a, 100, account.clone());
        granted(w.acquire(&run).unwrap());
        let second = w.lease(&a, 200, account.clone());
        joined(w.acquire(&second).unwrap());
        w.engine
            .check_holder(&run.id, &account, &run.requester)
            .unwrap();
        w.engine
            .check_holder(&second.id, &account, &second.requester)
            .unwrap();
        for (claim, resource, who) in [
            (&run.id, &account, &second.requester),
            (&run.id, &screen(), &run.requester),
            (&"nope".to_owned(), &account, &run.requester),
        ] {
            assert_eq!(
                w.engine
                    .check_holder(claim, resource, who)
                    .unwrap_err()
                    .code,
                ErrorCode::ClaimNotHeld
            );
        }
        let summary = w.engine.holder_of(&account).unwrap();
        assert_eq!((summary.id.as_str(), summary.linked), (run.id.as_str(), 1));
    }

    #[test]
    fn started_on_an_ended_claim_is_claim_not_held() {
        let mut w = world(0);
        let a = w.workspace("a");
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        w.release(&run.id, &a);
        let e = w
            .engine
            .started(ClaimStarted {
                claim_id: run.id.clone(),
                child: w.proc(101, Some(100)),
                group: RunGroup::UnixGroup {
                    pgid: 101,
                    boot_id: BOOT.into(),
                },
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ClaimNotHeld);
    }

    #[test]
    fn set_command_updates_snapshot_and_history() {
        let mut w = world(0);
        let a = w.workspace("a");
        let me = w.proc(100, None);
        let run = w.request(
            &a,
            &me,
            Selector::Exact { resource: screen() },
            Purpose::Lease {
                command: "<command pending>".into(),
            },
        );
        granted(w.acquire(&run).unwrap());
        w.engine.set_command(&run.id, &a, "login ***").unwrap();
        let snapshot = w.engine.snapshot().unwrap();
        assert_eq!(
            snapshot.resources[0].claims[0].command.as_deref(),
            Some("login ***")
        );
        let other = w.workspace("b");
        assert_eq!(
            w.engine.set_command(&run.id, &other, "x").unwrap_err().code,
            ErrorCode::ClaimNotHeld
        );
        assert_eq!(
            w.engine
                .set_command(&run.id, &a, &"x".repeat(MAX_COMMAND_BYTES + 1))
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        w.release(&run.id, &a);
        let item = w.engine.store.history_item_of(&run.id).unwrap().unwrap();
        assert_eq!(item.command.as_deref(), Some("login ***"));
    }

    /// T3-5: history, snapshots and seizures show displays, not ids.
    #[test]
    fn history_and_snapshot_show_displays() {
        let mut w = world(1);
        let a = w.workspace("a");
        let account = ResourceId::account(&w.project_of(&a), "qa-user");
        let run = w.lease(&a, 100, account.clone());
        granted(w.acquire(&run).unwrap());
        let snapshot = w.engine.snapshot().unwrap();
        let state = snapshot
            .resources
            .iter()
            .find(|r| r.resource.id == account)
            .unwrap();
        assert_eq!(state.resource.display, "account qa-user");
        w.release(&run.id, &a);
        let device = w.lease(&a, 200, dev(0));
        granted(w.acquire(&device).unwrap());
        w.release(&device.id, &a);
        let history = w
            .engine
            .history(HistoryQuery {
                since_ms: None,
                limit: 10,
            })
            .unwrap();
        let shown: Vec<&str> = history.iter().map(|i| i.resource.as_str()).collect();
        assert!(shown.contains(&"account qa-user"), "{shown:?}");
        assert!(shown.contains(&"fake-0"), "{shown:?}");
        assert!(
            !shown.iter().any(|s| s.starts_with("account:")),
            "{shown:?}"
        );
    }

    // ---- stopping and limits ----

    #[test]
    fn stopping_grants_nothing() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        let run = w.lease(&a, 100, screen());
        granted(w.acquire(&run).unwrap());
        let waiter = w.lease(&b, 200, screen());
        assert!(is_queued(&w.acquire(&waiter).unwrap()));
        w.engine.begin_stopping();
        let e = w.acquire(&w.lease(&b, 300, screen())).unwrap_err();
        assert_eq!(e.code, ErrorCode::ServiceUnavailable);
        w.release(&run.id, &a);
        assert!(
            is_queued(&w.state_of(&waiter.id)),
            "nothing granted while stopping"
        );
    }

    #[test]
    fn snapshot_event_once_per_change() {
        let mut w = world(0);
        let a = w.workspace("a");
        w.drain_events();
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        let changes = |events: Vec<Event>| {
            events
                .into_iter()
                .filter(|e| matches!(e, Event::ClaimsChanged(_)))
                .count()
        };
        assert_eq!(changes(w.drain_events()), 1);
        w.tick();
        w.tick();
        assert_eq!(changes(w.drain_events()), 0);
    }

    #[test]
    fn live_rows_bound_then_queue_busy() {
        let mut w = world(0);
        let (a, b) = (w.workspace("a"), w.workspace("b"));
        granted(w.acquire(&w.lease(&a, 100, screen())).unwrap());
        let me = w.proc(200, None);
        for _ in 0..MAX_LIVE_ROWS - 1 {
            let r = w.request(
                &b,
                &me,
                Selector::Exact { resource: screen() },
                Purpose::Lease {
                    command: "x".into(),
                },
            );
            w.engine.store.insert_claim_request(&r, w.now()).unwrap();
        }
        let e = w.acquire(&w.lease(&b, 300, screen())).unwrap_err();
        assert_eq!(e.code, ErrorCode::QueueBusy);
    }

    #[test]
    fn invalid_requests_are_refused() {
        let mut w = world(0);
        let a = w.workspace("a");
        let mut bad_id = w.lease(&a, 100, screen());
        bad_id.id = "not-a-uuid".into();
        let mut long = w.lease(&a, 101, screen());
        long.purpose = Purpose::Lease {
            command: "x".repeat(MAX_COMMAND_BYTES + 1),
        };
        let mut no_workspace = w.lease(&a, 102, screen());
        no_workspace.workspace_id = "ghost".into();
        let mut ttl = w.hold(&a, screen(), Some(0));
        ttl.id = uuid::Uuid::new_v4().to_string();
        let dead = ProcessIdentity {
            pid: 4242,
            boot_id: BOOT.into(),
            start_time: 1,
        };
        let bound = w.request(
            &a,
            &w.proc(103, None),
            Selector::Exact { resource: screen() },
            Purpose::Hold {
                ttl_ms: None,
                bound_to: Some(dead),
                distinct: false,
            },
        );
        for r in [bad_id, long, no_workspace, ttl, bound] {
            assert_eq!(
                w.acquire(&r).unwrap_err().code,
                ErrorCode::BadRequest,
                "{r:?}"
            );
        }
    }
}
