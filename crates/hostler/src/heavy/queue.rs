//! Pure heavy-queue decisions: who may run, queue position, and which holders are dead.

use crate::contract::heavy::{HeavyEntry, HeavyState};

/// The ids of the first `free` waiting entries by ticket, where
/// `free = capacity - running - foreign_running`.
pub fn grantable(capacity: u8, entries: &[HeavyEntry], foreign_running: usize) -> Vec<String> {
    let running = entries
        .iter()
        .filter(|entry| entry.state == HeavyState::Running)
        .count();
    let free = usize::from(capacity)
        .saturating_sub(running)
        .saturating_sub(foreign_running);

    let mut waiting: Vec<&HeavyEntry> = entries
        .iter()
        .filter(|entry| entry.state == HeavyState::Waiting)
        .collect();
    waiting.sort_by_key(|entry| entry.ticket);
    waiting
        .into_iter()
        .take(free)
        .map(|entry| entry.id.clone())
        .collect()
}

/// The 1-based position of `id` among waiting entries, plus the number waiting.
pub fn position(entries: &[HeavyEntry], id: &str) -> Option<(u32, u32)> {
    let mut waiting: Vec<&HeavyEntry> = entries
        .iter()
        .filter(|entry| entry.state == HeavyState::Waiting)
        .collect();
    waiting.sort_by_key(|entry| entry.ticket);
    let of = waiting.len() as u32;
    let index = waiting.iter().position(|entry| entry.id == id)?;
    Some((index as u32 + 1, of))
}

/// The ids of entries whose holder, child and run group are all gone.
pub fn dead_entries(entries: &[HeavyEntry], live: &dyn Fn(&HeavyEntry) -> bool) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| !live(entry))
        .map(|entry| entry.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::heavy::EntrySource;
    use crate::procs::ProcessIdentity;

    fn entry(id: &str, ticket: u64, state: HeavyState) -> HeavyEntry {
        HeavyEntry {
            id: id.into(),
            ticket,
            kind: "gradle".into(),
            command: "./gradlew assemble".into(),
            cwd: "/work/app".into(),
            workspace_root: None,
            holder: ProcessIdentity {
                pid: 100,
                boot_id: "boot-1".into(),
                start_time: 500,
            },
            child: None,
            group: None,
            state,
            enqueued_at_ms: 1,
            granted_at_ms: None,
            source: EntrySource::Hostler,
        }
    }

    #[test]
    fn grants_in_ticket_order() {
        let entries = vec![
            entry("t3", 3, HeavyState::Waiting),
            entry("t1", 1, HeavyState::Waiting),
            entry("t2", 2, HeavyState::Waiting),
        ];

        assert_eq!(grantable(2, &entries, 0), ["t1", "t2"]);
        assert_eq!(grantable(9, &entries, 0), ["t1", "t2", "t3"]);
    }

    #[test]
    fn foreign_running_reduces_free_slots() {
        let entries = vec![
            entry("t1", 1, HeavyState::Waiting),
            entry("t2", 2, HeavyState::Waiting),
        ];

        assert_eq!(grantable(2, &entries, 1), ["t1"]);
        assert_eq!(grantable(2, &entries, 2), Vec::<String>::new());
    }

    #[test]
    fn no_grant_when_full() {
        let entries = vec![
            entry("running", 1, HeavyState::Running),
            entry("t2", 2, HeavyState::Waiting),
        ];

        assert!(grantable(1, &entries, 0).is_empty());
    }

    #[test]
    fn position_counts_only_waiting() {
        let entries = vec![
            entry("running", 1, HeavyState::Running),
            entry("t3", 3, HeavyState::Waiting),
            entry("t2", 2, HeavyState::Waiting),
        ];

        assert_eq!(position(&entries, "t2"), Some((1, 2)));
        assert_eq!(position(&entries, "t3"), Some((2, 2)));
        assert_eq!(position(&entries, "running"), None);
        assert_eq!(position(&entries, "missing"), None);
    }

    #[test]
    fn dead_entries_uses_predicate() {
        let entries = vec![
            entry("a", 1, HeavyState::Running),
            entry("b", 2, HeavyState::Waiting),
            entry("c", 3, HeavyState::Waiting),
        ];
        let live = |entry: &HeavyEntry| entry.id != "b";

        assert_eq!(dead_entries(&entries, &live), ["b"]);
    }
}
