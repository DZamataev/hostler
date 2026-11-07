//! Test helper for `tests/tty_pty.rs`: asks one hidden value at the terminal and prints
//! `len=<n> sha256=<hex>` to stdout, never the value. On an error it prints `<code>: <message>`
//! to stdout (not to the terminal, which a test may leave undrained) and exits 1.
//!
//! `PROBE_SIGNALS_ON_WORKER=1` (Unix): the main thread masks the termination signals and an
//! idle worker thread takes them, as in a CLI whose runtime workers receive signals.
//! `PROBE_IGNORE_SIGHUP=1` (Unix): SIGHUP is ignored before the prompt, as under `nohup`.
//! `PROBE_GATE=<path>`: a second hidden prompt follows the first, once `<path>` exists.
//! `PROBE_LINE=1`: asks a visible line (`Tty::line`) instead and prints `line=<n>`.

use std::process::ExitCode;

use hostler::cli::tty::Tty;
use sha2::{Digest, Sha256};

fn main() -> ExitCode {
    #[cfg(unix)]
    if std::env::var_os("PROBE_SIGNALS_ON_WORKER").is_some() {
        std::thread::spawn(|| {
            loop {
                std::thread::park();
            }
        });
        // SAFETY: a valid, initialised signal set; only this thread's mask changes.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                libc::sigaddset(&mut set, sig);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
    }
    #[cfg(unix)]
    if std::env::var_os("PROBE_IGNORE_SIGHUP").is_some() {
        // SAFETY: SIG_IGN is a valid disposition.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    }
    let gate = std::env::var_os("PROBE_GATE").map(std::path::PathBuf::from);
    let mut tty = match Tty::open("probe") {
        Ok(tty) => tty,
        Err(e) => return failed(e),
    };
    if std::env::var_os("PROBE_LINE").is_some() {
        return match tty.line("Value:") {
            Ok(line) => {
                println!("line={}", line.len());
                ExitCode::SUCCESS
            }
            Err(e) => failed(e),
        };
    }
    for round in 0..if gate.is_some() { 2 } else { 1 } {
        if round == 1 {
            while !gate.as_ref().is_some_and(|g| g.exists()) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        match tty.hidden("Value:") {
            Ok(value) => {
                let digest = Sha256::digest(value.expose().as_bytes());
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                println!("len={} sha256={hex}", value.len());
            }
            Err(e) => return failed(e),
        }
    }
    ExitCode::SUCCESS
}

fn failed(e: hostler::contract::HostlerError) -> ExitCode {
    let code = serde_json::to_value(e.code).unwrap_or_default();
    println!("{}: {}", code.as_str().unwrap_or("unknown"), e.message);
    ExitCode::FAILURE
}
