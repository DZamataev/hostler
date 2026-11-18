//! Workspace rows and project identity resolution (plan P3a, Task 3; L15, L19, L21).

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};

use super::Store;
use crate::contract::projects::ProjectDecl;
use crate::contract::secrets::ProjectInfo;

/// A checkout as the store records it; claims belong to workspaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub git_dir: String,
    pub created_at_ms: i64,
    pub last_seen_ms: i64,
    /// The checkout root's device (`st_dev`, Unix) when it was last resolved (T5-R4).
    pub root_dev: Option<u64>,
}

/// What `resolve_project` decided (L15, L21).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectResolution {
    /// The row at the common dir, or a row re-pointed to it.
    Existing,
    /// A new, unapproved project.
    Created,
    /// A project followed its directory here; approval and group are kept.
    Repointed { from_path: String },
    /// The row at this path belonged to another repository and now waits elsewhere; this path
    /// got a new project.
    MovedAway { old_id: String },
}

const WORKSPACE_COLUMNS: &str =
    "SELECT id, project_id, path, git_dir, created_at_ms, last_seen_ms, root_dev FROM workspaces";

impl Store {
    /// Records a checkout and refreshes when it was last seen. A workspace keeps its id.
    pub fn upsert_workspace(
        &self,
        id: &str,
        project_id: &str,
        path: &str,
        git_dir: &str,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.conn
            .execute(
                "INSERT INTO workspaces (id, project_id, path, git_dir, created_at_ms, \
                 last_seen_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
                 ON CONFLICT(id) DO UPDATE SET project_id = excluded.project_id, \
                 path = excluded.path, git_dir = excluded.git_dir, \
                 last_seen_ms = excluded.last_seen_ms",
                params![id, project_id, path, git_dir, now_ms],
            )
            .context("cannot record the workspace")?;
        Ok(())
    }

    /// The checkout root's device as of this resolve (T5-R4); `None` where there is none.
    pub fn set_workspace_root_dev(&self, id: &str, dev: Option<u64>) -> anyhow::Result<()> {
        self.conn
            .execute(
                "UPDATE workspaces SET root_dev = ?1 WHERE id = ?2",
                params![dev.map(|d| d as i64), id],
            )
            .context("cannot record the workspace's device")?;
        Ok(())
    }

    /// Deletes a workspace row nothing refers to (no claim, request or history needs it).
    pub fn remove_unused_workspace(&self, id: &str) -> anyhow::Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM workspaces WHERE id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM claims WHERE workspace_id = ?1) \
             AND NOT EXISTS (SELECT 1 FROM claim_requests WHERE workspace_id = ?1)",
            [id],
        )?;
        Ok(removed > 0)
    }

    pub fn workspace(&self, id: &str) -> anyhow::Result<Option<WorkspaceRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{WORKSPACE_COLUMNS} WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(workspace_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn workspaces(&self) -> anyhow::Result<Vec<WorkspaceRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{WORKSPACE_COLUMNS} ORDER BY path, id"))?;
        let mut rows = stmt.query([])?;
        let mut workspaces = Vec::new();
        while let Some(row) = rows.next()? {
            workspaces.push(workspace_from_row(row)?);
        }
        Ok(workspaces)
    }

    /// P5: stores `decl` as the project's declaration when it differs from the stored one, in
    /// one immediate transaction; `true` when it changed. Compared as values, not as text.
    pub fn set_declared(
        &self,
        project_id: &str,
        decl: &ProjectDecl,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let json = serde_json::to_string(decl).context("cannot encode the declaration")?;
        let tx = self.immediate_tx()?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT json FROM project_declared WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?;
        if stored
            .as_deref()
            .and_then(|s| serde_json::from_str::<ProjectDecl>(s).ok())
            .as_ref()
            == Some(decl)
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO project_declared (project_id, json, updated_at_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT(project_id) DO UPDATE SET json = excluded.json, \
             updated_at_ms = excluded.updated_at_ms",
            params![project_id, json, now_ms],
        )
        .context("cannot store the declaration")?;
        tx.commit()?;
        Ok(true)
    }

    /// Every stored declaration with its time, by project id. A row that no longer decodes is
    /// skipped (logged): the next resolve from a valid `hostler.toml` writes it again.
    pub fn declarations(
        &self,
    ) -> anyhow::Result<std::collections::HashMap<String, (ProjectDecl, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project_id, json, updated_at_ms FROM project_declared")?;
        let mut rows = stmt.query([])?;
        let mut out = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let json: String = row.get(1)?;
            match serde_json::from_str::<ProjectDecl>(&json) {
                Ok(decl) => {
                    out.insert(id, (decl, row.get(2)?));
                }
                Err(e) => tracing::warn!("skipping the unreadable declaration of {id}: {e}"),
            }
        }
        Ok(out)
    }

    /// Moves a project to `new_common_dir` in one transaction; false and no change when the
    /// target path already holds a project that is not fresh (approved, has a group, a
    /// workspace, a request or a share). A fresh duplicate is deleted. Approval and the KeePass
    /// group link stay with `project_id` (L15).
    pub fn repoint_project(&self, project_id: &str, new_common_dir: &str) -> anyhow::Result<bool> {
        let tx = self.immediate_tx()?;
        let Some(project) = self.project(project_id)? else {
            tx.commit()?;
            return Ok(false);
        };
        if project.common_dir == new_common_dir {
            tx.commit()?;
            return Ok(true);
        }
        if let Some(duplicate) = self.project_by_dir(new_common_dir)? {
            if !is_fresh(&self.conn, &duplicate.id)? {
                tx.commit()?;
                return Ok(false);
            }
            tx.execute("DELETE FROM projects WHERE id = ?1", [&duplicate.id])?;
        }
        // The identity of the new place is unknown here; the next resolve records it, so the
        // stale identity is cleared rather than left to move the row away again.
        tx.execute(
            "UPDATE projects SET common_dir = ?1, dir_identity = NULL WHERE id = ?2",
            params![new_common_dir, project_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// L21: records a project for `common_dir`, guarding it with the filesystem identity of that
    /// directory. A different repository at a reused path gets a new, unapproved project and the
    /// old row waits at `moved-away:<id>:<old path>`; the same directory at a new path is
    /// re-pointed, keeping approval and the KeePass group.
    ///
    /// `dir_identity_now` returns the identity of a directory now, or `None` when it cannot be
    /// read (missing or unreadable); `identity` is the identity of `common_dir`.
    pub fn resolve_project(
        &self,
        common_dir: &str,
        name: &str,
        identity: Option<&str>,
        dir_identity_now: &dyn Fn(&str) -> Option<String>,
        now_ms: i64,
    ) -> anyhow::Result<(ProjectInfo, ProjectResolution)> {
        let tx = self.immediate_tx()?;
        let mut moved_away = None;

        if let Some(row) = self.project_by_dir(common_dir)? {
            let stored = project_identity(&self.conn, &row.id)?;
            match (stored.as_deref(), identity) {
                (Some(stored_id), Some(now_id)) if !same_dir_identity(stored_id, now_id) => {
                    move_away(&self.conn, &row.id, &row.common_dir)?;
                    moved_away = Some(row.id.clone());
                }
                (stored_id, Some(now_id)) => {
                    if stored_id != Some(now_id) {
                        tx.execute(
                            "UPDATE projects SET dir_identity = ?1 WHERE id = ?2",
                            params![now_id, row.id],
                        )?;
                    }
                    let row = self
                        .project(&row.id)?
                        .context("the project just resolved is missing")?;
                    tx.commit()?;
                    return Ok((row, ProjectResolution::Existing));
                }
                // The identity is unknown: the path decides, as in P2.
                (_, None) => {
                    tx.commit()?;
                    return Ok((row, ProjectResolution::Existing));
                }
            }
        }

        if let Some(now_id) = identity {
            for id in self.project_ids_with_identity(now_id)? {
                let Some(row) = self.project(&id)? else {
                    continue;
                };
                let eligible = is_moved_away(&row.common_dir, &row.id)
                    || match dir_identity_now(&row.common_dir) {
                        None => true,
                        Some(at_path) => !same_dir_identity(now_id, &at_path),
                    };
                if eligible {
                    let from_path = row.common_dir.clone();
                    tx.execute(
                        "UPDATE projects SET common_dir = ?1, dir_identity = ?2 WHERE id = ?3",
                        params![common_dir, now_id, row.id],
                    )?;
                    let row = self
                        .project(&row.id)?
                        .context("the re-pointed project is missing")?;
                    tx.commit()?;
                    return Ok((row, ProjectResolution::Repointed { from_path }));
                }
            }
        }

        tx.execute(
            "INSERT INTO projects (id, common_dir, name, created_at_ms, approved_at_ms, \
             group_uuid, dir_identity) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                common_dir,
                name,
                now_ms,
                identity
            ],
        )
        .context("cannot record the project")?;
        let row = self
            .project_by_dir(common_dir)?
            .context("the project just recorded is missing")?;
        tx.commit()?;

        let resolution = match moved_away {
            Some(old_id) => ProjectResolution::MovedAway { old_id },
            None => ProjectResolution::Created,
        };
        Ok((row, resolution))
    }

    fn project_ids_with_identity(&self, identity: &str) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM projects WHERE dir_identity = ?1 ORDER BY created_at_ms, id",
        )?;
        let ids = stmt
            .query_map([identity], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }
}

/// `Connection` behind the current transaction; reads see its uncommitted writes.
fn project_identity(conn: &Connection, id: &str) -> anyhow::Result<Option<String>> {
    conn.query_row(
        "SELECT dir_identity FROM projects WHERE id = ?1",
        [id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .context("cannot read the project identity")
    .map(Option::flatten)
}

fn move_away(conn: &Connection, id: &str, common_dir: &str) -> anyhow::Result<()> {
    let marker = format!("moved-away:{id}:{common_dir}");
    conn.execute(
        "UPDATE projects SET common_dir = ?1 WHERE id = ?2",
        params![marker, id],
    )?;
    Ok(())
}

fn is_moved_away(common_dir: &str, id: &str) -> bool {
    common_dir
        .strip_prefix("moved-away:")
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(project, _)| project == id)
}

/// A project row no one has used: unapproved, no group, no workspaces, requests or shares.
fn is_fresh(conn: &Connection, id: &str) -> anyhow::Result<bool> {
    let fresh = conn.query_row(
        "SELECT approved_at_ms IS NULL AND group_uuid IS NULL \
         AND NOT EXISTS (SELECT 1 FROM workspaces WHERE project_id = ?1) \
         AND NOT EXISTS (SELECT 1 FROM secret_requests WHERE project_id = ?1) \
         AND NOT EXISTS (SELECT 1 FROM secret_shares WHERE source_project = ?1 \
         OR target_project = ?1) FROM projects WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?;
    Ok(fresh)
}

/// `unix:<dev>:<ino>:<btime>`; the birth time is `-` on filesystems that do not record it.
fn unix_parts(identity: &str) -> Option<(&str, &str, &str)> {
    let rest = identity.strip_prefix("unix:")?;
    let mut parts = rest.split(':');
    let dev = parts.next()?;
    let ino = parts.next()?;
    let btime = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some((dev, ino, btime))
}

/// Device renumbering keeps a project: the same inode and birth time on another device is the
/// same directory. Without a recorded birth time (`-`, so inode reuse cannot be detected) the
/// inode number alone must not carry another project's approval across a device change (T3-2).
fn same_dir_identity(stored: &str, now: &str) -> bool {
    if stored == now {
        return true;
    }
    match (unix_parts(stored), unix_parts(now)) {
        (Some((_, ino_stored, btime_stored)), Some((_, ino_now, btime_now))) => {
            ino_stored == ino_now && btime_stored == btime_now && btime_stored != "-"
        }
        _ => false,
    }
}

fn workspace_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<WorkspaceRow> {
    Ok(WorkspaceRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path: row.get(2)?,
        git_dir: row.get(3)?,
        created_at_ms: row.get(4)?,
        last_seen_ms: row.get(5)?,
        root_dev: row.get::<_, Option<i64>>(6)?.map(|d| d as u64),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A fake filesystem: path → identity for the paths that exist.
    #[derive(Default)]
    struct Fs(BTreeMap<String, String>);

    impl Fs {
        fn set(&mut self, path: &str, identity: &str) {
            self.0.insert(path.into(), identity.into());
        }

        fn identity(&self, path: &str) -> Option<String> {
            self.0.get(path).cloned()
        }
    }

    fn resolve(
        store: &Store,
        fs: &Fs,
        common_dir: &str,
        now: i64,
    ) -> (ProjectInfo, ProjectResolution) {
        let identity = fs.identity(common_dir);
        store
            .resolve_project(
                common_dir,
                "app",
                identity.as_deref(),
                &|path| fs.identity(path),
                now,
            )
            .unwrap()
    }

    fn identity_at(store: &Store, id: &str) -> Option<String> {
        project_identity(&store.conn, id).unwrap()
    }

    #[test]
    fn reused_path_with_another_repo_is_a_new_project() {
        let store = Store::open_in_memory().unwrap();
        let mut fs = Fs::default();
        fs.set("/work/x/.git", "unix:1:10:100");

        let (old, resolution) = resolve(&store, &fs, "/work/x/.git", 1);
        assert_eq!(resolution, ProjectResolution::Created);
        store.approve_project(&old.id, 2).unwrap();
        store.set_project_group(&old.id, Some("group-1")).unwrap();

        // mv x y; git clone other x
        fs.set("/work/y/.git", "unix:1:10:100");
        fs.set("/work/x/.git", "unix:1:11:200");

        let (new_at_x, resolution) = resolve(&store, &fs, "/work/x/.git", 3);
        assert_eq!(
            resolution,
            ProjectResolution::MovedAway {
                old_id: old.id.clone()
            }
        );
        assert_ne!(new_at_x.id, old.id);
        assert!(!new_at_x.approved);
        assert_eq!(new_at_x.common_dir, "/work/x/.git");

        let (moved, resolution) = resolve(&store, &fs, "/work/y/.git", 4);
        assert_eq!(
            resolution,
            ProjectResolution::Repointed {
                from_path: format!("moved-away:{}:/work/x/.git", old.id)
            }
        );
        assert_eq!(moved.id, old.id);
        assert!(moved.approved);
        assert_eq!(moved.group_uuid.as_deref(), Some("group-1"));
        assert_eq!(moved.common_dir, "/work/y/.git");
        assert_eq!(store.projects().unwrap().len(), 2);
    }

    #[test]
    fn reused_path_resolved_after_the_move_repoints_before_creating() {
        let store = Store::open_in_memory().unwrap();
        let mut fs = Fs::default();
        fs.set("/work/x/.git", "unix:1:10:100");
        let (old, _) = resolve(&store, &fs, "/work/x/.git", 1);
        store.approve_project(&old.id, 2).unwrap();
        store.set_project_group(&old.id, Some("group-1")).unwrap();

        fs.set("/work/y/.git", "unix:1:10:100");
        fs.set("/work/x/.git", "unix:1:11:200");

        // The moved directory resolves first: the old project follows the identity.
        let (moved, resolution) = resolve(&store, &fs, "/work/y/.git", 3);
        assert_eq!(
            resolution,
            ProjectResolution::Repointed {
                from_path: "/work/x/.git".into()
            }
        );
        assert_eq!(moved.id, old.id);
        assert!(moved.approved);

        let (new_at_x, resolution) = resolve(&store, &fs, "/work/x/.git", 4);
        assert_eq!(resolution, ProjectResolution::Created);
        assert_ne!(new_at_x.id, old.id);
        assert!(!new_at_x.approved);
        assert_eq!(
            identity_at(&store, &new_at_x.id).as_deref(),
            Some("unix:1:11:200")
        );
    }

    #[test]
    fn missing_identity_is_filled_at_the_next_resolve() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        assert_eq!(identity_at(&store, &project.id), None);

        let mut fs = Fs::default();
        fs.set("/work/x/.git", "unix:1:10:100");
        let (row, resolution) = resolve(&store, &fs, "/work/x/.git", 2);
        assert_eq!(resolution, ProjectResolution::Existing);
        assert_eq!(row.id, project.id);
        assert_eq!(
            identity_at(&store, &project.id).as_deref(),
            Some("unix:1:10:100")
        );
    }

    #[test]
    fn device_renumbering_keeps_the_project() {
        let store = Store::open_in_memory().unwrap();
        let mut fs = Fs::default();
        fs.set("/work/x/.git", "unix:10:20:30");
        let (project, _) = resolve(&store, &fs, "/work/x/.git", 1);

        fs.set("/work/x/.git", "unix:99:20:30");
        let (same, resolution) = resolve(&store, &fs, "/work/x/.git", 2);
        assert_eq!(resolution, ProjectResolution::Existing);
        assert_eq!(same.id, project.id);
        assert_eq!(
            identity_at(&store, &project.id).as_deref(),
            Some("unix:99:20:30"),
            "the recorded device number follows the renumbering"
        );

        // Same inode number, another birth time: a different directory.
        fs.set("/work/x/.git", "unix:99:20:31");
        let (other, resolution) = resolve(&store, &fs, "/work/x/.git", 3);
        assert_eq!(
            resolution,
            ProjectResolution::MovedAway {
                old_id: project.id.clone()
            }
        );
        assert_ne!(other.id, project.id);
    }

    #[test]
    fn device_renumbering_without_a_birth_time_is_a_different_directory() {
        let store = Store::open_in_memory().unwrap();
        let mut fs = Fs::default();
        fs.set("/work/x/.git", "unix:10:20:-");
        let (project, _) = resolve(&store, &fs, "/work/x/.git", 1);
        store.approve_project(&project.id, 2).unwrap();
        store
            .set_project_group(&project.id, Some("group-1"))
            .unwrap();

        // Same inode number, no birth time anywhere: another device at this path must not
        // inherit the old project's approval (T3-2).
        fs.set("/work/x/.git", "unix:99:20:-");
        let (other, resolution) = resolve(&store, &fs, "/work/x/.git", 3);
        assert_eq!(
            resolution,
            ProjectResolution::MovedAway {
                old_id: project.id.clone()
            }
        );
        assert_ne!(other.id, project.id);
        assert!(!other.approved);
        assert_eq!(other.group_uuid, None);
    }

    #[test]
    fn identity_unknown_falls_back_to_the_path() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        let fs = Fs::default();

        let (row, resolution) = resolve(&store, &fs, "/work/x/.git", 2);
        assert_eq!(resolution, ProjectResolution::Existing);
        assert_eq!(row.id, project.id);
        assert_eq!(identity_at(&store, &project.id), None);

        let (created, resolution) = resolve(&store, &fs, "/work/y/.git", 3);
        assert_eq!(resolution, ProjectResolution::Created);
        assert!(!created.approved);
        assert_eq!(identity_at(&store, &created.id), None);
    }

    #[test]
    fn repoint_project_keeps_approval_and_group() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        store.approve_project(&project.id, 2).unwrap();
        store
            .set_project_group(&project.id, Some("group-1"))
            .unwrap();

        assert!(store.repoint_project(&project.id, "/work/y/.git").unwrap());
        let moved = store.project(&project.id).unwrap().unwrap();
        assert_eq!(moved.common_dir, "/work/y/.git");
        assert!(moved.approved);
        assert_eq!(moved.group_uuid.as_deref(), Some("group-1"));
        assert_eq!(moved.created_at_ms, 1);
        assert_eq!(identity_at(&store, &project.id), None);
        assert_eq!(store.project_by_dir("/work/x/.git").unwrap(), None);
        assert!(!store.repoint_project("missing", "/work/z/.git").unwrap());
    }

    #[test]
    fn repoint_project_deletes_a_fresh_duplicate() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        let duplicate = store.upsert_project("/work/y/.git", "app", 2).unwrap();
        assert!(!duplicate.approved);

        assert!(store.repoint_project(&project.id, "/work/y/.git").unwrap());
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().common_dir,
            "/work/y/.git"
        );
        assert_eq!(store.project(&duplicate.id).unwrap(), None);
        assert_eq!(store.projects().unwrap().len(), 1);
    }

    #[test]
    fn repoint_project_refuses_a_used_duplicate() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        let duplicate = store.upsert_project("/work/y/.git", "other", 2).unwrap();
        store.approve_project(&duplicate.id, 3).unwrap();

        assert!(!store.repoint_project(&project.id, "/work/y/.git").unwrap());
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().common_dir,
            "/work/x/.git"
        );
        assert_eq!(
            store.project(&duplicate.id).unwrap().unwrap().common_dir,
            "/work/y/.git"
        );
    }

    #[test]
    fn repoint_project_target_already_its_own_path_changes_nothing() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        assert!(store.repoint_project(&project.id, "/work/x/.git").unwrap());
        assert_eq!(store.projects().unwrap(), vec![project]);
    }

    #[test]
    fn workspace_upsert_keeps_created_time() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/app/.git", "app", 1).unwrap();
        store
            .upsert_workspace("w1", &project.id, "/work/app", "/work/app/.git", 10)
            .unwrap();
        let row = store.workspace("w1").unwrap().unwrap();
        assert_eq!(
            row,
            WorkspaceRow {
                id: "w1".into(),
                project_id: project.id.clone(),
                path: "/work/app".into(),
                git_dir: "/work/app/.git".into(),
                created_at_ms: 10,
                last_seen_ms: 10,
                root_dev: None,
            }
        );

        store
            .upsert_workspace("w1", &project.id, "/work/moved", "/work/moved/.git", 20)
            .unwrap();
        let row = store.workspace("w1").unwrap().unwrap();
        assert_eq!(row.created_at_ms, 10);
        assert_eq!(row.last_seen_ms, 20);
        assert_eq!(row.path, "/work/moved");
        assert_eq!(store.workspaces().unwrap().len(), 1);
        assert_eq!(store.workspace("missing").unwrap(), None);
    }

    /// P5: a declaration is stored once per change, compared as a value, and read back.
    #[test]
    fn declared_is_stored_when_it_changes() {
        let store = Store::open_in_memory().unwrap();
        let project = store.upsert_project("/work/x/.git", "app", 1).unwrap();
        let decl = crate::contract::projects::fixtures::decl();
        assert!(store.declarations().unwrap().is_empty());
        assert!(store.set_declared(&project.id, &decl, 10).unwrap());
        assert!(!store.set_declared(&project.id, &decl, 20).unwrap());
        assert_eq!(
            store.declarations().unwrap()[&project.id],
            (decl.clone(), 10)
        );
        let mut changed = decl;
        changed.apps.ios.clear();
        assert!(store.set_declared(&project.id, &changed, 30).unwrap());
        assert_eq!(store.declarations().unwrap()[&project.id], (changed, 30));
    }
}
