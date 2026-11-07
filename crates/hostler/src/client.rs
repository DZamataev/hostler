//! The shared client of the hostler service (plan Task 6). It reports failures as
//! `HostlerError`s; the policy of what to do about them (e.g. heavy fail-open) lives in callers.

use std::path::Path;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::{Stream, prelude::*};
use serde_json::{Value, json};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

use crate::contract::wire::{ClientFrame, Event, Request, ServerFrame};
use crate::contract::{CONTRACT_VERSION, ErrorCode, HostlerError};
use crate::paths::{Paths, SocketName};
use crate::service::ServiceManager;
use crate::service::server::{MAX_FRAME_BYTES, socket_name};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// The whole start-and-retry window of `connect_or_start`, every attempt included.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const START_RETRY_EVERY: Duration = Duration::from_millis(100);
/// Frames read ahead of the caller. While it is full the service holds back pushed snapshots
/// (keeping only the newest) but still reads our requests.
const FRAME_BUFFER: usize = 64;

type Incoming = Result<ServerFrame, HostlerError>;

pub struct Client {
    writer: FramedWrite<WriteHalf<Stream>, LinesCodec>,
    /// Filled by a background reader that keeps draining the socket while we write, so a large
    /// request and a large pushed snapshot never wait on each other.
    frames: mpsc::Receiver<Incoming>,
    reader: JoinHandle<()>,
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

async fn read_frames(
    mut reader: FramedRead<ReadHalf<Stream>, LinesCodec>,
    tx: mpsc::Sender<Incoming>,
    socket: SocketName,
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
            Some(Ok(line)) => serde_json::from_str(&line).map_err(|e| {
                unavailable(
                    "protocol",
                    format!("unreadable frame from the service: {e}"),
                    &socket,
                )
            }),
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
        let reader = tokio::spawn(read_frames(
            FramedRead::new(recv, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            tx,
            socket.clone(),
        ));
        Ok(Client {
            writer: FramedWrite::new(send, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            frames,
            reader,
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

    /// Sends one request and waits up to 5 s for its answer.
    pub async fn call(&mut self, req: Request) -> Result<Value, HostlerError> {
        self.call_within(req, CALL_TIMEOUT).await
    }

    async fn call_within(&mut self, req: Request, limit: Duration) -> Result<Value, HostlerError> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = serde_json::to_string(&ClientFrame { id, request: req })
            .map_err(|e| bad_request(format!("cannot encode the request: {e}")))?;
        if frame.len() > MAX_FRAME_BYTES {
            return Err(bad_request(format!(
                "the request is {} bytes, over the {MAX_FRAME_BYTES}-byte frame limit",
                frame.len()
            )));
        }
        match timeout(limit, self.exchange(id, frame)).await {
            Ok(result) => result,
            Err(_) => Err(unavailable(
                "timeout",
                format!("the hostler service did not answer within {limit:?}"),
                &self.socket,
            )),
        }
    }

    async fn exchange(&mut self, id: u64, frame: String) -> Result<Value, HostlerError> {
        if let Err(e) = self.writer.send(frame).await {
            return Err(unavailable(
                "io",
                format!("cannot reach the service: {e}"),
                &self.socket,
            ));
        }
        loop {
            match self.read_frame().await? {
                ServerFrame::Response { id: got, result } if got == id => return Ok(result),
                ServerFrame::Error { id: got, error } if got == id => return Err(error),
                ServerFrame::Event { event } => self.event = Some(event),
                // A late answer to a call that timed out earlier.
                _ => {}
            }
        }
    }

    async fn read_frame(&mut self) -> Result<ServerFrame, HostlerError> {
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
                Ok(ServerFrame::Event { event }) => return Some(event),
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
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DETACHED_PROCESS, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
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
    // SAFETY: NUL-terminated, writable command line; NUL-terminated cwd; a double-NUL
    // terminated UTF-16 environment block (CREATE_UNICODE_ENVIRONMENT); valid in/out structs.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS
                | CREATE_NO_WINDOW
                | CREATE_NEW_PROCESS_GROUP
                | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup,
            &mut info,
        )
    } != 0;
    if !created {
        return Err(std::io::Error::last_os_error());
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
}
