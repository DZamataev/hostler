//! Interactive-terminal detection and person prompts (plan P2, Task 7). Prompts read from the
//! controlling terminal (`/dev/tty`, the Windows console), never from stdin as data, an argument
//! or the environment. The hidden prompt reads in non-canonical mode through its own line editor
//! (L15): no line-length limit of the terminal applies, and the terminal is restored on every way
//! out — answer, timeout, Ctrl+C, SIGTERM, SIGHUP, SIGQUIT.

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(debug_assertions)]
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError, TryLockError, mpsc};
use std::time::{Duration, Instant};

use serde_json::json;
use zeroize::Zeroizing;

use crate::cli::wait::duration_text;
use crate::contract::secrets::errors;
use crate::contract::{ErrorCode, HostlerError};
use crate::secrets::MAX_VALUE_BYTES;
use crate::secrets::value::SecretValue;

/// A prompt nobody answers gives up after this long.
pub const PROMPT_IDLE: Duration = Duration::from_secs(300);
/// Confirmations and project names are short; a longer answer is a refused confirmation.
const MAX_LINE_BYTES: usize = 1023;

/// What the interactive check saw (Global Constraints "Interactive").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    pub stdin_tty: bool,
    pub stderr_tty: bool,
    pub terminal_opens: bool,
}

/// A person is at the terminal only when all three hold: an agent's shell has a pipe as stdin
/// even when it inherited a controlling terminal.
pub fn decide(p: &Probe) -> bool {
    p.stdin_tty && p.stderr_tty && p.terminal_opens
}

pub fn probe() -> Probe {
    platform::probe()
}

/// `probe()`, or true under the debug hook `HOSTLER_TEST_TTY`.
pub fn interactive() -> bool {
    #[cfg(debug_assertions)]
    if test_tty().is_some() {
        return true;
    }
    decide(&probe())
}

pub fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

#[cfg(debug_assertions)]
fn test_tty() -> Option<PathBuf> {
    std::env::var_os("HOSTLER_TEST_TTY").map(PathBuf::from)
}

fn prompt_idle() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_PROMPT_IDLE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    PROMPT_IDLE
}

fn not_interactive(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::NotInteractive,
        message,
        details: json!({}),
        recovery: None,
    }
}

fn timed_out(idle: Duration) -> HostlerError {
    not_interactive(format!(
        "no answer at the terminal within {}",
        duration_text(idle)
    ))
}

fn terminal_closed() -> HostlerError {
    not_interactive("the terminal closed before an answer".into())
}

/// Waits for a reader thread's answer. On a timeout the thread stays blocked in its read; it
/// owns its own descriptor, so nothing it holds is closed under it.
#[cfg(unix)]
fn answer<T>(
    rx: &mpsc::Receiver<Result<T, HostlerError>>,
    idle: Duration,
) -> Result<T, HostlerError> {
    match rx.recv_timeout(idle) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(timed_out(idle)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(terminal_closed()),
    }
}

/// Prompts for a person. Created only when `interactive()`; it never draws anything before a
/// prompt method is called.
pub struct Tty {
    source: Source,
    idle: Duration,
    command: String,
}

enum Source {
    Terminal,
    #[cfg(test)]
    TestLines(VecDeque<String>),
    /// `HOSTLER_TEST_TTY`: lines read lazily on a helper thread (a FIFO blocks like a terminal).
    #[cfg(debug_assertions)]
    TestFile(mpsc::Receiver<std::io::Result<String>>),
}

impl Tty {
    /// `NotInteractive` for `hostler {command}` unless a person is at the terminal.
    pub fn open(command: &str) -> Result<Tty, HostlerError> {
        #[cfg(debug_assertions)]
        if let Some(path) = test_tty() {
            return Ok(Tty::from_file(path, prompt_idle()).named(command));
        }
        if !decide(&probe()) {
            return Err(errors::not_interactive(command));
        }
        Ok(Tty {
            source: Source::Terminal,
            idle: prompt_idle(),
            command: command.into(),
        })
    }

    #[cfg(test)]
    fn from_lines(lines: &[&str]) -> Tty {
        Tty {
            source: Source::TestLines(lines.iter().map(|l| (*l).to_owned()).collect()),
            idle: PROMPT_IDLE,
            command: "test".into(),
        }
    }

    #[cfg(debug_assertions)]
    fn from_file(path: PathBuf, idle: Duration) -> Tty {
        use std::io::BufRead;
        // Rendezvous: a line is read only when a prompt is waiting for one.
        let (tx, rx) = mpsc::sync_channel(0);
        std::thread::spawn(move || match std::fs::File::open(&path) {
            Ok(file) => {
                for line in std::io::BufReader::new(file).lines() {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        });
        Tty {
            source: Source::TestFile(rx),
            idle,
            command: "test".into(),
        }
    }

    #[cfg(debug_assertions)]
    fn named(mut self, command: &str) -> Tty {
        self.command = command.into();
        self
    }

    /// The next scripted answer (the prompt goes to stderr), or `None` at a real terminal.
    fn scripted(&mut self, prompt: &str) -> Result<Option<String>, HostlerError> {
        // Only the debug/test sources print it; a release build has the terminal source alone.
        #[cfg(not(debug_assertions))]
        let _ = prompt;
        match &mut self.source {
            Source::Terminal => Ok(None),
            #[cfg(test)]
            Source::TestLines(lines) => {
                eprintln!("{prompt}");
                match lines.pop_front() {
                    Some(line) => Ok(Some(line)),
                    None => Err(not_interactive("no more test input".into())),
                }
            }
            #[cfg(debug_assertions)]
            Source::TestFile(rx) => {
                eprintln!("{prompt}");
                match rx.recv_timeout(self.idle) {
                    Ok(Ok(line)) => Ok(Some(line)),
                    Ok(Err(_)) => Err(not_interactive("unreadable test input".into())),
                    Err(mpsc::RecvTimeoutError::Timeout) => Err(timed_out(self.idle)),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Err(not_interactive("no more test input".into()))
                    }
                }
            }
        }
    }

    /// A value typed without echo: up to 64 KiB of UTF-8, never shortened.
    pub fn hidden(&mut self, prompt: &str) -> Result<SecretValue, HostlerError> {
        let Some(line) = self.scripted(prompt)? else {
            return platform::hidden(prompt, self.idle, &self.command);
        };
        let line = Zeroizing::new(line);
        let mut editor = LineEditor::new(EditKeys::DEFAULT);
        for &byte in line.as_bytes().iter().chain(b"\n") {
            if let Edit::Done(result) = editor.feed(byte) {
                return result;
            }
        }
        unreachable!("a newline always ends the edit")
    }

    /// A visible answer, e.g. a project name; longer than 1023 bytes → `BadRequest`.
    pub fn line(&mut self, prompt: &str) -> Result<String, HostlerError> {
        self.short_answer(prompt)?.ok_or_else(|| {
            errors::bad_request(format!("the answer is longer than {MAX_LINE_BYTES} bytes"))
        })
    }

    /// `y` or `yes` (any case); anything else, an over-long answer included, is no.
    pub fn confirm(&mut self, prompt: &str) -> Result<bool, HostlerError> {
        Ok(self.short_answer(prompt)?.is_some_and(|a| {
            let a = a.trim();
            a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
        }))
    }

    /// The answer must be `expected` exactly; trailing whitespace is ignored, case is not.
    pub fn confirm_typed(&mut self, prompt: &str, expected: &str) -> Result<bool, HostlerError> {
        Ok(self
            .short_answer(prompt)?
            .is_some_and(|a| a.trim_end() == expected))
    }

    /// The answer without its line break; `None` when it is longer than `MAX_LINE_BYTES`.
    fn short_answer(&mut self, prompt: &str) -> Result<Option<String>, HostlerError> {
        let bytes = match self.scripted(prompt)? {
            Some(line) => line.into_bytes(),
            None => platform::line(prompt, self.idle, &self.command)?,
        };
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_end_matches(['\r', '\n']);
        Ok((text.len() <= MAX_LINE_BYTES).then(|| text.to_owned()))
    }
}

/// The terminal's editing keys (Unix: from the saved `c_cc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditKeys {
    pub erase: u8,
    pub kill: u8,
    pub werase: u8,
    pub eof: u8,
}

impl EditKeys {
    const DEFAULT: EditKeys = EditKeys {
        erase: 0x7f,
        kill: 0x15,
        werase: 0x17,
        eof: 0x04,
    };
}

pub enum Edit {
    More,
    Done(Result<SecretValue, HostlerError>),
}

#[derive(Clone, Copy)]
enum Escape {
    None,
    Start,
    /// `ESC [` … up to a final byte 0x40–0x7E.
    Csi,
    /// `ESC O x`
    Ss3,
}

/// The hidden prompt's own line editing (the terminal's is off in non-canonical mode).
pub struct LineEditor {
    buf: Zeroizing<Vec<u8>>,
    overflow: bool,
    escape: Escape,
    keys: EditKeys,
}

impl LineEditor {
    pub fn new(keys: EditKeys) -> LineEditor {
        LineEditor {
            // Full capacity at once: a growing Vec would leave copies of the value behind.
            buf: Zeroizing::new(Vec::with_capacity(MAX_VALUE_BYTES)),
            overflow: false,
            escape: Escape::None,
            keys,
        }
    }

    pub fn feed(&mut self, byte: u8) -> Edit {
        let control = byte < 0x20 || byte == 0x7f;
        if control {
            // A control byte ends an escape sequence (a broken one) and keeps its own meaning.
            self.escape = Escape::None;
        } else {
            match self.escape {
                Escape::None => {}
                Escape::Start => {
                    self.escape = match byte {
                        b'[' => Escape::Csi,
                        b'O' => Escape::Ss3,
                        _ => Escape::None,
                    };
                    return Edit::More;
                }
                Escape::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = Escape::None;
                    }
                    return Edit::More;
                }
                Escape::Ss3 => {
                    self.escape = Escape::None;
                    return Edit::More;
                }
            }
        }
        match byte {
            b'\r' | b'\n' => return Edit::Done(self.finish()),
            _ if self.overflow => {}
            0x1b => self.escape = Escape::Start,
            b if control && (b == self.keys.erase || b == 0x7f || b == 0x08) => self.erase_char(),
            b if control && b == self.keys.kill => self.buf.clear(),
            b if control && b == self.keys.werase => self.erase_word(),
            b if control && b == self.keys.eof => {
                if self.buf.is_empty() {
                    return Edit::Done(Err(errors::bad_request("no value entered".into())));
                }
            }
            _ if control => {}
            _ if self.buf.len() == MAX_VALUE_BYTES => {
                self.overflow = true;
                self.buf.clear();
            }
            _ => self.buf.push(byte),
        }
        Edit::More
    }

    fn erase_char(&mut self) {
        // Continuation bytes, then the lead byte: one whole UTF-8 character.
        while let Some(byte) = self.buf.pop() {
            if byte & 0xc0 != 0x80 {
                break;
            }
        }
    }

    fn erase_word(&mut self) {
        while self.buf.last() == Some(&b' ') {
            self.buf.pop();
        }
        while self.buf.last().is_some_and(|&b| b != b' ') {
            self.buf.pop();
        }
    }

    fn finish(&mut self) -> Result<SecretValue, HostlerError> {
        if self.overflow {
            return Err(errors::bad_request(
                "the value is longer than 64 KiB; nothing was taken".into(),
            ));
        }
        match String::from_utf8(std::mem::take(&mut *self.buf)) {
            Ok(text) => Ok(SecretValue::new(text)),
            Err(e) => {
                drop(Zeroizing::new(e.into_bytes()));
                Err(errors::bad_request("the value is not UTF-8 text".into()))
            }
        }
    }
}

/// What the owner of a hidden prompt waits for: the reader's answer, or a termination request
/// handed over by the signal watcher (Unix) or the console control callback (Windows). Only the
/// owner ever changes or restores the terminal mode (L17).
enum Event<I> {
    Answer(Result<SecretValue, HostlerError>),
    Interrupt(I),
}

/// How long a termination request waits for the owner to restore the terminal and end the
/// process before it ends the process without the terminal (only a wedged owner takes longer).
const OWNER_GRACE: Duration = Duration::from_secs(2);

/// The answer (or the timeout), or the first termination request.
fn wait_event<I>(
    rx: &mpsc::Receiver<Event<I>>,
    idle: Duration,
) -> Result<Result<SecretValue, HostlerError>, I> {
    match rx.recv_timeout(idle) {
        Ok(Event::Answer(result)) => Ok(result),
        Ok(Event::Interrupt(request)) => Err(request),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(Err(timed_out(idle))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(Err(terminal_closed())),
    }
}

/// Who takes a termination request, and whether the process has committed to dying without an
/// owner. Both change only under the registry's lock, so committing to die and admitting a new
/// prompt are one linearizable step (T7-2): once `dying` is set no prompt changes the terminal.
struct Registry<I> {
    owner: Option<mpsc::Sender<Event<I>>>,
    dying: bool,
}

impl<I> Registry<I> {
    const fn new() -> Registry<I> {
        Registry {
            owner: None,
            dying: false,
        }
    }
}

/// The owner side: admits `owner`, or refuses because the process is about to die. A refused
/// prompt must not touch the terminal; it waits for death, then gives up as interrupted.
fn register<I>(
    registry: &Mutex<Registry<I>>,
    owner: mpsc::Sender<Event<I>>,
) -> Result<(), HostlerError> {
    let mut reg = registry.lock().unwrap_or_else(PoisonError::into_inner);
    if !reg.dying {
        reg.owner = Some(owner);
        return Ok(());
    }
    drop(reg);
    std::thread::sleep(OWNER_GRACE);
    Err(not_interactive("the prompt was interrupted".into()))
}

/// The termination side: hands `request` to the owner, or commits to dying when there is
/// none. The lock is only tried until `deadline` (T7-R3-1): an owner wedged while holding it
/// cannot hold the fallback up. Past the deadline → false (die without the terminal).
fn hand_over<I>(registry: &Mutex<Registry<I>>, request: I, deadline: Instant) -> bool {
    loop {
        let mut reg = match registry.try_lock() {
            Ok(reg) => reg,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
        };
        if let Some(owner) = &reg.owner
            && owner.send(Event::Interrupt(request)).is_ok()
        {
            return true;
        }
        reg.dying = true;
        return false;
    }
}

/// The termination side, after the owner's grace: no new prompt from here on (one try only; a
/// lock still held is held by a wedged owner, and there is only one prompt at a time).
fn commit_dying<I>(registry: &Mutex<Registry<I>>) {
    match registry.try_lock() {
        Ok(mut reg) => reg.dying = true,
        Err(TryLockError::Poisoned(p)) => p.into_inner().dying = true,
        Err(TryLockError::WouldBlock) => {}
    }
}

/// A console's input mode as the Windows prompts drive it (a fake one in the unit tests).
#[cfg(any(windows, test))]
trait Console {
    type Reader;
    fn mode(&self) -> Option<u32>;
    fn set_mode(&self, mode: u32) -> bool;
    /// A second handle for the reader thread.
    fn reader(&self) -> Option<Self::Reader>;
}

/// Puts the saved input mode back, on `restore` or on drop.
#[cfg(any(windows, test))]
struct ModeGuard<C: Console> {
    console: C,
    saved: u32,
    changed: bool,
}

#[cfg(any(windows, test))]
impl<C: Console> ModeGuard<C> {
    fn restore(&mut self) {
        if std::mem::take(&mut self.changed) {
            self.console.set_mode(self.saved);
        }
    }
}

#[cfg(any(windows, test))]
impl<C: Console> Drop for ModeGuard<C> {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Saves the mode, changes it to `wanted(saved)` (only when that differs) and takes the reader's
/// handle. The guard exists before the first change, so every way out restores the mode (CR-4).
#[cfg(any(windows, test))]
fn begin<C: Console>(
    console: C,
    wanted: impl FnOnce(u32) -> u32,
    command: &str,
) -> Result<(ModeGuard<C>, C::Reader), HostlerError> {
    let refused = || errors::not_interactive(command);
    let saved = console.mode().ok_or_else(refused)?;
    let mut guard = ModeGuard {
        console,
        saved,
        changed: false,
    };
    let mode = wanted(saved);
    if mode != saved {
        guard.changed = true;
        if !guard.console.set_mode(mode) {
            return Err(refused());
        }
    }
    let reader = guard.console.reader().ok_or_else(terminal_closed)?;
    Ok((guard, reader))
}

/// One console read. `Aborted`: it ended with no characters, which is what `ReadConsoleW` does
/// on Ctrl+C / Ctrl+Break (success, 0 units, `ERROR_OPERATION_ABORTED`), possibly before the
/// control event reaches the callback (W-7). `Failed`: the console is gone.
#[cfg(any(windows, test))]
enum ConsoleRead {
    Units(usize),
    Aborted,
    Failed,
}

/// What a Windows reader thread ends with: an answer, or `None` for an interrupted read, which
/// is never an answer (W-7).
#[cfg(any(windows, test))]
type Reply = Option<Result<SecretValue, HostlerError>>;

/// The hidden value from console reads: UTF-16 re-encoded as UTF-8 for the editor; a lone
/// surrogate becomes an invalid byte, so the value is refused as not UTF-8.
#[cfg(any(windows, test))]
fn read_value_from(keys: EditKeys, mut read: impl FnMut(&mut [u16]) -> ConsoleRead) -> Reply {
    let mut editor = LineEditor::new(keys);
    let mut units = Zeroizing::new([0u16; 4096]);
    let mut chunk: Zeroizing<Vec<u16>> = Zeroizing::new(Vec::with_capacity(4097));
    let mut pending_high: Option<u16> = None;
    loop {
        let n = match read(&mut units[..]) {
            ConsoleRead::Units(n) => n,
            ConsoleRead::Aborted => return None,
            ConsoleRead::Failed => return Some(Err(terminal_closed())),
        };
        chunk.clear();
        chunk.extend(pending_high.take());
        chunk.extend_from_slice(&units[..n]);
        // A surrogate pair split across two reads.
        if let Some(&last) = chunk.last()
            && (0xd800..=0xdbff).contains(&last)
        {
            pending_high = chunk.pop();
        }
        for c in char::decode_utf16(chunk.iter().copied()) {
            let mut utf8 = Zeroizing::new([0u8; 4]);
            let bytes: &[u8] = match c {
                Ok(c) => c.encode_utf8(&mut utf8[..]).as_bytes(),
                Err(_) => &[0xff],
            };
            for &byte in bytes {
                if let Edit::Done(result) = editor.feed(byte) {
                    return Some(result);
                }
            }
        }
    }
}

/// A visible line from console reads (line input on: a read ends at Enter); the units before
/// `\n`, at most `MAX_LINE_BYTES + 1` kept. A `SecretValue` only so both prompts share one
/// answer type.
#[cfg(any(windows, test))]
fn read_line_from(mut read: impl FnMut(&mut [u16]) -> ConsoleRead) -> Reply {
    let mut units: Vec<u16> = Vec::new();
    let mut buf = [0u16; 1024];
    loop {
        let n = match read(&mut buf) {
            ConsoleRead::Units(n) => n,
            ConsoleRead::Aborted => return None,
            ConsoleRead::Failed => return Some(Err(terminal_closed())),
        };
        let got = &buf[..n];
        let end = got.iter().position(|&u| u == u16::from(b'\n'));
        units.extend_from_slice(&got[..end.unwrap_or(got.len())]);
        // Units, not bytes, but enough to know the answer is over the limit.
        units.truncate(MAX_LINE_BYTES + 1);
        if end.is_some() {
            return Some(Ok(SecretValue::new(String::from_utf16_lossy(&units))));
        }
    }
}

/// The owner's way out of an interrupted Windows prompt, before it exits 130: the mode back
/// first, then — when the interruption was the reader's aborted read — up to `grace` for the
/// control callback to hand its event over; that event is acknowledged, so the callback
/// returns TRUE and default handling does not end the process with its own code (W-7).
#[cfg(any(windows, test))]
fn settle_interrupt<C: Console>(
    mode: &mut ModeGuard<C>,
    events: &mpsc::Receiver<Event<Option<mpsc::Sender<()>>>>,
    first: Option<mpsc::Sender<()>>,
    grace: Duration,
) {
    mode.restore();
    let deadline = Instant::now() + grace;
    let ack = first.or_else(|| {
        loop {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Interrupt(Some(ack))) => break Some(ack),
                Ok(_) => {}
                Err(_) => break None,
            }
        }
    });
    if let Some(ack) = ack {
        let _ = ack.send(());
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::io::{ErrorKind, Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError, mpsc};
    use std::time::{Duration, Instant};

    use libc::c_int;
    use zeroize::Zeroizing;

    use super::{
        Edit, EditKeys, Event, LineEditor, MAX_LINE_BYTES, OWNER_GRACE, Probe, Registry, answer,
        commit_dying, hand_over, register, terminal_closed, wait_event,
    };
    use crate::contract::HostlerError;
    use crate::contract::secrets::errors;
    use crate::secrets::value::SecretValue;

    pub(super) fn probe() -> Probe {
        use std::io::IsTerminal;
        Probe {
            stdin_tty: std::io::stdin().is_terminal(),
            stderr_tty: std::io::stderr().is_terminal(),
            terminal_opens: open_tty(0).is_ok(),
        }
    }

    fn open_tty(flags: c_int) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open("/dev/tty")
    }

    /// The write end of the self-pipe the termination handlers write to. Open for the life of
    /// the process: a late handler never writes into a reused descriptor.
    static PIPE_W: AtomicI32 = AtomicI32::new(-1);
    /// The hidden prompt the watcher hands a signal to, while there is one.
    static OWNER: Mutex<Registry<c_int>> = Mutex::new(Registry::new());
    /// One hidden prompt at a time in this process.
    static PROMPT: Mutex<()> = Mutex::new(());

    const SIGNALS: [c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

    fn signal_set(signals: &[c_int]) -> libc::sigset_t {
        // SAFETY: sigemptyset/sigaddset initialise and fill a sigset_t.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for &sig in signals {
                libc::sigaddset(&mut set, sig);
            }
            set
        }
    }

    /// Debug-only pause points (`HOSTLER_TEST_PROMPT_PAUSE=<point>:<ms>[,…]`) that make the
    /// races of T7-1/T7-2 deterministic in tests. Read before the handlers are installed; a
    /// configured point writes `[pause:<point>]` to fd 1 (the tests' stdout pipe, never the
    /// terminal) and sleeps (both async-signal-safe).
    mod hooks {
        use std::sync::atomic::{AtomicU32, Ordering};

        /// In the signal handler, before it hands the signal over.
        pub static HANDLER_ENTRY: AtomicU32 = AtomicU32::new(0);
        /// The owner: handlers installed, terminal not changed yet.
        pub static BEFORE_MODE: AtomicU32 = AtomicU32::new(0);
        /// The owner: hidden mode applied, not waiting yet.
        pub static AFTER_MODE: AtomicU32 = AtomicU32::new(0);
        /// The owner: the answer arrived, cleanup not started.
        pub static AFTER_ANSWER: AtomicU32 = AtomicU32::new(0);
        /// The owner: a signal arrived, the terminal not restored yet.
        pub static AFTER_SIGNAL: AtomicU32 = AtomicU32::new(0);
        /// The owner: terminal restored after a signal, about to die of it.
        pub static RESTORED: AtomicU32 = AtomicU32::new(0);
        /// The owner, holding the registry lock while it unregisters.
        pub static UNREGISTER: AtomicU32 = AtomicU32::new(0);
        /// The watcher: a signal read, the owner not looked up yet.
        pub static WATCHER_ENTRY: AtomicU32 = AtomicU32::new(0);
        /// The watcher: committed to die, not dead yet.
        pub static WATCHER_DECIDED: AtomicU32 = AtomicU32::new(0);

        #[cfg(debug_assertions)]
        pub fn load() {
            let spec = std::env::var("HOSTLER_TEST_PROMPT_PAUSE").unwrap_or_default();
            for (point, hook) in [
                ("handler_entry", &HANDLER_ENTRY),
                ("before_mode", &BEFORE_MODE),
                ("after_mode", &AFTER_MODE),
                ("after_answer", &AFTER_ANSWER),
                ("after_signal", &AFTER_SIGNAL),
                ("restored", &RESTORED),
                ("unregister", &UNREGISTER),
                ("watcher_entry", &WATCHER_ENTRY),
                ("watcher_decided", &WATCHER_DECIDED),
            ] {
                let ms = spec
                    .split(',')
                    .filter_map(|p| p.split_once(':'))
                    .find(|(name, _)| *name == point)
                    .and_then(|(_, ms)| ms.parse().ok())
                    .unwrap_or(0);
                hook.store(ms, Ordering::SeqCst);
            }
        }

        #[cfg(not(debug_assertions))]
        pub fn load() {}

        pub fn pause(hook: &AtomicU32, marker: &[u8]) {
            let ms = hook.load(Ordering::SeqCst);
            if cfg!(debug_assertions) && ms > 0 {
                let mut t = libc::timespec {
                    tv_sec: (ms / 1000) as libc::time_t,
                    tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
                };
                let mut left = t;
                // SAFETY: write and nanosleep are async-signal-safe; the marker is a static byte
                // string. An interrupted sleep sleeps the rest.
                unsafe {
                    libc::write(1, marker.as_ptr().cast(), marker.len());
                    while libc::nanosleep(&t, &mut left) != 0 {
                        t = left;
                    }
                }
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn errno() -> *mut c_int {
        // SAFETY: no preconditions; this thread's errno.
        unsafe { libc::__errno_location() }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn errno() -> *mut c_int {
        // SAFETY: no preconditions; this thread's errno.
        unsafe { libc::__error() }
    }

    /// The termination handler: hands the signal to the watcher with one non-blocking write
    /// (async-signal-safe) and returns. It never touches the terminal (L17).
    extern "C" fn on_signal(sig: c_int) {
        // SAFETY: this thread's errno, restored for the code the signal interrupted.
        let saved = unsafe { *errno() };
        hooks::pause(&hooks::HANDLER_ENTRY, b"[pause:handler_entry]");
        let byte = sig as u8;
        // SAFETY: write is async-signal-safe; the write end is non-blocking and never closed.
        unsafe {
            libc::write(
                PIPE_W.load(Ordering::SeqCst),
                std::ptr::from_ref(&byte).cast(),
                1,
            );
            *errno() = saved;
        }
    }

    /// Ends the process with `sig` under its default action (never called in a handler).
    fn die(sig: c_int) -> ! {
        // SAFETY: plain signal calls. A raised signal blocked on this thread stays pending until
        // the unblock delivers it; `_exit` only if it somehow did not end the process.
        unsafe {
            libc::sigaction(sig, &action(libc::SIG_DFL), std::ptr::null_mut());
            libc::raise(sig);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &signal_set(&[sig]), std::ptr::null_mut());
            libc::_exit(128 + sig)
        }
    }

    /// Creates, once per process, the self-pipe and the watcher thread that takes the
    /// handlers' signals to the prompt's owner.
    fn start_watcher() -> bool {
        static STARTED: OnceLock<bool> = OnceLock::new();
        *STARTED.get_or_init(|| {
            let mut fds = [-1; 2];
            // SAFETY: an array for two fds; the fds are then set up (close-on-exec; the write
            // end non-blocking, so a handler never blocks) and never closed.
            unsafe {
                if libc::pipe(fds.as_mut_ptr()) != 0 {
                    return false;
                }
                libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
            }
            PIPE_W.store(fds[1], Ordering::SeqCst);
            std::thread::Builder::new()
                .name("hostler-prompt-signals".into())
                .spawn(move || watch(fds[0]))
                .is_ok()
        })
    }

    /// Waits for the first signal a handler wrote. The owner gets it and ends the process once
    /// the terminal is restored; an owner that has not done so within `OWNER_GRACE` (or that
    /// holds the registry that long) is wedged, and the process dies without the terminal being
    /// touched (it may then stay hidden). With no owner the terminal is restored, no new prompt
    /// is admitted (`dying`), and the process dies at once.
    fn watch(fd: c_int) {
        // SAFETY: this thread's mask only: no handler runs here, it dies only on purpose.
        unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, &signal_set(&SIGNALS), std::ptr::null_mut())
        };
        let mut byte = 0u8;
        loop {
            // SAFETY: one byte from the pipe's read end into `byte`.
            match unsafe { libc::read(fd, std::ptr::from_mut(&mut byte).cast(), 1) } {
                1 => break,
                -1 if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted => {}
                _ => return,
            }
        }
        let deadline = Instant::now() + OWNER_GRACE;
        let sig = c_int::from(byte);
        hooks::pause(&hooks::WATCHER_ENTRY, b"[pause:watcher_entry]");
        if hand_over(&OWNER, sig, deadline) {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            commit_dying(&OWNER);
        }
        hooks::pause(&hooks::WATCHER_DECIDED, b"[pause:watcher_decided]");
        die(sig)
    }

    /// `sigaction`; `None` only reads the current action.
    fn sigaction(sig: c_int, new: Option<&libc::sigaction>) -> libc::sigaction {
        // SAFETY: valid pointers to sigaction structs (or null for "no change").
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(
                sig,
                new.map_or(std::ptr::null(), std::ptr::from_ref),
                &mut old,
            );
            old
        }
    }

    fn action(handler: libc::sighandler_t) -> libc::sigaction {
        // SAFETY: a zeroed sigaction is valid; its mask is then set properly.
        let mut a: libc::sigaction = unsafe { std::mem::zeroed() };
        a.sa_sigaction = handler;
        a.sa_mask = signal_set(&SIGNALS);
        a.sa_flags = libc::SA_RESTART;
        a
    }

    /// The open hidden prompt, held by the thread that asked: the only code that changes or
    /// restores the terminal mode. Restores the terminal, then the signal actions, on every
    /// way out (panics too).
    struct Prompt {
        tty: File,
        /// A separate `O_NONBLOCK` open of the terminal for the cosmetic newline: a stopped or
        /// undrained terminal never holds up cleanup or termination (T7-3).
        nb: File,
        saved: libc::termios,
        /// The terminal may be in hidden mode.
        changed: bool,
        old: Vec<(c_int, libc::sigaction)>,
        events: mpsc::Receiver<Event<c_int>>,
    }

    impl Prompt {
        fn restore_terminal(&mut self) {
            if std::mem::take(&mut self.changed) {
                // SAFETY: our open terminal fd and the settings read from it; TCSANOW never
                // waits for output to drain.
                unsafe { libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &self.saved) };
                let _ = (&self.nb).write(b"\n");
            }
        }

        /// A termination signal reached the prompt: restore the terminal, then die of it.
        fn interrupted(&mut self, sig: c_int) -> ! {
            hooks::pause(&hooks::AFTER_SIGNAL, b"[pause:after_signal]");
            self.restore_terminal();
            hooks::pause(&hooks::RESTORED, b"[pause:restored]");
            die(sig)
        }
    }

    impl Drop for Prompt {
        fn drop(&mut self) {
            self.restore_terminal();
            for (sig, old) in &self.old {
                sigaction(*sig, Some(old));
            }
            {
                let mut reg = OWNER.lock().unwrap_or_else(PoisonError::into_inner);
                hooks::pause(&hooks::UNREGISTER, b"[pause:unregister]");
                reg.owner = None;
            }
            // A signal handed over before the owner left still ends the process; the terminal
            // is restored already. Any later one finds no owner and does the same.
            while let Ok(event) = self.events.try_recv() {
                if let Event::Interrupt(sig) = event {
                    die(sig);
                }
            }
        }
    }

    pub(super) fn hidden(
        prompt: &str,
        idle: Duration,
        command: &str,
    ) -> Result<SecretValue, HostlerError> {
        let _one = PROMPT.lock().unwrap_or_else(PoisonError::into_inner);
        hooks::load();
        let refused = || errors::not_interactive(command);
        let tty = open_tty(0).map_err(|_| refused())?;
        let nb = open_tty(libc::O_NONBLOCK).map_err(|_| refused())?;
        let reader = tty.try_clone().map_err(|_| terminal_closed())?;
        let mut saved = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: an open fd and a writable termios.
        if unsafe { libc::tcgetattr(tty.as_raw_fd(), saved.as_mut_ptr()) } != 0 {
            return Err(refused());
        }
        // SAFETY: tcgetattr succeeded and filled it.
        let saved = unsafe { saved.assume_init() };
        if !start_watcher() {
            return Err(refused());
        }
        let (tx, events) = mpsc::channel();
        register(&OWNER, tx.clone())?;
        let mut prompt_guard = Prompt {
            tty,
            nb,
            saved,
            changed: false,
            old: Vec::new(),
            events,
        };
        let ours = action(on_signal as extern "C" fn(c_int) as libc::sighandler_t);
        for sig in SIGNALS {
            let old = sigaction(sig, None);
            // A signal the caller ignores (nohup) stays ignored.
            if old.sa_sigaction != libc::SIG_IGN {
                sigaction(sig, Some(&ours));
                prompt_guard.old.push((sig, old));
            }
        }
        let old_tstp = sigaction(libc::SIGTSTP, Some(&action(libc::SIG_IGN)));
        prompt_guard.old.push((libc::SIGTSTP, old_tstp));

        hooks::pause(&hooks::BEFORE_MODE, b"[pause:before_mode]");
        let mut raw = saved;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN);
        raw.c_lflag |= libc::ISIG;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        prompt_guard.changed = true;
        let fd = prompt_guard.tty.as_raw_fd();
        // SAFETY: our open terminal fd and a complete termios. Non-blocking calls only (TCSAFLUSH
        // would wait for output to drain): tcflush drops typed-ahead input, then TCSANOW.
        let applied = unsafe {
            libc::tcflush(fd, libc::TCIFLUSH);
            libc::tcsetattr(fd, libc::TCSANOW, &raw) == 0
        };
        if !applied {
            return Err(refused());
        }
        hooks::pause(&hooks::AFTER_MODE, b"[pause:after_mode]");
        let keys = EditKeys {
            erase: saved.c_cc[libc::VERASE],
            kill: saved.c_cc[libc::VKILL],
            werase: saved.c_cc[libc::VWERASE],
            eof: saved.c_cc[libc::VEOF],
        };
        let text = format!("{prompt} ");
        std::thread::spawn(move || {
            let _ = tx.send(Event::Answer(read_value(reader, &text, keys)));
        });
        match wait_event(&prompt_guard.events, idle) {
            Ok(result) => {
                hooks::pause(&hooks::AFTER_ANSWER, b"[pause:after_answer]");
                result
            }
            Err(sig) => prompt_guard.interrupted(sig),
        }
    }

    /// Writes the prompt and reads chunks (never touching the settings) until the editor is
    /// done. The prompt is written here, inside the timed wait: an undrained terminal cannot
    /// hold the prompt up past its bound (T7-3).
    fn read_value(
        mut tty: File,
        prompt: &str,
        keys: EditKeys,
    ) -> Result<SecretValue, HostlerError> {
        tty.write_all(prompt.as_bytes())
            .map_err(|_| terminal_closed())?;
        let mut editor = LineEditor::new(keys);
        let mut chunk = Zeroizing::new([0u8; 4096]);
        loop {
            let n = match tty.read(&mut chunk[..]) {
                Ok(0) => return Err(terminal_closed()),
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(terminal_closed()),
            };
            for &byte in &chunk[..n] {
                if let Edit::Done(result) = editor.feed(byte) {
                    return result;
                }
            }
        }
    }

    /// One canonical line from the terminal, as it is set (echo on); at most
    /// `MAX_LINE_BYTES + 1` bytes are kept. The prompt is written inside the timed wait.
    pub(super) fn line(
        prompt: &str,
        idle: Duration,
        command: &str,
    ) -> Result<Vec<u8>, HostlerError> {
        let tty = open_tty(0).map_err(|_| errors::not_interactive(command))?;
        let text = format!("{prompt} ");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_line(tty, &text));
        });
        answer(&rx, idle)
    }

    fn read_line(mut tty: File, prompt: &str) -> Result<Vec<u8>, HostlerError> {
        tty.write_all(prompt.as_bytes())
            .map_err(|_| terminal_closed())?;
        let mut line = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = match tty.read(&mut chunk) {
                // Ctrl+D on an empty line.
                Ok(0) => return Ok(line),
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(terminal_closed()),
            };
            let end = chunk[..n].iter().position(|&b| b == b'\n');
            line.extend_from_slice(&chunk[..end.unwrap_or(n)]);
            line.truncate(MAX_LINE_BYTES + 1);
            if end.is_some() {
                return Ok(line);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::{Mutex, PoisonError, mpsc};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        ERROR_OPERATION_ABORTED, FALSE, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, SetLastError, TRUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode, GetStdHandle,
        ReadConsoleW, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, SetConsoleCtrlHandler,
        SetConsoleMode, WriteConsoleW,
    };
    use windows_sys::core::BOOL;

    use super::{
        Console, ConsoleRead, EditKeys, Event, ModeGuard, OWNER_GRACE, Probe, Registry, Reply,
        begin, commit_dying, hand_over, read_line_from, read_value_from, register,
        settle_interrupt, terminal_closed, wait_event,
    };
    use crate::contract::HostlerError;
    use crate::contract::secrets::errors;
    use crate::secrets::value::SecretValue;

    /// Enter is `\r`; Backspace `\x08`; Ctrl+U clears; Ctrl+Z on an empty line is "no value".
    const KEYS: EditKeys = EditKeys {
        erase: 0x08,
        kill: 0x15,
        werase: 0x17,
        eof: 0x1a,
    };

    /// The prompt a control callback hands its event to, while there is one.
    static OWNER: Mutex<Registry<Interrupt>> = Mutex::new(Registry::new());
    /// An interruption: the control callback's event, with the sender for the owner's
    /// acknowledgement, or `None` from the reader (its read was aborted by Ctrl+C, W-7).
    type Interrupt = Option<mpsc::Sender<()>>;
    type Events = mpsc::Sender<Event<Interrupt>>;
    static PROMPT: Mutex<()> = Mutex::new(());

    pub(super) fn probe() -> Probe {
        Probe {
            stdin_tty: is_console(STD_INPUT_HANDLE),
            stderr_tty: is_console(STD_ERROR_HANDLE),
            terminal_opens: open("CONIN$").is_ok(),
        }
    }

    fn is_console(which: STD_HANDLE) -> bool {
        let mut mode = 0;
        // SAFETY: GetStdHandle has no preconditions; GetConsoleMode fails on a non-console.
        unsafe { GetConsoleMode(GetStdHandle(which), &mut mode) != 0 }
    }

    fn open(name: &str) -> std::io::Result<OwnedHandle> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a NUL-terminated name; the result is checked.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a handle CreateFileW just opened, owned here only.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn mode(handle: &OwnedHandle) -> Option<u32> {
        let mut mode = 0;
        // SAFETY: an open console handle and a writable mode.
        (unsafe { GetConsoleMode(handle.as_raw_handle(), &mut mode) } != 0).then_some(mode)
    }

    fn set_mode(handle: HANDLE, mode: u32) -> bool {
        // SAFETY: an open console input handle.
        unsafe { SetConsoleMode(handle, mode) != 0 }
    }

    fn write(conout: &OwnedHandle, text: &str) -> bool {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut written = 0;
        // SAFETY: an open console output handle and a buffer of `wide.len()` units.
        unsafe {
            WriteConsoleW(
                conout.as_raw_handle(),
                wide.as_ptr(),
                wide.len() as u32,
                &mut written,
                std::ptr::null(),
            ) != 0
        }
    }

    /// Console output blocks while the person selects text: cosmetic output goes through a
    /// thread nobody waits for (T7-3).
    fn write_detached(conout: &OwnedHandle, text: &'static str) {
        if let Ok(conout) = conout.try_clone() {
            std::thread::spawn(move || write(&conout, text));
        }
    }

    /// Ctrl+C / Ctrl+Break / close, on a thread of its own. It never touches the console mode
    /// (L17): it hands the event to the prompt's owner, which restores the mode and exits 130,
    /// and waits (bounded) for the owner's acknowledgement. Without one (no prompt: no new one
    /// is admitted either; or an owner wedged past `OWNER_GRACE`, the registry lock included)
    /// default handling ends the process.
    unsafe extern "system" fn on_ctrl(_ctrl: u32) -> BOOL {
        let deadline = Instant::now() + OWNER_GRACE;
        let (ack, acked) = mpsc::channel();
        if !hand_over(&OWNER, Some(ack), deadline) {
            return FALSE;
        }
        if acked
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
        {
            return TRUE;
        }
        commit_dying(&OWNER);
        FALSE
    }

    /// The console input handle as the mode guard drives it.
    struct Conin(OwnedHandle);

    impl Console for Conin {
        type Reader = OwnedHandle;

        fn mode(&self) -> Option<u32> {
            mode(&self.0)
        }

        fn set_mode(&self, mode: u32) -> bool {
            set_mode(self.0.as_raw_handle(), mode)
        }

        fn reader(&self) -> Option<OwnedHandle> {
            self.0.try_clone().ok()
        }
    }

    /// The registered prompt owner with the control callback installed. On drop: callback
    /// removed, unregistered; an event handed over meanwhile still ends the process (exit 130;
    /// the mode guard, dropped first, has restored the console) — a later callback finds no
    /// owner and returns FALSE.
    struct Owner {
        events: mpsc::Receiver<Event<Interrupt>>,
    }

    impl Owner {
        fn new() -> Result<(Owner, Events), HostlerError> {
            let (tx, events) = mpsc::channel();
            register(&OWNER, tx.clone())?;
            // SAFETY: a valid handler routine.
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), TRUE) };
            Ok((Owner { events }, tx))
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            // SAFETY: removes the callback added in `Owner::new`.
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), FALSE) };
            OWNER.lock().unwrap_or_else(PoisonError::into_inner).owner = None;
            while let Ok(event) = self.events.try_recv() {
                if let Event::Interrupt(ack) = event {
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                    std::process::exit(130);
                }
            }
        }
    }

    /// An open prompt (hidden or visible), held by the thread that asked: the only code that
    /// changes or restores the console mode (L17). Restores it on every way out (panics too).
    struct Session {
        /// Declared before `owner`: the mode is back before the owner leaves.
        mode: ModeGuard<Conin>,
        conout: OwnedHandle,
        /// The hidden prompt ends its line itself (no echo).
        newline: bool,
        owner: Owner,
    }

    impl Session {
        /// Ctrl+C reached the prompt (the callback's event or the reader's aborted read): still
        /// registered, restore the mode, acknowledge the callback's event (waiting for it up to
        /// `OWNER_GRACE` when the read came first), exit 130 — never an answer (W-7).
        fn interrupted(&mut self, first: Interrupt) -> ! {
            settle_interrupt(&mut self.mode, &self.owner.events, first, OWNER_GRACE);
            write_detached(&self.conout, "\r\n");
            std::process::exit(130)
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            self.mode.restore();
            if self.newline {
                write_detached(&self.conout, "\r\n");
            }
        }
    }

    /// One prompt: owner first (a control event during setup waits for the owner's wait), then
    /// the mode change under its guard, then the reader, which writes the prompt inside the
    /// timed wait (T7-3) and reads with `read`.
    fn ask(
        prompt: &str,
        idle: Duration,
        command: &str,
        wanted: impl FnOnce(u32) -> u32,
        newline: bool,
        read: fn(OwnedHandle) -> Reply,
    ) -> Result<SecretValue, HostlerError> {
        let _one = PROMPT.lock().unwrap_or_else(PoisonError::into_inner);
        let refused = || errors::not_interactive(command);
        let conin = open("CONIN$").map_err(|_| refused())?;
        let conout = open("CONOUT$").map_err(|_| refused())?;
        let writer = conout.try_clone().map_err(|_| terminal_closed())?;
        let (owner, tx) = Owner::new()?;
        let (mode, reader) = begin(Conin(conin), wanted, command)?;
        let mut session = Session {
            mode,
            conout,
            newline,
            owner,
        };
        let text = format!("{prompt} ");
        std::thread::spawn(move || {
            let reply = if write(&writer, &text) {
                read(reader)
            } else {
                Some(Err(terminal_closed()))
            };
            let _ = tx.send(reply.map_or(Event::Interrupt(None), Event::Answer));
        });
        match wait_event(&session.owner.events, idle) {
            Ok(result) => result,
            Err(first) => session.interrupted(first),
        }
    }

    /// No line input (no console line buffer, so no length limit), no echo, no VT input;
    /// processed input sends Ctrl+C to the callback instead of the buffer.
    pub(super) fn hidden(
        prompt: &str,
        idle: Duration,
        command: &str,
    ) -> Result<SecretValue, HostlerError> {
        ask(
            prompt,
            idle,
            command,
            |_| ENABLE_PROCESSED_INPUT,
            true,
            read_value,
        )
    }

    /// One `ReadConsoleW`. Debug builds with `HOSTLER_TEST_PROMPT_TRACE=1` print what it returned
    /// to stderr (never the characters).
    fn console_read(conin: &OwnedHandle, buf: &mut [u16]) -> ConsoleRead {
        let mut read = 0u32;
        // SAFETY: an open console input handle and a buffer of `buf.len()` units; the last
        // error is cleared first, as ReadConsoleW may report success with an error set.
        let (ok, error) = unsafe {
            SetLastError(0);
            let ok = ReadConsoleW(
                conin.as_raw_handle(),
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
                std::ptr::null(),
            ) != 0;
            (ok, GetLastError())
        };
        #[cfg(debug_assertions)]
        if std::env::var_os("HOSTLER_TEST_PROMPT_TRACE").is_some() {
            eprintln!("[hostler-trace] ReadConsoleW ok={ok} read={read} error={error}");
        }
        match (ok, read) {
            (false, _) if error == ERROR_OPERATION_ABORTED => ConsoleRead::Aborted,
            (false, _) => ConsoleRead::Failed,
            (true, 0) => ConsoleRead::Aborted,
            (true, n) => ConsoleRead::Units(n as usize),
        }
    }

    fn read_value(conin: OwnedHandle) -> Reply {
        read_value_from(KEYS, |buf| console_read(&conin, buf))
    }

    fn read_line(conin: OwnedHandle) -> Reply {
        read_line_from(|buf| console_read(&conin, buf))
    }

    /// A line with echo, line input and processed input on (added to the mode if missing, under
    /// the same owner protocol and guard as the hidden prompt, CR-4).
    pub(super) fn line(
        prompt: &str,
        idle: Duration,
        command: &str,
    ) -> Result<Vec<u8>, HostlerError> {
        let wanted = |saved| saved | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT;
        let answer = ask(prompt, idle, command, wanted, false, read_line)?;
        Ok(answer.expose().as_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ErrorCode;

    #[test]
    fn decide_requires_all_three() {
        for bits in 0..8u8 {
            let p = Probe {
                stdin_tty: bits & 1 != 0,
                stderr_tty: bits & 2 != 0,
                terminal_opens: bits & 4 != 0,
            };
            assert_eq!(decide(&p), bits == 7, "{p:?}");
        }
        // An agent's shell started from a person's terminal: stdin is a pipe.
        assert!(!decide(&Probe {
            stdin_tty: false,
            stderr_tty: true,
            terminal_opens: true
        }));
    }

    #[test]
    fn test_lines_feed_prompts_in_order() {
        let mut tty = Tty::from_lines(&["y", "pass-1234", "my app"]);
        assert!(tty.confirm("Create? [y/N]").unwrap());
        assert!(tty.hidden("Password:").unwrap().expose() == "pass-1234");
        assert_eq!(tty.line("Name:").unwrap(), "my app");
    }

    #[test]
    fn confirm_accepts_only_y_or_yes() {
        for yes in ["y", "yes", "Y", "YES", " y ", "yes\r"] {
            assert!(Tty::from_lines(&[yes]).confirm("?").unwrap(), "{yes:?}");
        }
        for no in ["", "n", "no", "yep", "yes please", "ok", "1"] {
            assert!(!Tty::from_lines(&[no]).confirm("?").unwrap(), "{no:?}");
        }
        let long = "y".repeat(MAX_LINE_BYTES + 1);
        assert!(!Tty::from_lines(&[&long]).confirm("?").unwrap());
        let err = Tty::from_lines(&[&long]).line("?").unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
    }

    #[test]
    fn confirm_typed_must_match_exactly() {
        let typed = |answer: &str| {
            Tty::from_lines(&[answer])
                .confirm_typed("Type it:", "My App")
                .unwrap()
        };
        assert!(typed("My App"));
        assert!(typed("My App  "));
        assert!(!typed("my app"));
        assert!(!typed(" My App"));
        assert!(!typed("My Ap"));
        assert!(!typed(""));
    }

    #[test]
    fn exhausted_test_input_is_not_interactive() {
        let mut tty = Tty::from_lines(&["y"]);
        tty.confirm("?").unwrap();
        let err = tty.hidden("Password:").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotInteractive);
        assert_eq!(err.message, "no more test input");
    }

    #[cfg(all(unix, debug_assertions))]
    #[test]
    fn idle_prompt_times_out() {
        use std::os::unix::ffi::OsStrExt;
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("tty");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // No writer ever opens the FIFO: reading it blocks like a silent terminal.
        let mut tty = Tty::from_file(fifo, Duration::from_millis(200));
        let started = Instant::now();
        let err = tty.hidden("Password:").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotInteractive);
        assert!(err.message.starts_with("no answer at the terminal within"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn hidden_value_is_redacted_in_debug() {
        let value = Tty::from_lines(&["hunter2-canary"])
            .hidden("Password:")
            .unwrap();
        assert_eq!(format!("{value:?}"), "[redacted]");
        assert!(value.expose() == "hunter2-canary");
    }

    /// A console whose mode is a cell; setting the mode or cloning the reader can fail.
    struct FakeConsole {
        mode: std::rc::Rc<std::cell::Cell<u32>>,
        sets: std::rc::Rc<std::cell::Cell<u32>>,
        set_fails: bool,
        reader_fails: bool,
    }

    impl Console for FakeConsole {
        type Reader = ();

        fn mode(&self) -> Option<u32> {
            Some(self.mode.get())
        }

        fn set_mode(&self, mode: u32) -> bool {
            self.sets.set(self.sets.get() + 1);
            if self.set_fails {
                return false;
            }
            self.mode.set(mode);
            true
        }

        fn reader(&self) -> Option<()> {
            (!self.reader_fails).then_some(())
        }
    }

    /// CR-4: the mode guard exists before the first change; an error after it, a failed change
    /// and the end of the prompt all bring the saved (non-default) mode back.
    #[test]
    fn console_mode_is_restored_on_every_way_out() {
        use std::cell::Cell;
        use std::rc::Rc;
        const SAVED: u32 = 0x0010; // e.g. a console with line input and echo cleared
        let fake = |set_fails, reader_fails| {
            let (mode, sets) = (Rc::new(Cell::new(SAVED)), Rc::new(Cell::new(0)));
            let console = FakeConsole {
                mode: mode.clone(),
                sets: sets.clone(),
                set_fails,
                reader_fails,
            };
            (console, mode, sets)
        };
        let wanted = |m| m | 0x7;

        // The reader's handle cannot be taken after the mode changed.
        let (console, mode, _) = fake(false, true);
        let err = begin(console, wanted, "test").err().unwrap();
        assert_eq!(err.code, ErrorCode::NotInteractive);
        assert_eq!(mode.get(), SAVED);

        // The change itself fails.
        let (console, mode, _) = fake(true, false);
        assert!(begin(console, wanted, "test").is_err());
        assert_eq!(mode.get(), SAVED);

        // Changed for the prompt; restored once on restore, not again on drop.
        let (console, mode, sets) = fake(false, false);
        let (mut guard, ()) = begin(console, wanted, "test").unwrap();
        assert_eq!(mode.get(), SAVED | 0x7);
        guard.restore();
        assert_eq!(mode.get(), SAVED);
        drop(guard);
        assert_eq!(sets.get(), 2);

        // Dropped (an error or a panic later in the prompt) → restored.
        let (console, mode, _) = fake(false, false);
        drop(begin(console, wanted, "test").unwrap());
        assert_eq!(mode.get(), SAVED);

        // The console already has what the prompt needs: never touched.
        let (console, mode, sets) = fake(false, false);
        drop(begin(console, |m| m, "test").unwrap());
        assert_eq!((mode.get(), sets.get()), (SAVED, 0));
    }

    /// Scripted console reads: text arrives as UTF-16 units; `None` is an aborted read (what
    /// `ReadConsoleW` returns on Ctrl+C); `Some("")` stands for a failed read.
    fn reads(script: &[Option<&str>]) -> impl FnMut(&mut [u16]) -> ConsoleRead {
        let mut script: VecDeque<Option<String>> =
            script.iter().map(|r| r.map(str::to_owned)).collect();
        move |buf| match script.pop_front().expect("no read left in the script") {
            None => ConsoleRead::Aborted,
            Some(text) if text.is_empty() => ConsoleRead::Failed,
            Some(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                buf[..units.len()].copy_from_slice(&units);
                ConsoleRead::Units(units.len())
            }
        }
    }

    /// W-7: Ctrl+C makes the console read end with nothing; that is an interruption, never an
    /// (empty, i.e. "no") answer — at the visible line and at the hidden prompt.
    #[test]
    fn an_aborted_console_read_is_never_an_answer() {
        let keys = EditKeys {
            erase: 0x08,
            ..KEYS
        };
        assert!(read_line_from(reads(&[None])).is_none());
        assert!(read_line_from(reads(&[Some("my a"), None])).is_none());
        let line = read_line_from(reads(&[Some("my a"), Some("pp\r\n")]));
        assert_eq!(line.unwrap().unwrap().expose(), "my app\r");
        let failed = read_line_from(reads(&[Some("")])).unwrap().unwrap_err();
        assert_eq!(failed.code, ErrorCode::NotInteractive);

        assert!(read_value_from(keys, reads(&[None])).is_none());
        assert!(read_value_from(keys, reads(&[Some("ab"), None])).is_none());
        let value = read_value_from(keys, reads(&[Some("ab"), Some("c\r")]));
        assert_eq!(value.unwrap().unwrap().expose(), "abc");
    }

    /// W-7: the owner's way out of an interrupted prompt restores the mode, then acknowledges
    /// the control callback's event (waiting for it when the aborted read came first, ignoring
    /// anything else), and gives up waiting after the grace.
    #[test]
    fn an_interrupted_prompt_restores_then_acknowledges_the_callback() {
        use std::cell::Cell;
        use std::rc::Rc;
        use std::time::Instant;
        const SAVED: u32 = 0x0010;
        let guard = || {
            let mode = Rc::new(Cell::new(SAVED));
            let console = FakeConsole {
                mode: mode.clone(),
                sets: Rc::new(Cell::new(0)),
                set_fails: false,
                reader_fails: false,
            };
            let (guard, ()) = begin(console, |m| m | 0x7, "test").unwrap();
            assert_eq!(mode.get(), SAVED | 0x7);
            (guard, mode)
        };
        let grace = Duration::from_secs(5);

        // The aborted read came first; the callback's event is (or arrives) in the queue.
        let (mut mode_guard, mode) = guard();
        let (tx, events) = mpsc::channel();
        let (ack, acked) = mpsc::channel();
        tx.send(Event::Answer(Err(terminal_closed()))).unwrap();
        tx.send(Event::Interrupt(None)).unwrap();
        tx.send(Event::Interrupt(Some(ack))).unwrap();
        let started = Instant::now();
        settle_interrupt(&mut mode_guard, &events, None, grace);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(mode.get(), SAVED);
        assert!(acked.try_recv().is_ok());

        // The callback's event came first: acknowledged at once.
        let (mut mode_guard, mode) = guard();
        let (ack, acked) = mpsc::channel();
        let started = Instant::now();
        settle_interrupt(&mut mode_guard, &events, Some(ack), grace);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(mode.get(), SAVED);
        assert!(acked.try_recv().is_ok());

        // No callback event in time: restored, then the wait ends after the grace.
        let (mut mode_guard, mode) = guard();
        let started = Instant::now();
        settle_interrupt(&mut mode_guard, &events, None, Duration::from_millis(100));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert_eq!(mode.get(), SAVED);
        drop(tx);
    }

    const KEYS: EditKeys = EditKeys {
        erase: 0x7f,
        kill: 0x15,
        werase: 0x17,
        eof: 0x04,
    };

    /// Feeds `input`; the first `Done` (or `None` when the editor wants more).
    fn edit(input: &[u8]) -> Option<Result<SecretValue, HostlerError>> {
        let mut editor = LineEditor::new(KEYS);
        for &b in input {
            if let Edit::Done(result) = editor.feed(b) {
                return Some(result);
            }
        }
        None
    }

    fn value(input: &[u8]) -> String {
        edit(input)
            .expect("the edit should be done")
            .expect("a value")
            .expose()
            .to_owned()
    }

    fn refused(input: &[u8]) -> String {
        let e = edit(input).expect("done").unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        e.message
    }

    #[test]
    fn line_editor_rules() {
        assert_eq!(value(b"abcd\r"), "abcd");
        assert_eq!(value(b"abcd\n"), "abcd");
        assert!(edit(b"abcd").is_none());
        // erase: the key, DEL and BS; a whole UTF-8 character at a time.
        assert_eq!(value(b"abX\x7fc\r"), "abc");
        assert_eq!(value(b"abX\x08c\r"), "abc");
        assert_eq!(value("xé\x7f\r".as_bytes()), "x");
        assert_eq!(value("x😀\x7fy\r".as_bytes()), "xy");
        assert_eq!(value(b"\x7f\x7fab\r"), "ab");
        let mut custom = LineEditor::new(EditKeys {
            erase: 0x08,
            ..KEYS
        });
        for &b in b"ab\x08" {
            assert!(matches!(custom.feed(b), Edit::More));
        }
        // kill and werase
        assert_eq!(value(b"junk\x15wxyz\r"), "wxyz");
        assert_eq!(value(b"one two\x17\r"), "one ");
        assert_eq!(value(b"one two  \x17\r"), "one ");
        assert_eq!(value(b"one\x17\x17x\r"), "x");
        // eof: "no value" on an empty line, ignored otherwise.
        assert_eq!(refused(b"\x04"), "no value entered");
        assert_eq!(value(b"ab\x04cd\r"), "abcd");
        assert_eq!(refused(b"ab\x15\x04"), "no value entered");
        // Escape sequences are skipped: arrows, SS3 keys, bracketed-paste markers, Alt+key.
        assert_eq!(value(b"ab\x1b[Acd\r"), "abcd");
        assert_eq!(value(b"ab\x1bOPcd\r"), "abcd");
        assert_eq!(value(b"\x1b[200~pasted\x1b[201~\r"), "pasted");
        assert_eq!(value(b"a\x1bxb\r"), "ab");
        assert_eq!(value(b"a\x1b[1;5Cb\r"), "ab");
        // A control byte ends a broken sequence and keeps its meaning.
        assert_eq!(value(b"ab\x1b[12\r"), "ab");
        // Other control bytes are ignored.
        assert_eq!(value(b"a\x01b\tc\x00d\r"), "abcd");
        // Not UTF-8 → refused.
        assert_eq!(refused(b"ab\xff\xfe\r"), "the value is not UTF-8 text");
        // The 64 KiB boundary: never cut.
        let mut exact = vec![b'x'; MAX_VALUE_BYTES];
        exact.push(b'\r');
        assert_eq!(value(&exact).len(), MAX_VALUE_BYTES);
        let mut over = vec![b'x'; MAX_VALUE_BYTES + 1];
        over.push(b'\r');
        assert_eq!(
            refused(&over),
            "the value is longer than 64 KiB; nothing was taken"
        );
        // Erasing after the limit does not bring the value back.
        let mut over_then_erase = vec![b'x'; MAX_VALUE_BYTES + 1];
        over_then_erase.extend_from_slice(b"\x7f\x7f\r");
        assert_eq!(
            refused(&over_then_erase),
            "the value is longer than 64 KiB; nothing was taken"
        );
    }
}
