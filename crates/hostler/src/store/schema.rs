//! Schema migrations driven by `PRAGMA user_version` (plan Task 5).

use anyhow::Context;
use rusqlite::Connection;

pub(crate) const SCHEMA_VERSION: i64 = 1;

const MIGRATION_V1: &str = "
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

pub(crate) fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("cannot read PRAGMA user_version")?;
    anyhow::ensure!(
        version <= SCHEMA_VERSION,
        "database schema version {version} is newer than schema version {SCHEMA_VERSION} \
         understood by this hostler build; upgrade hostler"
    );

    if version == 0 {
        let tx = conn.transaction()?;
        tx.execute_batch(MIGRATION_V1)
            .context("cannot create the schema")?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .context("cannot record the schema version")?;
        tx.commit()?;
    }
    Ok(())
}
