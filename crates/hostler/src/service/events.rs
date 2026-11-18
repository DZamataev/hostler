//! Event delivery of contract 5 (P5, spec §6.2): every event gets a `seq`, snapshots name the
//! point they were taken at, and a slow connection's backlog keeps what a client cannot
//! rebuild from a snapshot. Whatever cannot be delivered becomes one `resync`.
//!
//! **The rule for publishers** (spec §6.2; clean reads C-A1, R-1). Two phases:
//! 1. **Capture.** The publisher reads its state and clones it into a finished value, holding
//!    the lock that owns that state (an engine's mutex, the secrets store guard,
//!    `last_event` for devices, …). No bus lock is held: SQLite, file and cache reads never
//!    stall another engine's events.
//! 2. **Commit.** Still holding its owning lock, it hands the finished value to the bus, which
//!    numbers and broadcasts it in one short section (`publish`, `publish_snapshot`: a
//!    snapshot takes its own number as `as_of_seq` through `Snapshot::numbered`, a field
//!    assignment). A snapshot answer reads `current_seq()` at the same place.
//!
//! The numbering lock is a leaf: nothing is acquired, read or waited for while it is held, so
//! no lock cycle can pass through it, and a slow capture delays only its own engine. Numbers
//! are given at commit, so the channel's order is the numbers' order with no reservation to
//! wait for.
//!
//! **What a snapshot's point means.** Every event of a kind is committed under the lock that
//! owns that kind's state, and every snapshot of that kind is captured and given its point
//! under the same lock. So a snapshot of a kind with `as_of_seq = n` reflects every event of
//! that kind numbered `<= n` and none numbered `> n`, which is what a §6.2 client relies on
//! (it replays, per kind, the held events numbered above the snapshot's point). A snapshot
//! holds only its own kinds' state; events of other kinds are not in it, whatever their
//! numbers.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast};

use crate::contract::wire::{Event, ResyncReason, ServerFrame};

use super::server::MAX_FRAME_BYTES;

/// The broadcast channel's capacity; `HOSTLER_TEST_EVENTS_CAP` in debug builds.
pub const EVENTS_CAPACITY: usize = 64;
/// A connection's backlog: at most this many events …
pub const BACKLOG_EVENTS: usize = 256;
/// … and this many bytes of encoded lines.
pub const BACKLOG_BYTES: usize = 32 << 20;

/// A captured snapshot that takes its event number as its point (`as_of_seq`) at commit: a
/// field assignment, so the numbering section does no other work.
pub trait Snapshot {
    fn numbered(self, seq: u64) -> Event;
}

impl Snapshot for crate::contract::heavy::QueueSnapshot {
    fn numbered(mut self, seq: u64) -> Event {
        self.as_of_seq = seq;
        Event::QueueChanged(self)
    }
}

impl Snapshot for crate::contract::claims::ClaimsSnapshot {
    fn numbered(mut self, seq: u64) -> Event {
        self.as_of_seq = seq;
        Event::ClaimsChanged(self)
    }
}

impl Snapshot for crate::contract::devices::DevicesSnapshot {
    fn numbered(mut self, seq: u64) -> Event {
        self.as_of_seq = seq;
        Event::DevicesChanged(Box::new(self))
    }
}

impl Snapshot for crate::contract::secrets::SecretsStatus {
    fn numbered(mut self, seq: u64) -> Event {
        self.as_of_seq = seq;
        Event::SecretsLockChanged(self)
    }
}

impl Snapshot for crate::contract::projects::ProjectsSnapshot {
    fn numbered(mut self, seq: u64) -> Event {
        self.as_of_seq = seq;
        Event::ProjectsChanged(Box::new(self))
    }
}

/// One published event with its number.
#[derive(Clone, Debug)]
pub struct Published {
    pub seq: u64,
    pub event: Event,
}

struct Inner {
    tx: broadcast::Sender<Published>,
    /// The last `seq` assigned; numbering and sending happen under it, so the channel's order
    /// is the numbers' order.
    seq: Mutex<u64>,
    /// Set when a project, a workspace or a declaration changed; the server publishes
    /// `projects.changed` once it sees it (`take_projects_dirty`).
    projects_dirty: AtomicBool,
}

/// The service-wide event channel. Cheap to clone; every engine holds one.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl EventBus {
    pub fn new(capacity: usize) -> EventBus {
        let (tx, _) = broadcast::channel(capacity.max(1));
        EventBus {
            inner: Arc::new(Inner {
                tx,
                seq: Mutex::new(0),
                projects_dirty: AtomicBool::new(false),
            }),
        }
    }

    /// The capacity the service runs with: `EVENTS_CAPACITY`, or `HOSTLER_TEST_EVENTS_CAP` in
    /// debug builds.
    pub fn service_capacity() -> usize {
        #[cfg(debug_assertions)]
        if let Some(cap) = std::env::var("HOSTLER_TEST_EVENTS_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|cap| *cap > 0)
        {
            return cap;
        }
        EVENTS_CAPACITY
    }

    /// Commits a captured request or record event (never a snapshot: those go through
    /// `publish_snapshot`). Returns its number. No receivers is fine.
    pub fn publish(&self, event: Event) -> u64 {
        debug_assert!(
            event.as_of_seq().is_none(),
            "a {} snapshot is published with publish_snapshot",
            event.kind()
        );
        self.commit(|_| event)
    }

    /// Commits a captured snapshot: it takes its number as its point (`as_of_seq`).
    pub fn publish_snapshot(&self, snapshot: impl Snapshot) -> u64 {
        self.commit(|seq| snapshot.numbered(seq))
    }

    /// The numbering section (R-1): number and broadcast, nothing else. `event` only assembles
    /// an already-captured value (`Snapshot::numbered` or a moved event); the broadcast never
    /// waits.
    fn commit(&self, event: impl FnOnce(u64) -> Event) -> u64 {
        let mut last = self
            .inner
            .seq
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let seq = *last + 1;
        *last = seq;
        let _ = self.inner.tx.send(Published {
            seq,
            event: event(seq),
        });
        seq
    }

    /// Tests: publishes any event; a snapshot gets its own number as `as_of_seq`.
    #[cfg(test)]
    pub fn publish_test(&self, event: Event) -> u64 {
        self.commit(|seq| match event {
            Event::QueueChanged(s) => s.numbered(seq),
            Event::SecretsLockChanged(s) => s.numbered(seq),
            Event::ClaimsChanged(s) => s.numbered(seq),
            Event::DevicesChanged(s) => (*s).numbered(seq),
            Event::ProjectsChanged(s) => (*s).numbered(seq),
            other => other,
        })
    }

    /// The last number assigned (0 before the first event). A snapshot answer reads it after
    /// its capture, still holding the lock its kind's events are committed under: that is its
    /// point (`as_of_seq`).
    pub fn current_seq(&self) -> u64 {
        *self
            .inner
            .seq
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Published> {
        self.inner.tx.subscribe()
    }

    pub fn mark_projects_dirty(&self) {
        self.inner.projects_dirty.store(true, Ordering::SeqCst);
    }

    /// Whether `projects.changed` is owed, clearing the mark.
    pub fn take_projects_dirty(&self) -> bool {
        self.inner.projects_dirty.swap(false, Ordering::SeqCst)
    }
}

/// Tests: the bus with one receiver that yields the events alone, as the engines' tests read
/// them (`try_recv`, `recv`).
#[cfg(test)]
pub struct TestEvents(broadcast::Receiver<Published>);

#[cfg(test)]
impl TestEvents {
    pub fn try_recv(&mut self) -> Result<Event, broadcast::error::TryRecvError> {
        self.0.try_recv().map(|p| p.event)
    }

    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        self.0.recv().await.map(|p| p.event)
    }
}

#[cfg(test)]
impl EventBus {
    pub fn channel(capacity: usize) -> (EventBus, TestEvents) {
        let bus = EventBus::new(capacity);
        let rx = bus.subscribe();
        (bus, TestEvents(rx))
    }
}

/// What a waiting event is kept by (spec §6.2): the newest snapshot per kind, the newest state
/// per request id, every record.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Snapshot(&'static str),
    Request(&'static str, String),
    Record(u64),
    Resync,
}

fn key_of(seq: u64, event: &Event) -> Key {
    match event {
        Event::QueueChanged(_)
        | Event::ClaimsChanged(_)
        | Event::DevicesChanged(_)
        | Event::SecretsLockChanged(_)
        | Event::ProjectsChanged(_) => Key::Snapshot(event.kind()),
        Event::SecretRequestChanged(info) => Key::Request(event.kind(), info.id.clone()),
        Event::AccountRequestChanged(info) => Key::Request(event.kind(), info.id.clone()),
        Event::ClaimsSeized(_) | Event::DevicesInstalled(_) => Key::Record(seq),
        Event::Resync { .. } => Key::Resync,
    }
}

/// Bytes kept back for control lines, which take no permits: the `resync` (at most one
/// outstanding: a new one is queued only after the last was written) and the shutdown reply.
/// A control line is never refused or delayed for want of room (R-3).
pub const CONTROL_RESERVE: usize = 1024;
/// Bytes kept for replies (R-2): one maximum frame, so a reply, the largest sensitive answer
/// included, never waits behind event lines, waiting or owned by the writer.
pub const REPLY_RESERVE: usize = MAX_FRAME_BYTES;
/// The events' share: pending events and event lines in the writer's queue.
pub const EVENT_BYTES: usize = BACKLOG_BYTES - REPLY_RESERVE - CONTROL_RESERVE;

/// One of a connection's outbound byte budgets (spec §6.2's 32 MiB; clean reads C-A2, R-2):
/// `Outbound::events()` (`EVENT_BYTES`) and `Outbound::replies()` (`REPLY_RESERVE`); with
/// `CONTROL_RESERVE` they add up to `BACKLOG_BYTES`. A line holds permits for its bytes from
/// the moment it is queued until it is written, so a connection never holds more.
#[derive(Clone)]
pub struct Outbound(Arc<Semaphore>, usize);

/// A line's share of a budget, released when the line is written or dropped; `None` for a
/// control line.
pub type LineBytes = Option<OwnedSemaphorePermit>;

impl Outbound {
    pub fn with_capacity(bytes: usize) -> Outbound {
        Outbound(Arc::new(Semaphore::new(bytes)), bytes)
    }

    pub fn events() -> Outbound {
        Outbound::with_capacity(EVENT_BYTES)
    }

    pub fn replies() -> Outbound {
        Outbound::with_capacity(REPLY_RESERVE)
    }

    /// Room for `bytes` now, or `None`.
    pub fn try_take(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        let n = u32::try_from(bytes).ok()?;
        self.0.clone().try_acquire_many_owned(n).ok()
    }

    /// Room for `bytes` (at most the whole budget), once lines of this budget were written.
    pub async fn take(&self, bytes: usize) -> OwnedSemaphorePermit {
        let n = u32::try_from(bytes.min(self.1)).unwrap_or(u32::MAX);
        self.0
            .clone()
            .acquire_many_owned(n)
            .await
            .expect("the budget is never closed")
    }

    pub fn available(&self) -> usize {
        self.0.available_permits()
    }

    /// The bytes held now, waiting or in the writer.
    pub fn in_use(&self) -> usize {
        self.1 - self.available()
    }
}

impl Default for Outbound {
    fn default() -> Outbound {
        Outbound::events()
    }
}

/// One connection's events not yet handed to its writer, in increasing `seq`.
#[derive(Default)]
pub struct PendingEvents {
    /// `seq` → (key, line, its bytes of the budget).
    lines: BTreeMap<u64, (Key, String, LineBytes)>,
    by_key: HashMap<Key, u64>,
    /// The bytes of `lines` (for tests and the log).
    bytes: usize,
    /// The connection's budget, shared with its replies and its writer queue (C-A2).
    budget: Outbound,
    /// Events with a number up to this one are covered by a `resync` already queued or sent.
    covered_through: u64,
    /// A `resync` handed to the writer and not yet written (the writer answers once it is):
    /// while it is unwritten, it covers every later trigger too, so the reader still gets one.
    unwritten_resync: Option<tokio::sync::oneshot::Receiver<()>>,
    /// Debug builds: `HOSTLER_TEST_EVENT_PAD_BYTES=<kind>:<bytes>` counts that many extra bytes
    /// for every event of that kind, so a test can push one over the frame limit.
    pad: Option<(String, usize)>,
}

impl PendingEvents {
    /// With a budget of its own (tests).
    pub fn new() -> PendingEvents {
        PendingEvents::with_budget(Outbound::events())
    }

    /// With the connection's budget.
    pub fn with_budget(budget: Outbound) -> PendingEvents {
        PendingEvents {
            budget,
            pad: test_pad(),
            ..PendingEvents::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Adds a received event: it replaces the waiting one of its key and moves to the end, or
    /// it is folded into a waiting `resync`, or, when the backlog would pass 256 events or the
    /// connection's outbound bytes would pass the budget, it turns the backlog into one
    /// (`now_seq` is the bus's current number).
    pub fn push(&mut self, published: &Published, now_seq: u64) {
        let Published { seq, event } = published;
        if *seq <= self.covered_through {
            return;
        }
        if self.by_key.contains_key(&Key::Resync) {
            // A resync is waiting for the writer: it stands for this event too.
            self.resync_at(self.resync_reason(), *seq);
            return;
        }
        let line = match encode(*seq, event, self.pad.as_ref()) {
            Ok(line) => line,
            Err(size) => {
                match size {
                    Some(size) => tracing::error!(
                        "dropped a {} event of {size} bytes over the {MAX_FRAME_BYTES}-byte \
                         frame limit; the connection gets a resync",
                        event.kind()
                    ),
                    None => tracing::error!(
                        "cannot encode a {} event; the connection gets a resync",
                        event.kind()
                    ),
                }
                self.resync(ResyncReason::Oversized, now_seq.max(*seq));
                return;
            }
        };
        let key = key_of(*seq, event);
        let replaced = self.by_key.contains_key(&key);
        if self.lines.len() + usize::from(!replaced) > BACKLOG_EVENTS {
            self.resync(ResyncReason::Overflow, now_seq.max(*seq));
            return;
        }
        // The replaced line's bytes go back first: a newer state needs no room beside it.
        if let Some(old) = self.by_key.remove(&key)
            && let Some((_, old_line, _)) = self.lines.remove(&old)
        {
            self.bytes -= old_line.len();
        }
        let Some(room) = self.budget.try_take(line.len()) else {
            self.resync(ResyncReason::Overflow, now_seq.max(*seq));
            return;
        };
        self.bytes += line.len();
        self.by_key.insert(key.clone(), *seq);
        self.lines.insert(*seq, (key, line, Some(room)));
    }

    /// The broadcast channel overflowed for this connection.
    pub fn lagged(&mut self, now_seq: u64) {
        self.resync(ResyncReason::Lagged, now_seq);
    }

    /// Clears the backlog and queues one `resync` numbered `through`: everything up to it is
    /// covered (the client reads every snapshot after it, spec §6.2). A second trigger before
    /// the writer took the first keeps one.
    fn resync(&mut self, reason: ResyncReason, through: u64) {
        self.lines.clear();
        self.by_key.clear();
        self.bytes = 0;
        if let Some(written) = &mut self.unwritten_resync {
            match written.try_recv() {
                // Not written yet: the client reads it later, and reads every snapshot after
                // that, so it covers these events as well.
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    self.covered_through = self.covered_through.max(through);
                    return;
                }
                _ => self.unwritten_resync = None,
            }
        }
        self.resync_at(reason, through);
    }

    /// The `resync` line takes no bytes of the budget (`CONTROL_RESERVE` covers it).
    fn resync_at(&mut self, reason: ResyncReason, through: u64) {
        let through = through.max(self.covered_through);
        if let Some(old) = self.by_key.remove(&Key::Resync)
            && let Some((_, line, _)) = self.lines.remove(&old)
        {
            self.bytes -= line.len();
        }
        let line = encode(through, &Event::Resync { reason }, None)
            .expect("a resync frame always encodes");
        debug_assert!(line.len() <= CONTROL_RESERVE / 4);
        self.covered_through = through;
        self.bytes += line.len();
        self.by_key.insert(Key::Resync, through);
        self.lines.insert(through, (Key::Resync, line, None));
    }

    fn resync_reason(&self) -> ResyncReason {
        let seq = self.by_key[&Key::Resync];
        let line = &self.lines[&seq].1;
        if line.contains("\"lagged\"") {
            ResyncReason::Lagged
        } else if line.contains("\"oversized\"") {
            ResyncReason::Oversized
        } else {
            ResyncReason::Overflow
        }
    }

    /// The first waiting line, for the writer, with its bytes of the budget (held until it is
    /// written). For a `resync`, also the sender the writer answers once it wrote it.
    pub fn pop(&mut self) -> Option<(String, LineBytes, Option<tokio::sync::oneshot::Sender<()>>)> {
        let (_, (key, line, room)) = self.lines.pop_first()?;
        self.by_key.remove(&key);
        self.bytes -= line.len();
        let written = (key == Key::Resync).then(|| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.unwritten_resync = Some(rx);
            tx
        });
        Some((line, room, written))
    }
}

/// The event's frame line, or the size it would have (`None`: it cannot be encoded) when it
/// does not fit in a frame.
fn encode(seq: u64, event: &Event, pad: Option<&(String, usize)>) -> Result<String, Option<usize>> {
    let frame = ServerFrame::Event {
        event: event.clone(),
        seq,
    };
    let line = serde_json::to_string(&frame).map_err(|_| None)?;
    let extra = match pad {
        Some((kind, bytes)) if kind == event.kind() => *bytes,
        _ => 0,
    };
    let size = line.len().saturating_add(extra);
    if size > MAX_FRAME_BYTES {
        return Err(Some(size));
    }
    Ok(line)
}

#[cfg(debug_assertions)]
fn test_pad() -> Option<(String, usize)> {
    let raw = std::env::var("HOSTLER_TEST_EVENT_PAD_BYTES").ok()?;
    let (kind, bytes) = raw.split_once(':')?;
    Some((kind.to_owned(), bytes.parse().ok()?))
}

#[cfg(not(debug_assertions))]
fn test_pad() -> Option<(String, usize)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::secrets::{RequestState, SecretRequestInfo};
    use crate::contract::wire::tests::all_events;

    fn request(id: &str, state: RequestState) -> Event {
        Event::SecretRequestChanged(SecretRequestInfo {
            id: id.into(),
            project_id: "p1".into(),
            project_name: "app".into(),
            name: "API_KEY".into(),
            reason: "tests".into(),
            state,
            requester_pid: 42,
            created_at_ms: 1,
            answered_at_ms: None,
        })
    }

    fn queue() -> Event {
        all_events().remove(0)
    }

    fn seizure() -> Event {
        all_events().remove(4)
    }

    fn drain(pending: &mut PendingEvents) -> Vec<(u64, Event)> {
        std::iter::from_fn(|| pending.pop().map(|(line, _, _)| line))
            .map(|line| match serde_json::from_str(&line).unwrap() {
                ServerFrame::Event { event, seq } => (seq, event),
                _ => panic!("not an event"),
            })
            .collect()
    }

    #[test]
    fn publish_numbers_in_order_and_names_snapshots() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        assert_eq!(bus.current_seq(), 0);
        assert_eq!(bus.publish_test(queue()), 1);
        assert_eq!(bus.publish_test(seizure()), 2);
        assert_eq!(bus.current_seq(), 2);
        let first = rx.try_recv().unwrap();
        assert_eq!(first.seq, 1);
        let Event::QueueChanged(snapshot) = first.event else {
            panic!("not the queue")
        };
        assert_eq!(snapshot.as_of_seq, 1);
        assert_eq!(rx.try_recv().unwrap().seq, 2);
        assert!(!bus.take_projects_dirty());
        bus.mark_projects_dirty();
        assert!(bus.take_projects_dirty());
        assert!(!bus.take_projects_dirty());
    }

    #[test]
    fn newest_per_key_moves_to_the_end_records_stay() {
        let mut pending = PendingEvents::new();
        let mut seq = 0;
        let mut next = |e: Event, p: &mut PendingEvents| {
            seq += 1;
            p.push(&Published { seq, event: e }, seq);
        };
        next(request("a", RequestState::Pending), &mut pending); // 1
        next(queue(), &mut pending); // 2
        next(seizure(), &mut pending); // 3
        next(request("a", RequestState::Stored), &mut pending); // 4
        next(queue(), &mut pending); // 5
        next(seizure(), &mut pending); // 6
        let got = drain(&mut pending);
        let seqs: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
        assert_eq!(seqs, [3, 4, 5, 6]);
        assert!(
            matches!(&got[1].1, Event::SecretRequestChanged(i) if i.state == RequestState::Stored)
        );
        assert!(pending.is_empty());
        assert_eq!(pending.bytes, 0);
    }

    #[test]
    fn overflow_folds_into_one_resync_until_the_writer_takes_it() {
        let mut pending = PendingEvents::new();
        for seq in 1..=(BACKLOG_EVENTS as u64) {
            pending.push(
                &Published {
                    seq,
                    event: request(&format!("r{seq}"), RequestState::Pending),
                },
                seq,
            );
        }
        assert_eq!(pending.len(), BACKLOG_EVENTS);
        for seq in 257..=300 {
            pending.push(
                &Published {
                    seq,
                    event: request(&format!("r{seq}"), RequestState::Pending),
                },
                seq,
            );
        }
        let got = drain(&mut pending);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 300);
        assert_eq!(
            got[0].1,
            Event::Resync {
                reason: ResyncReason::Overflow
            }
        );
        // After the writer took it, events queue as usual; covered ones are dropped.
        pending.push(
            &Published {
                seq: 300,
                event: queue(),
            },
            300,
        );
        assert!(pending.is_empty());
        pending.push(
            &Published {
                seq: 301,
                event: queue(),
            },
            301,
        );
        assert_eq!(drain(&mut pending)[0].0, 301);
    }

    #[test]
    fn lag_covers_everything_published_so_far() {
        let mut pending = PendingEvents::new();
        pending.push(
            &Published {
                seq: 1,
                event: queue(),
            },
            1,
        );
        pending.lagged(20);
        // The channel still holds 17..=20: covered.
        for seq in 17..=20 {
            pending.push(
                &Published {
                    seq,
                    event: seizure(),
                },
                20,
            );
        }
        let got = drain(&mut pending);
        assert_eq!(
            got,
            [(
                20,
                Event::Resync {
                    reason: ResyncReason::Lagged
                }
            )]
        );
    }

    #[test]
    fn byte_bound_overflows_too() {
        let mut pending = PendingEvents::new();
        let mut first_resync = None;
        for seq in 1..=40u64 {
            let Event::SecretRequestChanged(mut info) =
                request(&format!("r{seq}"), RequestState::Pending)
            else {
                unreachable!()
            };
            info.reason = "x".repeat(1 << 20);
            pending.push(
                &Published {
                    seq,
                    event: Event::SecretRequestChanged(info),
                },
                seq,
            );
            if first_resync.is_none() && pending.by_key.contains_key(&Key::Resync) {
                first_resync = Some(seq);
            }
        }
        // The events' share is 24 MiB less 1 KiB (32 MiB less the reply reserve and the
        // control lines): 23 lines of just over 1 MiB fit; the 24th does not.
        assert_eq!(EVENT_BYTES, (24 << 20) - 1024);
        assert_eq!(first_resync, Some(24));
        let got = drain(&mut pending);
        assert_eq!(
            got,
            [(
                40,
                Event::Resync {
                    reason: ResyncReason::Overflow
                }
            )]
        );
    }

    #[test]
    fn oversized_event_becomes_a_resync() {
        let mut pending = PendingEvents::new();
        pending.pad = Some(("devices.changed".into(), MAX_FRAME_BYTES));
        pending.push(
            &Published {
                seq: 1,
                event: request("a", RequestState::Pending),
            },
            1,
        );
        pending.push(
            &Published {
                seq: 2,
                event: all_events().remove(5),
            },
            2,
        );
        let got = drain(&mut pending);
        assert_eq!(
            got,
            [(
                2,
                Event::Resync {
                    reason: ResyncReason::Oversized
                }
            )]
        );
    }

    /// R-1: commits from many threads come out of the channel in increasing, gap-free order,
    /// and a snapshot carries its own number as its point.
    #[test]
    fn commits_are_numbered_in_channel_order() {
        let bus = EventBus::new(4096);
        let mut rx = bus.subscribe();
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let bus = bus.clone();
                std::thread::spawn(move || {
                    for n in 0..200 {
                        if (t + n) % 2 == 0 {
                            let Event::QueueChanged(snapshot) = queue() else {
                                unreachable!()
                            };
                            bus.publish_snapshot(snapshot);
                        } else {
                            bus.publish(seizure());
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let got: Vec<Published> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(got.len(), 800);
        for (n, published) in got.iter().enumerate() {
            assert_eq!(published.seq, n as u64 + 1);
            if let Some(as_of) = published.event.as_of_seq() {
                assert_eq!(as_of, published.seq);
            }
        }
        assert_eq!(bus.current_seq(), 800);
    }

    /// A `resync` handed to the writer and not yet written covers later triggers (one resync
    /// for the reader); once written, the next trigger queues a new one.
    #[test]
    fn an_unwritten_resync_covers_later_triggers() {
        let mut pending = PendingEvents::new();
        pending.lagged(5);
        let (line, room, written) = pending.pop().unwrap();
        assert!(line.contains("\"resync\"") && room.is_none());
        let written = written.expect("the writer reports a resync written");
        pending.lagged(9);
        assert!(pending.is_empty(), "covered by the unwritten resync");
        pending.push(
            &Published {
                seq: 8,
                event: queue(),
            },
            9,
        );
        assert!(pending.is_empty(), "8 <= 9 is covered");
        written.send(()).unwrap();
        pending.lagged(12);
        let got = drain(&mut pending);
        assert_eq!(
            got,
            [(
                12,
                Event::Resync {
                    reason: ResyncReason::Lagged
                }
            )]
        );
    }
}
