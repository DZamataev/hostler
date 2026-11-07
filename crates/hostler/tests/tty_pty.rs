//! The hidden prompt against a real pseudo-terminal (plan P2 Task 7, L15). The probe runs in its
//! own session with the PTY as its controlling terminal and stderr; stdout is a pipe that
//! carries only `len=<n> sha256=<hex>`. Every value check compares length and hash, never the
//! value itself.
#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

fn probe_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        escargot::CargoBuild::new()
            .example("prompt_probe")
            .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .run()
            .expect("building the prompt_probe example")
            .path()
            .to_path_buf()
    })
}

fn cloexec(fd: libc::c_int) {
    // SAFETY: F_SETFD on an fd we own.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
}

struct Session {
    child: Child,
    /// The terminal's settings are read through the master afterwards: macOS revokes every
    /// slave fd when the probe, the session's leader, exits.
    master: File,
    _slave: OwnedFd,
    /// Everything the probe wrote to the terminal (once something drains it).
    screen: Arc<Mutex<Vec<u8>>>,
    /// The probe's stdout so far, read continuously.
    out: Arc<Mutex<Vec<u8>>>,
    out_reader: Option<std::thread::JoinHandle<()>>,
    /// The terminal's settings before the probe ran.
    before: libc::termios,
}

fn termios(fd: &impl AsRawFd) -> libc::termios {
    // SAFETY: a valid fd and a writable termios.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(fd.as_raw_fd(), &mut t), 0);
        t
    }
}

const RESTORED: libc::tcflag_t = libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN;

/// A fresh PTY: master, slave and the slave's device path.
fn open_pty() -> (File, OwnedFd, std::ffi::CString) {
    // openpty goes through ptsname's static buffer on macOS: one PTY at a time, or parallel
    // tests race for each other's slave.
    static OPENPTY: Mutex<()> = Mutex::new(());
    let _one = OPENPTY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut m, mut s) = (-1, -1);
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: out-pointers for two fds and a name buffer far longer than a PTY path; default
    // settings and size.
    let rc = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            name.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    cloexec(m);
    cloexec(s);
    // SAFETY: openpty wrote a NUL-terminated path.
    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_owned();
    // SAFETY: both fds were just opened and are owned here only.
    unsafe { (File::from_raw_fd(m), OwnedFd::from_raw_fd(s), name) }
}

/// Fills the terminal's output queue (nobody reads the master): every later blocking write
/// to the terminal blocks, as when a person pressed Ctrl+S.
fn fill_output(slave_path: &std::ffi::CStr) {
    // SAFETY: a NUL-terminated path; the fd is closed below.
    let fd = unsafe {
        libc::open(
            slave_path.as_ptr(),
            libc::O_WRONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    assert!(
        fd >= 0,
        "open the slave: {}",
        std::io::Error::last_os_error()
    );
    let chunk = [b'x'; 1024];
    let mut total = 0;
    loop {
        // SAFETY: a valid buffer of `chunk.len()` bytes.
        let n = unsafe { libc::write(fd, chunk.as_ptr().cast(), chunk.len()) };
        if n <= 0 {
            let e = std::io::Error::last_os_error();
            assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock, "{e}");
            break;
        }
        total += n as usize;
        assert!(total < 64 << 20, "the output queue never filled");
    }
    // SAFETY: opened above.
    unsafe { libc::close(fd) };
}

/// The probe on a fresh PTY; `stdin_tty: false` gives it a pipe as stdin (an agent's shell).
fn spawn(env: &[(&str, &str)], stdin_tty: bool) -> Session {
    start(env, stdin_tty, true)
}

/// `drain: false`: the output queue is filled first and never read.
fn start(env: &[(&str, &str)], stdin_tty: bool, drain: bool) -> Session {
    let (master, slave, slave_path) = open_pty();
    if !drain {
        fill_output(&slave_path);
    }
    let before = termios(&slave);
    assert_eq!(
        before.c_lflag & RESTORED,
        RESTORED,
        "a fresh PTY is canonical with echo"
    );

    let mut command = Command::new(probe_bin());
    command
        .env_remove("HOSTLER_TEST_TTY")
        .env_remove("HOSTLER_TEST_PROMPT_IDLE_MS")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let tty = slave.as_raw_fd();
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 || libc::ioctl(tty, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if (stdin_tty && libc::dup2(tty, 0) == -1) || libc::dup2(tty, 2) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();

    let screen = Arc::new(Mutex::new(Vec::new()));
    if drain {
        collect(master.try_clone().unwrap(), &screen);
    }
    let out = Arc::new(Mutex::new(Vec::new()));
    let out_reader = Some(collect(child.stdout.take().unwrap(), &out));
    Session {
        child,
        master,
        _slave: slave,
        screen,
        out,
        out_reader,
        before,
    }
}

/// Reads `from` into `into` until EOF or an error, on a thread.
fn collect(
    mut from: impl Read + Send + 'static,
    into: &Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    let sink = into.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                return;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    })
}

impl Session {
    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()).into_owned()
    }

    /// Typing starts only once the prompt is drawn: the probe flushes earlier input.
    fn wait_for_prompt(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.screen().contains("Value:") {
            assert!(
                Instant::now() < deadline,
                "no prompt; screen: {:?}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Writes from a thread: a big paste blocks on the PTY's flow control until it is read.
    fn type_bytes(&self, bytes: Vec<u8>) {
        let mut master = self.master.try_clone().unwrap();
        std::thread::spawn(move || {
            let _ = master.write_all(&bytes);
        });
    }

    fn wait(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("the probe did not finish within {within:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The whole stdout, once the probe closed it.
    fn stdout(&mut self) -> String {
        if let Some(reader) = self.out_reader.take() {
            reader.join().unwrap();
        }
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    fn stdout_so_far(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    fn assert_restored(&self) {
        let after = termios(&self.master);
        assert_eq!(
            after.c_lflag & RESTORED,
            RESTORED,
            "the terminal was not restored"
        );
        // PENDIN is kernel state (macOS sets it when ICANON comes back on and clears it once
        // it could retype pending input), not a setting the prompt changed.
        assert_eq!(
            after.c_lflag & !libc::PENDIN,
            self.before.c_lflag & !libc::PENDIN
        );
        assert_eq!(after.c_cc[libc::VMIN], self.before.c_cc[libc::VMIN]);
    }

    fn key(&self, index: usize) -> u8 {
        self.before.c_cc[index]
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // An exiting probe may wait for its terminal's output to drain (macOS).
        self.drain_now();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn summary(value: &[u8]) -> String {
    let hex: String = Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("len={} sha256={hex}", value.len())
}

/// `n` bytes of printable ASCII.
fn printable(n: usize) -> Vec<u8> {
    (0..n).map(|i| b'!' + (i % 94) as u8).collect()
}

/// Types `input` at the prompt and expects `value` to arrive.
fn entered(input: &[u8], value: &[u8], within: Duration) {
    let mut s = spawn(&[], true);
    s.wait_for_prompt();
    s.type_bytes(input.to_vec());
    let status = s.wait(within);
    assert!(status.success(), "{status:?}; screen: {:?}", s.screen());
    assert_eq!(s.stdout().trim(), summary(value));
    s.assert_restored();
}

#[test]
fn pty_hidden_input_is_read_without_echo() {
    let mut s = spawn(&[], true);
    s.wait_for_prompt();
    s.type_bytes(b"abcd\r".to_vec());
    assert!(s.wait(Duration::from_secs(5)).success());
    assert_eq!(s.stdout().trim(), summary(b"abcd"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!s.screen().contains("abcd"), "echoed: {:?}", s.screen());
    s.assert_restored();
}

#[test]
fn pty_long_values_arrive_exactly() {
    for n in [1023, 1024, 5000] {
        let value = printable(n);
        let mut input = value.clone();
        input.push(b'\r');
        entered(&input, &value, Duration::from_secs(2));
    }
}

#[test]
fn pty_over_64_kib_is_refused_not_cut() {
    let mut s = spawn(&[], true);
    s.wait_for_prompt();
    let mut input = printable(64 * 1024 + 1);
    input.push(b'\r');
    s.type_bytes(input);
    assert_eq!(s.wait(Duration::from_secs(5)).code(), Some(1));
    assert_eq!(
        s.stdout().trim(),
        "bad_request: the value is longer than 64 KiB; nothing was taken"
    );
    s.assert_restored();
}

#[test]
fn pty_erase_kill_and_escapes() {
    let defaults = termios(&open_pty().1);
    let (erase, kill) = (defaults.c_cc[libc::VERASE], defaults.c_cc[libc::VKILL]);
    let within = Duration::from_secs(5);
    entered(
        &[b"abX".as_slice(), &[erase], b"c\r"].concat(),
        b"abc",
        within,
    );
    entered(
        &[b"junk".as_slice(), &[kill], b"wxyz\r"].concat(),
        b"wxyz",
        within,
    );
    entered(b"ab\x1b[Acd\r", b"abcd", within);
    entered(&["xé".as_bytes(), &[erase], b"\r"].concat(), b"x", within);
}

#[test]
fn pty_ctrl_d_on_empty_input_is_no_value() {
    let mut s = spawn(&[], true);
    s.wait_for_prompt();
    s.type_bytes(vec![s.key(libc::VEOF)]);
    assert_eq!(s.wait(Duration::from_secs(5)).code(), Some(1));
    assert_eq!(s.stdout().trim(), "bad_request: no value entered");
    s.assert_restored();
}

#[test]
fn pty_ctrl_c_restores_the_terminal() {
    let mut s = spawn(&[], true);
    s.wait_for_prompt();
    s.type_bytes([b"ab".as_slice(), &[s.key(libc::VINTR)]].concat());
    let status = s.wait(Duration::from_secs(5));
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    s.assert_restored();
}

#[test]
fn pty_sigterm_restores_the_terminal() {
    let mut s = spawn(&[], true);
    s.wait_for_prompt();
    // SAFETY: a plain kill of our own child.
    unsafe { libc::kill(s.child.id() as libc::pid_t, libc::SIGTERM) };
    let status = s.wait(Duration::from_secs(5));
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
    s.assert_restored();
}

#[test]
fn pty_timeout_restores_the_terminal() {
    let mut s = spawn(&[("HOSTLER_TEST_PROMPT_IDLE_MS", "300")], true);
    s.wait_for_prompt();
    assert_eq!(s.wait(Duration::from_secs(3)).code(), Some(1));
    assert!(
        s.stdout()
            .starts_with("not_interactive: no answer at the terminal within")
    );
    s.assert_restored();
}

#[test]
fn pty_with_piped_stdin_is_not_interactive() {
    let mut s = spawn(&[], false);
    assert_eq!(s.wait(Duration::from_secs(5)).code(), Some(1));
    std::thread::sleep(Duration::from_millis(100));
    assert!(s.stdout().starts_with("not_interactive: "));
    let screen = s.screen();
    assert!(!screen.contains("Value:"), "a prompt was drawn: {screen:?}");
    s.assert_restored();
}

impl Session {
    /// Waits up to `within` for `text` on stdout.
    fn wait_for_stdout(&self, text: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.stdout_so_far().contains(text) {
            assert!(
                Instant::now() < deadline,
                "no {text:?} within {within:?}; stdout: {:?}",
                self.stdout_so_far()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Starts reading the terminal. macOS lets a session leader (the probe) finish exiting
    /// only once its terminal's output has drained.
    fn drain_now(&self) {
        collect(self.master.try_clone().unwrap(), &self.screen);
    }

    fn signal(&self, sig: libc::c_int) {
        // SAFETY: a plain kill of our own child.
        unsafe { libc::kill(self.child.id() as libc::pid_t, sig) };
    }

    /// Hidden mode is on (read through the master: the output queue may be full).
    fn wait_for_hidden_mode(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while termios(&self.master).c_lflag & libc::ECHO != 0 {
            assert!(Instant::now() < deadline, "hidden mode was never applied");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// T7-1: a second SIGINT while the first handler has not restored yet (paused at its entry)
/// must not kill the process with the terminal still hidden.
#[test]
fn pty_second_interrupt_during_the_handler_still_restores() {
    let mut s = spawn(&[("HOSTLER_TEST_PROMPT_PAUSE", "handler_entry:500")], true);
    s.wait_for_prompt();
    s.signal(libc::SIGINT);
    s.wait_for_stdout("[pause:handler_entry]", Duration::from_secs(5));
    s.signal(libc::SIGINT);
    let status = s.wait(Duration::from_secs(5));
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    s.assert_restored();
}

/// T7-2: a termination signal taken by another thread while the owner is about to apply hidden
/// mode (paused there). Hidden mode must not survive the signal.
#[test]
fn pty_signal_while_hidden_mode_is_applied_still_restores() {
    let mut s = spawn(
        &[
            ("PROBE_SIGNALS_ON_WORKER", "1"),
            ("HOSTLER_TEST_PROMPT_PAUSE", "before_mode:300"),
        ],
        true,
    );
    s.wait_for_stdout("[pause:before_mode]", Duration::from_secs(10));
    s.signal(libc::SIGTERM);
    let status = s.wait(Duration::from_secs(5));
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
    s.assert_restored();
}

/// T7-3: with the terminal's output never drained (the prompt text cannot be written), the
/// prompt still gives up on time and restores the terminal. Draining starts only afterwards,
/// so the probe can finish exiting.
#[test]
fn pty_undrained_output_still_times_out() {
    let mut s = start(&[("HOSTLER_TEST_PROMPT_IDLE_MS", "300")], true, false);
    s.wait_for_stdout(
        "not_interactive: no answer at the terminal within",
        Duration::from_secs(3),
    );
    s.assert_restored();
    s.drain_now();
    assert_eq!(s.wait(Duration::from_secs(5)).code(), Some(1));
}

/// T7-3: with the terminal's output never drained, SIGTERM still restores the terminal and
/// the owner gets past its newline to the re-raise (the marker comes right before it).
#[test]
fn pty_undrained_output_sigterm_still_terminates() {
    let mut s = start(&[("HOSTLER_TEST_PROMPT_PAUSE", "restored:1")], true, false);
    s.wait_for_hidden_mode();
    s.signal(libc::SIGTERM);
    s.wait_for_stdout("[pause:restored]", Duration::from_secs(3));
    s.assert_restored();
    s.drain_now();
    let status = s.wait(Duration::from_secs(5));
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
}

/// The probe dies of `sig` with the terminal restored.
fn dies_restored(s: &mut Session, sig: libc::c_int) {
    let status = s.wait(Duration::from_secs(10));
    assert_eq!(status.signal(), Some(sig), "{status:?}");
    s.assert_restored();
}

/// T7-1 (R2): two termination signals back to back, on the owner's thread or another one.
#[test]
fn pty_two_signals_back_to_back_still_restore() {
    for (worker, first, second) in [
        (false, libc::SIGINT, libc::SIGINT),
        (true, libc::SIGINT, libc::SIGINT),
        (false, libc::SIGTERM, libc::SIGHUP),
        (true, libc::SIGTERM, libc::SIGQUIT),
    ] {
        let env: &[(&str, &str)] = if worker {
            &[("PROBE_SIGNALS_ON_WORKER", "1")]
        } else {
            &[]
        };
        let mut s = spawn(env, true);
        s.wait_for_prompt();
        s.signal(first);
        s.signal(second);
        let status = s.wait(Duration::from_secs(10));
        let sig = status.signal();
        assert!(
            sig == Some(first) || sig == Some(second),
            "{status:?} (worker {worker})"
        );
        s.assert_restored();
    }
}

/// T7-2 (R2, startup): the signal's handler runs to its end on the owner's thread before the
/// owner applies hidden mode (it "sees idle"). Hidden mode applied afterwards must not survive.
#[test]
fn pty_signal_before_hidden_mode_still_restores() {
    let mut s = spawn(&[("HOSTLER_TEST_PROMPT_PAUSE", "before_mode:300")], true);
    s.wait_for_stdout("[pause:before_mode]", Duration::from_secs(10));
    s.signal(libc::SIGINT);
    dies_restored(&mut s, libc::SIGINT);
}

/// T7-2 (R2): a signal taken by another thread just after the owner applied hidden mode,
/// before it waits for the answer.
#[test]
fn pty_signal_on_another_thread_right_after_hidden_mode_still_restores() {
    let mut s = spawn(
        &[
            ("PROBE_SIGNALS_ON_WORKER", "1"),
            ("HOSTLER_TEST_PROMPT_PAUSE", "after_mode:300"),
        ],
        true,
    );
    s.wait_for_stdout("[pause:after_mode]", Duration::from_secs(10));
    s.signal(libc::SIGTERM);
    dies_restored(&mut s, libc::SIGTERM);
}

/// T7-1 (R2): the owner has the first signal but has not restored yet (paused) when a second
/// one arrives, on its own thread or another one. Neither may end the process first.
#[test]
fn pty_second_signal_while_the_owner_restores_still_restores() {
    for (worker, second) in [(false, libc::SIGINT), (true, libc::SIGTERM)] {
        let mut env = vec![("HOSTLER_TEST_PROMPT_PAUSE", "after_signal:800")];
        if worker {
            env.push(("PROBE_SIGNALS_ON_WORKER", "1"));
        }
        let mut s = spawn(&env, true);
        s.wait_for_prompt();
        s.signal(libc::SIGINT);
        s.wait_for_stdout("[pause:after_signal]", Duration::from_secs(5));
        s.signal(second);
        dies_restored(&mut s, libc::SIGINT);
    }
}

/// T7-2 (R2, cleanup): a signal while the owner is about to clean up after the answer still
/// ends the process, after the terminal is restored; the answer is not used.
#[test]
fn pty_signal_during_cleanup_still_ends_the_process() {
    let mut s = spawn(&[("HOSTLER_TEST_PROMPT_PAUSE", "after_answer:800")], true);
    s.wait_for_prompt();
    s.type_bytes(b"abcd\r".to_vec());
    s.wait_for_stdout("[pause:after_answer]", Duration::from_secs(5));
    s.signal(libc::SIGTERM);
    dies_restored(&mut s, libc::SIGTERM);
    assert!(!s.stdout().contains("len="), "{:?}", s.stdout());
}

/// The fallback (L17 residual): an owner wedged after the signal does not keep the process
/// alive; about `OWNER_GRACE` (2 s) later it dies of the signal without the terminal being
/// touched by anyone but the owner, so here it is still hidden.
#[test]
fn pty_wedged_owner_still_dies_of_the_signal() {
    let mut s = spawn(&[("HOSTLER_TEST_PROMPT_PAUSE", "after_signal:8000")], true);
    s.wait_for_prompt();
    let sent = Instant::now();
    s.signal(libc::SIGINT);
    let status = s.wait(Duration::from_secs(7));
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    assert!(
        sent.elapsed() >= Duration::from_millis(1500),
        "{:?}",
        sent.elapsed()
    );
    assert_eq!(termios(&s.master).c_lflag & libc::ECHO, 0);
}

/// A signal the caller ignores (nohup) stays ignored during the prompt, and so does SIGTSTP.
#[test]
fn pty_ignored_signals_stay_ignored() {
    let mut s = spawn(&[("PROBE_IGNORE_SIGHUP", "1")], true);
    s.wait_for_prompt();
    s.signal(libc::SIGHUP);
    s.signal(libc::SIGTSTP);
    std::thread::sleep(Duration::from_millis(300));
    s.type_bytes(b"abcd\r".to_vec());
    let status = s.wait(Duration::from_secs(5));
    assert!(status.success(), "{status:?}");
    assert_eq!(s.stdout().trim(), summary(b"abcd"));
    s.assert_restored();
}

/// T7-2 (R3): the watcher commits to dying with no owner (prompt A has just ended), and the
/// same process starts prompt B before the death. B must not enter hidden mode.
#[test]
fn pty_next_prompt_while_the_process_is_dying_is_not_admitted() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut s = spawn(
        &[
            ("PROBE_GATE", gate.to_str().unwrap()),
            (
                "HOSTLER_TEST_PROMPT_PAUSE",
                "watcher_entry:1000,watcher_decided:1500",
            ),
        ],
        true,
    );
    s.wait_for_prompt();
    // Caught by A's handler; the watcher holds it until A has ended.
    s.signal(libc::SIGTERM);
    s.wait_for_stdout("[pause:watcher_entry]", Duration::from_secs(5));
    s.type_bytes(b"abcd\r".to_vec());
    s.wait_for_stdout(&summary(b"abcd"), Duration::from_secs(5));
    // No owner: the watcher has committed to dying. B starts now, well before the death.
    s.wait_for_stdout("[pause:watcher_decided]", Duration::from_secs(5));
    std::fs::write(&gate, b"").unwrap();
    dies_restored(&mut s, libc::SIGTERM);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(s.screen().matches("Value:").count(), 1, "B drew its prompt");
}

/// T7-R3-1: the owner wedged while it holds the registry lock (unregistering) does not hold up
/// the fallback: the process dies of the signal about `OWNER_GRACE` after it arrived.
#[test]
fn pty_owner_wedged_holding_the_registry_does_not_block_the_fallback() {
    let mut s = spawn(
        &[(
            "HOSTLER_TEST_PROMPT_PAUSE",
            "watcher_entry:300,unregister:8000",
        )],
        true,
    );
    s.wait_for_prompt();
    let sent = Instant::now();
    s.signal(libc::SIGTERM);
    s.wait_for_stdout("[pause:watcher_entry]", Duration::from_secs(5));
    s.type_bytes(b"abcd\r".to_vec());
    s.wait_for_stdout("[pause:unregister]", Duration::from_secs(5));
    let status = s.wait(Duration::from_secs(6));
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
    assert!(
        sent.elapsed() >= Duration::from_millis(1500),
        "{:?}",
        sent.elapsed()
    );
    // The owner restored the terminal before it took the lock.
    s.assert_restored();
}

/// CR-4 (Unix side): the visible prompt reads a canonical line and never changes the terminal;
/// the answer is echoed by the terminal itself.
#[test]
fn pty_visible_line_leaves_the_terminal_as_it_was() {
    let mut s = spawn(&[("PROBE_LINE", "1")], true);
    s.wait_for_prompt();
    s.type_bytes(b"my app\r".to_vec());
    assert!(s.wait(Duration::from_secs(5)).success());
    assert_eq!(s.stdout().trim(), "line=6");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        s.screen().contains("my app"),
        "not echoed: {:?}",
        s.screen()
    );
    s.assert_restored();
}

/// CR-4 (Unix side): Ctrl+C at the visible prompt ends the process by the default action, with
/// the terminal as it was (nothing was changed).
#[test]
fn pty_ctrl_c_at_a_visible_line_leaves_the_terminal_as_it_was() {
    let mut s = spawn(&[("PROBE_LINE", "1")], true);
    s.wait_for_prompt();
    s.type_bytes([b"ab".as_slice(), &[s.key(libc::VINTR)]].concat());
    dies_restored(&mut s, libc::SIGINT);
}
