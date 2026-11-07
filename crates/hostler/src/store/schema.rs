//! Schema migrations driven by `PRAGMA user_version` (plan Task 5).

use anyhow::Context;
use rusqlite::Connection;

pub(crate) const SCHEMA_VERSION: i64 = 2;

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
