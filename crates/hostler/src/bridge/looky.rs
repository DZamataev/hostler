//! Reads rn-looky's running heavy tasks as foreign holders (plan Task 11). The ledger is only
//! ever read; hostler never writes it and never fails because of it.

use std::path::PathBuf;

use crate::contract::heavy::HeavyEntry;
use crate::heavy::engine::ForeignHeavySource;

#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use serde::Deserialize;

#[cfg(unix)]
use crate::contract::heavy::{EntrySource, HeavyState};
#[cfg(unix)]
use crate::procs::{ProcessIdentity, RunGroup};

/// Same tolerance rn-looky uses: the kernel rounds the boot instant by a second.
#[cfg(unix)]
const BOOT_TOLERANCE_SECONDS: u64 = 2;

pub struct LookyHeavy {
    #[cfg_attr(not(unix), allow(dead_code))]
    path: PathBuf,
}

impl LookyHeavy {
    /// `HOSTLER_LOOKY_LEDGER`, else `~/.looky/ledger.json`.
    pub fn new() -> LookyHeavy {
        LookyHeavy {
            path: ledger_path(),
        }
    }
}

impl Default for LookyHeavy {
    fn default() -> Self {
        LookyHeavy::new()
    }
}

fn ledger_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HOSTLER_LOOKY_LEDGER") {
        return PathBuf::from(path);
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".looky").join("ledger.json"),
        None => PathBuf::from(".looky").join("ledger.json"),
    }
}

impl ForeignHeavySource for LookyHeavy {
    fn running(&self) -> Vec<HeavyEntry> {
        #[cfg(unix)]
        {
            self.running_unix()
        }
        #[cfg(not(unix))]
        {
            Vec::new()
        }
    }
}

#[cfg(unix)]
impl LookyHeavy {
    fn running_unix(&self) -> Vec<HeavyEntry> {
        let Some(current_boot) = current_boot_seconds() else {
            return Vec::new();
        };
        let Some(heavy) = read_ledger(&self.path) else {
            return Vec::new();
        };

        heavy
            .entries
            .into_iter()
            .filter(|entry| {
                entry.state == "running"
                    && same_boot(&entry.owner.boot_id, current_boot)
                    && (owner_alive(entry.owner.pid, &entry.owner.start_time)
                        || entry
                            .process_group_id
                            .is_some_and(crate::procs::unix_group_has_live_member))
            })
            .map(|entry| {
                let boot_id = entry.owner.boot_id;
                HeavyEntry {
                    id: format!("looky:{}", entry.id),
                    ticket: entry.ticket,
                    kind: entry.kind,
                    command: entry.command,
                    cwd: entry.path,
                    workspace_root: None,
                    // rn-looky's startTime is a `ps` string and its liveness is decided above;
                    // hostler's numeric token is not used for foreign entries.
                    holder: ProcessIdentity {
                        pid: entry.owner.pid,
                        boot_id: boot_id.clone(),
                        start_time: 0,
                    },
                    child: None,
                    group: entry
                        .process_group_id
                        .map(|pgid| RunGroup::UnixGroup { pgid, boot_id }),
                    state: HeavyState::Running,
                    enqueued_at_ms: 0,
                    granted_at_ms: None,
                    source: EntrySource::RnLookyLedger,
                }
            })
            .collect()
    }
}

#[cfg(unix)]
#[derive(Deserialize)]
struct Ledger {
    #[serde(default)]
    heavy: Option<Heavy>,
}

#[cfg(unix)]
#[derive(Deserialize)]
struct Heavy {
    #[serde(default)]
    entries: Vec<LedgerEntry>,
}

#[cfg(unix)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerEntry {
    id: String,
    #[serde(default)]
    ticket: u64,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    path: String,
    owner: Owner,
    #[serde(default)]
    state: String,
    #[serde(default)]
    process_group_id: Option<u32>,
}

#[cfg(unix)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    pid: u32,
    boot_id: String,
    #[serde(default)]
    start_time: String,
}

#[cfg(unix)]
fn read_ledger(path: &Path) -> Option<Heavy> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Ledger>(&contents).ok()?.heavy
}

#[cfg(unix)]
fn same_boot(recorded: &str, current: u64) -> bool {
    let Some(seconds) = recorded
        .strip_prefix("boot-")
        .and_then(|seconds| seconds.parse::<u64>().ok())
    else {
        return false;
    };
    seconds.abs_diff(current) <= BOOT_TOLERANCE_SECONDS
}

/// The owner lives while `ps -o stat=` is not a zombie and its start time matches exactly.
#[cfg(unix)]
fn owner_alive(pid: u32, recorded_start_time: &str) -> bool {
    let Some(state) = ps_field(pid, "stat") else {
        return false;
    };
    if state.is_empty() || state.starts_with('Z') {
        return false;
    }
    let Some(start_time) = ps_field(pid, "lstart") else {
        return false;
    };
    !start_time.is_empty() && start_time == recorded_start_time.trim()
}

#[cfg(unix)]
fn ps_field(pid: u32, field: &str) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

/// Seconds since the epoch at boot, as `kern.boottime`/`btime` report it.
#[cfg(target_os = "macos")]
fn current_boot_seconds() -> Option<u64> {
    let output = Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_kern_boottime(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(target_os = "linux")]
fn current_boot_seconds() -> Option<u64> {
    parse_proc_stat_btime(&std::fs::read_to_string("/proc/stat").ok()?)
}

#[cfg(target_os = "macos")]
fn parse_kern_boottime(text: &str) -> Option<u64> {
    let rest = text.split("sec =").nth(1)?;
    rest.trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

#[cfg(target_os = "linux")]
fn parse_proc_stat_btime(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|seconds| seconds.trim().parse().ok())
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::{Value, json};

    use super::*;

    fn looky(path: PathBuf) -> LookyHeavy {
        LookyHeavy { path }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/looky")
            .join(name)
    }

    fn current_boot() -> String {
        format!("boot-{}", current_boot_seconds().expect("boot seconds"))
    }

    fn own_start_time() -> String {
        ps_field(std::process::id(), "lstart").expect("own start time")
    }

    fn own_group() -> u32 {
        // SAFETY: getpgrp takes no arguments and cannot fail.
        unsafe { libc::getpgrp() as u32 }
    }

    fn dead_pid() -> u32 {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        pid
    }

    fn entry(
        id: &str,
        pid: u32,
        boot_id: &str,
        start_time: &str,
        state: &str,
        group: Option<u32>,
    ) -> Value {
        json!({
            "id": id,
            "ticket": 1,
            "kind": "deps",
            "command": "yarn deps",
            "path": "/work/app",
            "owner": {"pid": pid, "bootId": boot_id, "startTime": start_time},
            "state": state,
            "enqueuedAt": "2026-01-01T00:00:00.000Z",
            "processGroupId": group,
        })
    }

    fn write_ledger(contents: Value) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.json");
        std::fs::write(&path, contents.to_string()).expect("write ledger");
        (dir, path)
    }

    fn running(path: PathBuf) -> Vec<HeavyEntry> {
        looky(path).running()
    }

    #[test]
    fn missing_ledger_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(running(dir.path().join("absent.json")).is_empty());
    }

    #[test]
    fn ledger_without_heavy_is_empty() {
        assert!(running(fixture("without_heavy.json")).is_empty());
    }

    #[test]
    fn running_entry_with_live_owner_counts() {
        let (_dir, path) = write_ledger(json!({
            "heavy": {"entries": [entry(
                "h1-abc123",
                std::process::id(),
                &current_boot(),
                &own_start_time(),
                "running",
                None,
            )]}
        }));

        let entries = running(path);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, "looky:h1-abc123");
        assert_eq!(entry.ticket, 1);
        assert_eq!(entry.kind, "deps");
        assert_eq!(entry.command, "yarn deps");
        assert_eq!(entry.cwd, "/work/app");
        assert_eq!(entry.holder.pid, std::process::id());
        assert_eq!(entry.state, HeavyState::Running);
        assert_eq!(entry.source, EntrySource::RnLookyLedger);
        assert_eq!(entry.group, None);
    }

    #[test]
    fn boot_within_two_seconds_counts() {
        let boot = current_boot_seconds().unwrap();
        let (_dir, path) = write_ledger(json!({
            "heavy": {"entries": [entry(
                "h1-abc123",
                std::process::id(),
                &format!("boot-{}", boot - 2),
                &own_start_time(),
                "running",
                None,
            )]}
        }));
        assert_eq!(running(path).len(), 1);

        let (_dir, path) = write_ledger(json!({
            "heavy": {"entries": [entry(
                "h1-abc123",
                std::process::id(),
                &format!("boot-{}", boot - 3),
                &own_start_time(),
                "running",
                None,
            )]}
        }));
        assert!(running(path).is_empty());
    }

    #[test]
    fn dead_owner_with_live_group_counts() {
        let (_dir, path) = write_ledger(json!({
            "heavy": {"entries": [entry(
                "h1-abc123",
                dead_pid(),
                &current_boot(),
                "Thu Jan  1 00:00:00 2026",
                "running",
                Some(own_group()),
            )]}
        }));

        assert_eq!(running(path).len(), 1);
    }

    #[test]
    fn dead_owner_and_group_ignored() {
        let (_dir, path) = write_ledger(json!({
            "heavy": {"entries": [entry(
                "h1-abc123",
                dead_pid(),
                &current_boot(),
                "Thu Jan  1 00:00:00 2026",
                "running",
                Some(999_999),
            )]}
        }));

        assert!(running(path).is_empty());
    }

    #[test]
    fn other_boot_ignored() {
        let (_dir, path) = write_ledger(json!({
            "heavy": {"entries": [entry(
                "h1-abc123",
                std::process::id(),
                "boot-1",
                &own_start_time(),
                "running",
                Some(own_group()),
            )]}
        }));

        assert!(running(path).is_empty());
    }

    #[test]
    fn waiting_entries_ignored() {
        assert!(running(fixture("waiting.json")).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kernel_boottime_is_parsed() {
        assert_eq!(
            parse_kern_boottime("{ sec = 1762487665, usec = 123456 } Mon Nov  6 10:00:00 2026"),
            Some(1_762_487_665)
        );
        assert_eq!(parse_kern_boottime(""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_btime_is_parsed() {
        assert_eq!(
            parse_proc_stat_btime("cpu  1 2 3\nbtime 1762487665\nprocesses 10\n"),
            Some(1_762_487_665)
        );
        assert_eq!(parse_proc_stat_btime("cpu 1 2 3"), None);
    }
}
