//! Account requests in the store (P5 Task 5, spec §8.4): a mirror of `secret_requests`, with the
//! requested field names as a JSON array. Names only; no field value is ever recorded.

use std::collections::BTreeSet;

use anyhow::Context;
use rusqlite::params;

use super::Store;
use crate::contract::accounts::{AccountRequestInfo, AccountRequestNew};
use crate::contract::secrets::RequestState;
use crate::procs::ProcessIdentity;

const REQUEST_COLUMNS: &str = "SELECT r.id, r.project_id, p.name, r.name, r.fields_json, \
     r.reason, r.state, r.requester_json, r.created_at_ms, r.answered_at_ms \
     FROM account_requests r JOIN projects p ON p.id = r.project_id";

struct Row {
    info: AccountRequestInfo,
    requester: ProcessIdentity,
}

impl Store {
    /// Idempotent by request id: a repeated id returns the recorded request unchanged.
    pub fn insert_account_request(
        &self,
        request: &AccountRequestNew,
        project_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<AccountRequestInfo> {
        let fields =
            serde_json::to_string(&request.fields).context("cannot encode the request's fields")?;
        let requester =
            serde_json::to_string(&request.requester).context("cannot encode the requester")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO account_requests (id, project_id, name, fields_json, \
                 reason, requester_json, state, created_at_ms, answered_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, NULL)",
                params![
                    request.id,
                    project_id,
                    request.name,
                    fields,
                    request.reason,
                    requester,
                    now_ms
                ],
            )
            .context("cannot record the account request")?;
        self.account_request(&request.id)?
            .context("the account request just recorded is missing")
    }

    pub fn account_request(&self, id: &str) -> anyhow::Result<Option<AccountRequestInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{REQUEST_COLUMNS} WHERE r.id = ?1"))?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_from_row(row)?.info)),
            None => Ok(None),
        }
    }

    pub fn pending_account_requests(
        &self,
    ) -> anyhow::Result<Vec<(AccountRequestInfo, ProcessIdentity)>> {
        let mut stmt = self.conn.prepare(&format!(
            "{REQUEST_COLUMNS} WHERE r.state = 'pending' ORDER BY r.created_at_ms, r.id"
        ))?;
        let mut rows = stmt.query([])?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next()? {
            let row = row_from_row(row)?;
            requests.push((row.info, row.requester));
        }
        Ok(requests)
    }

    /// True when the request was pending and is now in `state`; false when it was already
    /// answered.
    pub fn finish_account_request(
        &self,
        id: &str,
        state: RequestState,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE account_requests SET state = ?1, answered_at_ms = ?2 \
             WHERE id = ?3 AND state = 'pending'",
            params![state_str(state), now_ms, id],
        )?;
        Ok(changed == 1)
    }

    /// Answers every pending request for one project and account whose fields are all in `set`;
    /// one immediate transaction, so a request's fields and its state cannot fall apart.
    /// Returns the answered ids in recorded order.
    pub fn answer_account_requests(
        &self,
        project_id: &str,
        name: &str,
        set: &BTreeSet<String>,
        state: RequestState,
        now_ms: i64,
    ) -> anyhow::Result<Vec<String>> {
        let tx = self.immediate_tx()?;
        let mut ids = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id, fields_json FROM account_requests \
                 WHERE project_id = ?1 AND name = ?2 AND state = 'pending' \
                 ORDER BY created_at_ms, id",
            )?;
            let rows = stmt.query_map(params![project_id, name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut candidates = Vec::new();
            for row in rows {
                let (id, fields) = row?;
                let fields: Vec<String> =
                    serde_json::from_str(&fields).context("cannot decode a request's fields")?;
                if fields.iter().all(|field| set.contains(field)) {
                    candidates.push(id);
                }
            }
            for id in candidates {
                let changed = tx.execute(
                    "UPDATE account_requests SET state = ?1, answered_at_ms = ?2 \
                     WHERE id = ?3 AND state = 'pending'",
                    params![state_str(state), now_ms, id],
                )?;
                if changed == 1 {
                    ids.push(id);
                }
            }
        }
        tx.commit().context("cannot answer the account requests")?;
        Ok(ids)
    }

    /// Terminal requests whose *answer* (not creation) is older than the cutoff, as
    /// `secret_requests`: a long-pending request that just became terminal is kept (R1-3).
    pub fn prune_account_requests(&self, older_than_ms: i64) -> anyhow::Result<usize> {
        let removed = self.conn.execute(
            "DELETE FROM account_requests WHERE state <> 'pending' \
             AND answered_at_ms IS NOT NULL AND answered_at_ms < ?1",
            [older_than_ms],
        )?;
        Ok(removed)
    }
}

fn row_from_row(row: &rusqlite::Row<'_>) -> anyhow::Result<Row> {
    let fields_json: String = row.get(4)?;
    let requester_json: String = row.get(7)?;
    let fields: Vec<String> =
        serde_json::from_str(&fields_json).context("cannot decode a request's fields")?;
    let requester: ProcessIdentity =
        serde_json::from_str(&requester_json).context("cannot decode a request's requester")?;
    let state: String = row.get(6)?;
    Ok(Row {
        info: AccountRequestInfo {
            id: row.get(0)?,
            project_id: row.get(1)?,
            project_name: row.get(2)?,
            name: row.get(3)?,
            fields,
            reason: row.get(5)?,
            state: parse_state(&state)?,
            requester_pid: requester.pid,
            created_at_ms: row.get(8)?,
            answered_at_ms: row.get(9)?,
        },
        requester,
    })
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
        other => anyhow::bail!("unknown account request state {other:?}"),
    }
}
