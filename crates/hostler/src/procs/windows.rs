//! Windows: the registry `BootId`, `GetProcessTimes`, a Toolhelp32 snapshot and job objects.

use std::ffi::c_void;
use std::mem::size_of;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
    ERROR_INVALID_PARAMETER, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, FILETIME, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenSessionId};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    IsProcessInJob, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
    OpenJobObjectW, QueryInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};

use super::{JobState, ProcInfo};

/// From `<winnt.h>`; windows-sys has them only behind the large `Win32_System_SystemServices`
/// feature.
const JOB_OBJECT_QUERY: u32 = 0x0004;
const JOB_OBJECT_TERMINATE: u32 = 0x0008;
/// How long a terminated job or process gets to be empty or gone.
const KILL_WAIT_MS: u32 = 5_000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Closes the handle on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by the OS and is closed exactly once, here.
        unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn boot_id() -> anyhow::Result<String> {
    let key = wide(
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters",
    );
    let value = wide("BootId");
    let mut data: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: NUL-terminated key and value names; `data` is writable for `size` (4) bytes and
    // RRF_RT_REG_DWORD makes the call fail rather than write anything that is not a DWORD.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast::<c_void>(),
            &mut size,
        )
    };
    anyhow::ensure!(
        rc == ERROR_SUCCESS,
        "cannot read the BootId registry value (error {rc})"
    );
    Ok(data.to_string())
}

fn filetime_u64(t: &FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// The creation time of the process behind `process`, as `ProcessIdentity::start_time` holds it.
fn created_on(process: &Handle) -> Option<u64> {
    let mut created = FILETIME::default();
    let (mut exit_time, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: a valid process handle and four writable FILETIME out-parameters.
    let ok = unsafe {
        GetProcessTimes(
            process.0,
            &mut created,
            &mut exit_time,
            &mut kernel,
            &mut user,
        )
    } != 0;
    ok.then(|| filetime_u64(&created))
}

/// `OpenProcess` for any process: opened or access denied, it exists; an invalid parameter is
/// no such pid. Pid 0 (the idle process) cannot be told.
pub(super) fn pid_exists(pid: u32) -> Option<bool> {
    if pid == 0 {
        return None;
    }
    // SAFETY: OpenProcess has no memory preconditions; a null result is checked below.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if !raw.is_null() {
        drop(Handle(raw));
        return Some(true);
    }
    // SAFETY: reads this thread's last error, set by the failed OpenProcess.
    match unsafe { GetLastError() } {
        ERROR_INVALID_PARAMETER => Some(false),
        ERROR_ACCESS_DENIED => Some(true),
        _ => None,
    }
}

pub(super) fn info(pid: u32) -> Option<ProcInfo> {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    // SAFETY: OpenProcess has no memory preconditions; a null result is checked below.
    let raw = unsafe { OpenProcess(access, 0, pid) };
    if raw.is_null() {
        return None;
    }
    let process = Handle(raw);
    let mut created = FILETIME::default();
    let (mut exit_time, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: a valid process handle and four writable FILETIME out-parameters.
    let times_ok = unsafe {
        GetProcessTimes(
            process.0,
            &mut created,
            &mut exit_time,
            &mut kernel,
            &mut user,
        )
    } != 0;
    // An exited process stays openable while someone holds a handle to it: Windows' zombie.
    // Its handle is signaled; the exit code cannot tell (a process may exit with 259).
    // SAFETY: a valid process handle opened with SYNCHRONIZE; a zero timeout never blocks.
    let exited = match unsafe { WaitForSingleObject(process.0, 0) } {
        WAIT_TIMEOUT => Some(false),
        WAIT_OBJECT_0 => Some(true),
        // WAIT_FAILED: the state is unknown, so the start time counts as unreadable (= dead).
        _ => None,
    };
    Some(ProcInfo {
        pid,
        // Costly here (a full process snapshot); `parent_map` serves ancestry walks.
        parent: None,
        start_time: (times_ok && exited.is_some()).then(|| filetime_u64(&created)),
        zombie: exited == Some(true),
    })
}

/// The executable's file name, via `QueryFullProcessImageNameW` (the file name alone, not the
/// path, as the binding allowlist compares them). The first 1024-unit buffer is enough for a
/// normal path; a longer one retries at the 32768-unit maximum instead of failing.
pub(super) fn process_name(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess has no memory preconditions; a null result is checked below.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return None;
    }
    let process = Handle(raw);
    for capacity in [1024usize, 32_768] {
        let mut buf = vec![0u16; capacity];
        let mut size = capacity as u32;
        // SAFETY: a valid process handle; `buf` is writable for `size` UTF-16 units.
        let ok =
            unsafe { QueryFullProcessImageNameW(process.0, 0, buf.as_mut_ptr(), &mut size) } != 0;
        if ok {
            if size == 0 {
                return None;
            }
            buf.truncate(size as usize);
            let path = String::from_utf16(&buf).ok()?;
            return path
                .rsplit(['\\', '/'])
                .next()
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
        }
        // SAFETY: no preconditions; read right after the failed call.
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
    }
    None
}

/// L46: pid → (parent pid, creation time) for every process, from one
/// `NtQuerySystemInformation(SystemProcessInformation)` call; no process is opened, so an
/// elevated or another user's process has its start time too. Empty if the call fails.
pub(super) fn lineage_snapshot() -> std::collections::HashMap<u32, (u32, u64)> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    const MAX_BYTES: usize = 64 << 20;
    let mut bytes = 512 << 10;
    while bytes <= MAX_BYTES {
        // u64 words: the entries hold 8-byte fields.
        let mut buf = vec![0u64; bytes / 8];
        let mut returned = 0u32;
        // SAFETY: `buf` is writable for `bytes` bytes, which is the length passed; `returned` is
        // a writable u32.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION,
                buf.as_mut_ptr().cast(),
                bytes as u32,
                &mut returned,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // Processes come and go between calls: some room to spare.
            bytes = (returned as usize).max(bytes) + (256 << 10);
            continue;
        }
        if status < 0 {
            return std::collections::HashMap::new();
        }
        let len = (returned as usize).min(bytes);
        // SAFETY: the first `len` bytes of `buf` were written by the call.
        let raw = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len) };
        return super::parse_process_information(raw);
    }
    std::collections::HashMap::new()
}

/// Every process's parent, from one Toolhelp32 snapshot (empty if it cannot be taken).
pub(super) fn parent_map() -> std::collections::HashMap<u32, u32> {
    let mut parents = std::collections::HashMap::new();
    // SAFETY: no memory preconditions; the result is checked against INVALID_HANDLE_VALUE.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return parents;
    }
    let snapshot = Handle(raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: a valid snapshot handle and an entry whose dwSize is set, as the API requires.
    let mut ok = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while ok {
        if entry.th32ParentProcessID != 0 {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        }
        // SAFETY: as for Process32FirstW.
        ok = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    parents
}

/// `ActiveProcesses` of an open job, or `None` when it cannot be queried.
fn active_processes(job: &Handle) -> Option<u32> {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: a valid job handle; `info` is writable for exactly the size passed.
    let ok = unsafe {
        QueryInformationJobObject(
            job.0,
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast::<c_void>(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    } != 0;
    ok.then_some(info.ActiveProcesses)
}

/// The name job `local_name`, made in logon session `session`, has from session `own` (P3b
/// Task 10, verified on Windows 11): its `Local\` name in its own session; from another, a
/// session-0 job's `Global\` name (the same object) or `Session\<n>\` for any other.
pub(crate) fn session_job_name(local_name: &str, session: u32, own: u32) -> String {
    let rest = local_name.strip_prefix(r"Local\").unwrap_or(local_name);
    match session {
        s if s == own => local_name.to_owned(),
        0 => format!(r"Global\{rest}"),
        s => format!(r"Session\{s}\{rest}"),
    }
}

/// Whether the recorded `session` is known to be another than this process's `own`.
fn elsewhere(session: Option<u32>, own: Option<u32>) -> bool {
    matches!((session, own), (Some(s), Some(o)) if s != o)
}

/// Opens job `name`, made in `session`, from session `own` by its session name; the error code
/// otherwise. A P1-era record (no session) is opened by its name as recorded.
fn open_job(
    name: &str,
    session: Option<u32>,
    own: Option<u32>,
    access: u32,
) -> Result<Handle, u32> {
    let name = match (session, own) {
        (Some(s), Some(o)) => session_job_name(name, s, o),
        _ => name.to_owned(),
    };
    let wide_name = wide(&name);
    // SAFETY: a NUL-terminated name; a null result is checked below.
    let raw = unsafe { OpenJobObjectW(access, 0, wide_name.as_ptr()) };
    if raw.is_null() {
        // SAFETY: no preconditions; read right after the failed call.
        Err(unsafe { GetLastError() })
    } else {
        Ok(Handle(raw))
    }
}

/// No such name: error 2, or 3 for a session without a namespace (one that has ended).
fn not_found(error: u32) -> bool {
    error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND
}

/// An open job: no active process → gone, some → alive, unqueryable → unknown.
fn state_of(job: &Handle) -> JobState {
    match active_processes(job) {
        Some(0) => JobState::Gone,
        Some(_) => JobState::Alive,
        None => JobState::Unknown,
    }
}

/// See `procs::group_state`. "Not found" proves the job gone only in its own session; from
/// another it is `NotFound`, gone only once the launcher is dead too, which the caller decides
/// (`claims::engine::seize`'s `gone`, `procs::stop::stop_group`; P2-11).
pub(super) fn job_state(name: &str, session: Option<u32>) -> JobState {
    job_state_from(name, session, current_session())
}

fn job_state_from(name: &str, session: Option<u32>, own: Option<u32>) -> JobState {
    match open_job(name, session, own, JOB_OBJECT_QUERY) {
        Ok(job) => state_of(&job),
        Err(error) if not_found(error) && session.is_some() && session == own => JobState::Gone,
        Err(error) if not_found(error) && elsewhere(session, own) => JobState::NotFound,
        Err(_) => JobState::Unknown,
    }
}

/// The logon session of this process, from its token.
pub(super) fn current_session() -> Option<u32> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of this process and a writable out-parameter.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Handle(token);
    let mut session: u32 = 0;
    let mut returned: u32 = 0;
    // SAFETY: a valid token handle; `session` is writable for exactly the 4 bytes passed.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenSessionId,
            (&raw mut session).cast::<c_void>(),
            size_of::<u32>() as u32,
            &mut returned,
        )
    } != 0;
    ok.then_some(session)
}

/// What terminating a job or a process came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KillOutcome {
    /// Terminated (or found empty) and empty within 5 s; a process: exited within 5 s.
    Gone,
    /// A job's name is not found: nothing was terminated. In its own session (or a P1-era
    /// record) the job is gone once its launcher is; in another, only once its launcher is
    /// verifiably dead (P2-11). Never a process's answer.
    NotFound,
    AccessDenied,
    /// Still active 5 s after `TerminateJobObject` / `TerminateProcess`.
    StillAlive,
    /// Another error (its code).
    Failed(u32),
}

/// `TerminateJobObject` on job `name`, made in `session`, opened by its session name; then up
/// to 5 s for `ActiveProcesses` to reach 0.
pub fn terminate_job(name: &str, session: Option<u32>) -> KillOutcome {
    terminate_job_from(name, session, current_session())
}

/// `terminate_job` as seen from session `own`.
pub(crate) fn terminate_job_from(
    name: &str,
    session: Option<u32>,
    own: Option<u32>,
) -> KillOutcome {
    let job = match open_job(name, session, own, JOB_OBJECT_TERMINATE | JOB_OBJECT_QUERY) {
        Ok(job) => job,
        Err(error) if not_found(error) => return KillOutcome::NotFound,
        Err(ERROR_ACCESS_DENIED) => return KillOutcome::AccessDenied,
        Err(other) => return KillOutcome::Failed(other),
    };
    // SAFETY: a valid job handle opened with JOB_OBJECT_TERMINATE.
    if unsafe { TerminateJobObject(job.0, 1) } == 0 {
        // SAFETY: as above.
        let error = unsafe { GetLastError() };
        if error == ERROR_ACCESS_DENIED {
            return KillOutcome::AccessDenied;
        }
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(KILL_WAIT_MS.into());
    loop {
        match active_processes(&job) {
            Some(0) => return KillOutcome::Gone,
            _ if std::time::Instant::now() >= deadline => return KillOutcome::StillAlive,
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

/// Whether process `pid` is in job `name` (`IsProcessInJob`); false when either cannot be opened.
pub(super) fn process_in_job(pid: u32, name: &str) -> bool {
    // SAFETY: OpenProcess has no memory preconditions; a null result is checked below.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return false;
    }
    let process = Handle(raw);
    let wide_name = wide(name);
    // SAFETY: a NUL-terminated name; a null result is checked below.
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, wide_name.as_ptr()) };
    if raw.is_null() {
        return false;
    }
    let job = Handle(raw);
    let mut inside: windows_sys::core::BOOL = 0;
    // SAFETY: two valid handles and a writable BOOL.
    let ok = unsafe { IsProcessInJob(process.0, job.0, &mut inside) } != 0;
    ok && inside != 0
}

/// `TerminateProcess` on `p`, then up to 5 s. Its identity is checked on the same handle
/// (T6-5): a pid reused since the caller's check is somebody else's process, not ours.
pub fn terminate_process(p: &super::ProcessIdentity) -> KillOutcome {
    let pid = p.pid;
    let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    // SAFETY: OpenProcess has no memory preconditions; a null result is checked below.
    let raw = unsafe { OpenProcess(access, 0, pid) };
    if raw.is_null() {
        // SAFETY: no preconditions; read right after the failed call.
        return match unsafe { GetLastError() } {
            ERROR_ACCESS_DENIED => KillOutcome::AccessDenied,
            // ERROR_INVALID_PARAMETER: no such process.
            87 => KillOutcome::Gone,
            other => KillOutcome::Failed(other),
        };
    }
    let process = Handle(raw);
    if !super::same_process(created_on(&process), p) {
        return KillOutcome::Gone;
    }
    // SAFETY: a valid process handle opened with PROCESS_TERMINATE.
    if unsafe { TerminateProcess(process.0, 1) } == 0 {
        // SAFETY: as above.
        let error = unsafe { GetLastError() };
        if error == ERROR_ACCESS_DENIED {
            return KillOutcome::AccessDenied;
        }
    }
    // SAFETY: a valid handle opened with SYNCHRONIZE.
    match unsafe { WaitForSingleObject(process.0, KILL_WAIT_MS) } {
        WAIT_OBJECT_0 => KillOutcome::Gone,
        _ => KillOutcome::StillAlive,
    }
}

/// L31: a query handle the service keeps on a run's job from `*.started` until the claim or
/// entry ends. A job's name leaves the `Local\` namespace once its last handle closes, even
/// while members still run (verified on Windows 11), and the wrapper holds the only other one:
/// without this handle a killed wrapper made a live job read as gone. Another session's job is
/// held by its session name (P3b Task 10).
///
/// Residual (documented): a wrapper that dies while the service is down leaves no handle and no
/// name, so its job reads as gone once its launcher has exited, even while members still run.
pub(super) struct HeldJob(Handle);

// SAFETY: a kernel handle may be used and closed from any thread; `Handle` closes it once.
unsafe impl Send for HeldJob {}
// SAFETY: as above; the handle is only read (QueryInformationJobObject) through `&self`.
unsafe impl Sync for HeldJob {}

/// Opens a query handle on job `name`, made in `session`, while its name still exists.
pub(super) fn hold_job(name: &str, session: Option<u32>) -> Option<HeldJob> {
    hold_job_from(name, session, current_session())
}

fn hold_job_from(name: &str, session: Option<u32>, own: Option<u32>) -> Option<HeldJob> {
    open_job(name, session, own, JOB_OBJECT_QUERY)
        .ok()
        .map(HeldJob)
}

pub(super) fn held_job_state(job: &HeldJob) -> JobState {
    state_of(&job.0)
}

/// Whether job `name`, made in `session`, opens and has an active process.
pub(super) fn job_has_active_process(name: &str, session: Option<u32>) -> bool {
    open_job(name, session, current_session(), JOB_OBJECT_QUERY)
        .ok()
        .and_then(|job| active_processes(&job))
        .is_some_and(|n| n > 0)
}

/// A real job for tests (also `cli::takeover`'s).
#[cfg(test)]
pub(crate) mod test_job {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};

    use super::*;

    /// A job of this session with one `ping` in it; `_job` keeps its name alive.
    pub(crate) struct TestJob {
        _job: Handle,
        pub(crate) name: String,
        pub(crate) child: std::process::Child,
    }

    impl Drop for TestJob {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    pub(crate) fn test_job() -> TestJob {
        let name = format!(r"Local\hostler-test-{}", uuid::Uuid::new_v4());
        // SAFETY: default security attributes and a NUL-terminated name.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), wide(&name).as_ptr()) };
        assert!(!raw.is_null());
        let job = Handle(raw);
        let child = std::process::Command::new("ping")
            .args(["-n", "300", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // SAFETY: both handles are open.
        let ok = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
        assert_ne!(ok, 0);
        TestJob {
            _job: job,
            name,
            child,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_job::test_job;
    use super::*;

    fn own() -> u32 {
        current_session().unwrap()
    }

    /// A session that is not this one, from which this session's jobs have their session names.
    fn viewer() -> u32 {
        own() + 1
    }

    fn missing() -> String {
        format!(r"Local\hostler-test-missing-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn session_names_per_session() {
        let local = r"Local\hostler-heavy-x";
        assert_eq!(session_job_name(local, 3, 3), local);
        assert_eq!(session_job_name(local, 0, 0), local);
        assert_eq!(session_job_name(local, 0, 2), r"Global\hostler-heavy-x");
        assert_eq!(session_job_name(local, 1, 0), r"Session\1\hostler-heavy-x");
        assert_eq!(session_job_name(local, 4, 1), r"Session\4\hostler-heavy-x");
    }

    #[test]
    fn own_session_job_reads_alive_then_gone_by_session_name() {
        let mut job = test_job();
        let s = Some(own());
        assert_eq!(job_state(&job.name, s), JobState::Alive);
        // As another session sees it: by its `Global\` or `Session\<n>\` name.
        assert_eq!(
            job_state_from(&job.name, s, Some(viewer())),
            JobState::Alive
        );
        assert!(hold_job_from(&job.name, s, Some(viewer())).is_some());
        assert!(job_has_active_process(&job.name, s));
        // The same `Local\` name made in another session is not this job.
        let made_elsewhere = Some(viewer());
        assert_eq!(
            job_state_from(&job.name, made_elsewhere, s),
            JobState::NotFound
        );
        assert!(hold_job_from(&job.name, made_elsewhere, s).is_none());
        assert!(!job_has_active_process(&job.name, made_elsewhere));
        job.child.kill().unwrap();
        job.child.wait().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while job_state_from(&job.name, s, Some(viewer())) != JobState::Gone
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(job_state_from(&job.name, s, Some(viewer())), JobState::Gone);
        assert_eq!(job_state(&job.name, s), JobState::Gone);
        assert!(!job_has_active_process(&job.name, s));
    }

    #[test]
    fn terminate_job_opens_by_session_name() {
        let mut job = test_job();
        // A job of another session with the same `Local\` name is not this one: left alone.
        assert_eq!(
            terminate_job(&job.name, Some(viewer())),
            KillOutcome::NotFound
        );
        assert!(job.child.try_wait().unwrap().is_none());
        assert_eq!(
            terminate_job_from(&job.name, Some(own()), Some(viewer())),
            KillOutcome::Gone
        );
        assert_eq!(job.child.wait().unwrap().code(), Some(1));
        // P2-11: a missing name terminates nothing, in this session or another.
        assert_eq!(
            terminate_job(&missing(), Some(own())),
            KillOutcome::NotFound
        );
        assert_eq!(
            terminate_job(&missing(), Some(viewer())),
            KillOutcome::NotFound
        );
        // A session with no namespace at all (error 3) is not found either.
        assert_eq!(
            terminate_job(&missing(), Some(own() + 1000)),
            KillOutcome::NotFound
        );
    }

    #[test]
    fn other_session_not_found_is_not_found_here() {
        assert_eq!(job_state(&missing(), Some(own())), JobState::Gone);
        assert_eq!(job_state(&missing(), Some(viewer())), JobState::NotFound);
        assert_eq!(
            job_state(&missing(), Some(own() + 1000)),
            JobState::NotFound
        );
        assert_eq!(job_state(&missing(), None), JobState::Unknown);
    }
}
