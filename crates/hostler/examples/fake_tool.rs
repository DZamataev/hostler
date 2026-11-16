//! A fake device tool for the tests (P3b Task 2). `tests/common/fake_tools.rs` copies it to
//! `<dir>/bin/<tool>[.exe]` (`adb`, `emulator`, `xcrun`, `plutil`, `ideviceinfo`, `node`); the
//! name it was started under is the tool it plays.
//!
//! Each call appends one JSON line to `<dir>/calls.log`: `{"tool", "args", "stdin": <bytes>,
//! "stdout": "null"|"pipe"|"file"|"other", "env": [names], "at_ms": <Unix ms at its start>}`.
//! `env` lists which of `ENV_NAMES` it got, never their values. It then answers from `<dir>/script.json`, the first
//! entry whose `tool` matches and whose `args_prefix` starts the args:
//! `[{tool, args_prefix, stdout?, stdout_file?, stderr?, rc?, delay_ms?, once?,
//! spawn_holding_child?, then_write?}]`.
//! - `stdout` is inline text; `stdout_file` a file (relative to `<dir>`).
//! - `spawn_holding_child: <ms>` first starts a grandchild that keeps stdout and stderr open for
//!   that long and appends its pid to `<dir>/held.pids` (a pipe holder, or an adb "server");
//!   with `holder_new_session: true` it starts in its own session, as an adb server does (Unix).
//! - `delay_ms` then sleeps before answering; `once` removes the entry when used.
//! - `then_write: [entries]` replaces the script after answering (so a `close` drops a session).
//! - `json_output_file: <name>` copies `<dir>/<name>` to the path after `--json-output`, as
//!   devicectl writes its answer (P3b Task 5).
//!
//! - `@NOW_S@` in `stdout` or `stdout_file` is replaced by the Unix time in seconds, as a device's
//!   `date +%s` prints it (P3b Task 8's snapshots).
//!
//! No entry: exit 1 with `fake_tool: no script entry`.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The names a device tool may get from hostler, plus two it must never get.
const ENV_NAMES: &[&str] = &[
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "ANDROID_SDK_HOME",
    "ANDROID_USER_HOME",
    "ANDROID_AVD_HOME",
    "ANDROID_EMULATOR_HOME",
    "ANDROID_ADB_SERVER_PORT",
    "ADB_SERVER_SOCKET",
    "AGENT_DEVICE_STATE_DIR",
    "ANDROID_SERIAL",
    "NODE_OPTIONS",
];

fn main() -> ExitCode {
    let mut argv = std::env::args_os();
    let me = PathBuf::from(argv.next().unwrap_or_default());
    let args: Vec<String> = argv.map(|a| a.to_string_lossy().into_owned()).collect();
    if args.first().map(String::as_str) == Some("__hold") {
        let ms = args.get(1).and_then(|m| m.parse().ok()).unwrap_or(0);
        std::thread::sleep(Duration::from_millis(ms));
        return ExitCode::SUCCESS;
    }
    let tool = me
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = me
        .parent()
        .and_then(Path::parent)
        .expect("the fake tool runs from <dir>/bin")
        .to_path_buf();
    let stdout_kind = stdout_kind();
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let env: Vec<&str> = ENV_NAMES
        .iter()
        .copied()
        .filter(|n| std::env::var_os(n).is_some())
        .collect();
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let line = json!({"tool": tool, "args": args, "stdin": input.len(), "stdout": stdout_kind, "env": env, "at_ms": at_ms});
    append(&dir.join("calls.log"), &format!("{line}\n"));

    let Some(entry) = take_entry(&dir, &tool, &args) else {
        eprintln!("fake_tool: no script entry");
        return ExitCode::from(1);
    };
    if let Some(ms) = entry["spawn_holding_child"].as_u64() {
        let mut holder = Command::new(&me);
        holder
            .args(["__hold", &ms.to_string()])
            .stdin(Stdio::null());
        #[cfg(unix)]
        if entry["holder_new_session"].as_bool() == Some(true) {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe and touches no memory.
            unsafe {
                holder.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        // Never waited for: it must outlive this tool, as a pipe holder or a "server" does.
        #[allow(clippy::zombie_processes)]
        let child = holder.spawn().expect("starting the holding child");
        append(&dir.join("held.pids"), &format!("{}\n", child.id()));
    }
    if let Some(ms) = entry["delay_ms"].as_u64() {
        std::thread::sleep(Duration::from_millis(ms));
    }
    let mut out = std::io::stdout().lock();
    if let Some(text) = entry["stdout"].as_str() {
        let _ = out.write_all(with_now(text).as_bytes());
    }
    if let Some(file) = entry["stdout_file"].as_str() {
        let bytes = std::fs::read(dir.join(file)).expect("stdout_file");
        match String::from_utf8(bytes) {
            Ok(text) => {
                let _ = out.write_all(with_now(&text).as_bytes());
            }
            Err(raw) => {
                let _ = out.write_all(raw.as_bytes());
            }
        }
    }
    let _ = out.flush();
    if let Some(text) = entry["stderr"].as_str() {
        let _ = std::io::stderr().write_all(text.as_bytes());
    }
    if let Some(file) = entry["json_output_file"].as_str()
        && let Some(at) = args.iter().position(|a| a == "--json-output")
        && let Some(path) = args.get(at + 1)
    {
        std::fs::copy(dir.join(file), path).expect("json_output_file");
    }
    if let Some(next) = entry.get("then_write").filter(|v| v.is_array()) {
        with_lock(&dir, || write_script(&dir, next));
    }
    ExitCode::from(entry["rc"].as_u64().unwrap_or(0) as u8)
}

/// The first matching entry; a `once` entry is removed from the script under the lock.
fn take_entry(dir: &Path, tool: &str, args: &[String]) -> Option<Value> {
    with_lock(dir, || {
        let path = dir.join("script.json");
        let mut script: Vec<Value> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let at = script.iter().position(|e| {
            e["tool"].as_str() == Some(tool)
                && e["args_prefix"].as_array().is_some_and(|prefix| {
                    prefix.len() <= args.len()
                        && prefix.iter().zip(args).all(|(p, a)| p.as_str() == Some(a))
                })
        })?;
        let entry = script[at].clone();
        if entry["once"].as_bool() == Some(true) {
            script.remove(at);
            write_script(dir, &Value::Array(script));
        }
        Some(entry)
    })
}

fn write_script(dir: &Path, script: &Value) {
    let tmp = dir.join("script.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(script).unwrap()).unwrap();
    std::fs::rename(&tmp, dir.join("script.json")).unwrap();
}

/// Calls run concurrently (lanes): script rewrites are serialized by a lock file.
fn with_lock<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let lock = dir.join("script.lock");
    let until = Instant::now() + Duration::from_secs(10);
    while OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .is_err()
    {
        assert!(
            Instant::now() < until,
            "fake_tool: script.lock held for 10 s"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let result = f();
    let _ = std::fs::remove_file(&lock);
    result
}

/// One `write` per line, in append mode, so concurrent calls never interleave inside a line.
fn append(path: &Path, line: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

#[cfg(unix)]
fn stdout_kind() -> &'static str {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    // SAFETY: fd 1 is open for the whole process; ManuallyDrop never closes it.
    let stdout = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
    let (Ok(meta), Ok(null)) = (stdout.metadata(), std::fs::metadata("/dev/null")) else {
        return "other";
    };
    let kind = meta.file_type();
    if kind.is_char_device() && meta.rdev() == null.rdev() {
        "null"
    } else if kind.is_fifo() {
        "pipe"
    } else if kind.is_file() {
        "file"
    } else {
        "other"
    }
}

#[cfg(windows)]
fn stdout_kind() -> &'static str {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: GetStdHandle and GetFileType only read the handle value.
    let kind = unsafe { GetFileType(GetStdHandle(STD_OUTPUT_HANDLE)) };
    match kind {
        // NUL is a character device.
        FILE_TYPE_CHAR => "null",
        FILE_TYPE_PIPE => "pipe",
        FILE_TYPE_DISK => "file",
        _ => "other",
    }
}

/// `text` with `@NOW_S@` replaced by the Unix time in seconds.
fn with_now(text: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    text.replace("@NOW_S@", &now.to_string())
}
