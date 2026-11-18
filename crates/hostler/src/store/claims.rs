//! Claims, waiting requests, claim history and seizures (plan P3a, Task 3).

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};

use super::Store;
use crate::contract::claims::{
    ClaimHistoryItem, ClaimKind, ClaimRequest, EndReason, HoldBinding, Purpose, ResourceId,
    ResourceKind, SeizedFrom, SeizureRecord, Selector,
};
use crate::contract::heavy::{HistoryQuery, MAX_HISTORY_LIMIT};
use crate::procs::{ProcessIdentity, RunGroup};

/// A live claim, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimRow {
    pub id: String,
    pub resource: ResourceId,
    pub kind: ClaimKind,
    pub parent: Option<String>,
    pub workspace_id: String,
    pub holder: Option<ProcessIdentity>,
    pub child: Option<ProcessIdentity>,
    pub group: Option<RunGroup>,
    pub binding: Option<HoldBinding>,
    pub command: Option<String>,
    pub granted_at_ms: i64,
}

/// A request that has not been granted, withdrawn or reaped yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRow {
    pub id: String,
    pub ticket: u64,
    pub selector: Selector,
    pub workspace_id: String,
    pub requester: ProcessIdentity,
    pub purpose: Purpose,
    pub created_at_ms: i64,
}

const CLAIM_COLUMNS: &str = "SELECT id, resource, kind, parent, workspace_id, holder_json, \
                             child_json, group_json, binding_json, command, granted_at_ms \
                             FROM claims";
const REQUEST_COLUMNS: &str = "SELECT id, ticket, selector_json, workspace_id, requester_json, \
                               purpose_json, created_at_ms FROM claim_requests";
const HISTORY_COLUMNS: &str = "SELECT id, resource, kind, workspace_path, command, granted_at_ms, \
                               ended_at_ms, reason, exit_code, detail, resource_id \
                               FROM claim_history";

impl Store {
    /// L28: the enclosing lease (its claim id) or heavy entry (`heavy:<id>`) of a claim request
    /// or claim, found with the acquire's one parent lookup.
    pub fn set_nested_in(&self, id: &str, nested_in: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE claim_requests SET nested_in = ?1 WHERE id = ?2",
            params![nested_in, id],
        )?;
        self.conn.execute(
            "UPDATE claims SET nested_in = ?1 WHERE id = ?2",
            params![nested_in, id],
        )?;
        Ok(())
    }

    /// L28: as `set_nested_in`, for a heavy entry.
    pub fn set_heavy_nested_in(&self, id: &str, nested_in: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE heavy_entries SET nested_in = ?1 WHERE id = ?2",
            params![nested_in, id],
        )?;
        Ok(())
    }

    /// Every live claim's and heavy entry's (`heavy:<id>`) enclosing run, by id.
    pub fn nestings(&self) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, nested_in FROM claims WHERE nested_in IS NOT NULL \
             UNION ALL SELECT 'heavy:' || id, nested_in FROM heavy_entries \
             WHERE nested_in IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Records a waiting request; the ticket is monotonic across restarts and reopens. A repeated
    /// *pending* id returns the ticket it already has and changes nothing; an id that is already
    /// a live claim or in claim history is refused (its state is read from there, not queued).
    pub fn insert_claim_request(&self, r: &ClaimRequest, now_ms: i64) -> anyhow::Result<u64> {
        let selector = serde_json::to_string(&r.selector).context("cannot encode the selector")?;
        let requester =
            serde_json::to_string(&r.requester).context("cannot encode the requester")?;
        let purpose = serde_json::to_string(&r.purpose).context("cannot encode the purpose")?;

        let tx = self.immediate_tx()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT ticket FROM claim_requests WHERE id = ?1",
                [&r.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(ticket) = existing {
            tx.commit()?;
            return Ok(ticket as u64);
        }
        if let Some(where_found) = known_claim_id(&tx, &r.id)? {
            anyhow::bail!("claim request {} is already {where_found}", r.id);
        }

        let ticket = match super::setting(&tx, "claims.next_ticket")?.as_deref() {
            Some(raw) => raw
                .parse::<i64>()
                .context("claims.next_ticket is not an integer")?,
            None => 1,
        };
        super::set_setting(&tx, "claims.next_ticket", &(ticket + 1).to_string())?;
        tx.execute(
            "INSERT INTO claim_requests (id, ticket, selector_json, workspace_id, requester_json, \
             purpose_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                r.id,
                ticket,
                selector,
                r.workspace_id,
                requester,
                purpose,
                now_ms
            ],
        )
        .context("cannot record the claim request")?;
        tx.commit()?;
        Ok(ticket as u64)
    }

    /// Every waiting request, oldest ticket first.
    pub fn claim_requests(&self) -> anyhow::Result<Vec<PendingRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{REQUEST_COLUMNS} ORDER BY ticket"))?;
        let mut rows = stmt.query([])?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next()? {
            requests.push(pending_from_row(row)?);
        }
        Ok(requests)
    }

    /// One transaction: the request becomes a live claim with its own id and is removed from the
    /// queue. The claim keeps the requester as its holder; a lease keeps its display command.
    pub fn grant(
        &self,
        request_id: &str,
        resource: &ResourceId,
        kind: ClaimKind,
        parent: Option<&str>,
        binding: Option<&HoldBinding>,
        now_ms: i64,
    ) -> anyhow::Result<ClaimRow> {
        let tx = self.immediate_tx()?;
        let pending = pending_by_id(&tx, request_id)?
            .with_context(|| format!("claim request {request_id} is not queued"))?;
        let command = match &pending.purpose {
            Purpose::Lease { command } => Some(command.clone()),
            Purpose::Hold { .. } => None,
        };
        let holder =
            serde_json::to_string(&pending.requester).context("cannot encode the holder")?;
        let binding_json = binding
            .map(serde_json::to_string)
            .transpose()
            .context("cannot encode the binding")?;
        // The enclosing run found at acquire (L28) stays with the claim.
        tx.execute(
            "INSERT INTO claims (id, resource, kind, parent, workspace_id, holder_json, child_json, \
             group_json, binding_json, command, granted_at_ms, nested_in) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, ?9, \
             (SELECT nested_in FROM claim_requests WHERE id = ?1))",
            params![
                request_id,
                resource.0,
                kind_str(kind),
                parent,
                pending.workspace_id,
                holder,
                binding_json,
                command,
                now_ms
            ],
        )
        .context("cannot insert the claim")?;
        tx.execute("DELETE FROM claim_requests WHERE id = ?1", [request_id])?;
        tx.commit()?;

        Ok(ClaimRow {
            id: request_id.to_owned(),
            resource: resource.clone(),
            kind,
            parent: parent.map(str::to_owned),
            workspace_id: pending.workspace_id,
            holder: Some(pending.requester),
            child: None,
            group: None,
            binding: binding.cloned(),
            command,
            granted_at_ms: now_ms,
        })
    }

    /// Inserts a claim the store did not grant from a request: a linked claim, a reservation or a
    /// seized one.
    pub fn insert_claim(&self, row: &ClaimRow) -> anyhow::Result<()> {
        let holder = opt_json(&row.holder, "the holder")?;
        let child = opt_json(&row.child, "the child")?;
        let group = opt_json(&row.group, "the run group")?;
        let binding = opt_json(&row.binding, "the binding")?;
        self.conn
            .execute(
                "INSERT INTO claims (id, resource, kind, parent, workspace_id, holder_json, \
                 child_json, group_json, binding_json, command, granted_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    row.id,
                    row.resource.0,
                    kind_str(row.kind),
                    row.parent,
                    row.workspace_id,
                    holder,
                    child,
                    group,
                    binding,
                    row.command,
                    row.granted_at_ms
                ],
            )
            .context("cannot insert the claim")?;
        Ok(())
    }

    /// Every live claim, oldest first.
    pub fn claims(&self) -> anyhow::Result<Vec<ClaimRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{CLAIM_COLUMNS} ORDER BY granted_at_ms, id"))?;
        let mut rows = stmt.query([])?;
        let mut claims = Vec::new();
        while let Some(row) = rows.next()? {
            claims.push(claim_from_row(row)?);
        }
        Ok(claims)
    }

    pub fn claim(&self, id: &str) -> anyhow::Result<Option<ClaimRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{CLAIM_COLUMNS} WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(claim_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// False when the claim already ended.
    pub fn set_claim_started(
        &self,
        id: &str,
        child: &ProcessIdentity,
        group: &RunGroup,
    ) -> anyhow::Result<bool> {
        let child = serde_json::to_string(child).context("cannot encode the child")?;
        let group = serde_json::to_string(group).context("cannot encode the run group")?;
        let changed = self.conn.execute(
            "UPDATE claims SET child_json = ?1, group_json = ?2 WHERE id = ?3",
            params![child, group, id],
        )?;
        Ok(changed == 1)
    }

    /// False when the claim already ended.
    pub fn set_claim_command(&self, id: &str, command: &str) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE claims SET command = ?1 WHERE id = ?2",
            params![command, id],
        )?;
        Ok(changed == 1)
    }

    pub fn set_binding(&self, id: &str, binding: &HoldBinding) -> anyhow::Result<()> {
        let binding = serde_json::to_string(binding).context("cannot encode the binding")?;
        let changed = self.conn.execute(
            "UPDATE claims SET binding_json = ?1 WHERE id = ?2",
            params![binding, id],
        )?;
        anyhow::ensure!(changed == 1, "claim {id} is not live");
        Ok(())
    }

    /// Moves a live claim to history in one transaction; false when it already ended.
    pub fn end_claim(
        &self,
        id: &str,
        reason: EndReason,
        exit_code: Option<i32>,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        self.end_claim_with(id, reason, exit_code, None, now_ms)
    }

    /// `end_claim` with the history row's `detail` (a seized claim's: the taker's workspace path).
    pub fn end_claim_with(
        &self,
        id: &str,
        reason: EndReason,
        exit_code: Option<i32>,
        detail: Option<&str>,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let tx = self.immediate_tx()?;
        // LEFT JOIN: a claim must reach history even if its workspace row is somehow gone.
        let moved = tx.execute(
            "INSERT INTO claim_history (id, resource, kind, workspace_path, command, granted_at_ms, \
             ended_at_ms, reason, exit_code, detail, resource_id) \
             SELECT c.id, c.resource, c.kind, COALESCE(w.path, c.workspace_id), c.command, \
             c.granted_at_ms, ?1, ?2, ?3, ?5, c.resource FROM claims c \
             LEFT JOIN workspaces w ON w.id = c.workspace_id WHERE c.id = ?4",
            params![now_ms, reason_str(reason), exit_code, id, detail],
        )?;
        if moved == 0 {
            return Ok(false);
        }
        tx.execute("DELETE FROM claims WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Moves a waiting request to history in one transaction; false when it is no longer queued.
    /// A request that never got a resource has `granted_at_ms` NULL.
    pub fn end_request(
        &self,
        id: &str,
        reason: EndReason,
        detail: Option<&str>,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let tx = self.immediate_tx()?;
        let Some(pending) = pending_by_id(&tx, id)? else {
            tx.commit()?;
            return Ok(false);
        };
        let kind = match &pending.purpose {
            Purpose::Lease { .. } => ClaimKind::Lease,
            Purpose::Hold { .. } => ClaimKind::Hold,
        };
        let command = match &pending.purpose {
            Purpose::Lease { command } => Some(command.clone()),
            Purpose::Hold { .. } => None,
        };
        let workspace_path = workspace_path(&tx, &pending.workspace_id)?;
        let resource = selector_resource(&pending.selector);
        tx.execute(
            "INSERT INTO claim_history (id, resource, kind, workspace_path, command, granted_at_ms, \
             ended_at_ms, reason, exit_code, detail, resource_id) VALUES (?1, ?2, ?3, ?4, ?5, NULL, \
             ?6, ?7, NULL, ?8, ?9)",
            params![
                id,
                resource,
                kind_str(kind),
                workspace_path,
                command,
                now_ms,
                reason_str(reason),
                detail,
                stable_resource_id(&resource)
            ],
        )
        .context("cannot record the ended request")?;
        tx.execute("DELETE FROM claim_requests WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn history_item_of(&self, id: &str) -> anyhow::Result<Option<ClaimHistoryItem>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{HISTORY_COLUMNS} WHERE id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(history_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Newest first.
    pub fn claim_history(&self, q: &HistoryQuery) -> anyhow::Result<Vec<ClaimHistoryItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "{HISTORY_COLUMNS} WHERE (?1 IS NULL OR ended_at_ms >= ?1) \
             ORDER BY ended_at_ms DESC, id DESC LIMIT ?2"
        ))?;
        let limit = q.limit.min(MAX_HISTORY_LIMIT) as i64;
        let mut rows = stmt.query(params![q.since_ms, limit])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            items.push(history_from_row(row)?);
        }
        Ok(items)
    }

    pub fn record_seizure(&self, s: &SeizureRecord) -> anyhow::Result<()> {
        self.conn
            .execute(
                "INSERT INTO seizures (id, resource, from_workspace_path, from_kind, from_pid, \
                 by_workspace_path, by_pid, at_ms, stranded, forced, from_source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    s.id,
                    s.resource,
                    s.from_workspace_path,
                    seized_from_str(s.from_kind),
                    s.from_pid,
                    s.by_workspace_path,
                    s.by_pid,
                    s.at_ms,
                    s.stranded as i64,
                    s.forced as i64,
                    s.from_source
                ],
            )
            .context("cannot record the seizure")?;
        Ok(())
    }

    /// Seizures, newest first (as `claim_history`); `resource` is the id as recorded.
    pub fn seizures(&self, q: &HistoryQuery) -> anyhow::Result<Vec<SeizureRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, resource, from_workspace_path, from_kind, from_source, from_pid, \
             by_workspace_path, by_pid, at_ms, stranded, forced FROM seizures \
             WHERE (?1 IS NULL OR at_ms >= ?1) ORDER BY at_ms DESC, id DESC LIMIT ?2",
        )?;
        let limit = q.limit.min(MAX_HISTORY_LIMIT) as i64;
        let mut rows = stmt.query(params![q.since_ms, limit])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            let from_kind: String = row.get(3)?;
            items.push(SeizureRecord {
                id: row.get(0)?,
                resource: row.get(1)?,
                from_workspace_path: row.get(2)?,
                from_kind: parse_seized_from(&from_kind)?,
                from_source: row.get(4)?,
                from_pid: row.get(5)?,
                by_workspace_path: row.get(6)?,
                by_pid: row.get(7)?,
                at_ms: row.get(8)?,
                stranded: row.get::<_, i64>(9)? != 0,
                forced: row.get::<_, i64>(10)? != 0,
            });
        }
        Ok(items)
    }

    /// Drops ended claims and seizures older than the cutoff; live rows stay.
    pub fn prune_claim_history(&self, older_than_ms: i64) -> anyhow::Result<usize> {
        let history = self.conn.execute(
            "DELETE FROM claim_history WHERE ended_at_ms < ?1",
            [older_than_ms],
        )?;
        let seizures = self
            .conn
            .execute("DELETE FROM seizures WHERE at_ms < ?1", [older_than_ms])?;
        Ok(history + seizures)
    }
}

/// Whether the id already belongs to a live claim or to claim history.
fn known_claim_id(conn: &Connection, id: &str) -> anyhow::Result<Option<&'static str>> {
    let live: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM claims WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if live {
        return Ok(Some("a live claim"));
    }
    let ended: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM claim_history WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    Ok(ended.then_some("in claim history"))
}

fn opt_json<T: serde::Serialize>(value: &Option<T>, what: &str) -> anyhow::Result<Option<String>> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .with_context(|| format!("cannot encode {what}"))
}

fn pending_by_id(conn: &Connection, id: &str) -> anyhow::Result<Option<PendingRow>> {
    let mut stmt = conn.prepare(&format!("{REQUEST_COLUMNS} WHERE id = ?1"))?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(row) => Ok(Some(pending_from_row(row)?)),
        None => Ok(None),
    }
}

fn pending_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<PendingRow> {
    let selector_json: String = row.get(2)?;
    let requester_json: String = row.get(4)?;
    let purpose_json: String = row.get(5)?;
    Ok(PendingRow {
        id: row.get(0)?,
        ticket: row.get::<_, i64>(1)? as u64,
        selector: serde_json::from_str(&selector_json)
            .context("claim_requests.selector_json is unreadable")?,
        workspace_id: row.get(3)?,
        requester: serde_json::from_str(&requester_json)
            .context("claim_requests.requester_json is unreadable")?,
        purpose: serde_json::from_str(&purpose_json)
            .context("claim_requests.purpose_json is unreadable")?,
        created_at_ms: row.get(6)?,
    })
}

fn claim_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<ClaimRow> {
    let kind: String = row.get(2)?;
    let holder_json: Option<String> = row.get(5)?;
    let child_json: Option<String> = row.get(6)?;
    let group_json: Option<String> = row.get(7)?;
    let binding_json: Option<String> = row.get(8)?;
    Ok(ClaimRow {
        id: row.get(0)?,
        resource: ResourceId(row.get(1)?),
        kind: parse_kind(&kind)?,
        parent: row.get(3)?,
        workspace_id: row.get(4)?,
        holder: holder_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("claims.holder_json is unreadable")?,
        child: child_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("claims.child_json is unreadable")?,
        group: group_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("claims.group_json is unreadable")?,
        binding: binding_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("claims.binding_json is unreadable")?,
        command: row.get(9)?,
        granted_at_ms: row.get(10)?,
    })
}

fn history_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<ClaimHistoryItem> {
    let kind: String = row.get(2)?;
    let reason: String = row.get(7)?;
    let resource_id: Option<String> = row.get(10)?;
    Ok(ClaimHistoryItem {
        id: row.get(0)?,
        resource: row.get(1)?,
        resource_id: resource_id.map(ResourceId),
        kind: parse_kind(&kind)?,
        workspace_path: row.get(3)?,
        command: row.get(4)?,
        granted_at_ms: row.get(5)?,
        ended_at_ms: row.get(6)?,
        reason: parse_reason(&reason)?,
        exit_code: row.get(8)?,
        detail: row.get(9)?,
    })
}

fn workspace_path(conn: &Connection, workspace_id: &str) -> anyhow::Result<String> {
    let path: Option<String> = conn
        .query_row(
            "SELECT path FROM workspaces WHERE id = ?1",
            [workspace_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(path.unwrap_or_else(|| workspace_id.to_owned()))
}

/// The stable id of a recorded resource string: `Some` for a real resource id, `None` for a
/// selector that never got a resource (`any:lease`) — P5-L16.
fn stable_resource_id(resource: &str) -> Option<String> {
    let id = ResourceId(resource.to_owned());
    id.kind().map(|_| resource.to_owned())
}

fn selector_resource(selector: &Selector) -> String {
    match selector {
        Selector::Exact { resource } => resource.0.clone(),
        Selector::Any { kind, .. } => format!("any:{}", resource_kind_str(*kind)),
    }
}

fn resource_kind_str(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Screen => "screen",
        ResourceKind::Account => "account",
        ResourceKind::Device => "device",
    }
}

fn kind_str(kind: ClaimKind) -> &'static str {
    match kind {
        ClaimKind::Lease => "lease",
        ClaimKind::Hold => "hold",
        ClaimKind::Reservation => "reservation",
    }
}

/// The claim kinds as `kind_str` writes them, plus `foreign` (P2-12).
fn seized_from_str(from: SeizedFrom) -> &'static str {
    match from {
        SeizedFrom::Lease => "lease",
        SeizedFrom::Hold => "hold",
        SeizedFrom::Reservation => "reservation",
        SeizedFrom::Foreign => "foreign",
    }
}

fn parse_seized_from(raw: &str) -> anyhow::Result<SeizedFrom> {
    match raw {
        "foreign" => Ok(SeizedFrom::Foreign),
        other => parse_kind(other).map(SeizedFrom::from),
    }
}

fn parse_kind(raw: &str) -> anyhow::Result<ClaimKind> {
    match raw {
        "lease" => Ok(ClaimKind::Lease),
        "hold" => Ok(ClaimKind::Hold),
        "reservation" => Ok(ClaimKind::Reservation),
        other => anyhow::bail!("unknown claim kind {other:?}"),
    }
}

fn reason_str(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Released => "released",
        EndReason::Expired => "expired",
        EndReason::Reaped => "reaped",
        EndReason::Stranded => "stranded",
        EndReason::Seized => "seized",
        EndReason::Withdrawn => "withdrawn",
        EndReason::TimedOut => "timed_out",
        EndReason::Deadlock => "deadlock",
        EndReason::Refused => "refused",
    }
}

fn parse_reason(raw: &str) -> anyhow::Result<EndReason> {
    match raw {
        "released" => Ok(EndReason::Released),
        "expired" => Ok(EndReason::Expired),
        "reaped" => Ok(EndReason::Reaped),
        "stranded" => Ok(EndReason::Stranded),
        "seized" => Ok(EndReason::Seized),
        "withdrawn" => Ok(EndReason::Withdrawn),
        "timed_out" => Ok(EndReason::TimedOut),
        "deadlock" => Ok(EndReason::Deadlock),
        "refused" => Ok(EndReason::Refused),
        other => anyhow::bail!("unknown claim end reason {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::contract::claims::{Purpose, Selector};
    use crate::contract::secrets::ProjectInfo;

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

    fn request(id: &str) -> ClaimRequest {
        ClaimRequest {
            id: id.into(),
            selector: Selector::Exact {
                resource: ResourceId::screen(),
            },
            workspace_id: "ws-1".into(),
            requester: identity(100),
            purpose: Purpose::Lease {
                command: "make test".into(),
            },
            reentrant_tokens: Vec::new(),
            no_wait: false,
        }
    }

    fn seed(store: &Store) -> ProjectInfo {
        let project = store.upsert_project("/work/app/.git", "app", 1).unwrap();
        store
            .upsert_workspace("ws-1", &project.id, "/work/app", "/work/app/.git", 1)
            .unwrap();
        project
    }

    fn fixture() -> Store {
        let store = Store::open_in_memory().unwrap();
        seed(&store);
        store
    }

    #[test]
    fn tickets_monotonic_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hostler.db");
        {
            let store = Store::open(&path).unwrap();
            seed(&store);
            assert_eq!(store.insert_claim_request(&request("r1"), 1).unwrap(), 1);
            assert_eq!(store.insert_claim_request(&request("r2"), 2).unwrap(), 2);
            store
                .grant("r1", &ResourceId::screen(), ClaimKind::Lease, None, None, 3)
                .unwrap();
            store
                .end_request("r2", EndReason::Withdrawn, None, 4)
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.insert_claim_request(&request("r3"), 5).unwrap(), 3);
    }

    #[test]
    fn request_insert_is_idempotent() {
        let store = fixture();
        let first = request("r1");
        assert_eq!(store.insert_claim_request(&first, 10).unwrap(), 1);

        let mut again = request("r1");
        again.purpose = Purpose::Hold {
            ttl_ms: Some(60_000),
            bound_to: None,
            distinct: true,
        };
        assert_eq!(store.insert_claim_request(&again, 20).unwrap(), 1);

        let pending = store.claim_requests().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "r1");
        assert_eq!(pending[0].ticket, 1);
        assert_eq!(pending[0].purpose, first.purpose);
        assert_eq!(pending[0].created_at_ms, 10);
        assert_eq!(pending[0].workspace_id, "ws-1");
        assert_eq!(pending[0].requester, identity(100));
    }

    #[test]
    fn request_insert_refuses_an_id_that_is_already_a_claim() {
        let store = fixture();
        store.insert_claim_request(&request("c1"), 10).unwrap();
        store
            .grant(
                "c1",
                &ResourceId::screen(),
                ClaimKind::Lease,
                None,
                None,
                20,
            )
            .unwrap();

        let err = store.insert_claim_request(&request("c1"), 30).unwrap_err();
        assert!(
            format!("{err:#}").contains("already a live claim"),
            "{err:#}"
        );

        store
            .end_claim("c1", EndReason::Released, Some(0), 40)
            .unwrap();
        let err = store.insert_claim_request(&request("c1"), 50).unwrap_err();
        assert!(
            format!("{err:#}").contains("already in claim history"),
            "{err:#}"
        );
        assert!(store.claim_requests().unwrap().is_empty());
    }

    #[test]
    fn grant_moves_request_to_claim_atomically() {
        let store = fixture();
        store.insert_claim_request(&request("r1"), 10).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER block_delete BEFORE DELETE ON claim_requests \
                 BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
            )
            .unwrap();

        let err = store
            .grant(
                "r1",
                &ResourceId::screen(),
                ClaimKind::Lease,
                None,
                None,
                20,
            )
            .unwrap_err();
        assert!(format!("{err:#}").contains("blocked"), "{err:#}");
        assert_eq!(store.claim("r1").unwrap(), None);
        assert_eq!(store.claim_requests().unwrap().len(), 1);

        store
            .conn
            .execute_batch("DROP TRIGGER block_delete")
            .unwrap();
        let claim = store
            .grant(
                "r1",
                &ResourceId::screen(),
                ClaimKind::Lease,
                Some("h1"),
                None,
                20,
            )
            .unwrap();
        assert_eq!(claim.id, "r1");
        assert_eq!(claim.resource, ResourceId::screen());
        assert_eq!(claim.parent.as_deref(), Some("h1"));
        assert_eq!(claim.holder, Some(identity(100)));
        assert_eq!(claim.child, None);
        assert_eq!(claim.group, None);
        assert_eq!(claim.command.as_deref(), Some("make test"));
        assert_eq!(claim.granted_at_ms, 20);
        assert!(store.claim_requests().unwrap().is_empty());
        assert_eq!(store.claim("r1").unwrap(), Some(claim));
    }

    #[test]
    fn claims_round_trip_every_column() {
        let store = fixture();
        let row = ClaimRow {
            id: "c1".into(),
            resource: ResourceId("account:p1/qa-user".into()),
            kind: ClaimKind::Hold,
            parent: None,
            workspace_id: "ws-1".into(),
            holder: Some(identity(100)),
            child: Some(identity(101)),
            group: Some(group(101)),
            binding: Some(HoldBinding::Ttl {
                ttl_ms: 1_800_000,
                expires_at_ms: 9_000,
            }),
            command: None,
            granted_at_ms: 5,
        };
        store.insert_claim(&row).unwrap();
        assert_eq!(store.claim("c1").unwrap(), Some(row));
        assert_eq!(store.claims().unwrap().len(), 1);
    }

    #[test]
    fn end_claim_moves_to_history_once() {
        let store = fixture();
        store.insert_claim_request(&request("c1"), 10).unwrap();
        store
            .grant(
                "c1",
                &ResourceId::screen(),
                ClaimKind::Lease,
                None,
                None,
                20,
            )
            .unwrap();
        assert!(
            store
                .set_claim_started("c1", &identity(200), &group(200))
                .unwrap()
        );
        assert!(store.set_claim_command("c1", "make test --quiet").unwrap());

        assert!(
            store
                .end_claim("c1", EndReason::Released, Some(0), 30)
                .unwrap()
        );
        let item = store.history_item_of("c1").unwrap().unwrap();
        assert_eq!(item.id, "c1");
        assert_eq!(item.resource, "screen");
        // P5-L16: the stable id, so a rename never loses the row.
        assert_eq!(item.resource_id, Some(ResourceId::screen()));
        assert_eq!(item.kind, ClaimKind::Lease);
        assert_eq!(item.workspace_path, "/work/app");
        assert_eq!(item.command.as_deref(), Some("make test --quiet"));
        assert_eq!(item.granted_at_ms, Some(20));
        assert_eq!(item.ended_at_ms, 30);
        assert_eq!(item.reason, EndReason::Released);
        assert_eq!(item.exit_code, Some(0));
        assert_eq!(item.detail, None);

        assert!(store.claims().unwrap().is_empty());
        assert!(
            !store
                .end_claim("c1", EndReason::Released, Some(0), 40)
                .unwrap()
        );
        assert_eq!(store.history_item_of("c1").unwrap(), Some(item));
    }

    #[test]
    fn withdrawn_request_reaches_history_without_grant_time() {
        let store = fixture();
        store.insert_claim_request(&request("r1"), 10).unwrap();

        assert!(
            store
                .end_request("r1", EndReason::Withdrawn, Some("the client left"), 20)
                .unwrap()
        );
        let item = store.history_item_of("r1").unwrap().unwrap();
        assert_eq!(item.resource, "screen");
        assert_eq!(item.kind, ClaimKind::Lease);
        assert_eq!(item.workspace_path, "/work/app");
        assert_eq!(item.command.as_deref(), Some("make test"));
        assert_eq!(item.granted_at_ms, None);
        assert_eq!(item.ended_at_ms, 20);
        assert_eq!(item.reason, EndReason::Withdrawn);
        assert_eq!(item.exit_code, None);
        assert_eq!(item.detail.as_deref(), Some("the client left"));
        assert!(store.claim_requests().unwrap().is_empty());
        assert!(
            !store
                .end_request("r1", EndReason::Withdrawn, None, 30)
                .unwrap()
        );
    }

    #[test]
    fn set_claim_started_on_an_ended_claim_is_false() {
        let store = fixture();
        store.insert_claim_request(&request("c1"), 10).unwrap();
        store
            .grant(
                "c1",
                &ResourceId::screen(),
                ClaimKind::Lease,
                None,
                None,
                20,
            )
            .unwrap();
        assert!(
            store
                .set_claim_started("c1", &identity(200), &group(200))
                .unwrap()
        );
        store
            .end_claim("c1", EndReason::Released, None, 30)
            .unwrap();

        assert!(
            !store
                .set_claim_started("c1", &identity(201), &group(201))
                .unwrap()
        );
        assert!(!store.set_claim_command("c1", "make test").unwrap());
    }

    #[test]
    fn prune_keeps_live_rows() {
        let store = fixture();
        store.insert_claim_request(&request("c-live"), 10).unwrap();
        store
            .grant(
                "c-live",
                &ResourceId::screen(),
                ClaimKind::Lease,
                None,
                None,
                10,
            )
            .unwrap();

        store.insert_claim_request(&request("r-old"), 10).unwrap();
        store
            .end_request("r-old", EndReason::TimedOut, None, 20)
            .unwrap();
        store
            .record_seizure(&SeizureRecord {
                id: "s-old".into(),
                resource: "screen".into(),
                from_workspace_path: "/work/app".into(),
                from_kind: ClaimKind::Lease.into(),
                from_source: None,
                from_pid: Some(42),
                by_workspace_path: "/work/other".into(),
                by_pid: 77,
                at_ms: 20,
                stranded: false,
                forced: false,
            })
            .unwrap();

        store.insert_claim_request(&request("c-new"), 90).unwrap();
        store
            .grant(
                "c-new",
                &ResourceId::screen(),
                ClaimKind::Lease,
                None,
                None,
                90,
            )
            .unwrap();
        store
            .end_claim("c-new", EndReason::Released, Some(0), 100)
            .unwrap();

        assert_eq!(store.prune_claim_history(50).unwrap(), 2);
        assert!(store.claim("c-live").unwrap().is_some());
        let history = store.claim_history(&HistoryQuery::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "c-new");
        let seizures: i64 = store
            .conn
            .query_row("SELECT count(*) FROM seizures", [], |row| row.get(0))
            .unwrap();
        assert_eq!(seizures, 0);
    }

    #[test]
    fn seizure_round_trip() {
        let store = fixture();
        let seizure = SeizureRecord {
            id: "s1".into(),
            resource: "screen".into(),
            from_workspace_path: "/work/app".into(),
            from_kind: ClaimKind::Hold.into(),
            from_source: None,
            from_pid: Some(42),
            by_workspace_path: "/work/other".into(),
            by_pid: 77,
            at_ms: 3_000,
            stranded: true,
            forced: true,
        };
        store.record_seizure(&seizure).unwrap();

        let row = store
            .conn
            .query_row(
                "SELECT resource, from_workspace_path, from_kind, from_pid, by_workspace_path, \
                 by_pid, at_ms, stranded, forced FROM seizures WHERE id = 's1'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i32>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "screen".into(),
                "/work/app".into(),
                "hold".into(),
                Some(42),
                "/work/other".into(),
                77,
                3_000,
                1,
                1
            )
        );
    }

    #[test]
    fn foreign_seizure_round_trips_with_source() {
        let store = fixture();
        let claim = SeizureRecord {
            id: "s1".into(),
            resource: "device:android:SERIAL0001".into(),
            from_workspace_path: "/work/app".into(),
            from_kind: ClaimKind::Lease.into(),
            from_source: None,
            from_pid: Some(42),
            by_workspace_path: "/work/other".into(),
            by_pid: 77,
            at_ms: 3_000,
            stranded: false,
            forced: false,
        };
        let foreign = SeizureRecord {
            id: "s2".into(),
            from_kind: SeizedFrom::Foreign,
            from_source: Some("agent-device".into()),
            from_workspace_path: "/work/app/sub".into(),
            from_pid: None,
            at_ms: 3_001,
            ..claim.clone()
        };
        store.record_seizure(&claim).unwrap();
        store.record_seizure(&foreign).unwrap();

        let raw: (String, Option<String>) = store
            .conn
            .query_row(
                "SELECT from_kind, from_source FROM seizures WHERE id = 's2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(raw, ("foreign".into(), Some("agent-device".into())));
        assert_eq!(
            store.seizures(&HistoryQuery::default()).unwrap(),
            [foreign.clone(), claim.clone()]
        );
        let since = store
            .seizures(&HistoryQuery {
                since_ms: Some(3_001),
                limit: 10,
            })
            .unwrap();
        assert_eq!(since, [foreign]);
    }

    #[test]
    fn history_is_newest_first_and_limited() {
        let store = fixture();
        for (id, at) in [("a", 10), ("b", 20), ("c", 30)] {
            store.insert_claim_request(&request(id), 1).unwrap();
            store
                .grant(id, &ResourceId::screen(), ClaimKind::Lease, None, None, 2)
                .unwrap();
            store
                .end_claim(id, EndReason::Released, Some(0), at)
                .unwrap();
        }

        let ids: Vec<String> = store
            .claim_history(&HistoryQuery::default())
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, ["c", "b", "a"]);
        let limited = store
            .claim_history(&HistoryQuery {
                since_ms: Some(25),
                limit: 100,
            })
            .unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, "c");
    }
}
