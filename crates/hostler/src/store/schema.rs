//! Schema migrations driven by `PRAGMA user_version` (plan Task 5, P3a Task 3, P3b Task 1).

use anyhow::Context;
use rusqlite::Connection;

pub(crate) const SCHEMA_VERSION: i64 = 4;

pub(crate) const MIGRATION_V1: &str = "
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE heavy_entries (id TEXT PRIMARY KEY, ticket INTEGER NOT NULL UNIQUE, kind TEXT NOT NULL,
  command TEXT NOT NULL, cwd TEXT NOT NULL, workspace_root TEXT, holder_json TEXT NOT NULL,
  child_json TEXT, group_json TEXT, state TEXT NOT NULL CHECK (state IN ('waiting','running')),
  enqueued_at_ms INTEGER NOT NULL, granted_at_ms INTEGER);
CREATE TABLE heavy_history (id TEXT PRIMARY KEY, ticket INTEGER NOT NULL, kind TEXT NOT NULL,
  command TEXT NOT NULL, cwd TEXT NOT NULL, workspace_root TEXT, enqueued_at_ms INTEGER NOT NULL,
  granted_at_ms INTEGER, finished_at_ms INTEGER NOT NULL, exit_code INTEGER, outcome TEXT NOT NULL);
CREATE INDEX heavy_history_finished ON heavy_history (finished_at_ms);
";

const MIGRATION_V2: &str = "
CREATE TABLE projects (id TEXT PRIMARY KEY, common_dir TEXT NOT NULL UNIQUE, name TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL, approved_at_ms INTEGER, group_uuid TEXT);
CREATE TABLE secret_shares (source_project TEXT NOT NULL REFERENCES projects(id),
  name TEXT NOT NULL, target_project TEXT NOT NULL REFERENCES projects(id),
  created_at_ms INTEGER NOT NULL, PRIMARY KEY (target_project, name));
CREATE TABLE secret_requests (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
  name TEXT NOT NULL, reason TEXT NOT NULL, requester_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending','stored','declined','withdrawn','timed_out')),
  created_at_ms INTEGER NOT NULL, answered_at_ms INTEGER);
CREATE INDEX secret_requests_pending ON secret_requests (state, project_id, name);
CREATE TABLE secret_materializations (run_id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
  names_json TEXT NOT NULL, workspace TEXT NOT NULL, command TEXT NOT NULL, at_ms INTEGER NOT NULL);
CREATE INDEX secret_materializations_at ON secret_materializations (at_ms);
";

const MIGRATION_V3: &str = "
CREATE TABLE workspaces (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
  path TEXT NOT NULL, git_dir TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
  last_seen_ms INTEGER NOT NULL, root_dev INTEGER);
CREATE INDEX workspaces_path ON workspaces (path);
CREATE TABLE claims (id TEXT PRIMARY KEY, resource TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('lease','hold','reservation')),
  parent TEXT, workspace_id TEXT NOT NULL REFERENCES workspaces(id), holder_json TEXT,
  child_json TEXT, group_json TEXT, binding_json TEXT, command TEXT,
  granted_at_ms INTEGER NOT NULL, nested_in TEXT);
CREATE INDEX claims_resource ON claims (resource);
CREATE INDEX claims_parent ON claims (parent);
CREATE TABLE claim_requests (id TEXT PRIMARY KEY, ticket INTEGER NOT NULL UNIQUE,
  selector_json TEXT NOT NULL, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  requester_json TEXT NOT NULL, purpose_json TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
  nested_in TEXT);
CREATE TABLE claim_history (id TEXT PRIMARY KEY, resource TEXT NOT NULL, kind TEXT NOT NULL,
  workspace_path TEXT NOT NULL, command TEXT, granted_at_ms INTEGER, ended_at_ms INTEGER NOT NULL,
  reason TEXT NOT NULL, exit_code INTEGER, detail TEXT);
CREATE INDEX claim_history_ended ON claim_history (ended_at_ms);
CREATE TABLE seizures (id TEXT PRIMARY KEY, resource TEXT NOT NULL,
  from_workspace_path TEXT NOT NULL, from_kind TEXT NOT NULL, from_pid INTEGER,
  by_workspace_path TEXT NOT NULL, by_pid INTEGER NOT NULL, at_ms INTEGER NOT NULL,
  stranded INTEGER NOT NULL, forced INTEGER NOT NULL DEFAULT 0);
ALTER TABLE projects ADD COLUMN dir_identity TEXT;
ALTER TABLE heavy_entries ADD COLUMN nested_in TEXT;
CREATE INDEX projects_dir_identity ON projects (dir_identity);
";

/// P3b: devices, their apps and install history, boots, claim windows, the last good
/// agent-device read (L51), the record of `forget-sessions` (L54), and a seizure's source
/// (P2-12). `claim_windows.head_commit`: `commit` is an SQL keyword (P-11).
const MIGRATION_V4: &str = "
CREATE TABLE devices (resource TEXT PRIMARY KEY, platform TEXT NOT NULL, kind TEXT NOT NULL,
  alias TEXT UNIQUE COLLATE NOCASE, alias_by_person INTEGER NOT NULL DEFAULT 0, model TEXT NOT NULL,
  name TEXT, os_version TEXT, note TEXT, coredevice_id TEXT, serials_json TEXT NOT NULL DEFAULT '[]',
  first_seen_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL, registered_at_ms INTEGER);
CREATE TABLE device_apps (resource TEXT NOT NULL, app_id TEXT NOT NULL, version_name TEXT,
  version_code TEXT, installed_json TEXT, updated_json TEXT, first_seen_ms INTEGER NOT NULL,
  last_seen_ms INTEGER NOT NULL, PRIMARY KEY (resource, app_id));
CREATE TABLE device_snapshots (resource TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, ok INTEGER NOT NULL);
CREATE TABLE device_installs (id TEXT PRIMARY KEY, resource TEXT NOT NULL, app_id TEXT NOT NULL,
  change TEXT NOT NULL, version_name TEXT, version_code TEXT, when_json TEXT NOT NULL,
  source_json TEXT NOT NULL, recorded_at_ms INTEGER NOT NULL);
CREATE INDEX device_installs_at ON device_installs (recorded_at_ms);
CREATE INDEX device_installs_device ON device_installs (resource, recorded_at_ms);
CREATE TABLE device_boots (resource TEXT PRIMARY KEY, booted_at_ms INTEGER NOT NULL,
  identity_json TEXT NOT NULL, claim_id TEXT NOT NULL, idle_since_ms INTEGER);
CREATE TABLE claim_windows (claim_id TEXT PRIMARY KEY, resource TEXT NOT NULL, workspace_id TEXT NOT NULL,
  workspace_path TEXT NOT NULL, branch TEXT, head_commit TEXT, command TEXT,
  from_ms INTEGER NOT NULL, to_ms INTEGER);
CREATE INDEX claim_windows_device ON claim_windows (resource, from_ms);
CREATE TABLE agent_device_last (id INTEGER PRIMARY KEY CHECK (id = 1), claims_json TEXT NOT NULL,
  at_ms INTEGER NOT NULL);
ALTER TABLE seizures ADD COLUMN from_source TEXT;
CREATE TABLE device_session_forgets (id TEXT PRIMARY KEY, resource TEXT NOT NULL,
  sessions_json TEXT NOT NULL, by_workspace_path TEXT, by_pid INTEGER, reason TEXT NOT NULL,
  at_ms INTEGER NOT NULL);
";

pub(crate) fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let mut version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("cannot read PRAGMA user_version")?;
    anyhow::ensure!(
        version <= SCHEMA_VERSION,
        "database schema version {version} is newer than schema version {SCHEMA_VERSION} \
         understood by this hostler build; upgrade hostler"
    );

    if version < 1 {
        migrate_to(conn, 1, MIGRATION_V1)?;
        version = 1;
    }
    if version < 2 {
        migrate_to(conn, 2, MIGRATION_V2)?;
        version = 2;
    }
    if version < 3 {
        migrate_to(conn, 3, MIGRATION_V3)?;
        version = 3;
    }
    // A released 0.0.2 database (v2) runs v3 above, commits, then v4 (P3b).
    if version < 4 {
        migrate_to(conn, 4, MIGRATION_V4)?;
    }
    Ok(())
}

fn migrate_to(conn: &mut Connection, version: i64, sql: &str) -> anyhow::Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(sql)
        .with_context(|| format!("cannot create schema version {version}"))?;
    tx.pragma_update(None, "user_version", version)
        .context("cannot record the schema version")?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;
    use crate::contract::heavy::HistoryQuery;
    use crate::store::Store;

    /// A v2 file exactly as P2's migrations left it, with a row in every table.
    #[test]
    fn v2_database_migrates_to_v3_keeping_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute(
                "INSERT INTO projects (id, common_dir, name, created_at_ms, approved_at_ms, \
                 group_uuid) VALUES ('p1', '/work/app/.git', 'app', 1, 2, 'group-1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO heavy_history (id, ticket, kind, command, cwd, workspace_root, \
                 enqueued_at_ms, granted_at_ms, finished_at_ms, exit_code, outcome) \
                 VALUES ('h1', 1, 'gradle', './gradlew build', '/work/app', '/work/app', 1, 2, 3, \
                 0, 'ok')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO secret_requests (id, project_id, name, reason, requester_json, \
                 state, created_at_ms, answered_at_ms) VALUES ('r1', 'p1', 'API_KEY', \
                 'the tests need it', '{\"pid\":100,\"boot_id\":\"boot-1\",\"start_time\":500}', \
                 'pending', 4, NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO secret_materializations (run_id, project_id, names_json, workspace, \
                 command, at_ms) VALUES ('m1', 'p1', '[\"API_KEY\"]', '/work/app', 'make test', 5)",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);

        let projects = store.projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "p1");
        assert_eq!(projects[0].common_dir, "/work/app/.git");
        assert!(projects[0].approved);
        assert_eq!(projects[0].group_uuid.as_deref(), Some("group-1"));
        let identity: Option<String> = store
            .conn
            .query_row(
                "SELECT dir_identity FROM projects WHERE id = 'p1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(identity, None, "P2 rows keep an unknown identity");

        let history = store.history(&HistoryQuery::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "h1");
        let request = store.request("r1").unwrap().unwrap();
        assert_eq!(request.name, "API_KEY");
        assert_eq!(request.project_id, "p1");

        assert!(store.workspaces().unwrap().is_empty());
        assert!(store.claims().unwrap().is_empty());
        assert!(store.claim_requests().unwrap().is_empty());
        assert!(
            store
                .claim_history(&HistoryQuery::default())
                .unwrap()
                .is_empty()
        );
    }

    /// The tables V4 creates, all empty after a migration.
    const V4_TABLES: [&str; 8] = [
        "devices",
        "device_apps",
        "device_snapshots",
        "device_installs",
        "device_boots",
        "claim_windows",
        "agent_device_last",
        "device_session_forgets",
    ];

    fn user_version(conn: &Connection) -> i64 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    fn assert_v4_tables_empty(store: &Store) {
        for table in V4_TABLES {
            let rows: i64 = store
                .conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
    }

    /// A v3 file exactly as P3a's migrations left it, with a row in its claims tables.
    #[test]
    fn v3_database_migrates_to_v4_keeping_rows() {
        use crate::contract::claims::{ClaimKind, SeizedFrom};

        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute_batch(MIGRATION_V3).unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
            conn.execute_batch(
                "INSERT INTO projects (id, common_dir, name, created_at_ms, approved_at_ms, \
                 group_uuid, dir_identity) VALUES ('p1', '/work/app/.git', 'app', 1, NULL, NULL, \
                 'dev:1:ino:2');
                 INSERT INTO workspaces (id, project_id, path, git_dir, created_at_ms, \
                 last_seen_ms, root_dev) VALUES ('ws-1', 'p1', '/work/app', '/work/app/.git', 1, \
                 2, 7);
                 INSERT INTO claims (id, resource, kind, parent, workspace_id, holder_json, \
                 child_json, group_json, binding_json, command, granted_at_ms, nested_in) VALUES \
                 ('c1', 'screen', 'hold', NULL, 'ws-1', NULL, NULL, NULL, NULL, NULL, 3, NULL);
                 INSERT INTO claim_history (id, resource, kind, workspace_path, command, \
                 granted_at_ms, ended_at_ms, reason, exit_code, detail) VALUES ('c0', 'screen', \
                 'lease', '/work/app', 'make test', 1, 2, 'released', 0, NULL);
                 INSERT INTO seizures (id, resource, from_workspace_path, from_kind, from_pid, \
                 by_workspace_path, by_pid, at_ms, stranded, forced) VALUES ('s1', 'screen', \
                 '/work/app', 'lease', 42, '/work/other', 77, 4, 0, 1);",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store.conn), 4);
        let workspaces = store.workspaces().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].root_dev, Some(7));
        let claims = store.claims().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            (claims[0].id.as_str(), claims[0].kind),
            ("c1", ClaimKind::Hold)
        );
        let history = store.claim_history(&HistoryQuery::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "c0");
        let seizures = store.seizures(&HistoryQuery::default()).unwrap();
        assert_eq!(seizures.len(), 1);
        assert_eq!(seizures[0].from_kind, SeizedFrom::Lease);
        assert_eq!(seizures[0].from_source, None, "P3a rows have no source");
        assert!(seizures[0].forced);
        assert_v4_tables_empty(&store);

        // Reopening a v4 database migrates nothing.
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store.conn), 4);
        assert_eq!(store.claims().unwrap().len(), 1);
    }

    /// The database a released 0.0.2 left (`tests/fixtures/store/README.md`): v3 commits, then
    /// v4, keeping its project, its heavy history and its settings.
    #[test]
    fn v2_database_from_0_0_2_migrates_to_v4() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/store/hostler-0.0.2.db");
        // Sanitized (P3bT1-3): no pid, boot id or home path left in its free pages either.
        let bytes = std::fs::read(&fixture).unwrap();
        for needle in [&b"boot_id"[..], b"\"pid\"", b"/Users/", b"/home/"] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "the fixture holds {:?}",
                String::from_utf8_lossy(needle)
            );
        }
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        std::fs::copy(&fixture, &path).unwrap();
        assert_eq!(user_version(&Connection::open(&path).unwrap()), 2);

        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store.conn), 4);
        let projects = store.projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "app");
        assert_eq!(
            projects[0].common_dir,
            "/private/tmp/hostler-fixture/app/.git"
        );
        assert!(!projects[0].approved);
        let history = store.history(&HistoryQuery::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].command, "echo heavy-ok");
        assert_eq!(history[0].kind, "other");
        assert_eq!(history[0].exit_code, Some(0));
        assert_eq!(
            store.setting("heavy.next_ticket").unwrap().as_deref(),
            Some("2")
        );
        assert!(store.workspaces().unwrap().is_empty());
        assert!(store.claims().unwrap().is_empty());
        assert!(store.seizures(&HistoryQuery::default()).unwrap().is_empty());
        assert_v4_tables_empty(&store);
    }

    /// One transaction per version: a v4 step that fails leaves a valid v3 database.
    #[test]
    fn a_failed_v4_step_leaves_the_v3_commit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            // Something V4 creates already exists: its CREATE TABLE fails.
            conn.execute_batch("CREATE TABLE devices (x TEXT);")
                .unwrap();
        }
        let err = match Store::open(&path) {
            Ok(_) => panic!("the v4 step must fail"),
            Err(err) => format!("{err:#}"),
        };
        assert!(err.contains("schema version 4"), "{err}");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), 3);
        let workspaces: i64 = conn
            .query_row("SELECT count(*) FROM workspaces", [], |row| row.get(0))
            .unwrap();
        assert_eq!(workspaces, 0, "v3 committed");
        let seizure_columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('seizures')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            !seizure_columns.iter().any(|c| c == "from_source"),
            "v4 rolled back"
        );
    }

    #[test]
    fn newer_user_version_refused() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
        }
        let err = match Store::open(&path) {
            Ok(_) => panic!("opening a newer database must fail"),
            Err(err) => format!("{err:#}"),
        };
        assert!(
            err.contains("version 5") && err.contains("schema version 4"),
            "{err}"
        );
        // Never rewritten: still 5, still not WAL.
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), 5);
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_ne!(mode, "wal");
    }
}
