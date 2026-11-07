//! The shared client of the hostler service (plan Task 6). It reports failures as
//! `HostlerError`s; the policy of what to do about them (e.g. heavy fail-open) lives in callers.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::{Stream, prelude::*};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};
use zeroize::Zeroizing;

use crate::contract::wire::{ClientFrame, Event, Request, ServerFrame};
use crate::contract::{CONTRACT_VERSION, ErrorCode, HostlerError};
use crate::paths::{Paths, SocketName};
use crate::service::ServiceManager;
use crate::service::server::{MAX_FRAME_BYTES, socket_name};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// `secrets.unlock`: the KDF of opening (or creating) the database, maybe calibrated to seconds.
pub const UNLOCK_TIMEOUT: Duration = Duration::from_secs(120);
/// Methods that may open, re-read or save the KDBX (one KDF each).
pub const VAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The whole start-and-retry window of `connect_or_start`, every attempt included.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const START_RETRY_EVERY: Duration = Duration::from_millis(100);
/// Frames read ahead of the caller. While it is full the service holds back pushed snapshots
/// (keeping only the newest) but still reads our requests.
const FRAME_BUFFER: usize = 64;

/// What the background reader hands the client.
enum Incoming {
    Frame(ServerFrame),
    /// The raw answer line to a call registered as sensitive; never parsed into a `Value`.
    Sensitive(u64, Zeroizing<String>),
}

type Item = Result<Incoming, HostlerError>;

/// Ids of every sensitive call made on this connection. They are never removed: a late or a
/// duplicated answer is still routed as raw and erased, never parsed into a `Value`.
type SensitiveIds = Arc<Mutex<HashSet<u64>>>;

/// `sensitive_answer` for a typed request: these answers carry values.
fn answer_is_sensitive(req: &Request) -> bool {
    matches!(req, Request::SecretMaterialize(_))
}

/// How long a call may wait for its answer.
pub fn call_bound(req: &Request) -> Duration {
    match req {
        Request::SecretsUnlock(_) => UNLOCK_TIMEOUT,
        Request::SecretList { .. }
        | Request::SecretSet(_)
        | Request::SecretRemove { .. }
        | Request::SecretImport(_)
        | Request::SecretRequest(_)
        | Request::SecretShare(_)
        | Request::SecretMaterialize(_) => VAULT_CALL_TIMEOUT,
        _ => CALL_TIMEOUT,
    }
}

pub struct Client {
    writer: FramedWrite<WriteHalf<Stream>, LinesCodec>,
    /// Filled by a background reader that keeps draining the socket while we write, so a large
    /// request and a large pushed snapshot never wait on each other.
    frames: mpsc::Receiver<Item>,
    reader: JoinHandle<()>,
    sensitive: SensitiveIds,
    /// The first transport error; every later read reports it again.
    broken: Option<HostlerError>,
    next_id: u64,
    /// Events are whole queue snapshots: only the newest unread one is kept.
    event: Option<Event>,
    socket: SocketName,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// `ServiceUnavailable` with a machine-readable `details.reason`.
pub fn unavailable(reason: &str, message: String, socket: &SocketName) -> HostlerError {
    HostlerError {
        code: ErrorCode::ServiceUnavailable,
        message,
        details: json!({ "reason": reason, "socket": socket_display(socket) }),
        recovery: Some("hostler service run".into()),
    }
}

pub fn socket_display(socket: &SocketName) -> String {
    match socket {
        SocketName::File(path) => path.display().to_string(),
        SocketName::Namespaced(name) => name.clone(),
    }
}

/// How the reader routes a line.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// An answer to a sensitive call: forwarded raw, never parsed into a frame.
    Sensitive(u64),
    /// An event, or an answer to an ordinary call.
    Frame,
    /// After a sensitive call, a line that is neither of the above: truncated, an answer
    /// (`result`/`error`) without a numeric id (missing, `null`, a string, duplicated), or no
    /// envelope at all. It may hold values, so it is never handed to the frame parser.
    Unreadable,
}

/// Whether a key is in the envelope at all — `null` included; its payload is skipped unread.
#[derive(Default)]
struct Present(bool);

impl<'de> Deserialize<'de> for Present {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(d).map(|_| Present(true))
    }
}

/// Routes by the envelope's shape and id only; no payload is ever built.
fn route(ids: &SensitiveIds, line: &str) -> Route {
    #[derive(Deserialize)]
    struct Head {
        id: Option<u64>,
        #[serde(default)]
        result: Present,
        #[serde(default)]
        error: Present,
        #[serde(default)]
        event: Present,
    }
    let ids = ids.lock().unwrap_or_else(PoisonError::into_inner);
    if ids.is_empty() {
        return Route::Frame;
    }
    let Ok(head) = serde_json::from_str::<Head>(line) else {
        return Route::Unreadable;
    };
    match head.id {
        Some(id) if ids.contains(&id) => Route::Sensitive(id),
        // An answer to an ordinary call.
        Some(_) if head.result.0 || head.error.0 => Route::Frame,
        // An event: no id, no answer keys.
        None if head.event.0 && !head.result.0 && !head.error.0 => Route::Frame,
        _ => Route::Unreadable,
    }
}

async fn read_frames(
    mut reader: FramedRead<ReadHalf<Stream>, LinesCodec>,
    tx: mpsc::Sender<Item>,
    socket: SocketName,
    sensitive: SensitiveIds,
) {
    loop {
        let item = match reader.next().await {
            None => Err(unavailable(
                "closed",
                "the hostler service closed the connection".into(),
                &socket,
            )),
            Some(Err(e)) => Err(unavailable(
                "io",
                format!("reading from the service failed: {e}"),
                &socket,
            )),
            Some(Ok(line)) => {
                // Erased once routed: after a sensitive call any line may carry values.
                let line = Zeroizing::new(line);
                match route(&sensitive, &line) {
                    Route::Sensitive(id) => Ok(Incoming::Sensitive(id, line)),
                    Route::Unreadable => Err(unreadable_answer()),
                    Route::Frame => serde_json::from_str(&line)
                        .map(Incoming::Frame)
                        .map_err(|e| {
                            unavailable(
                                "protocol",
                                format!("unreadable frame from the service: {e}"),
                                &socket,
                            )
                        }),
                }
            }
        };
        let last = item.is_err();
        if tx.send(item).await.is_err() || last {
            return;
        }
    }
}

impl Client {
    /// Connects and says `hello`; never starts the service.
    pub async fn connect(paths: &Paths) -> Result<Client, HostlerError> {
        let mut client = Client::connect_lifecycle(paths).await?;
        let hello = Request::Hello {
            contract_version: CONTRACT_VERSION,
            client: format!("hostler {}", env!("CARGO_PKG_VERSION")),
        };
        client.call_within(hello, HELLO_TIMEOUT).await?;
        Ok(client)
    }

    /// Connects without `hello`: only lifecycle methods work, whatever the service's contract.
    pub async fn connect_lifecycle(paths: &Paths) -> Result<Client, HostlerError> {
        let socket = &paths.socket;
        #[cfg(unix)]
        check_socket_owner(socket)?;
        let name = socket_name(socket)
            .map_err(|e| unavailable("bad_socket_name", e.to_string(), socket))?;
        let stream = match timeout(CONNECT_TIMEOUT, Stream::connect(name)).await {
            Err(_) => {
                return Err(unavailable(
                    "timeout",
                    format!(
                        "connecting to the hostler service timed out after {CONNECT_TIMEOUT:?}"
                    ),
                    socket,
                ));
            }
            Ok(Err(e)) => {
                return Err(unavailable(
                    "not_running",
                    format!("the hostler service is not running ({e})"),
                    socket,
                ));
            }
            Ok(Ok(stream)) => stream,
        };
        let (recv, send) = tokio::io::split(stream);
        let (tx, frames) = mpsc::channel(FRAME_BUFFER);
        let sensitive = SensitiveIds::default();
        let reader = tokio::spawn(read_frames(
            FramedRead::new(recv, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            tx,
            socket.clone(),
            sensitive.clone(),
        ));
        Ok(Client {
            writer: FramedWrite::new(send, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            frames,
            reader,
            sensitive,
            broken: None,
            next_id: 1,
            event: None,
            socket: socket.clone(),
        })
    }

    /// `connect`; when the service is unavailable, starts it and retries until 5 s have passed
    /// in total — OS-manager calls included. With an OS registration for this home, only the OS
    /// manager may start it (a spawned service would inherit the caller's sandbox); without one,
    /// a detached `hostler service run` is spawned. A `ContractMismatch` is returned as is.
    pub async fn connect_or_start<M: ServiceManager + Clone + 'static>(
        paths: &Paths,
        manager: &M,
    ) -> Result<Client, HostlerError> {
        let exe = std::env::current_exe().map_err(|e| {
            unavailable(
                "no_executable",
                format!("cannot find the hostler executable: {e}"),
                &paths.socket,
            )
        })?;
        Client::connect_or_start_with(paths, manager, &exe).await
    }

    /// `connect_or_start` with an explicit hostler executable (tests run from another binary).
    #[doc(hidden)]
    pub async fn connect_or_start_with<M: ServiceManager + Clone + 'static>(
        paths: &Paths,
        manager: &M,
        exe: &Path,
    ) -> Result<Client, HostlerError> {
        match Client::connect(paths).await {
            Ok(client) => return Ok(client),
            Err(e) if e.code == ErrorCode::ContractMismatch => return Err(e),
            Err(_) => {}
        }
        let deadline = Instant::now() + START_TIMEOUT;
        let registered = || async {
            if paths.custom_home {
                return Ok(false);
            }
            off_thread(manager, deadline, paths, |m| m.is_installed()).await
        };
        let start_managed = || async {
            off_thread(manager, deadline, paths, |m| m.start())
                .await?
                .map_err(|e| manager_start_failed(e, paths))
        };
        let mut via_manager = registered().await?;
        if via_manager {
            start_managed().await?;
        } else {
            spawn_service(exe, paths).map_err(|e| {
                unavailable(
                    "spawn_failed",
                    format!("cannot start the hostler service: {e}"),
                    &paths.socket,
                )
            })?;
        }
        let mut last = None;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            sleep(START_RETRY_EVERY.min(deadline - now)).await;
            match timeout_at(deadline, Client::connect(paths)).await {
                Ok(Ok(client)) => return Ok(client),
                Ok(Err(e)) if e.code == ErrorCode::ContractMismatch => return Err(e),
                Ok(Err(e)) => {
                    last = Some(e);
                    // A registration appeared after our check: the spawned service steps aside
                    // for it, so start the registered one (same deadline).
                    if !via_manager && registered().await? {
                        via_manager = true;
                        start_managed().await?;
                    }
                }
                Err(_) => break,
            }
        }
        let mut error = unavailable(
            "start_timeout",
            format!("the hostler service did not come up within {START_TIMEOUT:?}"),
            &paths.socket,
        );
        if via_manager {
            // A registration from before `--managed` starts a service that steps aside.
            error.message = format!(
                "the OS service manager started hostler, but no service answered within \
                 {START_TIMEOUT:?}; the OS registration may be from an older hostler"
            );
            error.recovery = Some("hostler service install".into());
        }
        if let Some(last) = last {
            error.details["last_error"] = Value::String(last.message);
        }
        Err(error)
    }

    /// Sends one request and waits for its answer within `call_bound`.
    /// `sensitive_answer` methods are refused without sending anything: their answers go
    /// through `call_sensitive` only.
    pub async fn call(&mut self, req: Request) -> Result<Value, HostlerError> {
        if answer_is_sensitive(&req) {
            return Err(wrong_call_path(
                "this method's answer carries secret values; use Client::call_sensitive",
            ));
        }
        let limit = call_bound(&req);
        self.call_within(req, limit).await
    }

    /// For `sensitive_answer` methods only (others are refused without sending anything): the
    /// answer line is decoded straight into `T`, never into a `Value`; an undecodable answer
    /// gets a fixed text (serde's quotes the input).
    pub async fn call_sensitive<T: DeserializeOwned>(
        &mut self,
        req: Request,
    ) -> Result<T, HostlerError> {
        if !answer_is_sensitive(&req) {
            return Err(wrong_call_path(
                "this method's answer carries no secret values; use Client::call",
            ));
        }
        let limit = call_bound(&req);
        match self.round_trip(req, limit, true).await? {
            Answer::Raw(line) => decode_sensitive(&line),
            Answer::Value(_) => Err(unreadable_answer()),
        }
    }

    async fn call_within(&mut self, req: Request, limit: Duration) -> Result<Value, HostlerError> {
        match self.round_trip(req, limit, false).await? {
            Answer::Value(value) => Ok(value),
            Answer::Raw(_) => Err(unreadable_answer()),
        }
    }

    async fn round_trip(
        &mut self,
        req: Request,
        limit: Duration,
        sensitive: bool,
    ) -> Result<Answer, HostlerError> {
        let id = self.next_id;
        self.next_id += 1;
        // The frame may carry a value (unlock, set, import): erased once sent.
        let frame = serde_json::to_string(&ClientFrame { id, request: req })
            .map(Zeroizing::new)
            .map_err(|e| bad_request(format!("cannot encode the request: {e}")))?;
        if frame.len() > MAX_FRAME_BYTES {
            return Err(bad_request(format!(
                "the request is {} bytes, over the {MAX_FRAME_BYTES}-byte frame limit",
                frame.len()
            )));
        }
        if sensitive {
            self.sensitive
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id);
        }
        match timeout(limit, self.exchange(id, &frame)).await {
            Ok(result) => result,
            Err(_) => Err(unavailable(
                "timeout",
                format!("the hostler service did not answer within {limit:?}"),
                &self.socket,
            )),
        }
    }

    async fn exchange(&mut self, id: u64, frame: &str) -> Result<Answer, HostlerError> {
        if let Err(e) = self.writer.send(frame).await {
            return Err(unavailable(
                "io",
                format!("cannot reach the service: {e}"),
                &self.socket,
            ));
        }
        loop {
            match self.read_frame().await? {
                Incoming::Frame(ServerFrame::Response { id: got, result }) if got == id => {
                    return Ok(Answer::Value(result));
                }
                Incoming::Frame(ServerFrame::Error { id: got, error }) if got == id => {
                    return Err(error);
                }
                Incoming::Sensitive(got, line) if got == id => return Ok(Answer::Raw(line)),
                Incoming::Frame(ServerFrame::Event { event }) => self.event = Some(event),
                // A late answer to a call that timed out earlier (a sensitive one is erased).
                _ => {}
            }
        }
    }

    async fn read_frame(&mut self) -> Result<Incoming, HostlerError> {
        if let Some(e) = &self.broken {
            return Err(e.clone());
        }
        let error = match self.frames.recv().await {
            Some(Ok(frame)) => return Ok(frame),
            Some(Err(e)) => e,
            None => unavailable(
                "closed",
                "the hostler service closed the connection".into(),
                &self.socket,
            ),
        };
        self.broken = Some(error.clone());
        Err(error)
    }

    /// The newest pushed event not yet returned; `None` once the connection is closed.
    /// Cancel-safe.
    pub async fn next_event(&mut self) -> Option<Event> {
        if let Some(event) = self.event.take() {
            return Some(event);
        }
        loop {
            match self.read_frame().await {
                Ok(Incoming::Frame(ServerFrame::Event { event })) => return Some(event),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
}

/// Runs a (blocking) OS-manager call on its own thread, bounded by `deadline`. A plain thread,
/// not `spawn_blocking`: a call that never returns must not keep the process from exiting.
async fn off_thread<M, T>(
    manager: &M,
    deadline: Instant,
    paths: &Paths,
    call: impl FnOnce(&M) -> T + Send + 'static,
) -> Result<T, HostlerError>
where
    M: ServiceManager + Clone + 'static,
    T: Send + 'static,
{
    let manager = manager.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(call(&manager));
    });
    match timeout_at(deadline, rx).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(unavailable(
            "manager_failed",
            "the OS service manager check ended without an answer".into(),
            &paths.socket,
        )),
        Err(_) => {
            let mut error = unavailable(
                "manager_timeout",
                format!("the OS service manager did not answer within {START_TIMEOUT:?}"),
                &paths.socket,
            );
            error.recovery = Some("hostler service status".into());
            Err(error)
        }
    }
}

fn manager_start_failed(e: anyhow::Error, paths: &Paths) -> HostlerError {
    let mut error = unavailable(
        "manager_start_failed",
        format!("the OS service manager could not start the hostler service: {e:#}"),
        &paths.socket,
    );
    error.recovery = Some("hostler service status".into());
    error
}

/// A call's answer: parsed, or the raw line of a sensitive one.
enum Answer {
    Value(Value),
    Raw(Zeroizing<String>),
}

/// `{"id":…,"result":T}` or `{"id":…,"error":…}`, without ever building a `Value`.
fn decode_sensitive<T: DeserializeOwned>(line: &str) -> Result<T, HostlerError> {
    #[derive(Deserialize)]
    struct Frame<T> {
        result: Option<T>,
        error: Option<HostlerError>,
    }
    match serde_json::from_str::<Frame<T>>(line) {
        Ok(Frame {
            result: Some(result),
            error: None,
        }) => Ok(result),
        Ok(Frame {
            result: None,
            error: Some(error),
        }) => Err(error),
        _ => Err(unreadable_answer()),
    }
}

fn wrong_call_path(message: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: message.into(),
        details: json!({}),
        recovery: None,
    }
}

fn unreadable_answer() -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: "unreadable answer from the service".into(),
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

/// A socket file owned by someone else is never trusted.
#[cfg(unix)]
fn check_socket_owner(socket: &SocketName) -> Result<(), HostlerError> {
    use std::os::unix::fs::MetadataExt;
    let SocketName::File(path) = socket else {
        return Ok(());
    };
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        unavailable(
            "not_running",
            format!(
                "the hostler service is not running ({}: {e})",
                path.display()
            ),
            socket,
        )
    })?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(unavailable(
            "foreign_owner",
            format!(
                "{} belongs to uid {}, not to the current user (uid {me}); refusing to use it",
                path.display(),
                meta.uid()
            ),
            socket,
        ));
    }
    Ok(())
}

/// Starts `exe service run` detached from this process and its terminal, holding none of our
/// handles or fds: a caller reading our output must see EOF as soon as we exit, not when the
/// long-lived service does.
#[cfg(unix)]
fn spawn_service(exe: &Path, paths: &Paths) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut command = Command::new(exe);
    command
        .args(["service", "run"])
        .env("HOSTLER_HOME", &paths.home)
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Read before the fork: the child may only make async-signal-safe calls.
    let fd_limit = fd_scan_limit();
    let open = open_fds();
    // SAFETY: between fork and exec only async-signal-safe calls run (setsid, close_range,
    // fcntl) over data prepared before the fork; nothing allocates.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            cloexec_from(3, fd_limit, &open);
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // Reap it if it exits while we live (e.g. it lost the race for the lock).
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The fds open in this process. A soft RLIMIT_NOFILE says nothing about fds opened before it
/// was lowered, so it is not enough to bound the scan. Empty if they cannot be listed.
#[cfg(target_os = "macos")]
fn open_fds() -> Vec<libc::c_int> {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    // SAFETY: getpid has no preconditions.
    let pid = unsafe { libc::getpid() };
    let mut capacity = 256;
    loop {
        let mut buf: Vec<libc::proc_fdinfo> = Vec::with_capacity(capacity);
        let bytes = (capacity * entry) as libc::c_int;
        // SAFETY: `buf` has room for `capacity` entries, which `bytes` tells the kernel.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                buf.as_mut_ptr().cast(),
                bytes,
            )
        };
        if written <= 0 {
            return Vec::new();
        }
        let n = written as usize / entry;
        // A full buffer may be truncated: retry larger.
        if n >= capacity && capacity < (1 << 20) {
            capacity *= 4;
            continue;
        }
        // SAFETY: the kernel initialised the first `n` entries.
        unsafe { buf.set_len(n.min(capacity)) };
        return buf.iter().map(|info| info.proc_fd).collect();
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_fds() -> Vec<libc::c_int> {
    std::fs::read_dir("/proc/self/fd")
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Fds to scan when `close_range` is unavailable: the soft RLIMIT_NOFILE, capped. Covers fds
/// opened (by another thread) between `open_fds` and the fork.
#[cfg(unix)]
fn fd_scan_limit() -> libc::c_int {
    const CAP: libc::rlim_t = 65_536;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid, writable rlimit.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
    let soft = if read && limit.rlim_cur != libc::RLIM_INFINITY {
        limit.rlim_cur.min(CAP)
    } else {
        CAP
    };
    soft as libc::c_int
}

/// Marks every fd from `first` on close-on-exec, so the service keeps only 0–2 — also fds a
/// caller passed us without CLOEXEC, however high. Marked rather than closed: std's own
/// exec-error pipe must stay open until the exec.
///
/// # Safety
/// Call only between fork and exec (it touches every fd of the process).
#[cfg(unix)]
unsafe fn cloexec_from(first: libc::c_int, limit: libc::c_int, open: &[libc::c_int]) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: close_range only changes fd flags; a kernel without it returns ENOSYS.
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                first as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0;
        if marked {
            return;
        }
    }
    let listed = open.iter().copied().filter(|&fd| fd >= limit);
    for fd in (first..limit).chain(listed) {
        // SAFETY: fcntl on a closed fd fails harmlessly with EBADF.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Windows: `CreateProcessW` with no handle inheritance at all (std's spawn always passes
/// bInheritHandles = TRUE, which leaks every inheritable handle, e.g. a caller's stdout pipe)
/// and null std handles; the service logs to its file.
#[cfg(windows)]
fn spawn_service(exe: &Path, paths: &Paths) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    let exe: Vec<u16> = exe.as_os_str().encode_wide().collect();
    let mut command_line = quote_windows_arg(&exe);
    command_line.extend(" service run".encode_utf16());
    command_line.push(0);
    let vars = std::env::vars_os()
        .map(|(k, v)| (k.encode_wide().collect(), v.encode_wide().collect()))
        .collect();
    let home: Vec<u16> = paths.home.as_os_str().encode_wide().collect();
    let environment = environment_block(vars, &home);
    let cwd: Vec<u16> = home.iter().copied().chain(std::iter::once(0)).collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        // With null handles: the service gets no std handles at all.
        dwFlags: STARTF_USESTDHANDLES,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    let base =
        DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
    // Break away from the caller's job first: a kill-on-close job (an OpenSSH session, some IDE
    // terminals) would otherwise end the service — and an unlocked secrets database — with the
    // session (W-5). A job that forbids breakaway answers ACCESS_DENIED: start inside it then.
    let mut created = false;
    for flags in [base | CREATE_BREAKAWAY_FROM_JOB, base] {
        // SAFETY: NUL-terminated, writable command line; NUL-terminated cwd; a double-NUL
        // terminated UTF-16 environment block (CREATE_UNICODE_ENVIRONMENT); valid in/out
        // structs.
        created = unsafe {
            CreateProcessW(
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                &startup,
                &mut info,
            )
        } != 0;
        if created {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) || flags == base {
            return Err(error);
        }
    }
    if !created {
        return Err(std::io::Error::other(
            "the hostler service could not be started",
        ));
    }
    // SAFETY: both handles were returned open by CreateProcessW and are closed once; closing
    // them does not affect the detached process.
    unsafe {
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
    }
    Ok(())
}

/// One argument quoted for `CommandLineToArgvW` / the MSVC runtime: always in quotes, with
/// backslashes doubled only where they precede a quote.
#[cfg(any(windows, test))]
fn quote_windows_arg(arg: &[u16]) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    const QUOTE: u16 = b'"' as u16;
    let mut out = vec![QUOTE];
    let mut backslashes = 0;
    for &c in arg {
        match c {
            BACKSLASH => backslashes += 1,
            QUOTE => {
                out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
                out.push(QUOTE);
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    out.push(QUOTE);
    out
}

/// A `CREATE_UNICODE_ENVIRONMENT` block: the given variables with `HOSTLER_HOME` set to `home`
/// (names compare case-insensitively, as Windows does), sorted, each `NAME=value\0`, ending in an
/// extra `\0`.
#[cfg(any(windows, test))]
fn environment_block(vars: Vec<(Vec<u16>, Vec<u16>)>, home: &[u16]) -> Vec<u16> {
    let upper = |name: &[u16]| String::from_utf16_lossy(name).to_uppercase();
    let hostler_home: Vec<u16> = "HOSTLER_HOME".encode_utf16().collect();
    let mut vars: Vec<(Vec<u16>, Vec<u16>)> = vars
        .into_iter()
        .filter(|(name, _)| upper(name) != "HOSTLER_HOME")
        .collect();
    vars.push((hostler_home, home.to_vec()));
    vars.sort_by_key(|(name, _)| upper(name));
    let mut block = Vec::new();
    for (name, value) in vars {
        block.extend(name);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn windows_args_are_quoted_for_the_msvc_runtime() {
        let cases = [
            (
                r"C:\Program Files\hostler\hostler.exe",
                r#""C:\Program Files\hostler\hostler.exe""#,
            ),
            (r"C:\dir\", r#""C:\dir\\""#),
            (r#"a"b"#, r#""a\"b""#),
            (r#"a\"b"#, r#""a\\\"b""#),
            ("", r#""""#),
        ];
        for (arg, quoted) in cases {
            assert_eq!(
                String::from_utf16(&quote_windows_arg(&wide(arg))).unwrap(),
                quoted,
                "{arg}"
            );
        }
    }

    #[test]
    fn environment_block_overrides_hostler_home_and_is_double_terminated() {
        let vars = vec![
            (wide("Path"), wide(r"C:\bin")),
            (wide("hostler_home"), wide(r"C:\old")),
            (wide("APPDATA"), wide(r"C:\Users\a")),
        ];
        let block = environment_block(vars, &wide(r"C:\new home"));
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(
            text,
            "APPDATA=C:\\Users\\a\0HOSTLER_HOME=C:\\new home\0Path=C:\\bin\0\0"
        );
        assert_eq!(
            environment_block(Vec::new(), &wide("h")),
            wide("HOSTLER_HOME=h\0\0")
        );
    }

    fn project() -> crate::contract::secrets::ProjectRef {
        crate::contract::secrets::ProjectRef {
            common_dir: "/work/app/.git".into(),
            name: "app".into(),
        }
    }

    #[test]
    fn call_bound_per_method() {
        use crate::contract::secrets::{SecretSet, Unlock};
        use crate::secrets::value::SecretValue;
        let unlock = Request::SecretsUnlock(Unlock {
            password: SecretValue::new("pw-12345".into()),
            key_file: None,
            forget_key_file: false,
            create: false,
        });
        assert_eq!(call_bound(&unlock), Duration::from_secs(120));
        let set = Request::SecretSet(SecretSet {
            project: project(),
            name: "API_KEY".into(),
            value: SecretValue::new("value-1".into()),
        });
        assert_eq!(call_bound(&set), Duration::from_secs(60));
        let list = Request::SecretList {
            project: project(),
            declared: Vec::new(),
        };
        assert_eq!(call_bound(&list), VAULT_CALL_TIMEOUT);
        assert_eq!(call_bound(&Request::SecretRequests), Duration::from_secs(5));
        assert_eq!(call_bound(&Request::QueueSnapshot), CALL_TIMEOUT);
        assert_eq!(call_bound(&Request::SecretsStatus), CALL_TIMEOUT);
    }

    /// A one-connection service stand-in: answers `hello`, then, for each request it reads,
    /// waits the given time and writes the next answer (which may hold several lines). Returns
    /// the methods of the requests it read after `hello`.
    async fn fake_service(
        listener: interprocess::local_socket::tokio::Listener,
        answers: Vec<(Duration, String)>,
    ) -> Vec<String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let stream = listener.accept().await.unwrap();
        let (recv, mut send) = tokio::io::split(stream);
        let mut lines = BufReader::new(recv).lines();
        let hello = lines.next_line().await.unwrap().unwrap();
        assert!(hello.contains("\"hello\""), "{hello}");
        send.write_all(
            b"{\"id\":1,\"result\":{\"contract_version\":2,\"service_version\":\"t\"}}\n",
        )
        .await
        .unwrap();
        let mut seen = Vec::new();
        for (delay, answer) in answers {
            let Ok(Some(request)) = lines.next_line().await else {
                return seen;
            };
            let request: Value = serde_json::from_str(&request).unwrap();
            seen.push(request["method"].as_str().unwrap().to_owned());
            tokio::time::sleep(delay).await;
            if send.write_all(answer.as_bytes()).await.is_err()
                || send.write_all(b"\n").await.is_err()
            {
                return seen;
            }
        }
        // Keep the connection open until the client goes away.
        let _ = lines.next_line().await;
        seen
    }

    fn materialize() -> Request {
        use crate::contract::secrets::Materialize;
        use crate::secrets::value::RawCommand;
        Request::SecretMaterialize(Materialize {
            run_id: "run-1".into(),
            project: project(),
            names: vec!["API_KEY".into()],
            command: RawCommand::new("make".into()),
            workspace: "/work/app".into(),
        })
    }

    const GOOD: &str = r#"{"id":2,"result":[{"name":"API_KEY","value":"hunter2-canary"}]}"#;
    const SNAPSHOT: &str = r#"{"capacity":1,"running":[],"waiting":[],"foreign_running":[]}"#;

    /// A fake service with `answers` and a client connected to it.
    async fn connected(
        answers: Vec<(Duration, String)>,
    ) -> (
        Client,
        tokio::task::JoinHandle<Vec<String>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().join("h"), true);
        paths.ensure_dirs().unwrap();
        let listener = crate::service::server::bind(&paths.socket).unwrap();
        let service = tokio::spawn(fake_service(listener, answers));
        let client = Client::connect(&paths).await.unwrap();
        (client, service, dir)
    }

    fn now(answer: &str) -> (Duration, String) {
        (Duration::ZERO, answer.to_owned())
    }

    #[tokio::test]
    async fn call_sensitive_decodes_without_value_and_hides_decode_errors() {
        use crate::contract::secrets::MaterializedSecret;

        let (mut client, service, _dir) = connected(vec![
            // A pushed event between the request and its answer.
            now(&(r#"{"event":{"type":"secrets.lock_changed","data":{"state":"locked","database":"d","key_file":null,"pending_requests":0,"preserved_files":[]}}}"#.to_owned()
                + "\n"
                + GOOD)),
            now(r#"{"id":3,"result":[{"name":"API_KEY","value":12,"x":"hunter2-canary"}]}"#),
            now(r#"{"id":4,"error":{"code":"secret_missing","message":"secret API_KEY is not set for project app","details":{},"recovery":null}}"#),
        ])
        .await;

        let got: Vec<MaterializedSecret> = client.call_sensitive(materialize()).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "API_KEY");
        assert!(got[0].value.expose() == "hunter2-canary");
        assert!(!format!("{got:?}").contains("hunter2"));
        assert!(matches!(
            client.next_event().await,
            Some(Event::SecretsLockChanged(_))
        ));

        let bad = client
            .call_sensitive::<Vec<MaterializedSecret>>(materialize())
            .await
            .unwrap_err();
        assert_eq!(bad.code, ErrorCode::Internal);
        assert_eq!(bad.message, "unreadable answer from the service");
        assert!(!format!("{bad:?}").contains("hunter2"));

        let missing = client
            .call_sensitive::<Vec<MaterializedSecret>>(materialize())
            .await
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::SecretMissing);

        drop(client);
        service.await.unwrap();
    }

    /// T1-2: a line the reader cannot route after a sensitive call fails closed with the fixed
    /// text; it never reaches the frame parser, whose messages quote the input.
    #[tokio::test]
    async fn unroutable_sensitive_answers_fail_closed() {
        use crate::contract::secrets::MaterializedSecret;

        let unroutable = [
            // Truncated: the outer object is not closed.
            r#"{"id":2,"result":[{"name":"API_KEY","value":"hunter2-canary"}]"#,
            // An id that is not an unsigned integer.
            r#"{"id":"2","result":[{"name":"API_KEY","value":"hunter2-canary"}]}"#,
            r#"{"id":-2,"result":[{"name":"API_KEY","value":"hunter2-canary"}]}"#,
            // A duplicated id field.
            r#"{"id":2,"id":2,"result":[{"name":"API_KEY","value":"hunter2-canary"}]}"#,
            // Answers without a usable id: missing, or null after the payload.
            r#"{"result":[{"name":"API_KEY","value":"hunter2-canary"}]}"#,
            r#"{"result":[{"name":"API_KEY","value":"hunter2-canary"}],"id":null}"#,
            r#"{"error":{"code":"internal","message":"hunter2-canary","details":{},"recovery":null}}"#,
            // No envelope at all.
            r#"{"value":"hunter2-canary"}"#,
        ];
        // Every case is checked; failures name case numbers only (the lines hold a canary).
        let mut failed = Vec::new();
        for (case, line) in unroutable.into_iter().enumerate() {
            let (mut client, service, _dir) = connected(vec![now(line)]).await;
            let first = client
                .call_sensitive::<Vec<MaterializedSecret>>(materialize())
                .await;
            // The connection stays broken with the same fixed error.
            let again = client.call(Request::QueueSnapshot).await;
            let fixed = |e: Option<&HostlerError>| {
                e.is_some_and(|e| {
                    e.code == ErrorCode::Internal
                        && e.message == "unreadable answer from the service"
                        && !format!("{e:?}").contains("hunter2")
                })
            };
            if !fixed(first.as_ref().err()) || !fixed(again.as_ref().err()) {
                failed.push(case);
            }
            drop(client);
            service.await.unwrap();
        }
        assert!(failed.is_empty(), "cases not failing closed: {failed:?}");
    }

    /// T1-2: a second answer under a sensitive id and a late answer to a timed-out sensitive
    /// call are routed raw and dropped, never parsed as a frame; later calls still work.
    #[tokio::test]
    async fn duplicate_and_late_sensitive_answers_are_dropped() {
        use crate::contract::secrets::MaterializedSecret;

        let (mut client, service, _dir) = connected(vec![
            // id 2 times out; its answer arrives late, together with a duplicate that the
            // frame parser would refuse (no result, error or event).
            (
                Duration::from_millis(300),
                format!("{GOOD}\n{{\"id\":2,\"x\":\"hunter2-canary\"}}"),
            ),
            now(&GOOD.replace("\"id\":2", "\"id\":3")),
            now(&format!("{{\"id\":4,\"result\":{SNAPSHOT}}}")),
        ])
        .await;

        let timed_out = client
            .round_trip(materialize(), Duration::from_millis(100), true)
            .await;
        assert!(timed_out.is_err());
        let got: Vec<MaterializedSecret> = client.call_sensitive(materialize()).await.unwrap();
        assert!(got[0].value.expose() == "hunter2-canary");
        assert!(!format!("{got:?}").contains("hunter2"));
        let snapshot = client.call(Request::QueueSnapshot).await.unwrap();
        assert_eq!(snapshot["capacity"], 1);
        drop(client);
        service.await.unwrap();
    }

    /// T1-1: the Value-returning path refuses materialize before sending anything, and the
    /// typed path refuses every other method.
    #[tokio::test]
    async fn materialize_never_goes_through_call() {
        let (mut client, service, _dir) =
            connected(vec![now(&format!("{{\"id\":2,\"result\":{SNAPSHOT}}}"))]).await;

        let e = client.call(materialize()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal);
        assert!(e.message.contains("call_sensitive"), "{}", e.message);
        let e = client
            .call_sensitive::<Value>(Request::QueueSnapshot)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal);
        assert!(client.call(Request::QueueSnapshot).await.is_ok());
        drop(client);
        // Only the snapshot reached the service: both refusals sent nothing.
        assert_eq!(service.await.unwrap(), ["queue.snapshot"]);
    }

    #[test]
    fn answer_is_sensitive_matches_the_classifier() {
        use crate::contract::secrets::{SecretSet, Unlock};
        use crate::contract::wire::sensitive_answer;
        use crate::secrets::value::SecretValue;

        let requests = [
            materialize(),
            Request::SecretSet(SecretSet {
                project: project(),
                name: "API_KEY".into(),
                value: SecretValue::new("value-1".into()),
            }),
            Request::SecretsUnlock(Unlock {
                password: SecretValue::new("pw-12345".into()),
                key_file: None,
                forget_key_file: false,
                create: false,
            }),
            Request::SecretRequests,
            Request::SecretsStatus,
            Request::QueueSnapshot,
            Request::ServiceStatus,
        ];
        for request in requests {
            let method = serde_json::to_value(&request).unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                answer_is_sensitive(&request),
                sensitive_answer(&method),
                "{method}"
            );
        }
    }
}
