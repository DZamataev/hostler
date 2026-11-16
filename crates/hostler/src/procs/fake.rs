//! An in-memory process table for unit tests (this module and `heavy/engine.rs`). Methods take
//! `&self` so a test can change it while an engine holds it as `Arc<dyn ProcessTable>`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{JobState, LineageLookup, ParentLookup, ProcInfo, ProcessTable, RunGroup};

pub(crate) struct FakeTable {
    state: Mutex<State>,
    info_calls: AtomicUsize,
    parent_lookups: AtomicUsize,
}

struct State {
    boot_id: String,
    procs: HashMap<u32, ProcInfo>,
    /// Names for `process_name`; a pid absent here has no readable name.
    names: HashMap<u32, String>,
    live_groups: Vec<RunGroup>,
    /// Groups whose `job_state` is `Unknown` (a Windows job the service cannot open).
    unknown_groups: Vec<RunGroup>,
    /// Groups whose `job_state` is `NotFound` (another session's job whose name is gone).
    not_found_groups: Vec<RunGroup>,
    /// L31: the groups each owner holds (`hold_group`, `hold_only`).
    held: HashMap<&'static str, Vec<RunGroup>>,
    /// L46: processes `info` cannot open (an elevated one on Windows); the lineage still has
    /// their start times.
    unopenable: Vec<u32>,
    /// `exists` answers set by a test; others answer whether `procs` has the pid.
    exists: HashMap<u32, Option<bool>>,
}

impl FakeTable {
    pub(crate) fn new(boot_id: &str) -> Self {
        Self {
            state: Mutex::new(State {
                boot_id: boot_id.into(),
                procs: HashMap::new(),
                names: HashMap::new(),
                live_groups: Vec::new(),
                unknown_groups: Vec::new(),
                not_found_groups: Vec::new(),
                held: HashMap::new(),
                unopenable: Vec::new(),
                exists: HashMap::new(),
            }),
            info_calls: AtomicUsize::new(0),
            parent_lookups: AtomicUsize::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Simulates a reboot when the id changes.
    pub(crate) fn set_boot_id(&self, boot_id: &str) {
        self.state().boot_id = boot_id.into();
    }

    /// Adds or replaces a running (non-zombie) process.
    pub(crate) fn insert(&self, pid: u32, parent: Option<u32>, start_time: Option<u64>) {
        let info = ProcInfo {
            pid,
            parent,
            start_time,
            zombie: false,
        };
        self.state().procs.insert(pid, info);
    }

    pub(crate) fn remove(&self, pid: u32) {
        self.state().procs.remove(&pid);
    }

    /// What `process_name` answers for this pid (`None` by default).
    pub(crate) fn set_name(&self, pid: u32, name: &str) {
        self.state().names.insert(pid, name.into());
    }

    /// Panics if `pid` was never inserted: a test bug.
    pub(crate) fn set_zombie(&self, pid: u32, zombie: bool) {
        self.state()
            .procs
            .get_mut(&pid)
            .expect("set_zombie on a pid not in the FakeTable")
            .zombie = zombie;
    }

    /// What `group_alive` answers for this exact group (boot id included); groups default to dead.
    pub(crate) fn set_group_alive(&self, group: &RunGroup, alive: bool) {
        let mut s = self.state();
        s.live_groups.retain(|g| g != group);
        if alive {
            s.live_groups.push(group.clone());
        }
    }

    /// Makes `job_state` answer `Unknown` for this exact group (until set back with `false`).
    pub(crate) fn set_group_unknown(&self, group: &RunGroup, unknown: bool) {
        let mut s = self.state();
        s.unknown_groups.retain(|g| g != group);
        if unknown {
            s.unknown_groups.push(group.clone());
        }
    }

    /// Makes `job_state` answer `NotFound` for this exact group (until set back with `false`).
    pub(crate) fn set_group_not_found(&self, group: &RunGroup, not_found: bool) {
        let mut s = self.state();
        s.not_found_groups.retain(|g| g != group);
        if not_found {
            s.not_found_groups.push(group.clone());
        }
    }
}

impl FakeTable {
    /// Makes `info` answer no start time for `pid` (it cannot be opened); `lineage_lookup`
    /// still has it, as the Windows system snapshot does (L46).
    pub(crate) fn set_unopenable(&self, pid: u32, unopenable: bool) {
        let mut s = self.state();
        s.unopenable.retain(|p| *p != pid);
        if unopenable {
            s.unopenable.push(pid);
        }
    }

    /// Makes `exists` answer `answer` for `pid`, whatever `info` says (a process that cannot be
    /// read, or an existence the OS cannot tell).
    pub(crate) fn set_exists(&self, pid: u32, answer: Option<bool>) {
        self.state().exists.insert(pid, answer);
    }

    /// The groups `owner` holds now (L31).
    pub(crate) fn held(&self, owner: &str) -> Vec<RunGroup> {
        self.state().held.get(owner).cloned().unwrap_or_default()
    }

    /// `info` calls so far (what a tick costs on a real table).
    pub(crate) fn info_calls(&self) -> usize {
        self.info_calls.load(Ordering::SeqCst)
    }

    /// `parent_lookup` calls so far (a process snapshot each on Windows).
    pub(crate) fn parent_lookups(&self) -> usize {
        self.parent_lookups.load(Ordering::SeqCst)
    }
}

impl ProcessTable for FakeTable {
    fn boot_id(&self) -> String {
        self.state().boot_id.clone()
    }

    fn info(&self, pid: u32) -> Option<ProcInfo> {
        self.info_calls.fetch_add(1, Ordering::SeqCst);
        let s = self.state();
        let mut info = s.procs.get(&pid).cloned()?;
        if s.unopenable.contains(&pid) {
            info.start_time = None;
        }
        Some(info)
    }

    /// One "snapshot" per walk (counted as a parent lookup), with every start time.
    fn lineage_lookup(&self) -> LineageLookup<'_> {
        self.parent_lookups.fetch_add(1, Ordering::SeqCst);
        Box::new(move |pid| match self.state().procs.get(&pid) {
            Some(info) => (info.parent, info.start_time),
            None => (None, None),
        })
    }

    fn parent_lookup(&self) -> ParentLookup<'_> {
        self.parent_lookups.fetch_add(1, Ordering::SeqCst);
        Box::new(move |pid| self.state().procs.get(&pid).and_then(|i| i.parent))
    }

    fn process_name(&self, pid: u32) -> Option<String> {
        self.state().names.get(&pid).cloned()
    }

    fn exists(&self, pid: u32) -> Option<bool> {
        let s = self.state();
        s.exists
            .get(&pid)
            .copied()
            .unwrap_or_else(|| Some(s.procs.contains_key(&pid)))
    }

    fn group_alive(&self, group: &RunGroup) -> bool {
        self.state().live_groups.contains(group)
    }

    fn hold_group(&self, owner: &'static str, group: &RunGroup) {
        let mut s = self.state();
        let held = s.held.entry(owner).or_default();
        if !held.contains(group) {
            held.push(group.clone());
        }
    }

    fn hold_only(&self, owner: &'static str, groups: &[RunGroup]) {
        let mut s = self.state();
        let held = s.held.entry(owner).or_default();
        held.retain(|g| groups.contains(g));
        for g in groups {
            if !held.contains(g) {
                held.push(g.clone());
            }
        }
    }

    fn job_state(&self, group: &RunGroup) -> JobState {
        let s = self.state();
        if s.unknown_groups.contains(group) {
            JobState::Unknown
        } else if s.not_found_groups.contains(group) {
            JobState::NotFound
        } else if s.live_groups.contains(group) {
            JobState::Alive
        } else {
            JobState::Gone
        }
    }
}
