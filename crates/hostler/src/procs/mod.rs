//! Process identity, run groups and liveness (spec §5 "Holder", plan Task 3).
//!
//! A process is alive only while the exact `{pid, boot_id, start_time}` exists. Every value comes
//! from a stable per-OS source; none is derived from the current wall clock.

use serde::{Deserialize, Serialize};

#[cfg(test)]
pub(crate) mod fake;
#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("hostler supports Linux, macOS and Windows only");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub boot_id: String,
    pub start_time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunGroup {
    UnixGroup { pgid: u32, boot_id: String },
    WindowsJob { name: String, boot_id: String },
}

impl RunGroup {
    fn boot_id(&self) -> &str {
        match self {
            RunGroup::UnixGroup { boot_id, .. } | RunGroup::WindowsJob { boot_id, .. } => boot_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    /// Filled where it comes for free (Unix). Windows leaves it `None`: finding a parent there
    /// needs a snapshot of every process, so use `ProcessTable::parent_lookup` instead.
    pub parent: Option<u32>,
    pub start_time: Option<u64>,
    pub zombie: bool,
}

/// A way to look up parents, made once per walk: on Windows it holds one process snapshot.
pub type ParentLookup<'a> = Box<dyn FnMut(u32) -> Option<u32> + 'a>;

pub trait ProcessTable: Send + Sync {
    fn boot_id(&self) -> String;
    /// Liveness data; must stay cheap, it runs for every queue entry on every tick.
    fn info(&self, pid: u32) -> Option<ProcInfo>;
    fn group_alive(&self, group: &RunGroup) -> bool;
    /// Parent lookups for one walk up the process tree (`ancestors`).
    fn parent_lookup(&self) -> ParentLookup<'_> {
        Box::new(move |pid| self.info(pid).and_then(|i| i.parent))
    }
}

/// The real process table. Process data is read fresh on every call; the boot id is read once,
/// since it cannot change while this process runs.
pub struct SysProcessTable {
    boot_id: String,
}

impl SysProcessTable {
    /// Fails if the boot id is unreadable: without it no identity can be trusted.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            boot_id: os::boot_id()?,
        })
    }
}

impl ProcessTable for SysProcessTable {
    fn boot_id(&self) -> String {
        self.boot_id.clone()
    }

    fn info(&self, pid: u32) -> Option<ProcInfo> {
        os::info(pid)
    }

    #[cfg(windows)]
    fn parent_lookup(&self) -> ParentLookup<'_> {
        let parents = os::parent_map();
        Box::new(move |pid| parents.get(&pid).copied())
    }

    fn group_alive(&self, group: &RunGroup) -> bool {
        match group {
            #[cfg(unix)]
            RunGroup::UnixGroup { pgid, .. } => unix_group_has_live_member(*pgid),
            #[cfg(windows)]
            RunGroup::WindowsJob { name, .. } => os::job_has_active_process(name),
            // A group of the other OS family cannot exist on this machine.
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead(DeadReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeadReason {
    NoProcess,
    OtherBoot,
    PidReused,
    StartTimeUnreadable,
    Zombie,
}

pub fn current_identity(table: &dyn ProcessTable) -> anyhow::Result<ProcessIdentity> {
    let pid = std::process::id();
    identity_of(table, pid)
        .ok_or_else(|| anyhow::anyhow!("cannot read the start time of the current process {pid}"))
}

/// The identity of `pid` now, or `None` if it does not exist or its start time is unreadable.
pub fn identity_of(table: &dyn ProcessTable, pid: u32) -> Option<ProcessIdentity> {
    let start_time = table.info(pid)?.start_time?;
    Some(ProcessIdentity {
        pid,
        boot_id: table.boot_id(),
        start_time,
    })
}

pub fn liveness(id: &ProcessIdentity, table: &dyn ProcessTable) -> Liveness {
    if id.boot_id != table.boot_id() {
        return Liveness::Dead(DeadReason::OtherBoot);
    }
    let Some(info) = table.info(id.pid) else {
        return Liveness::Dead(DeadReason::NoProcess);
    };
    if info.zombie {
        return Liveness::Dead(DeadReason::Zombie);
    }
    match info.start_time {
        None => Liveness::Dead(DeadReason::StartTimeUnreadable),
        Some(t) if t != id.start_time => Liveness::Dead(DeadReason::PidReused),
        Some(_) => Liveness::Alive,
    }
}

/// A run group is alive while it is from this boot and has a non-zombie member.
pub fn group_liveness(group: &RunGroup, table: &dyn ProcessTable) -> bool {
    group.boot_id() == table.boot_id() && table.group_alive(group)
}

/// True while some non-zombie process has process group `pgid`. `killpg(pgid, 0)` is not used:
/// it succeeds for a group of unreaped zombies.
#[cfg(unix)]
pub fn unix_group_has_live_member(pgid: u32) -> bool {
    // pgid 0 means "no group" in /proc and "the caller's group" to the kernel; never a run group.
    pgid != 0 && os::group_has_live_member(pgid)
}

const MAX_ANCESTORS: usize = 64;

/// The parent chain of `pid` (nearest first, `pid` itself excluded), stopping at a cycle, at an
/// unknown parent, or after 64 entries.
pub fn ancestors(pid: u32, table: &dyn ProcessTable) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut current = pid;
    let mut parent_of = table.parent_lookup();
    while chain.len() < MAX_ANCESTORS {
        let Some(parent) = parent_of(current) else {
            break;
        };
        if parent == pid || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::fake::FakeTable;
    use super::*;

    fn id(pid: u32, boot: &str, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: boot.into(),
            start_time: start,
        }
    }

    #[test]
    fn alive_when_identity_matches() {
        let t = FakeTable::new("b1");
        t.insert(10, Some(1), Some(500));
        assert_eq!(liveness(&id(10, "b1", 500), &t), Liveness::Alive);
        assert_eq!(identity_of(&t, 10), Some(id(10, "b1", 500)));
    }

    #[test]
    fn dead_other_boot() {
        let t = FakeTable::new("b1");
        t.insert(10, None, Some(500));
        t.set_boot_id("b2");
        assert_eq!(
            liveness(&id(10, "b1", 500), &t),
            Liveness::Dead(DeadReason::OtherBoot)
        );
    }

    #[test]
    fn dead_no_process() {
        let t = FakeTable::new("b1");
        t.insert(10, None, Some(500));
        t.remove(10);
        assert_eq!(
            liveness(&id(10, "b1", 500), &t),
            Liveness::Dead(DeadReason::NoProcess)
        );
    }

    #[test]
    fn dead_zombie() {
        let t = FakeTable::new("b1");
        t.insert(10, None, Some(500));
        t.set_zombie(10, true);
        assert_eq!(
            liveness(&id(10, "b1", 500), &t),
            Liveness::Dead(DeadReason::Zombie)
        );
    }

    #[test]
    fn dead_start_time_unreadable() {
        let t = FakeTable::new("b1");
        t.insert(10, None, None);
        assert_eq!(
            liveness(&id(10, "b1", 500), &t),
            Liveness::Dead(DeadReason::StartTimeUnreadable)
        );
        assert_eq!(identity_of(&t, 10), None);
    }

    #[test]
    fn dead_pid_reused() {
        let t = FakeTable::new("b1");
        t.insert(10, None, Some(501));
        assert_eq!(
            liveness(&id(10, "b1", 500), &t),
            Liveness::Dead(DeadReason::PidReused)
        );
    }

    #[test]
    fn group_on_other_boot_is_dead() {
        let t = FakeTable::new("b2");
        let old = RunGroup::UnixGroup {
            pgid: 42,
            boot_id: "b1".into(),
        };
        let now = RunGroup::UnixGroup {
            pgid: 42,
            boot_id: "b2".into(),
        };
        t.set_group_alive(&old, true);
        t.set_group_alive(&now, true);
        assert!(!group_liveness(&old, &t));
        assert!(group_liveness(&now, &t));
        t.set_group_alive(&now, false);
        assert!(!group_liveness(&now, &t));
    }

    #[test]
    fn ancestors_stops_on_cycle_and_at_64() {
        let t = FakeTable::new("b1");
        // 1 <- 2 <- 3, and 1's parent is 3: a cycle.
        t.insert(1, Some(3), Some(1));
        t.insert(2, Some(1), Some(1));
        t.insert(3, Some(2), Some(1));
        assert_eq!(ancestors(3, &t), vec![2, 1]);
        // A chain of 100: only the 64 nearest parents.
        for pid in 100..200 {
            t.insert(pid, Some(pid + 1), Some(1));
        }
        let chain = ancestors(100, &t);
        assert_eq!(chain.len(), 64);
        assert_eq!(chain.first(), Some(&101));
        assert_eq!(chain.last(), Some(&164));
        // An unknown parent ends the chain.
        assert_eq!(ancestors(199, &t), vec![200]);
    }
}
