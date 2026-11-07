//! Windows: like Unix, the command starts behind a gate. The wrapper spawns
//! `hostler __gate --fd <handle> -- <argv…>` and puts it into the named job object
//! `Local\hostler-heavy-<entry id>` (no kill-on-close) while it still waits; the launcher only
//! starts the command (its child, so inside the job, with inherited stdio) after reading one byte
//! from an inherited pipe handle. EOF there means the wrapper is gone: exit 125, nothing runs.
//! The registered identity is the launcher's; the job covers the command and its descendants.
//! While the command runs, a Ctrl+C the launcher records gives it 5 s to exit by itself (it may
//! have missed the console event); then the job is terminated and the launcher exits 130.

use std::ffi::OsString;
use std::fs::File;
use std::io::{PipeWriter, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::task::{Poll, Waker};
use std::time::Duration;

use anyhow::Context;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, SE_KERNEL_OBJECT,
    SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE, PSECURITY_DESCRIPTOR,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, OpenJobObjectW, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
};

use super::{
    CANNOT_RUN, Ended, GATE_CANCEL, GATE_CLOSED, GATE_INTERRUPTED, GATE_OPEN, GateDecision,
    TOKEN_ENV, Wake, gate_decision, read_gate, supervise,
};
use crate::procs::{ProcessIdentity, ProcessTable, RunGroup, identity_of};

/// Every Ctrl+C or Ctrl+Break the launcher has seen since it started.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// A manual-reset event set with `INTERRUPTED`, so a waiting launcher wakes up.
static INTERRUPT_EVENT: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
/// From `<winnt.h>`; windows-sys has them only behind the large `Win32_System_SystemServices`.
const JOB_OBJECT_QUERY: u32 = 0x0004;
const JOB_OBJECT_TERMINATE: u32 = 0x0008;
/// What the same user's service needs to tell the wrapper and the launcher are alive
/// (`procs::windows::info`).
const OBSERVE_PROCESS: u32 = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
/// What the service (query) and an operator's tools (terminate) need on the run's job.
const OBSERVE_JOB: u32 = JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE;

/// Closes the job handle on drop; the job lives on while it has processes.
struct Job(HANDLE);

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: a handle returned open by CreateJobObjectW, closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: a job handle may be used and closed from any thread.
unsafe impl Send for Job {}

/// The gate launcher, before or after its gate opens. The wrapper ignores Ctrl+C while it is
/// held; the command gets it from the console.
pub struct Launched {
    child: Child,
    gate: Option<PipeWriter>,
    signals: Signals,
    /// Ctrl+C arrived before the launch: the gate never opens.
    pending: bool,
}

/// A launched command in the entry's job object, with its identity and run group.
pub struct Spawned {
    launched: Launched,
    _job: Job,
    identity: ProcessIdentity,
    group: RunGroup,
}

/// Ctrl+C, registered before the last stop check and owned until the command ends: one that
/// arrives in between keeps the gate shut instead of being lost.
pub struct Signals {
    ctrl_c: tokio::signal::windows::CtrlC,
}

impl Signals {
    pub fn register() -> std::io::Result<Signals> {
        Ok(Signals {
            ctrl_c: tokio::signal::windows::ctrl_c()?,
        })
    }

    fn pending(&mut self) -> bool {
        let mut cx = std::task::Context::from_waker(Waker::noop());
        matches!(self.ctrl_c.poll_recv(&mut cx), Poll::Ready(Some(())))
    }
}

/// Starts the gate launcher for `argv`, with `TOKEN_ENV` set to `token` if given. The command
/// does not run until `release_gate`. Needs a tokio runtime.
pub fn launch(
    argv: Vec<OsString>,
    token: Option<&str>,
    signals: Signals,
) -> anyhow::Result<Launched> {
    launch_in(argv, token, None, signals)
}

/// `launch`; `job` names the job the launcher will be in, so it can end the command's tree.
fn launch_in(
    argv: Vec<OsString>,
    token: Option<&str>,
    job: Option<&str>,
    mut signals: Signals,
) -> anyhow::Result<Launched> {
    anyhow::ensure!(!argv.is_empty(), "no command to run");
    let exe = std::env::current_exe().context("cannot find the hostler executable")?;
    let (reader, writer) = std::io::pipe().context("cannot create the gate pipe")?;
    let handle = reader.as_raw_handle();
    // Only the read end may reach the launcher; std passes every inheritable handle on.
    // SAFETY: `handle` is the open read end owned by `reader`.
    if unsafe { SetHandleInformation(handle as HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
        == 0
    {
        anyhow::bail!(
            "cannot pass the gate to the launcher: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut command = Command::new(exe);
    command
        .arg("__gate")
        .arg("--fd")
        .arg((handle as usize).to_string());
    if let Some(job) = job {
        command.arg("--job").arg(job);
    }
    command.arg("--").args(&argv);
    if let Some(token) = token {
        command.env(TOKEN_ENV, token);
    }
    let child = command.spawn().context("cannot start the gate launcher")?;
    drop(reader);
    // Our handle has full access; the launcher still waits at its gate.
    observable(
        child.as_raw_handle() as HANDLE,
        OBSERVE_PROCESS,
        "the gate launcher",
    );
    let pending = signals.pending();
    Ok(Launched {
        child,
        gate: Some(writer),
        signals,
        pending,
    })
}

/// `launch`, inside the entry's job, plus the identity and group the service needs; reads the
/// process table first.
pub fn spawn_in_group(
    argv: Vec<OsString>,
    entry_id: &str,
    signals: Signals,
) -> anyhow::Result<Spawned> {
    let table = super::process_table()?;
    let name = format!(r"Local\hostler-heavy-{entry_id}");
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: default security attributes and a NUL-terminated name; null is checked.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), wide.as_ptr()) };
    if raw.is_null() {
        anyhow::bail!(
            "cannot create the job object {name}: {}",
            std::io::Error::last_os_error()
        );
    }
    let job = Job(raw);
    observable(job.0, OBSERVE_JOB, "the job object");
    let launched = launch_in(argv, Some(entry_id), Some(&name), signals)?;
    // The launcher waits at its gate, so nothing has escaped the job yet.
    // SAFETY: both handles are open: the job above, the process owned by `launched.child`.
    let assigned =
        unsafe { AssignProcessToJobObject(job.0, launched.child.as_raw_handle() as HANDLE) };
    if assigned == 0 {
        let error = std::io::Error::last_os_error();
        // The gate closes unopened: the launcher exits without running anything.
        let _ = launched.wait();
        anyhow::bail!("cannot put the command into the job object {name}: {error}");
    }
    let pid = launched.child.id();
    // `child` holds a process handle, so the start time stays readable even after an exit.
    let Some(identity) = identity_of(&table, pid) else {
        let _ = launched.wait();
        anyhow::bail!("cannot read the start time of the launched process {pid}");
    };
    Ok(Spawned {
        launched,
        _job: job,
        identity,
        group: RunGroup::WindowsJob {
            name,
            boot_id: table.boot_id(),
        },
    })
}

impl Spawned {
    /// The launcher's identity; the command runs as its child inside the job.
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
    /// Lets the command start, unless Ctrl+C came at any time since before the launch: then
    /// the launcher is told to exit 130. One arriving after this check is seen by the launcher
    /// itself (`gate_main`).
    pub fn release_gate(&mut self) -> anyhow::Result<()> {
        let Some(mut gate) = self.gate.take() else {
            return Ok(());
        };
        if self.pending || self.signals.pending() {
            let _ = gate.write_all(&[GATE_CANCEL]);
            anyhow::bail!("interrupted before the command started");
        }
        gate.write_all(&[GATE_OPEN])
            .context("the gate launcher exited before the command could start")
    }

    /// Blocks until the launcher (and so the command) exits.
    pub fn wait(mut self) -> anyhow::Result<ExitStatus> {
        // A gate never released closes here: the launcher exits without running the command.
        drop(self.gate.take());
        self.child.wait().context("waiting for the command failed")
    }
}

/// Keeps Ctrl+C from ending the launcher, so it can report the command's exit, and records it.
/// A handler routine, unlike `SetConsoleCtrlHandler(NULL, TRUE)`, is not inherited.
unsafe extern "system" fn note_ctrl(ctrl_type: u32) -> windows_sys::core::BOOL {
    if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
        record_interrupt();
    }
    1
}

/// What the console handler does on Ctrl+C (and the debug hook, to test it).
fn record_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let event = INTERRUPT_EVENT.load(Ordering::SeqCst);
    if !event.is_null() {
        // SAFETY: the event was created by `gate_main` and is never closed.
        unsafe { SetEvent(event) };
    }
}

/// Waits for the command to exit or, if `interrupt` is given, for it to be set.
fn wait_command(child: HANDLE, interrupt: Option<HANDLE>, timeout: Option<Duration>) -> Wake {
    let handles: Vec<HANDLE> = std::iter::once(child).chain(interrupt).collect();
    let ms = timeout.map_or(INFINITE, |t| {
        t.as_millis().min(u128::from(INFINITE - 1)) as u32
    });
    // SAFETY: valid, open handles; with several signaled the lowest index (the exit) wins.
    match unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, ms) } {
        r if r == WAIT_OBJECT_0 + 1 => Wake::Interrupted,
        WAIT_TIMEOUT => Wake::TimedOut,
        // The exit, or a failed wait: fall back to an ordinary blocking wait.
        _ => Wake::Exited,
    }
}

/// Ends the command and everything in the job (this launcher included, with exit 130); without
/// a job, the command alone.
fn stop_command(job: Option<&str>, child: &mut Child) {
    if let Some(job) = job {
        let wide: Vec<u16> = job.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a NUL-terminated name; a null result (no such job) is checked.
        let handle = unsafe { OpenJobObjectW(JOB_OBJECT_TERMINATE, 0, wide.as_ptr()) };
        if !handle.is_null() {
            // SAFETY: an open job handle with terminate access; closed right after.
            unsafe {
                TerminateJobObject(handle, GATE_INTERRUPTED as u32);
                CloseHandle(handle);
            }
        }
    }
    let _ = child.kill();
}

/// `hostler __gate --fd <handle> [--job <name>] -- <argv…>`: waits for one byte on the inherited
/// pipe handle, then runs `argv` as its child and exits with its exit code.
pub fn gate_main(handle: u64, job: Option<&str>, argv: &[OsString]) -> i32 {
    // Manual-reset, unnamed; set by `record_interrupt`. Without it waits only watch the exit.
    // SAFETY: default attributes, no name; a null result is handled by every user.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    INTERRUPT_EVENT.store(event, Ordering::SeqCst);
    // SAFETY: a handler routine with the signature the API requires, added for this process.
    unsafe { SetConsoleCtrlHandler(Some(note_ctrl), 1) };
    if handle == 0 {
        eprintln!("[hostler] __gate: no gate handle");
        return GATE_CLOSED;
    }
    // SAFETY: the wrapper passed the read end of its gate pipe, inherited by this process and
    // owned by nothing else here.
    let mut gate = unsafe { File::from_raw_handle(handle as usize as RawHandle) };
    let byte = read_gate(&mut gate);
    // Closed before the spawn, so the command does not inherit it.
    drop(gate);
    match gate_decision(byte, INTERRUPTED.load(Ordering::SeqCst)) {
        // EOF (or a broken pipe): the wrapper died or gave up; never run the command.
        GateDecision::Closed => return GATE_CLOSED,
        GateDecision::Cancelled => return GATE_INTERRUPTED,
        GateDecision::Run => {}
    }
    let Some((program, args)) = argv.split_first() else {
        return GATE_CLOSED;
    };
    let mut child = match Command::new(program).args(args).spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "[hostler] cannot run {}: {error}",
                program.to_string_lossy()
            );
            return CANNOT_RUN;
        }
    };
    // A Ctrl+C while the command was being created may have missed it: stop it here.
    if INTERRUPTED.load(Ordering::SeqCst) {
        stop_command(job, &mut child);
        let _ = child.wait();
        return GATE_INTERRUPTED;
    }
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_GATE_INTERRUPT_AFTER_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            record_interrupt();
        });
    }
    let process = child.as_raw_handle() as HANDLE;
    let interrupt = (!event.is_null()).then_some(event);
    let ended = supervise(
        |watch, timeout| wait_command(process, interrupt.filter(|_| watch), timeout),
        || stop_command(job, &mut child),
    );
    if ended == Ended::Stopped {
        let _ = child.wait();
        return GATE_INTERRUPTED;
    }
    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "[hostler] waiting for {} failed: {error}",
                program.to_string_lossy()
            );
            1
        }
    }
}

/// Lets the hostler service, which runs as the same user but maybe not elevated, observe this
/// wrapper. Call before the first heavy request names this process as the holder.
pub fn let_service_observe_self() {
    // SAFETY: the pseudo-handle of the current process, which has every access right.
    observable(
        unsafe { GetCurrentProcess() },
        OBSERVE_PROCESS,
        "this process",
    );
}

/// `grant_current_user`, with a warning instead of an error: without it only an elevated run is
/// affected (the service may judge it dead and reap it).
fn observable(handle: HANDLE, access: u32, what: &str) {
    if let Err(e) = grant_current_user(handle, access) {
        eprintln!(
            "[hostler] Warning: cannot let the hostler service observe {what} ({e:#}); an \
             elevated run may be judged gone."
        );
    }
}

/// Adds an ACE granting the current user's SID `access` to the kernel object `handle`. An
/// elevated token's default DACL grants only Administrators and SYSTEM, and in the same user's
/// non-elevated token Administrators is deny-only, so the service could open nothing of an
/// elevated run. Read and synchronize rights are not blocked by the higher integrity label
/// (no-write-up; for processes also no-read-up, which covers neither right here).
fn grant_current_user(handle: HANDLE, access: u32) -> anyhow::Result<()> {
    let user = current_user()?;
    // SAFETY: `TokenUser` filled `user` with a TOKEN_USER whose SID points into `user`.
    let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut old: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: an open handle and writable out-pointers; the descriptor is freed below.
    let rc = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    anyhow::ensure!(rc == 0, "cannot read the access list (error {rc})");
    let _descriptor = Local(descriptor);
    // A null DACL already grants everyone everything; a new list would restrict it.
    if old.is_null() {
        return Ok(());
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        },
    };
    let mut new: *mut ACL = std::ptr::null_mut();
    // SAFETY: one valid entry, `old` alive inside `_descriptor`; `new` is freed below.
    let rc = unsafe { SetEntriesInAclW(1, &entry, old, &mut new) };
    anyhow::ensure!(rc == 0, "cannot extend the access list (error {rc})");
    let _new = Local(new.cast());
    // SAFETY: an open handle with WRITE_DAC (ours or the pseudo-handle) and a valid ACL.
    let rc = unsafe {
        SetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new,
            std::ptr::null(),
        )
    };
    anyhow::ensure!(rc == 0, "cannot write the access list (error {rc})");
    Ok(())
}

/// Memory from LocalAlloc, freed on drop.
struct Local(*mut core::ffi::c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by the security API for us, freed exactly once.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The current token's TOKEN_USER, in u64 storage so its pointers are aligned.
fn current_user() -> anyhow::Result<Vec<u64>> {
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
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` is writable for `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: `token` was opened above and is closed once.
    unsafe { CloseHandle(token) };
    anyhow::ensure!(ok != 0, "cannot read the token user: {error}");
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Security::Authorization::GetEffectiveRightsFromAclW;

    use super::*;

    /// The rights the current user has on `handle` by its DACL.
    fn user_rights(handle: HANDLE) -> u32 {
        let user = current_user().unwrap();
        // SAFETY: as in `grant_current_user`.
        let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: an open handle and writable out-pointers.
        let rc = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(rc, 0);
        let _descriptor = Local(descriptor);
        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        };
        let mut rights = 0;
        // SAFETY: a DACL alive inside `_descriptor` and a valid trustee.
        assert_eq!(
            unsafe { GetEffectiveRightsFromAclW(dacl, &trustee, &mut rights) },
            0
        );
        rights
    }

    #[test]
    fn job_and_process_grant_the_user_what_the_service_needs() {
        // SAFETY: an unnamed job with default security; closed by `Job`.
        let job = Job(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) });
        assert!(!job.0.is_null());
        grant_current_user(job.0, OBSERVE_JOB).unwrap();
        assert_eq!(user_rights(job.0) & OBSERVE_JOB, OBSERVE_JOB);

        let mut child = Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap();
        let process = child.as_raw_handle() as HANDLE;
        grant_current_user(process, OBSERVE_PROCESS).unwrap();
        assert_eq!(user_rights(process) & OBSERVE_PROCESS, OBSERVE_PROCESS);
        child.wait().unwrap();
    }
}
