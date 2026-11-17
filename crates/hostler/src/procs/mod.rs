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

/// `pid`, `boot_id`, `start_time`. An **empty** `boot_id` means "this machine's current boot":
/// a client that may not read the boot id (an agent sandbox denies `sysctl
/// kern.bootsessionuuid`, P4 Task 9 F-1) and has not yet learned it from the service sends it
/// empty, since a client on the service's local socket always runs in the service's boot. The
/// service fills in its own boot id as it reads the identity (`set_local_boot_id`), so every
/// identity it keeps, reaps or binds has a real one; a client learns the service's from `hello`
/// or `service.status` (`learn_boot_id`) before it persists or compares anything. An empty boot
/// id never reaches a liveness comparison (`same_boot`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub boot_id: String,
    pub start_time: u64,
}

impl<'de> Deserialize<'de> for ProcessIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            pid: u32,
            #[serde(default)]
            boot_id: String,
            start_time: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(ProcessIdentity {
            pid: wire.pid,
            boot_id: local_boot(wire.boot_id),
            start_time: wire.start_time,
        })
    }
}

/// This machine's boot id as this process knows it: read by the service itself, or learned by a
/// client from the service (F-1 r1). What an empty `boot_id` stands for.
static LOCAL_BOOT_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Called once by the service with its own (non-empty) boot id: from then on every identity and
/// run group it reads with an empty `boot_id` gets this one.
pub fn set_local_boot_id(boot_id: &str) {
    if !boot_id.is_empty() {
        let _ = LOCAL_BOOT_ID.set(boot_id.to_owned());
    }
}

/// A client whose own read of the boot id was denied takes the service's, from `hello` or
/// `service.status`, right after connecting (review f1-r1): every process table it makes from
/// then on has the real boot id, so nothing it persists (`owner.json`) or compares is empty.
/// A client that can read its own keeps that.
pub fn learn_boot_id(boot_id: &str) {
    if LOCAL_BOOT_ID.get().is_none() && !own_boot_id_readable() {
        set_local_boot_id(boot_id);
    }
}

/// The boot id this process knows without asking the OS again (the service's own, or the one a
/// client learned), if any.
pub fn local_boot_id() -> Option<String> {
    LOCAL_BOOT_ID.get().cloned()
}

/// Whether this process may read the boot id itself (read once).
fn own_boot_id_readable() -> bool {
    static READABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READABLE.get_or_init(|| read_boot_id().is_ok())
}

/// `boot_id`, or the local boot id for an empty one (in the service; a client keeps it empty).
fn local_boot(boot_id: String) -> String {
    fill_boot(boot_id, LOCAL_BOOT_ID.get().map(String::as_str))
}

/// `boot_id`, or `local` for an empty one (pure, for `local_boot`).
fn fill_boot(boot_id: String, local: Option<&str>) -> String {
    match local {
        Some(local) if boot_id.is_empty() => local.to_owned(),
        _ => boot_id,
    }
}

/// Whether two boot ids name the same boot. An empty one never gets here (review f1-r1): the
/// service fills every identity it reads, and a client learns the boot id before it compares;
/// should one slip through, it matches nothing (a release build judges it dead, never alive).
pub fn same_boot(a: &str, b: &str) -> bool {
    debug_assert!(
        !a.is_empty() && !b.is_empty(),
        "an empty boot id reached a liveness comparison ({a:?} vs {b:?})"
    );
    !a.is_empty() && a == b
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunGroup {
    UnixGroup {
        pgid: u32,
        boot_id: String,
    },
    WindowsJob {
        name: String,
        boot_id: String,
        /// The Windows logon session the job was made in (P3a Task 6); absent in P1-era JSON.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<u32>,
    },
}

impl<'de> Deserialize<'de> for RunGroup {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire {
            UnixGroup {
                pgid: u32,
                #[serde(default)]
                boot_id: String,
            },
            WindowsJob {
                name: String,
                #[serde(default)]
                boot_id: String,
                #[serde(default)]
                session: Option<u32>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::UnixGroup { pgid, boot_id } => RunGroup::UnixGroup {
                pgid,
                boot_id: local_boot(boot_id),
            },
            Wire::WindowsJob {
                name,
                boot_id,
                session,
            } => RunGroup::WindowsJob {
                name,
                boot_id: local_boot(boot_id),
                session,
            },
        })
    }
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

/// One walk's view of the process tree: each pid's parent and start time (L46). `(None, None)`
/// for a pid that is not there.
pub type LineageLookup<'a> = Box<dyn FnMut(u32) -> (Option<u32>, Option<u64>) + 'a>;

pub trait ProcessTable: Send + Sync {
    fn boot_id(&self) -> String;
    /// Liveness data; must stay cheap, it runs for every queue entry on every tick.
    fn info(&self, pid: u32) -> Option<ProcInfo>;
    fn group_alive(&self, group: &RunGroup) -> bool;
    /// L31: keep a handle on `group` for `owner` (an engine) from its `*.started` on, so a
    /// Windows job stays findable after its last other handle (the wrapper's) closes. A no-op
    /// where groups need none (Unix).
    fn hold_group(&self, _owner: &'static str, _group: &RunGroup) {}
    /// L31: the groups `owner` still needs; the others' handles close, and missing ones are
    /// opened while their name still exists (a restarted service).
    fn hold_only(&self, _owner: &'static str, _groups: &[RunGroup]) {}
    /// A takeover's three-valued view of `group` (see `group_state`); by default from
    /// `group_alive`, which never answers unknown.
    fn job_state(&self, group: &RunGroup) -> JobState {
        if self.group_alive(group) {
            JobState::Alive
        } else {
            JobState::Gone
        }
    }
    /// Parent lookups for one walk up the process tree (`ancestors`).
    fn parent_lookup(&self) -> ParentLookup<'_> {
        Box::new(move |pid| self.info(pid).and_then(|i| i.parent))
    }
    /// Parents and start times for one ancestry walk (L46): by default `parent_lookup` plus
    /// `info`; Windows reads both from one system snapshot, so no process has to be opened
    /// (an elevated one cannot be by a non-elevated service).
    fn lineage_lookup(&self) -> LineageLookup<'_> {
        let mut parent = self.parent_lookup();
        Box::new(move |pid| (parent(pid), self.info(pid).and_then(|i| i.start_time)))
    }
    /// The executable's file name for `pid`; the real table reads it from the OS, the fake one
    /// from the names a test set.
    fn process_name(&self, pid: u32) -> Option<String> {
        process_name(pid)
    }
    /// Whether any process has `pid`, readable or not (`pid_exists`). Only agent-device's L53
    /// check asks; liveness keeps "no info" as dead.
    fn exists(&self, pid: u32) -> Option<bool> {
        pid_exists(pid)
    }
}

/// The real process table. Process data is read fresh on every call; the boot id is read once,
/// since it cannot change while this process runs.
pub struct SysProcessTable {
    boot_id: String,
    /// L31: the job handles each engine keeps, by job name.
    #[cfg(windows)]
    jobs: std::sync::Mutex<
        std::collections::HashMap<&'static str, std::collections::HashMap<String, os::HeldJob>>,
    >,
}

impl SysProcessTable {
    /// Fails if the boot id is unreadable, except when this process is not permitted to read
    /// it (an agent sandbox, P4 Task 9 F-1): then the boot id is empty, "this boot", and the
    /// service fills in its own (see `ProcessIdentity`). The service itself requires a real one
    /// (`boot_id_known`).
    pub fn new() -> anyhow::Result<Self> {
        let boot_id = match read_boot_id() {
            Ok(boot_id) => boot_id,
            // Denied (an agent sandbox): the one learned from the service, else "this boot".
            Err(e) if permission_denied(&e) => local_boot_id().unwrap_or_default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            boot_id,
            #[cfg(windows)]
            jobs: Default::default(),
        })
    }
}

impl SysProcessTable {
    /// Whether the boot id could be read (the service needs it; a sandboxed client may not).
    pub fn boot_id_known(&self) -> bool {
        !self.boot_id.is_empty()
    }
}

/// The OS boot id. Debug builds: `HOSTLER_TEST_BOOTID_ERRNO=EPERM` fails it as a sandbox does.
fn read_boot_id() -> anyhow::Result<String> {
    #[cfg(debug_assertions)]
    if std::env::var("HOSTLER_TEST_BOOTID_ERRNO").as_deref() == Ok("EPERM") {
        return Err(
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                .context("the boot id (HOSTLER_TEST_BOOTID_ERRNO)"),
        );
    }
    os::boot_id()
}

/// Whether `e` (or a cause of it) is an OS `PermissionDenied`.
pub fn permission_denied(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
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

    #[cfg(windows)]
    fn lineage_lookup(&self) -> LineageLookup<'_> {
        let snapshot = os::lineage_snapshot();
        Box::new(move |pid| match snapshot.get(&pid) {
            Some(&(parent, created)) => (Some(parent), Some(created)),
            None => (None, None),
        })
    }

    fn group_alive(&self, group: &RunGroup) -> bool {
        match group {
            #[cfg(unix)]
            RunGroup::UnixGroup { pgid, .. } => unix_group_has_live_member(*pgid),
            #[cfg(windows)]
            RunGroup::WindowsJob { name, session, .. } => match self.held_job_state(name) {
                Some(state) => state == JobState::Alive,
                None => os::job_has_active_process(name, *session),
            },
            // A group of the other OS family cannot exist on this machine.
            _ => false,
        }
    }

    #[cfg(windows)]
    fn job_state(&self, group: &RunGroup) -> JobState {
        match group {
            RunGroup::WindowsJob { name, session, .. } => self
                .held_job_state(name)
                .unwrap_or_else(|| os::job_state(name, *session)),
            RunGroup::UnixGroup { .. } => JobState::Gone,
        }
    }

    #[cfg(windows)]
    fn hold_group(&self, owner: &'static str, group: &RunGroup) {
        // Another session's job is opened by its session name (P3b Task 10); a P1-era record
        // without a session is not held.
        let RunGroup::WindowsJob {
            name,
            session: session @ Some(_),
            ..
        } = group
        else {
            return;
        };
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let held = jobs.entry(owner).or_default();
        if !held.contains_key(name)
            && let Some(job) = os::hold_job(name, *session)
        {
            held.insert(name.clone(), job);
        }
    }

    #[cfg(windows)]
    fn hold_only(&self, owner: &'static str, groups: &[RunGroup]) {
        let names: std::collections::HashSet<&str> = groups
            .iter()
            .filter_map(|g| match g {
                RunGroup::WindowsJob { name, .. } => Some(name.as_str()),
                RunGroup::UnixGroup { .. } => None,
            })
            .collect();
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(owner)
            .or_default()
            .retain(|name, _| names.contains(name.as_str()));
        for group in groups {
            self.hold_group(owner, group);
        }
    }
}

#[cfg(windows)]
impl SysProcessTable {
    /// What a held handle says about job `name`, if any engine holds one (L31).
    fn held_job_state(&self, name: &str) -> Option<JobState> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.values()
            .find_map(|held| held.get(name))
            .map(os::held_job_state)
    }
}

/// Whether a run group still has members, as a takeover commit must know it (L23). Reaping keeps
/// `group_liveness`, where an unopenable Windows job counts as dead so queues never stick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Gone,
    Alive,
    /// Windows: the job cannot be opened for a reason that does not prove it gone (access
    /// denied, a P1-era record without a session, an unreadable session). Blocks a commit.
    Unknown,
    /// Windows: another logon session's job is not found by its session name. Its name went
    /// with its last handle, so it is gone only once its launcher is dead too (P2-11, L31's
    /// residual); the caller, which knows the launcher, decides.
    NotFound,
}

/// Unix: from `group_liveness`, never `Unknown`. Windows: the job is opened by its name as seen
/// from the caller's session (`Global\`, `Session\<n>\`); `Gone` when it opens with no active
/// process, or when it is not found *and* its recorded session is the caller's; `NotFound` when
/// it is not found in another session; `Alive` when it opens with active processes; `Unknown`
/// otherwise. A group of another boot is `Gone`.
pub fn group_state(group: &RunGroup, table: &dyn ProcessTable) -> JobState {
    if !same_boot(group.boot_id(), &table.boot_id()) {
        return JobState::Gone;
    }
    table.job_state(group)
}

/// Whether process `pid` is a member of `group` now: Unix, its process group is the group's
/// pgid; Windows, it is in the group's job (`IsProcessInJob`). A group of the other OS family:
/// no.
pub fn in_group(pid: u32, group: &RunGroup) -> bool {
    match group {
        #[cfg(unix)]
        RunGroup::UnixGroup { pgid, .. } => {
            // SAFETY: getpgid has no memory preconditions; -1 (no such process) never matches.
            let found = unsafe { libc::getpgid(pid as libc::pid_t) };
            found >= 0 && found as u32 == *pgid
        }
        #[cfg(windows)]
        RunGroup::WindowsJob { name, .. } => os::process_in_job(pid, name),
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

/// Whether a process seen with this start time (`None`: unreadable) is the one `p` names: a
/// stop acts on a handle only after this check on that same handle (T6-5).
pub fn same_process(observed_start: Option<u64>, p: &ProcessIdentity) -> bool {
    observed_start == Some(p.start_time)
}

/// The Windows logon session of this process (`None` on Unix, or when it cannot be read).
pub fn current_session() -> Option<u32> {
    #[cfg(windows)]
    {
        os::current_session()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
pub(crate) use windows::terminate_job_from;
#[cfg(all(windows, test))]
pub(crate) use windows::test_job::test_job;
/// Windows termination for takeovers (`cli::takeover::RunGroupStopper`).
#[cfg(windows)]
pub use windows::{KillOutcome, terminate_job, terminate_process};

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
    identity_of(table, pid).ok_or_else(|| {
        // The OS's reason, read right after the failed lookup: a sandbox's EPERM stays an
        // `io::Error` so callers classify it (`client::process_error`, P4 Task 9 F-1).
        let os = std::io::Error::last_os_error();
        let what = format!("cannot read the start time of the current process {pid}");
        if os.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::Error::new(os).context(what)
        } else {
            anyhow::anyhow!(what)
        }
    })
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

/// The executable's file name for `pid` (no directory): Linux `/proc/<pid>/comm`, macOS
/// `proc_name`, Windows `QueryFullProcessImageNameW`. `None` when it cannot be read.
pub fn process_name(pid: u32) -> Option<String> {
    os::process_name(pid)
}

/// Whether any process has `pid`, also one this process cannot read (another user's, an
/// elevated one): `Some(false)` only when the OS says no such process; `None` when it cannot
/// tell. Unix `kill(pid, 0)` (EPERM: it exists); Windows `OpenProcess` (access denied: it exists).
pub fn pid_exists(pid: u32) -> Option<bool> {
    os_pid_exists(pid)
}

#[cfg(unix)]
fn os_pid_exists(pid: u32) -> Option<bool> {
    // 0 would signal our own group, and a pid past `pid_t` a group or every process.
    let pid = libc::pid_t::try_from(pid).ok().filter(|&p| p > 0)?;
    // SAFETY: signal 0 sends nothing; it only checks the pid and the permission.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => Some(true),
        Some(libc::ESRCH) => Some(false),
        _ => None,
    }
}

#[cfg(windows)]
fn os_pid_exists(pid: u32) -> Option<bool> {
    os::pid_exists(pid)
}

pub fn liveness(id: &ProcessIdentity, table: &dyn ProcessTable) -> Liveness {
    if !same_boot(&id.boot_id, &table.boot_id()) {
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
    same_boot(group.boot_id(), &table.boot_id()) && table.group_alive(group)
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
    ancestors_with(pid, &mut table.parent_lookup())
}

/// `ancestors` through a lookup the caller made once: one lookup (on Windows, one process
/// snapshot) serves many walks.
pub fn ancestors_with(pid: u32, parent_of: &mut ParentLookup<'_>) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut current = pid;
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

/// L46: `SYSTEM_PROCESS_INFORMATION` (x64 layout) entries of an
/// `NtQuerySystemInformation(SystemProcessInformation)` buffer → pid → (parent pid, creation
/// time as `ProcessIdentity::start_time` holds it on Windows). Pure, so it is tested everywhere;
/// a truncated or malformed buffer yields what was whole.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn parse_process_information(buf: &[u8]) -> std::collections::HashMap<u32, (u32, u64)> {
    const ENTRY: usize = 0x60;
    const CREATE_TIME: usize = 0x20;
    const PROCESS_ID: usize = 0x50;
    const PARENT_ID: usize = 0x58;
    let u32_at = |at: usize| {
        buf.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let u64_at = |at: usize| {
        buf.get(at..at + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    let mut out = std::collections::HashMap::new();
    let mut offset = 0usize;
    while offset + ENTRY <= buf.len() {
        let (Some(next), Some(created), Some(pid), Some(parent)) = (
            u32_at(offset),
            u64_at(offset + CREATE_TIME),
            u64_at(offset + PROCESS_ID),
            u64_at(offset + PARENT_ID),
        ) else {
            break;
        };
        // The idle process (pid 0) has no creation time and is no one's parent here.
        if pid != 0 {
            out.insert(pid as u32, (parent as u32, created));
        }
        if next == 0 {
            break;
        }
        offset += next as usize;
    }
    out
}

/// `pid`'s ancestors through one `lineage_lookup`, cut at the first parent that started after
/// its child, or that is not there (CR-A-1, L46): Windows keeps a dead parent's pid as the
/// parent, and that pid may be a stranger's now.
pub fn ancestors_checked(pid: u32, lineage: &mut LineageLookup<'_>) -> Vec<u32> {
    let mut chain = Vec::new();
    let (mut next, mut current_start) = lineage(pid);
    while chain.len() < MAX_ANCESTORS {
        let Some(parent) = next else {
            break;
        };
        if parent == pid || chain.contains(&parent) {
            break;
        }
        let (parent_parent, parent_start) = lineage(parent);
        match (current_start, parent_start) {
            (Some(child), Some(started)) if started <= child => {}
            _ => break,
        }
        chain.push(parent);
        current_start = parent_start;
        next = parent_parent;
    }
    chain
}

#[cfg(test)]
mod boot_tests {
    use super::*;

    /// Review f1-r1: boot ids compare equal only when both are real and the same.
    #[test]
    fn boot_ids_compare_by_equality() {
        assert!(same_boot("b1", "b1"));
        assert!(!same_boot("b1", "b2"));
        let table = fake::FakeTable::new("b1");
        table.insert(7, None, Some(70));
        let id = ProcessIdentity {
            pid: 7,
            boot_id: "b1".into(),
            start_time: 70,
        };
        assert_eq!(liveness(&id, &table), Liveness::Alive);
        let other_boot = ProcessIdentity {
            boot_id: "b0".into(),
            ..id.clone()
        };
        assert_eq!(
            liveness(&other_boot, &table),
            Liveness::Dead(DeadReason::OtherBoot)
        );
    }

    /// An empty boot id reaching a comparison is a bug (debug builds stop on it).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "an empty boot id reached a liveness comparison")]
    fn an_empty_boot_id_never_reaches_a_comparison() {
        same_boot("", "b1");
    }

    /// The service completes an identity sent without a boot id with its own; one with a boot
    /// id keeps it.
    #[test]
    fn the_service_fills_its_own_boot_id() {
        assert_eq!(fill_boot(String::new(), Some("svc-boot")), "svc-boot");
        assert_eq!(fill_boot("b1".into(), Some("svc-boot")), "b1");
        assert_eq!(fill_boot(String::new(), None), "");
        // On the wire: a missing or empty `boot_id` goes through the same fill.
        let local = LOCAL_BOOT_ID.get().cloned().unwrap_or_default();
        for wire in [
            serde_json::json!({"pid": 1, "start_time": 2}),
            serde_json::json!({"pid": 1, "boot_id": "", "start_time": 2}),
        ] {
            let id: ProcessIdentity = serde_json::from_value(wire).unwrap();
            assert_eq!(id.boot_id, local);
        }
        let kept: ProcessIdentity =
            serde_json::from_value(serde_json::json!({"pid": 1, "boot_id": "b9", "start_time": 2}))
                .unwrap();
        assert_eq!(kept.boot_id, "b9");
        let group: RunGroup =
            serde_json::from_value(serde_json::json!({"kind": "unix_group", "pgid": 5})).unwrap();
        assert_eq!(
            group,
            RunGroup::UnixGroup {
                pgid: 5,
                boot_id: local.clone()
            }
        );
        let job: RunGroup = serde_json::from_value(serde_json::json!({
            "kind": "windows_job", "name": "Local\\j", "boot_id": "b", "session": 3
        }))
        .unwrap();
        assert_eq!(
            job,
            RunGroup::WindowsJob {
                name: "Local\\j".into(),
                boot_id: "b".into(),
                session: Some(3)
            }
        );
    }

    #[test]
    fn permission_denied_is_found_in_the_chain() {
        let denied = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context("sysctl kern.bootsessionuuid");
        assert!(permission_denied(&denied));
        assert!(!permission_denied(&anyhow::anyhow!("empty")));
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeTable;
    use super::*;

    /// L46: the snapshot's entries are read whole, linked by `NextEntryOffset`; a truncated
    /// buffer yields the entries that were complete.
    #[test]
    fn process_information_is_parsed_from_the_snapshot() {
        let entry = |next: u32, pid: u64, parent: u64, created: u64| {
            let mut e = vec![0u8; 0x60];
            e[0..4].copy_from_slice(&next.to_le_bytes());
            e[0x20..0x28].copy_from_slice(&created.to_le_bytes());
            e[0x50..0x58].copy_from_slice(&pid.to_le_bytes());
            e[0x58..0x60].copy_from_slice(&parent.to_le_bytes());
            e
        };
        let mut buf = entry(0x60, 0, 0, 0);
        buf.extend(entry(0x70, 4, 0, 10));
        buf.extend(vec![0u8; 0x10]);
        buf.extend(entry(0, 9000, 4, 99));
        let map = parse_process_information(&buf);
        assert_eq!(map.len(), 2, "{map:?}");
        assert_eq!(map[&4], (0, 10));
        assert_eq!(map[&9000], (4, 99));
        // Cut in the middle of the last entry: the whole ones only.
        let map = parse_process_information(&buf[..buf.len() - 8]);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&4));
        assert!(parse_process_information(&[]).is_empty());
    }

    /// L46: through a process the service cannot open (an elevated one), the snapshot still
    /// gives its start time, so the chain goes on; only a younger or missing parent cuts it.
    #[test]
    fn the_chain_goes_through_an_unopenable_process() {
        let t = FakeTable::new("b1");
        t.insert(160, Some(150), Some(1600));
        t.insert(150, Some(101), Some(1500));
        t.insert(101, Some(100), Some(1010));
        t.insert(100, Some(1), Some(1000));
        t.set_unopenable(150, true);
        assert_eq!(t.info(150).unwrap().start_time, None);
        assert_eq!(
            ancestors_checked(160, &mut t.lineage_lookup()),
            [150, 101, 100]
        );
    }

    /// CR-A-1: a parent that started after its child is not its parent (a reused pid).
    #[test]
    fn ancestors_checked_stops_at_a_parent_younger_than_its_child() {
        let t = FakeTable::new("b1");
        t.insert(8000, Some(7000), Some(800));
        t.insert(7000, Some(4200), Some(500));
        // 4200 is a stranger now: it started after 7000.
        t.insert(4200, Some(4100), Some(900));
        t.insert(4100, Some(1), Some(100));
        assert_eq!(ancestors_checked(8000, &mut t.lineage_lookup()), [7000]);
        // Unchecked, the stale hop goes on.
        assert_eq!(
            ancestors_with(8000, &mut t.parent_lookup()),
            [7000, 4200, 4100, 1]
        );
        // A parent that is not there (no start time in this lineage) ends the chain too.
        t.insert(4200, Some(4100), None);
        assert_eq!(
            ancestors_checked(7000, &mut t.lineage_lookup()),
            Vec::<u32>::new()
        );
        // An ordinary chain is whole.
        t.insert(4200, Some(4100), Some(300));
        assert_eq!(
            ancestors_checked(8000, &mut t.lineage_lookup()),
            [7000, 4200, 4100]
        );
    }

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
    fn group_state_is_three_valued_and_other_boot_is_gone() {
        let t = FakeTable::new("b2");
        let old = RunGroup::WindowsJob {
            name: "Local\\hostler-heavy-1".into(),
            boot_id: "b1".into(),
            session: Some(1),
        };
        let now = RunGroup::WindowsJob {
            name: "Local\\hostler-heavy-2".into(),
            boot_id: "b2".into(),
            session: Some(1),
        };
        assert_eq!(group_state(&now, &t), JobState::Gone);
        t.set_group_alive(&now, true);
        assert_eq!(group_state(&now, &t), JobState::Alive);
        t.set_group_unknown(&now, true);
        assert_eq!(group_state(&now, &t), JobState::Unknown);
        // Reaping keeps its two-valued view: an unknown job counts as dead only if it is not
        // alive by `group_alive` (here it still is).
        assert!(group_liveness(&now, &t));
        t.set_group_unknown(&old, true);
        assert_eq!(group_state(&old, &t), JobState::Gone);
    }

    /// CRA-1: existence answers for pids this process may not read; never for pid 0 or a pid
    /// past `pid_t` (they name groups to `kill`).
    #[cfg(unix)]
    #[test]
    fn pid_exists_tells_gone_from_unreadable() {
        assert_eq!(pid_exists(std::process::id()), Some(true));
        // pid 1 (init/launchd) is another user's: EPERM, still there.
        assert_eq!(pid_exists(1), Some(true));
        assert_eq!(pid_exists(0), None);
        assert_eq!(pid_exists(u32::MAX), None);
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(pid_exists(pid), Some(false), "reaped");
    }

    #[cfg(unix)]
    #[test]
    fn in_group_compares_the_process_group() {
        // SAFETY: getpgrp has no preconditions.
        let mine = unsafe { libc::getpgrp() } as u32;
        let group = |pgid| RunGroup::UnixGroup {
            pgid,
            boot_id: "b".into(),
        };
        assert!(in_group(std::process::id(), &group(mine)));
        assert!(!in_group(std::process::id(), &group(mine + 1)));
        // No such process.
        assert!(!in_group(u32::MAX / 2, &group(mine)));
    }

    #[test]
    fn same_process_needs_the_same_start_time() {
        let p = ProcessIdentity {
            pid: 7,
            boot_id: "b".into(),
            start_time: 70,
        };
        assert!(same_process(Some(70), &p));
        assert!(!same_process(Some(71), &p));
        assert!(!same_process(None, &p));
    }

    #[cfg(unix)]
    #[test]
    fn unix_group_state_follows_the_real_group() {
        use std::os::unix::process::CommandExt;

        let t = SysProcessTable::new().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let group = RunGroup::UnixGroup {
            pgid: child.id(),
            boot_id: t.boot_id(),
        };
        assert_eq!(group_state(&group, &t), JobState::Alive);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(group_state(&group, &t), JobState::Gone);
        assert_eq!(current_session(), None);
    }

    #[test]
    fn one_lookup_serves_many_walks() {
        let t = FakeTable::new("b1");
        t.insert(1, None, Some(1));
        t.insert(2, Some(1), Some(2));
        t.insert(3, Some(2), Some(3));
        t.insert(4, Some(2), Some(4));
        let before = t.parent_lookups();
        let mut parent_of = t.parent_lookup();
        assert_eq!(ancestors_with(3, &mut parent_of), vec![2, 1]);
        assert_eq!(ancestors_with(4, &mut parent_of), vec![2, 1]);
        assert_eq!(ancestors_with(1, &mut parent_of), Vec::<u32>::new());
        drop(parent_of);
        assert_eq!(t.parent_lookups() - before, 1);
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
