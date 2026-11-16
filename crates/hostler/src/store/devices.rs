//! The device registry in the store (P3b Task 5): every device the service has seen, its alias and
//! note, and the adb transport serials it was seen on (P-10). The service's own connection (the
//! 4th) writes it; every write is one short statement (P2-19), so it never holds the write lock
//! while `claims.acquire` writes.

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::Store;
use crate::contract::claims::ResourceId;
use crate::contract::devices::{
    AppState, DeviceKind, ForgetRecord, InstallChange, InstallEvent, InstallTime, Platform,
};
use crate::contract::heavy::{HistoryQuery, MAX_HISTORY_LIMIT};
use crate::devices::boot::BootRow;
use crate::devices::windows::ClaimWindow;

/// Transport serials kept per device besides its hardware serial (P-10).
pub const RECENT_SERIALS: usize = 8;
/// An unchanged serial list is written again at most this often (P3bT5-12).
const SERIAL_REWRITE_MS: i64 = 60_000;

/// A serial the device was seen on, and when last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialSeen {
    pub serial: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRow {
    pub resource: ResourceId,
    pub platform: Platform,
    pub kind: DeviceKind,
    pub alias: Option<String>,
    pub alias_by_person: bool,
    pub model: String,
    pub name: Option<String>,
    pub os_version: Option<String>,
    pub note: Option<String>,
    pub coredevice_id: Option<String>,
    /// The hardware serial (Android) first, then the newest transport serials.
    pub serials: Vec<SerialSeen>,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub registered_at_ms: Option<i64>,
}

impl DeviceRow {
    pub fn registered(&self) -> bool {
        self.registered_at_ms.is_some()
    }
}

/// What discovery learned about a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceSeen {
    pub resource: ResourceId,
    pub platform: Platform,
    pub kind: DeviceKind,
    pub model: String,
    pub name: Option<String>,
    pub os_version: Option<String>,
    pub coredevice_id: Option<String>,
}

fn platform_str(p: Platform) -> &'static str {
    match p {
        Platform::Android => "android",
        Platform::Ios => "ios",
    }
}

fn parse_platform(s: &str) -> anyhow::Result<Platform> {
    match s {
        "android" => Ok(Platform::Android),
        "ios" => Ok(Platform::Ios),
        other => anyhow::bail!("unknown platform {other:?}"),
    }
}

fn kind_str(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Physical => "physical",
        DeviceKind::Emulator => "emulator",
        DeviceKind::Simulator => "simulator",
    }
}

fn parse_kind(s: &str) -> anyhow::Result<DeviceKind> {
    match s {
        "physical" => Ok(DeviceKind::Physical),
        "emulator" => Ok(DeviceKind::Emulator),
        "simulator" => Ok(DeviceKind::Simulator),
        other => anyhow::bail!("unknown device kind {other:?}"),
    }
}

const COLUMNS: &str = "SELECT resource, platform, kind, alias, alias_by_person, model, name, \
     os_version, note, coredevice_id, serials_json, first_seen_ms, last_seen_ms, \
     registered_at_ms FROM devices";

fn row_of(row: &Row) -> rusqlite::Result<(DeviceRow, String, String, String)> {
    let platform: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let serials: String = row.get(10)?;
    Ok((
        DeviceRow {
            resource: ResourceId(row.get(0)?),
            platform: Platform::Android,
            kind: DeviceKind::Physical,
            alias: row.get(3)?,
            alias_by_person: row.get::<_, i64>(4)? != 0,
            model: row.get(5)?,
            name: row.get(6)?,
            os_version: row.get(7)?,
            note: row.get(8)?,
            coredevice_id: row.get(9)?,
            serials: Vec::new(),
            first_seen_ms: row.get(11)?,
            last_seen_ms: row.get(12)?,
            registered_at_ms: row.get(13)?,
        },
        platform,
        kind,
        serials,
    ))
}

fn finish(parts: (DeviceRow, String, String, String)) -> anyhow::Result<DeviceRow> {
    let (mut row, platform, kind, serials) = parts;
    row.platform = parse_platform(&platform)?;
    row.kind = parse_kind(&kind)?;
    // A damaged list only loses the serial memory, never the device.
    row.serials = serde_json::from_str(&serials).unwrap_or_default();
    Ok(row)
}

impl Store {
    /// Every device, in resource order.
    pub fn devices(&self) -> anyhow::Result<Vec<DeviceRow>> {
        let mut stmt = self.conn.prepare(&format!("{COLUMNS} ORDER BY resource"))?;
        let rows = stmt.query_map([], row_of)?;
        rows.map(|r| finish(r?)).collect()
    }

    pub fn device(&self, r: &ResourceId) -> anyhow::Result<Option<DeviceRow>> {
        self.conn
            .query_row(&format!("{COLUMNS} WHERE resource = ?1"), [&r.0], row_of)
            .optional()?
            .map(finish)
            .transpose()
    }

    /// Inserts an unregistered row for a new device, or updates a known one's facts. An empty
    /// model, or no name, OS version or CoreDevice id, keeps the stored one (P3bT5-12).
    /// `last_seen` is the caller's to throttle (once a minute, or on a change: P2-19).
    pub fn upsert_device_seen(&self, d: &DeviceSeen, now_ms: i64) -> anyhow::Result<()> {
        self.conn
            .execute(
                "INSERT INTO devices (resource, platform, kind, model, name, os_version, \
                 coredevice_id, first_seen_ms, last_seen_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
                 ON CONFLICT (resource) DO UPDATE SET platform = ?2, kind = ?3, \
                 model = COALESCE(NULLIF(?4, ''), model), name = COALESCE(?5, name), \
                 os_version = COALESCE(?6, os_version), \
                 coredevice_id = COALESCE(?7, coredevice_id), \
                 last_seen_ms = MAX(last_seen_ms, ?8)",
                params![
                    d.resource.0,
                    platform_str(d.platform),
                    kind_str(d.kind),
                    d.model,
                    d.name,
                    d.os_version,
                    d.coredevice_id,
                    now_ms
                ],
            )
            .context("cannot record a seen device")?;
        Ok(())
    }

    /// Rows changed by this connection since it opened (tests: no write on a quiet round).
    #[cfg(test)]
    pub(crate) fn total_changes(&self) -> i64 {
        self.conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap_or(-1)
    }

    pub fn touch_device(&self, r: &ResourceId, now_ms: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE devices SET last_seen_ms = MAX(last_seen_ms, ?2) WHERE resource = ?1",
            params![r.0, now_ms],
        )?;
        Ok(())
    }

    /// Remembers that `r` was seen on adb transport `serial` (P-10): the hardware serial stays
    /// first, then the `RECENT_SERIALS` newest others. A serial already listed in that place is
    /// written again only once a minute (P3bT5-12). False when nothing changed but the time.
    pub fn add_device_serial(
        &self,
        r: &ResourceId,
        hardware: Option<&str>,
        serial: &str,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let Some(row) = self.device(r)? else {
            return Ok(false);
        };
        let mut serials = row.serials.clone();
        let known = serials.iter().any(|s| s.serial == serial);
        serials.retain(|s| s.serial != serial);
        let mut hw: Vec<SerialSeen> = Vec::new();
        if let Some(h) = hardware {
            if let Some(at) = serials.iter().position(|s| s.serial == h) {
                hw.push(serials.remove(at));
            } else if h != serial {
                hw.push(SerialSeen {
                    serial: h.to_owned(),
                    at_ms: now_ms,
                });
            }
        }
        let entry = SerialSeen {
            serial: serial.to_owned(),
            at_ms: now_ms,
        };
        let mut others = vec![entry];
        others.extend(serials);
        let keep = if hardware == Some(serial) {
            // The hardware serial is itself a transport serial (USB).
            RECENT_SERIALS + 1
        } else {
            RECENT_SERIALS
        };
        others.truncate(keep);
        let list: Vec<SerialSeen> = hw.into_iter().chain(others).collect();
        if let Some(old) = row.serials.iter().find(|s| s.serial == serial)
            && now_ms - old.at_ms < SERIAL_REWRITE_MS
        {
            let mut same = list.clone();
            for s in same.iter_mut().filter(|s| s.serial == serial) {
                s.at_ms = old.at_ms;
            }
            if same == row.serials {
                return Ok(false);
            }
        }
        self.conn.execute(
            "UPDATE devices SET serials_json = ?2 WHERE resource = ?1",
            params![r.0, serde_json::to_string(&list)?],
        )?;
        Ok(!known)
    }

    /// Registers `r` under `alias` (L49 is the caller's check). An alias another device holds
    /// fails on the table's unique, case-insensitive index.
    pub fn register_device(
        &self,
        r: &ResourceId,
        alias: &str,
        by_person: bool,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE devices SET alias = ?2, alias_by_person = ?3, \
             registered_at_ms = COALESCE(registered_at_ms, ?4) WHERE resource = ?1",
            params![r.0, alias, by_person as i64, now_ms],
        )?;
        Ok(n == 1)
    }

    /// The registered device that holds `alias`, ignoring case.
    pub fn device_by_alias(&self, alias: &str) -> anyhow::Result<Option<DeviceRow>> {
        self.conn
            .query_row(
                &format!("{COLUMNS} WHERE alias = ?1 COLLATE NOCASE"),
                [alias],
                row_of,
            )
            .optional()?
            .map(finish)
            .transpose()
    }

    pub fn set_device_note(&self, r: &ResourceId, note: Option<&str>) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE devices SET note = ?2 WHERE resource = ?1",
            params![r.0, note],
        )?;
        Ok(n == 1)
    }

    /// `devices remove`: the device stays known (seen), without alias or registration.
    pub fn unregister_device(&self, r: &ResourceId) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE devices SET alias = NULL, alias_by_person = 0, registered_at_ms = NULL \
             WHERE resource = ?1",
            [&r.0],
        )?;
        Ok(n == 1)
    }
}

/// The last good agent-device read (L51) and the `forget-sessions` record (L54), Task 9b.
impl Store {
    /// The last good read's sessions, as JSON, and when they were written.
    pub fn agent_device_last(&self) -> anyhow::Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT claims_json, at_ms FROM agent_device_last WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Written only when the list changed (P2-19).
    pub fn set_agent_device_last(&self, claims_json: &str, at_ms: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO agent_device_last (id, claims_json, at_ms) VALUES (1, ?1, ?2) \
             ON CONFLICT (id) DO UPDATE SET claims_json = ?1, at_ms = ?2",
            params![claims_json, at_ms],
        )?;
        Ok(())
    }

    pub fn record_session_forget(&self, r: &ForgetRecord) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO device_session_forgets (id, resource, sessions_json, by_workspace_path, \
             by_pid, reason, at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                r.id,
                r.resource.0,
                serde_json::to_string(&r.sessions)?,
                r.by_workspace_path,
                r.by_pid,
                r.reason,
                r.at_ms
            ],
        )?;
        Ok(())
    }

    /// The `forget-sessions` records of `r`, newest first (`devices show`).
    pub fn session_forgets(&self, r: &ResourceId) -> anyhow::Result<Vec<ForgetRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sessions_json, by_workspace_path, by_pid, reason, at_ms \
             FROM device_session_forgets WHERE resource = ?1 ORDER BY at_ms DESC, id DESC",
        )?;
        let rows = stmt.query_map([&r.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<u32>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (id, sessions, by_workspace_path, by_pid, reason, at_ms) = row?;
            Ok(ForgetRecord {
                id,
                resource: r.clone(),
                sessions: serde_json::from_str(&sessions).unwrap_or_default(),
                by_workspace_path,
                by_pid,
                reason,
                at_ms,
            })
        })
        .collect()
    }
}

/// Task 8's tables, read for `devices show` and `devices installs`; Task 8 writes them.
impl Store {
    /// The apps of `r`'s last snapshot.
    pub fn device_apps(&self, r: &ResourceId) -> anyhow::Result<Vec<AppState>> {
        let mut stmt = self.conn.prepare(
            "SELECT app_id, version_name, version_code, installed_json, updated_json \
             FROM device_apps WHERE resource = ?1 ORDER BY app_id",
        )?;
        let rows = stmt.query_map([&r.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let time = |raw: Option<String>| raw.and_then(|j| serde_json::from_str(&j).ok());
        rows.map(|r| {
            let (app_id, version_name, version_code, installed, updated) = r?;
            Ok(AppState {
                app_id,
                version_name,
                version_code,
                installed: time(installed),
                updated: time(updated),
            })
        })
        .collect()
    }

    /// When `r`'s last good snapshot was taken.
    pub fn device_snapshot_at(&self, r: &ResourceId) -> anyhow::Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT at_ms FROM device_snapshots WHERE resource = ?1 AND ok = 1",
                [&r.0],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Install history, newest first; one device or all. Rows that do not parse are skipped.
    pub fn device_installs(
        &self,
        device: Option<&ResourceId>,
        q: &HistoryQuery,
    ) -> anyhow::Result<Vec<InstallEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.resource, d.alias, i.app_id, i.change, i.version_name, \
             i.version_code, i.when_json, i.source_json, i.recorded_at_ms \
             FROM device_installs i LEFT JOIN devices d ON d.resource = i.resource \
             WHERE (?1 IS NULL OR i.resource = ?1) AND (?2 IS NULL OR i.recorded_at_ms >= ?2) \
             ORDER BY i.recorded_at_ms DESC, i.id DESC LIMIT ?3",
        )?;
        let limit = q.limit.min(MAX_HISTORY_LIMIT) as i64;
        let rows = stmt.query_map(params![device.map(|r| &r.0), q.since_ms, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, resource, alias, app_id, change, vn, vc, when, source, at) = row?;
            let change = match change.as_str() {
                "installed" => InstallChange::Installed,
                "updated" => InstallChange::Updated,
                "removed" => InstallChange::Removed,
                _ => continue,
            };
            let (Ok(when), Ok(source)) =
                (serde_json::from_str(&when), serde_json::from_str(&source))
            else {
                continue;
            };
            out.push(InstallEvent {
                id,
                device: ResourceId(resource),
                alias,
                app_id,
                change,
                version_name: vn,
                version_code: vc,
                when,
                source,
                recorded_at_ms: at,
            });
        }
        Ok(out)
    }
}

/// Task 7's `device_boots`: what hostler booted itself, so the shutdown rule outlives a service
/// restart (P-6). Each write is one statement.
impl Store {
    pub fn device_boots(&self) -> anyhow::Result<Vec<BootRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource, booted_at_ms, identity_json, claim_id, idle_since_ms \
             FROM device_boots ORDER BY resource",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(BootRow {
                resource: ResourceId(row.get(0)?),
                booted_at_ms: row.get(1)?,
                identity_json: row.get(2)?,
                claim_id: row.get(3)?,
                idle_since_ms: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inserts or replaces `b`'s row.
    pub fn put_device_boot(&self, b: &BootRow) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO device_boots \
             (resource, booted_at_ms, identity_json, claim_id, idle_since_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                b.resource.0,
                b.booted_at_ms,
                b.identity_json,
                b.claim_id,
                b.idle_since_ms
            ],
        )?;
        Ok(())
    }

    pub fn set_device_boot_idle(&self, r: &ResourceId, since: Option<i64>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE device_boots SET idle_since_ms = ?2 WHERE resource = ?1",
            params![r.0, since],
        )?;
        Ok(())
    }

    /// L49 (P3bT7-7): an expired or reaped device hold's open sessions, in its history row's
    /// `detail` when it has none.
    pub fn set_claim_history_detail(&self, id: &str, detail: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE claim_history SET detail = ?2 WHERE id = ?1 AND detail IS NULL",
            params![id, detail],
        )?;
        Ok(())
    }

    pub fn delete_device_boot(&self, r: &ResourceId) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM device_boots WHERE resource = ?1", [&r.0])?;
        Ok(())
    }
}

/// Task 8's writes: claim windows (P-5), app snapshots and install events.
impl Store {
    /// A window for a live device claim; `false` when it exists already.
    pub fn insert_claim_window(&self, w: &ClaimWindow) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO claim_windows (claim_id, resource, workspace_id, \
             workspace_path, branch, head_commit, command, from_ms, to_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                w.claim_id,
                w.resource.0,
                w.workspace_id,
                w.workspace_path,
                w.branch,
                w.head_commit,
                w.command,
                w.from_ms,
                w.to_ms
            ],
        )?;
        Ok(n == 1)
    }

    /// Closes an open window; `false` when it was closed already.
    pub fn close_claim_window(&self, claim_id: &str, to_ms: i64) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE claim_windows SET to_ms = ?2 WHERE claim_id = ?1 AND to_ms IS NULL",
            params![claim_id, to_ms],
        )?;
        Ok(n == 1)
    }

    pub fn has_claim_window(&self, claim_id: &str) -> anyhow::Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM claim_windows WHERE claim_id = ?1",
                [claim_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The live device leases and holds on `r`, from the claims engine's own table, as open
    /// windows without a head (R3-1: for one snapshot's attribution, never written).
    pub fn live_device_claims(&self, r: &ResourceId) -> anyhow::Result<Vec<ClaimWindow>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.workspace_id, COALESCE(w.path, c.workspace_id), c.command, \
             c.granted_at_ms FROM claims c LEFT JOIN workspaces w ON w.id = c.workspace_id \
             WHERE c.resource = ?1 AND c.kind IN ('lease', 'hold')",
        )?;
        let rows = stmt.query_map([&r.0], |row| {
            Ok(ClaimWindow {
                claim_id: row.get(0)?,
                resource: r.clone(),
                workspace_id: row.get(1)?,
                workspace_path: row.get(2)?,
                branch: None,
                head_commit: None,
                command: row.get(3)?,
                from_ms: row.get(4)?,
                to_ms: None,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The ended device leases and holds on `r` that were granted by `to_ms` and ended at
    /// `from_ms` or later, from the engine's history, as closed windows without a head.
    pub fn ended_device_claims(
        &self,
        r: &ResourceId,
        from_ms: i64,
        to_ms: i64,
    ) -> anyhow::Result<Vec<ClaimWindow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace_path, command, granted_at_ms, ended_at_ms FROM claim_history \
             WHERE resource = ?1 AND kind IN ('lease', 'hold') AND granted_at_ms IS NOT NULL \
             AND granted_at_ms <= ?3 AND ended_at_ms >= ?2",
        )?;
        let rows = stmt.query_map(params![r.0, from_ms, to_ms], |row| {
            Ok(ClaimWindow {
                claim_id: row.get(0)?,
                resource: r.clone(),
                workspace_id: String::new(),
                workspace_path: row.get(1)?,
                branch: None,
                head_commit: None,
                command: row.get(2)?,
                from_ms: row.get(3)?,
                to_ms: Some(row.get(4)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn open_claim_windows(&self) -> anyhow::Result<Vec<ClaimWindow>> {
        self.claim_windows_where("to_ms IS NULL", params![])
    }

    /// `r`'s windows that were open at some time in `[from_ms, to_ms]`, oldest first.
    pub fn claim_windows_on(
        &self,
        r: &ResourceId,
        from_ms: i64,
        to_ms: i64,
    ) -> anyhow::Result<Vec<ClaimWindow>> {
        self.claim_windows_where(
            "resource = ?1 AND from_ms <= ?3 AND (to_ms IS NULL OR to_ms >= ?2)",
            params![r.0, from_ms, to_ms],
        )
    }

    fn claim_windows_where(
        &self,
        filter: &str,
        args: impl rusqlite::Params,
    ) -> anyhow::Result<Vec<ClaimWindow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT claim_id, resource, workspace_id, workspace_path, branch, head_commit, \
             command, from_ms, to_ms FROM claim_windows WHERE {filter} ORDER BY from_ms, claim_id"
        ))?;
        let rows = stmt.query_map(args, |row| {
            Ok(ClaimWindow {
                claim_id: row.get(0)?,
                resource: ResourceId(row.get(1)?),
                workspace_id: row.get(2)?,
                workspace_path: row.get(3)?,
                branch: row.get(4)?,
                head_commit: row.get(5)?,
                command: row.get(6)?,
                from_ms: row.get(7)?,
                to_ms: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A good snapshot: its apps replace the last ones, in one transaction.
    /// `setting`: a `(key, value)` written in the same transaction (an Android snapshot's clock,
    /// L69: the stored apps and their clock always come from one snapshot). `installs`: the
    /// changes it found, in the same transaction too (L71): all of it is stored, or none.
    pub fn put_device_snapshot(
        &self,
        r: &ResourceId,
        apps: &[AppState],
        at_ms: i64,
        setting: Option<(&str, &str)>,
        installs: &[InstallEvent],
    ) -> anyhow::Result<()> {
        let tx = self.immediate_tx()?;
        for e in installs {
            insert_install(&tx, e)?;
        }
        let first_seen: std::collections::HashMap<String, i64> = {
            let mut stmt =
                tx.prepare("SELECT app_id, first_seen_ms FROM device_apps WHERE resource = ?1")?;
            let rows = stmt.query_map([&r.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        tx.execute("DELETE FROM device_apps WHERE resource = ?1", [&r.0])?;
        let json = |t: &Option<InstallTime>| t.as_ref().and_then(|t| serde_json::to_string(t).ok());
        for app in apps {
            tx.execute(
                "INSERT INTO device_apps (resource, app_id, version_name, version_code, \
                 installed_json, updated_json, first_seen_ms, last_seen_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    r.0,
                    app.app_id,
                    app.version_name,
                    app.version_code,
                    json(&app.installed),
                    json(&app.updated),
                    first_seen.get(&app.app_id).copied().unwrap_or(at_ms),
                    at_ms
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO device_snapshots (resource, at_ms, ok) VALUES (?1, ?2, 1) \
             ON CONFLICT (resource) DO UPDATE SET at_ms = ?2, ok = 1",
            params![r.0, at_ms],
        )?;
        if let Some((key, value)) = setting {
            super::set_setting(&tx, key, value)?;
        }
        tx.commit().context("cannot record an app snapshot")
    }

    /// Install history recorded before `before_ms` (the plan's hourly 90-day pruning).
    pub fn prune_device_installs(&self, before_ms: i64) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM device_installs WHERE recorded_at_ms < ?1",
            [before_ms],
        )?)
    }

    /// Windows closed before `before_ms` (30 days); an open window is never pruned.
    pub fn prune_claim_windows(&self, before_ms: i64) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM claim_windows WHERE to_ms IS NOT NULL AND to_ms < ?1",
            [before_ms],
        )?)
    }
}

fn insert_install(conn: &Connection, e: &InstallEvent) -> anyhow::Result<()> {
    let change = match e.change {
        InstallChange::Installed => "installed",
        InstallChange::Updated => "updated",
        InstallChange::Removed => "removed",
    };
    conn.execute(
        "INSERT INTO device_installs (id, resource, app_id, change, version_name, \
         version_code, when_json, source_json, recorded_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            e.id,
            e.device.0,
            e.app_id,
            change,
            e.version_name,
            e.version_code,
            serde_json::to_string(&e.when)?,
            serde_json::to_string(&e.source)?,
            e.recorded_at_ms
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::devices::InstallSource;

    fn window(id: &str, from_ms: i64, to_ms: Option<i64>) -> ClaimWindow {
        ClaimWindow {
            claim_id: id.into(),
            resource: ResourceId("device:android:X".into()),
            workspace_id: "w".into(),
            workspace_path: "/w".into(),
            branch: None,
            head_commit: None,
            command: None,
            from_ms,
            to_ms,
        }
    }

    fn install(id: &str, recorded_at_ms: i64) -> InstallEvent {
        InstallEvent {
            id: id.into(),
            device: ResourceId("device:android:X".into()),
            alias: None,
            app_id: "com.example.a".into(),
            change: InstallChange::Installed,
            version_name: None,
            version_code: None,
            when: InstallTime::FirstSeen {
                at_ms: recorded_at_ms,
            },
            source: InstallSource::NotThroughHostler,
            recorded_at_ms,
        }
    }

    /// L69: a snapshot's setting (an Android clock) is written in its transaction.
    #[test]
    fn a_snapshot_and_its_setting_are_stored_together() {
        let store = Store::open_in_memory().unwrap();
        let r = ResourceId("device:android:X".into());
        store
            .put_device_snapshot(&r, &[], 10, Some(("devices.clock.x", "-1000 UTC")), &[])
            .unwrap();
        assert_eq!(store.device_snapshot_at(&r).unwrap(), Some(10));
        assert_eq!(
            store.setting("devices.clock.x").unwrap().as_deref(),
            Some("-1000 UTC")
        );
        store.put_device_snapshot(&r, &[], 20, None, &[]).unwrap();
        assert_eq!(
            store.setting("devices.clock.x").unwrap().as_deref(),
            Some("-1000 UTC"),
            "an iOS-style snapshot leaves it"
        );
    }

    /// R1-5: old installs and old closed windows go; recent ones and every open window stay.
    #[test]
    fn device_history_prune_keeps_open_windows() {
        let store = Store::open_in_memory().unwrap();
        for w in [
            window("old-closed", 10, Some(20)),
            window("new-closed", 10, Some(200)),
            window("old-open", 10, None),
        ] {
            assert!(store.insert_claim_window(&w).unwrap());
        }
        store
            .put_device_snapshot(
                &ResourceId("device:android:X".into()),
                &[],
                150,
                None,
                &[install("i-old", 50), install("i-new", 150)],
            )
            .unwrap();
        assert_eq!(store.prune_claim_windows(100).unwrap(), 1);
        assert_eq!(store.prune_device_installs(100).unwrap(), 1);
        let r = ResourceId("device:android:X".into());
        let left: Vec<String> = store
            .claim_windows_on(&r, i64::MIN, i64::MAX)
            .unwrap()
            .into_iter()
            .map(|w| w.claim_id)
            .collect();
        assert_eq!(left, ["new-closed", "old-open"]);
        let q = HistoryQuery {
            since_ms: None,
            limit: 10,
        };
        let installs: Vec<String> = store
            .device_installs(Some(&r), &q)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(installs, ["i-new"]);
    }

    fn seen(r: &str, kind: DeviceKind, model: &str) -> DeviceSeen {
        DeviceSeen {
            resource: ResourceId(r.into()),
            platform: Platform::Android,
            kind,
            model: model.into(),
            name: None,
            os_version: Some("15".into()),
            coredevice_id: None,
        }
    }

    #[test]
    fn devices_are_seen_registered_noted_and_removed() {
        let store = Store::open_in_memory().unwrap();
        let r = ResourceId("device:android:SERIAL0001".into());
        store
            .upsert_device_seen(&seen(&r.0, DeviceKind::Physical, "Pixel 9"), 10)
            .unwrap();
        let row = store.device(&r).unwrap().unwrap();
        assert!(!row.registered());
        assert_eq!((row.first_seen_ms, row.last_seen_ms), (10, 10));
        assert_eq!(row.model, "Pixel 9");

        // Seen again: facts update, first-seen stays, last-seen never goes back.
        let mut again = seen(&r.0, DeviceKind::Physical, "Pixel 9 Pro");
        again.coredevice_id = Some("CD-1".into());
        store.upsert_device_seen(&again, 30).unwrap();
        store
            .upsert_device_seen(&seen(&r.0, DeviceKind::Physical, "Pixel 9 Pro"), 20)
            .unwrap();
        let row = store.device(&r).unwrap().unwrap();
        assert_eq!((row.first_seen_ms, row.last_seen_ms), (10, 30));
        assert_eq!(row.model, "Pixel 9 Pro");
        assert_eq!(
            row.coredevice_id.as_deref(),
            Some("CD-1"),
            "kept when not seen"
        );

        assert!(store.register_device(&r, "pixel-9", true, 40).unwrap());
        let row = store.device(&r).unwrap().unwrap();
        assert_eq!(row.alias.as_deref(), Some("pixel-9"));
        assert!(row.alias_by_person && row.registered());
        assert_eq!(
            store.device_by_alias("PIXEL-9").unwrap().unwrap().resource,
            r
        );

        // Aliases are unique ignoring case.
        let other = ResourceId("device:android:avd:Pixel_9".into());
        store
            .upsert_device_seen(&seen(&other.0, DeviceKind::Emulator, "sdk_gphone"), 50)
            .unwrap();
        assert!(store.register_device(&other, "Pixel-9", false, 50).is_err());

        assert!(store.set_device_note(&r, Some("on the left")).unwrap());
        assert_eq!(
            store.device(&r).unwrap().unwrap().note.as_deref(),
            Some("on the left")
        );
        assert!(store.unregister_device(&r).unwrap());
        let row = store.device(&r).unwrap().unwrap();
        assert!(!row.registered() && row.alias.is_none() && !row.alias_by_person);
        assert_eq!(
            row.note.as_deref(),
            Some("on the left"),
            "the row stays as seen"
        );
        assert_eq!(store.devices().unwrap().len(), 2);
        assert!(
            !store
                .set_device_note(&ResourceId("device:x:y".into()), None)
                .unwrap()
        );
    }

    #[test]
    fn apps_snapshots_and_installs_read_task_8s_rows() {
        let store = Store::open_in_memory().unwrap();
        let r = ResourceId("device:android:SERIAL0001".into());
        store
            .upsert_device_seen(&seen(&r.0, DeviceKind::Physical, "Pixel 9"), 1)
            .unwrap();
        store.register_device(&r, "pixel-9", true, 1).unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO device_apps VALUES ('device:android:SERIAL0001', 'com.example.a', \
                 '1.0', '10', '{\"kind\":\"device\",\"at_ms\":5}', NULL, 1, 2);
                 INSERT INTO device_snapshots VALUES ('device:android:SERIAL0001', 7, 1);
                 INSERT INTO device_installs VALUES ('i1', 'device:android:SERIAL0001', \
                 'com.example.a', 'installed', '1.0', '10', '{\"kind\":\"device\",\"at_ms\":5}', \
                 '{\"kind\":\"not_through_hostler\"}', 9);
                 INSERT INTO device_installs VALUES ('i0', 'device:android:SERIAL0001', \
                 'com.example.b', 'bogus', NULL, NULL, '{}', '{}', 8);",
            )
            .unwrap();
        let apps = store.device_apps(&r).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(
            apps[0].installed,
            Some(crate::contract::devices::InstallTime::Device { at_ms: 5 })
        );
        assert_eq!(store.device_snapshot_at(&r).unwrap(), Some(7));
        let installs = store
            .device_installs(None, &HistoryQuery::default())
            .unwrap();
        assert_eq!(installs.len(), 1, "an unreadable row is skipped");
        assert_eq!(installs[0].alias.as_deref(), Some("pixel-9"));
        assert_eq!(installs[0].change, InstallChange::Installed);
        let other = ResourceId("device:android:OTHER".into());
        assert!(
            store
                .device_installs(Some(&other), &HistoryQuery::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_last_agent_device_read_and_forgets_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.agent_device_last().unwrap(), None);
        store.set_agent_device_last("[1]", 5).unwrap();
        store.set_agent_device_last("[2]", 6).unwrap();
        assert_eq!(store.agent_device_last().unwrap(), Some(("[2]".into(), 6)));
        let r = ResourceId("device:android:SERIAL0001".into());
        let record = ForgetRecord {
            id: "f1".into(),
            resource: r.clone(),
            sessions: vec!["s1".into()],
            by_workspace_path: Some("/work/app".into()),
            by_pid: Some(42),
            reason: "agent-device status unreadable: node not found".into(),
            at_ms: 7,
        };
        store.record_session_forget(&record).unwrap();
        assert_eq!(store.session_forgets(&r).unwrap(), [record]);
    }

    /// P-10: the hardware serial, then the eight newest transport serials, with times.
    #[test]
    fn serials_keep_the_hardware_serial_and_the_newest_eight() {
        let store = Store::open_in_memory().unwrap();
        let r = ResourceId("device:android:SERIAL0001".into());
        store
            .upsert_device_seen(&seen(&r.0, DeviceKind::Physical, "Pixel 9"), 1)
            .unwrap();
        assert!(
            store
                .add_device_serial(&r, Some("SERIAL0001"), "SERIAL0001", 1)
                .unwrap()
        );
        for i in 0..10 {
            store
                .add_device_serial(&r, Some("SERIAL0001"), &format!("192.0.2.{i}:5555"), 10 + i)
                .unwrap();
        }
        let serials: Vec<String> = store
            .device(&r)
            .unwrap()
            .unwrap()
            .serials
            .into_iter()
            .map(|s| s.serial)
            .collect();
        assert_eq!(serials[0], "SERIAL0001");
        assert_eq!(serials.len(), 1 + RECENT_SERIALS);
        assert_eq!(serials[1], "192.0.2.9:5555", "newest first");
        assert!(!serials.contains(&"192.0.2.0:5555".to_owned()));
        // Seen again: not new, moves to the front.
        assert!(
            !store
                .add_device_serial(&r, Some("SERIAL0001"), "192.0.2.5:5555", 99)
                .unwrap()
        );
        let row = store.device(&r).unwrap().unwrap();
        assert_eq!(row.serials[1].serial, "192.0.2.5:5555");
        assert_eq!(row.serials[1].at_ms, 99);
        // An unknown device records nothing.
        assert!(
            !store
                .add_device_serial(&ResourceId("device:x:y".into()), None, "s", 1)
                .unwrap()
        );
    }

    #[test]
    fn device_boots_are_put_idled_and_deleted() {
        let store = Store::open_in_memory().unwrap();
        let r = ResourceId("device:android:avd:Pixel_9".into());
        let mut row = BootRow {
            resource: r.clone(),
            booted_at_ms: 10,
            identity_json: "{}".into(),
            claim_id: "c1".into(),
            idle_since_ms: None,
        };
        store.put_device_boot(&row).unwrap();
        row.identity_json = "{\"serial\":\"emulator-5554\"}".into();
        store.put_device_boot(&row).unwrap();
        store.set_device_boot_idle(&r, Some(20)).unwrap();
        row.idle_since_ms = Some(20);
        assert_eq!(store.device_boots().unwrap(), vec![row]);
        store.delete_device_boot(&r).unwrap();
        assert!(store.device_boots().unwrap().is_empty());
    }
}
