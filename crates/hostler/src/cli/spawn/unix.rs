//! Unix: the command starts as `hostler __gate --fd 3 -- <argv…>` in a new process group and
//! blocks on a pipe until the wrapper has registered it; then it `exec`s the command in place
//! (same pid, start time and group). A blocking `pre_exec` cannot do this: std's `spawn` waits
//! for the child's `exec`.

use std::ffi::OsString;
use std::fs::File;
use std::io::{PipeWriter, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use std::task::{Poll, Waker};

use anyhow::Context;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::task::JoinHandle;

use super::{
    CANNOT_RUN, GATE_CLOSED, GATE_INTERRUPTED, GATE_OPEN, GateDecision, TOKEN_ENV, gate_decision,
    read_gate,
};
use crate::procs::{ProcessIdentity, ProcessTable, RunGroup, identity_of};

/// The descriptor the launcher reads its gate from.
const GATE_FD: RawFd = 3;

/// The command behind its gate, in its own process group, with signals forwarded to the group.
/// Needs no process identity, so an unqueued run works where identities cannot be read.
pub struct Launched {
    child: Child,
    gate: Option<PipeWriter>,
    pgid: libc::pid_t,
    /// Our own process group, when the terminal's foreground was handed to the child.
    terminal_back_to: Option<libc::pid_t>,
    forwarder: JoinHandle<()>,
    /// A signal that arrived before the launch: delivered instead of opening the gate.
    pending: Option<libc::c_int>,
}

/// A launched command registered under a heavy entry: its identity and run group.
pub struct Spawned {
    launched: Launched,
    identity: ProcessIdentity,
    group: RunGroup,
}

/// Starts the gate launcher for `argv` in a new process group, with `TOKEN_ENV` set to `token`
/// if given. The command does not run until `release_gate`. A signal `signals` already holds
/// is delivered to the gated group by `release_gate` instead of opening it (the command never
/// starts); later ones are forwarded at once. Needs a tokio runtime.
pub fn launch(
    argv: Vec<OsString>,
    token: Option<&str>,
    mut signals: Signals,
) -> anyhow::Result<Launched> {
    anyhow::ensure!(!argv.is_empty(), "no command to run");
    let exe = std::env::current_exe().context("cannot find the hostler executable")?;
    let (reader, writer) = std::io::pipe().context("cannot create the gate pipe")?;
    let read_fd = reader.as_raw_fd();
    let mut command = Command::new(exe);
    command
        .arg("__gate")
        .arg("--fd")
        .arg(GATE_FD.to_string())
        .arg("--")
        .args(&argv)
        .process_group(0);
    if let Some(token) = token {
        command.env(TOKEN_ENV, token);
    }
    // SAFETY: only dup2 and fcntl, both async-signal-safe, run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            // dup2 onto the same descriptor would keep close-on-exec set.
            let rc = if read_fd == GATE_FD {
                libc::fcntl(GATE_FD, libc::F_SETFD, 0)
            } else {
                libc::dup2(read_fd, GATE_FD)
            };
            if rc == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().context("cannot start the gate launcher")?;
    drop(reader);
    let pgid = child.id() as libc::pid_t;
    let pending = signals.pending();
    Ok(Launched {
        child,
        gate: Some(writer),
        pgid,
        terminal_back_to: None,
        forwarder: tokio::spawn(signals.forward_to(pgid)),
        pending,
    })
}

/// `launch` plus the identity and group the service needs; reads the process table first.
pub fn spawn_in_group(
    argv: Vec<OsString>,
    entry_id: &str,
    signals: Signals,
) -> anyhow::Result<Spawned> {
    let table = super::process_table()?;
    let launched = launch(argv, Some(entry_id), signals)?;
    let pid = launched.child.id();
    let Some(identity) = identity_of(&table, pid) else {
        // The gate closes unopened: the launcher exits without running anything.
        let _ = launched.wait();
        anyhow::bail!("cannot read the start time of the launched process {pid}");
    };
    Ok(Spawned {
        launched,
        identity,
        group: RunGroup::UnixGroup {
            pgid: pid,
            boot_id: table.boot_id(),
        },
    })
}

impl Spawned {
    /// The launcher's identity, which the command keeps after `exec`.
    pub fn identity(&self) -> ProcessIdentity {
        self.identity.clone()
    }

    pub fn group(&self) -> RunGroup {
        self.group.clone()
    }

    pub fn release_gate(&mut self) -> anyhow::Result<()> {
        self.launched.release_gate()
    }

    pub fn wait(self) -> anyhow::Result<ExitStatus> {
        self.launched.wait()
    }

    /// Never opens the gate: the launcher exits 125 without running the command.
    pub fn abandon(self) -> anyhow::Result<ExitStatus> {
        self.launched.wait()
    }
}

impl Launched {
    /// Lets the command run; first hands it the terminal when we are the foreground job.
    pub fn release_gate(&mut self) -> anyhow::Result<()> {
        let Some(mut gate) = self.gate.take() else {
            return Ok(());
        };
        if let Some(sig) = self.pending {
            // SAFETY: killpg has no memory preconditions; the group is our gated launcher.
            unsafe { libc::killpg(self.pgid, sig) };
            anyhow::bail!("interrupted before the command started");
        }
        self.terminal_back_to = give_terminal(self.pgid);
        gate.write_all(&[GATE_OPEN])
            .context("the gate launcher exited before the command could start")
    }

    /// Blocks until the command exits, then takes the terminal back.
    pub fn wait(mut self) -> anyhow::Result<ExitStatus> {
        // A gate never released closes here: the launcher exits without running the command.
        drop(self.gate.take());
        let status = self.child.wait().context("waiting for the command failed");
        if let Some(ours) = self.terminal_back_to {
            set_foreground(ours);
        }
        self.forwarder.abort();
        status
    }
}

/// SIGINT, SIGTERM, SIGHUP and SIGQUIT sent to the wrapper. Registered before the last stop
/// check and owned until the command ends, so none is lost in between: it is seen before the
/// launch or forwarded to the command's whole group.
pub struct Signals {
    int: Signal,
    term: Signal,
    hup: Signal,
    quit: Signal,
}

impl Signals {
    pub fn register() -> std::io::Result<Signals> {
        Ok(Signals {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    /// The first signal already received, if any; never waits.
    fn pending(&mut self) -> Option<libc::c_int> {
        let mut cx = std::task::Context::from_waker(Waker::noop());
        [
            (&mut self.int, libc::SIGINT),
            (&mut self.term, libc::SIGTERM),
            (&mut self.hup, libc::SIGHUP),
            (&mut self.quit, libc::SIGQUIT),
        ]
        .into_iter()
        .find_map(|(s, sig)| matches!(s.poll_recv(&mut cx), Poll::Ready(Some(()))).then_some(sig))
    }

    async fn forward_to(mut self, pgid: libc::pid_t) {
        loop {
            let sig = tokio::select! {
                Some(()) = self.int.recv() => libc::SIGINT,
                Some(()) = self.term.recv() => libc::SIGTERM,
                Some(()) = self.hup.recv() => libc::SIGHUP,
                Some(()) = self.quit.recv() => libc::SIGQUIT,
                else => return,
            };
            // SAFETY: killpg has no memory preconditions; a gone group is not an error here.
            unsafe { libc::killpg(pgid, sig) };
        }
    }
}

/// Makes `pgid` the terminal's foreground group if stdin is our controlling terminal and we are
/// its foreground job; returns our group to restore later.
fn give_terminal(pgid: libc::pid_t) -> Option<libc::pid_t> {
    // SAFETY: isatty, getpgrp and tcgetpgrp only read process and terminal state.
    let ours = unsafe {
        if libc::isatty(libc::STDIN_FILENO) != 1 {
            return None;
        }
        let ours = libc::getpgrp();
        if libc::tcgetpgrp(libc::STDIN_FILENO) != ours {
            return None;
        }
        ours
    };
    set_foreground(pgid).then_some(ours)
}

fn set_foreground(pgid: libc::pid_t) -> bool {
    // SAFETY: SIGTTOU is ignored only around tcsetpgrp (a background group calling it would be
    // stopped) and restored right after; tokio never handles SIGTTOU.
    unsafe {
        let previous = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        let ok = libc::tcsetpgrp(libc::STDIN_FILENO, pgid) == 0;
        libc::signal(libc::SIGTTOU, previous);
        ok
    }
}

/// `hostler __gate --fd <fd> -- <argv…>`: waits for one byte on `fd`, then becomes `argv`.
/// `job` is Windows-only and ignored here.
pub fn gate_main(fd: u64, _job: Option<&str>, argv: &[OsString]) -> i32 {
    let fd = RawFd::try_from(fd).unwrap_or(-1);
    // SAFETY: F_GETFD only checks that the descriptor is open.
    if fd <= 2 || unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        eprintln!("[hostler] __gate: descriptor {fd} is not an open gate");
        return GATE_CLOSED;
    }
    // SAFETY: the descriptor is open (checked above) and owned by nothing else in this process:
    // the wrapper placed the read end of its gate pipe there.
    let mut gate = unsafe { File::from_raw_fd(fd) };
    // Signals reach this gated group directly (the wrapper forwards them): an interrupt ends
    // this process before it can exec.
    let byte = read_gate(&mut gate);
    drop(gate);
    match gate_decision(byte, false) {
        // EOF: the wrapper died before registering the command, which must never run.
        GateDecision::Closed => return GATE_CLOSED,
        GateDecision::Cancelled => return GATE_INTERRUPTED,
        GateDecision::Run => {}
    }
    let Some((program, args)) = argv.split_first() else {
        return GATE_CLOSED;
    };
    let error = Command::new(program).args(args).exec();
    eprintln!(
        "[hostler] cannot run {}: {error}",
        program.to_string_lossy()
    );
    CANNOT_RUN
}
