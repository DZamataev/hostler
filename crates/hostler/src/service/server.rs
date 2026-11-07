//! The local-socket server: accepts connections from the same user, speaks newline-delimited JSON
//! frames, and dispatches versioned requests to the heavy engine (plan Tasks 6–7) and, on the
//! blocking pool, to the secrets engine (P2 Task 6).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use interprocess::local_socket::{ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;
use zeroize::Zeroizing;

use super::lifecycle;
use crate::contract::heavy::ServiceStatus;
use crate::contract::wire::{ClientFrame, Event, Request, ServerFrame, sensitive_request};
use crate::contract::{ErrorCode, HostlerError};
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

struct Shared {
    engine: Mutex<HeavyEngine>,
    secrets: Arc<SecretsEngine>,
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

/// Serves until `service.shutdown` or a termination signal. Then it refuses new secrets work,
/// keeps serving connections while running vault jobs finish (up to 30 s; past that the process
/// exits holding the service lock), and locks the vault. Before returning, every connection
/// task and the ticker are stopped, so nothing touches the store once the caller drops the lock.
pub async fn serve(
    listener: Listener,
    engine: HeavyEngine,
    secrets: SecretsEngine,
    events: broadcast::Sender<Event>,
) -> anyhow::Result<()> {
    let (shutdown, mut stopping) = watch::channel(false);
    let shared = Arc::new(Shared {
        engine: Mutex::new(engine),
        secrets: Arc::new(secrets),
        jobs: TaskTracker::new(),
        events,
        contract_version: lifecycle::service_contract_version(),
        shutdown,
    });
    shared.engine.lock().await.tick();

    let ticker = {
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(TICK_EVERY);
            loop {
                every.tick().await;
                shared.engine.lock().await.tick();
                let secrets = shared.secrets.clone();
                let _ = shared.jobs.spawn_blocking(move || secrets.tick()).await;
            }
        })
    };

    let mut connections = JoinSet::new();
    let terminated = termination();
    tokio::pin!(terminated);
    loop {
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
    ticker.abort();
    let _ = ticker.await;
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
                    "vault work still runs after {VAULT_DRAIN:?}; exiting while still holding                      the service lock"
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
    connections.shutdown().await;
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

async fn handle_line(line: &str, conn: &mut ConnState, shared: &Shared) -> Option<Reply> {
    let frame: ClientFrame = match serde_json::from_str(line) {
        Ok(frame) => frame,
        Err(e) => return Some(Reply::Frame(unreadable(line, &e))),
    };
    let id = frame.id;
    let reply = match frame.request {
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
            // The idle check and "grant nothing more" happen in one step under the engine lock.
            let refused_by = shared.engine.lock().await.begin_shutdown(if_idle);
            return Some(match refused_by {
                Ok(running) if !running.is_empty() => {
                    Reply::Frame(response::<Value>(id, Err(lifecycle::busy(&running))))
                }
                Ok(_) => {
                    // Accepted: no new secrets work from here on, before any reply I/O (T6-6).
                    shared.secrets.begin_shutdown();
                    Reply::Stop(response(id, Ok(json!({ "stopping": true }))))
                }
                Err(e) => Reply::Frame(response::<Value>(id, Err(e))),
            });
        }
        _ if !conn.hello_ok => {
            response::<Value>(id, Err(lifecycle::hello_required(shared.contract_version)))
        }
        versioned if SecretsEngine::handles(&versioned) => {
            return Some(secrets_reply(id, versioned, shared).await);
        }
        versioned => dispatch(id, versioned, &mut *shared.engine.lock().await),
    };
    Some(Reply::Frame(reply))
}

async fn status(shared: &Shared) -> Result<ServiceStatus, HostlerError> {
    let snapshot = shared.engine.lock().await.snapshot()?;
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
            secrets: Arc::new(SecretsEngine::new(
                Vault::new(kdbx.clone()),
                kdbx,
                Store::open_in_memory().unwrap(),
                table,
                clock,
                events.clone(),
                KdfChoice::Fast,
            )),
            jobs: TaskTracker::new(),
            events,
            contract_version: lifecycle::service_contract_version(),
            shutdown: watch::channel(false).0,
        }
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
}
