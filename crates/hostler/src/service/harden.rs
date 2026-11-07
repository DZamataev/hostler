//! Core-dump rules for the service and for CLI commands that hold values (plan P2, Task 6, D2):
//! a crash must not write secret values to disk.

/// The service: no core files (Unix `RLIMIT_CORE` 0), not dumpable (Linux), no error dialogs and
/// no heap in error reports (Windows). Failures are logged; the service still serves.
pub fn harden_service_process() {
    #[cfg(unix)]
    {
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid rlimit struct.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) } != 0 {
            tracing::warn!(
                "cannot disable core files: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    not_dumpable();
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        };
        use windows_sys::Win32::System::ErrorReporting::{
            WER_FAULT_REPORTING_FLAG_NOHEAP, WerSetFlags,
        };
        // SAFETY: both calls only change this process's error-reporting flags.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            if WerSetFlags(WER_FAULT_REPORTING_FLAG_NOHEAP) < 0 {
                tracing::warn!("cannot keep the heap out of Windows error reports");
            }
        }
    }
}

/// A CLI command that holds values: not dumpable on Linux (reset by `exec`, so the commands it
/// starts are unaffected; an rlimit would be inherited). Nothing elsewhere.
pub fn harden_value_holder() {
    not_dumpable();
}

fn not_dumpable() {
    #[cfg(target_os = "linux")]
    // SAFETY: PR_SET_DUMPABLE takes one integer argument and only changes this process.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        tracing::warn!(
            "cannot mark the process not dumpable: {}",
            std::io::Error::last_os_error()
        );
    }
}
