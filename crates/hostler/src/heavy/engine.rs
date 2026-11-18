//! The heavy-queue engine inside the service (plan Task 7). Single-threaded: the server holds it
//! behind one mutex. Every decision reads the store fresh, so a restarted service continues
//! exactly where the last one committed.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use super::queue;
use crate::contract::claims::StopTarget;
use crate::contract::heavy::{
    AcquireOutcome, HeavyAcquire, HeavyEntry, HeavyHistoryItem, HeavyOutcome, HeavyRelease,
    HeavyStarted, HeavyState, HistoryQuery, QueueSnapshot, QueueStopOutcome,
};
use crate::contract::{ErrorCode, HostlerError};
use crate::procs::stop::RunStop;
use crate::procs::{self, Liveness, ProcessTable, RunGroup};
use crate::store::Store;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// Wall-clock milliseconds: only for timestamps shown to people and for history retention.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Heavy work hostler did not grant but must count (Task 11: the rn-looky ledger).
pub trait ForeignHeavySource: Send + Sync {
    fn running(&self) -> Vec<HeavyEntry>;
}

pub struct NoForeign;

impl ForeignHeavySource for NoForeign {
    fn running(&self) -> Vec<HeavyEntry> {
        Vec::new()
    }
}

pub const MIN_CAPACITY: u8 = 1;
pub const MAX_CAPACITY: u8 = 16;
/// Admission limits that keep every snapshot, history page and busy error within one frame.
/// String limits count JSON bytes (`json_len`): escaping can make a string up to 6x longer on
/// the wire than in memory.
pub const MAX_COMMAND_BYTES: usize = 1024;
pub const MAX_PATH_BYTES: usize = 2048;
/// Boot ids and job names inside identities and run groups (real ones are under 64 bytes).
pub const MAX_IDENTITY_BYTES: usize = 256;
pub const MAX_LIVE_ENTRIES: usize = 512;

/// The bytes `s` takes inside a JSON string as serde_json writes it: escapes included, the
/// surrounding quotes not.
pub fn json_len(s: &str) -> usize {
    s.chars().map(json_char_len).sum()
}

fn json_char_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\u{08}' | '\u{0C}' | '\n' | '\r' | '\t' => 2,
        '\u{00}'..='\u{1F}' => 6,
        c => c.len_utf8(),
    }
}

/// The longest prefix of `s`, in whole characters, whose `json_len` is at most `max`.
pub fn truncate_to_json_len(s: &str, max: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += json_char_len(c);
        if used > max {
            return &s[..i];
        }
    }
    s
}

fn check_json_len(field: &str, value: &str, max: usize) -> Result<(), HostlerError> {
    let len = json_len(value);
    if len > max {
        return Err(bad_request(format!(
            "{field} is {len} bytes as JSON; at most {max} are accepted"
        )));
    }
    Ok(())
}

fn check_group(group: &RunGroup) -> Result<(), HostlerError> {
    match group {
        RunGroup::UnixGroup { boot_id, .. } => {
            check_json_len("group.boot_id", boot_id, MAX_IDENTITY_BYTES)
        }
        RunGroup::WindowsJob { name, boot_id, .. } => {
            check_json_len("group.name", name, MAX_IDENTITY_BYTES)?;
            check_json_len("group.boot_id", boot_id, MAX_IDENTITY_BYTES)
        }
    }
}

fn cut(s: &mut String, max: usize) {
    if json_len(s) > max {
        *s = truncate_to_json_len(s, max).to_owned();
    }
}

/// Display strings of an entry cut to the admission limits, so a response is always
/// representable — also for rows stored under older limits. Ids, tickets, states, pids and
/// start times are never touched, and the engine itself decides on the stored rows, not on
/// these copies.
fn clamp_display(mut e: HeavyEntry) -> HeavyEntry {
    cut(&mut e.command, MAX_COMMAND_BYTES);
    cut(&mut e.cwd, MAX_PATH_BYTES);
    if let Some(root) = e.workspace_root.as_mut() {
        cut(root, MAX_PATH_BYTES);
    }
    cut(&mut e.holder.boot_id, MAX_IDENTITY_BYTES);
    if let Some(child) = e.child.as_mut() {
        cut(&mut child.boot_id, MAX_IDENTITY_BYTES);
    }
    match e.group.as_mut() {
        Some(RunGroup::UnixGroup { boot_id, .. }) => cut(boot_id, MAX_IDENTITY_BYTES),
        Some(RunGroup::WindowsJob { name, boot_id, .. }) => {
            cut(name, MAX_IDENTITY_BYTES);
            cut(boot_id, MAX_IDENTITY_BYTES);
        }
        None => {}
    }
    e
}

fn clamp_history(mut item: HeavyHistoryItem) -> HeavyHistoryItem {
    cut(&mut item.command, MAX_COMMAND_BYTES);
    cut(&mut item.cwd, MAX_PATH_BYTES);
    if let Some(root) = item.workspace_root.as_mut() {
        cut(root, MAX_PATH_BYTES);
    }
    item
}

/// Foreign entries come from files hostler does not control: their id and kind are cut too.
fn clamp_foreign(e: HeavyEntry) -> HeavyEntry {
    let mut e = clamp_display(e);
    cut(&mut e.id, MAX_IDENTITY_BYTES);
    cut(&mut e.kind, MAX_IDENTITY_BYTES);
    e
}

const HISTORY_RETENTION_MS: i64 = 30 * 24 * 3_600_000;
const PRUNE_EVERY_MS: i64 = 3_600_000;

pub struct HeavyEngine {
    store: Store,
    table: Arc<dyn ProcessTable>,
    clock: Arc<dyn Clock>,
    foreign: Box<dyn ForeignHeavySource>,
    events: crate::service::events::EventBus,
    last_snapshot: Option<QueueSnapshot>,
    last_prune_ms: Option<i64>,
    /// Set by an accepted shutdown: no slot is granted any more; entries stay persisted.
    stopping: bool,
    /// P5 (spec §11.1): the `heavy.stop`s in progress, by entry id, each with the exit code its
    /// holder's release reported meanwhile. In memory only: a restart abandons them.
    stops: HashMap<String, Option<i32>>,
    /// Whether a pid is a member of a run group now (`procs::in_group`); unit tests, whose pids
    /// are fake, put their own.
    member: fn(u32, &RunGroup) -> bool,
}

/// P5 (spec §11.1): a `Stopping` entry is the stop's alone.
fn being_stopped(id: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::BadRequest,
        message: format!("heavy entry {id}: the task is being stopped"),
        details: json!({ "id": id }),
        recovery: Some("hostler queue".into()),
    }
}

/// P5 (spec §11.1): nothing starts under a task being stopped (a nested `run --heavy`, or its
/// own command when the stop came before it started).
fn stopping_taken_over(id: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::TakenOver,
        message: format!("heavy task {id} is being stopped; nothing may start under it now"),
        details: json!({ "id": id }),
        recovery: Some("hostler queue".into()),
    }
}

fn internal(e: anyhow::Error) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: format!("{e:#}"),
        details: json!({}),
        recovery: None,
    }
}

fn bad_request(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::BadRequest,
        message,
        details: json!({}),
        recovery: None,
    }
}

fn unknown_entry(id: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::UnknownEntry,
        message: format!("no heavy entry {id}"),
        details: json!({ "id": id }),
        recovery: Some("hostler queue".into()),
    }
}

fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|u| u.get_version_num() == 4)
}

fn valid_kind(kind: &str) -> bool {
    (1..=32).contains(&kind.len())
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl HeavyEngine {
    pub fn new(
        store: Store,
        table: Arc<dyn ProcessTable>,
        clock: Arc<dyn Clock>,
        foreign: Box<dyn ForeignHeavySource>,
        events: crate::service::events::EventBus,
    ) -> Self {
        HeavyEngine {
            store,
            table,
            clock,
            foreign,
            events,
            last_snapshot: None,
            last_prune_ms: None,
            stopping: false,
            stops: HashMap::new(),
            member: procs::in_group,
        }
        .abandoning_stops()
    }

    /// Spec §11.1: a stop the last service run did not finish is abandoned; its entry runs
    /// again, and the usual reaping records `reaped` once its processes are gone (never
    /// `stopped`: this service did not verify the stop).
    fn abandoning_stops(self) -> Self {
        let entries = match self.store.entries() {
            Ok(entries) => entries,
            Err(e) => {
                tracing::error!("cannot read the heavy queue for unfinished stops: {e:#}");
                return self;
            }
        };
        for e in entries.iter().filter(|e| e.state == HeavyState::Stopping) {
            match self.store.unmark_stopping(&e.id) {
                Ok(_) => tracing::warn!("abandoned an unfinished stop of {}", e.id),
                Err(err) => tracing::error!("cannot abandon the stop of {}: {err:#}", e.id),
            }
        }
        self
    }

    /// Idempotent by `req.id`: a known id answers its current state and never re-enqueues.
    pub fn acquire(&mut self, req: HeavyAcquire) -> Result<AcquireOutcome, HostlerError> {
        if !valid_id(&req.id) {
            return Err(bad_request(format!(
                "heavy request id {:?} is not a UUID v4",
                req.id
            )));
        }
        if !valid_kind(&req.kind) {
            return Err(bad_request(format!(
                "heavy kind {:?} must match ^[a-z0-9-]{{1,32}}$",
                req.kind
            )));
        }
        check_json_len("command", &req.command, MAX_COMMAND_BYTES)?;
        check_json_len("cwd", &req.cwd, MAX_PATH_BYTES)?;
        if let Some(root) = &req.workspace_root {
            check_json_len("workspace_root", root, MAX_PATH_BYTES)?;
        }
        check_json_len("holder.boot_id", &req.holder.boot_id, MAX_IDENTITY_BYTES)?;
        if let Some(outcome) = self.known(&req.id)? {
            return Ok(outcome);
        }
        self.tick();
        if let Some(token) = &req.reentrant_token {
            match self.nested_in(token, req.holder.pid)? {
                Some(HeavyState::Running) => return Ok(AcquireOutcome::Reentrant),
                Some(HeavyState::Stopping) => return Err(stopping_taken_over(token)),
                _ => {}
            }
        }
        let entries = self.store.entries().map_err(internal)?;
        if entries.len() >= MAX_LIVE_ENTRIES {
            return Err(HostlerError {
                code: ErrorCode::QueueBusy,
                message: format!(
                    "the heavy queue already holds {MAX_LIVE_ENTRIES} requests; try again later"
                ),
                details: json!({ "live_entries": entries.len(), "max": MAX_LIVE_ENTRIES }),
                recovery: Some("hostler queue".into()),
            });
        }
        if req.no_wait && self.free_slots(&entries)? == 0 {
            let waiting = entries
                .iter()
                .filter(|e| e.state == HeavyState::Waiting)
                .count() as u32;
            return Ok(AcquireOutcome::Busy {
                position: waiting + 1,
                of: waiting + 1,
            });
        }
        self.store
            .insert_waiting(&req, self.clock.now_ms())
            .map_err(internal)?;
        #[cfg(debug_assertions)]
        if std::env::var("HOSTLER_TEST_EXIT_AFTER_COMMIT").as_deref() == Ok("heavy.acquire") {
            // Simulates a crash between the commit and the reply.
            std::process::exit(1);
        }
        self.tick();
        Ok(self
            .known(&req.id)?
            .expect("an entry inserted under the engine lock is live or finished"))
    }

    /// The same answers as an idempotent `acquire`; `UnknownEntry` only for an id never seen.
    pub fn attach(&mut self, id: &str) -> Result<AcquireOutcome, HostlerError> {
        self.tick();
        self.known(id)?.ok_or_else(|| unknown_entry(id))
    }

    /// `heavy.started` from the connection whose peer is `peer_pid` (where the OS gives one).
    /// A new report's command must be the run it claims (P5 T6-R1-1), see `check_started`.
    pub fn started(&mut self, s: HeavyStarted, peer_pid: Option<u32>) -> Result<(), HostlerError> {
        check_json_len("child.boot_id", &s.child.boot_id, MAX_IDENTITY_BYTES)?;
        check_group(&s.group)?;
        // Definitive: only a live, granted entry may start its command.
        match self.store.entry(&s.id).map_err(internal)? {
            Some(e) if e.state == HeavyState::Waiting => Err(bad_request(format!(
                "heavy entry {} has not been granted a slot",
                s.id
            ))),
            // P5 (spec §11.1): the stop came first; the command must not start.
            Some(e) if e.state == HeavyState::Stopping && e.child.is_none() => {
                Err(stopping_taken_over(&s.id))
            }
            Some(e) if e.child.is_some() => {
                // A retry of the same report is fine; a different child is not.
                if e.child.as_ref() == Some(&s.child) && e.group.as_ref() == Some(&s.group) {
                    Ok(())
                } else {
                    Err(bad_request(format!(
                        "heavy entry {} already started another command",
                        s.id
                    )))
                }
            }
            Some(e) => {
                self.check_started(&e.holder, &s, peer_pid)?;
                self.store
                    .set_started(&s.id, &s.child, &s.group)
                    .map_err(internal)?;
                // L31: the service keeps the run's job findable while the entry lives.
                self.table.hold_group("heavy", &s.group);
                self.tick();
                Ok(())
            }
            None => match self.store.history_item(&s.id).map_err(internal)? {
                // Withdrawn, timed out, force-released or reaped before the command started
                // (and, for completeness, an already released one): the command must not run.
                Some(item) => Err(HostlerError {
                    code: ErrorCode::Withdrawn,
                    message: format!(
                        "heavy entry {} has already finished ({:?}); its command must not start",
                        s.id, item.outcome
                    ),
                    details: json!({ "id": s.id, "outcome": item.outcome }),
                    recovery: None,
                }),
                None => Err(unknown_entry(&s.id)),
            },
        }
    }

    /// P5 T6-R1-1: the run a granted entry reports is what a person-approved `heavy.stop` will
    /// signal later, so it is checked when it arrives. The command must be live with the
    /// reported identity (pid, boot, start time); it must be a child of the entry's live
    /// holder, and the report must come from that holder (the connection's peer, where the OS
    /// gives one; elsewhere the holder's live identity stands in); the group must be of the
    /// command's boot, led by the command on Unix (`pgid == child.pid`), and contain it now.
    fn check_started(
        &self,
        holder: &procs::ProcessIdentity,
        s: &HeavyStarted,
        peer_pid: Option<u32>,
    ) -> Result<(), HostlerError> {
        let table = self.table.as_ref();
        let refuse = |why: String| {
            Err(HostlerError {
                code: ErrorCode::BadRequest,
                message: format!("heavy entry {}: {why}", s.id),
                details: json!({ "id": s.id }),
                recovery: None,
            })
        };
        let child = &s.child;
        if procs::liveness(child, table) != Liveness::Alive {
            return refuse(format!(
                "its command {} is not running with the reported identity",
                child.pid
            ));
        }
        if procs::liveness(holder, table) != Liveness::Alive {
            return refuse(format!("its holder {} is not running", holder.pid));
        }
        if let Some(peer) = peer_pid
            && peer != holder.pid
        {
            return refuse(format!(
                "the report comes from process {peer}, not from the entry's holder {}",
                holder.pid
            ));
        }
        if table.info(child.pid).and_then(|i| i.parent) != Some(holder.pid) {
            return refuse(format!(
                "its command {} is not a child of the entry's holder {}",
                child.pid, holder.pid
            ));
        }
        let (group_boot, leader) = match &s.group {
            RunGroup::UnixGroup { pgid, boot_id } => (boot_id, Some(*pgid)),
            RunGroup::WindowsJob { boot_id, .. } => (boot_id, None),
        };
        if !procs::same_boot(group_boot, &child.boot_id) {
            return refuse("its group is of another boot than its command".into());
        }
        if let Some(pgid) = leader
            && pgid != child.pid
        {
            return refuse(format!(
                "its process group {pgid} is not led by its command {}",
                child.pid
            ));
        }
        if !(self.member)(child.pid, &s.group) {
            return refuse(format!(
                "its command {} is not in the reported group",
                child.pid
            ));
        }
        Ok(())
    }

    pub fn release(&mut self, r: HeavyRelease) -> Result<(), HostlerError> {
        if !matches!(r.outcome, HeavyOutcome::Ok | HeavyOutcome::Failed) {
            return Err(bad_request(format!(
                "a release reports ok or failed, not {:?}",
                r.outcome
            )));
        }
        // P5 (spec §11.1): during a stop the holder's release is accepted and its exit code
        // kept for the `stopped` line; the stop records the entry.
        if let Some(code) = self.stops.get_mut(&r.id) {
            *code = r.exit_code;
            return Ok(());
        }
        self.store
            .finish(&r.id, r.exit_code, r.outcome, self.clock.now_ms())
            .map_err(internal)?;
        self.tick();
        Ok(())
    }

    /// `heavy.stop`, its first step under the heavy lock (spec §11.1): the confirmation, a
    /// running hostler entry, and `is_self` (R2-9: a target that contains the service is
    /// refused) are checked before anything changes; then the entry is `stopping`, persisted.
    /// Returns the run the caller stops, outside this lock, and hands to `finish_stop`. The
    /// requester's peer pid goes to the log only (R2-5).
    pub fn begin_stop(
        &mut self,
        id: &str,
        confirmation: &str,
        peer_pid: Option<u32>,
        is_self: &dyn Fn(&StopTarget) -> bool,
    ) -> Result<StopTarget, HostlerError> {
        if confirmation != id {
            return Err(HostlerError {
                code: ErrorCode::BadRequest,
                message: "type the task's id to confirm".into(),
                details: json!({ "id": id }),
                recovery: Some(format!(
                    "hostler queue stop {id} --force --confirmation {id}"
                )),
            });
        }
        if self.foreign.running().iter().any(|e| e.id == id) {
            return Err(HostlerError {
                code: ErrorCode::BadRequest,
                message: format!(
                    "heavy task {id} is rn-looky's; rn-looky's tasks are never stopped by hostler"
                ),
                details: json!({ "id": id }),
                recovery: None,
            });
        }
        let Some(entry) = self.store.entry(id).map_err(internal)? else {
            return Err(match self.store.history_item(id).map_err(internal)? {
                Some(_) => HostlerError {
                    code: ErrorCode::BadRequest,
                    message: format!("heavy task {id} has already finished"),
                    details: json!({ "id": id }),
                    recovery: Some("hostler queue history".into()),
                },
                None => unknown_entry(id),
            });
        };
        match entry.state {
            HeavyState::Waiting => {
                return Err(HostlerError {
                    code: ErrorCode::BadRequest,
                    message: format!("heavy task {id} is waiting, not running"),
                    details: json!({ "id": id }),
                    recovery: Some(format!("hostler queue withdraw --id {id}")),
                });
            }
            HeavyState::Stopping => return Err(being_stopped(id)),
            HeavyState::Running => {}
        }
        let target = StopTarget::RunGroup {
            claim_id: format!("heavy:{id}"),
            holder: entry.holder,
            child: entry.child,
            group: entry.group,
        };
        if is_self(&target) {
            return Err(HostlerError {
                code: ErrorCode::BadRequest,
                message: "refusing to stop the hostler service itself".into(),
                details: json!({ "id": id }),
                recovery: None,
            });
        }
        if !self.store.mark_stopping(id).map_err(internal)? {
            return Err(internal(anyhow::anyhow!(
                "heavy entry {id} was not running when it was marked stopping"
            )));
        }
        self.stops.insert(id.to_owned(), None);
        let peer = peer_pid.map_or_else(|| "unknown".into(), |pid| pid.to_string());
        tracing::info!("stopping heavy task {id} for peer pid {peer}");
        self.tick();
        Ok(target)
    }

    /// `heavy.stop`, its last step under the heavy lock: a verified stop that signalled the
    /// task finishes the entry as `stopped` with the exit code its holder reported meanwhile
    /// (else none) and frees the slot. When everything was already gone before any signal, the
    /// task ended on its own: it is recorded `reaped` (P5-L10). Otherwise the entry runs again,
    /// keeps its slot, and the answer is `stop_failed`.
    pub fn finish_stop(
        &mut self,
        id: &str,
        run: &RunStop,
    ) -> Result<QueueStopOutcome, HostlerError> {
        let exit_code = self.stops.remove(id).flatten();
        if !run.gone {
            if let Some(e) = &run.error {
                tracing::warn!("could not stop heavy task {id}: {e:?}; it keeps its slot");
            } else {
                tracing::warn!("heavy task {id} still runs after its stop; it keeps its slot");
            }
            self.store.unmark_stopping(id).map_err(internal)?;
            self.tick();
            return Err(crate::contract::heavy::errors::stop_failed(id, &run.alive));
        }
        let entry = self.store.entry(id).map_err(internal)?;
        let outcome = if run.signalled {
            tracing::info!("stopped heavy task {id}");
            HeavyOutcome::Stopped
        } else {
            tracing::info!("heavy task {id} had already ended before its stop signalled it");
            HeavyOutcome::Reaped
        };
        self.store
            .finish(id, exit_code, outcome, self.clock.now_ms())
            .map_err(internal)?;
        self.tick();
        let pids = entry
            .map(|e| vec![e.child.as_ref().unwrap_or(&e.holder).pid])
            .unwrap_or_default();
        Ok(QueueStopOutcome {
            id: id.to_owned(),
            stopped: run.signalled,
            pids,
        })
    }

    /// Withdraws a request that has not started a command: waiting, or granted with no child yet
    /// (a grant racing the client's timeout or Ctrl+C).
    pub fn withdraw(&mut self, id: &str, timed_out: bool) -> Result<(), HostlerError> {
        match self.store.entry(id).map_err(internal)? {
            None if self.store.history_item(id).map_err(internal)?.is_some() => Ok(()),
            None => Err(unknown_entry(id)),
            Some(e) if e.state == HeavyState::Stopping => Err(being_stopped(id)),
            Some(e) if e.child.is_some() => Err(HostlerError {
                code: ErrorCode::BadRequest,
                message: format!("heavy entry {id} is already running a command"),
                details: json!({ "id": id }),
                recovery: Some(format!("hostler queue release --id {id} --force")),
            }),
            Some(_) => {
                let outcome = if timed_out {
                    HeavyOutcome::TimedOut
                } else {
                    HeavyOutcome::Withdrawn
                };
                self.store
                    .finish(id, None, outcome, self.clock.now_ms())
                    .map_err(internal)?;
                self.tick();
                Ok(())
            }
        }
    }

    /// Frees the slot (outcome `ForceReleased`, P5-L10); the process itself is left alone.
    pub fn force_release(&mut self, id: &str) -> Result<(), HostlerError> {
        if self
            .store
            .entry(id)
            .map_err(internal)?
            .is_some_and(|e| e.state == HeavyState::Stopping)
        {
            return Err(being_stopped(id));
        }
        let finished = self
            .store
            .finish(id, None, HeavyOutcome::ForceReleased, self.clock.now_ms())
            .map_err(internal)?;
        if !finished && self.store.history_item(id).map_err(internal)?.is_none() {
            return Err(unknown_entry(id));
        }
        self.tick();
        Ok(())
    }

    pub fn set_capacity(&mut self, n: u8) -> Result<(), HostlerError> {
        if !(MIN_CAPACITY..=MAX_CAPACITY).contains(&n) {
            return Err(HostlerError {
                code: ErrorCode::CapacityOutOfRange,
                message: format!("capacity must be {MIN_CAPACITY}–{MAX_CAPACITY}, not {n}"),
                details: json!({ "min": MIN_CAPACITY, "max": MAX_CAPACITY, "requested": n }),
                recovery: None,
            });
        }
        self.store.set_capacity(n).map_err(internal)?;
        self.tick();
        Ok(())
    }

    /// Accepts a shutdown and stops granting slots, in one step under the engine lock, unless
    /// `if_idle` and entries run: then nothing changes and those entries are returned.
    pub fn begin_shutdown(&mut self, if_idle: bool) -> Result<Vec<HeavyEntry>, HostlerError> {
        let running = self.snapshot()?.running;
        if if_idle && !running.is_empty() {
            return Ok(running);
        }
        self.stopping = true;
        Ok(Vec::new())
    }

    /// Captured under the heavy lock (`&self`); its point is read under the same lock, where
    /// every queue event is committed (spec §6.2, R-1).
    pub fn snapshot(&self) -> Result<QueueSnapshot, HostlerError> {
        let entries = self.store.entries().map_err(internal)?;
        let mut snapshot = self.snapshot_of(entries, self.foreign.running())?;
        snapshot.as_of_seq = self.events.current_seq();
        Ok(snapshot)
    }

    pub fn history(&self, q: HistoryQuery) -> Result<Vec<HeavyHistoryItem>, HostlerError> {
        let items = self.store.history(&q).map_err(internal)?;
        Ok(items.into_iter().map(clamp_history).collect())
    }

    /// Reaps dead entries, grants free slots, prunes history hourly and broadcasts a changed
    /// snapshot. Runs every 2 s and after every mutation.
    pub fn tick(&mut self) {
        if let Err(e) = self.try_tick() {
            tracing::error!("heavy queue tick failed: {e:#}");
        }
    }

    fn try_tick(&mut self) -> anyhow::Result<()> {
        #[cfg(debug_assertions)]
        let started = std::time::Instant::now();
        let now = self.clock.now_ms();
        let entries = self.store.entries()?;
        #[cfg(debug_assertions)]
        let checked = entries.len();
        for id in queue::dead_entries(&entries, &|e| self.entry_alive(e)) {
            tracing::info!("reaping heavy entry {id}: holder, child and group are gone");
            self.store.finish(&id, None, HeavyOutcome::Reaped, now)?;
        }
        let foreign = self.foreign.running();
        let entries = self.store.entries()?;
        // L31: handles only for live entries' jobs (reopened after a restart).
        let groups: Vec<RunGroup> = entries.iter().filter_map(|e| e.group.clone()).collect();
        self.table.hold_only("heavy", &groups);
        let capacity = self.store.capacity()?;
        if !self.stopping {
            for id in queue::grantable(capacity, &entries, foreign.len()) {
                self.store.mark_running(&id, now)?;
            }
        }
        if self
            .last_prune_ms
            .is_none_or(|last| now - last >= PRUNE_EVERY_MS)
        {
            self.store.prune_history(now - HISTORY_RETENTION_MS)?;
            self.last_prune_ms = Some(now);
        }
        // Captured under the heavy lock, then committed there (R-1); sent only when it changed.
        // No receivers is fine: nobody is listening.
        let snapshot = self
            .snapshot_of(self.store.entries()?, foreign)
            .map_err(|e| anyhow::anyhow!(e.message))?;
        if self.last_snapshot.as_ref() != Some(&snapshot) {
            self.last_snapshot = Some(snapshot.clone());
            self.events.publish_snapshot(snapshot);
        }
        // Debug builds: a tick holds the engine lock, so a slow one delays every request.
        #[cfg(debug_assertions)]
        {
            let took = started.elapsed();
            if took >= std::time::Duration::from_millis(50) {
                tracing::info!("slow heavy-queue tick: {took:?} for {checked} entries");
            }
        }
        Ok(())
    }

    fn entry_alive(&self, e: &HeavyEntry) -> bool {
        let table = self.table.as_ref();
        procs::liveness(&e.holder, table) == Liveness::Alive
            || e.child
                .as_ref()
                .is_some_and(|c| procs::liveness(c, table) == Liveness::Alive)
            || e.group
                .as_ref()
                .is_some_and(|g| procs::group_liveness(g, table))
    }

    fn free_slots(&self, entries: &[HeavyEntry]) -> Result<usize, HostlerError> {
        let running = entries.iter().filter(|e| e.state.occupies_slot()).count();
        if self.stopping {
            return Ok(0);
        }
        let capacity = self.store.capacity().map_err(internal)?;
        Ok(usize::from(capacity)
            .saturating_sub(running)
            .saturating_sub(self.foreign.running().len()))
    }

    /// The state of the slot-holding entry `token` names when its live holder or child is an
    /// ancestor of `pid`: a heavy command started inside a running one already owns the slot
    /// (P5: inside a stopping one, nothing may start). `None` otherwise.
    fn nested_in(&self, token: &str, pid: u32) -> Result<Option<HeavyState>, HostlerError> {
        let Some(entry) = self.store.entry(token).map_err(internal)? else {
            return Ok(None);
        };
        if !entry.state.occupies_slot() {
            return Ok(None);
        }
        let table = self.table.as_ref();
        let ancestors = procs::ancestors(pid, table);
        let owns = |id: &procs::ProcessIdentity| {
            ancestors.contains(&id.pid) && procs::liveness(id, table) == Liveness::Alive
        };
        Ok((owns(&entry.holder) || entry.child.as_ref().is_some_and(owns)).then_some(entry.state))
    }

    /// The current answer for a known id: live entry or history item.
    fn known(&self, id: &str) -> Result<Option<AcquireOutcome>, HostlerError> {
        if let Some(entry) = self.store.entry(id).map_err(internal)? {
            let entry = clamp_display(entry);
            return Ok(Some(match entry.state {
                HeavyState::Running | HeavyState::Stopping => AcquireOutcome::Granted { entry },
                HeavyState::Waiting => {
                    let entries = self.store.entries().map_err(internal)?;
                    let (position, of) = queue::position(&entries, id).unwrap_or((1, 1));
                    AcquireOutcome::Queued {
                        entry,
                        position,
                        of,
                    }
                }
            }));
        }
        Ok(self
            .store
            .history_item(id)
            .map_err(internal)?
            .map(|item| AcquireOutcome::Finished {
                item: clamp_history(item),
            }))
    }

    fn snapshot_of(
        &self,
        mut entries: Vec<HeavyEntry>,
        foreign_running: Vec<HeavyEntry>,
    ) -> Result<QueueSnapshot, HostlerError> {
        entries.sort_by_key(|e| e.ticket);
        let (running, waiting) = entries
            .into_iter()
            .map(clamp_display)
            .partition(|e| e.state.occupies_slot());
        Ok(QueueSnapshot {
            capacity: self.store.capacity().map_err(internal)?,
            running,
            waiting,
            // Grants count every foreign entry; the snapshot lists a bounded, clamped few.
            foreign_running: foreign_running
                .into_iter()
                .take(MAX_LIVE_ENTRIES)
                .map(clamp_foreign)
                .collect(),
            // The tick's comparison sees 0; `snapshot()` and `publish` fill it.
            as_of_seq: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::contract::heavy::EntrySource;
    use crate::contract::wire::Event;
    use crate::procs::fake::FakeTable;
    use crate::procs::stop::StopError;
    use crate::procs::{ProcessIdentity, RunGroup};

    const BOOT: &str = "b1";

    struct FakeClock(AtomicI64);

    impl Clock for FakeClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Clone, Default)]
    struct FakeForeign(Arc<StdMutex<Vec<HeavyEntry>>>);

    impl ForeignHeavySource for FakeForeign {
        fn running(&self) -> Vec<HeavyEntry> {
            self.0.lock().unwrap().clone()
        }
    }

    struct World {
        engine: HeavyEngine,
        table: Arc<FakeTable>,
        clock: Arc<FakeClock>,
        foreign: FakeForeign,
        events: crate::service::events::TestEvents,
    }

    fn world() -> World {
        let table = Arc::new(FakeTable::new(BOOT));
        let clock = Arc::new(FakeClock(AtomicI64::new(1_000)));
        let foreign = FakeForeign::default();
        let (tx, events) = crate::service::events::EventBus::channel(64);
        let engine = HeavyEngine::new(
            Store::open_in_memory().unwrap(),
            table.clone(),
            clock.clone(),
            Box::new(foreign.clone()),
            tx,
        );
        // Fake pids: a command leads its own group (the gate launcher does).
        let mut engine = engine;
        engine.member = leads_its_group;
        World {
            engine,
            table,
            clock,
            foreign,
            events,
        }
    }

    fn leads_its_group(pid: u32, group: &RunGroup) -> bool {
        matches!(group, RunGroup::UnixGroup { pgid, .. } if *pgid == pid)
    }

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: BOOT.into(),
            start_time: u64::from(pid) * 10,
        }
    }

    impl World {
        /// A live process `pid` (parent `parent`) and an acquire request it holds.
        fn request(&self, pid: u32, parent: Option<u32>) -> HeavyAcquire {
            self.table.insert(pid, parent, Some(u64::from(pid) * 10));
            HeavyAcquire {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "gradle".into(),
                command: format!("build {pid}"),
                cwd: "/work".into(),
                workspace_root: None,
                holder: identity(pid),
                reentrant_token: None,
                no_wait: false,
            }
        }

        fn acquire(&mut self, req: &HeavyAcquire) -> AcquireOutcome {
            self.engine.acquire(req.clone()).unwrap()
        }

        fn state(&self, id: &str) -> Option<HeavyState> {
            self.engine.store.entry(id).unwrap().map(|e| e.state)
        }
    }

    fn release(id: &str) -> HeavyRelease {
        HeavyRelease {
            id: id.into(),
            exit_code: Some(0),
            outcome: HeavyOutcome::Ok,
        }
    }

    /// P5 R2-4: a `Stopping` entry holds its slot, is listed as running, is never reaped,
    /// withdrawn or force-released, and answers as granted.
    #[test]
    fn stopping_occupies_a_slot() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        assert!(matches!(w.acquire(&a), AcquireOutcome::Granted { .. }));
        w.engine
            .store
            .set_entry_state(&a.id, HeavyState::Stopping)
            .unwrap();
        assert!(matches!(w.acquire(&b), AcquireOutcome::Queued { .. }));
        w.engine.tick();
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        let snapshot = w.engine.snapshot().unwrap();
        assert_eq!(snapshot.running.len(), 1);
        assert_eq!(snapshot.running[0].state, HeavyState::Stopping);
        assert_eq!(snapshot.waiting[0].id, b.id);
        for refused in [
            w.engine.withdraw(&a.id, false).unwrap_err(),
            w.engine.force_release(&a.id).unwrap_err(),
        ] {
            assert_eq!(refused.code, ErrorCode::BadRequest);
            assert!(
                refused.message.ends_with("the task is being stopped"),
                "{}",
                refused.message
            );
        }
        // Its holder is gone: still not reaped, the stop decides.
        w.table.remove(10);
        w.engine.tick();
        assert_eq!(w.state(&a.id), Some(HeavyState::Stopping));
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        assert!(matches!(
            w.engine.attach(&a.id).unwrap(),
            AcquireOutcome::Granted { .. }
        ));
        // `no_wait` sees no free slot.
        let mut c = w.request(30, None);
        c.no_wait = true;
        assert!(matches!(w.acquire(&c), AcquireOutcome::Busy { .. }));
    }

    fn not_the_service(_: &StopTarget) -> bool {
        false
    }

    fn verified() -> RunStop {
        RunStop {
            signalled: true,
            gone: true,
            alive: Vec::new(),
            error: None,
        }
    }

    impl World {
        /// `req`'s granted entry starts child `pid` leading group `pid`.
        fn start(&mut self, req: &HeavyAcquire, pid: u32) -> RunGroup {
            self.table
                .insert(pid, Some(req.holder.pid), Some(u64::from(pid) * 10));
            let group = RunGroup::UnixGroup {
                pgid: pid,
                boot_id: BOOT.into(),
            };
            self.engine
                .started(
                    HeavyStarted {
                        id: req.id.clone(),
                        child: identity(pid),
                        group: group.clone(),
                    },
                    None,
                )
                .unwrap();
            group
        }

        fn stop(&mut self, id: &str) -> Result<StopTarget, HostlerError> {
            self.engine.begin_stop(id, id, Some(4242), &not_the_service)
        }

        fn outcome(&self, id: &str) -> Option<(HeavyOutcome, Option<i32>)> {
            self.engine
                .store
                .history_item(id)
                .unwrap()
                .map(|i| (i.outcome, i.exit_code))
        }
    }

    /// P5 T6-R1-1: a report that names anything but the holder's own live command, leading
    /// its own group, is refused and stores nothing.
    #[test]
    fn started_metadata_is_checked() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        w.table.insert(11, Some(10), Some(110));
        w.table.insert(30, Some(1), Some(300));
        let group = |pgid: u32, boot: &str| RunGroup::UnixGroup {
            pgid,
            boot_id: boot.into(),
        };
        let report = |child: ProcessIdentity, group: RunGroup| HeavyStarted {
            id: a.id.clone(),
            child,
            group,
        };
        let refused = |w: &mut World, s: HeavyStarted, peer: Option<u32>, why: &str| {
            let e = w.engine.started(s, peer).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest);
            assert_eq!(e.message, format!("heavy entry {}: {why}", a.id));
            assert_eq!(w.engine.store.entry(&a.id).unwrap().unwrap().child, None);
        };
        // A process that is not running, or not with that start time.
        refused(
            &mut w,
            report(identity(12), group(12, BOOT)),
            None,
            "its command 12 is not running with the reported identity",
        );
        let stale = ProcessIdentity {
            start_time: 1,
            ..identity(11)
        };
        refused(
            &mut w,
            report(stale, group(11, BOOT)),
            None,
            "its command 11 is not running with the reported identity",
        );
        // Another live process, not the holder's child.
        refused(
            &mut w,
            report(identity(30), group(30, BOOT)),
            None,
            "its command 30 is not a child of the entry's holder 10",
        );
        // From another connection's process than the holder.
        refused(
            &mut w,
            report(identity(11), group(11, BOOT)),
            Some(99),
            "the report comes from process 99, not from the entry's holder 10",
        );
        // A group the command does not lead (another live process's group), or of another boot.
        refused(
            &mut w,
            report(identity(11), group(30, BOOT)),
            None,
            "its process group 30 is not led by its command 11",
        );
        refused(
            &mut w,
            report(identity(11), group(11, "another boot")),
            None,
            "its group is of another boot than its command",
        );
        // Not a member of it now.
        w.engine.member = |_, _| false;
        refused(
            &mut w,
            report(identity(11), group(11, BOOT)),
            Some(10),
            "its command 11 is not in the reported group",
        );
        w.engine.member = leads_its_group;
        // The holder gone.
        w.table.remove(10);
        refused(
            &mut w,
            report(identity(11), group(11, BOOT)),
            None,
            "its holder 10 is not running",
        );
        w.table.insert(10, None, Some(100));
        // The real report, from the holder's own connection.
        w.engine
            .started(report(identity(11), group(11, BOOT)), Some(10))
            .unwrap();
        assert_eq!(
            w.engine.store.entry(&a.id).unwrap().unwrap().child,
            Some(identity(11))
        );
    }

    /// Spec §11.1: each refusal comes before anything is marked.
    #[test]
    fn stop_refusals_mark_nothing() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        w.start(&a, 11);
        let refused = |e: HostlerError, message: &str| {
            assert_eq!(e.code, ErrorCode::BadRequest, "{}", e.message);
            assert_eq!(e.message, message);
            e
        };
        refused(
            w.engine
                .begin_stop(&a.id, "nope", None, &not_the_service)
                .unwrap_err(),
            "type the task's id to confirm",
        );
        let e = refused(
            w.stop(&b.id).unwrap_err(),
            &format!("heavy task {} is waiting, not running", b.id),
        );
        assert_eq!(
            e.recovery,
            Some(format!("hostler queue withdraw --id {}", b.id))
        );
        let unknown = uuid::Uuid::new_v4().to_string();
        assert_eq!(w.stop(&unknown).unwrap_err().code, ErrorCode::UnknownEntry);
        let mut looky = w.engine.store.entry(&a.id).unwrap().unwrap();
        looky.id = "looky:7".into();
        looky.source = EntrySource::RnLookyLedger;
        w.foreign.0.lock().unwrap().push(looky);
        refused(
            w.stop("looky:7").unwrap_err(),
            "heavy task looky:7 is rn-looky's; rn-looky's tasks are never stopped by hostler",
        );
        // R2-9: the target the check sees is the entry's run.
        let seen = std::cell::RefCell::new(None);
        let e = w
            .engine
            .begin_stop(&a.id, &a.id, None, &|t: &StopTarget| {
                *seen.borrow_mut() = Some(t.clone());
                true
            })
            .unwrap_err();
        refused(e, "refusing to stop the hostler service itself");
        assert_eq!(
            seen.into_inner(),
            Some(StopTarget::RunGroup {
                claim_id: format!("heavy:{}", a.id),
                holder: identity(10),
                child: Some(identity(11)),
                group: Some(RunGroup::UnixGroup {
                    pgid: 11,
                    boot_id: BOOT.into()
                }),
            })
        );
        assert_eq!(w.state(&a.id), Some(HeavyState::Running));
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        // A finished task.
        w.engine.release(release(&a.id)).unwrap();
        refused(
            w.stop(&a.id).unwrap_err(),
            &format!("heavy task {} has already finished", a.id),
        );
    }

    /// R2-4: while it stops, the entry holds its slot and answers every other change with a
    /// refusal; its holder's release is kept for the `stopped` line.
    #[test]
    fn stop_lifecycle() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        let group = w.start(&a, 11);
        let target = w.stop(&a.id).unwrap();
        assert_eq!(
            target,
            StopTarget::RunGroup {
                claim_id: format!("heavy:{}", a.id),
                holder: identity(10),
                child: Some(identity(11)),
                group: Some(group),
            }
        );
        assert_eq!(w.state(&a.id), Some(HeavyState::Stopping));
        let snapshot = w.engine.snapshot().unwrap();
        assert_eq!(snapshot.running[0].state, HeavyState::Stopping);
        for refused in [
            w.stop(&a.id).unwrap_err(),
            w.engine.withdraw(&a.id, false).unwrap_err(),
            w.engine.force_release(&a.id).unwrap_err(),
        ] {
            assert_eq!(refused.code, ErrorCode::BadRequest);
            assert_eq!(
                refused.message,
                format!("heavy entry {}: the task is being stopped", a.id)
            );
        }
        // A nested `run --heavy` inside it.
        let mut nested = w.request(12, Some(11));
        nested.reentrant_token = Some(a.id.clone());
        let e = w.engine.acquire(nested.clone()).unwrap_err();
        assert_eq!(e.code, ErrorCode::TakenOver);
        assert_eq!(
            e.message,
            format!(
                "heavy task {} is being stopped; nothing may start under it now",
                a.id
            )
        );
        assert_eq!(w.engine.store.entry(&nested.id).unwrap(), None);
        // Its processes end; it is not reaped, its slot is not granted.
        w.table.remove(11);
        w.engine.tick();
        assert_eq!(w.state(&a.id), Some(HeavyState::Stopping));
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        // Its holder reports the exit; the entry stays until the stop records it.
        w.engine
            .release(HeavyRelease {
                id: a.id.clone(),
                exit_code: Some(143),
                outcome: HeavyOutcome::Failed,
            })
            .unwrap();
        assert_eq!(w.state(&a.id), Some(HeavyState::Stopping));
        assert_eq!(w.outcome(&a.id), None);
        w.table.remove(10);
        let outcome = w.engine.finish_stop(&a.id, &verified()).unwrap();
        assert_eq!(
            outcome,
            QueueStopOutcome {
                id: a.id.clone(),
                stopped: true,
                pids: vec![11],
            }
        );
        assert_eq!(w.outcome(&a.id), Some((HeavyOutcome::Stopped, Some(143))));
        assert_eq!(w.state(&b.id), Some(HeavyState::Running));
        // A late release changes nothing.
        w.engine.release(release(&a.id)).unwrap();
        assert_eq!(w.outcome(&a.id), Some((HeavyOutcome::Stopped, Some(143))));
    }

    /// Granted, its command not started: the holder is the target, and the command may not
    /// start during the stop.
    #[test]
    fn a_stop_before_the_command_started_keeps_it_from_starting() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        let target = w.stop(&a.id).unwrap();
        assert_eq!(
            target,
            StopTarget::RunGroup {
                claim_id: format!("heavy:{}", a.id),
                holder: identity(10),
                child: None,
                group: None,
            }
        );
        w.table.insert(11, Some(10), Some(110));
        let e = w
            .engine
            .started(
                HeavyStarted {
                    id: a.id.clone(),
                    child: identity(11),
                    group: RunGroup::UnixGroup {
                        pgid: 11,
                        boot_id: BOOT.into(),
                    },
                },
                None,
            )
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::TakenOver);
        assert_eq!(w.engine.store.entry(&a.id).unwrap().unwrap().child, None);
        let outcome = w.engine.finish_stop(&a.id, &verified()).unwrap();
        assert_eq!(outcome.pids, vec![10]);
        assert_eq!(w.outcome(&a.id), Some((HeavyOutcome::Stopped, None)));
    }

    /// P5-L10: a stop that found everything already gone signalled nothing; the task ended on
    /// its own and is recorded `reaped` (its holder's exit code kept), never `stopped`. A
    /// force-release records `force_released`.
    #[test]
    fn only_a_signalled_stop_records_stopped() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        w.start(&a, 11);
        w.stop(&a.id).unwrap();
        w.engine
            .release(HeavyRelease {
                id: a.id.clone(),
                exit_code: Some(0),
                outcome: HeavyOutcome::Ok,
            })
            .unwrap();
        let ended = RunStop {
            signalled: false,
            ..verified()
        };
        let outcome = w.engine.finish_stop(&a.id, &ended).unwrap();
        assert_eq!(
            outcome,
            QueueStopOutcome {
                id: a.id.clone(),
                stopped: false,
                pids: vec![11],
            }
        );
        assert_eq!(w.outcome(&a.id), Some((HeavyOutcome::Reaped, Some(0))));
        assert_eq!(w.state(&b.id), Some(HeavyState::Running));
        w.engine.force_release(&b.id).unwrap();
        assert_eq!(w.outcome(&b.id), Some((HeavyOutcome::ForceReleased, None)));
    }

    /// A stop that could not verify: the entry runs again, keeps its slot, and can be stopped
    /// again; nothing is recorded.
    #[test]
    fn a_failed_stop_keeps_the_slot() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        w.start(&a, 11);
        w.stop(&a.id).unwrap();
        // A release during it is not kept past it.
        w.engine
            .release(HeavyRelease {
                id: a.id.clone(),
                exit_code: Some(1),
                outcome: HeavyOutcome::Failed,
            })
            .unwrap();
        let failed = RunStop {
            signalled: false,
            gone: false,
            alive: vec![10, 11],
            error: Some(StopError::PermissionDenied("no".into())),
        };
        let e = w.engine.finish_stop(&a.id, &failed).unwrap_err();
        assert_eq!(e.code, ErrorCode::StopFailed);
        assert_eq!(e.details["pids"], json!([10, 11]));
        assert_eq!(w.state(&a.id), Some(HeavyState::Running));
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        assert_eq!(w.outcome(&a.id), None);
        w.stop(&a.id).unwrap();
        w.engine.finish_stop(&a.id, &verified()).unwrap();
        assert_eq!(w.outcome(&a.id), Some((HeavyOutcome::Stopped, None)));
    }

    /// A restart abandons a stop: the entry runs again, and once its processes are gone it is
    /// reaped, never recorded as stopped.
    #[test]
    fn restart_abandons_a_stop() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        w.start(&a, 11);
        w.stop(&a.id).unwrap();
        let World {
            engine,
            table,
            clock,
            ..
        } = w;
        let HeavyEngine { store, .. } = engine;
        let (tx, _events) = crate::service::events::EventBus::channel(64);
        let mut engine = HeavyEngine::new(store, table.clone(), clock, Box::new(NoForeign), tx);
        let entry = engine.store.entry(&a.id).unwrap().unwrap();
        assert_eq!(entry.state, HeavyState::Running);
        // Its holder's release now finishes it as usual: no stop is in progress.
        assert!(engine.stops.is_empty());
        table.remove(10);
        table.remove(11);
        engine.tick();
        assert_eq!(
            engine.store.history_item(&a.id).unwrap().unwrap().outcome,
            HeavyOutcome::Reaped
        );
    }

    #[test]
    fn second_acquire_queues_when_capacity_one() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        assert!(matches!(w.acquire(&a), AcquireOutcome::Granted { .. }));
        match w.acquire(&b) {
            AcquireOutcome::Queued {
                entry,
                position,
                of,
            } => {
                assert_eq!((position, of), (1, 1));
                assert_eq!(entry.id, b.id);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn release_grants_next_in_ticket_order() {
        let mut w = world();
        let reqs: Vec<_> = (1..=3).map(|i| w.request(i * 10, None)).collect();
        for r in &reqs {
            w.acquire(r);
        }
        w.engine.release(release(&reqs[0].id)).unwrap();
        assert_eq!(w.state(&reqs[1].id), Some(HeavyState::Running));
        assert_eq!(w.state(&reqs[2].id), Some(HeavyState::Waiting));
        // Idempotent: a second release of a finished id is fine.
        w.engine.release(release(&reqs[0].id)).unwrap();
        assert_eq!(
            w.engine
                .store
                .history_item(&reqs[0].id)
                .unwrap()
                .unwrap()
                .outcome,
            HeavyOutcome::Ok
        );
    }

    #[test]
    fn acquire_same_id_twice_returns_same_entry() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        let AcquireOutcome::Granted { entry: first } = w.acquire(&a) else {
            panic!()
        };
        let AcquireOutcome::Granted { entry: again } = w.acquire(&a) else {
            panic!()
        };
        assert_eq!(first.ticket, again.ticket);
        let AcquireOutcome::Queued { entry: queued, .. } = w.acquire(&b) else {
            panic!()
        };
        let AcquireOutcome::Queued {
            entry: requeued, ..
        } = w.acquire(&b)
        else {
            panic!()
        };
        assert_eq!(queued.ticket, requeued.ticket);
        assert_eq!(w.engine.store.entries().unwrap().len(), 2);
    }

    #[test]
    fn acquire_finished_id_returns_finished() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        w.engine.release(release(&a.id)).unwrap();
        match w.acquire(&a) {
            AcquireOutcome::Finished { item } => assert_eq!(item.outcome, HeavyOutcome::Ok),
            other => panic!("{other:?}"),
        }
        assert!(w.engine.store.entries().unwrap().is_empty());
        assert!(matches!(
            w.engine.attach(&a.id).unwrap(),
            AcquireOutcome::Finished { .. }
        ));
    }

    #[test]
    fn malformed_id_or_kind_is_bad_request() {
        let mut w = world();
        let mut a = w.request(10, None);
        a.id = "not-a-uuid".into();
        assert_eq!(
            w.engine.acquire(a.clone()).unwrap_err().code,
            ErrorCode::BadRequest
        );
        a.id = uuid::Uuid::new_v4().to_string();
        a.kind = "Gradle Build".into();
        assert_eq!(w.engine.acquire(a).unwrap_err().code, ErrorCode::BadRequest);
    }

    #[test]
    fn attach_unknown_id_is_unknown_entry() {
        let mut w = world();
        let err = w
            .engine
            .attach(&uuid::Uuid::new_v4().to_string())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownEntry);
    }

    #[test]
    fn no_wait_returns_busy_without_entry() {
        let mut w = world();
        let a = w.request(10, None);
        let mut b = w.request(20, None);
        b.no_wait = true;
        w.acquire(&a);
        assert_eq!(w.acquire(&b), AcquireOutcome::Busy { position: 1, of: 1 });
        assert_eq!(w.engine.store.entries().unwrap().len(), 1);
        // With a free slot, no_wait is granted normally.
        w.engine.release(release(&a.id)).unwrap();
        assert!(matches!(w.acquire(&b), AcquireOutcome::Granted { .. }));
    }

    #[test]
    fn reentrant_token_with_ancestor_holder_is_noop() {
        let mut w = world();
        let outer = w.request(10, None);
        w.acquire(&outer);
        // 30 (nested wrapper) <- 20 (the outer command) <- 10 (the outer wrapper).
        w.table.insert(20, Some(10), Some(200));
        let mut inner = w.request(30, Some(20));
        inner.reentrant_token = Some(outer.id.clone());
        assert_eq!(w.acquire(&inner), AcquireOutcome::Reentrant);
        assert_eq!(w.engine.store.entries().unwrap().len(), 1);
        // One walk up the tree: one parent lookup (one process snapshot on Windows).
        assert_eq!(w.table.parent_lookups(), 1);
    }

    #[test]
    fn reentrant_token_without_ancestor_is_ignored() {
        let mut w = world();
        let outer = w.request(10, None);
        w.acquire(&outer);
        let mut stranger = w.request(30, Some(1));
        stranger.reentrant_token = Some(outer.id.clone());
        assert!(matches!(
            w.acquire(&stranger),
            AcquireOutcome::Queued { .. }
        ));
    }

    #[test]
    fn dead_holder_reaped_and_next_granted() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        w.table.remove(10);
        w.engine.tick();
        assert_eq!(w.state(&b.id), Some(HeavyState::Running));
        assert_eq!(
            w.engine.store.history_item(&a.id).unwrap().unwrap().outcome,
            HeavyOutcome::Reaped
        );
    }

    #[test]
    fn holder_and_child_dead_group_alive_keeps_slot() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.table.insert(11, Some(10), Some(110));
        let group = RunGroup::UnixGroup {
            pgid: 11,
            boot_id: BOOT.into(),
        };
        w.engine
            .started(
                HeavyStarted {
                    id: a.id.clone(),
                    child: identity(11),
                    group: group.clone(),
                },
                None,
            )
            .unwrap();
        w.acquire(&b);
        w.table.set_group_alive(&group, true);
        w.table.remove(10);
        w.table.set_zombie(11, true);
        w.engine.tick();
        assert_eq!(w.state(&a.id), Some(HeavyState::Running));
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        w.table.set_group_alive(&group, false);
        w.engine.tick();
        assert_eq!(w.state(&a.id), None);
        assert_eq!(w.state(&b.id), Some(HeavyState::Running));
    }

    #[test]
    fn foreign_running_blocks_grant() {
        let mut w = world();
        let foreign = HeavyEntry {
            id: "looky:1".into(),
            ticket: 0,
            kind: "gradle".into(),
            command: "yarn android".into(),
            cwd: "/other".into(),
            workspace_root: None,
            holder: identity(99),
            child: None,
            group: None,
            state: HeavyState::Running,
            enqueued_at_ms: 0,
            granted_at_ms: Some(0),
            source: EntrySource::RnLookyLedger,
        };
        w.foreign.0.lock().unwrap().push(foreign);
        let a = w.request(10, None);
        assert!(matches!(w.acquire(&a), AcquireOutcome::Queued { .. }));
        assert_eq!(w.engine.snapshot().unwrap().foreign_running.len(), 1);
        w.foreign.0.lock().unwrap().clear();
        w.engine.tick();
        assert_eq!(w.state(&a.id), Some(HeavyState::Running));
    }

    #[test]
    fn capacity_out_of_range_rejected() {
        let mut w = world();
        for n in [0, 17] {
            assert_eq!(
                w.engine.set_capacity(n).unwrap_err().code,
                ErrorCode::CapacityOutOfRange
            );
        }
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        w.engine.set_capacity(2).unwrap();
        assert_eq!(w.state(&b.id), Some(HeavyState::Running));
        assert_eq!(w.engine.snapshot().unwrap().capacity, 2);
    }

    #[test]
    fn withdraw_timed_out_records_timed_out() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        w.acquire(&b);
        w.clock.0.store(5_000, Ordering::SeqCst);
        w.engine.withdraw(&b.id, true).unwrap();
        let item = w.engine.store.history_item(&b.id).unwrap().unwrap();
        assert_eq!(item.outcome, HeavyOutcome::TimedOut);
        assert_eq!(item.finished_at_ms, 5_000);
        // Idempotent; a running entry with a child is not withdrawn.
        w.engine.withdraw(&b.id, true).unwrap();
        w.start(&a, 11);
        assert_eq!(
            w.engine.withdraw(&a.id, false).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn changed_snapshot_broadcasts_once() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        let mut changes = 0;
        while let Ok(Event::QueueChanged(_)) = w.events.try_recv() {
            changes += 1;
        }
        assert!(changes >= 1);
        w.engine.tick();
        w.engine.tick();
        assert!(w.events.try_recv().is_err(), "an unchanged tick broadcast");
        w.engine.release(release(&a.id)).unwrap();
        assert!(matches!(w.events.try_recv(), Ok(Event::QueueChanged(s)) if s.running.is_empty()));
        assert!(w.events.try_recv().is_err());
    }

    #[test]
    fn idle_shutdown_then_racing_acquire_is_never_granted() {
        let mut w = world();
        assert!(w.engine.begin_shutdown(true).unwrap().is_empty());
        // A request racing in after the accepted stop is persisted but stays waiting, for the
        // next service to grant.
        let b = w.request(20, None);
        assert!(matches!(w.acquire(&b), AcquireOutcome::Queued { .. }));
        w.engine.tick();
        assert_eq!(w.state(&b.id), Some(HeavyState::Waiting));
        assert!(matches!(
            w.engine.attach(&b.id).unwrap(),
            AcquireOutcome::Queued { .. }
        ));
        let mut c = w.request(30, None);
        c.no_wait = true;
        assert!(matches!(w.acquire(&c), AcquireOutcome::Busy { .. }));
    }

    #[test]
    fn grant_before_idle_shutdown_refuses_it() {
        let mut w = world();
        let (a, b) = (w.request(10, None), w.request(20, None));
        w.acquire(&a);
        let refusing = w.engine.begin_shutdown(true).unwrap();
        assert_eq!(refusing.len(), 1);
        assert_eq!(refusing[0].id, a.id);
        // Refused: the engine keeps granting.
        w.acquire(&b);
        w.engine.release(release(&a.id)).unwrap();
        assert_eq!(w.state(&b.id), Some(HeavyState::Running));
        // A forced stop is accepted while entries run.
        assert!(w.engine.begin_shutdown(false).unwrap().is_empty());
    }

    #[test]
    fn oversize_command_or_paths_rejected() {
        let mut w = world();
        let ok = w.request(10, None);
        let cases: [fn(&mut HeavyAcquire); 3] = [
            |r| r.command = "x".repeat(MAX_COMMAND_BYTES + 1),
            |r| r.cwd = "/".repeat(MAX_PATH_BYTES + 1),
            |r| r.workspace_root = Some("/".repeat(MAX_PATH_BYTES + 1)),
        ];
        for change in cases {
            let mut r = ok.clone();
            change(&mut r);
            assert_eq!(w.engine.acquire(r).unwrap_err().code, ErrorCode::BadRequest);
        }
        let mut max = ok.clone();
        max.command = "x".repeat(MAX_COMMAND_BYTES);
        max.cwd = "/".repeat(MAX_PATH_BYTES);
        max.workspace_root = Some("/".repeat(MAX_PATH_BYTES));
        assert!(matches!(w.acquire(&max), AcquireOutcome::Granted { .. }));
    }

    #[test]
    fn live_entries_capped_at_512() {
        let mut w = world();
        let holder = w.request(10, None);
        let mut first = None;
        for _ in 0..MAX_LIVE_ENTRIES {
            let mut r = holder.clone();
            r.id = uuid::Uuid::new_v4().to_string();
            w.acquire(&r);
            first.get_or_insert(r);
        }
        let mut over = holder.clone();
        over.id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            w.engine.acquire(over).unwrap_err().code,
            ErrorCode::QueueBusy
        );
        assert_eq!(w.engine.store.entries().unwrap().len(), MAX_LIVE_ENTRIES);
        // A known id still answers while the queue is full.
        assert!(matches!(
            w.acquire(&first.unwrap()),
            AcquireOutcome::Granted { .. }
        ));
    }

    #[test]
    fn json_len_matches_serde_json() {
        let mut samples: Vec<String> = (0u8..=0x7f).map(|b| char::from(b).to_string()).collect();
        samples
            .extend(["", "plain", "a\"b\\c", "é漢🦀", "\u{1}\u{1f}\n\t\u{7f}"].map(String::from));
        samples.push((0u8..=0x7f).map(char::from).collect());
        for s in &samples {
            let serde = serde_json::to_string(s).unwrap().len() - 2;
            assert_eq!(json_len(s), serde, "{s:?}");
        }
    }

    #[test]
    fn truncate_to_json_len_keeps_the_longest_fitting_prefix() {
        let s = "ab\u{1}c🦀\"d";
        for max in 0..=json_len(s) + 1 {
            let cut = truncate_to_json_len(s, max);
            assert!(json_len(cut) <= max, "{max}");
            assert!(s.starts_with(cut));
            if let Some(next) = s[cut.len()..].chars().next() {
                assert!(
                    json_len(cut) + json_char_len(next) > max,
                    "{max}: not the longest"
                );
            }
        }
    }

    #[test]
    fn escaped_oversize_fields_are_bad_request() {
        let mut w = world();
        let ok = w.request(10, None);
        // 171 control characters are 171 bytes in memory but 1026 as JSON.
        let cases: [fn(&mut HeavyAcquire); 4] = [
            |r| r.command = "\u{1}".repeat(171),
            |r| r.cwd = "\"".repeat(1025),
            |r| r.workspace_root = Some("\u{1f}".repeat(342)),
            |r| r.holder.boot_id = "\u{1}".repeat(43),
        ];
        for change in cases {
            let mut r = ok.clone();
            change(&mut r);
            assert_eq!(w.engine.acquire(r).unwrap_err().code, ErrorCode::BadRequest);
        }
        let mut max = ok.clone();
        max.command = "\u{1}".repeat(170) + "\"\"";
        max.cwd = "\"".repeat(1024);
        assert_eq!(json_len(&max.command), MAX_COMMAND_BYTES);
        assert!(matches!(w.acquire(&max), AcquireOutcome::Granted { .. }));
        let oversize_job = HeavyStarted {
            id: max.id.clone(),
            child: identity(10),
            group: RunGroup::WindowsJob {
                name: "\u{1}".repeat(43),
                boot_id: BOOT.into(),
                session: None,
            },
        };
        assert_eq!(
            w.engine.started(oversize_job, None).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn foreign_entries_are_clamped_in_snapshots() {
        let w = world();
        let foreign = HeavyEntry {
            id: "looky:".to_owned() + &"\u{1}".repeat(1000),
            ticket: 0,
            kind: "\u{1}".repeat(1000),
            command: "\u{1}".repeat(10_000),
            cwd: "\"".repeat(10_000),
            workspace_root: None,
            holder: identity(99),
            child: None,
            group: None,
            state: HeavyState::Running,
            enqueued_at_ms: 0,
            granted_at_ms: Some(0),
            source: EntrySource::RnLookyLedger,
        };
        w.foreign
            .0
            .lock()
            .unwrap()
            .extend(std::iter::repeat_n(foreign, MAX_LIVE_ENTRIES + 10));
        let snapshot = w.engine.snapshot().unwrap();
        assert_eq!(snapshot.foreign_running.len(), MAX_LIVE_ENTRIES);
        let e = &snapshot.foreign_running[0];
        assert!(json_len(&e.command) <= MAX_COMMAND_BYTES);
        assert!(json_len(&e.cwd) <= MAX_PATH_BYTES);
        assert!(json_len(&e.id) <= MAX_IDENTITY_BYTES);
        assert!(json_len(&e.kind) <= MAX_IDENTITY_BYTES);
    }

    fn started_for(id: &str, child: u32) -> HeavyStarted {
        HeavyStarted {
            id: id.into(),
            child: identity(child),
            group: RunGroup::UnixGroup {
                pgid: child,
                boot_id: BOOT.into(),
            },
        }
    }

    #[test]
    fn started_on_a_live_entry_is_idempotent_for_the_same_child() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        w.table.insert(11, Some(10), Some(110));
        w.engine.started(started_for(&a.id, 11), None).unwrap();
        w.engine.started(started_for(&a.id, 11), None).unwrap();
        assert_eq!(
            w.engine
                .started(started_for(&a.id, 12), None)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        let entry = w.engine.store.entry(&a.id).unwrap().unwrap();
        assert_eq!(entry.child, Some(identity(11)));
    }

    /// L31: the engine holds a handle on an entry's group from `heavy.started` until the entry
    /// ends; the tick keeps only live entries' (and reopens them after a restart).
    #[test]
    fn run_groups_are_held_while_their_entries_live() {
        let mut w = world();
        let a = w.request(10, None);
        w.acquire(&a);
        w.table.insert(11, Some(10), Some(110));
        let s = started_for(&a.id, 11);
        w.engine.started(s.clone(), None).unwrap();
        assert_eq!(w.table.held("heavy"), vec![s.group.clone()]);
        w.engine.tick();
        assert_eq!(w.table.held("heavy"), vec![s.group.clone()]);
        w.engine.release(release(&a.id)).unwrap();
        assert!(w.table.held("heavy").is_empty());
    }

    #[test]
    fn started_after_a_finish_without_a_command_is_withdrawn() {
        let mut w = world();
        // Granted, then finished before `heavy.started`: by an operator, a timeout, a force
        // release, or reaping (holder gone).
        type Finish = fn(&mut World, &str);
        let finishers: [(HeavyOutcome, Finish); 4] = [
            (HeavyOutcome::Withdrawn, |w, id| {
                w.engine.withdraw(id, false).unwrap()
            }),
            (HeavyOutcome::TimedOut, |w, id| {
                w.engine.withdraw(id, true).unwrap()
            }),
            (HeavyOutcome::ForceReleased, |w, id| {
                w.engine.force_release(id).unwrap()
            }),
            (HeavyOutcome::Reaped, |w, id| {
                let holder = w.engine.store.entry(id).unwrap().unwrap().holder.pid;
                w.table.remove(holder);
                w.engine.tick();
            }),
        ];
        for (i, (outcome, finish)) in finishers.into_iter().enumerate() {
            let pid = 100 + i as u32;
            let req = w.request(pid, None);
            assert!(matches!(w.acquire(&req), AcquireOutcome::Granted { .. }));
            finish(&mut w, &req.id);
            let err = w
                .engine
                .started(started_for(&req.id, pid + 50), None)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Withdrawn, "{outcome:?}");
            assert_eq!(err.details["outcome"], json!(outcome), "{outcome:?}");
            assert_eq!(err.recovery, None);
        }
    }

    #[test]
    fn started_for_an_unknown_id_is_unknown_entry() {
        let mut w = world();
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            w.engine
                .started(started_for(&id, 11), None)
                .unwrap_err()
                .code,
            ErrorCode::UnknownEntry
        );
    }

    #[test]
    fn a_tick_reads_each_entry_once_and_never_walks_parents() {
        let mut w = world();
        let n = 50;
        for i in 0..n {
            let req = w.request(1_000 + i, None);
            w.acquire(&req);
        }
        let (infos, parents) = (w.table.info_calls(), w.table.parent_lookups());
        w.engine.tick();
        // Live holders: one `info` each, and no parent walks (no snapshots on Windows).
        assert_eq!(w.table.info_calls() - infos, n as usize);
        assert_eq!(w.table.parent_lookups(), parents);
        assert_eq!(parents, 0, "plain acquires walked the process tree");

        // Dead holders (all reaped): still a bounded number of `info` calls per entry.
        for i in 0..n {
            w.table.remove(1_000 + i);
        }
        let infos = w.table.info_calls();
        w.engine.tick();
        assert!(w.table.info_calls() - infos <= 2 * n as usize);
        assert_eq!(w.table.parent_lookups(), 0);
    }
}
