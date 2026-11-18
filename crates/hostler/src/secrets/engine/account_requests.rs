//! Account requests (P5 Task 5, spec §8.4): a mirror of secret requests.
//!
//! An agent asks for an account's fields by name; a person answers with one card (`account.answer`,
//! every field in one vault save) or with `hostler account set` per field — a set answers every
//! pending request whose fields are now all set. Declining is a person command or the app, and it
//! passes the person gate (spec §8.5). Values never travel out of the vault through these methods:
//! an answer goes in, a request carries names only.

use std::collections::BTreeSet;
use std::sync::Mutex;

use keepass::Database;
use keepass::db::GroupId;

use super::{
    Caller, MAX_ACCOUNT_FIELDS, MAX_PENDING_REQUESTS, MAX_REASON_BYTES, SecretsEngine, bad_request,
    duplicate_entries, group_of, internal, store_error,
};
use crate::contract::HostlerError;
use crate::contract::accounts::{
    AccountAnswer, AccountAnswerOutcome, AccountRequestInfo, AccountRequestNew, INVALID_FIELD_NAME,
    valid_account_name, valid_field_name,
};
use crate::contract::secrets::{ProjectInfo, RequestState};
use crate::contract::wire::Event;
use crate::procs::{self, Liveness};
use crate::secrets::check_value;
use crate::secrets::gate::{self, GrantMeta};
use crate::secrets::layout::{self, AccountLookup};
use crate::secrets::vault::VaultError;
use crate::store::Store;

impl SecretsEngine {
    fn check_account_name(name: &str) -> Result<(), HostlerError> {
        if valid_account_name(name) {
            Ok(())
        } else {
            Err(bad_request(crate::contract::accounts::INVALID_ACCOUNT_NAME))
        }
    }

    /// The account's field names, read from the vault; missing (and no group) is empty. A
    /// duplicated account is a conflict: a person keeps one of the entries.
    fn account_field_names(
        &self,
        project: &ProjectInfo,
        name: &str,
    ) -> Result<Vec<String>, HostlerError> {
        let Some(group) = group_of(project) else {
            return Ok(Vec::new());
        };
        self.with_vault(|v| {
            v.read(|db| match layout::account(db, group, name) {
                AccountLookup::Found(fields) => {
                    Ok(fields.into_iter().map(|(field, _)| field).collect())
                }
                AccountLookup::Missing => Ok(Vec::new()),
                AccountLookup::Duplicate => Err(duplicate_entries(name)),
            })
        })
        .map_err(|e| e.to_hostler())?
    }

    /// The set of field names of the account right now.
    fn account_field_set(
        &self,
        project: &ProjectInfo,
        name: &str,
    ) -> Result<BTreeSet<String>, HostlerError> {
        Ok(self
            .account_field_names(project, name)?
            .into_iter()
            .collect())
    }

    /// The request recorded under `r.id`, when it is the same payload. A different project,
    /// account, field set or reason is a mismatch: an id belongs to one request only (R1-2).
    fn known_account_request(
        &self,
        r: &AccountRequestNew,
    ) -> Result<Option<AccountRequestInfo>, HostlerError> {
        let store = self.store()?;
        let Some(known) = store.account_request(&r.id).map_err(store_error)? else {
            return Ok(None);
        };
        let recorded = store.project(&known.project_id).map_err(store_error)?;
        let canonical = std::fs::canonicalize(&r.project.common_dir)
            .map(|d| crate::project::display_path(&d))
            .ok();
        let same_project = recorded.is_some_and(|p| {
            p.common_dir == r.project.common_dir
                || canonical.as_deref() == Some(p.common_dir.as_str())
        });
        let same_fields: BTreeSet<&str> = known.fields.iter().map(String::as_str).collect();
        let requested: BTreeSet<&str> = r.fields.iter().map(String::as_str).collect();
        if !same_project
            || known.name != r.name
            || same_fields != requested
            || known.reason != r.reason
        {
            return Err(bad_request(
                "this request id belongs to another project, account, field set or reason",
            ));
        }
        Ok(Some(known))
    }

    /// `account.request` (spec §8.4): idempotent by id; the project must be approved; names and
    /// fields are checked; the request is recorded until a person answers it. An account whose
    /// requested fields are all set answers `stored` at once.
    pub(super) fn account_request(
        &self,
        r: AccountRequestNew,
    ) -> Result<AccountRequestInfo, HostlerError> {
        // A known id answers its recorded state, whatever the lock state (a waiter re-sends).
        // Repeated below under the publish guard, which is where it is authoritative (R1-2).
        if let Some(known) = self.known_account_request(&r)? {
            return Ok(self.recheck_account(known));
        }
        if uuid::Uuid::parse_str(&r.id)
            .map(|u| u.get_version_num() != 4)
            .unwrap_or(true)
        {
            return Err(bad_request("a request id is a UUID v4"));
        }
        Self::check_account_name(&r.name)?;
        if r.fields.is_empty() || r.fields.len() > MAX_ACCOUNT_FIELDS {
            return Err(bad_request(format!(
                "a request names 1 to {MAX_ACCOUNT_FIELDS} fields"
            )));
        }
        let unique: BTreeSet<&str> = r.fields.iter().map(String::as_str).collect();
        if unique.len() != r.fields.len() {
            return Err(bad_request("a request names each field once"));
        }
        for field in &r.fields {
            if !valid_field_name(field) {
                return Err(bad_request(INVALID_FIELD_NAME));
            }
        }
        if r.reason.trim().is_empty() || r.reason.len() > MAX_REASON_BYTES {
            return Err(bad_request(format!(
                "the reason must be 1 to {MAX_REASON_BYTES} bytes"
            )));
        }
        let project = self.approved(&r.project)?;
        self.require_unlocked()?;
        // No set, answer, decline, withdraw or tick can run between the check and the insert
        // (T6-3, R1-1/R1-2). The lookup is repeated here: two same-id calls that both saw
        // "unknown" above cannot both insert, and the second answers its mismatch.
        let _publishing = self.publishing();
        if let Some(known) = self.known_account_request(&r)? {
            drop(_publishing);
            return Ok(self.recheck_account(known));
        }
        let project = self.current(&project)?;
        let now = self.now();
        let have = self.account_field_set(&project, &r.name)?;
        if r.fields.iter().all(|field| have.contains(field)) {
            return Ok(AccountRequestInfo {
                id: r.id,
                project_id: project.id,
                project_name: project.name,
                name: r.name,
                fields: r.fields,
                reason: r.reason,
                state: RequestState::Stored,
                requester_pid: r.requester.pid,
                created_at_ms: now,
                answered_at_ms: Some(now),
            });
        }
        self.pause_at("account_request_checked");
        let store = self.store()?;
        if store.pending_account_requests().map_err(store_error)?.len() >= MAX_PENDING_REQUESTS {
            return Err(HostlerError {
                code: crate::contract::ErrorCode::QueueBusy,
                message: format!(
                    "{MAX_PENDING_REQUESTS} account requests are already pending on this machine"
                ),
                details: serde_json::json!({ "limit": MAX_PENDING_REQUESTS }),
                recovery: Some("hostler account requests".into()),
            });
        }
        let info = store
            .insert_account_request(&r, &project.id, now)
            .map_err(store_error)?;
        self.emit(Event::AccountRequestChanged(info.clone()));
        drop(store);
        Ok(info)
    }

    /// A re-sent pending request whose fields have all been set since is answered now. Locked:
    /// the recorded state, without the vault. Terminal states never change. Never called with
    /// the publish guard held.
    fn recheck_account(&self, known: AccountRequestInfo) -> AccountRequestInfo {
        if known.state != RequestState::Pending
            || !self.unlocked.load(std::sync::atomic::Ordering::SeqCst)
        {
            return known;
        }
        let _publishing = self.publishing();
        let answered = (|| -> Result<AccountRequestInfo, HostlerError> {
            let project = self
                .store()?
                .project(&known.project_id)
                .map_err(store_error)?
                .ok_or_else(|| internal("the request's project is gone"))?;
            let have = self.account_field_set(&project, &known.name)?;
            if known.fields.iter().all(|field| have.contains(field)) {
                self.answer_account_requests_for(&project, &known.name)?;
            }
            self.store()?
                .account_request(&known.id)
                .map_err(store_error)?
                .ok_or_else(|| internal("the account request is gone"))
        })();
        answered.unwrap_or(known)
    }

    pub(super) fn account_request_withdraw(
        &self,
        id: &str,
        timed_out: bool,
    ) -> Result<AccountRequestInfo, HostlerError> {
        let state = if timed_out {
            RequestState::TimedOut
        } else {
            RequestState::Withdrawn
        };
        // One ordering with the answer and every other terminal transition (R1-1).
        let _publishing = self.publishing();
        let store = self.store()?;
        let changed = store
            .finish_account_request(id, state, self.now())
            .map_err(store_error)?;
        let info = store
            .account_request(id)
            .map_err(store_error)?
            .ok_or_else(|| bad_request("unknown account request"))?;
        if changed {
            self.emit(Event::AccountRequestChanged(info.clone()));
        }
        drop(store);
        Ok(info)
    }

    pub(super) fn account_requests(&self) -> Result<Vec<AccountRequestInfo>, HostlerError> {
        Ok(self
            .store()?
            .pending_account_requests()
            .map_err(store_error)?
            .into_iter()
            .map(|(info, _)| info)
            .collect())
    }

    /// Gated (spec §8.5): the grant is admitted in a vault section before the request changes.
    /// `id` is a request id or 8+ characters of one (the CLI prints the first 8).
    pub(super) fn account_decline(
        &self,
        id: &str,
        caller: &Caller,
    ) -> Result<AccountRequestInfo, HostlerError> {
        if id.is_empty() {
            return Err(bad_request("give a request id"));
        }
        // One ordering with the answer and every other terminal transition (R1-1); the publish
        // guard comes before the vault's admission, as everywhere else.
        let _publishing = self.publishing();
        self.admit_grant(caller.grant.as_ref())?;
        let store = self.store()?;
        let prefix = id.to_ascii_lowercase();
        let pending = store.pending_account_requests().map_err(store_error)?;
        let matching: Vec<&AccountRequestInfo> = pending
            .iter()
            .map(|(info, _)| info)
            .filter(|info| info.id == prefix || (prefix.len() >= 8 && info.id.starts_with(&prefix)))
            .collect();
        let full = match matching.as_slice() {
            [one] => one.id.clone(),
            [] => return Err(bad_request("no pending account request has this id")),
            _ => {
                return Err(bad_request(
                    "several pending requests start with this id; give more of it",
                ));
            }
        };
        let changed = store
            .finish_account_request(&full, RequestState::Declined, self.now())
            .map_err(store_error)?;
        if !changed {
            return Err(bad_request("no pending account request has this id"));
        }
        let info = store
            .account_request(&full)
            .map_err(store_error)?
            .ok_or_else(|| bad_request("unknown account request"))?;
        self.emit(Event::AccountRequestChanged(info.clone()));
        drop(store);
        Ok(info)
    }

    /// `account.answer` (gated, spec §8.5): every field of the request stored in one vault save,
    /// then the request — and every other pending request of the same account whose fields are
    /// now all set — marked `stored`.
    pub(super) fn account_answer(
        &self,
        a: AccountAnswer,
        caller: &Caller,
    ) -> Result<AccountAnswerOutcome, HostlerError> {
        if a.request_id.is_empty() {
            return Err(bad_request("an answer names its request"));
        }
        // The pause point of the interleaving test: an answer that has started but not yet
        // taken the ordering (R1-1).
        self.pause_at("account_answer_admit");
        // From the first read to the store write, one ordering with every terminal transition
        // (decline, withdraw, the dead-requester tick): a request that is no longer pending is
        // never written (R1-1).
        let _publishing = self.publishing();
        let requested = {
            let store = self.store()?;
            let request = store
                .account_request(&a.request_id)
                .map_err(store_error)?
                .ok_or_else(|| bad_request("unknown account request"))?;
            if request.state != RequestState::Pending {
                return Err(bad_request("the account request is not pending"));
            }
            let project = store
                .project(&request.project_id)
                .map_err(store_error)?
                .ok_or_else(|| internal("the request's project is gone"))?;
            (request, project)
        };
        let (pending, project) = requested;
        // The answer is exactly the request's fields, once each, with valid values: every
        // precondition is checked before the vault, so one bad field stores nothing.
        let requested: BTreeSet<&str> = pending.fields.iter().map(String::as_str).collect();
        let answered: BTreeSet<&str> = a.fields.iter().map(|e| e.name.as_str()).collect();
        if answered.len() != a.fields.len() || answered != requested {
            return Err(bad_request(
                "the answer must hold exactly the request's fields, once each",
            ));
        }
        for entry in &a.fields {
            if !valid_field_name(&entry.name) {
                return Err(bad_request(INVALID_FIELD_NAME));
            }
            check_value(&entry.value)?;
        }
        self.require_unlocked()?;
        let project = self.current(&project)?;
        // The decisive re-read, immediately before the vault write (R1-1).
        {
            let store = self.store()?;
            let request = store
                .account_request(&pending.id)
                .map_err(store_error)?
                .ok_or_else(|| bad_request("unknown account request"))?;
            if request.state != RequestState::Pending {
                return Err(bad_request("the account request is not pending"));
            }
        }
        // The pause point of the ordering test: the answer holds the guard here, between its
        // decisive re-read and the vault write (R1-1).
        self.pause_at("account_answer_checked");
        let stored_fields: Vec<String> = a.fields.iter().map(|e| e.name.clone()).collect();
        self.admitted_write_group(caller.grant.as_ref(), &project, |db, group| {
            let existing = match layout::account(db, group, &pending.name) {
                AccountLookup::Duplicate => return Err(duplicate_entries(&pending.name)),
                AccountLookup::Found(fields) => fields,
                AccountLookup::Missing => Vec::new(),
            };
            let new = a
                .fields
                .iter()
                .filter(|entry| !existing.iter().any(|(field, _)| *field == entry.name))
                .count();
            if existing.len() + new > MAX_ACCOUNT_FIELDS {
                return Err(bad_request(format!(
                    "an account has at most {MAX_ACCOUNT_FIELDS} fields"
                )));
            }
            for entry in &a.fields {
                layout::put_account_field(db, group, &pending.name, &entry.name, &entry.value);
            }
            Ok(())
        })?;
        // The group may have been created by this write; the record follows it.
        let project = self.current(&project)?;
        // The account's complete field set now, under the vault; the store is taken after it.
        let set = self.account_field_set(&project, &pending.name)?;
        let now = self.now();
        let store = self.store()?;
        if !store
            .finish_account_request(&pending.id, RequestState::Stored, now)
            .map_err(store_error)?
        {
            return Err(bad_request("the account request is not pending"));
        }
        let mut answered_requests = vec![pending.id.clone()];
        answered_requests.extend(
            store
                .answer_account_requests(
                    &project.id,
                    &pending.name,
                    &set,
                    RequestState::Stored,
                    now,
                )
                .map_err(store_error)?,
        );
        for id in &answered_requests {
            if let Some(info) = store.account_request(id).map_err(store_error)? {
                self.emit(Event::AccountRequestChanged(info));
            }
        }
        drop(store);
        Ok(AccountAnswerOutcome {
            name: pending.name,
            stored_fields,
            answered_requests,
        })
    }

    /// Answers every pending request of `project`/`name` whose fields are now all set. The caller
    /// holds the `publish` guard, so the field read and the answer are one step with the set (or
    /// the re-send) that caused them.
    pub(super) fn answer_account_requests_for(
        &self,
        project: &ProjectInfo,
        name: &str,
    ) -> Result<Vec<AccountRequestInfo>, HostlerError> {
        // A set may have just created the project's group; the record follows it.
        let project = self.current(project)?;
        let set = self.account_field_set(&project, name)?;
        let store = self.store()?;
        let ids = store
            .answer_account_requests(&project.id, name, &set, RequestState::Stored, self.now())
            .map_err(store_error)?;
        let mut infos = Vec::new();
        for id in ids {
            if let Some(info) = store.account_request(&id).map_err(store_error)? {
                infos.push(info);
            }
        }
        // Under the store guard that wrote them (spec §6.2).
        for info in &infos {
            self.emit(Event::AccountRequestChanged(info.clone()));
        }
        drop(store);
        Ok(infos)
    }

    /// The pending account requests for `requests.snapshot`, read under the store guard the
    /// caller holds (spec §6.2).
    pub(super) fn pending_account_requests(
        &self,
        store: &Store,
    ) -> Result<Vec<AccountRequestInfo>, HostlerError> {
        Ok(store
            .pending_account_requests()
            .map_err(store_error)?
            .into_iter()
            .map(|(info, _)| info)
            .collect())
    }

    /// The 2 s tick under the store guard (the caller holds the publish guard and publishes what
    /// it answers): a request whose requester is gone is withdrawn, in the same ordering as a
    /// decline and an answer (R1-1).
    pub(super) fn tick_account_requests(&self, store: &Store, now: i64) -> Vec<AccountRequestInfo> {
        let pending = match store.pending_account_requests() {
            Ok(pending) => pending,
            Err(e) => {
                tracing::warn!("cannot read pending account requests: {e:#}");
                return Vec::new();
            }
        };
        let mut changed = Vec::new();
        for (info, requester) in pending {
            if matches!(procs::liveness(&requester, &*self.table), Liveness::Alive) {
                continue;
            }
            if store
                .finish_account_request(&info.id, RequestState::Withdrawn, now)
                .unwrap_or(false)
                && let Ok(Some(info)) = store.account_request(&info.id)
            {
                changed.push(info);
            }
        }
        changed
    }

    /// One vault write into `project`'s group with the grant admitted in the same section
    /// (spec §8.5); the group used is recorded after the write, as `write_group` does.
    fn admitted_write_group<T>(
        &self,
        grant: Option<&GrantMeta>,
        project: &ProjectInfo,
        change: impl Fn(&mut Database, GroupId) -> Result<T, HostlerError>,
    ) -> Result<T, HostlerError> {
        let now = self.now();
        let before = group_of(project);
        let used: Mutex<Option<GroupId>> = Mutex::new(None);
        let result = self.with_vault(|v| {
            gate::admit(grant, now, v.generation()).map_err(VaultError::Rejected)?;
            v.write(|db: &mut Database| {
                let group = layout::ensure_group(db, before, &project.name);
                let out = change(db, group)?;
                if let Ok(mut used) = used.lock() {
                    *used = Some(group);
                }
                Ok(out)
            })
        });
        if matches!(&result, Ok(_) | Err(VaultError::NotDurable))
            && let Some(group) = used.lock().ok().and_then(|g| *g)
            && Some(group) != before
        {
            self.store()?
                .set_project_group(&project.id, Some(&group.uuid().to_string()))
                .map_err(store_error)?;
            self.events.mark_projects_dirty();
        }
        result.map_err(|e| e.to_hostler())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::world;
    use super::*;
    use crate::contract::accounts::AccountAnswer;
    use crate::contract::secrets::{ImportEntry, ProjectRef, Prove};
    use crate::contract::wire::Request;
    use crate::procs::ProcessIdentity;
    use crate::secrets::engine::Reply;
    use crate::secrets::value::SecretValue;

    const PASSWORD: &str = "pass-1234";
    const MARKER: &str = "account-answer-2f71";

    /// A live requester for the store (no tick runs in these tests).
    fn requester() -> ProcessIdentity {
        ProcessIdentity {
            pid: 4242,
            boot_id: "b1".into(),
            start_time: 42_420,
        }
    }

    /// A caller holding a fresh proof.
    fn proven(w: &super::super::tests::World) -> Caller {
        let answer = w
            .engine
            .dispatch_as(
                Request::SecretsProve(Prove {
                    password: SecretValue::new(PASSWORD.into()),
                    key_file: None,
                }),
                &Caller::default(),
            )
            .expect("prove");
        assert!(answer.grant.is_some());
        Caller {
            peer_pid: Some(4242),
            grant: answer.grant,
        }
    }

    fn ask(
        w: &super::super::tests::World,
        p: &ProjectRef,
        id: &str,
        fields: &[&str],
    ) -> Result<AccountRequestInfo, HostlerError> {
        w.call_as(Request::AccountRequest(AccountRequestNew {
            id: id.into(),
            project: p.clone(),
            name: "qa-user".into(),
            fields: fields.iter().map(|f| (*f).into()).collect(),
            reason: "run the login test".into(),
            requester: requester(),
        }))
    }

    fn answer(
        w: &super::super::tests::World,
        caller: &Caller,
        id: &str,
        fields: &[(&str, &str)],
    ) -> Result<AccountAnswerOutcome, HostlerError> {
        let answer = w.engine.dispatch_as(
            Request::AccountAnswer(AccountAnswer {
                request_id: id.into(),
                fields: fields
                    .iter()
                    .map(|(name, value)| ImportEntry {
                        name: (*name).into(),
                        value: SecretValue::new((*value).into()),
                    })
                    .collect(),
            }),
            caller,
        )?;
        match answer.reply {
            Reply::Value(value) => Ok(serde_json::from_value(value).unwrap()),
            Reply::Sensitive(_) => panic!("account.answer answers names"),
        }
    }

    fn pending_ids(w: &super::super::tests::World) -> Vec<String> {
        w.call_as::<Vec<AccountRequestInfo>>(Request::AccountRequests)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    #[test]
    fn answer_answers_every_satisfied_request_of_the_account() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let one = uuid::Uuid::new_v4().to_string();
        let two = uuid::Uuid::new_v4().to_string();
        let three = uuid::Uuid::new_v4().to_string();
        ask(&w, &p, &one, &["session"]).unwrap();
        ask(&w, &p, &two, &["session", "env"]).unwrap();
        ask(&w, &p, &three, &["session", "profile"]).unwrap();
        let caller = proven(&w);
        // The answer is refused without a grant, and stores nothing.
        let e = answer(
            &w,
            &Caller::default(),
            &two,
            &[("session", MARKER), ("env", "stage-1")],
        )
        .unwrap_err();
        assert_eq!(e.code, crate::contract::ErrorCode::ProofRequired);
        assert_eq!(pending_ids(&w).len(), 3);
        // With one, both satisfied requests are answered; the third waits for its field.
        let outcome = answer(
            &w,
            &caller,
            &two,
            &[("session", MARKER), ("env", "stage-1")],
        )
        .unwrap();
        assert_eq!(outcome.name, "qa-user");
        assert_eq!(outcome.stored_fields, vec!["session", "env"]);
        // The answered request first, then the sibling it satisfied.
        assert_eq!(outcome.answered_requests, vec![two.clone(), one.clone()]);
        assert_eq!(pending_ids(&w), vec![three]);
        // The value is in the vault, never in a request.
        let fields = w
            .call_as::<Vec<crate::contract::accounts::AccountInfo>>(Request::AccountList {
                project: p.clone(),
            })
            .unwrap();
        assert_eq!(fields[0].fields, vec!["env", "session"]);
    }

    #[test]
    fn answer_is_atomic_and_only_pending_requests_are_answered() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let id = uuid::Uuid::new_v4().to_string();
        ask(&w, &p, &id, &["session", "env"]).unwrap();
        let caller = proven(&w);
        // A missing field, an extra field and a bad value each store nothing.
        for fields in [
            vec![("session", MARKER)],
            vec![("session", MARKER), ("env", "stage-1"), ("profile", "qa")],
            vec![("session", MARKER), ("env", "ab")],
        ] {
            assert!(answer(&w, &caller, &id, &fields).is_err(), "{fields:?}");
            assert_eq!(pending_ids(&w), vec![id.clone()]);
            let listed = w
                .call_as::<Vec<crate::contract::accounts::AccountInfo>>(Request::AccountList {
                    project: p.clone(),
                })
                .unwrap();
            assert!(listed.is_empty(), "{fields:?}");
        }
        answer(&w, &caller, &id, &[("session", MARKER), ("env", "stage-1")]).unwrap();
        assert!(pending_ids(&w).is_empty());
        // A decided request is not answered again.
        let e = answer(&w, &caller, &id, &[("session", MARKER), ("env", "stage-1")]).unwrap_err();
        assert_eq!(e.code, crate::contract::ErrorCode::BadRequest);
        assert_eq!(e.message, "the account request is not pending");
    }

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    /// A second connection to the same SQLite file, for rows the engine's own store cannot
    /// create with an old timestamp.
    fn db(w: &super::super::tests::World) -> rusqlite::Connection {
        rusqlite::Connection::open(w.dir.path().join("hostler.db")).unwrap()
    }

    /// R1-3: one hourly branch prunes both histories and advances the timestamp once; account
    /// rows are deleted by their answer time, not their creation time.
    #[test]
    fn hourly_tick_prunes_finished_requests_by_their_answer_time() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let project = w.project(&p);
        let now = w.engine.now();
        let path = w.dir.path().join("hostler.db");
        let store = Store::open(&path).unwrap();

        let old = uuid::Uuid::new_v4().to_string();
        ask(&w, &p, &old, &["session"]).unwrap();
        store
            .finish_account_request(&old, RequestState::Declined, now - 40 * DAY_MS)
            .unwrap();

        // Pending for 40 days, declined just now: kept (its answer time is now).
        let just_answered = uuid::Uuid::new_v4().to_string();
        ask(&w, &p, &just_answered, &["session"]).unwrap();
        store
            .finish_account_request(&just_answered, RequestState::Declined, now)
            .unwrap();
        db(&w)
            .execute(
                "UPDATE account_requests SET created_at_ms = ?1 WHERE id = ?2",
                rusqlite::params![now - 40 * DAY_MS, just_answered],
            )
            .unwrap();

        // A finished secret request, to show the same branch prunes both.
        let secret = crate::contract::secrets::SecretRequest {
            id: uuid::Uuid::new_v4().to_string(),
            project: p.clone(),
            name: "API_KEY".into(),
            reason: "x".into(),
            requester: requester(),
        };
        store.insert_request(&secret, &project.id, now).unwrap();
        store
            .finish_request(&secret.id, RequestState::Declined, now - 40 * DAY_MS)
            .unwrap();

        w.engine.tick();
        assert!(
            store.account_request(&old).unwrap().is_none(),
            "an old finished account request was not pruned"
        );
        assert!(
            store.account_request(&just_answered).unwrap().is_some(),
            "a long-pending request that just became terminal was pruned by its creation time"
        );
        assert!(
            store.request(&secret.id).unwrap().is_none(),
            "the same hourly branch did not prune the secret history"
        );

        // The timestamp advanced once: a second tick in the same hour prunes nothing.
        let later = uuid::Uuid::new_v4().to_string();
        ask(&w, &p, &later, &["session"]).unwrap();
        store
            .finish_account_request(&later, RequestState::Declined, now - 40 * DAY_MS)
            .unwrap();
        w.engine.tick();
        assert!(
            store.account_request(&later).unwrap().is_some(),
            "the hourly prune ran twice in one hour"
        );
    }

    #[test]
    fn a_request_is_stored_at_once_when_every_field_is_set() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.call(Request::AccountSet(crate::contract::accounts::AccountSet {
            project: p.clone(),
            name: "qa-user".into(),
            field: "session".into(),
            value: SecretValue::new(MARKER.into()),
        }))
        .unwrap();
        let info = ask(&w, &p, &uuid::Uuid::new_v4().to_string(), &["session"]).unwrap();
        assert_eq!(info.state, RequestState::Stored);
        assert_eq!(info.answered_at_ms, Some(info.created_at_ms));
        assert!(pending_ids(&w).is_empty());
        // A set answers a pending request whose fields are now all set.
        let id = uuid::Uuid::new_v4().to_string();
        ask(&w, &p, &id, &["session", "env"]).unwrap();
        w.call(Request::AccountSet(crate::contract::accounts::AccountSet {
            project: p.clone(),
            name: "qa-user".into(),
            field: "env".into(),
            value: SecretValue::new("stage-1".into()),
        }))
        .unwrap();
        assert!(pending_ids(&w).is_empty());
        let info = w
            .call_as::<AccountRequestInfo>(Request::AccountRequest(AccountRequestNew {
                id: id.clone(),
                project: p.clone(),
                name: "qa-user".into(),
                fields: vec!["session".into(), "env".into()],
                reason: "run the login test".into(),
                requester: requester(),
            }))
            .unwrap();
        assert_eq!(info.state, RequestState::Stored);
    }
}
