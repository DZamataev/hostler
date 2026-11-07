//! The SQLite store behind the heavy queue (plan Task 5).

mod schema;

use std::path::Path;

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};

use crate::contract::heavy::{
    EntrySource, HeavyAcquire, HeavyEntry, HeavyHistoryItem, HeavyOutcome, HeavyState,
    HistoryQuery, MAX_HISTORY_LIMIT,
};
use crate::procs::{ProcessIdentity, RunGroup};

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the database and brings its schema up to date.
    pub fn open(path: &Path) -> anyhow::Result<Store> {
        let mut conn =
            Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        // Migrate before switching to WAL so a database from a newer hostler is never rewritten.
        schema::migrate(&mut conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("cannot enable WAL")?;
        Ok(Store { conn })
    }

    pub fn open_in_memory() -> anyhow::Result<Store> {
        let mut conn = Connection::open_in_memory()?;
        schema::migrate(&mut conn)?;
        Ok(Store { conn })
    }

    pub fn capacity(&self) -> anyhow::Result<u8> {
        match setting(&self.conn, "heavy.capacity")? {
            Some(raw) => raw.parse().context("heavy.capacity is not a number"),
            None => Ok(1),
        }
    }

    /// The engine validates 1..=16; the store only persists.
    pub fn set_capacity(&self, n: u8) -> anyhow::Result<()> {
        set_setting(&self.conn, "heavy.capacity", &n.to_string())
    }

    pub fn insert_waiting(&self, req: &HeavyAcquire, now_ms: i64) -> anyhow::Result<HeavyEntry> {
        let holder = serde_json::to_string(&req.holder).context("cannot encode the holder")?;
        let tx = self.conn.unchecked_transaction()?;
        let next_ticket = match setting(&tx, "heavy.next_ticket")?.as_deref() {
            Some(raw) => raw
                .parse::<i64>()
                .context("heavy.next_ticket is not an integer")?,
            None => 1,
        };
        set_setting(&tx, "heavy.next_ticket", &(next_ticket + 1).to_string())?;
        tx.execute(
            "INSERT INTO heavy_entries (id, ticket, kind, command, cwd, workspace_root, \
             holder_json, child_json, group_json, state, enqueued_at_ms, granted_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 'waiting', ?8, NULL)",
            params![
                req.id,
                next_ticket,
                req.kind,
                req.command,
                req.cwd,
                req.workspace_root,
                holder,
                now_ms
            ],
        )
        .context("cannot insert the heavy entry")?;
        tx.commit()?;

        Ok(HeavyEntry {
            id: req.id.clone(),
            ticket: next_ticket as u64,
            kind: req.kind.clone(),
            command: req.command.clone(),
            cwd: req.cwd.clone(),
            workspace_root: req.workspace_root.clone(),
            holder: req.holder.clone(),
            child: None,
            group: None,
            state: HeavyState::Waiting,
            enqueued_at_ms: now_ms,
            granted_at_ms: None,
            source: EntrySource::Hostler,
        })
    }

    /// All live entries, ordered by ticket.
    pub fn entries(&self) -> anyhow::Result<Vec<HeavyEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{ENTRY_COLUMNS} ORDER BY ticket"))?;
        let mut rows = stmt.query([])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            entries.push(entry_from_row(row)?);
        }
        Ok(entries)
    }

    pub fn entry(&self, id: &str) -> anyhow::Result<Option<HeavyEntry>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{ENTRY_COLUMNS} WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(entry_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn history_item(&self, id: &str) -> anyhow::Result<Option<HeavyHistoryItem>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{HISTORY_COLUMNS} WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(history_item_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn mark_running(&self, id: &str, now_ms: i64) -> anyhow::Result<()> {
        let changed = self.conn.execute(
            "UPDATE heavy_entries SET state = 'running', granted_at_ms = ?1 WHERE id = ?2",
            params![now_ms, id],
        )?;
        anyhow::ensure!(changed == 1, "heavy entry {id} is not queued");
        Ok(())
    }

    pub fn set_started(
        &self,
        id: &str,
        child: &ProcessIdentity,
        group: &RunGroup,
    ) -> anyhow::Result<()> {
        let child = serde_json::to_string(child).context("cannot encode the child")?;
        let group = serde_json::to_string(group).context("cannot encode the run group")?;
        let changed = self.conn.execute(
            "UPDATE heavy_entries SET child_json = ?1, group_json = ?2 WHERE id = ?3",
            params![child, group, id],
        )?;
        anyhow::ensure!(changed == 1, "heavy entry {id} is not queued");
        Ok(())
    }

    /// Moves a live entry to history in one transaction; false when it is already gone.
    pub fn finish(
        &self,
        id: &str,
        exit_code: Option<i32>,
        outcome: HeavyOutcome,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let moved = tx.execute(
            "INSERT INTO heavy_history (id, ticket, kind, command, cwd, workspace_root, \
             enqueued_at_ms, granted_at_ms, finished_at_ms, exit_code, outcome) \
             SELECT id, ticket, kind, command, cwd, workspace_root, enqueued_at_ms, \
             granted_at_ms, ?1, ?2, ?3 FROM heavy_entries WHERE id = ?4",
            params![now_ms, exit_code, outcome_str(outcome), id],
        )?;
        if moved == 0 {
            return Ok(false);
        }
        tx.execute("DELETE FROM heavy_entries WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Newest finished entry first.
    pub fn history(&self, q: &HistoryQuery) -> anyhow::Result<Vec<HeavyHistoryItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "{HISTORY_COLUMNS} WHERE (?1 IS NULL OR finished_at_ms >= ?1) \
             ORDER BY finished_at_ms DESC, ticket DESC LIMIT ?2"
        ))?;
        let limit = q.limit.min(MAX_HISTORY_LIMIT) as i64;
        let mut rows = stmt.query(params![q.since_ms, limit])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            items.push(history_item_from_row(row)?);
        }
        Ok(items)
    }

    pub fn prune_history(&self, older_than_ms: i64) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM heavy_history WHERE finished_at_ms < ?1",
            [older_than_ms],
        )?)
    }
}

const ENTRY_COLUMNS: &str = "SELECT id, ticket, kind, command, cwd, workspace_root, holder_json, \
                             child_json, group_json, state, enqueued_at_ms, granted_at_ms \
                             FROM heavy_entries";
const HISTORY_COLUMNS: &str = "SELECT id, ticket, kind, command, cwd, workspace_root, \
                               enqueued_at_ms, granted_at_ms, finished_at_ms, exit_code, outcome \
                               FROM heavy_history";

fn entry_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<HeavyEntry> {
    let holder_json: String = row.get(6)?;
    let child_json: Option<String> = row.get(7)?;
    let group_json: Option<String> = row.get(8)?;
    let state: String = row.get(9)?;

    Ok(HeavyEntry {
        id: row.get(0)?,
        ticket: row.get::<_, i64>(1)? as u64,
        kind: row.get(2)?,
        command: row.get(3)?,
        cwd: row.get(4)?,
        workspace_root: row.get(5)?,
        holder: serde_json::from_str(&holder_json).context("heavy_entries.holder_json")?,
        child: child_json
            .as_deref()
            .map(serde_json::from_str::<ProcessIdentity>)
            .transpose()
            .context("heavy_entries.child_json")?,
        group: group_json
            .as_deref()
            .map(serde_json::from_str::<RunGroup>)
            .transpose()
            .context("heavy_entries.group_json")?,
        state: parse_state(&state)?,
        enqueued_at_ms: row.get(10)?,
        granted_at_ms: row.get(11)?,
        source: EntrySource::Hostler,
    })
}

fn history_item_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<HeavyHistoryItem> {
    let outcome: String = row.get(10)?;
    Ok(HeavyHistoryItem {
        id: row.get(0)?,
        ticket: row.get::<_, i64>(1)? as u64,
        kind: row.get(2)?,
        command: row.get(3)?,
        cwd: row.get(4)?,
        workspace_root: row.get(5)?,
        enqueued_at_ms: row.get(6)?,
        granted_at_ms: row.get(7)?,
        finished_at_ms: row.get(8)?,
        exit_code: row.get(9)?,
        outcome: parse_outcome(&outcome)?,
    })
}

fn setting(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .optional()
    .with_context(|| format!("cannot read setting {key}"))
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn parse_state(raw: &str) -> anyhow::Result<HeavyState> {
    match raw {
        "waiting" => Ok(HeavyState::Waiting),
        "running" => Ok(HeavyState::Running),
        other => anyhow::bail!("unknown heavy entry state {other:?}"),
    }
}

fn outcome_str(outcome: HeavyOutcome) -> &'static str {
    match outcome {
        HeavyOutcome::Ok => "ok",
        HeavyOutcome::Failed => "failed",
        HeavyOutcome::Withdrawn => "withdrawn",
        HeavyOutcome::TimedOut => "timed_out",
        HeavyOutcome::Stopped => "stopped",
        HeavyOutcome::Reaped => "reaped",
    }
}

fn parse_outcome(raw: &str) -> anyhow::Result<HeavyOutcome> {
    match raw {
        "ok" => Ok(HeavyOutcome::Ok),
        "failed" => Ok(HeavyOutcome::Failed),
        "withdrawn" => Ok(HeavyOutcome::Withdrawn),
        "timed_out" => Ok(HeavyOutcome::TimedOut),
        "stopped" => Ok(HeavyOutcome::Stopped),
        "reaped" => Ok(HeavyOutcome::Reaped),
        other => anyhow::bail!("unknown heavy outcome {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: "boot-1".into(),
            start_time: 500,
        }
    }

    fn group(pgid: u32) -> RunGroup {
        RunGroup::UnixGroup {
            pgid,
            boot_id: "boot-1".into(),
        }
    }

    fn acquire(id: &str) -> HeavyAcquire {
        HeavyAcquire {
            id: id.into(),
            kind: "gradle".into(),
            command: "./gradlew assemble".into(),
            cwd: "/work/app".into(),
            workspace_root: Some("/work/app".into()),
            holder: identity(100),
            reentrant_token: None,
            no_wait: false,
        }
    }

    #[test]
    fn tickets_monotonic_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.insert_waiting(&acquire("a"), 1).unwrap().ticket, 1);
            assert_eq!(store.insert_waiting(&acquire("b"), 2).unwrap().ticket, 2);
            assert!(store.finish("a", Some(0), HeavyOutcome::Ok, 3).unwrap());
            assert!(store.finish("b", None, HeavyOutcome::Failed, 4).unwrap());
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.insert_waiting(&acquire("c"), 5).unwrap().ticket, 3);
    }

    #[test]
    fn tickets_monotonic_after_pruning_everything() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let store = Store::open(&path).unwrap();
            store.insert_waiting(&acquire("a"), 1).unwrap();
            store.insert_waiting(&acquire("b"), 2).unwrap();
            store.finish("a", Some(0), HeavyOutcome::Ok, 3).unwrap();
            store.finish("b", Some(0), HeavyOutcome::Ok, 4).unwrap();
            assert_eq!(store.prune_history(10).unwrap(), 2);
            assert!(store.history(&HistoryQuery::default()).unwrap().is_empty());
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.insert_waiting(&acquire("c"), 5).unwrap().ticket, 3);
    }

    #[test]
    fn finish_moves_to_history_atomically() {
        let store = Store::open_in_memory().unwrap();
        store.insert_waiting(&acquire("a"), 1).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER block_delete BEFORE DELETE ON heavy_entries \
                 BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
            )
            .unwrap();

        let err = store.finish("a", Some(0), HeavyOutcome::Ok, 2).unwrap_err();
        assert!(format!("{err:#}").contains("blocked"));
        assert!(
            store.entry("a").unwrap().is_some(),
            "the live entry must survive a failed finish"
        );
        assert!(
            store.history(&HistoryQuery::default()).unwrap().is_empty(),
            "no history row may be committed when the move fails"
        );

        store
            .conn
            .execute_batch("DROP TRIGGER block_delete")
            .unwrap();
        assert!(store.finish("a", Some(0), HeavyOutcome::Ok, 3).unwrap());
        assert!(store.entries().unwrap().is_empty());
        assert_eq!(store.entry("a").unwrap(), None);
        assert_eq!(store.history(&HistoryQuery::default()).unwrap().len(), 1);

        assert!(!store.finish("a", Some(0), HeavyOutcome::Ok, 4).unwrap());
    }

    #[test]
    fn entries_ordered_by_ticket() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .pragma_update(None, "reverse_unordered_selects", true)
            .unwrap();
        for id in ["a", "b", "c"] {
            store.insert_waiting(&acquire(id), 1).unwrap();
        }

        let tickets: Vec<u64> = store
            .entries()
            .unwrap()
            .iter()
            .map(|entry| entry.ticket)
            .collect();
        assert_eq!(tickets, [1, 2, 3]);
    }

    #[test]
    fn history_item_found_after_finish() {
        let store = Store::open_in_memory().unwrap();
        store.insert_waiting(&acquire("a"), 100).unwrap();
        store.mark_running("a", 200).unwrap();
        store.set_started("a", &identity(200), &group(200)).unwrap();

        let running = store.entry("a").unwrap().unwrap();
        assert_eq!(running.state, HeavyState::Running);
        assert_eq!(running.granted_at_ms, Some(200));
        assert_eq!(running.child, Some(identity(200)));
        assert_eq!(running.group, Some(group(200)));

        assert!(
            store
                .finish("a", Some(7), HeavyOutcome::Failed, 300)
                .unwrap()
        );
        let item = store.history_item("a").unwrap().unwrap();
        assert_eq!(item.id, "a");
        assert_eq!(item.ticket, 1);
        assert_eq!(item.workspace_root.as_deref(), Some("/work/app"));
        assert_eq!(item.enqueued_at_ms, 100);
        assert_eq!(item.granted_at_ms, Some(200));
        assert_eq!(item.finished_at_ms, 300);
        assert_eq!(item.exit_code, Some(7));
        assert_eq!(item.outcome, HeavyOutcome::Failed);
        assert_eq!(store.history_item("missing").unwrap(), None);
    }

    #[test]
    fn history_newest_first_and_limited() {
        let store = Store::open_in_memory().unwrap();
        for (id, finished_at) in [("a", 10), ("b", 20), ("c", 30)] {
            store.insert_waiting(&acquire(id), 1).unwrap();
            store
                .finish(id, Some(0), HeavyOutcome::Ok, finished_at)
                .unwrap();
        }

        let all = store.history(&HistoryQuery::default()).unwrap();
        assert_eq!(
            all.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );

        let limited = store
            .history(&HistoryQuery {
                since_ms: None,
                limit: 2,
            })
            .unwrap();
        assert_eq!(
            limited
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b"]
        );

        let recent = store
            .history(&HistoryQuery {
                since_ms: Some(25),
                limit: 100,
            })
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "c");
    }

    #[test]
    fn capacity_defaults_to_one() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.capacity().unwrap(), 1);
        store.set_capacity(4).unwrap();
        assert_eq!(store.capacity().unwrap(), 4);
    }

    #[test]
    fn newer_user_version_refused() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }

        let err = match Store::open(&path) {
            Ok(_) => panic!("opening a newer database must fail"),
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(
            message.contains("version 99") && message.contains("version 1"),
            "unexpected error: {message}"
        );
    }
}
