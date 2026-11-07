//! Starting a heavy command in its own run group (plan Task 8), behind a gate launcher that runs
//! nothing until the wrapper has registered it: a process group on Unix, a named job object on
//! Windows.

use std::ffi::OsString;
use std::process::ExitStatus;

use crate::procs::SysProcessTable;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{Launched, Signals, Spawned, gate_main, launch, spawn_in_group};
#[cfg(windows)]
pub use windows::{
    Launched, Signals, Spawned, gate_main, launch, let_service_observe_self, spawn_in_group,
};

/// Unix: the service reads `/proc` / `sysctl`, which elevation does not restrict.
#[cfg(unix)]
pub fn let_service_observe_self() {}

/// Names the slot a heavy command holds, so a nested `hostler run --heavy` can find it.
pub const TOKEN_ENV: &str = "HOSTLER_HEAVY_TOKEN";

/// The gate byte that lets the command start, and the one that cancels it.
const GATE_OPEN: u8 = 1;
#[cfg(windows)]
const GATE_CANCEL: u8 = 0;
/// A launcher's exit code: the wrapper died or gave up before opening the gate.
const GATE_CLOSED: i32 = 125;
/// A launcher's exit code: interrupted before the command started (as for SIGINT).
const GATE_INTERRUPTED: i32 = 130;
const CANNOT_RUN: i32 = 127;

#[derive(Debug, PartialEq, Eq)]
enum GateDecision {
    Run,
    Cancelled,
    Closed,
}

/// What a launcher does once its gate read ends. `byte` is `None` at EOF; `interrupted` is
/// whether the launcher itself has seen an interrupt since it started, which wins over an
/// "open" decided by a wrapper that had not seen it yet.
fn gate_decision(byte: Option<u8>, interrupted: bool) -> GateDecision {
    match byte {
        None => GateDecision::Closed,
        Some(GATE_OPEN) if !interrupted => GateDecision::Run,
        Some(_) => GateDecision::Cancelled,
    }
}

/// How long a running command may take to stop by itself after an interrupt it may have missed
/// (it usually got the same console event and may be shutting down cleanly).
#[cfg_attr(not(windows), allow(dead_code))]
const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// What ended one wait of a launcher on its running command.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum Wake {
    Exited,
    Interrupted,
    TimedOut,
}

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The command exited by itself: report its own exit code.
    Exited,
    /// Stopped after an interrupt and the grace: report 130.
    Stopped,
}

/// Supervises a running command. `wait(watch_interrupt, timeout)` blocks until the command
/// exits, an interrupt is recorded (only when watched), or the timeout (`None` = never). After
/// an interrupt the command gets `INTERRUPT_GRACE` to exit by itself; then `stop` ends it.
#[cfg_attr(not(windows), allow(dead_code))]
fn supervise(
    mut wait: impl FnMut(bool, Option<std::time::Duration>) -> Wake,
    stop: impl FnOnce(),
) -> Ended {
    if wait(true, None) != Wake::Interrupted {
        return Ended::Exited;
    }
    if wait(false, Some(INTERRUPT_GRACE)) == Wake::Exited {
        return Ended::Exited;
    }
    stop();
    Ended::Stopped
}

/// Blocks until the gate yields its one byte, or `None` at EOF or on an error.
fn read_gate(gate: &mut impl std::io::Read) -> Option<u8> {
    let mut byte = [0u8; 1];
    loop {
        match gate.read(&mut byte) {
            Ok(1) => return Some(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
}

/// The process table that registration needs. Debug builds fail it on
/// `HOSTLER_TEST_NO_PROCESS_TABLE=1`, as when the boot id is unreadable.
pub fn process_table() -> anyhow::Result<SysProcessTable> {
    #[cfg(debug_assertions)]
    if std::env::var("HOSTLER_TEST_NO_PROCESS_TABLE").as_deref() == Ok("1") {
        anyhow::bail!("the process table is disabled by HOSTLER_TEST_NO_PROCESS_TABLE");
    }
    SysProcessTable::new()
}

/// The exit code a wrapper reports for its child: the child's code, or 128+n for signal n.
pub fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .or_else(|| status.signal().map(|s| 128 + s))
            .unwrap_or(1)
    }
    #[cfg(windows)]
    {
        status.code().unwrap_or(1)
    }
}

/// The program name for messages.
pub fn program(argv: &[OsString]) -> String {
    argv.first()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// While alive, terminal interrupts (Unix SIGINT/SIGQUIT, Windows Ctrl+C) do not kill this
/// process; the child in the same terminal still gets them. Signal handlers, unlike an ignored
/// disposition, are not inherited by children. Needs a tokio runtime.
pub struct IgnoreInterrupts {
    #[cfg(unix)]
    streams: Vec<tokio::signal::unix::Signal>,
    #[cfg(windows)]
    ctrl_c: Option<tokio::signal::windows::CtrlC>,
}

pub fn ignore_interrupts() -> IgnoreInterrupts {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        IgnoreInterrupts {
            streams: [SignalKind::interrupt(), SignalKind::quit()]
                .into_iter()
                .filter_map(|kind| signal(kind).ok())
                .collect(),
        }
    }
    #[cfg(windows)]
    {
        IgnoreInterrupts {
            ctrl_c: tokio::signal::windows::ctrl_c().ok(),
        }
    }
}

impl IgnoreInterrupts {
    /// Hands an interrupt received since registration to `child`, which started too late to get
    /// it from the terminal.
    pub fn pass_pending_to(&mut self, child: &mut std::process::Child) {
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        #[cfg(unix)]
        if self
            .streams
            .iter_mut()
            .any(|s| matches!(s.poll_recv(&mut cx), Poll::Ready(Some(()))))
        {
            // SAFETY: kill has no memory preconditions; `child` is ours and not yet reaped.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        }
        #[cfg(windows)]
        if self
            .ctrl_c
            .as_mut()
            .is_some_and(|c| matches!(c.poll_recv(&mut cx), Poll::Ready(Some(()))))
        {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_runs_only_on_open_without_an_interrupt() {
        assert_eq!(gate_decision(Some(GATE_OPEN), false), GateDecision::Run);
        // An interrupt the launcher saw beats the wrapper's earlier "open".
        assert_eq!(
            gate_decision(Some(GATE_OPEN), true),
            GateDecision::Cancelled
        );
        assert_eq!(gate_decision(Some(0), false), GateDecision::Cancelled);
        assert_eq!(gate_decision(Some(0), true), GateDecision::Cancelled);
        // EOF: the wrapper is gone.
        assert_eq!(gate_decision(None, false), GateDecision::Closed);
        assert_eq!(gate_decision(None, true), GateDecision::Closed);
    }

    /// Runs `supervise` against scripted wake-ups; returns the result, the waits asked for and
    /// whether the command was stopped.
    fn supervised(script: &[Wake]) -> (Ended, Vec<(bool, Option<std::time::Duration>)>, bool) {
        let mut script = script.iter();
        let mut asked = Vec::new();
        let mut stopped = false;
        let ended = supervise(
            |watch, timeout| {
                asked.push((watch, timeout));
                match script.next().unwrap() {
                    Wake::Exited => Wake::Exited,
                    Wake::Interrupted => Wake::Interrupted,
                    Wake::TimedOut => Wake::TimedOut,
                }
            },
            || stopped = true,
        );
        (ended, asked, stopped)
    }

    #[test]
    fn command_that_exits_keeps_its_own_code() {
        assert_eq!(
            supervised(&[Wake::Exited]),
            (Ended::Exited, vec![(true, None)], false)
        );
    }

    #[test]
    fn interrupt_gives_a_grace_then_stops_the_command() {
        let grace = vec![(true, None), (false, Some(INTERRUPT_GRACE))];
        // Exits by itself within the grace: its own exit code, nothing killed.
        assert_eq!(
            supervised(&[Wake::Interrupted, Wake::Exited]),
            (Ended::Exited, grace.clone(), false)
        );
        // Still running after the grace: stopped, exit 130.
        assert_eq!(
            supervised(&[Wake::Interrupted, Wake::TimedOut]),
            (Ended::Stopped, grace, true)
        );
        assert_eq!(INTERRUPT_GRACE, std::time::Duration::from_secs(5));
    }

    #[test]
    fn gate_read_yields_one_byte_or_eof() {
        assert_eq!(read_gate(&mut &[GATE_OPEN, 7][..]), Some(GATE_OPEN));
        assert_eq!(read_gate(&mut &[][..]), None);
    }
}
