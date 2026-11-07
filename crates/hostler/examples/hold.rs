//! Test helper for the heavy-queue tests: `hold <file> <ms> [exit code] [grandchild ms]`.
//! Appends `start <ms>` to `<file>`, sleeps, appends `end <ms>` and exits with the code. With a
//! grandchild time it first starts `hold <file>.grandchild <grandchild ms>`, which stays in the
//! same process group and may outlive it.

use std::fs::OpenOptions;
use std::io::Write;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn mark(file: &str, what: &str) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .unwrap();
    writeln!(f, "{what} {ms}").unwrap();
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let file = &args[0];
    let ms: u64 = args[1].parse().unwrap();
    let code: u8 = args.get(2).map_or(0, |c| c.parse().unwrap());
    mark(file, "start");
    if let Some(grandchild_ms) = args.get(3) {
        // Null stdio: a test reading our output must not wait for the grandchild. Never waited
        // for: it is meant to outlive us.
        #[allow(clippy::zombie_processes)]
        Command::new(std::env::current_exe().unwrap())
            .args([&format!("{file}.grandchild"), grandchild_ms])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(ms));
    mark(file, "end");
    ExitCode::from(code)
}
