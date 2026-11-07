//! Windows: the registry `BootId`, `GetProcessTimes`, a Toolhelp32 snapshot and job objects.

use std::ffi::c_void;
use std::mem::size_of;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation, OpenJobObjectW,
    QueryInformationJobObject,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};

use super::ProcInfo;

/// From `<winnt.h>`; windows-sys has it only behind the large `Win32_System_SystemServices` feature.
const JOB_OBJECT_QUERY: u32 = 0x0004;

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

pub(super) fn job_has_active_process(name: &str) -> bool {
    let name = wide(name);
    // SAFETY: a NUL-terminated name; a null result (no such job, no access) is checked below.
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr()) };
    if raw.is_null() {
        return false;
    }
    let job = Handle(raw);
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
    ok && info.ActiveProcesses > 0
}
