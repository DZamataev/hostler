//! The local-socket server: accepts connections from the same user, speaks newline-delimited JSON
//! frames, and dispatches versioned requests to the heavy engine (plan Tasks 6–7).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use interprocess::local_socket::{ListenerOptions, Name};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

use super::lifecycle;
use crate::contract::heavy::ServiceStatus;
use crate::contract::wire::{ClientFrame, Event, Request, ServerFrame};
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::HeavyEngine;
use crate::paths::SocketName;

/// The largest frame either side sends or accepts; a longer incoming line closes the connection.
/// The engine's admission limits keep every answer and snapshot below it.
pub const MAX_FRAME_BYTES: usize = 8 << 20;
const TICK_EVERY: Duration = Duration::from_secs(2);
/// How long an accepted `service.shutdown` waits for its reply to be queued and written; the
/// service stops either way.
const STOP_REPLY_FLUSH: Duration = Duration::from_secs(2);

struct Shared {
    engine: Mutex<HeavyEngine>,
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
        options.security_descriptor(windows_sd::current_user_only()?)
    };
    Ok(options.create_tokio()?)
}

/// Serves until `service.shutdown` or a termination signal. Before returning, every connection
/// task and the ticker are stopped, so nothing touches the store once the caller drops the lock.
pub async fn serve(
    listener: Listener,
    engine: HeavyEngine,
    events: broadcast::Sender<Event>,
) -> anyhow::Result<()> {
    let (shutdown, mut stopping) = watch::channel(false);
    let shared = Arc::new(Shared {
        engine: Mutex::new(engine),
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
    ticker.abort();
    let _ = ticker.await;
    connections.shutdown().await;
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
        let (line, flushed) = match out {
            Out::Line(line) => (line, None),
            Out::Flushed(line, done) => (line, Some(done)),
        };
        if writer.send(line).await.is_err() {
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
                let Some(reply) = handle_line(&line, &mut conn, shared).await else {
                    continue;
                };
                match reply {
                    Reply::Frame(frame) => {
                        if tx.send(Out::Line(encode_reply(&frame))).await.is_err() {
                            return;
                        }
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

async fn handle_line(line: &str, conn: &mut ConnState, shared: &Shared) -> Option<Reply> {
    let frame: ClientFrame = match serde_json::from_str(line) {
        Ok(frame) => frame,
        Err(e) => {
            // Answer under the frame's id when it has one, so the client is not left waiting.
            let id = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("id")?.as_u64())
                .unwrap_or(0);
            return Some(Reply::Frame(ServerFrame::Error {
                id,
                error: HostlerError {
                    code: ErrorCode::BadRequest,
                    message: format!("unreadable request: {e}"),
                    details: json!({}),
                    recovery: None,
                },
            }));
        }
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
                Ok(_) => Reply::Stop(response(id, Ok(json!({ "stopping": true })))),
                Err(e) => Reply::Frame(response::<Value>(id, Err(e))),
            });
        }
        _ if !conn.hello_ok => {
            response::<Value>(id, Err(lifecycle::hello_required(shared.contract_version)))
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
    }
}

#[cfg(windows)]
mod windows_sd {
    //! A named-pipe security descriptor whose DACL grants access to the current user only.

    use std::ffi::c_void;

    use interprocess::os::windows::security_descriptor::{
        AsSecurityDescriptorExt, BorrowedSecurityDescriptor, SecurityDescriptor,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub fn current_user_only() -> anyhow::Result<SecurityDescriptor> {
        let sid = current_user_sid()?;
        // P = protected (no inherited ACEs); GA = generic all, for this SID only.
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string and a writable out-pointer; the result is checked.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            anyhow::bail!(
                "cannot build the pipe security descriptor: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: `sd` is a valid self-relative descriptor until the LocalFree below.
        let owned = unsafe { BorrowedSecurityDescriptor::from_ptr(sd.cast_const()) }.to_owned_sd();
        // SAFETY: `sd` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(sd) };
        Ok(owned?)
    }

    fn current_user_sid() -> anyhow::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle of the current process and a writable out-handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            anyhow::bail!(
                "cannot open the process token: {}",
                std::io::Error::last_os_error()
            );
        }
        let mut needed = 0u32;
        // SAFETY: a size query with a null buffer; it fails with ERROR_INSUFFICIENT_BUFFER.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        // u64 storage keeps TOKEN_USER (which holds pointers) aligned.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: `buf` is writable for `needed` bytes.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                needed,
                &mut needed,
            )
        };
        // SAFETY: `token` was opened above and is closed once.
        unsafe { CloseHandle(token) };
        if ok == 0 {
            anyhow::bail!(
                "cannot read the token user: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` points into `buf`, which is alive; `text` receives a LocalAlloc'd string.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            anyhow::bail!(
                "cannot format the user SID: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: `text` is a NUL-terminated UTF-16 string allocated by the call above.
        let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        // SAFETY: `len` u16s before the terminator are initialized.
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        // SAFETY: allocated by ConvertSidToStringSidW.
        unsafe { LocalFree(text.cast()) };
        Ok(sid)
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
}
