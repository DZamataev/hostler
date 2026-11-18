//! The local-socket server: accepts connections from the same user, speaks newline-delimited JSON
//! frames, and dispatches versioned requests to the heavy engine (plan Tasks 6–7) and, on the
//! blocking pool, to the secrets engine (P2 Task 6).

use std::cell::Cell;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use interprocess::local_socket::{ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{Mutex, MutexGuard, broadcast, oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;
use zeroize::Zeroizing;

use super::events::{EventBus, LineBytes, Outbound, PendingEvents, Published};
use super::lifecycle;
use crate::claims::engine::{ClaimsEngine, heavy_view, no_heavy, resolve_workspace};
use crate::contract::accounts::AccountInfo;
use crate::contract::claims::{Overview, ResourceId, errors as claims_errors};
use crate::contract::heavy::{HeavyAcquire, QueueStopOutcome, ServiceStatus};
use crate::contract::projects::ProjectsSnapshot;
use crate::contract::secrets::{ProjectInfo, SecretsStatus};
use crate::contract::wire::{ClientFrame, Event, Request, ServerFrame, sensitive_request};
use crate::contract::{ErrorCode, HostlerError};
use crate::devices::agent_device;
use crate::devices::service::DevicesService;
use crate::heavy::engine::HeavyEngine;
use crate::paths::SocketName;
use crate::secrets::engine::{Caller, Reply as SecretsReply, SecretsAnswer, SecretsEngine};
use crate::secrets::gate::GrantMeta;

/// The largest frame either side sends or accepts; a longer incoming line closes the connection.
/// The engine's admission limits keep every answer and snapshot below it.
pub const MAX_FRAME_BYTES: usize = 8 << 20;
const TICK_EVERY: Duration = Duration::from_secs(2);
/// How long an accepted `service.shutdown` waits for its reply to be queued and written; the
/// service stops either way.
const STOP_REPLY_FLUSH: Duration = Duration::from_secs(2);
/// How long a stop waits for running vault work (P2-5, L8).
const VAULT_DRAIN: Duration = Duration::from_secs(30);
/// How long a stop waits for the claims engine (T5-1): a stop never hangs on it. Under the
/// client's 5 s call bound, so the refusal reaches the caller.
const CLAIMS_LOCK_BOUND: Duration = Duration::from_secs(3);
/// O-1b: a request this slow inside the service is logged: half the client's 5 s bound.
const SLOW_REQUEST: Duration = Duration::from_secs(2);
/// A claims or heavy tick this slow is logged.
const SLOW_TICK: Duration = Duration::from_secs(1);

/// How long the current request or tick waited for the heavy and the claims lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LockWaits {
    heavy: Duration,
    claims: Duration,
}

tokio::task_local! {
    static WAITS: Cell<LockWaits>;
}

#[derive(Clone, Copy)]
enum Lock {
    Heavy,
    Claims,
}

/// `lock`, with its wait added to the current request's or tick's `LockWaits` (none outside),
/// also when a bound gives up on it.
async fn waited<F: Future>(which: Lock, lock: F) -> F::Output {
    struct Wait(Lock, Instant);
    impl Drop for Wait {
        fn drop(&mut self) {
            let _ = WAITS.try_with(|w| {
                let mut waits = w.get();
                match self.0 {
                    Lock::Heavy => waits.heavy += self.1.elapsed(),
                    Lock::Claims => waits.claims += self.1.elapsed(),
                }
                w.set(waits);
            });
        }
    }
    let _wait = Wait(which, Instant::now());
    lock.await
}

fn heavy_lock(shared: &Shared) -> impl Future<Output = MutexGuard<'_, HeavyEngine>> {
    waited(Lock::Heavy, shared.engine.lock())
}

fn claims_lock(shared: &Shared) -> impl Future<Output = MutexGuard<'_, ClaimsEngine>> {
    waited(Lock::Claims, shared.claims.lock())
}

/// `work`, with how long it took and its lock waits.
async fn timed<T>(work: impl Future<Output = T>) -> (T, Duration, LockWaits) {
    let started = Instant::now();
    WAITS
        .scope(Cell::new(LockWaits::default()), async {
            let out = work.await;
            (out, started.elapsed(), WAITS.with(Cell::get))
        })
        .await
}

/// The log line of a request or tick that took `took`; `None` under `limit`.
fn slow_line(what: &str, took: Duration, waits: LockWaits, limit: Duration) -> Option<String> {
    (took >= limit).then(|| {
        format!(
            "slow {what}: {} ms (heavy lock {} ms, claims lock {} ms)",
            took.as_millis(),
            waits.heavy.as_millis(),
            waits.claims.as_millis()
        )
    })
}

/// A frame's wire method only (`claims.acquire`), never its params; read again only for a slow
/// request, so values in the params are skipped, not copied.
fn method_of(line: &str) -> String {
    #[derive(Deserialize)]
    struct Method {
        method: String,
    }
    serde_json::from_str::<Method>(line)
        .map_or_else(|_| "an unreadable request".into(), |m| m.method)
}

struct Shared {
    engine: Mutex<HeavyEngine>,
    /// Taken after `engine` when both are needed, and never held across a secrets call.
    claims: Mutex<ClaimsEngine>,
    secrets: Arc<SecretsEngine>,
    /// Never called under the claims or heavy lock (P3b): its calls are cache reads and short
    /// store writes; its tool calls run in its own lanes.
    devices: Arc<DevicesService>,
    /// Every secrets job and its reply; a stop drains them before the vault is locked.
    jobs: TaskTracker,
    events: EventBus,
    contract_version: u32,
    shutdown: watch::Sender<bool>,
    /// This run's id (spec §6.2), in `hello` and `service.status`.
    instance: String,
    /// The service's own binary, canonicalized (spec §9.7).
    exe: Option<String>,
}

/// `std::env::current_exe()`, canonicalized, as the CLI displays paths.
fn own_exe() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(crate::project::display_path(&exe))
}

pub fn socket_name(socket: &SocketName) -> std::io::Result<Name<'_>> {
    match socket {
        SocketName::File(path) => path
            .as_path()
            .to_fs_name::<interprocess::local_socket::GenericFilePath>(),
        SocketName::Namespaced(name) => name
            .as_str()
            .to_ns_name::<interprocess::local_socket::GenericNamespaced>(),
    }
}

/// Binds the socket. The caller holds the service lock, so a leftover socket file can only be
/// stale and is removed first.
pub fn bind(socket: &SocketName) -> anyhow::Result<Listener> {
    #[cfg(unix)]
    if let SocketName::File(path) = socket
        && std::fs::symlink_metadata(path).is_ok()
    {
        std::fs::remove_file(path).map_err(|e| {
            anyhow::anyhow!("cannot remove the stale socket {}: {e}", path.display())
        })?;
    }
    let options = ListenerOptions::new().name(socket_name(socket)?);
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        options.security_descriptor(crate::winsec::user_only_sd("")?)
    };
    Ok(options.create_tokio()?)
}

/// Serves until `service.shutdown`, the debug-only test-owner watchdog (L58) or a termination
/// signal. Then it refuses new secrets work, keeps serving connections while running vault jobs
/// finish (up to 30 s; past that the process exits holding the service lock), and locks the
/// vault. Before returning, every connection task and the ticker are stopped, so nothing touches
/// the store once the caller drops the lock.
pub async fn serve(
    listener: Listener,
    engine: HeavyEngine,
    claims: ClaimsEngine,
    secrets: SecretsEngine,
    devices: Arc<DevicesService>,
    events: EventBus,
    shutdown: watch::Sender<bool>,
) -> anyhow::Result<()> {
    let mut stopping = shutdown.subscribe();
    // Every claims snapshot reaches the device service (its claims view, Task 8's windows),
    // off the claims lock. Subscribed before the first tick, so none is missed.
    let mut claims_events = events.subscribe();
    let shared = Arc::new(Shared {
        engine: Mutex::new(engine),
        claims: Mutex::new(claims),
        secrets: Arc::new(secrets),
        devices: devices.clone(),
        jobs: TaskTracker::new(),
        events,
        contract_version: lifecycle::service_contract_version(),
        shutdown,
        instance: uuid::Uuid::new_v4().to_string(),
        exe: own_exe(),
    });
    // The first `projects.changed` compares against the projects as they are at start.
    claims_lock(&shared).await.remember_projects();
    // Task 7: the boot shutdown rule's one engine question, under the claims lock (P2-14);
    // no answer within the bound keeps the device running.
    devices.boots().set_in_use({
        let weak = Arc::downgrade(&shared);
        Box::new(move |r| {
            let weak = weak.clone();
            Box::pin(async move {
                let shared = weak.upgrade()?;
                let claims = tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(&shared))
                    .await
                    .ok()?;
                Some(claims.device_in_use(&r))
            })
        })
    });
    heavy_lock(&shared).await.tick();
    claims_tick(&shared).await;
    // Task 7 (P3bT7-1): the device service's claims view before its first tick, so a held
    // hostler-booted device never looks idle after a start.
    let snapshot = claims_lock(&shared).await.snapshot();
    if let Ok(snapshot) = snapshot {
        devices.on_claims(&snapshot).await;
    }

    let poller = tokio::spawn(devices.clone().run_poller());
    let claims_feed = {
        let shared = shared.clone();
        tokio::spawn(async move {
            loop {
                match claims_events.recv().await {
                    Ok(Published {
                        event: Event::ClaimsChanged(snapshot),
                        ..
                    }) => shared.devices.on_claims(&snapshot).await,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // A lost snapshot: read the current one (Task 8 rebuilds its windows).
                        let snapshot = claims_lock(&shared).await.snapshot();
                        if let Ok(snapshot) = snapshot {
                            shared.devices.on_claims(&snapshot).await;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };
    let claims_ticker = {
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(claims_tick_every());
            loop {
                // A sessions read that cleared a device ticks at once (P3-15).
                tokio::select! {
                    _ = every.tick() => {}
                    _ = shared.devices.tick_notify().notified() => {}
                }
                claims_tick(&shared).await;
                shared.devices.on_tick();
            }
        })
    };
    let ticker = {
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(TICK_EVERY);
            loop {
                every.tick().await;
                let ((), took, waits) = timed(async { heavy_lock(&shared).await.tick() }).await;
                if let Some(text) = slow_line("heavy tick", took, waits, SLOW_TICK) {
                    tracing::warn!("{text}");
                }
                let secrets = shared.secrets.clone();
                let _ = shared.jobs.spawn_blocking(move || secrets.tick()).await;
            }
        })
    };

    let mut connections = JoinSet::new();
    let terminated = termination();
    tokio::pin!(terminated);
    // A `while` condition rather than only a `select!` arm: a stop sent before this subscription
    // (L58's watchdog on an owner that cannot be read) is a value, not a change.
    while !*stopping.borrow_and_update() {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    if !peer_is_current_user(&stream) {
                        tracing::warn!("dropped a connection from another user");
                        continue;
                    }
                    connections.spawn(connection(stream, shared.clone()));
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            _ = stopping.changed() => break,
            signal = &mut terminated => {
                tracing::info!("stopping on {signal}");
                break;
            }
        }
    }
    shared.secrets.begin_shutdown();
    // Bounded (T5-1): a stop on a signal never hangs on the claims engine.
    match tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(&shared)).await {
        Ok(mut claims) => claims.begin_stopping(),
        Err(_) => tracing::warn!("stopping without the claims engine's answer"),
    }
    ticker.abort();
    let _ = ticker.await;
    claims_ticker.abort();
    let _ = tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_ticker).await;
    poller.abort();
    claims_feed.abort();
    shared.jobs.close();
    let drained = shared.jobs.wait();
    let deadline = tokio::time::sleep(VAULT_DRAIN);
    tokio::pin!(drained, deadline);
    // Connections keep being served meanwhile: new secrets work is answered `stopping`.
    loop {
        tokio::select! {
            _ = &mut drained => break,
            _ = &mut deadline => {
                tracing::error!(
                    "vault work still runs after {VAULT_DRAIN:?}; exiting while still holding \
                     the service lock"
                );
                std::process::exit(1);
            }
            accepted = listener.accept() => {
                if let Ok(stream) = accepted
                    && peer_is_current_user(&stream)
                {
                    connections.spawn(connection(stream, shared.clone()));
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    // Bounded: a connection task cannot be cancelled inside an engine call.
    let _ = tokio::time::timeout(CLAIMS_LOCK_BOUND, connections.shutdown()).await;
    shared.secrets.lock_for_shutdown();
    Ok(())
}

#[cfg(unix)]
async fn termination() -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
    }
}

#[cfg(windows)]
async fn termination() -> &'static str {
    match tokio::signal::ctrl_c().await {
        Ok(()) => "Ctrl+C",
        Err(_) => std::future::pending().await,
    }
}

/// Only connections from our own effective uid are served; missing credentials count as foreign.
pub fn peer_allowed(peer_euid: Option<u32>, own_euid: u32) -> bool {
    peer_euid == Some(own_euid)
}

#[cfg(unix)]
fn peer_is_current_user(stream: &Stream) -> bool {
    let peer = stream.peer_creds().ok().and_then(|c| c.euid());
    // SAFETY: geteuid has no preconditions and cannot fail.
    peer_allowed(peer, unsafe { libc::geteuid() })
}

/// The pipe's security descriptor already admits only the current user.
#[cfg(windows)]
fn peer_is_current_user(_stream: &Stream) -> bool {
    true
}

/// The connecting process's pid, read once at accept, for log lines only (spec §8.5, R2-5):
/// macOS `getsockopt(SOL_LOCAL, LOCAL_PEERPID)`, Linux `SO_PEERCRED`; none on Windows or when
/// the OS does not say.
#[cfg(target_os = "macos")]
fn peer_pid(stream: &Stream) -> Option<u32> {
    use std::os::fd::{AsFd, AsRawFd};
    #[allow(irrefutable_let_patterns)]
    let Stream::UdSocket(socket) = stream else {
        return None;
    };
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: a valid socket fd, and `pid`/`len` point to a pid_t-sized buffer and its size.
    let rc = unsafe {
        libc::getsockopt(
            socket.as_fd().as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut len,
        )
    };
    (rc == 0 && pid > 0).then_some(pid as u32)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn peer_pid(stream: &Stream) -> Option<u32> {
    let pid = stream.peer_creds().ok()?.pid()?;
    (pid > 0).then_some(pid as u32)
}

#[cfg(windows)]
fn peer_pid(_stream: &Stream) -> Option<u32> {
    None
}

/// What the reader hands the writer task.
enum Out {
    Line(String),
    /// A line whose writer reports back once it is flushed.
    Flushed(String, oneshot::Sender<()>),
    /// A line carrying secret values: erased once written (or not).
    Sensitive(Zeroizing<String>, oneshot::Sender<()>),
}

/// One connection: a reader and a writer that run independently, so a large pushed snapshot
/// never blocks reading the client's next request (and the reverse).
async fn connection(stream: Stream, shared: Arc<Shared>) {
    let peer = peer_pid(&stream);
    let (recv, send) = tokio::io::split(stream);
    let reader = FramedRead::new(recv, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
    let writer = FramedWrite::new(send, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
    let queue = Arc::new(WriterQueue::default());
    tokio::join!(
        read_loop(reader, queue.clone(), &shared, peer),
        write_loop(writer, queue)
    );
}

/// How many event lines the writer's queue holds; past it events wait in `PendingEvents`,
/// where they are coalesced (spec §6.2).
const EVENT_SLOTS: usize = 16;

/// A line for the writer with its bytes of a budget (none for a control line) and, for an
/// event line, its slot; both are released once it is written or dropped.
struct Queued {
    out: Out,
    _bytes: LineBytes,
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// One connection's outbound lines (R-2, R-3): replies first, then events. Queueing a line
/// never waits; the budgets (`Outbound::replies`, `Outbound::events`, `EVENT_SLOTS`) are taken
/// before. So a reply never waits behind event lines, and a control line (the shutdown reply,
/// a `resync`) never waits at all.
struct WriterQueue {
    lanes: std::sync::Mutex<Lanes>,
    ready: tokio::sync::Notify,
    /// Pending events and event lines here (`EVENT_BYTES`).
    events_budget: Outbound,
    /// Replies here, sensitive answers included (`REPLY_RESERVE`).
    replies_budget: Outbound,
    /// Event lines here: at most `EVENT_SLOTS`.
    slots: Arc<tokio::sync::Semaphore>,
}

impl Default for WriterQueue {
    fn default() -> WriterQueue {
        WriterQueue {
            lanes: std::sync::Mutex::default(),
            ready: tokio::sync::Notify::new(),
            events_budget: Outbound::events(),
            replies_budget: Outbound::replies(),
            slots: Arc::new(tokio::sync::Semaphore::new(EVENT_SLOTS)),
        }
    }
}

#[derive(Default)]
struct Lanes {
    replies: std::collections::VecDeque<Queued>,
    events: std::collections::VecDeque<Queued>,
    /// The reader is done: the writer drains what is queued, then ends.
    finished: bool,
    /// The writer is gone (a write failed): nothing more is taken.
    broken: bool,
}

impl WriterQueue {
    fn lanes(&self) -> std::sync::MutexGuard<'_, Lanes> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `false` once the writer is gone.
    fn push_reply(&self, line: Queued) -> bool {
        let mut lanes = self.lanes();
        if lanes.broken {
            return false;
        }
        lanes.replies.push_back(line);
        drop(lanes);
        self.ready.notify_one();
        true
    }

    /// `false` once the writer is gone.
    fn push_event(&self, line: Queued) -> bool {
        let mut lanes = self.lanes();
        if lanes.broken {
            return false;
        }
        lanes.events.push_back(line);
        drop(lanes);
        self.ready.notify_one();
        true
    }

    fn finish(&self) {
        self.lanes().finished = true;
        self.ready.notify_one();
    }

    fn break_off(&self) {
        let mut lanes = self.lanes();
        lanes.broken = true;
        lanes.replies.clear();
        lanes.events.clear();
    }

    /// The next line for the socket, replies first; `None` once the reader is done and
    /// everything was taken.
    async fn next(&self) -> Option<Queued> {
        loop {
            {
                let mut lanes = self.lanes();
                if let Some(line) = lanes
                    .replies
                    .pop_front()
                    .or_else(|| lanes.events.pop_front())
                {
                    return Some(line);
                }
                if lanes.finished || lanes.broken {
                    return None;
                }
            }
            // A notification sent since the check is stored, so none is lost.
            self.ready.notified().await;
        }
    }
}

/// The writer: one line at a time, replies first; a line's bytes and slot go back once it is
/// written.
async fn write_loop(
    mut writer: FramedWrite<WriteHalf<Stream>, LinesCodec>,
    queue: Arc<WriterQueue>,
) {
    while let Some(line) = queue.next().await {
        // Held until the line is written, dropped at the end of this turn.
        let Queued {
            out,
            _bytes: bytes,
            _slot: slot,
        } = line;
        let (sent, flushed) = match out {
            Out::Line(line) => (writer.send(line).await, None),
            Out::Flushed(line, done) => (writer.send(line).await, Some(done)),
            Out::Sensitive(line, done) => (writer.send(line.as_str()).await, Some(done)),
        };
        if sent.is_err() {
            queue.break_off();
            return;
        }
        drop((bytes, slot));
        if let Some(done) = flushed {
            let _ = done.send(());
        }
    }
}

struct ConnState {
    hello_ok: bool,
    events: Option<broadcast::Receiver<Published>>,
    /// The proof this connection holds (spec §8.5): installed after a successful
    /// `secrets.prove` or `secrets.unlock` that answered one, cleared by a failed one, dropped
    /// with the connection. Every gated method is admitted against it.
    grant: Option<GrantMeta>,
    /// The peer's pid, read at accept; log lines only (R2-5).
    peer_pid: Option<u32>,
}

impl ConnState {
    #[cfg(test)]
    fn new() -> ConnState {
        ConnState::with_peer(None)
    }

    fn with_peer(peer_pid: Option<u32>) -> ConnState {
        ConnState {
            hello_ok: false,
            events: None,
            grant: None,
            peer_pid,
        }
    }

    fn caller(&self) -> Caller {
        Caller {
            peer_pid: self.peer_pid,
            grant: self.grant,
        }
    }
}

/// R2-3: a prove or unlock that succeeded with a grant installs it; one that failed clears
/// the connection's grant; anything else leaves it.
fn apply_grant(
    grant: &mut Option<GrantMeta>,
    grants: bool,
    succeeded: bool,
    answered: Option<GrantMeta>,
) {
    if !grants {
        return;
    }
    match (succeeded, answered) {
        (true, Some(meta)) => *grant = Some(meta),
        (true, None) => {}
        (false, _) => *grant = None,
    }
}

async fn next_event(
    events: &mut Option<broadcast::Receiver<Published>>,
) -> Result<Published, broadcast::error::RecvError> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn read_loop(
    mut reader: FramedRead<ReadHalf<Stream>, LinesCodec>,
    queue: Arc<WriterQueue>,
    shared: &Shared,
    peer: Option<u32>,
) {
    read_requests(&mut reader, &queue, shared, peer).await;
    // The writer drains what is queued (a stop's reply included), then ends.
    queue.finish();
}

async fn read_requests(
    reader: &mut FramedRead<ReadHalf<Stream>, LinesCodec>,
    queue: &WriterQueue,
    shared: &Shared,
    peer: Option<u32>,
) {
    let mut conn = ConnState::with_peer(peer);
    // Spec §6.2: the newest snapshot per kind, the newest state per request, every record;
    // whatever cannot be delivered becomes one `resync`. Pending events and event lines in the
    // writer's queue share `Outbound::events()`; replies have their own reserve (R-2).
    let replies_budget = &queue.replies_budget;
    let mut pending = PendingEvents::with_budget(queue.events_budget.clone());
    loop {
        tokio::select! {
            line = reader.next() => {
                let Some(Ok(line)) = line else {
                    // Closed, an I/O error, or a frame over the limit: drop the connection.
                    return;
                };
                // A request may carry values (unlock, set, import, a materialize command).
                let line = Zeroizing::new(line);
                let Some(reply) = handle_line(&line, &mut conn, shared).await else {
                    continue;
                };
                match reply {
                    Reply::Frame(frame) => {
                        let line = encode_reply(&frame);
                        // Waits only for earlier replies to be written, never for events.
                        let bytes = replies_budget.take(line.len()).await;
                        let queued = Queued { out: Out::Line(line), _bytes: Some(bytes), _slot: None };
                        if !queue.push_reply(queued) {
                            return;
                        }
                    }
                    Reply::Secrets(out, token, _) => {
                        // A stop's drain waits for this reply at most STOP_REPLY_FLUSH, queueing
                        // and writing together (T6-5); past it the token goes, the reply still
                        // queues (a slow reader loses nothing; a stop drops it, erased).
                        let mut token = Some(token);
                        let deadline = tokio::time::Instant::now() + STOP_REPLY_FLUSH;
                        let (done, flushed) = oneshot::channel();
                        let out = match out {
                            SecretsOut::Frame(frame) => Out::Flushed(encode_reply(&frame), done),
                            SecretsOut::Sensitive(line) => Out::Sensitive(line, done),
                        };
                        let len = match &out {
                            Out::Line(line) | Out::Flushed(line, _) => line.len(),
                            Out::Sensitive(line, _) => line.len(),
                        };
                        // The reply reserve holds the largest answer: this waits only for
                        // earlier replies, never for events (R-2).
                        let mut room = std::pin::pin!(replies_budget.take(len));
                        let bytes = match tokio::time::timeout_at(deadline, &mut room).await {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                token.take();
                                room.await
                            }
                        };
                        let queued = Queued { out, _bytes: Some(bytes), _slot: None };
                        if !queue.push_reply(queued) {
                            return;
                        }
                        let _ = tokio::time::timeout_at(deadline, flushed).await;
                        drop(token);
                    }
                    Reply::Stop(frame) => {
                        // R-3: a control line, queued at once (no budget to wait for); the
                        // writer owns it from here. The deadline bounds only how long the stop
                        // waits for it to be written: a client that stopped reading cannot hold
                        // up the stop, and the reply stays queued for the writer meanwhile.
                        let (done, flushed) = oneshot::channel();
                        let line = encode_reply(&frame);
                        debug_assert!(line.len() <= crate::service::events::CONTROL_RESERVE / 4);
                        let queued = Queued { out: Out::Flushed(line, done), _bytes: None, _slot: None };
                        if queue.push_reply(queued) {
                            let _ = tokio::time::timeout(STOP_REPLY_FLUSH, flushed).await;
                        }
                        let _ = shared.shutdown.send(true);
                        return;
                    }
                }
            }
            received = next_event(&mut conn.events) => match received {
                Ok(published) => pending.push(&published, shared.events.current_seq()),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    pending.lagged(shared.events.current_seq());
                }
                Err(broadcast::error::RecvError::Closed) => conn.events = None,
            },
            slot = queue.slots.clone().acquire_owned(), if !pending.is_empty() => {
                let slot = slot.expect("the slots are never closed");
                let (line, bytes, written) =
                    pending.pop().expect("guarded by the branch condition");
                // A resync's writer says when it is written (one resync until then).
                let out = match written {
                    Some(done) => Out::Flushed(line, done),
                    None => Out::Line(line),
                };
                if !queue.push_event(Queued { out, _bytes: bytes, _slot: Some(slot) }) {
                    return;
                }
            }
        }
    }
}

enum Reply {
    Frame(ServerFrame),
    /// The reply to an accepted `service.shutdown`: written, then the service stops.
    Stop(ServerFrame),
    /// A secrets reply; the token keeps a stop waiting until it is written. The grant is the
    /// one a successful prove or unlock answered (R2-3), for `handle_request` to install.
    Secrets(SecretsOut, TaskTrackerToken, Option<GrantMeta>),
}

enum SecretsOut {
    Frame(ServerFrame),
    /// `{"id":…,"result":…}` with values, under the request's own id.
    Sensitive(Zeroizing<String>),
}

/// A reply line; one that would exceed the frame limit becomes an `Internal` error for its id.
fn encode_reply(frame: &ServerFrame) -> String {
    let encoded = serde_json::to_string(frame).map_err(|e| e.to_string());
    match encoded {
        Ok(line) if line.len() <= MAX_FRAME_BYTES => line,
        other => {
            let reason = match other {
                Ok(line) => format!(
                    "the answer is {} bytes, over the {MAX_FRAME_BYTES}-byte frame limit",
                    line.len()
                ),
                Err(e) => format!("cannot encode the answer: {e}"),
            };
            tracing::error!("{reason}");
            let id = match frame {
                ServerFrame::Response { id, .. } | ServerFrame::Error { id, .. } => *id,
                ServerFrame::Event { .. } => 0,
            };
            let error = ServerFrame::Error {
                id,
                error: HostlerError {
                    code: ErrorCode::Internal,
                    message: reason,
                    details: json!({}),
                    recovery: None,
                },
            };
            serde_json::to_string(&error).expect("a small error frame always encodes")
        }
    }
}

fn response<T: Serialize>(id: u64, result: Result<T, HostlerError>) -> ServerFrame {
    match result.and_then(|v| {
        serde_json::to_value(v).map_err(|e| HostlerError {
            code: ErrorCode::Internal,
            message: format!("cannot encode the reply: {e}"),
            details: json!({}),
            recovery: None,
        })
    }) {
        Ok(result) => ServerFrame::Response { id, result },
        Err(error) => ServerFrame::Error { id, error },
    }
}

/// The error for a frame that does not parse, under its id when it has one. A frame whose
/// method may carry a value — or whose method cannot be read — gets a fixed text: serde's
/// messages quote the input. The payload is skipped unread, never built into a `Value`.
fn unreadable(line: &str, e: &serde_json::Error) -> ServerFrame {
    // Read apart, so a bad method does not lose the id.
    #[derive(Deserialize)]
    struct Id {
        id: Option<u64>,
    }
    #[derive(Deserialize)]
    struct Method {
        method: Option<String>,
    }
    let id = serde_json::from_str::<Id>(line)
        .ok()
        .and_then(|h| h.id)
        .unwrap_or(0);
    let sensitive = serde_json::from_str::<Method>(line)
        .ok()
        .and_then(|h| h.method)
        .is_none_or(|m| sensitive_request(&m));
    let message = if sensitive {
        "unreadable request".to_owned()
    } else {
        format!("unreadable request: {e}")
    };
    ServerFrame::Error {
        id,
        error: HostlerError {
            code: ErrorCode::BadRequest,
            message,
            details: json!({}),
            recovery: None,
        },
    }
}

/// `{"id":<id>,"result":<json>}`, built inside a zeroizing string.
fn sensitive_line(id: u64, json: &str) -> Zeroizing<String> {
    let id = id.to_string();
    let mut line = Zeroizing::new(String::with_capacity(json.len() + id.len() + 16));
    line.push_str("{\"id\":");
    line.push_str(&id);
    line.push_str(",\"result\":");
    line.push_str(json);
    line.push('}');
    line
}

/// Runs a secrets request on the blocking pool (the KDF never runs on the runtime, and never
/// under the heavy engine's lock).
async fn secrets_reply(id: u64, request: Request, shared: &Shared, caller: Caller) -> Reply {
    let token = shared.jobs.token();
    let secrets = shared.secrets.clone();
    let job = shared
        .jobs
        .spawn_blocking(move || secrets.dispatch_as(request, &caller));
    let mut grant = None;
    let out = match job.await {
        Ok(Ok(SecretsAnswer {
            reply,
            grant: answered,
        })) => {
            grant = answered;
            match reply {
                SecretsReply::Value(result) => {
                    SecretsOut::Frame(ServerFrame::Response { id, result })
                }
                SecretsReply::Sensitive(json) => SecretsOut::Sensitive(sensitive_line(id, &json)),
            }
        }
        Ok(Err(error)) => SecretsOut::Frame(ServerFrame::Error { id, error }),
        Err(_) => SecretsOut::Frame(ServerFrame::Error {
            id,
            error: HostlerError {
                code: ErrorCode::Internal,
                message: "the secrets job failed".into(),
                details: json!({}),
                recovery: None,
            },
        }),
    };
    Reply::Secrets(out, token, grant)
}

/// One secrets dispatch on the blocking pool, its answer decoded (never a sensitive one).
async fn secrets_value<T: serde::de::DeserializeOwned>(
    shared: &Shared,
    request: Request,
) -> Result<T, HostlerError> {
    let secrets = shared.secrets.clone();
    let reply = shared
        .jobs
        .spawn_blocking(move || secrets.dispatch(request))
        .await
        .map_err(|_| internal_error("the secrets job failed"))??;
    match reply.reply {
        SecretsReply::Value(value) => {
            serde_json::from_value(value).map_err(|_| internal_error("unreadable secrets answer"))
        }
        SecretsReply::Sensitive(_) => Err(internal_error("unexpected sensitive answer")),
    }
}

/// The person gate's admission for a gated method served outside the secrets engine
/// (`heavy.stop`): a live grant of the vault's current generation, checked under the vault
/// mutex, which is released when this returns (spec §8.5).
async fn admit_grant(shared: &Shared, grant: Option<GrantMeta>) -> Result<(), HostlerError> {
    let secrets = shared.secrets.clone();
    shared
        .jobs
        .spawn_blocking(move || secrets.admit_grant(grant.as_ref()))
        .await
        .map_err(|_| internal_error("the secrets job failed"))?
}

fn internal_error(message: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: message.into(),
        details: json!({}),
        recovery: None,
    }
}

/// `account.list | remove | check | materialize` (P3a Task 9; `account.set` needs no claim).
/// The project is resolved first (its id names the resource), then the claims lock is taken for
/// `holder_of` / `check_holder` and released before the secrets engine runs: it is never held
/// across a secrets call or the KDF. The token keeps a stop waiting until the reply is written.
async fn account_reply(id: u64, request: Request, shared: &Shared) -> Reply {
    let token = shared.jobs.token();
    let project = match &request {
        Request::AccountList { project } | Request::AccountCheck { project, .. } => project,
        Request::AccountRemove(r) => &r.project,
        Request::AccountMaterialize(m) => &m.project,
        _ => unreachable!("handle_line routes only these"),
    };
    let resolved = secrets_value::<ProjectInfo>(shared, Request::ProjectResolve(project.clone()));
    let project = match resolved.await {
        Ok(project) => project,
        Err(e) => {
            return Reply::Secrets(
                SecretsOut::Frame(response::<Value>(id, Err(e))),
                token,
                None,
            );
        }
    };
    let account = |name: &str| ResourceId::account(&project.id, name);
    let result = match request {
        Request::AccountMaterialize(m) => {
            let held = claims_lock(shared).await.check_holder(
                &m.claim_id,
                &account(&m.name),
                &m.requester,
            );
            match held {
                Ok(()) => {
                    return secrets_reply(
                        id,
                        Request::AccountMaterialize(m),
                        shared,
                        Caller::default(),
                    )
                    .await;
                }
                Err(e) => Err(e),
            }
        }
        Request::AccountRemove(r) => {
            let holder = claims_lock(shared).await.holder_of(&account(&r.name));
            match holder {
                None => {
                    return secrets_reply(id, Request::AccountRemove(r), shared, Caller::default())
                        .await;
                }
                Some(h) => Err(claims_errors::resource_in_use(
                    &h.resource,
                    &h.workspace_path,
                )),
            }
        }
        check @ Request::AccountCheck { .. } => {
            match secrets_value::<AccountInfo>(shared, check).await {
                Ok(mut info) => {
                    info.holder = claims_lock(shared).await.holder_of(&account(&info.name));
                    encode(&info)
                }
                Err(e) => Err(e),
            }
        }
        list => match secrets_value::<Vec<AccountInfo>>(shared, list).await {
            Ok(mut infos) => {
                let claims = claims_lock(shared).await;
                for info in &mut infos {
                    info.holder = claims.holder_of(&account(&info.name));
                }
                drop(claims);
                encode(&infos)
            }
            Err(e) => Err(e),
        },
    };
    Reply::Secrets(SecretsOut::Frame(response(id, result)), token, None)
}

fn encode<T: Serialize>(value: &T) -> Result<Value, HostlerError> {
    serde_json::to_value(value).map_err(|_| internal_error("cannot encode the reply"))
}

async fn handle_line(line: &str, conn: &mut ConnState, shared: &Shared) -> Option<Reply> {
    let frame: ClientFrame = match serde_json::from_str(line) {
        Ok(frame) => frame,
        Err(e) => return Some(Reply::Frame(unreadable(line, &e))),
    };
    let id = frame.id;
    let (reply, took, waits) = timed(async {
        let reply = handle_request(id, frame.request, conn, shared).await;
        // A project, a workspace or a declaration changed: `projects.changed` (spec §6.3).
        if shared.events.take_projects_dirty() {
            publish_projects(shared).await;
        }
        reply
    })
    .await;
    if let Some(text) = slow_line(
        &format!("request {}", method_of(line)),
        took,
        waits,
        SLOW_REQUEST,
    ) {
        tracing::warn!("{text}");
    }
    reply
}

async fn handle_request(
    id: u64,
    request: Request,
    conn: &mut ConnState,
    shared: &Shared,
) -> Option<Reply> {
    let reply = match request {
        Request::Hello {
            contract_version, ..
        } => {
            let result =
                lifecycle::hello(contract_version, shared.contract_version).map(|mut r| {
                    r.instance = Some(shared.instance.clone());
                    r
                });
            conn.hello_ok = result.is_ok();
            conn.events = conn.hello_ok.then(|| shared.events.subscribe());
            response(id, result)
        }
        Request::ServiceStatus => response(id, status(shared).await),
        Request::ServiceShutdown {
            if_idle,
            refuse_if_unlocked,
        } => {
            // The idle check and "grant nothing more" happen in one step under both engine
            // locks, heavy then claims; holds and reservations never block a stop.
            let mut heavy = heavy_lock(shared).await;
            // Bounded (T5-1): this waits while holding the heavy lock.
            let mut claims = tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(shared))
                .await
                .ok();
            if claims.is_none() && if_idle {
                return Some(Reply::Frame(response::<Value>(
                    id,
                    Err(HostlerError {
                        code: ErrorCode::ServiceBusy,
                        message: format!(
                            "the claims engine has not answered for {} s, so hostler cannot tell \
                             whether runs hold resources. {}",
                            CLAIMS_LOCK_BOUND.as_secs(),
                            crate::contract::claims::FORCE_SENTENCE
                        ),
                        details: json!({}),
                        recovery: Some("hostler service stop --force".into()),
                    }),
                )));
            }
            let (leases, plans) = match (&claims, if_idle) {
                (Some(claims), true) => (claims.live_leases(), claims.plan_summaries()),
                _ => (Vec::new(), Vec::new()),
            };
            if !leases.is_empty() || !plans.is_empty() {
                let running = heavy.snapshot().map(|s| s.running).unwrap_or_default();
                return Some(Reply::Frame(response::<Value>(
                    id,
                    Err(lifecycle::busy(&running, &leases, &plans)),
                )));
            }
            // P4 (R1-6, R1-1): an automatic stop never locks a person's unlock. Running heavy
            // work is named first (a read under the heavy lock, so `begin_shutdown` below sees
            // the same queue). Then, in one step under the secrets admission gate, admission
            // closes only when the database is locked and no admitted unlock/lock job runs; a
            // job admitted before this decision keeps the service up, one after it is refused.
            if refuse_if_unlocked {
                let running = match if_idle {
                    true => heavy.snapshot().map(|s| s.running).unwrap_or_default(),
                    false => Vec::new(),
                };
                if !running.is_empty() {
                    let mut busy = lifecycle::busy(&running, &[], &[]);
                    if shared.secrets.is_unlocked() {
                        busy.details["secrets"] = json!("unlocked");
                    }
                    return Some(Reply::Frame(response::<Value>(id, Err(busy))));
                }
                if !shared.secrets.close_if_locked() {
                    return Some(Reply::Frame(response::<Value>(
                        id,
                        Err(lifecycle::secrets_unlocked_busy()),
                    )));
                }
            }
            // A refusal from here on reopens what `close_if_locked` closed.
            let reopen = || {
                if refuse_if_unlocked {
                    shared.secrets.reopen_admission();
                }
            };
            return Some(match heavy.begin_shutdown(if_idle) {
                Ok(running) if !running.is_empty() => {
                    reopen();
                    Reply::Frame(response::<Value>(
                        id,
                        Err(lifecycle::busy(&running, &[], &[])),
                    ))
                }
                Ok(_) => {
                    // Accepted: no new claims or secrets work from here on, before any reply
                    // I/O (T6-6).
                    match claims.as_mut() {
                        Some(claims) => claims.begin_stopping(),
                        None => tracing::warn!("stopping without the claims engine's answer"),
                    }
                    shared.secrets.begin_shutdown();
                    Reply::Stop(response(
                        id,
                        Ok(crate::contract::heavy::ShutdownAccepted { stopping: true }),
                    ))
                }
                Err(e) => {
                    reopen();
                    Reply::Frame(response::<Value>(id, Err(e)))
                }
            });
        }
        _ if !conn.hello_ok => {
            response::<Value>(id, Err(lifecycle::hello_required(shared.contract_version)))
        }
        versioned @ (Request::AccountList { .. }
        | Request::AccountRemove(_)
        | Request::AccountCheck { .. }
        | Request::AccountMaterialize(_)) => {
            return Some(account_reply(id, versioned, shared).await);
        }
        versioned if SecretsEngine::handles(&versioned) => {
            let grants = matches!(
                versioned,
                Request::SecretsProve(_) | Request::SecretsUnlock(_)
            );
            let reply = secrets_reply(id, versioned, shared, conn.caller()).await;
            if let Reply::Secrets(out, _, answered) = &reply {
                let succeeded = !matches!(out, SecretsOut::Frame(ServerFrame::Error { .. }));
                apply_grant(&mut conn.grant, grants, succeeded, *answered);
            }
            return Some(reply);
        }
        Request::ProjectsSnapshot => response(id, projects_snapshot(shared).await),
        // Gated (spec §8.5): the admission first, under the vault mutex, on the blocking pool
        // (an unlock's KDF may hold the vault); the mutex is released before anything stops.
        Request::HeavyStop {
            id: entry,
            confirmation,
        } => match admit_grant(shared, conn.grant).await {
            Err(e) => response::<Value>(id, Err(e)),
            Ok(()) => response(
                id,
                heavy_stop(shared, &entry, &confirmation, conn.peer_pid).await,
            ),
        },
        versioned if handles_claims(&versioned) => claims_reply(id, versioned, shared).await,
        versioned if handles_devices(&versioned) => devices_reply(id, versioned, shared).await,
        Request::HeavyAcquire(req) => heavy_acquire(id, req, shared).await,
        Request::HeavyStarted(s) => {
            // L27: the claims engine first (then released), the heavy engine after; the commit
            // checks the entry's current state too. A stuck claims engine does not hold up the
            // heavy queue (fail open, as the heavy queue does).
            let allowed = match tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(shared)).await {
                Ok(claims) => claims.heavy_start_allowed(&s.id, &s.child),
                Err(_) => Ok(()),
            };
            match allowed {
                Err(e) => response::<Value>(id, Err(e)),
                // P5 T6-R1-1: the report is checked against the connection's peer.
                Ok(()) => response(id, heavy_lock(shared).await.started(s, conn.peer_pid)),
            }
        }
        versioned => dispatch(id, versioned, &mut *heavy_lock(shared).await),
    };
    Some(Reply::Frame(reply))
}

/// `heavy.acquire` (L28): refused under a run that is being taken over, and the enclosing runs
/// are recorded as the entry's `nested_in`. Neither lock is held while the other is awaited
/// (T6-R2-3): a heavy snapshot; then the admission under the claims lock (bounded); then the
/// acquire under the heavy lock; then the nesting under a short claims lock. A run that slips in
/// between gets its row, and a takeover's commit re-discovers it.
/// `heavy.stop` after the gate (spec §11.1, P5 Task 6): the entry is marked `stopping` under
/// the heavy lock; its run is stopped and verified outside it, on the blocking pool; the result
/// is recorded under the lock again. The stop runs to its end even when the requester goes.
async fn heavy_stop(
    shared: &Shared,
    entry: &str,
    confirmation: &str,
    peer_pid: Option<u32>,
) -> Result<QueueStopOutcome, HostlerError> {
    let service = std::process::id();
    let target = heavy_lock(shared)
        .await
        .begin_stop(entry, confirmation, peer_pid, &|t| {
            crate::procs::stop::refuses_self_target(service, t)
        })?;
    let grace = crate::procs::stop::takeover_grace();
    let stopping = target.clone();
    let run = tokio::task::spawn_blocking(move || {
        crate::procs::stop::pause_at("heavy_stop");
        crate::procs::stop::stop_run(&stopping, grace)
    })
    .await
    .unwrap_or_else(|_| {
        crate::procs::stop::RunStop::failed(
            &target,
            crate::procs::stop::StopError::Unknown("the stop failed".into()),
        )
    });
    heavy_lock(shared).await.finish_stop(entry, &run)
}

async fn heavy_acquire(id: u64, req: HeavyAcquire, shared: &Shared) -> ServerFrame {
    let snapshot = heavy_lock(shared).await.snapshot();
    let view = snapshot
        .map(|s| heavy_view(&s))
        .unwrap_or_else(|_| no_heavy());
    let nested = match tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(shared)).await {
        Ok(claims) => match claims.heavy_admission(&req.holder, &view) {
            Ok(nested) => nested,
            Err(e) => return response::<Value>(id, Err(e)),
        },
        // A stuck claims engine does not hold up the heavy queue (its fail-open).
        Err(_) => None,
    };
    let entry = req.id.clone();
    let result = heavy_lock(shared).await.acquire(req);
    if let (Ok(_), Some(outer)) = (&result, &nested)
        && let Ok(claims) = tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(shared)).await
    {
        claims.record_heavy_nesting(&entry, outer);
    }
    response(id, result)
}

/// The claims tick interval: 2 s; `HOSTLER_TEST_CLAIMS_TICK_MS` in debug builds.
fn claims_tick_every() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_CLAIMS_TICK_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        return Duration::from_millis(ms);
    }
    TICK_EVERY
}

async fn claims_tick(shared: &Shared) {
    let ((), took, waits) = timed(claims_tick_once(shared)).await;
    if let Some(text) = slow_line("claims tick", took, waits, SLOW_TICK) {
        tracing::warn!("{text}");
    }
}

/// The heavy view is taken under the heavy lock, which is released before the claims lock.
async fn claims_tick_once(shared: &Shared) {
    let snapshot = heavy_lock(shared).await.snapshot();
    let mut claims = claims_lock(shared).await;
    // Debug builds: `HOSTLER_TEST_CLAIMS_STALL=<file>` — once the file exists, one tick removes
    // it and holds the claims lock for 8 s (without blocking a runtime thread).
    #[cfg(debug_assertions)]
    if let Some(marker) = std::env::var_os("HOSTLER_TEST_CLAIMS_STALL")
        && std::fs::remove_file(&marker).is_ok()
    {
        tokio::time::sleep(Duration::from_secs(8)).await;
    }
    let view = match snapshot {
        Ok(snapshot) => heavy_view(&snapshot),
        Err(_) => no_heavy(),
    };
    claims.tick(&view);
}

fn handles_claims(request: &Request) -> bool {
    matches!(
        request,
        Request::WorkspaceResolve(_)
            | Request::WorkspaceList
            | Request::ClaimsAcquire(_)
            | Request::ClaimsAttach { .. }
            | Request::ClaimsStarted(_)
            | Request::ClaimsRelease(_)
            | Request::ClaimsForceRelease { .. }
            | Request::ClaimsWithdraw { .. }
            | Request::ClaimsRenew { .. }
            | Request::ClaimsSetCommand { .. }
            | Request::ClaimsReserve { .. }
            | Request::ClaimsUnreserve { .. }
            | Request::ClaimsSnapshot
            | Request::ClaimsHistory(_)
            | Request::ClaimsSeizeBegin(_)
            | Request::ClaimsSeizeCommit(_)
            | Request::ClaimsSeizeAbort { .. }
            | Request::ClaimsSeizeProgress(_)
            | Request::StatusOverview
    )
}

fn handles_devices(request: &Request) -> bool {
    matches!(
        request,
        Request::DevicesList { .. }
            | Request::DevicesShow { .. }
            | Request::DevicesResolve { .. }
            | Request::DevicesAdd { .. }
            | Request::DevicesRename { .. }
            | Request::DevicesNote { .. }
            | Request::DevicesRemove { .. }
            | Request::DevicesInstalls { .. }
            | Request::DevicesBoot { .. }
            | Request::DevicesTools(_)
            | Request::DevicesForgetSessions { .. }
    )
}

/// How long `devices.show { fresh_sessions }` waits for a sessions read that started after the
/// call (L49, P2-15), within the client's 10 s bound.
const FRESH_SESSIONS_WAIT: Duration = Duration::from_secs(6);

/// `devices.*` (P3b Task 5). None runs under the claims or heavy lock, except `remove`'s one
/// `device_in_use` question (P2-14), which reads only the claims store.
async fn devices_reply(id: u64, request: Request, shared: &Shared) -> ServerFrame {
    let devices = &shared.devices;
    match request {
        // It may ask a foreign adb server again (a socket read, P22-1): on the blocking pool,
        // never on a runtime worker (L76).
        Request::DevicesList { all } => {
            let devices = devices.clone();
            let listed = shared.jobs.spawn_blocking(move || devices.list(all)).await;
            response(
                id,
                listed.map_err(|_| internal_error("the devices list failed")),
            )
        }
        Request::DevicesShow {
            device,
            fresh_sessions,
        } => {
            let unknown = if fresh_sessions {
                let asked = crate::heavy::engine::Clock::now_ms(&crate::heavy::engine::SystemClock);
                let fresh =
                    agent_device::read_started_after(devices, asked, FRESH_SESSIONS_WAIT).await;
                (!fresh).then(|| {
                    devices
                        .sessions_unreadable()
                        .unwrap_or_else(|| "agent-device status not read in time".into())
                })
            } else {
                None
            };
            // CRB-4: a read that failed or came late is said, never answered as fresh.
            let shown = devices.show(&device).map(|mut detail| {
                detail.sessions_unknown = unknown;
                detail
            });
            response(id, shown)
        }
        Request::DevicesResolve { device } => response(id, devices.resolve(&device)),
        Request::DevicesAdd {
            device,
            alias,
            person,
        } => response(id, devices.add(&device, alias.as_deref(), person)),
        Request::DevicesRename {
            device,
            alias,
            person,
        } => response(id, devices.rename(&device, &alias, person)),
        Request::DevicesNote { device, note } => {
            response(id, devices.note(&device, note.as_deref()))
        }
        Request::DevicesRemove { device, person } => {
            // 1. Mark it removing (the catalog lists it unavailable); 2. ask the engine under
            // the claims lock, released at once; 3. unregister unless it is in use.
            let resource = match devices.begin_remove(&device, person) {
                Ok(r) => r,
                Err(e) => return response::<Value>(id, Err(e)),
            };
            let in_use = match tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(shared)).await {
                Ok(claims) => claims.device_in_use(&resource),
                // Fail closed: an unanswered engine counts as in use.
                Err(_) => crate::claims::engine::InUse {
                    claims: Vec::new(),
                    waiting: 1,
                },
            };
            response(
                id,
                // The device `begin_remove` marked and the engine was asked about (P3bT5-5).
                devices
                    .remove(&resource, person, in_use)
                    .map(|()| crate::contract::devices::DeviceRemoved { removed: resource }),
            )
        }
        Request::DevicesInstalls { device, query } => {
            response(id, devices.installs(device.as_deref(), &query))
        }
        // P2-19: the claim is checked under the claims lock, released before the boot.
        Request::DevicesBoot {
            resource,
            claim_id,
            workspace_id,
        } => {
            let snapshot = match tokio::time::timeout(CLAIMS_LOCK_BOUND, claims_lock(shared)).await
            {
                Ok(claims) => claims.snapshot(),
                Err(_) => Err(crate::contract::secrets::errors::bad_request(
                    "the claims engine did not answer; try again".into(),
                )),
            };
            let checked = snapshot.and_then(|s| {
                crate::devices::boot::check_claim(&s, &resource, &claim_id, &workspace_id)
            });
            match checked {
                Ok(()) => response(
                    id,
                    crate::devices::boot::boot(devices, &resource, &claim_id).await,
                ),
                Err(e) => response::<Value>(id, Err(e)),
            }
        }
        // Settings writes, the tool search and the same re-ask: on the blocking pool too (L76).
        Request::DevicesTools(set) => {
            let devices = devices.clone();
            let set = shared
                .jobs
                .spawn_blocking(move || devices.set_tools(set))
                .await;
            response(
                id,
                set.map_err(|_| internal_error("the devices tools call failed"))
                    .and_then(|status| status),
            )
        }
        Request::DevicesForgetSessions {
            device,
            person,
            by_workspace_path,
            by_pid,
        } => response(
            id,
            agent_device::forget_sessions(
                devices,
                &shared.claims,
                &device,
                person,
                by_workspace_path,
                by_pid,
            )
            .await,
        ),
        _ => unreachable!("handle_line routes only devices requests here"),
    }
}

/// `workspace.*`, `claims.*` and `status.overview`. Methods that need the heavy queue take its
/// view first (heavy lock taken and released), then the claims lock.
async fn claims_reply(id: u64, request: Request, shared: &Shared) -> ServerFrame {
    if matches!(request, Request::StatusOverview) {
        return response(id, overview(shared).await);
    }
    // Its filesystem work runs off the claims lock; it takes the lock only for store steps.
    let request = match request {
        Request::WorkspaceResolve(r) => {
            return response(id, resolve_workspace(&shared.claims, r).await);
        }
        other => other,
    };
    // P-2: the one fresh agent-device read before a takeover commit, before the claims lock;
    // the commit refuses on a failed one where it matters (P3-1).
    let refresh = if matches!(request, Request::ClaimsSeizeCommit(_)) {
        agent_device::refresh_now(&shared.devices).await
    } else {
        Ok(())
    };
    let snapshot = match request {
        Request::ClaimsAcquire(_)
        | Request::ClaimsSeizeBegin(_)
        | Request::ClaimsSeizeCommit(_) => match heavy_lock(shared).await.snapshot() {
            Ok(snapshot) => Some(snapshot),
            Err(e) => return response::<Value>(id, Err(e)),
        },
        _ => None,
    };
    let mut claims = claims_lock(shared).await;
    let view = snapshot.map_or_else(no_heavy, |s| heavy_view(&s));
    match request {
        Request::WorkspaceList => response(id, claims.workspaces()),
        Request::ClaimsAcquire(r) => response(id, claims.acquire(r, &view)),
        Request::ClaimsAttach { request_id } => response(id, claims.attach(&request_id)),
        Request::ClaimsStarted(s) => response(id, claims.started(s)),
        Request::ClaimsRelease(r) => response(id, claims.release(r)),
        Request::ClaimsForceRelease {
            claim_id,
            confirmation,
            workspace_id,
            requester,
        } => response(
            id,
            claims.force_release(&claim_id, &confirmation, &workspace_id, &requester),
        ),
        Request::ClaimsWithdraw {
            request_id,
            timed_out,
        } => response(id, claims.withdraw(&request_id, timed_out)),
        Request::ClaimsRenew {
            claim_id,
            workspace_id,
        } => response(id, claims.renew(&claim_id, &workspace_id)),
        Request::ClaimsSetCommand {
            claim_id,
            workspace_id,
            command,
        } => response(id, claims.set_command(&claim_id, &workspace_id, &command)),
        Request::ClaimsReserve {
            resource,
            workspace_id,
        } => response(id, claims.reserve(&resource, &workspace_id)),
        Request::ClaimsUnreserve {
            resource,
            workspace_id,
        } => response(id, claims.unreserve(&resource, &workspace_id)),
        Request::ClaimsSnapshot => response(id, claims.snapshot()),
        Request::ClaimsHistory(q) => response(id, claims.history(q)),
        Request::ClaimsSeizeBegin(s) => response(id, claims.seize_begin(s, &view)),
        Request::ClaimsSeizeCommit(c) => {
            response(id, claims.seize_commit_after_read(c, &view, &refresh))
        }
        Request::ClaimsSeizeAbort { plan_id } => response(id, claims.seize_abort(&plan_id)),
        Request::ClaimsSeizeProgress(p) => response(
            id,
            claims
                .seize_progress(&p.plan_id, &p.requester)
                .map(|expires_at_ms| crate::contract::claims::SeizeProgressReply { expires_at_ms }),
        ),
        _ => unreachable!("handles_claims routes only these"),
    }
}

/// Heavy and claims one after the other, never both; then the secrets status on the blocking
/// pool, with no engine lock held.
async fn overview(shared: &Shared) -> Result<Overview, HostlerError> {
    let heavy = heavy_lock(shared).await.snapshot()?;
    let claims = claims_lock(shared).await.snapshot()?;
    let secrets = shared.secrets.clone();
    let status = shared
        .jobs
        .spawn_blocking(move || secrets.dispatch(Request::SecretsStatus))
        .await
        // A panicked or cancelled secrets job (CR-B-8).
        .map_err(|_| internal_error("the secrets job failed"))??;
    let secrets: SecretsStatus = match status.reply {
        SecretsReply::Value(value) => serde_json::from_value(value).map_err(|_| HostlerError {
            code: ErrorCode::Internal,
            message: "cannot read the secrets status".into(),
            details: json!({}),
            recovery: None,
        })?,
        SecretsReply::Sensitive(_) => unreachable!("secrets.status carries no values"),
    };
    Ok(Overview {
        service_version: lifecycle::SERVICE_VERSION.into(),
        heavy,
        claims,
        secrets,
    })
}

async fn status(shared: &Shared) -> Result<ServiceStatus, HostlerError> {
    let snapshot = heavy_lock(shared).await.snapshot()?;
    Ok(ServiceStatus {
        service_version: lifecycle::service_version(),
        contract_version: shared.contract_version,
        pid: std::process::id(),
        running_heavy: snapshot.running.len() as u32,
        waiting_heavy: snapshot.waiting.len() as u32,
        secrets_unlocked: Some(shared.secrets.is_unlocked()),
        boot_id: crate::procs::local_boot_id(),
        instance: Some(shared.instance.clone()),
        exe: shared.exe.clone(),
    })
}

/// `projects.snapshot`: read under the claims lock, where `projects.changed` is published
/// (spec §6.2).
async fn projects_snapshot(shared: &Shared) -> Result<ProjectsSnapshot, HostlerError> {
    let claims = claims_lock(shared).await;
    claims.projects_snapshot()
}

/// Publishes `projects.changed` under the claims lock when the projects differ from the last
/// published ones.
async fn publish_projects(shared: &Shared) {
    claims_lock(shared).await.publish_projects();
}

fn dispatch(id: u64, request: Request, engine: &mut HeavyEngine) -> ServerFrame {
    match request {
        Request::HeavyAcquire(req) => response(id, engine.acquire(req)),
        Request::HeavyAttach { id: entry } => response(id, engine.attach(&entry)),
        Request::HeavyRelease(r) => response(id, engine.release(r)),
        Request::HeavyWithdraw {
            id: entry,
            timed_out,
        } => response(id, engine.withdraw(&entry, timed_out)),
        Request::HeavyForceRelease { id: entry } => response(id, engine.force_release(&entry)),
        Request::QueueSnapshot => response(id, engine.snapshot()),
        Request::QueueHistory(q) => response(id, engine.history(q)),
        Request::QueueSetCapacity { capacity } => response(id, engine.set_capacity(capacity)),
        Request::Hello { .. } | Request::ServiceStatus | Request::ServiceShutdown { .. } => {
            unreachable!("lifecycle requests are answered before dispatch")
        }
        _ => unreachable!("secrets requests are answered before dispatch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O-1b: a request or tick at its threshold is logged with its lock waits; a faster one is
    /// not.
    #[test]
    fn slow_line_only_at_the_threshold() {
        let waits = LockWaits {
            heavy: Duration::from_millis(1500),
            claims: Duration::from_millis(250),
        };
        assert_eq!(
            slow_line("request claims.acquire", SLOW_REQUEST, waits, SLOW_REQUEST).as_deref(),
            Some("slow request claims.acquire: 2000 ms (heavy lock 1500 ms, claims lock 250 ms)")
        );
        let fast = SLOW_REQUEST - Duration::from_millis(1);
        assert_eq!(
            slow_line("request claims.acquire", fast, waits, SLOW_REQUEST),
            None
        );
        assert_eq!(
            slow_line("claims tick", Duration::from_millis(999), waits, SLOW_TICK),
            None
        );
    }

    /// The method only: a value in the params never reaches the log line.
    #[test]
    fn method_of_names_the_method_only() {
        let line = r#"{"id":7,"method":"secrets.unlock","params":{"password":"hunter2"}}"#;
        assert_eq!(method_of(line), "secrets.unlock");
        assert_eq!(method_of("{"), "an unreadable request");
    }

    /// Lock waits are split by lock, counted inside `timed` only, and also when a bound gives up.
    #[tokio::test]
    async fn timed_splits_the_lock_waits() {
        let heavy = Arc::new(Mutex::new(()));
        let claims = Mutex::new(());
        let held = heavy.clone().lock_owned().await;
        let release = tokio::spawn(async move {
            // Far apart from the claims wait below, so a late timer on a loaded machine never
            // blurs the split.
            tokio::time::sleep(Duration::from_millis(600)).await;
            drop(held);
        });
        let ((), took, waits) = timed(async {
            drop(waited(Lock::Heavy, heavy.lock()).await);
            drop(waited(Lock::Claims, claims.lock()).await);
            let stuck = claims.lock().await;
            let gave_up = tokio::time::timeout(
                Duration::from_millis(50),
                waited(Lock::Claims, claims.lock()),
            )
            .await;
            assert!(gave_up.is_err());
            drop(stuck);
        })
        .await;
        release.await.unwrap();
        assert!(waits.heavy >= Duration::from_millis(590), "{waits:?}");
        assert!(waits.claims >= Duration::from_millis(50), "{waits:?}");
        assert!(waits.claims < Duration::from_millis(500), "{waits:?}");
        assert!(took >= waits.heavy + waits.claims, "{took:?} {waits:?}");
        // Outside `timed` nothing is counted, and nothing fails.
        drop(waited(Lock::Heavy, heavy.lock()).await);
    }

    #[test]
    fn peer_with_other_euid_rejected() {
        assert!(peer_allowed(Some(501), 501));
        assert!(!peer_allowed(Some(0), 501));
        assert!(!peer_allowed(Some(502), 501));
        assert!(!peer_allowed(None, 501));
    }

    #[test]
    fn unreadable_sensitive_frames_get_a_fixed_text() {
        let cases = [
            r#"{"id":7,"method":"secret.set","params":{"value":12,"name":"hunter2-canary"}}"#,
            r#"{"id":7,"method":"secret.set","params":{"value":"hunter2-canary"}}"#,
            r#"{"id":8,"method":"secret.materialize","params":"curl hunter2-canary"}"#,
            r#"{"id":8,"method":"secret.materialize","params":{"names":"hunter2-canary"}}"#,
            r#"{"id":9,"method":"secrets.unlock","params":{"password":["hunter2-canary"]}}"#,
            // The method cannot be read: treated as sensitive.
            r#"{"id":10,"method":7,"params":"hunter2-canary"}"#,
        ];
        for (case, line) in cases.into_iter().enumerate() {
            let e = serde_json::from_str::<ClientFrame>(line).unwrap_err();
            let ServerFrame::Error { id, error } = unreadable(line, &e) else {
                panic!("case {case}: not an error frame");
            };
            assert!(id >= 7, "case {case}");
            assert_eq!(error.message, "unreadable request", "case {case}");
        }
        // Other methods keep serde's diagnosis.
        let line = r#"{"id":3,"method":"queue.set_capacity","params":{"capacity":"x"}}"#;
        let e = serde_json::from_str::<ClientFrame>(line).unwrap_err();
        let ServerFrame::Error { id, error } = unreadable(line, &e) else {
            panic!("not an error frame");
        };
        assert_eq!(id, 3);
        assert!(error.message.starts_with("unreadable request: "));
    }

    #[test]
    fn sensitive_line_is_an_answer_frame() {
        let line = sensitive_line(42, r#"[{"name":"A","value":"v"}]"#);
        let frame: ServerFrame = serde_json::from_str(&line).unwrap();
        assert!(matches!(frame, ServerFrame::Response { id: 42, .. }));
    }

    /// A service core with in-memory stores; `running` seeds a running heavy entry.
    fn shared(dir: &std::path::Path, running: bool) -> Shared {
        shared_with(dir, running, 16)
    }

    /// `shared` with the broadcast channel's capacity.
    fn shared_with(dir: &std::path::Path, running: bool, capacity: usize) -> Shared {
        use std::sync::atomic::{AtomicI64, Ordering};

        use crate::contract::heavy::HeavyAcquire;
        use crate::heavy::engine::{Clock, NoForeign};
        use crate::procs::ProcessIdentity;
        use crate::procs::fake::FakeTable;
        use crate::secrets::vault::{KdfChoice, Vault};
        use crate::store::Store;

        struct FixedClock(AtomicI64);
        impl Clock for FixedClock {
            fn now_ms(&self) -> i64 {
                self.0.load(Ordering::SeqCst)
            }
        }
        let table = Arc::new(FakeTable::new("b1"));
        table.insert(42, None, Some(420));
        let heavy_store = Store::open_in_memory().unwrap();
        if running {
            let req = HeavyAcquire {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "gradle".into(),
                command: "./gradlew build".into(),
                cwd: "/work/app".into(),
                workspace_root: None,
                holder: ProcessIdentity {
                    pid: 42,
                    boot_id: "b1".into(),
                    start_time: 420,
                },
                reentrant_token: None,
                no_wait: false,
            };
            heavy_store.insert_waiting(&req, 0).unwrap();
            heavy_store.mark_running(&req.id, 0).unwrap();
        }
        let clock = Arc::new(FixedClock(AtomicI64::new(1_000)));
        let events = EventBus::new(capacity);
        let kdbx = dir.join("hostler.kdbx");
        Shared {
            engine: Mutex::new(HeavyEngine::new(
                heavy_store,
                table.clone(),
                clock.clone(),
                Box::new(NoForeign),
                events.clone(),
            )),
            claims: Mutex::new(ClaimsEngine::new(
                Store::open_in_memory().unwrap(),
                table.clone(),
                clock.clone(),
                vec![Box::new(crate::claims::ScreenCatalog)],
                Vec::new(),
                events.clone(),
            )),
            secrets: Arc::new(SecretsEngine::new(
                Vault::new(kdbx.clone()),
                kdbx,
                Store::open_in_memory().unwrap(),
                table,
                clock,
                events.clone(),
                KdfChoice::Fast,
            )),
            devices: DevicesService::new(
                Store::open_in_memory().unwrap(),
                crate::devices::tools::ToolPaths::default(),
                events.clone(),
            )
            .0,
            jobs: TaskTracker::new(),
            events,
            contract_version: lifecycle::service_contract_version(),
            shutdown: watch::channel(false).0,
            instance: "test-instance".into(),
            exe: None,
        }
    }

    /// A connection that said hello.
    async fn greeted(shared: &Shared) -> ConnState {
        let mut conn = ConnState::new();
        let hello = format!(
            r#"{{"id":1,"method":"hello","params":{{"contract_version":{},"client":"t"}}}}"#,
            shared.contract_version
        );
        handle_line(&hello, &mut conn, shared).await.unwrap();
        assert!(conn.hello_ok);
        conn
    }

    async fn error_of(line: &str, conn: &mut ConnState, shared: &Shared) -> HostlerError {
        match handle_line(line, conn, shared).await {
            Some(Reply::Frame(ServerFrame::Error { error, .. })) => error,
            _ => panic!("{line}: not an error frame"),
        }
    }

    async fn result_of(line: &str, conn: &mut ConnState, shared: &Shared) -> Value {
        match handle_line(line, conn, shared).await {
            Some(Reply::Frame(ServerFrame::Response { result, .. })) => result,
            _ => panic!("{line}: not an answer"),
        }
    }

    /// P3b Task 5: `devices.*` answer from the device service; only `devices.boot` is still a
    /// stub (Task 7).
    #[tokio::test]
    async fn devices_methods_answer_from_the_service() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(dir.path(), false);
        let mut conn = greeted(&shared).await;
        let list = result_of(
            r#"{"id":3,"method":"devices.list","params":{"all":true}}"#,
            &mut conn,
            &shared,
        )
        .await;
        assert_eq!(list["devices"], json!([]));
        assert_eq!(list["tools"]["adb_state"], "missing");
        for (method, params) in [
            (
                "devices.show",
                r#"{"device":"pixel-9","fresh_sessions":false}"#,
            ),
            ("devices.resolve", r#"{"device":"pixel-9"}"#),
            (
                "devices.add",
                r#"{"device":"SERIAL0001","alias":null,"person":true}"#,
            ),
            ("devices.remove", r#"{"device":"pixel-9","person":true}"#),
        ] {
            let line = format!(r#"{{"id":9,"method":"{method}","params":{params}}}"#);
            let error = error_of(&line, &mut conn, &shared).await;
            assert_eq!(error.code, ErrorCode::DeviceNotFound, "{method}");
            assert_eq!(
                error.recovery.as_deref(),
                Some("hostler devices list --all")
            );
        }
        let tools = result_of(
            r#"{"id":4,"method":"devices.tools","params":{"adb":null,"agent_device":null,"node":null,"detected_script":null,"explicit_agent_device":false,"android_env":{}}}"#,
            &mut conn,
            &shared,
        )
        .await;
        assert_eq!(tools["adb_state"], "missing");
        let error = error_of(
            r#"{"id":5,"method":"devices.tools","params":{"adb":"relative/adb","agent_device":null,"node":null,"detected_script":null,"explicit_agent_device":false,"android_env":{}}}"#,
            &mut conn,
            &shared,
        )
        .await;
        assert_eq!(error.code, ErrorCode::BadRequest);
        let installs = result_of(
            r#"{"id":6,"method":"devices.installs","params":{"device":null,"query":{"since_ms":null,"limit":10}}}"#,
            &mut conn,
            &shared,
        )
        .await;
        assert_eq!(installs, json!([]));
        let error = error_of(
            r#"{"id":7,"method":"devices.boot","params":{"resource":"device:android:avd:p","claim_id":"c1","workspace_id":"w"}}"#,
            &mut conn,
            &shared,
        )
        .await;
        // Task 7: a claim id that is no live claim of the workspace is refused (P2-19).
        assert_eq!(error.code, crate::contract::ErrorCode::BadRequest);
        assert!(
            error
                .message
                .starts_with("c1 is not a live lease or hold of this workspace"),
            "{}",
            error.message
        );
        // Still behind the hello, as every versioned method.
        let mut fresh = ConnState::new();
        let line = r#"{"id":2,"method":"devices.list","params":{"all":false}}"#;
        let error = error_of(line, &mut fresh, &shared).await;
        assert_eq!(error.code, ErrorCode::ContractMismatch);
    }

    #[tokio::test]
    async fn forget_sessions_without_person_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(dir.path(), false);
        let mut conn = greeted(&shared).await;
        let line = r#"{"id":4,"method":"devices.forget_sessions","params":{"device":"pixel-9","person":false}}"#;
        let error = error_of(line, &mut conn, &shared).await;
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(
            error.message,
            "`hostler devices forget-sessions pixel-9` needs a person at a terminal. A person \
             runs this; an agent asks the operator."
        );
        assert_eq!(
            error.recovery.as_deref(),
            Some("hostler devices forget-sessions pixel-9")
        );
    }

    /// T6-6: an accepted shutdown closes secrets admission before its reply is written; a
    /// refused one does not.
    #[tokio::test]
    async fn accepted_shutdown_closes_secrets_admission_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let shutdown = r#"{"id":5,"method":"service.shutdown","params":{"if_idle":true}}"#;
        for running in [true, false] {
            let shared = shared(dir.path(), running);
            let mut conn = ConnState::new();
            let reply = handle_line(shutdown, &mut conn, &shared).await.unwrap();
            let status = shared.secrets.reply(Request::SecretsStatus);
            if running {
                assert!(matches!(reply, Reply::Frame(ServerFrame::Error { .. })));
                assert!(status.is_ok(), "a refused stop must not close admission");
            } else {
                // The reply has not been queued, let alone written.
                assert!(matches!(reply, Reply::Stop(_)));
                let e = status.err().expect("admission is closed");
                assert_eq!(e.code, ErrorCode::ServiceUnavailable);
                assert_eq!(e.details["reason"], "stopping");
            }
        }
    }

    /// Task 5: an accepted stop closes claims admission before its reply is written; a refused
    /// one (a lease runs) leaves it open.
    #[tokio::test]
    async fn accepted_stop_grants_nothing_more() {
        use crate::contract::claims::{ClaimRequest, Purpose, ResourceId, Selector};
        use crate::procs::ProcessIdentity;

        let dir = tempfile::tempdir().unwrap();
        let shared = shared(dir.path(), false);
        let mut conn = ConnState::new();
        let hello = format!(
            r#"{{"id":1,"method":"hello","params":{{"contract_version":{},"client":"t"}}}}"#,
            shared.contract_version
        );
        handle_line(&hello, &mut conn, &shared).await.unwrap();
        assert!(conn.hello_ok);
        let shutdown = r#"{"id":2,"method":"service.shutdown","params":{"if_idle":true}}"#;
        let reply = handle_line(shutdown, &mut conn, &shared).await.unwrap();
        assert!(matches!(reply, Reply::Stop(_)));

        let request = ClaimRequest {
            id: uuid::Uuid::new_v4().to_string(),
            selector: Selector::Exact {
                resource: ResourceId::screen(),
            },
            workspace_id: "ws-1".into(),
            requester: ProcessIdentity {
                pid: 42,
                boot_id: "b1".into(),
                start_time: 420,
            },
            purpose: Purpose::Lease {
                command: "make".into(),
            },
            reentrant_tokens: Vec::new(),
            no_wait: false,
        };
        let line = serde_json::to_string(&serde_json::json!({
            "id": 3,
            "method": "claims.acquire",
            "params": request,
        }))
        .unwrap();
        let Some(Reply::Frame(ServerFrame::Error { error, .. })) =
            handle_line(&line, &mut conn, &shared).await
        else {
            panic!("acquire after an accepted stop must be refused");
        };
        assert_eq!(error.code, ErrorCode::ServiceUnavailable);
        assert_eq!(error.details["reason"], "stopping");
    }

    /// Task 1 (P3a): every new method is routed to an engine.
    #[test]
    fn new_methods_are_routed() {
        use crate::contract::accounts::{AccountMaterialize, AccountRemove, AccountSet};
        use crate::contract::claims::{
            ClaimRelease, ClaimRequest, ClaimStarted, Purpose, ResourceId, SeizeBegin, SeizeCommit,
            Selector, WorkspaceResolve,
        };
        use crate::contract::heavy::HistoryQuery;
        use crate::contract::secrets::ProjectRef;
        use crate::procs::{ProcessIdentity, RunGroup};
        use crate::secrets::value::RawCommand;
        use crate::secrets::value::SecretValue;

        let project = ProjectRef {
            common_dir: "/work/app/.git".into(),
            name: "app".into(),
        };
        let me = ProcessIdentity {
            pid: 42,
            boot_id: "b1".into(),
            start_time: 420,
        };
        let request = ClaimRequest {
            id: uuid::Uuid::new_v4().to_string(),
            selector: Selector::Exact {
                resource: ResourceId::screen(),
            },
            workspace_id: "ws-1".into(),
            requester: me.clone(),
            purpose: Purpose::Lease {
                command: "make".into(),
            },
            reentrant_tokens: Vec::new(),
            no_wait: false,
        };
        let requests = [
            Request::ClaimsAcquire(request.clone()),
            Request::ClaimsStarted(ClaimStarted {
                claim_id: "c1".into(),
                child: me.clone(),
                group: RunGroup::UnixGroup {
                    pgid: 42,
                    boot_id: "b1".into(),
                },
            }),
            Request::ClaimsForceRelease {
                claim_id: "c1".into(),
                confirmation: "screen".into(),
                workspace_id: "ws-2".into(),
                requester: me.clone(),
            },
            Request::ClaimsSeizeBegin(SeizeBegin {
                resource: ResourceId::screen(),
                confirmation: "screen".into(),
                request,
            }),
            Request::ClaimsSeizeCommit(SeizeCommit {
                plan_id: "p1".into(),
                requester: me.clone(),
                evidence: Vec::new(),
            }),
            Request::AccountMaterialize(AccountMaterialize {
                run_id: "run-1".into(),
                claim_id: "c1".into(),
                requester: me,
                project: project.clone(),
                name: "qa-user".into(),
                command: RawCommand::new("make".into()),
                workspace: "/work/app".into(),
            }),
            Request::WorkspaceResolve(WorkspaceResolve {
                declared: None,
                project: project.clone(),
                root: "/work/app".into(),
            }),
            Request::WorkspaceList,
            Request::ClaimsAttach {
                request_id: "r1".into(),
            },
            Request::ClaimsRelease(ClaimRelease {
                claim_id: "c1".into(),
                workspace_id: "ws-1".into(),
                requester: ProcessIdentity {
                    pid: 42,
                    boot_id: "b1".into(),
                    start_time: 420,
                },
                exit_code: None,
            }),
            Request::ClaimsWithdraw {
                request_id: "r1".into(),
                timed_out: false,
            },
            Request::ClaimsRenew {
                claim_id: "h1".into(),
                workspace_id: "ws-1".into(),
            },
            Request::ClaimsSetCommand {
                claim_id: "c1".into(),
                workspace_id: "ws-1".into(),
                command: "make".into(),
            },
            Request::ClaimsReserve {
                resource: ResourceId::screen(),
                workspace_id: "ws-1".into(),
            },
            Request::ClaimsUnreserve {
                resource: ResourceId::screen(),
                workspace_id: "ws-1".into(),
            },
            Request::ClaimsSnapshot,
            Request::ClaimsHistory(HistoryQuery {
                since_ms: None,
                limit: 10,
            }),
            Request::ClaimsSeizeAbort {
                plan_id: "p1".into(),
            },
            Request::StatusOverview,
            Request::AccountList {
                project: project.clone(),
            },
            Request::AccountSet(AccountSet {
                project: project.clone(),
                name: "qa-user".into(),
                field: "session".into(),
                value: SecretValue::new("value-1".into()),
            }),
            Request::AccountRemove(AccountRemove {
                project: project.clone(),
                name: "qa-user".into(),
                field: None,
            }),
            Request::AccountCheck {
                project,
                name: "qa-user".into(),
            },
        ];
        assert_eq!(requests.len(), 23, "every new P3a method");
        // Task 5 routes workspace.*, claims.* and status.overview to the claims engine; Task 9
        // routes account.* to the secrets engine.
        let (routed, accounts): (Vec<Request>, Vec<Request>) =
            requests.into_iter().partition(handles_claims);
        assert_eq!(routed.len(), 18);
        assert_eq!(accounts.len(), 5);
        assert!(accounts.iter().all(SecretsEngine::handles));
    }

    // ---- P5 Task 1: event delivery (spec §6.2) ----

    use crate::contract::secrets::{RequestState, SecretRequestInfo};
    use crate::contract::wire::ResyncReason;

    /// A connection's read loop over a real local socket; the test is its writer: it takes
    /// lines from the connection's `WriterQueue` only when it reads (`next`, `drain`). `stall`
    /// holds every event slot, so the backlog forms exactly as behind a stalled socket (kernel
    /// buffer sizes play no part).
    struct Piped {
        /// The client's side, kept open for the connection's life.
        send: WriteHalf<Stream>,
        queue: Arc<WriterQueue>,
        /// The event slots the test holds while stalled.
        held: Option<tokio::sync::OwnedSemaphorePermit>,
        reader: tokio::task::JoinHandle<()>,
        _dir: tempfile::TempDir,
        _server_send: WriteHalf<Stream>,
    }

    fn line_of(queued: Queued) -> String {
        match queued.out {
            Out::Line(line) | Out::Flushed(line, _) => line,
            Out::Sensitive(line, _) => line.to_string(),
        }
    }

    /// Waits for `ready` (a failure bound only: the condition is the handshake).
    async fn until(what: &str, ready: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(tokio::time::Instant::now() < deadline, "never: {what}");
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    impl Piped {
        async fn new(shared: Arc<Shared>) -> Piped {
            use tokio::io::AsyncWriteExt;

            let dir = tempfile::tempdir().unwrap();
            let socket = SocketName::File(dir.path().join("t.sock"));
            let listener = bind(&socket).unwrap();
            let client = Stream::connect(socket_name(&socket).unwrap())
                .await
                .unwrap();
            let server = listener.accept().await.unwrap();
            let (recv, server_send) = tokio::io::split(server);
            let reader = FramedRead::new(recv, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
            let queue = Arc::new(WriterQueue::default());
            let reader = tokio::spawn({
                let queue = queue.clone();
                async move { read_loop(reader, queue, &shared, None).await }
            });
            let (_client_recv, mut send) = tokio::io::split(client);
            let hello = format!(
                "{{\"id\":1,\"method\":\"hello\",\"params\":{{\"contract_version\":{},\"client\":\"t\"}}}}\n",
                crate::contract::CONTRACT_VERSION
            );
            send.write_all(hello.as_bytes()).await.unwrap();
            let mut piped = Piped {
                send,
                queue,
                held: None,
                reader,
                _dir: dir,
                _server_send: server_send,
            };
            let reply = piped.next().await.expect("the hello reply");
            assert!(reply.contains("\"result\""), "{reply}");
            piped
        }

        async fn request(&mut self, line: &str) {
            use tokio::io::AsyncWriteExt;
            self.send.write_all(line.as_bytes()).await.unwrap();
            self.send.write_all(b"\n").await.unwrap();
        }

        /// The next line the writer would take, or `None` after 500 ms without one.
        async fn next(&mut self) -> Option<String> {
            tokio::time::timeout(Duration::from_millis(500), self.queue.next())
                .await
                .ok()
                .flatten()
                .map(line_of)
        }

        /// The writer is stalled: the test holds every event slot (none may be in use yet).
        async fn stall(&mut self) {
            let all = self
                .queue
                .slots
                .clone()
                .try_acquire_many_owned(EVENT_SLOTS as u32)
                .expect("no event line is queued before the stall");
            self.held = Some(all);
        }

        fn replies_queued(&self) -> usize {
            self.queue.lanes().replies.len()
        }

        /// The writer resumes: every event line, in order, until 500 ms pass without one.
        async fn drain(&mut self) -> Vec<(u64, Event)> {
            self.held = None;
            let mut got = Vec::new();
            while let Some(line) = self.next().await {
                match serde_json::from_str(&line).unwrap() {
                    ServerFrame::Event { event, seq } => got.push((seq, event)),
                    other => panic!("not an event: {other:?}"),
                }
            }
            got
        }
    }

    fn request_event(id: &str, n: u32) -> Event {
        Event::SecretRequestChanged(SecretRequestInfo {
            id: id.into(),
            project_id: "p1".into(),
            project_name: "app".into(),
            name: "API_KEY".into(),
            reason: format!("change {n}"),
            state: RequestState::Pending,
            requester_pid: 42,
            created_at_ms: i64::from(n),
            answered_at_ms: None,
        })
    }

    fn devices_event(n: i64) -> Event {
        let mut snapshot = crate::contract::devices::fixtures::snapshot();
        snapshot.at_ms = n;
        Event::DevicesChanged(Box::new(snapshot))
    }

    fn seizure_event(n: u32) -> Event {
        let mut seizure = crate::contract::claims::fixtures::seizure();
        seizure.id = format!("s{n}");
        Event::ClaimsSeized(seizure)
    }

    /// Publishes and lets the read loop take the event (no lag).
    async fn publish(shared: &Shared, event: Event) -> u64 {
        let seq = shared.events.publish_test(event);
        tokio::task::yield_now().await;
        seq
    }

    #[tokio::test]
    async fn slow_reader_gets_newest_per_key_in_seq_order() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(dir.path(), false));
        let mut piped = Piped::new(shared.clone()).await;
        piped.stall().await;
        // 40 changes to 5 request ids among 100 device snapshots, then 3 seizures.
        let mut last_request = std::collections::HashMap::new();
        let mut last_devices = 0;
        let (mut changes, mut snapshots) = (0u32, 0u32);
        for n in 0..140u32 {
            if n % 7 < 2 {
                let id = format!("r{}", changes % 5);
                let seq = publish(&shared, request_event(&id, n)).await;
                last_request.insert(id, (seq, n));
                changes += 1;
            } else {
                last_devices = publish(&shared, devices_event(i64::from(n))).await;
                snapshots += 1;
            }
        }
        assert_eq!((changes, snapshots), (40, 100));
        let mut seizures = Vec::new();
        for n in 0..3 {
            seizures.push(publish(&shared, seizure_event(n)).await);
        }
        let got = piped.drain().await;
        let seqs: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
        let mut want: Vec<u64> = last_request.values().map(|(s, _)| *s).collect();
        want.push(last_devices);
        want.extend(&seizures);
        want.sort();
        assert_eq!(seqs, want);
        assert_eq!(got.len(), 5 + 1 + 3);
        for (seq, event) in &got {
            match event {
                Event::SecretRequestChanged(info) => {
                    assert_eq!(last_request[&info.id].0, *seq);
                    assert_eq!(info.reason, format!("change {}", last_request[&info.id].1));
                }
                Event::DevicesChanged(s) => assert_eq!(s.as_of_seq, *seq),
                Event::ClaimsSeized(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn records_are_never_coalesced() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(dir.path(), false));
        let mut piped = Piped::new(shared.clone()).await;
        piped.stall().await;
        let mut seqs = Vec::new();
        for n in 0..3 {
            seqs.push(publish(&shared, seizure_event(n)).await);
        }
        let got = piped.drain().await;
        assert_eq!(got.iter().map(|(s, _)| *s).collect::<Vec<_>>(), seqs);
        assert!(got.iter().all(|(_, e)| matches!(e, Event::ClaimsSeized(_))));
    }

    #[tokio::test]
    async fn lag_sends_one_resync_then_later_events() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared_with(dir.path(), false, 4));
        let mut piped = Piped::new(shared.clone()).await;
        // The connection's task does not run between these (one thread): its receiver lags.
        for n in 0..20 {
            shared
                .events
                .publish_test(request_event(&format!("r{n}"), n));
        }
        let held_through = shared.events.current_seq();
        let got = piped.drain().await;
        assert_eq!(
            got,
            [(
                held_through,
                Event::Resync {
                    reason: ResyncReason::Lagged
                }
            )]
        );
        let e = publish(&shared, request_event("e", 99)).await;
        let got = piped.drain().await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, e);
        assert!(matches!(&got[0].1, Event::SecretRequestChanged(i) if i.id == "e"));
    }

    #[tokio::test]
    async fn overflow_sends_one_resync_then_later_events() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(dir.path(), false));
        let mut piped = Piped::new(shared.clone()).await;
        piped.stall().await;
        let mut last = 0;
        for n in 0..300 {
            last = publish(&shared, request_event(&format!("r{n}"), n)).await;
        }
        let got = piped.drain().await;
        assert_eq!(
            got,
            [(
                last,
                Event::Resync {
                    reason: ResyncReason::Overflow
                }
            )]
        );
        let e = publish(&shared, request_event("e", 999)).await;
        let got = piped.drain().await;
        assert_eq!(got.iter().map(|(s, _)| *s).collect::<Vec<_>>(), [e]);
    }

    #[tokio::test]
    async fn oversized_event_sends_resync() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(dir.path(), false));
        let mut piped = Piped::new(shared.clone()).await;
        let mut big = crate::contract::devices::fixtures::snapshot();
        big.unmapped_sessions = vec!["x".repeat(MAX_FRAME_BYTES)];
        let dropped = publish(&shared, Event::DevicesChanged(Box::new(big))).await;
        let got = piped.drain().await;
        assert_eq!(
            got,
            [(
                dropped,
                Event::Resync {
                    reason: ResyncReason::Oversized
                }
            )]
        );
        let e = publish(&shared, request_event("e", 1)).await;
        let got = piped.drain().await;
        assert_eq!(got.iter().map(|(s, _)| *s).collect::<Vec<_>>(), [e]);
    }

    /// A `queue.snapshot` between two queue events A < B: `A.seq <= as_of_seq < B.seq`.
    #[tokio::test]
    async fn as_of_seq_orders_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(dir.path(), false);
        let mut events = shared.events.subscribe();
        let mut conn = greeted(&shared).await;
        let set = |n: u8| {
            format!(r#"{{"id":5,"method":"queue.set_capacity","params":{{"capacity":{n}}}}}"#)
        };
        result_of(&set(2), &mut conn, &shared).await;
        let snapshot = result_of(r#"{"id":6,"method":"queue.snapshot"}"#, &mut conn, &shared).await;
        result_of(&set(3), &mut conn, &shared).await;
        let queue_seqs: Vec<u64> = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|p| matches!(p.event, Event::QueueChanged(_)))
            .map(|p| p.seq)
            .collect();
        let (a, b) = (
            queue_seqs[queue_seqs.len() - 2],
            queue_seqs[queue_seqs.len() - 1],
        );
        let as_of = snapshot["as_of_seq"].as_u64().unwrap();
        assert!(a <= as_of && as_of < b, "{a} <= {as_of} < {b}");
        assert_eq!(snapshot["capacity"], 2);
    }

    /// R2-3: a successful prove/unlock with a grant installs it; a failed one clears it;
    /// other methods never touch it.
    #[test]
    fn grant_plumbing() {
        let meta = GrantMeta {
            expires_at_ms: 5_000,
            generation_at_grant: 3,
        };
        let other = GrantMeta {
            expires_at_ms: 9_000,
            generation_at_grant: 4,
        };
        let mut grant = None;
        apply_grant(&mut grant, true, true, Some(meta));
        assert_eq!(grant, Some(meta));
        apply_grant(&mut grant, true, true, None);
        assert_eq!(grant, Some(meta));
        apply_grant(&mut grant, false, true, Some(other));
        assert_eq!(grant, Some(meta), "not a prove or unlock");
        apply_grant(&mut grant, false, false, None);
        assert_eq!(grant, Some(meta));
        apply_grant(&mut grant, true, false, Some(other));
        assert_eq!(grant, None);
    }

    /// The plumbing end to end: a successful unlock grants its connection; a wrong unlock on
    /// it clears the grant; another connection never has one.
    #[tokio::test]
    async fn unlock_installs_and_failure_clears_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(dir.path(), false);
        let mut conn = greeted(&shared).await;
        let mut other = greeted(&shared).await;
        let unlock = |password: &str, create: bool| {
            format!(
                r#"{{"id":7,"method":"secrets.unlock","params":{{"password":"{password}","key_file":null,"forget_key_file":false,"create":{create}}}}}"#
            )
        };
        let reply = handle_line(&unlock("master-pass-1", true), &mut conn, &shared)
            .await
            .unwrap();
        assert!(matches!(
            reply,
            Reply::Secrets(SecretsOut::Frame(ServerFrame::Response { .. }), _, Some(_))
        ));
        let grant = conn
            .grant
            .expect("a successful unlock grants its connection");
        assert_eq!(
            grant.expires_at_ms,
            1_000 + crate::secrets::gate::PROOF_TTL_MS
        );
        // The create was the vault's first generation change (P5 Task 4).
        assert_eq!(grant.generation_at_grant, 1);
        assert_eq!(other.grant, None);
        handle_line(r#"{"id":8,"method":"secrets.lock"}"#, &mut conn, &shared)
            .await
            .unwrap();
        assert!(conn.grant.is_some(), "a lock is not a prove or unlock");
        let reply = handle_line(&unlock("wrong-pass-1", false), &mut conn, &shared)
            .await
            .unwrap();
        assert!(matches!(
            reply,
            Reply::Secrets(SecretsOut::Frame(ServerFrame::Error { .. }), _, None)
        ));
        assert_eq!(conn.grant, None);
        let _ = handle_line(&unlock("wrong-pass-1", false), &mut other, &shared).await;
        assert_eq!(other.grant, None);
    }

    /// Contract 5's stubs answer `not_available_yet` until their tasks fill them; a gated one
    /// (Task 4) answers `proof_required` first without a grant. Task 5 fills `account.requests`.
    #[tokio::test]
    async fn p5_stubs_answer_not_available_yet() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(dir.path(), false);
        let mut conn = greeted(&shared).await;
        for (method, params) in [
            (
                "secret.reveal",
                r#"{"project":{"common_dir":"/w/.git","name":"w"},"name":"API_KEY"}"#,
            ),
            (
                "account.reveal",
                r#"{"project":{"common_dir":"/w/.git","name":"w"},"name":"qa"}"#,
            ),
            (
                "secrets.remove_preserved",
                r#"{"path":"/x/hostler.kdbx.prev-1"}"#,
            ),
            ("account.decline", r#"{"id":"r1"}"#),
            ("heavy.stop", r#"{"id":"e1","confirmation":"e1"}"#),
        ] {
            let line = format!(r#"{{"id":9,"method":"{method}","params":{params}}}"#);
            let error = match handle_line(&line, &mut conn, &shared).await.unwrap() {
                Reply::Frame(ServerFrame::Error { error, .. })
                | Reply::Secrets(SecretsOut::Frame(ServerFrame::Error { error, .. }), _, _) => {
                    error
                }
                _ => panic!("{method}: not an error"),
            };
            assert_eq!(error.code, ErrorCode::ProofRequired, "{method}");
        }
        for (method, params) in [("account.requests", "null")] {
            let line = if params == "null" {
                format!(r#"{{"id":9,"method":"{method}"}}"#)
            } else {
                format!(r#"{{"id":9,"method":"{method}","params":{params}}}"#)
            };
            let result = match handle_line(&line, &mut conn, &shared).await.unwrap() {
                Reply::Frame(ServerFrame::Response { result, .. })
                | Reply::Secrets(SecretsOut::Frame(ServerFrame::Response { result, .. }), _, _) => {
                    result
                }
                _ => panic!("{method}: not an answer"),
            };
            assert_eq!(result, json!([]), "{method}");
        }
        let requests = match handle_line(
            r#"{"id":10,"method":"requests.snapshot"}"#,
            &mut conn,
            &shared,
        )
        .await
        .unwrap()
        {
            Reply::Secrets(SecretsOut::Frame(ServerFrame::Response { result, .. }), _, _) => result,
            _ => panic!("requests.snapshot: not an answer"),
        };
        assert_eq!(requests["secrets"], json!([]));
        assert_eq!(requests["accounts"], json!([]));
        assert!(requests["as_of_seq"].is_u64());
    }

    /// C-A1, R-1. First, deterministically: a status read blocked inside its capture (holding
    /// the secrets store guard) stops no other engine: the queue and the projects publish
    /// meanwhile, and the read's point, taken once it resumes, comes after their events. Then
    /// two engines (the heavy queue and the secrets lock state) publish concurrently while two
    /// readers take snapshot answers: a client applying spec §6.2's rule discards the held
    /// events of a snapshot's kind numbered `<= as_of_seq`, and each snapshot equals the state
    /// after the last of them, so no discarded event is missing from it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn snapshots_name_exact_points_across_engines() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc as std_mpsc;

        use crate::contract::heavy::QueueSnapshot;
        use crate::contract::secrets::{LockState, SecretsStatus};

        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared_with(dir.path(), false, 1 << 16));
        let mut rx = shared.events.subscribe();
        let secrets = shared.secrets.clone();
        let unlock = move |create: bool| {
            secrets
                .reply(Request::SecretsUnlock(crate::contract::secrets::Unlock {
                    password: crate::secrets::value::SecretValue::new("master-pass-1".into()),
                    key_file: None,
                    forget_key_file: false,
                    create,
                }))
                .map(|_| ())
                .unwrap();
        };
        tokio::task::spawn_blocking({
            let unlock = unlock.clone();
            move || unlock(true)
        })
        .await
        .unwrap();
        let status = |shared: &Shared| -> SecretsStatus {
            match shared.secrets.reply(Request::SecretsStatus).unwrap() {
                SecretsReply::Value(v) => serde_json::from_value(v).unwrap(),
                SecretsReply::Sensitive(_) => unreachable!(),
            }
        };

        // 1. Deterministic (R-1): a status read blocked inside its capture.
        let (inside_tx, inside) = std_mpsc::channel();
        let (release, release_rx) = std_mpsc::channel::<()>();
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        let paused = Arc::new(AtomicBool::new(false));
        shared.secrets.set_test_hook(Some(Arc::new({
            let paused = paused.clone();
            move |at| {
                if at == "status_read" && !paused.swap(true, Ordering::SeqCst) {
                    inside_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                }
            }
        })));
        let reader = tokio::task::spawn_blocking({
            let shared = shared.clone();
            move || status(&shared)
        });
        // Handshake: the read is inside its capture, holding the store guard.
        tokio::task::spawn_blocking(move || inside.recv().unwrap())
            .await
            .unwrap();
        // Two other engines commit meanwhile (a bound only for a hang: they must not wait).
        let others = tokio::task::spawn_blocking({
            let shared = shared.clone();
            move || {
                shared.engine.blocking_lock().set_capacity(2).unwrap();
                shared.claims.blocking_lock().publish_projects();
            }
        });
        tokio::time::timeout(Duration::from_secs(30), others)
            .await
            .expect("another engine waited for the blocked capture")
            .unwrap();
        let committed: Vec<Published> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let queue_seq = committed
            .iter()
            .find(|p| matches!(p.event, Event::QueueChanged(_)))
            .expect("the queue event, broadcast while the read was blocked")
            .seq;
        let projects_seq = committed
            .iter()
            .find(|p| matches!(p.event, Event::ProjectsChanged(_)))
            .expect("the projects event, broadcast while the read was blocked")
            .seq;
        assert!(!reader.is_finished(), "the read is still blocked");
        release.send(()).unwrap();
        let read = reader.await.unwrap();
        shared.secrets.set_test_hook(None);
        assert!(
            read.as_of_seq >= queue_seq.max(projects_seq),
            "the read's point ({}) is taken after it resumes",
            read.as_of_seq
        );

        // 2. Concurrent: changes on both engines, snapshot answers read meanwhile.
        let queue_writer = tokio::task::spawn_blocking({
            let shared = shared.clone();
            move || {
                for n in 0..200u8 {
                    shared
                        .engine
                        .blocking_lock()
                        .set_capacity(1 + n % 3)
                        .unwrap();
                }
            }
        });
        let lock_writer = tokio::task::spawn_blocking({
            let shared = shared.clone();
            let unlock = unlock.clone();
            move || {
                for _ in 0..25 {
                    shared.secrets.reply(Request::SecretsLock).unwrap();
                    unlock(false);
                }
            }
        });
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let shared = shared.clone();
                tokio::task::spawn_blocking(move || {
                    let mut queue: Vec<QueueSnapshot> = Vec::new();
                    let mut locks: Vec<SecretsStatus> = Vec::new();
                    for _ in 0..150 {
                        queue.push(shared.engine.blocking_lock().snapshot().unwrap());
                        locks.push(status(&shared));
                    }
                    (queue, locks)
                })
            })
            .collect();
        queue_writer.await.unwrap();
        lock_writer.await.unwrap();
        let mut queue_reads = Vec::new();
        let mut lock_reads = Vec::new();
        for reader in readers {
            let (q, l) = reader.await.unwrap();
            queue_reads.extend(q);
            lock_reads.extend(l);
        }
        let events: Vec<Published> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let queue_events: Vec<(u64, u8)> = events
            .iter()
            .filter_map(|p| match &p.event {
                Event::QueueChanged(s) => Some((p.seq, s.capacity)),
                _ => None,
            })
            .collect();
        let lock_events: Vec<(u64, LockState)> = events
            .iter()
            .filter_map(|p| match &p.event {
                Event::SecretsLockChanged(s) => Some((p.seq, s.state)),
                _ => None,
            })
            .collect();
        assert!(queue_events.len() >= 100 && lock_events.len() >= 40);
        // The state after the last event numbered `<= as_of`, as the client rebuilds it.
        fn after<S: Copy>(events: &[(u64, S)], as_of: u64) -> Option<S> {
            events
                .iter()
                .rev()
                .find(|(seq, _)| *seq <= as_of)
                .map(|(_, s)| *s)
        }
        for read in &queue_reads {
            if let Some(capacity) = after(&queue_events, read.as_of_seq) {
                assert_eq!(read.capacity, capacity, "queue read at {}", read.as_of_seq);
            }
        }
        for read in &lock_reads {
            if let Some(state) = after(&lock_events, read.as_of_seq) {
                assert_eq!(read.state, state, "status read at {}", read.as_of_seq);
            }
        }
    }

    /// C-A2, R-2, R-4. Behind a stalled writer, waiting events and event lines in the
    /// writer's queue never hold more than `EVENT_BYTES`; past it the events become one
    /// `resync`; a reply meanwhile is queued in its own reserve and written first.
    #[tokio::test]
    async fn outbound_bytes_are_bounded_with_the_writer_queue() {
        use crate::service::events::EVENT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(dir.path(), false));
        let mut piped = Piped::new(shared.clone()).await;
        let big = |n: u32| {
            let mut seizure = crate::contract::claims::fixtures::seizure();
            seizure.id = format!("s{n}");
            seizure.from_workspace_path = "x".repeat(3 << 20);
            Event::ClaimsSeized(seizure)
        };
        // 20 records of 3 MiB; nobody writes: the writer's 16 slots alone would take 48 MiB.
        let mut last = 0;
        for n in 0..20 {
            last = publish(&shared, big(n)).await;
        }
        // Handshake: the reader has taken every record (the budget is as full as it gets).
        until("the records are queued", || {
            piped.queue.events_budget.in_use() >= 7 * (3 << 20)
        })
        .await;
        piped.request(r#"{"id":7,"method":"queue.snapshot"}"#).await;
        until("the reply is queued", || piped.replies_queued() == 1).await;
        assert!(piped.queue.events_budget.in_use() <= EVENT_BYTES);
        // The reply is written first, then the event lines.
        let reply = piped.next().await.unwrap();
        assert!(
            reply.starts_with("{\"id\":7,\"result\":"),
            "{}",
            &reply[..40]
        );
        let lines = piped.drain().await;
        let records: Vec<u64> = lines
            .iter()
            .filter(|(_, e)| matches!(e, Event::ClaimsSeized(_)))
            .map(|(seq, _)| *seq)
            .collect();
        assert_eq!(records.len(), 7, "as many as fit in EVENT_BYTES");
        let resyncs: Vec<&(u64, Event)> = lines
            .iter()
            .filter(|(_, e)| matches!(e, Event::Resync { .. }))
            .collect();
        assert_eq!(resyncs.len(), 1, "exactly one resync");
        // It stands for the first record that did not fit, and, unwritten, for the rest too.
        assert_eq!(
            *resyncs[0],
            (
                records.iter().max().unwrap() + 1,
                Event::Resync {
                    reason: ResyncReason::Overflow
                }
            )
        );
        assert!(resyncs[0].0 < last);
        assert_eq!(lines.last().unwrap(), resyncs[0]);
        assert_eq!(
            piped.queue.events_budget.in_use(),
            0,
            "every byte given back"
        );
        assert_eq!(piped.queue.replies_budget.in_use(), 0);
    }

    /// R-2: event lines own every event byte and slot (the writer is stalled); a large
    /// sensitive answer (3.6 MB of values) is still queued at once, from the reply reserve; an
    /// event meanwhile becomes the one `resync`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_large_sensitive_reply_never_waits_behind_events() {
        use crate::contract::secrets::{
            ImportEntry, Materialize, ProjectRef, SecretImport, Unlock,
        };
        use crate::secrets::value::{RawCommand, SecretValue};
        use crate::service::events::EVENT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("app");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let project = ProjectRef {
            common_dir: repo.join(".git").display().to_string(),
            name: "app".into(),
        };
        let shared = Arc::new(shared(dir.path(), false));
        let names: Vec<String> = (0..60).map(|n| format!("BIG_{n:02}")).collect();
        tokio::task::spawn_blocking({
            let (shared, project, names) = (shared.clone(), project.clone(), names.clone());
            move || {
                let call = |r| shared.secrets.reply(r).map(|_| ()).unwrap();
                call(Request::SecretsUnlock(Unlock {
                    password: SecretValue::new("master-pass-1".into()),
                    key_file: None,
                    forget_key_file: false,
                    create: true,
                }));
                call(Request::ProjectApprove(project.clone()));
                call(Request::SecretImport(SecretImport {
                    project,
                    entries: names
                        .iter()
                        .map(|name| ImportEntry {
                            name: name.clone(),
                            value: SecretValue::new("v".repeat(60_000)),
                        })
                        .collect(),
                    overwrite: false,
                }));
            }
        })
        .await
        .unwrap();
        // The approval's `projects.changed` is not this test's business.
        shared.events.take_projects_dirty();
        let mut piped = Piped::new(shared.clone()).await;
        piped.stall().await;
        // One small event waits in the backlog, holding bytes of the events' share.
        publish(&shared, request_event("f", 1)).await;
        until("f waits in the backlog", || {
            piped.queue.events_budget.in_use() > 0
        })
        .await;
        // Writer-owned event lines hold the rest of the events' bytes, and every slot.
        let budget = &piped.queue.events_budget;
        let owned = budget.try_take(budget.available()).unwrap();
        assert_eq!(budget.in_use(), EVENT_BYTES);
        let materialize = serde_json::to_string(&serde_json::json!({
            "id": 9,
            "method": "secret.materialize",
            "params": Materialize {
                run_id: "run-1".into(),
                project,
                names,
                command: RawCommand::new("make".into()),
                workspace: repo.display().to_string(),
            },
        }))
        .unwrap();
        piped.request(&materialize).await;
        // Queued without any event byte or slot coming back.
        until("the sensitive answer is queued", || {
            piped.replies_queued() == 1
        })
        .await;
        let answer = piped.next().await.unwrap();
        assert!(answer.starts_with("{\"id\":9,\"result\":"));
        assert!(answer.len() > 3_600_000, "{} bytes", answer.len());
        // An event with no room: the backlog (f) becomes the one resync, giving f's bytes back.
        let e = publish(&shared, request_event("e", 2)).await;
        until("the backlog became one resync", || {
            piped.queue.events_budget.in_use() < EVENT_BYTES
        })
        .await;
        drop(owned);
        assert_eq!(
            piped.drain().await,
            [(
                e,
                Event::Resync {
                    reason: ResyncReason::Overflow
                }
            )]
        );
    }

    /// R-3: the shutdown reply is a control line: with every event and reply byte owned by the
    /// writer's queue and nothing written, it is queued at once, the stop goes ahead, and the
    /// reply stays queued for the writer (never dropped).
    #[tokio::test]
    async fn the_shutdown_reply_is_never_dropped() {
        use crate::service::events::{EVENT_BYTES, REPLY_RESERVE};

        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(dir.path(), false));
        let mut stopping = shared.shutdown.subscribe();
        let mut piped = Piped::new(shared.clone()).await;
        piped.stall().await;
        let _events = piped.queue.events_budget.try_take(EVENT_BYTES).unwrap();
        let _replies = piped.queue.replies_budget.try_take(REPLY_RESERVE).unwrap();
        piped
            .request(r#"{"id":5,"method":"service.shutdown","params":{"if_idle":true}}"#)
            .await;
        until("the stop reply is queued", || piped.replies_queued() == 1).await;
        // The stop goes ahead after its flush deadline (nobody writes here).
        tokio::time::timeout(Duration::from_secs(30), stopping.wait_for(|stop| *stop))
            .await
            .expect("the stop goes ahead")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), &mut piped.reader)
            .await
            .expect("the reader ends")
            .unwrap();
        // Still queued for the writer, which drains it before it ends.
        let reply = piped.next().await.expect("the stop reply is still queued");
        assert_eq!(reply, r#"{"id":5,"result":{"stopping":true}}"#);
        assert_eq!(piped.next().await, None, "then the writer ends");
    }
}
