//! An in-memory process table for unit tests (this module and `heavy/engine.rs`). Methods take
//! `&self` so a test can change it while an engine holds it as `Arc<dyn ProcessTable>`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{ParentLookup, ProcInfo, ProcessTable, RunGroup};

pub(crate) struct FakeTable {
    state: Mutex<State>,
    info_calls: AtomicUsize,
    parent_lookups: AtomicUsize,
}

struct State {
    boot_id: String,
    procs: HashMap<u32, ProcInfo>,
    live_groups: Vec<RunGroup>,
}

impl FakeTable {
    pub(crate) fn new(boot_id: &str) -> Self {
        Self {
            state: Mutex::new(State {
                boot_id: boot_id.into(),
                procs: HashMap::new(),
                live_groups: Vec::new(),
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
}

impl FakeTable {
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
        self.state().procs.get(&pid).cloned()
    }

    fn parent_lookup(&self) -> ParentLookup<'_> {
        self.parent_lookups.fetch_add(1, Ordering::SeqCst);
        Box::new(move |pid| self.state().procs.get(&pid).and_then(|i| i.parent))
    }

    fn group_alive(&self, group: &RunGroup) -> bool {
        self.state().live_groups.contains(group)
    }
}
