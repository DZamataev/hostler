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
use tokio::sync::{Mutex, MutexGuard, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;
use zeroize::Zeroizing;

use super::lifecycle;
use crate::claims::engine::{ClaimsEngine, heavy_view, no_heavy, resolve_workspace};
use crate::contract::accounts::AccountInfo;
use crate::contract::claims::{Overview, ResourceId, errors as claims_errors};
use crate::contract::heavy::{HeavyAcquire, ServiceStatus};
use crate::contract::secrets::{ProjectInfo, SecretsStatus};
use crate::contract::wire::{ClientFrame, Event, Request, ServerFrame, sensitive_request};
use crate::contract::{ErrorCode, HostlerError};
use crate::devices::agent_device;
use crate::devices::service::DevicesService;
use crate::heavy::engine::HeavyEngine;
use crate::paths::SocketName;
use crate::secrets::engine::{Reply as SecretsReply, SecretsEngine};

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
    events: broadcast::Sender<Event>,
    contract_version: u32,
    shutdown: watch::Sender<bool>,
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
    events: broadcast::Sender<Event>,
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
    });
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
                    Ok(Event::ClaimsChanged(snapshot)) => shared.devices.on_claims(&snapshot).await,
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
    let (recv, send) = tokio::io::split(stream);
    let reader = FramedRead::new(recv, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
    let writer = FramedWrite::new(send, LinesCodec::new_with_max_length(MAX_FRAME_BYTES));
    let (tx, rx) = mpsc::channel(16);
    tokio::join!(read_loop(reader, tx, &shared), write_loop(writer, rx));
}

async fn write_loop(
    mut writer: FramedWrite<WriteHalf<Stream>, LinesCodec>,
    mut rx: mpsc::Receiver<Out>,
) {
    while let Some(out) = rx.recv().await {
        let (sent, flushed) = match out {
            Out::Line(line) => (writer.send(line).await, None),
            Out::Flushed(line, done) => (writer.send(line).await, Some(done)),
            Out::Sensitive(line, done) => (writer.send(line.as_str()).await, Some(done)),
        };
        if sent.is_err() {
            return;
        }
        if let Some(done) = flushed {
            let _ = done.send(());
        }
    }
}

struct ConnState {
    hello_ok: bool,
    events: Option<broadcast::Receiver<Event>>,
}

async fn next_event(
    events: &mut Option<broadcast::Receiver<Event>>,
) -> Result<Event, broadcast::error::RecvError> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn read_loop(
    mut reader: FramedRead<ReadHalf<Stream>, LinesCodec>,
    tx: mpsc::Sender<Out>,
    shared: &Shared,
) {
    let mut conn = ConnState {
        hello_ok: false,
        events: None,
    };
    // Snapshots are coalesced: only the newest one waits for the writer.
    let mut pending_event: Option<String> = None;
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
                        if tx.send(Out::Line(encode_reply(&frame))).await.is_err() {
                            return;
                        }
                    }
                    Reply::Secrets(out, token) => {
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
                        let mut send = std::pin::pin!(tx.send(out));
                        let sent = match tokio::time::timeout_at(deadline, &mut send).await {
                            Ok(sent) => sent,
                            Err(_) => {
                                token.take();
                                send.await
                            }
                        };
                        if sent.is_err() {
                            return;
                        }
                        let _ = tokio::time::timeout_at(deadline, flushed).await;
                        drop(token);
                    }
                    Reply::Stop(frame) => {
                        // One deadline for queueing and writing the reply: a client that stopped
                        // reading (its queue full of snapshots) cannot hold up the stop.
                        let (done, flushed) = oneshot::channel();
                        let line = encode_reply(&frame);
                        let _ = tokio::time::timeout(STOP_REPLY_FLUSH, async {
                            if tx.send(Out::Flushed(line, done)).await.is_ok() {
                                let _ = flushed.await;
                            }
                        })
                        .await;
                        let _ = shared.shutdown.send(true);
                        return;
                    }
                }
            }
            received = next_event(&mut conn.events) => match received {
                Ok(event) => pending_event = encode_event(&event).or(pending_event),
                // A slow reader missed snapshots; the next change brings it up to date.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => conn.events = None,
            },
            permit = tx.reserve(), if pending_event.is_some() => {
                let Ok(permit) = permit else { return };
                permit.send(Out::Line(pending_event.take().expect("guarded by the branch condition")));
            }
        }
    }
}

enum Reply {
    Frame(ServerFrame),
    /// The reply to an accepted `service.shutdown`: written, then the service stops.
    Stop(ServerFrame),
    /// A secrets reply; the token keeps a stop waiting until it is written.
    Secrets(SecretsOut, TaskTrackerToken),
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

/// An event line, or `None` (logged) when it cannot be sent within the frame limit.
fn encode_event(event: &Event) -> Option<String> {
    let frame = ServerFrame::Event {
        event: event.clone(),
    };
    match serde_json::to_string(&frame) {
        Ok(line) if line.len() <= MAX_FRAME_BYTES => Some(line),
        Ok(line) => {
            tracing::error!(
                "dropped a {}-byte queue event over the frame limit",
                line.len()
            );
            None
        }
        Err(e) => {
            tracing::error!("cannot encode a queue event: {e}");
            None
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
async fn secrets_reply(id: u64, request: Request, shared: &Shared) -> Reply {
    let token = shared.jobs.token();
    let secrets = shared.secrets.clone();
    let job = shared
        .jobs
        .spawn_blocking(move || secrets.dispatch(request));
    let out = match job.await {
        Ok(Ok(SecretsReply::Value(result))) => {
            SecretsOut::Frame(ServerFrame::Response { id, result })
        }
        Ok(Ok(SecretsReply::Sensitive(json))) => SecretsOut::Sensitive(sensitive_line(id, &json)),
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
    Reply::Secrets(out, token)
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
    match reply {
        SecretsReply::Value(value) => {
            serde_json::from_value(value).map_err(|_| internal_error("unreadable secrets answer"))
        }
        SecretsReply::Sensitive(_) => Err(internal_error("unexpected sensitive answer")),
    }
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
        Err(e) => return Reply::Secrets(SecretsOut::Frame(response::<Value>(id, Err(e))), token),
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
                Ok(()) => return secrets_reply(id, Request::AccountMaterialize(m), shared).await,
                Err(e) => Err(e),
            }
        }
        Request::AccountRemove(r) => {
            let holder = claims_lock(shared).await.holder_of(&account(&r.name));
            match holder {
                None => return secrets_reply(id, Request::AccountRemove(r), shared).await,
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
    Reply::Secrets(SecretsOut::Frame(response(id, result)), token)
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
    let (reply, took, waits) = timed(handle_request(id, frame.request, conn, shared)).await;
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
            let result = lifecycle::hello(contract_version, shared.contract_version);
            conn.hello_ok = result.is_ok();
            conn.events = conn.hello_ok.then(|| shared.events.subscribe());
            response(id, result)
        }
        Request::ServiceStatus => response(id, status(shared).await),
        Request::ServiceShutdown { if_idle } => {
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
            return Some(match heavy.begin_shutdown(if_idle) {
                Ok(running) if !running.is_empty() => Reply::Frame(response::<Value>(
                    id,
                    Err(lifecycle::busy(&running, &[], &[])),
                )),
                Ok(_) => {
                    // Accepted: no new claims or secrets work from here on, before any reply
                    // I/O (T6-6).
                    match claims.as_mut() {
                        Some(claims) => claims.begin_stopping(),
                        None => tracing::warn!("stopping without the claims engine's answer"),
                    }
                    shared.secrets.begin_shutdown();
                    Reply::Stop(response(id, Ok(json!({ "stopping": true }))))
                }
                Err(e) => Reply::Frame(response::<Value>(id, Err(e))),
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
            return Some(secrets_reply(id, versioned, shared).await);
        }
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
                Ok(()) => dispatch(id, Request::HeavyStarted(s), &mut *heavy_lock(shared).await),
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
            let shown = devices.show(&device).and_then(|detail| {
                let mut value = serde_json::to_value(detail)
                    .map_err(|e| crate::contract::secrets::errors::bad_request(format!("{e}")))?;
                if let (Some(reason), Some(map)) = (unknown, value.as_object_mut()) {
                    map.insert(
                        crate::contract::devices::SESSIONS_UNKNOWN_KEY.into(),
                        Value::String(reason),
                    );
                }
                Ok(value)
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
                    .map(|()| json!({ "removed": resource })),
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
                .map(|expires_at_ms| json!({ "expires_at_ms": expires_at_ms })),
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
    let secrets: SecretsStatus = match status {
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
        service_version: lifecycle::SERVICE_VERSION.into(),
        contract_version: shared.contract_version,
        pid: std::process::id(),
        running_heavy: snapshot.running.len() as u32,
        waiting_heavy: snapshot.waiting.len() as u32,
    })
}

fn dispatch(id: u64, request: Request, engine: &mut HeavyEngine) -> ServerFrame {
    match request {
        Request::HeavyAcquire(req) => response(id, engine.acquire(req)),
        Request::HeavyAttach { id: entry } => response(id, engine.attach(&entry)),
        Request::HeavyStarted(s) => response(id, engine.started(s)),
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
        let (events, _) = broadcast::channel(16);
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
        }
    }

    /// A connection that said hello.
    async fn greeted(shared: &Shared) -> ConnState {
        let mut conn = ConnState {
            hello_ok: false,
            events: None,
        };
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
        let mut fresh = ConnState {
            hello_ok: false,
            events: None,
        };
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
            let mut conn = ConnState {
                hello_ok: false,
                events: None,
            };
            let reply = handle_line(shutdown, &mut conn, &shared).await.unwrap();
            let status = shared.secrets.dispatch(Request::SecretsStatus);
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
        let mut conn = ConnState {
            hello_ok: false,
            events: None,
        };
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
}
