//! Projects, shares, requests and materialization history (plan P2, Task 5).

use anyhow::Context;
use rusqlite::params;

use super::Store;
use crate::contract::secrets::{ProjectInfo, RequestState, SecretRequest, SecretRequestInfo};
use crate::procs::ProcessIdentity;

const PROJECT_COLUMNS: &str =
    "SELECT id, name, common_dir, created_at_ms, approved_at_ms, group_uuid FROM projects";
const REQUEST_COLUMNS: &str = "SELECT r.id, r.project_id, p.name, r.name, r.reason, \
                               r.requester_json, r.state, r.created_at_ms, r.answered_at_ms \
                               FROM secret_requests r JOIN projects p ON p.id = r.project_id";

impl Store {
    /// Records a project by its common dir; an existing project keeps its id and its name.
    pub fn upsert_project(
        &self,
        common_dir: &str,
        name: &str,
        now_ms: i64,
    ) -> anyhow::Result<ProjectInfo> {
        self.conn
            .execute(
                "INSERT INTO projects (id, common_dir, name, created_at_ms, approved_at_ms, \
                 group_uuid) VALUES (?1, ?2, ?3, ?4, NULL, NULL) \
                 ON CONFLICT(common_dir) DO NOTHING",
                params![uuid::Uuid::new_v4().to_string(), common_dir, name, now_ms],
            )
            .context("cannot record the project")?;
        self.project_by_dir(common_dir)?
            .context("the project just recorded is missing")
    }

    pub fn project(&self, id: &str) -> anyhow::Result<Option<ProjectInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{PROJECT_COLUMNS} WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(project_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn project_by_dir(&self, common_dir: &str) -> anyhow::Result<Option<ProjectInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{PROJECT_COLUMNS} WHERE common_dir = ?1"))?;
        let mut rows = stmt.query([common_dir])?;
        match rows.next()? {
            Some(row) => Ok(Some(project_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// By name, then by common dir.
    pub fn projects(&self) -> anyhow::Result<Vec<ProjectInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{PROJECT_COLUMNS} ORDER BY name, common_dir"))?;
        let mut rows = stmt.query([])?;
        let mut projects = Vec::new();
        while let Some(row) = rows.next()? {
            projects.push(project_from_row(row)?);
        }
        Ok(projects)
    }

    pub fn approve_project(&self, id: &str, now_ms: i64) -> anyhow::Result<()> {
        let changed = self.conn.execute(
            "UPDATE projects SET approved_at_ms = ?1 WHERE id = ?2",
            params![now_ms, id],
        )?;
        anyhow::ensure!(changed == 1, "project {id} does not exist");
        Ok(())
    }

    pub fn set_project_group(&self, id: &str, group_uuid: Option<&str>) -> anyhow::Result<()> {
        let changed = self.conn.execute(
            "UPDATE projects SET group_uuid = ?1 WHERE id = ?2",
            params![group_uuid, id],
        )?;
        anyhow::ensure!(changed == 1, "project {id} does not exist");
        Ok(())
    }

    /// True when the share is new; the primary key is (target, name).
    pub fn add_share(
        &self,
        source: &str,
        name: &str,
        target: &str,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO secret_shares (source_project, name, target_project, \
             created_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![source, name, target, now_ms],
        )?;
        Ok(changed == 1)
    }

    pub fn remove_share(&self, source: &str, name: &str, target: &str) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM secret_shares WHERE source_project = ?1 AND name = ?2 \
             AND target_project = ?3",
            params![source, name, target],
        )?;
        Ok(changed == 1)
    }

    /// `(source project, name)` pairs shared into `target`, by name then source.
    pub fn shares_into(&self, target: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_project, name FROM secret_shares WHERE target_project = ?1 \
             ORDER BY name, source_project",
        )?;
        let mut rows = stmt.query([target])?;
        let mut shares = Vec::new();
        while let Some(row) = rows.next()? {
            shares.push((row.get(0)?, row.get(1)?));
        }
        Ok(shares)
    }

    /// Idempotent by request id: a repeated id returns the recorded request unchanged.
    pub fn insert_request(
        &self,
        request: &SecretRequest,
        project_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<SecretRequestInfo> {
        let requester =
            serde_json::to_string(&request.requester).context("cannot encode the requester")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO secret_requests (id, project_id, name, reason, \
                 requester_json, state, created_at_ms, answered_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, NULL)",
                params![
                    request.id,
                    project_id,
                    request.name,
                    request.reason,
                    requester,
                    now_ms
                ],
            )
            .context("cannot record the request")?;
        self.request(&request.id)?
            .context("the request just recorded is missing")
    }

    pub fn request(&self, id: &str) -> anyhow::Result<Option<SecretRequestInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{REQUEST_COLUMNS} WHERE r.id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(request_from_row(row)?.0)),
            None => Ok(None),
        }
    }

    pub fn pending_requests(&self) -> anyhow::Result<Vec<(SecretRequestInfo, ProcessIdentity)>> {
        let mut stmt = self.conn.prepare(&format!(
            "{REQUEST_COLUMNS} WHERE r.state = 'pending' ORDER BY r.created_at_ms, r.id"
        ))?;
        let mut rows = stmt.query([])?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next()? {
            requests.push(request_from_row(row)?);
        }
        Ok(requests)
    }

    /// True when the request was pending and is now in `state`; false when it was already answered.
    pub fn finish_request(
        &self,
        id: &str,
        state: RequestState,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE secret_requests SET state = ?1, answered_at_ms = ?2 \
             WHERE id = ?3 AND state = 'pending'",
            params![state_str(state), now_ms, id],
        )?;
        Ok(changed == 1)
    }

    /// Answers every pending request for one project and name; returns the answered ids.
    pub fn answer_pending(
        &self,
        project_id: &str,
        name: &str,
        state: RequestState,
        now_ms: i64,
    ) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "UPDATE secret_requests SET state = ?1, answered_at_ms = ?2 \
             WHERE project_id = ?3 AND name = ?4 AND state = 'pending' RETURNING id",
        )?;
        let ids = stmt
            .query_map(params![state_str(state), now_ms, project_id, name], |row| {
                row.get(0)
            })?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    pub fn record_materialization(
        &self,
        run_id: &str,
        project_id: &str,
        names: &[String],
        workspace: &str,
        command: &str,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let names = serde_json::to_string(names).context("cannot encode the names")?;
        self.conn.execute(
            "INSERT INTO secret_materializations (run_id, project_id, names_json, workspace, \
             command, at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_id, project_id, names, workspace, command, now_ms],
        )?;
        Ok(())
    }

    /// Drops answered requests and materializations older than the cutoff; pending requests stay.
    pub fn prune_secret_history(&self, older_than_ms: i64) -> anyhow::Result<usize> {
        let requests = self.conn.execute(
            "DELETE FROM secret_requests WHERE state != 'pending' \
             AND answered_at_ms IS NOT NULL AND answered_at_ms < ?1",
            [older_than_ms],
        )?;
        let materializations = self.conn.execute(
            "DELETE FROM secret_materializations WHERE at_ms < ?1",
            [older_than_ms],
        )?;
        Ok(requests + materializations)
    }

    pub fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        super::setting(&self.conn, key)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> anyhow::Result<()> {
        match value {
            Some(value) => super::set_setting(&self.conn, key, value),
            None => {
                self.conn
                    .execute("DELETE FROM settings WHERE key = ?1", [key])?;
                Ok(())
            }
        }
    }
}

fn project_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<ProjectInfo> {
    let approved_at_ms: Option<i64> = row.get(4)?;
    Ok(ProjectInfo {
        id: row.get(0)?,
        name: row.get(1)?,
        common_dir: row.get(2)?,
        approved: approved_at_ms.is_some(),
        group_uuid: row.get(5)?,
        created_at_ms: row.get(3)?,
        approved_at_ms,
    })
}

fn request_from_row(
    row: &rusqlite::Row<'_>,
) -> anyhow::Result<(SecretRequestInfo, ProcessIdentity)> {
    let requester_json: String = row.get(5)?;
    let requester: ProcessIdentity = serde_json::from_str(&requester_json)
        .context("secret_requests.requester_json is unreadable")?;
    let state: String = row.get(6)?;
    let info = SecretRequestInfo {
        id: row.get(0)?,
        project_id: row.get(1)?,
        project_name: row.get(2)?,
        name: row.get(3)?,
        reason: row.get(4)?,
        state: parse_state(&state)?,
        requester_pid: requester.pid,
        created_at_ms: row.get(7)?,
        answered_at_ms: row.get(8)?,
    };
    Ok((info, requester))
}

fn state_str(state: RequestState) -> &'static str {
    match state {
        RequestState::Pending => "pending",
        RequestState::Stored => "stored",
        RequestState::Declined => "declined",
        RequestState::Withdrawn => "withdrawn",
        RequestState::TimedOut => "timed_out",
    }
}

fn parse_state(raw: &str) -> anyhow::Result<RequestState> {
    match raw {
        "pending" => Ok(RequestState::Pending),
        "stored" => Ok(RequestState::Stored),
        "declined" => Ok(RequestState::Declined),
        "withdrawn" => Ok(RequestState::Withdrawn),
        "timed_out" => Ok(RequestState::TimedOut),
        other => anyhow::bail!("unknown secret request state {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;
    use crate::contract::heavy::HistoryQuery;
    use crate::contract::secrets::ProjectRef;
    use crate::store::schema;

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            boot_id: "boot-1".into(),
            start_time: 500,
        }
    }

    fn request(id: &str, project: &str, name: &str) -> SecretRequest {
        SecretRequest {
            id: id.into(),
            project: ProjectRef {
                common_dir: format!("/work/{project}/.git"),
                name: project.into(),
            },
            name: name.into(),
            reason: "the tests need it".into(),
            requester: identity(100),
        }
    }

    fn store_with_project(name: &str) -> (tempfile::TempDir, Store, ProjectInfo) {
        let dir = tempdir().unwrap();
        let store = Store::open(&dir.path().join("hostler.db")).unwrap();
        let project = store
            .upsert_project(&format!("/work/{name}/.git"), name, 1)
            .unwrap();
        (dir, store, project)
    }

    #[test]
    fn v1_database_migrates_to_v2_keeping_heavy_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(schema::MIGRATION_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO heavy_history (id, ticket, kind, command, cwd, workspace_root, \
                 enqueued_at_ms, granted_at_ms, finished_at_ms, exit_code, outcome) \
                 VALUES ('old', 1, 'gradle', './gradlew build', '/work/app', '/work/app', \
                 1, 2, 3, 0, 'ok')",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let history = store.history(&HistoryQuery::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "old");
        assert!(store.projects().unwrap().is_empty());
    }

    #[test]
    fn upsert_is_stable_and_keeps_the_first_name() {
        let (_dir, store, first) = store_with_project("app");

        let again = store
            .upsert_project("/work/app/.git", "renamed", 2)
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.name, "app");
        assert_eq!(again.created_at_ms, 1);
        assert_eq!(again.common_dir, "/work/app/.git");
        assert!(!again.approved);

        assert_eq!(store.projects().unwrap(), vec![again.clone()]);
        assert_eq!(store.project(&first.id).unwrap(), Some(again.clone()));
        assert_eq!(store.project_by_dir("/work/app/.git").unwrap(), Some(again));
        assert_eq!(store.project("missing").unwrap(), None);
    }

    #[test]
    fn approve_sets_time() {
        let (_dir, store, project) = store_with_project("app");
        store.approve_project(&project.id, 42).unwrap();
        let approved = store.project(&project.id).unwrap().unwrap();
        assert!(approved.approved);
        assert_eq!(approved.approved_at_ms, Some(42));

        assert!(store.approve_project("missing", 43).is_err());
    }

    #[test]
    fn project_group_is_set_and_cleared() {
        let (_dir, store, project) = store_with_project("app");
        store
            .set_project_group(&project.id, Some("group-1"))
            .unwrap();
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().group_uuid,
            Some("group-1".into())
        );
        store.set_project_group(&project.id, None).unwrap();
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().group_uuid,
            None
        );
    }

    #[test]
    fn projects_are_ordered_by_name_then_dir() {
        let (_dir, store, _) = store_with_project("zeta");
        store
            .upsert_project("/work/zeta-2/.git", "zeta", 2)
            .unwrap();
        store
            .upsert_project("/work/alpha/.git", "alpha", 3)
            .unwrap();

        let names: Vec<(String, String)> = store
            .projects()
            .unwrap()
            .into_iter()
            .map(|project| (project.name, project.common_dir))
            .collect();
        assert_eq!(
            names,
            [
                ("alpha".into(), "/work/alpha/.git".into()),
                ("zeta".into(), "/work/zeta-2/.git".into()),
                ("zeta".into(), "/work/zeta/.git".into()),
            ]
        );
    }

    #[test]
    fn share_primary_key_refuses_a_second_source() {
        let (_dir, store, target) = store_with_project("target");
        let source_1 = store
            .upsert_project("/work/source-1/.git", "source-1", 1)
            .unwrap();
        let source_2 = store
            .upsert_project("/work/source-2/.git", "source-2", 1)
            .unwrap();

        assert!(
            store
                .add_share(&source_1.id, "API_KEY", &target.id, 10)
                .unwrap()
        );
        assert!(
            !store
                .add_share(&source_2.id, "API_KEY", &target.id, 11)
                .unwrap()
        );
        assert_eq!(
            store.shares_into(&target.id).unwrap(),
            vec![(source_1.id.clone(), "API_KEY".into())]
        );

        assert!(
            !store
                .remove_share(&source_2.id, "API_KEY", &target.id)
                .unwrap()
        );
        assert!(
            store
                .remove_share(&source_1.id, "API_KEY", &target.id)
                .unwrap()
        );
        assert!(store.shares_into(&target.id).unwrap().is_empty());
    }

    #[test]
    fn request_insert_is_idempotent() {
        let (_dir, store, project) = store_with_project("app");
        let request = request("req-1", "app", "API_KEY");

        let first = store.insert_request(&request, &project.id, 10).unwrap();
        assert_eq!(first.id, "req-1");
        assert_eq!(first.project_id, project.id);
        assert_eq!(first.project_name, "app");
        assert_eq!(first.name, "API_KEY");
        assert_eq!(first.reason, "the tests need it");
        assert_eq!(first.state, RequestState::Pending);
        assert_eq!(first.requester_pid, 100);
        assert_eq!(first.created_at_ms, 10);
        assert_eq!(first.answered_at_ms, None);

        let again = store
            .insert_request(&request, "another-project", 20)
            .unwrap();
        assert_eq!(again, first);
        assert_eq!(store.pending_requests().unwrap().len(), 1);
        assert_eq!(store.pending_requests().unwrap()[0].0, first);
    }

    #[test]
    fn finish_request_only_from_pending() {
        let (_dir, store, project) = store_with_project("app");
        store
            .insert_request(&request("req-1", "app", "API_KEY"), &project.id, 10)
            .unwrap();

        assert!(
            store
                .finish_request("req-1", RequestState::Stored, 20)
                .unwrap()
        );
        let done = store.request("req-1").unwrap().unwrap();
        assert_eq!(done.state, RequestState::Stored);
        assert_eq!(done.answered_at_ms, Some(20));

        assert!(
            !store
                .finish_request("req-1", RequestState::Declined, 30)
                .unwrap()
        );
        assert_eq!(
            store.request("req-1").unwrap().unwrap().state,
            RequestState::Stored
        );
        assert!(store.pending_requests().unwrap().is_empty());
    }

    #[test]
    fn answer_pending_returns_changed_ids() {
        let (_dir, store, project) = store_with_project("app");
        for id in ["req-1", "req-2"] {
            store
                .insert_request(&request(id, "app", "API_KEY"), &project.id, 10)
                .unwrap();
        }
        store
            .insert_request(&request("req-3", "app", "OTHER"), &project.id, 10)
            .unwrap();

        let mut answered = store
            .answer_pending(&project.id, "API_KEY", RequestState::Stored, 20)
            .unwrap();
        answered.sort();
        assert_eq!(answered, ["req-1", "req-2"]);
        assert_eq!(
            store.request("req-1").unwrap().unwrap().state,
            RequestState::Stored
        );
        assert_eq!(
            store.request("req-3").unwrap().unwrap().state,
            RequestState::Pending
        );

        assert!(
            store
                .answer_pending(&project.id, "API_KEY", RequestState::Stored, 30)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn prune_keeps_pending_requests() {
        let (_dir, store, project) = store_with_project("app");
        store
            .insert_request(&request("pending", "app", "A"), &project.id, 10)
            .unwrap();
        for (id, name) in [("stored", "B"), ("declined", "C")] {
            store
                .insert_request(&request(id, "app", name), &project.id, 10)
                .unwrap();
        }
        store
            .finish_request("stored", RequestState::Stored, 20)
            .unwrap();
        store
            .finish_request("declined", RequestState::Declined, 20)
            .unwrap();
        store
            .record_materialization("run-old", &project.id, &["A".into()], "/w", "cmd", 10)
            .unwrap();
        store
            .record_materialization("run-new", &project.id, &["A".into()], "/w", "cmd", 100)
            .unwrap();

        assert_eq!(store.prune_secret_history(50).unwrap(), 3);
        assert!(store.request("pending").unwrap().is_some());
        assert_eq!(
            store.request("pending").unwrap().unwrap().state,
            RequestState::Pending
        );
        assert!(store.request("stored").unwrap().is_none());
        assert!(store.request("declined").unwrap().is_none());
        let runs: i64 = store
            .conn
            .query_row("SELECT count(*) FROM secret_materializations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(runs, 1);
    }

    #[test]
    fn settings_round_trip_and_clear() {
        let (_dir, store, _) = store_with_project("app");
        assert_eq!(store.setting("secrets.key_file").unwrap(), None);
        store
            .set_setting("secrets.key_file", Some("/keys/app.key"))
            .unwrap();
        assert_eq!(
            store.setting("secrets.key_file").unwrap().as_deref(),
            Some("/keys/app.key")
        );
        store.set_setting("secrets.key_file", None).unwrap();
        assert_eq!(store.setting("secrets.key_file").unwrap(), None);
    }

    fn hold_write(path: &std::path::Path, held: Duration, acquired: std::sync::mpsc::Sender<()>) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        acquired.send(()).unwrap();
        std::thread::sleep(held);
        conn.execute_batch("COMMIT").unwrap();
    }

    #[test]
    fn writer_waits_for_a_held_transaction() {
        let (dir, store, project) = store_with_project("app");
        let path = dir.path().join("hostler.db");
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            hold_write(&path, Duration::from_millis(1000), tx);
        });
        rx.recv().unwrap();

        let started = Instant::now();
        let info = store
            .insert_request(&request("req-1", "app", "API_KEY"), &project.id, 10)
            .unwrap();
        let elapsed = started.elapsed();
        writer.join().unwrap();

        assert_eq!(info.id, "req-1");
        assert!(
            elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(5),
            "waited {elapsed:?}"
        );
        // Production keeps the 5 s bound; the wait above is not a busy failure.
        let timeout: i64 = store
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn busy_timeout_bounds_the_wait() {
        // The mechanism, not wall-clock luck: with a 300 ms bound every locking event is short,
        // while the other connection holds the writer lock far beyond any multiple of it. The
        // insert must fail with the busy error instead of waiting for the holder to finish.
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        let store = Store::open_with_busy_timeout(&path, Duration::from_millis(300)).unwrap();
        let project = store.upsert_project("/work/app/.git", "app", 1).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            hold_write(&writer_path, Duration::from_secs(3), tx);
        });
        rx.recv().unwrap();

        let started = Instant::now();
        let error = store
            .insert_request(&request("req-1", "app", "API_KEY"), &project.id, 10)
            .unwrap_err();
        let elapsed = started.elapsed();
        writer.join().unwrap();

        assert!(elapsed < Duration::from_secs(3), "waited {elapsed:?}");
        assert!(
            format!("{error:#}").contains("locked"),
            "unexpected error: {error:#}"
        );
        // The refused write left no row, and the connection works again once the lock is free.
        assert!(store.request("req-1").unwrap().is_none());
        assert!(
            store
                .insert_request(&request("req-1", "app", "API_KEY"), &project.id, 11)
                .is_ok()
        );
    }
}
