//! macOS: `sysctl kern.bootsessionuuid`, `proc_pidinfo(PROC_PIDTBSDINFO)` and
//! `proc_listpids(PROC_PGRP_ONLY)`.

use std::ffi::{c_int, c_void};
use std::mem::size_of;

use super::ProcInfo;

/// From `<libproc.h>`; the libc crate does not export it.
const PROC_PGRP_ONLY: u32 = 2;
/// A process group with more members than this is not a run group we care to count exactly.
const MAX_GROUP_PIDS: usize = 1 << 16;

pub(super) fn boot_id() -> anyhow::Result<String> {
    let name = c"kern.bootsessionuuid";
    let mut len: libc::size_t = 0;
    // SAFETY: a NUL-terminated name, null oldp to query the length into `len`, no new value.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        anyhow::bail!(
            "sysctl kern.bootsessionuuid: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` is writable for `len` bytes and `len` tells the kernel that size.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        anyhow::bail!(
            "sysctl kern.bootsessionuuid: {}",
            std::io::Error::last_os_error()
        );
    }
    buf.truncate(len.min(buf.len()));
    let id = String::from_utf8(buf)?;
    let id = id.trim_end_matches('\0').trim();
    anyhow::ensure!(!id.is_empty(), "sysctl kern.bootsessionuuid is empty");
    Ok(id.to_owned())
}

fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = c_int::try_from(pid).ok()?;
    // SAFETY: proc_bsdinfo is plain old data; all-zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as c_int;
    // SAFETY: `info` is writable for `size` bytes; the kernel writes at most `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        )
    };
    // 0 = error (no such process, no permission); a short write is not a full record.
    (written == size).then_some(info)
}

/// The kernel answers ESRCH to `proc_pidinfo` for a zombie (checked on macOS 26), so a zombie
/// usually reads as no process at all; both are dead. The status check covers any other case.
fn is_zombie(info: &libc::proc_bsdinfo) -> bool {
    info.pbi_status == libc::SZOMB
}

pub(super) fn info(pid: u32) -> Option<ProcInfo> {
    let info = bsd_info(pid)?;
    let start_time = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|us| us.checked_add(info.pbi_start_tvusec));
    Some(ProcInfo {
        pid,
        parent: Some(info.pbi_ppid).filter(|&p| p != 0),
        start_time,
        zombie: is_zombie(&info),
    })
}

/// The pids in process group `pgid`. Empty on error: a false "alive" would hold a slot forever.
fn group_pids(pgid: u32) -> Vec<u32> {
    let mut cap = 256;
    loop {
        let mut pids = vec![0 as libc::pid_t; cap];
        let bytes = (cap * size_of::<libc::pid_t>()) as c_int;
        // SAFETY: `pids` is writable for `bytes` bytes; the call returns the bytes it wrote.
        let written = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                pgid,
                pids.as_mut_ptr().cast::<c_void>(),
                bytes,
            )
        };
        if written <= 0 {
            return Vec::new();
        }
        let n = (written as usize / size_of::<libc::pid_t>()).min(cap);
        // A full buffer may have been truncated: retry larger.
        if n == cap && cap < MAX_GROUP_PIDS {
            cap *= 2;
            continue;
        }
        pids.truncate(n);
        return pids
            .into_iter()
            .filter_map(|p| u32::try_from(p).ok())
            .filter(|&p| p != 0)
            .collect();
    }
}

pub(super) fn group_has_live_member(pgid: u32) -> bool {
    group_pids(pgid).into_iter().any(|pid| {
        // Re-check the group: the member may have moved or exited since the listing.
        bsd_info(pid).is_some_and(|i| i.pbi_pgid == pgid && !is_zombie(&i))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn boot_id_is_a_uuid() {
        let id = super::boot_id().unwrap();
        assert_eq!(id.len(), 36, "{id}");
        assert_eq!(id, super::boot_id().unwrap());
    }

    #[test]
    fn own_group_has_live_member() {
        // SAFETY: getpgrp has no preconditions.
        let pgid = unsafe { libc::getpgrp() } as u32;
        assert!(super::group_pids(pgid).contains(&std::process::id()));
        assert!(super::group_has_live_member(pgid));
    }
}
