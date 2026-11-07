//! Windows security descriptors that admit the current user only (the service pipe; run
//! directories in P2 Task 10).

use std::ffi::c_void;

use interprocess::os::windows::security_descriptor::{
    AsSecurityDescriptorExt, BorrowedSecurityDescriptor, SecurityDescriptor,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `D:P(A;{sddl_flags};GA;;;{user SID})`: P = protected (no inherited ACEs); one ACE, generic
/// all, for this SID only, with the given ACE flags (e.g. `""` for the pipe, `"OICI"` for a
/// directory whose files inherit it).
pub fn user_only_sd(sddl_flags: &str) -> anyhow::Result<SecurityDescriptor> {
    user_only_sd_with_rights(sddl_flags, "GA")
}

/// As [`user_only_sd`] with explicit SDDL rights, e.g. `("OICI", "FA")` for a run directory: an
/// inheritable generic-rights ACE may be stored as two ACEs (an effective one plus an
/// inherit-only one), while `FA` stays one ACE.
pub fn user_only_sd_with_rights(
    sddl_flags: &str,
    rights: &str,
) -> anyhow::Result<SecurityDescriptor> {
    let sid = current_user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;{sddl_flags};{rights};;;{sid})")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated SDDL string and a writable out-pointer; the result is checked.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        anyhow::bail!(
            "cannot build the security descriptor: {}",
            std::io::Error::last_os_error()
        );
    }
    // SAFETY: `sd` is a valid self-relative descriptor until the LocalFree below.
    let owned = unsafe { BorrowedSecurityDescriptor::from_ptr(sd.cast_const()) }.to_owned_sd();
    // SAFETY: `sd` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(sd) };
    Ok(owned?)
}

pub fn current_user_sid() -> anyhow::Result<String> {
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
    // u64 storage keeps TOKEN_USER (which holds pointers) aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` is writable for `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        )
    };
    // SAFETY: `token` was opened above and is closed once.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        anyhow::bail!(
            "cannot read the token user: {}",
            std::io::Error::last_os_error()
        );
    }
    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` points into `buf`, which is alive; `text` receives a LocalAlloc'd string.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        anyhow::bail!(
            "cannot format the user SID: {}",
            std::io::Error::last_os_error()
        );
    }
    // SAFETY: `text` is a NUL-terminated UTF-16 string allocated by the call above.
    let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    // SAFETY: `len` u16s before the terminator are initialized.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}
