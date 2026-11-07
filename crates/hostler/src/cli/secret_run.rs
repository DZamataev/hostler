//! `hostler run --secret` / `--secret-file` with masked output (plan P2, Task 10).
//!
//! The values come from `secret.materialize` (`Client::call_sensitive`) and reach the command
//! as environment variables or private files; its stdout and stderr go through one `Pump` each,
//! which masks every value. Anything that goes wrong before the command runs exits 78.
//!
//! Debug-only test hooks read here: `HOSTLER_TEST_DRAIN_GRACE_MS=<ms>` (replaces
//! `DRAIN_GRACE`), `HOSTLER_TEST_PAUSE_AFTER_SEAL_MS=<ms>` (sleep after sealing, before `run`
//! returns) and `HOSTLER_TEST_PAUSE_AFTER_MATERIALIZE_MS=<ms>` (sleep once the values are
//! materialized and the files written).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{ChildStderr, ChildStdout};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::json;
use zeroize::Zeroizing;

use super::output::print_error;
use crate::client::Client;
use crate::contract::secrets::{Materialize, MaterializedSecret};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::mask::{MaskSet, Masker};
use crate::paths::Paths;
use crate::project::{self, Current, config::ProjectConfig};
use crate::secrets::rundir::{self, RunDir};
use crate::secrets::value::RawCommand;
use crate::secrets::{valid_env_name, valid_secret_name};
use crate::service::manager::OsManager;

/// Set (to `1`) in a secret run's command: a `hostler run` inside it may see inherited values
/// in its own command line, which it cannot mask (CR-1).
pub const SECRET_RUN_ENV: &str = "HOSTLER_SECRET_RUN";
/// What a run inside a secret run sends instead of its command, wherever it leaves the process.
pub const INSIDE_SECRET_RUN: &str = "<command inside a secret run>";

/// True inside a secret run's command (`SECRET_RUN_ENV` inherited).
pub fn inside_secret_run() -> bool {
    std::env::var_os(SECRET_RUN_ENV).is_some()
}

/// Refused before running: the command never started (D: exit codes).
pub const EXIT_REFUSED: i32 = 78;
const EXIT_USAGE: i32 = 2;
/// How long the output may still flow after the command exited (a background process may keep
/// the pipes open).
pub const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// A longer display is sent as a content-free placeholder (L13).
const MAX_RAW_COMMAND: usize = 16 * 1024;
const READ_BYTES: usize = 64 * 1024;
/// Masked buffers waiting for the writer; the reader waits (and so the command) beyond this.
const QUEUE_CAP: usize = 16;

/// `NAME[=ENV]` of `--secret` / `--secret-file`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretSpec {
    pub name: String,
    pub env: Option<String>,
}

/// `None` for anything else; the caller's message never quotes it: an agent may have passed a
/// value where a name belongs.
pub fn parse_spec(s: &str) -> Option<SecretSpec> {
    let (name, env) = match s.split_once('=') {
        Some((name, env)) => (name, Some(env)),
        None => (s, None),
    };
    if !valid_secret_name(name) || env.is_some_and(|env| !valid_env_name(env)) {
        return None;
    }
    Some(SecretSpec {
        name: name.to_owned(),
        env: env.map(str::to_owned),
    })
}

/// One secret for the command: in `env`, as the value or (`file`) as a file's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Injection {
    pub name: String,
    pub env: String,
    pub file: bool,
}

/// Env names: the spec's, else `env` in `hostler.toml`, else NAME. One variable set twice is
/// refused (names compare case-insensitively on Windows).
pub fn injections(
    secrets: &[SecretSpec],
    files: &[SecretSpec],
    config: Option<&ProjectConfig>,
) -> Result<Vec<Injection>, String> {
    let mut out: Vec<Injection> = Vec::new();
    let specs = (secrets.iter().map(|s| (s, false))).chain(files.iter().map(|s| (s, true)));
    for (spec, file) in specs {
        let env = spec
            .env
            .clone()
            .or_else(|| {
                config
                    .and_then(|c| c.env_for(&spec.name))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| spec.name.clone());
        if out.iter().any(|i| same_env(&i.env, &env)) {
            return Err(format!(
                "the environment variable {env} would be set twice; give one secret another \
                 name with NAME=ENV"
            ));
        }
        out.push(Injection {
            name: spec.name.clone(),
            env,
            file,
        });
    }
    Ok(out)
}

fn same_env(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// What the spawn paths give the command: extra environment and, for a secret run, the values
/// its output is masked with. The default is a P1 run: nothing added, output inherited.
#[derive(Default)]
pub struct ChildIo {
    pub env: Vec<(OsString, OsString)>,
    pub masks: Option<Arc<MaskSet>>,
}

impl ChildIo {
    /// `s` with every value masked; as is without values.
    pub fn mask(&self, s: &str) -> String {
        match &self.masks {
            Some(set) => String::from_utf8_lossy(&set.mask_all(s.as_bytes())).into_owned(),
            None => s.to_owned(),
        }
    }
}

pub struct Prepared {
    pub io: ChildIo,
    pub files: Option<RunDir>,
    /// Masked with the materialized values.
    pub display: String,
}

/// Everything before the spawn of a secret run: on failure the reason is printed and the exit
/// code returned (2 for usage, else 78).
pub async fn setup(
    secrets: &[String],
    files: &[String],
    run_id: &str,
    display: &str,
) -> Result<Prepared, i32> {
    let parse = |raw: &[String]| {
        raw.iter()
            .map(|s| parse_spec(s))
            .collect::<Option<Vec<_>>>()
    };
    let (Some(secrets), Some(files)) = (parse(secrets), parse(files)) else {
        eprintln!(
            "[hostler] --secret and --secret-file take NAME or NAME=ENV, where NAME matches \
             ^[A-Z][A-Z0-9_]{{0,63}}$ and ENV is an environment variable name (the argument is \
             not shown: it may be a value)"
        );
        return Err(EXIT_USAGE);
    };
    let refused = |e: HostlerError| {
        print_error(&e);
        EXIT_REFUSED
    };
    let cwd = std::env::current_dir().map_err(|e| refused(internal(format!("no cwd: {e}"))))?;
    let current = project::current(&cwd).map_err(refused)?;
    let injections = injections(&secrets, &files, current.config.as_ref()).map_err(|m| {
        eprintln!("[hostler] {m}");
        EXIT_USAGE
    })?;
    let paths = Paths::resolve().map_err(|e| refused(internal(format!("{e:#}"))))?;
    let mut client = Client::connect_or_start(&paths, &OsManager)
        .await
        .map_err(refused)?;
    prepare(&injections, &current, display, &paths, run_id, &mut client)
        .await
        .map_err(refused)
}

fn internal(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message,
        details: json!({}),
        recovery: None,
    }
}

/// Materializes the values (the raw display goes along for history, L13), masks the display,
/// writes the secret files and builds the command's environment.
pub async fn prepare(
    injections: &[Injection],
    current: &Current,
    display: &str,
    paths: &Paths,
    run_id: &str,
    client: &mut Client,
) -> Result<Prepared, HostlerError> {
    let mut names: Vec<String> = Vec::new();
    for i in injections {
        if !names.contains(&i.name) {
            names.push(i.name.clone());
        }
    }
    let command = if display.len() <= MAX_RAW_COMMAND {
        display.to_owned()
    } else {
        format!("<command of {} bytes>", display.len())
    };
    let request = Request::SecretMaterialize(Materialize {
        run_id: run_id.to_owned(),
        project: current.project.clone(),
        names,
        command: RawCommand::new(command),
        workspace: project::display_path(&current.checkout.workspace_root),
    });
    let values: Vec<MaterializedSecret> = client.call_sensitive(request).await?;
    let set = Arc::new(MaskSet::new(
        values.iter().map(|v| v.value.expose().as_bytes().to_vec()),
    ));
    let files = if injections.iter().any(|i| i.file) {
        rundir::sweep(&paths.runs);
        Some(RunDir::create(&paths.runs, run_id).map_err(|e| internal(format!("{e:#}")))?)
    } else {
        None
    };
    let mut written: Vec<(&str, PathBuf)> = Vec::new();
    // First: an injection of the same name cannot unset it.
    let mut env = vec![(OsString::from(SECRET_RUN_ENV), OsString::from("1"))];
    for i in injections {
        let Some(value) = values.iter().find(|v| v.name == i.name).map(|v| &v.value) else {
            return Err(internal(format!(
                "the service did not return the secret {}",
                i.name
            )));
        };
        let value: OsString = match (&files, i.file) {
            (Some(dir), true) => {
                let path = match written.iter().find(|(name, _)| *name == i.name) {
                    Some((_, path)) => path.clone(),
                    None => {
                        let path = dir
                            .write(&i.name, value)
                            .map_err(|e| internal(format!("{e:#}")))?;
                        written.push((&i.name, path.clone()));
                        path
                    }
                };
                path.into_os_string()
            }
            _ => value.expose().into(),
        };
        env.push((OsString::from(&i.env), value));
    }
    let io = ChildIo {
        env,
        masks: Some(set),
    };
    let display = io.mask(display);
    #[cfg(debug_assertions)]
    if let Some(ms) = test_ms("HOSTLER_TEST_PAUSE_AFTER_MATERIALIZE_MS") {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    Ok(Prepared { io, files, display })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutStream {
    Stdout,
    Stderr,
}

/// The pumps of a launched secret run; none without values.
pub fn start_pumps(
    io: &ChildIo,
    (stdout, stderr): (Option<ChildStdout>, Option<ChildStderr>),
) -> Vec<Pump> {
    let Some(set) = &io.masks else {
        return Vec::new();
    };
    let mut pumps = Vec::new();
    if let Some(stdout) = stdout {
        pumps.push(Pump::start(stdout, OutStream::Stdout, set.clone()));
    }
    if let Some(stderr) = stderr {
        pumps.push(Pump::start(stderr, OutStream::Stderr, set.clone()));
    }
    pumps
}

/// After the command exited at `exited`: every pump finishes by one shared deadline, the drain
/// grace after that exit.
pub async fn drain(pumps: Vec<Pump>, exited: Instant) {
    if pumps.is_empty() {
        return;
    }
    let deadline = exited + drain_grace();
    let _ = tokio::task::spawn_blocking(move || {
        for pump in pumps {
            pump.finish_by(deadline);
        }
        #[cfg(debug_assertions)]
        if let Some(ms) = test_ms("HOSTLER_TEST_PAUSE_AFTER_SEAL_MS") {
            std::thread::sleep(Duration::from_millis(ms));
        }
    })
    .await;
}

fn drain_grace() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = test_ms("HOSTLER_TEST_DRAIN_GRACE_MS") {
        return Duration::from_millis(ms);
    }
    DRAIN_GRACE
}

#[cfg(debug_assertions)]
fn test_ms(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// One output stream of the command: a reader thread masks what it reads into a bounded queue,
/// a writer thread is the only writer of the stream. The lock covers the masker and the queue,
/// never a read or a write (L14).
pub struct Pump {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    masker: Masker,
    queue: VecDeque<Vec<u8>>,
    /// The reader has seen EOF and queued the rest.
    read_all: bool,
    /// The writer has stopped: everything written, or a write failed.
    writer_done: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Pump {
    pub fn start(read: impl Read + Send + 'static, out: OutStream, set: Arc<MaskSet>) -> Pump {
        Pump::start_with(read, StdStream { out, ready: false }, set)
    }

    fn start_with(
        read: impl Read + Send + 'static,
        sink: impl Write + Send + 'static,
        set: Arc<MaskSet>,
    ) -> Pump {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                masker: Masker::new(set),
                queue: VecDeque::new(),
                read_all: false,
                writer_done: false,
            }),
            changed: Condvar::new(),
        });
        let reader = shared.clone();
        std::thread::spawn(move || read_loop(read, &reader));
        let writer = shared.clone();
        std::thread::spawn(move || write_loop(sink, &writer));
        Pump { shared }
    }

    /// Returns by `deadline` at the latest, whatever the downstream does: waits for the writer
    /// to have written everything after EOF; at the deadline seals (L7, L14) and returns.
    pub fn finish_by(self, deadline: Instant) {
        let mut state = self.shared.lock();
        while !state.writer_done {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        // Done or not: nothing read from now on reaches the stream. A held prefix becomes
        // `***`, queued behind what is already masked; a blocked writer may never write it.
        let mut out = Vec::new();
        state.masker.seal(&mut out);
        if !out.is_empty() {
            state.queue.push_back(out);
        }
        self.shared.changed.notify_all();
    }
}

fn read_loop(mut read: impl Read, shared: &Shared) {
    let mut buf = Zeroizing::new(vec![0u8; READ_BYTES]);
    loop {
        // `None`: a read error. Only a real EOF (`Some(0)`) may flush a held prefix verbatim
        // (L7); an error seals.
        let got = match read.read(&mut buf) {
            Ok(n) => Some(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => None,
        };
        let mut state = shared.lock();
        while got.is_some_and(|n| n > 0)
            && state.queue.len() >= QUEUE_CAP
            && !state.masker.is_sealed()
            && !state.writer_done
        {
            state = shared.wait(state);
        }
        // Sealed or nobody writes: discard, and drop the read end (the command sees a closed
        // pipe, as in a pipeline).
        if state.masker.is_sealed() || state.writer_done {
            return;
        }
        let mut out = Vec::new();
        match got {
            Some(0) => {
                state.masker.finish(&mut out);
                state.read_all = true;
            }
            Some(n) => state.masker.push(&buf[..n], &mut out),
            None => state.masker.seal(&mut out),
        }
        if !out.is_empty() {
            state.queue.push_back(out);
        }
        shared.changed.notify_all();
        if !got.is_some_and(|n| n > 0) {
            return;
        }
    }
}

fn write_loop(mut sink: impl Write, shared: &Shared) {
    let failed = loop {
        let next = {
            let mut state = shared.lock();
            loop {
                if let Some(chunk) = state.queue.pop_front() {
                    shared.changed.notify_all();
                    break Some(chunk);
                }
                if state.read_all || state.masker.is_sealed() {
                    break None;
                }
                state = shared.wait(state);
            }
        };
        let Some(chunk) = next else {
            break false;
        };
        if sink.write_all(&chunk).and_then(|()| sink.flush()).is_err() {
            break true;
        }
    };
    let mut state = shared.lock();
    state.writer_done = true;
    if failed {
        state.queue.clear();
    }
    shared.changed.notify_all();
}

/// The wrapper's own stdout or stderr, written without `std::io::Stdout`'s lock and buffer
/// (they could hold up the exit) and, on Windows, as raw bytes (`std` refuses non-UTF-8 to a
/// console).
struct StdStream {
    out: OutStream,
    /// Unix: SIGTTOU is blocked in this (the writer's) thread.
    #[cfg_attr(windows, allow(dead_code))]
    ready: bool,
}

#[cfg(unix)]
impl Write for StdStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::os::fd::FromRawFd;
        if !self.ready {
            // The command may hold the terminal's foreground; with `tostop` a background write
            // would stop this process. Blocked, the write goes through; only this thread.
            // SAFETY: sigemptyset/sigaddset fill a local set; pthread_sigmask changes this
            // thread's mask only.
            unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGTTOU);
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            }
            self.ready = true;
        }
        let fd = match self.out {
            OutStream::Stdout => 1,
            OutStream::Stderr => 2,
        };
        // SAFETY: borrowed for one write and never closed (ManuallyDrop): fd 1/2 stay ours.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        (&*file).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
impl Write for StdStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };
        let which = match self.out {
            OutStream::Stdout => STD_OUTPUT_HANDLE,
            OutStream::Stderr => STD_ERROR_HANDLE,
        };
        // SAFETY: GetStdHandle only reads the process's standard handles.
        let handle = unsafe { GetStdHandle(which) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::other("no standard handle"));
        }
        let len = buf.len().min(u32::MAX as usize) as u32;
        let mut written = 0u32;
        // SAFETY: `buf` is readable for `len` bytes and `written` is writable; no overlap.
        if unsafe {
            WriteFile(
                handle,
                buf.as_ptr(),
                len,
                &mut written,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn spec(name: &str, env: Option<&str>) -> SecretSpec {
        SecretSpec {
            name: name.into(),
            env: env.map(Into::into),
        }
    }

    #[test]
    fn secret_spec_parses() {
        assert_eq!(parse_spec("A"), Some(spec("A", None)));
        assert_eq!(parse_spec("API_KEY=B"), Some(spec("API_KEY", Some("B"))));
        assert_eq!(parse_spec("A=_lower1"), Some(spec("A", Some("_lower1"))));
        for bad in ["a", "A=1x", "", "=B", "A=", "A=B=C", "sk-live-0123"] {
            assert_eq!(parse_spec(bad), None, "spec of {} bytes", bad.len());
        }
    }

    #[test]
    fn env_defaults_from_config_then_name() {
        let config = crate::project::config::parse(
            "[secrets.API_KEY]\npurpose = \"x\"\nenv = \"SERVICE_TOKEN\"\n",
        )
        .unwrap();
        let got = injections(
            &[spec("API_KEY", None), spec("OTHER", None)],
            &[spec("CERT", Some("CERT_PATH"))],
            Some(&config),
        )
        .unwrap();
        let envs: Vec<(&str, bool)> = got.iter().map(|i| (i.env.as_str(), i.file)).collect();
        assert_eq!(
            envs,
            [
                ("SERVICE_TOKEN", false),
                ("OTHER", false),
                ("CERT_PATH", true)
            ]
        );
        // The flag wins over the config.
        let got = injections(&[spec("API_KEY", Some("MINE"))], &[], Some(&config)).unwrap();
        assert_eq!(got[0].env, "MINE");
    }

    #[test]
    fn duplicate_env_is_refused() {
        assert!(injections(&[spec("A", None), spec("B", Some("A"))], &[], None).is_err());
        assert!(injections(&[spec("A", None)], &[spec("A", None)], None).is_err());
        // The same secret twice under two names is fine.
        assert!(injections(&[spec("A", None)], &[spec("A", Some("A_FILE"))], None).is_ok());
        let case = injections(&[spec("PATH", None), spec("X", Some("Path"))], &[], None);
        assert_eq!(case.is_err(), cfg!(windows));
    }

    #[test]
    fn child_io_masks_messages() {
        let io = ChildIo {
            env: Vec::new(),
            masks: Some(Arc::new(MaskSet::new([b"s3cr3t-value".to_vec()]))),
        };
        assert_eq!(
            io.mask("cannot run ./s3cr3t-value.sh"),
            "cannot run ./***.sh"
        );
        assert_eq!(ChildIo::default().mask("as is"), "as is");
    }

    /// Yields what the test sends; EOF once the sender is dropped.
    struct ChanReader(mpsc::Receiver<Vec<u8>>);

    impl Read for ChanReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(bytes) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Err(_) => Ok(0),
            }
        }
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn get(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }

        /// Waits (up to 5 s, a loaded machine) for the sink to hold `want`. No bytes in the
        /// message: they may be a value.
        fn wait_for(&self, want: &[u8]) {
            let until = Instant::now() + Duration::from_secs(5);
            while self.get() != want {
                assert!(
                    Instant::now() < until,
                    "the sink never held the expected bytes"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn set(value: &[u8]) -> Arc<MaskSet> {
        Arc::new(MaskSet::new([value.to_vec()]))
    }

    #[test]
    fn sealed_pump_discards_later_data() {
        let (tx, rx) = mpsc::channel();
        let sink = Sink::default();
        let pump = Pump::start_with(ChanReader(rx), sink.clone(), set(b"abcdef"));
        tx.send(b"xabc".to_vec()).unwrap();
        sink.wait_for(b"x");
        pump.finish_by(Instant::now());
        sink.wait_for(b"x***");
        tx.send(b"def".to_vec()).unwrap();
        drop(tx);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            sink.get() == b"x***",
            "data after the seal reached the stream"
        );
    }

    #[test]
    fn eof_flushes_a_held_prefix_verbatim() {
        let (tx, rx) = mpsc::channel();
        let sink = Sink::default();
        let pump = Pump::start_with(ChanReader(rx), sink.clone(), set(b"abcdef"));
        tx.send(b"ab".to_vec()).unwrap();
        drop(tx);
        pump.finish_by(Instant::now() + Duration::from_secs(5));
        assert_eq!(sink.get(), b"ab");
    }

    #[test]
    fn output_is_masked_across_reads_and_kept_in_order() {
        let (tx, rx) = mpsc::channel();
        let sink = Sink::default();
        let pump = Pump::start_with(ChanReader(rx), sink.clone(), set(b"abcdef"));
        for chunk in [&b"1 ab"[..], b"cd", b"ef 2 abc", b"def", b" 3"] {
            tx.send(chunk.to_vec()).unwrap();
        }
        drop(tx);
        pump.finish_by(Instant::now() + Duration::from_secs(5));
        assert_eq!(sink.get(), b"1 *** 2 *** 3");
    }

    /// Yields `first`, then fails.
    struct FailsAfter(Option<Vec<u8>>);

    impl Read for FailsAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.take() {
                Some(bytes) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                None => Err(std::io::Error::other("the pipe broke")),
            }
        }
    }

    #[test]
    fn read_error_seals_a_held_prefix() {
        let sink = Sink::default();
        let pump = Pump::start_with(
            FailsAfter(Some(b"xabc".to_vec())),
            sink.clone(),
            set(b"abcdef"),
        );
        sink.wait_for(b"x***");
        pump.finish_by(Instant::now() + Duration::from_secs(5));
        assert!(
            sink.get() == b"x***",
            "a held prefix left after a read error"
        );
    }

    /// Never accepts a byte: a full pipe nobody reads.
    struct Stuck(mpsc::Receiver<()>);

    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Err(std::io::Error::other("gone"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn finish_by_returns_on_time_with_a_blocked_writer() {
        let (_never, stuck) = mpsc::channel();
        let mut output = vec![b'.'; 256 * 1024];
        output[1000..1006].copy_from_slice(b"abcdef");
        let pump = Pump::start_with(std::io::Cursor::new(output), Stuck(stuck), set(b"abcdef"));
        let shared = pump.shared.clone();
        let started = Instant::now();
        pump.finish_by(started + Duration::from_millis(300));
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_millis(1300),
            "{took:?}"
        );
        assert!(shared.lock().masker.is_sealed());
    }

    #[test]
    fn failed_write_ends_the_pump_at_once() {
        let (never, stuck) = mpsc::channel::<()>();
        drop(never);
        // The writer fails at once (the reader of our stdout is gone).
        let (tx, rx) = mpsc::channel();
        let pump = Pump::start_with(ChanReader(rx), Stuck(stuck), set(b"abcdef"));
        tx.send(b"first".to_vec()).unwrap();
        let shared = pump.shared.clone();
        let until = Instant::now() + Duration::from_secs(5);
        while !shared.lock().writer_done {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        pump.finish_by(started + Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
