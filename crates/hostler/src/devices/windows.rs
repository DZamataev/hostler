//! Claim windows from the claims lifecycle, for install attribution (P3b Task 8). Not
//! Microsoft Windows: the time windows of device claims.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use super::installs::WindowSnapshots;
use super::service::{DevicesService, keys};
use crate::contract::claims::{ClaimKind, ClaimsSnapshot, ResourceId, ResourceKind};
use crate::contract::heavy::{HistoryQuery, MAX_HISTORY_LIMIT};

/// One device claim's window: from its grant to its end (P-5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimWindow {
    pub claim_id: String,
    pub resource: ResourceId,
    pub workspace_id: String,
    pub workspace_path: String,
    pub branch: Option<String>,
    pub head_commit: Option<String>,
    pub command: Option<String>,
    pub from_ms: i64,
    pub to_ms: Option<i64>,
}

/// How far back the history catch-up looks before the last processed time (P2-21).
const CATCH_UP_SLACK_MS: i64 = 60_000;
/// A window's snapshot record is kept this long after its close snapshot.
const KEEP_SNAPSHOTS_MS: i64 = 60 * 60 * 1000;
/// The plan's hourly pruning: install history after 90 days, closed windows after 30.
const PRUNE_EVERY_MS: i64 = 60 * 60 * 1000;
const INSTALLS_KEPT_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const WINDOWS_KEPT_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// What this service remembers in memory for Task 8: the windows' snapshots, the windows that
/// wait for their close snapshot, and each device's last snapshot and telemetry attempt.
#[derive(Default)]
pub struct Book(Mutex<BookState>);

#[derive(Default)]
pub struct BookState {
    started: bool,
    /// The history is caught up to here (P2-21).
    processed_ms: Option<i64>,
    snapshots: HashMap<String, WindowSnapshots>,
    /// Closed windows waiting for a snapshot that starts after their end: `(claim id, to_ms)`.
    closing: HashMap<ResourceId, Vec<(String, i64)>>,
    pub snapshot_tried: HashMap<ResourceId, i64>,
    /// Failed snapshot reads in a row, and when an owed one (claim-open, close) may be tried
    /// again (R3-1, R3-2). Cleared by a good read, a new window and a new close.
    pub snapshot_retry: HashMap<ResourceId, (u32, i64)>,
    pub telemetry_tried: HashMap<ResourceId, i64>,
    pruned_ms: Option<i64>,
}

impl Book {
    pub fn lock(&self) -> MutexGuard<'_, BookState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the hourly pruning is due at `now_ms`; marks it done.
    pub fn prune_due(&self, now_ms: i64) -> bool {
        let mut book = self.lock();
        if book
            .pruned_ms
            .is_some_and(|at| now_ms - at < PRUNE_EVERY_MS)
        {
            return false;
        }
        book.pruned_ms = Some(now_ms);
        true
    }
}

/// Install history older than 90 days and windows closed more than 30 days ago go; an open
/// window never does. App rows are replaced per snapshot, one set per device: not pruned.
pub fn prune(svc: &DevicesService, now_ms: i64) {
    let store = svc.store();
    if let Err(e) = store.prune_device_installs(now_ms - INSTALLS_KEPT_MS) {
        tracing::warn!("cannot prune the install history: {e:#}");
    }
    if let Err(e) = store.prune_claim_windows(now_ms - WINDOWS_KEPT_MS) {
        tracing::warn!("cannot prune the claim windows: {e:#}");
    }
}

impl BookState {
    /// `windows` with what is known of their snapshots.
    pub fn with_snapshots(&self, windows: Vec<ClaimWindow>) -> Vec<(ClaimWindow, WindowSnapshots)> {
        windows
            .into_iter()
            .map(|w| {
                let s = self.snapshots.get(&w.claim_id).cloned().unwrap_or_default();
                (w, s)
            })
            .collect()
    }

    /// The windows pass closed `w`: a snapshot that starts after `to_ms` is its close snapshot,
    /// unless a snapshot already found the claim ended and took that role (R2-1).
    fn closed(&mut self, w: &ClaimWindow, to_ms: i64) {
        if self.close_recorded(&w.claim_id) {
            return;
        }
        self.snapshot_retry.remove(&w.resource);
        self.closing
            .entry(w.resource.clone())
            .or_default()
            .push((w.claim_id.clone(), to_ms));
    }

    /// Whether a snapshot started inside this claim's window (its first in-window one).
    pub fn has_in_window(&self, claim_id: &str) -> bool {
        self.snapshots
            .get(claim_id)
            .is_some_and(|s| s.first_in_window_ms.is_some())
    }

    /// The latest end among `r`'s windows still waiting for their close snapshot.
    pub fn pending_close_ms(&self, r: &ResourceId) -> Option<i64> {
        self.closing.get(r)?.iter().map(|(_, to)| *to).max()
    }

    fn close_recorded(&self, claim_id: &str) -> bool {
        self.snapshots
            .get(claim_id)
            .is_some_and(|s| s.close_at_ms.is_some())
    }

    /// A good snapshot of `r` that started at `started_ms` and finished at `at_ms`, against
    /// `windows` as the claims engine has them now (an ended claim's window carries its end,
    /// whether or not the windows pass closed the row yet): the first in-window snapshot of each
    /// window it started in, and the close snapshot of each window that ended before it started.
    /// `true` while a window still waits for its close snapshot (another one is owed).
    pub fn snapshot_taken(
        &mut self,
        r: &ResourceId,
        windows: &[ClaimWindow],
        started_ms: i64,
        at_ms: i64,
    ) -> bool {
        for w in windows.iter().filter(|w| w.resource == *r) {
            let s = self.snapshots.entry(w.claim_id.clone()).or_default();
            match w.to_ms {
                // Ended: never in-window again; the close snapshot of a window that had one in
                // it (an older window with no record here is past attributing).
                Some(to) if to <= started_ms => {
                    if s.close_at_ms.is_none() && s.first_in_window_ms.is_some() {
                        s.close_started_ms = Some(started_ms);
                        s.close_at_ms = Some(at_ms);
                    }
                }
                _ => {
                    if started_ms >= w.from_ms && s.first_in_window_ms.is_none() {
                        s.first_in_window_ms = Some(at_ms);
                    }
                }
            }
        }
        let pending = self.closing.remove(r).unwrap_or_default();
        let (done, waiting): (Vec<_>, Vec<_>) =
            pending.into_iter().partition(|(_, to)| *to <= started_ms);
        for (id, _) in done {
            let s = self.snapshots.entry(id).or_default();
            if s.close_at_ms.is_none() {
                s.close_started_ms = Some(started_ms);
                s.close_at_ms = Some(at_ms);
            }
        }
        self.snapshots.retain(|_, s| {
            s.close_at_ms
                .is_none_or(|at| at_ms - at < KEEP_SNAPSHOTS_MS)
        });
        let waiting: Vec<_> = waiting
            .into_iter()
            .filter(|(id, _)| !self.close_recorded(id))
            .collect();
        let owed = !waiting.is_empty();
        if owed {
            self.closing.insert(r.clone(), waiting);
        }
        owed
    }
}

/// Runs on the blocking pool for every `ClaimsChanged`, in order, and at startup against the
/// live snapshot: opens a window for each live device lease or hold without one, closes the
/// windows of claims that are no longer live, and catches up from `claims.history` what a lagged
/// broadcast hid. `Book` is never held across a store call.
pub fn on_claims(svc: &Arc<DevicesService>, snapshot: &ClaimsSnapshot, now_ms: i64) {
    let (startup, processed_ms) = {
        let mut book = svc.book().lock();
        (
            !std::mem::replace(&mut book.started, true),
            book.processed_ms,
        )
    };
    let live: HashMap<&str, (&ResourceId, &crate::contract::claims::Claim)> = snapshot
        .resources
        .iter()
        .filter(|s| s.resource.kind == ResourceKind::Device)
        .flat_map(|s| s.claims.iter().map(move |c| (&s.resource.id, c)))
        .filter(|(_, c)| matches!(c.kind, ClaimKind::Lease | ClaimKind::Hold))
        .map(|(r, c)| (c.id.as_str(), (r, c)))
        .collect();

    for (r, c) in live.values() {
        if svc.store().has_claim_window(&c.id).unwrap_or(true) {
            continue;
        }
        // R1-2: the head as the window opens, read before the row exists, so no install event
        // can name a missing or a later one.
        let (branch, head_commit) = workspace_head(svc, &c.workspace.id);
        let window = ClaimWindow {
            claim_id: c.id.clone(),
            resource: (*r).clone(),
            workspace_id: c.workspace.id.clone(),
            workspace_path: c.workspace.path.clone(),
            branch,
            head_commit,
            command: c.command.clone(),
            from_ms: c.granted_at_ms,
            to_ms: None,
        };
        // An iOS window's claim-open snapshot: due from now until one starts inside it (F-3:
        // also when the device is still shut down or booting).
        if svc.store().insert_claim_window(&window).unwrap_or(false) {
            svc.book().lock().snapshot_retry.remove(&window.resource);
            if window.resource.0.starts_with("device:ios:") {
                svc.wake_snapshots();
            }
        }
    }

    // A claim the startup pass finds ended (or reaped meanwhile) closes at the last alive
    // time, not at its end as this start recorded it (P2-21).
    let alive_ms = startup
        .then(|| {
            svc.store()
                .setting(keys::ALIVE_MS)
                .ok()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok())
        })
        .flatten();
    let mut closed: Vec<(ClaimWindow, i64)> = Vec::new();
    let open_windows = svc.store().open_claim_windows().unwrap_or_default();
    for w in open_windows
        .into_iter()
        .filter(|w| !live.contains_key(w.claim_id.as_str()))
    {
        let ended = svc
            .store()
            .history_item_of(&w.claim_id)
            .ok()
            .flatten()
            .map_or(now_ms, |h| h.ended_at_ms);
        let to = match alive_ms {
            Some(alive) if ended > alive => alive.max(w.from_ms),
            _ => ended,
        };
        if svc
            .store()
            .close_claim_window(&w.claim_id, to)
            .unwrap_or(false)
        {
            closed.push((w, to));
        }
    }

    // P2-21: a short claim a lagged broadcast hid still gets its window, from its history. Its
    // head is not read: today's would be later than its grant.
    if let Some(since) = processed_ms.or(alive_ms) {
        let q = HistoryQuery {
            since_ms: Some(since - CATCH_UP_SLACK_MS),
            limit: MAX_HISTORY_LIMIT,
        };
        let items = svc.store().claim_history(&q).unwrap_or_default();
        let workspaces = svc.store().workspaces().unwrap_or_default();
        for h in items {
            let Some(from_ms) = h.granted_at_ms else {
                continue;
            };
            if !h.resource.starts_with("device:")
                || !matches!(h.kind, ClaimKind::Lease | ClaimKind::Hold)
                || svc.store().has_claim_window(&h.id).unwrap_or(true)
            {
                continue;
            }
            let window = ClaimWindow {
                claim_id: h.id.clone(),
                resource: ResourceId(h.resource.clone()),
                workspace_id: workspaces
                    .iter()
                    .find(|w| w.path == h.workspace_path)
                    .map(|w| w.id.clone())
                    .unwrap_or_default(),
                workspace_path: h.workspace_path.clone(),
                branch: None,
                head_commit: None,
                command: h.command.clone(),
                from_ms,
                to_ms: Some(h.ended_at_ms),
            };
            if svc.store().insert_claim_window(&window).unwrap_or(false) {
                closed.push((window, h.ended_at_ms));
            }
        }
    }

    // Memory last, then the close snapshots: one that starts now sees its window waiting.
    {
        let mut book = svc.book().lock();
        for (w, to) in &closed {
            book.closed(w, *to);
        }
        book.processed_ms = Some(now_ms);
    }
    for (w, _) in &closed {
        svc.snapshot_soon(&w.resource);
    }
}

/// The branch and commit of a workspace's checkout now, from its git files.
fn workspace_head(svc: &DevicesService, workspace_id: &str) -> (Option<String>, Option<String>) {
    let git_dir = svc
        .store()
        .workspace(workspace_id)
        .ok()
        .flatten()
        .map(|w| PathBuf::from(w.git_dir));
    match git_dir {
        Some(git_dir) => head_of(&git_dir, &common_dir_of(&git_dir)),
        None => (None, None),
    }
}

/// A worktree's shared git dir (its `commondir` file), else the git dir itself.
pub fn common_dir_of(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let dir = Path::new(text.trim());
            if dir.is_absolute() {
                dir.to_owned()
            } else {
                git_dir.join(dir)
            }
        }
        Err(_) => git_dir.to_owned(),
    }
}

/// The branch and commit of `HEAD`, from files only (`HEAD`, loose refs, `packed-refs`): a
/// detached `HEAD` has no branch.
pub fn head_of(git_dir: &Path, common_dir: &Path) -> (Option<String>, Option<String>) {
    let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) else {
        return (None, None);
    };
    let head = head.trim();
    let Some(name) = head.strip_prefix("ref:").map(str::trim) else {
        return (None, is_commit(head).then(|| head.to_owned()));
    };
    let branch = name.strip_prefix("refs/heads/").unwrap_or(name).to_owned();
    (Some(branch), ref_commit(git_dir, common_dir, name))
}

fn ref_commit(git_dir: &Path, common_dir: &Path, name: &str) -> Option<String> {
    let mut seen = HashSet::new();
    let mut name = name.to_owned();
    // A symbolic ref may name another; a cycle or a long chain is no commit.
    while seen.insert(name.clone()) && seen.len() <= 5 {
        if name.split('/').any(|part| part.is_empty() || part == "..") {
            return None;
        }
        let loose = [git_dir, common_dir]
            .iter()
            .find_map(|dir| std::fs::read_to_string(dir.join(&name)).ok());
        match loose.as_deref().map(str::trim) {
            Some(text) if is_commit(text) => return Some(text.to_owned()),
            Some(text) => name = text.strip_prefix("ref:")?.trim().to_owned(),
            None => return packed(common_dir, &name),
        }
    }
    None
}

fn packed(common_dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    text.lines().find_map(|line| {
        let (commit, r) = line.split_once(' ')?;
        (r.trim() == name && is_commit(commit)).then(|| commit.to_owned())
    })
}

/// A full SHA-1 or SHA-256 object name.
fn is_commit(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const C1: &str = "1111111111111111111111111111111111111111";
    const C2: &str = "2222222222222222222222222222222222222222";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn head_of_reads_branch_and_commit_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("repo/.git");
        // A loose ref.
        write(&git.join("HEAD"), "ref: refs/heads/feature/x\n");
        write(&git.join("refs/heads/feature/x"), &format!("{C1}\n"));
        assert_eq!(
            head_of(&git, &common_dir_of(&git)),
            (Some("feature/x".into()), Some(C1.into()))
        );
        // A packed ref.
        std::fs::remove_file(git.join("refs/heads/feature/x")).unwrap();
        write(
            &git.join("packed-refs"),
            &format!("# pack-refs with: peeled fully-peeled sorted\n{C2} refs/heads/feature/x\n"),
        );
        assert_eq!(
            head_of(&git, &common_dir_of(&git)),
            (Some("feature/x".into()), Some(C2.into()))
        );
        // A branch with no commit yet.
        write(&git.join("HEAD"), "ref: refs/heads/new\n");
        assert_eq!(head_of(&git, &git), (Some("new".into()), None));
        // A detached HEAD.
        write(&git.join("HEAD"), &format!("{C1}\n"));
        assert_eq!(head_of(&git, &git), (None, Some(C1.into())));
        // A worktree: its own HEAD, the refs in the common dir.
        let wt = git.join("worktrees/wt");
        write(&wt.join("HEAD"), "ref: refs/heads/wt-branch\n");
        write(&wt.join("commondir"), "../..\n");
        write(&git.join("refs/heads/wt-branch"), &format!("{C2}\n"));
        let common = common_dir_of(&wt);
        assert_eq!(
            std::fs::canonicalize(&common).unwrap(),
            std::fs::canonicalize(&git).unwrap()
        );
        assert_eq!(
            head_of(&wt, &common),
            (Some("wt-branch".into()), Some(C2.into()))
        );
        // No checkout: nothing, never an error.
        assert_eq!(head_of(&dir.path().join("none"), &git), (None, None));
    }

    struct Fixed(i64);

    impl crate::heavy::engine::Clock for Fixed {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    /// A device lease granted at `from` and ended at `to`, straight into the history (no live
    /// snapshot ever showed it).
    fn ended_lease(store: &crate::store::Store, id: &str, from: i64, to: i64) {
        store
            .insert_claim(&crate::store::ClaimRow {
                id: id.into(),
                resource: ResourceId("device:android:X".into()),
                kind: ClaimKind::Lease,
                parent: None,
                workspace_id: "w".into(),
                holder: None,
                child: None,
                group: None,
                binding: None,
                command: Some("make test".into()),
                granted_at_ms: from,
            })
            .unwrap();
        assert!(
            store
                .end_claim(id, crate::contract::claims::EndReason::Released, None, to)
                .unwrap()
        );
    }

    /// P2-21: a short claim that no processed `ClaimsChanged` showed (a lagged broadcast) gets
    /// its window from `claims.history`, at the start pass and at later passes.
    #[test]
    fn lagged_broadcast_rebuilds_windows() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/app/.git", "app", 1).unwrap();
        store
            .upsert_workspace("w", &project.id, "/work/app", "/work/app/.git", 1)
            .unwrap();
        store.set_setting(keys::ALIVE_MS, Some("1000")).unwrap();
        ended_lease(&store, "hidden1", 2_000, 3_000);
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let (svc, _) = DevicesService::with_clock(
            store,
            super::super::tools::ToolPaths::default(),
            tx,
            Arc::new(Fixed(10_000)),
        );
        let empty = ClaimsSnapshot {
            resources: Vec::new(),
        };
        on_claims(&svc, &empty, 10_000);
        ended_lease(&svc.store(), "hidden2", 11_000, 12_000);
        on_claims(&svc, &empty, 13_000);
        let r = ResourceId("device:android:X".into());
        let windows = svc.store().claim_windows_on(&r, 0, i64::MAX).unwrap();
        let got: Vec<_> = windows
            .iter()
            .map(|w| {
                (
                    w.claim_id.as_str(),
                    w.from_ms,
                    w.to_ms,
                    w.command.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("hidden1", 2_000, Some(3_000), Some("make test")),
                ("hidden2", 11_000, Some(12_000), Some("make test")),
            ]
        );
        // Each waits for a close snapshot.
        assert_eq!(svc.book().lock().closing[&r].len(), 2);
    }

    /// A device service on an in-memory store with workspaces `w` (/work/app) and `w2`
    /// (/work/other), and the claims engine's records written straight into its tables.
    struct Race {
        svc: Arc<DevicesService>,
        r: ResourceId,
    }

    impl Race {
        fn new() -> Race {
            let store = crate::store::Store::open_in_memory().unwrap();
            let project = store.upsert_project("/work/app/.git", "app", 1).unwrap();
            store
                .upsert_workspace("w", &project.id, "/work/app", "/work/app/.git", 1)
                .unwrap();
            let other = store
                .upsert_project("/work/other/.git", "other", 1)
                .unwrap();
            store
                .upsert_workspace("w2", &other.id, "/work/other", "/work/other/.git", 1)
                .unwrap();
            let (tx, _) = tokio::sync::broadcast::channel(16);
            let (svc, _) = DevicesService::with_clock(
                store,
                super::super::tools::ToolPaths::default(),
                tx,
                Arc::new(Fixed(0)),
            );
            Race {
                svc,
                r: ResourceId("device:ios:SIM".into()),
            }
        }

        /// The engine grants a hold (the windows pass has not run).
        fn grant(&self, id: &str, workspace: &str, from: i64) {
            self.svc
                .store()
                .insert_claim(&crate::store::ClaimRow {
                    id: id.into(),
                    resource: self.r.clone(),
                    kind: ClaimKind::Hold,
                    parent: None,
                    workspace_id: workspace.into(),
                    holder: None,
                    child: None,
                    group: None,
                    binding: None,
                    command: None,
                    granted_at_ms: from,
                })
                .unwrap();
        }

        /// The engine ends it: its history row, its live row gone.
        fn end(&self, id: &str, to: i64) {
            self.svc
                .store()
                .end_claim(id, crate::contract::claims::EndReason::Released, None, to)
                .unwrap();
        }

        /// The windows pass recorded its window (head `main`).
        fn row(&self, id: &str, from: i64) {
            self.svc
                .store()
                .insert_claim_window(&ClaimWindow {
                    resource: self.r.clone(),
                    workspace_id: "w".into(),
                    workspace_path: "/work/app".into(),
                    branch: Some("main".into()),
                    ..window(id, from, None)
                })
                .unwrap();
        }

        /// One good snapshot that started at `started` and finished at `at`, after one at
        /// `prev`: an install between them, or nothing.
        fn snapshot(
            &self,
            prev: i64,
            started: i64,
            at: i64,
            change: bool,
        ) -> (bool, Vec<crate::contract::devices::InstallSource>) {
            use crate::contract::devices::{InstallChange, InstallTime};
            let changes = if change {
                vec![(
                    "com.example.new".to_owned(),
                    InstallChange::Installed,
                    InstallTime::ChangedBetween {
                        from_ms: prev,
                        to_ms: at,
                    },
                )]
            } else {
                Vec::new()
            };
            crate::devices::installs::account(&self.svc, &self.r, &changes, prev, started, at)
        }

        fn late_pass(&self, now: i64) {
            let empty = ClaimsSnapshot {
                resources: Vec::new(),
            };
            on_claims(&self.svc, &empty, now);
        }

        fn owed(&self) -> bool {
            self.svc
                .book()
                .lock()
                .closing
                .get(&self.r)
                .is_some_and(|c| !c.is_empty())
        }
    }

    fn claim_of(sources: &[crate::contract::devices::InstallSource]) -> Option<Vec<String>> {
        match sources {
            [crate::contract::devices::InstallSource::Claim { claim_ids, .. }] => {
                Some(claim_ids.clone())
            }
            _ => None,
        }
    }

    fn around_of(sources: &[crate::contract::devices::InstallSource]) -> Option<Vec<String>> {
        match sources {
            [crate::contract::devices::InstallSource::Around { claim_ids, .. }] => {
                Some(claim_ids.clone())
            }
            _ => None,
        }
    }

    /// R2-1: a claim ended in the engine, its windows pass not run yet (the row still open, no
    /// close owed). A snapshot that starts after the end is never in-window: a change it shows
    /// is `Around`, not `Claim`, unless it qualifies as the close snapshot (started within 30 s,
    /// the plan's close rule). When the pass runs late, nothing is closed twice or left owed.
    #[test]
    fn a_snapshot_before_the_windows_pass_sees_the_claim_ended() {
        let t = Race::new();
        // c1: an in-window snapshot, then the claim ends at 20 s; the pass is held back.
        t.grant("c1", "w", 10_000);
        t.row("c1", 10_000);
        assert_eq!(t.snapshot(11_000, 11_000, 12_000, false), (false, vec![]));
        t.end("c1", 20_000);
        // 40 s after the end (no close snapshot by the plan's 30 s rule): around it.
        let (owed, sources) = t.snapshot(12_000, 60_000, 61_000, true);
        assert!(!owed);
        assert_eq!(around_of(&sources), Some(vec!["c1".into()]), "{sources:?}");
        // The pass runs late: it closes the row, nothing waits for another close snapshot.
        t.late_pass(62_000);
        assert!(t.svc.store().open_claim_windows().unwrap().is_empty());
        assert!(!t.owed());
        assert_eq!(
            t.svc.book().lock().snapshots["c1"].close_started_ms,
            Some(60_000)
        );

        // c2, the normal short hold with the pass still behind: its close snapshot started 1 s
        // after the end attributes the change to it.
        t.grant("c2", "w", 100_000);
        t.row("c2", 100_000);
        t.snapshot(101_000, 101_000, 102_000, false);
        t.end("c2", 105_000);
        let (owed, sources) = t.snapshot(102_000, 106_000, 107_000, true);
        assert!(!owed);
        assert_eq!(claim_of(&sources), Some(vec!["c2".into()]), "{sources:?}");
        t.late_pass(108_000);
        assert!(!t.owed(), "not owed again");
        assert_eq!(
            t.svc.book().lock().snapshots["c2"].close_at_ms,
            Some(107_000),
            "not closed twice"
        );
    }

    /// R3-1: the claims engine's own records count, not only window rows. W ends at 100 s; the
    /// next claim N (another workspace) is granted at 101 s, its row not written yet; W's close
    /// snapshot starts at 110 s, inside 30 s: N opened in between, so the change is `Around`.
    #[test]
    fn a_next_claim_the_windows_pass_has_not_recorded_blocks_the_close_rule() {
        let t = Race::new();
        t.grant("w1", "w", 90_000);
        t.row("w1", 90_000);
        t.snapshot(91_000, 91_000, 92_000, false);
        t.end("w1", 100_000);
        t.grant("n1", "w2", 101_000);
        let (_, sources) = t.snapshot(92_000, 110_000, 111_000, true);
        let around = around_of(&sources).unwrap_or_else(|| panic!("{sources:?}"));
        assert!(around.contains(&"w1".to_string()) && around.contains(&"n1".to_string()));
        assert!(
            t.svc
                .store()
                .claim_windows_on(&t.r, 0, i64::MAX)
                .unwrap()
                .len()
                == 1
        );

        // A short third claim that lived entirely inside the gap, never recorded either.
        let t = Race::new();
        t.grant("w1", "w", 90_000);
        t.row("w1", 90_000);
        t.snapshot(91_000, 91_000, 92_000, false);
        t.end("w1", 100_000);
        t.grant("g1", "w2", 102_000);
        t.end("g1", 104_000);
        let (_, sources) = t.snapshot(92_000, 110_000, 111_000, true);
        let around = around_of(&sources).unwrap_or_else(|| panic!("{sources:?}"));
        assert!(around.contains(&"g1".to_string()), "{sources:?}");

        // The late pass: it records g1 from the history and closes w1; nothing closed twice,
        // nothing owed; the next normal short hold still gets `Claim` by its close snapshot.
        t.late_pass(112_000);
        assert!(t.svc.store().open_claim_windows().unwrap().is_empty());
        let close = t.svc.book().lock().snapshots["w1"].close_at_ms;
        assert_eq!(close, Some(111_000));
        // g1's window (recorded late from history) waits for one snapshot after its end.
        let (owed, _) = t.snapshot(111_000, 113_000, 114_000, false);
        assert!(!owed && !t.owed());
        t.grant("c3", "w", 200_000);
        t.row("c3", 200_000);
        t.snapshot(201_000, 201_000, 202_000, false);
        t.end("c3", 205_000);
        let (_, sources) = t.snapshot(202_000, 206_000, 207_000, true);
        assert_eq!(claim_of(&sources), Some(vec!["c3".into()]), "{sources:?}");
    }

    /// L68 (R4-1b): a claim found both live and ended is one blocker, the ended one.
    #[test]
    fn a_claim_in_both_records_is_one_blocker() {
        let t = Race::new();
        t.grant("x1", "w2", 50_000);
        t.end("x1", 55_000);
        t.grant("x1", "w2", 50_000);
        let (_, sources) = t.snapshot(40_000, 60_000, 61_000, true);
        assert_eq!(around_of(&sources), Some(vec!["x1".into()]), "{sources:?}");
    }

    /// R2-1 residue: an open row whose claim is neither live nor in history (a pruned history
    /// row) has an unknown end: never in-window, never `Claim`, `Around` what it overlaps.
    #[test]
    fn an_open_row_without_a_live_or_ended_claim_is_never_claim() {
        let t = Race::new();
        t.grant("c1", "w", 10_000);
        t.row("c1", 10_000);
        t.snapshot(11_000, 11_000, 12_000, false);
        t.end("c1", 20_000);
        t.svc.store().prune_claim_history(30_000).unwrap();
        // Even 1 s after the real end, where a close snapshot would count.
        let (_, sources) = t.snapshot(12_000, 21_000, 22_000, true);
        assert_eq!(around_of(&sources), Some(vec!["c1".into()]), "{sources:?}");
        let (_, sources) = t.snapshot(22_000, 60_000, 61_000, true);
        assert_eq!(around_of(&sources), Some(vec!["c1".into()]), "{sources:?}");
        // The late pass closes it (at the pass time); one snapshot after that, nothing owed.
        t.late_pass(62_000);
        assert!(t.svc.store().open_claim_windows().unwrap().is_empty());
        let (owed, _) = t.snapshot(61_000, 63_000, 64_000, false);
        assert!(!owed && !t.owed());
    }

    fn window(id: &str, from: i64, to: Option<i64>) -> ClaimWindow {
        ClaimWindow {
            claim_id: id.into(),
            resource: ResourceId("device:ios:SIM".into()),
            workspace_id: "w".into(),
            workspace_path: "/w".into(),
            branch: None,
            head_commit: None,
            command: None,
            from_ms: from,
            to_ms: to,
        }
    }

    /// The first snapshot that starts inside a window is its first in-window one; the first
    /// that starts after its end is its close snapshot; one that started before the end leaves
    /// the close owed.
    #[test]
    fn snapshots_are_matched_to_windows() {
        let r = ResourceId("device:ios:SIM".into());
        let mut book = BookState::default();
        let w = window("c1", 1_000, None);
        assert!(!book.snapshot_taken(&r, std::slice::from_ref(&w), 1_500, 1_800));
        assert!(!book.snapshot_taken(&r, std::slice::from_ref(&w), 2_500, 2_800));
        assert_eq!(book.snapshots["c1"].first_in_window_ms, Some(1_800));
        let w = window("c1", 1_000, Some(3_000));
        book.closed(&w, 3_000);
        // Started before the end: not the close snapshot, one is still owed.
        assert!(book.snapshot_taken(&r, std::slice::from_ref(&w), 2_900, 3_100));
        assert!(!book.snapshot_taken(&r, std::slice::from_ref(&w), 3_200, 3_400));
        let s = &book.snapshots["c1"];
        assert_eq!(
            (s.first_in_window_ms, s.close_started_ms, s.close_at_ms),
            (Some(1_800), Some(3_200), Some(3_400))
        );
    }
}
