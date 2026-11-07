//! Test helper for the heavy-queue and secret-run tests:
//! `hold [print flags] [<file> <ms> [exit code] [grandchild ms]]`.
//! Appends `start <ms>` to `<file>`, sleeps, appends `end <ms>` and exits with the code. With a
//! grandchild time it first starts `hold <file>.grandchild <grandchild ms>`, which stays in the
//! same process group and may outlive it.
//!
//! Print flags, in order, before any file: `--print-env NAME` writes the variable's value,
//! `--print-file-of NAME` the content of the file whose path is in NAME, `--print TEXT` the
//! text, `--echo-stdin` all of stdin, `--fill N` N dots. `--slowly` writes one byte per 50 ms,
//! `--stderr` writes to stderr instead of stdout. With print flags and no file it exits 0
//! after printing.

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

/// Consumes the print flags at the front of `args` and writes what they ask for.
fn print(args: &mut Vec<String>) {
    let mut text: Vec<u8> = Vec::new();
    let (mut slowly, mut stderr) = (false, false);
    while let Some(flag) = args.first().filter(|a| a.starts_with("--")).cloned() {
        args.remove(0);
        match flag.as_str() {
            "--slowly" => slowly = true,
            "--stderr" => stderr = true,
            "--echo-stdin" => {
                std::io::Read::read_to_end(&mut std::io::stdin(), &mut text).unwrap();
            }
            _ => {
                let arg = args.remove(0);
                match flag.as_str() {
                    "--print-env" => text.extend(std::env::var(&arg).unwrap().into_bytes()),
                    "--print-file-of" => {
                        text.extend(std::fs::read(std::env::var(&arg).unwrap()).unwrap())
                    }
                    "--print" => text.extend(arg.into_bytes()),
                    "--fill" => text.extend(std::iter::repeat_n(b'.', arg.parse().unwrap())),
                    other => panic!("unknown flag {other}"),
                }
            }
        }
    }
    let mut out: Box<dyn Write> = if stderr {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    };
    if slowly {
        for byte in text {
            out.write_all(&[byte]).unwrap();
            out.flush().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
    } else {
        out.write_all(&text).unwrap();
    }
    out.flush().unwrap();
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let printed = args.first().is_some_and(|a| a.starts_with("--"));
    print(&mut args);
    if printed && args.is_empty() {
        return ExitCode::SUCCESS;
    }
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
