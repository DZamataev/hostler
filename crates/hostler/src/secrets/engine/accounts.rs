//! Accounts in the vault: an `impl SecretsEngine` block (plan P3a, Task 9).
//!
//! Field values follow P2's value rules and cross the socket only in `account.set` and
//! `account.materialize`. Errors name an account and its fields, never a value. The claims checks
//! (`holder_of`, `check_holder`) are the server's, made before these run, with the claims lock
//! released.

use zeroize::Zeroizing;

use super::{
    Caller, HISTORY_COMMAND_BYTES, MAX_COMMAND_BYTES, SecretsEngine, answer_bound, bad_request,
    duplicate_entries, encode_answer, group_of, internal, invalid_value, keep_head, store_error,
};
use crate::contract::HostlerError;
use crate::contract::accounts::{
    AccountFieldSet, AccountInfo, AccountMaterialize, AccountRemove, AccountSet,
    INVALID_ACCOUNT_NAME, INVALID_FIELD_NAME, MaterializedAccount, valid_account_name,
    valid_field_name,
};
use crate::contract::claims::errors as claims_errors;
use crate::contract::secrets::{MaterializedSecret, ProjectInfo, ProjectRef, Removed};
use crate::mask::MaskSet;
use crate::secrets::check_value;
use crate::secrets::layout::{self, AccountLookup};
use crate::secrets::value::SecretValue;

/// T1-5: one entry's fields become `HOSTLER_ACCOUNT_<FIELD>` variables of a run.
pub(crate) const MAX_ACCOUNT_FIELDS: usize = 16;

fn check_account_name(name: &str) -> Result<(), HostlerError> {
    if valid_account_name(name) {
        Ok(())
    } else {
        Err(bad_request(INVALID_ACCOUNT_NAME))
    }
}

fn check_field_name(field: &str) -> Result<(), HostlerError> {
    if valid_field_name(field) {
        Ok(())
    } else {
        Err(bad_request(INVALID_FIELD_NAME))
    }
}

/// L19 for a field edited outside hostler: names the field, never the value.
fn invalid_field(name: &str, field: &str) -> HostlerError {
    let mut e = invalid_value(&format!("{name}.{field}"));
    e.recovery = Some(format!("hostler account set {name} --field {field}"));
    e
}

impl SecretsEngine {
    /// The account's fields with their values, each checked against the value rules.
    fn read_account(
        &self,
        project: &ProjectInfo,
        name: &str,
    ) -> Result<Vec<(String, SecretValue)>, HostlerError> {
        self.read_account_as(project, name, None)
    }

    /// `read_account`; with a caller, the read is admitted with its grant in the same vault
    /// section (spec §8.5).
    fn read_account_as(
        &self,
        project: &ProjectInfo,
        name: &str,
        gated: Option<&Caller>,
    ) -> Result<Vec<(String, SecretValue)>, HostlerError> {
        let missing = || claims_errors::account_missing(name, &project.name);
        let group = group_of(project).ok_or_else(missing)?;
        let read =
            |v: &mut crate::secrets::vault::Vault| v.read(|db| layout::account(db, group, name));
        let found = match gated {
            Some(caller) => self.admitted_vault(caller.grant.as_ref(), read)?,
            None => self.with_vault(read).map_err(|e| e.to_hostler())?,
        };
        let fields = match found {
            AccountLookup::Found(fields) if !fields.is_empty() => fields,
            AccountLookup::Found(_) | AccountLookup::Missing => return Err(missing()),
            AccountLookup::Duplicate => return Err(duplicate_entries(name)),
        };
        if let Some((field, _)) = fields.iter().find(|(_, v)| check_value(v).is_err()) {
            return Err(invalid_field(name, field));
        }
        Ok(fields)
    }

    pub(super) fn account_list(
        &self,
        project: &ProjectRef,
    ) -> Result<Vec<AccountInfo>, HostlerError> {
        let project = self.approved(project)?;
        self.require_unlocked()?;
        let Some(group) = group_of(&project) else {
            return Ok(Vec::new());
        };
        self.with_vault(|v| {
            v.read(|db| {
                layout::account_names(db, group)
                    .into_iter()
                    .map(|name| AccountInfo {
                        fields: layout::account_fields(db, group, &name).unwrap_or_default(),
                        name,
                        holder: None,
                    })
                    .collect()
            })
        })
        .map_err(|e| e.to_hostler())
    }

    pub(super) fn account_check(
        &self,
        project: &ProjectRef,
        name: &str,
    ) -> Result<AccountInfo, HostlerError> {
        check_account_name(name)?;
        let project = self.approved(project)?;
        self.require_unlocked()?;
        let fields = self.read_account(&project, name)?;
        Ok(AccountInfo {
            name: name.to_owned(),
            fields: fields.into_iter().map(|(field, _)| field).collect(),
            holder: None,
        })
    }

    /// The first field creates the account; at most `MAX_ACCOUNT_FIELDS` fields.
    pub(super) fn account_set(&self, a: AccountSet) -> Result<AccountFieldSet, HostlerError> {
        check_account_name(&a.name)?;
        check_field_name(&a.field)?;
        check_value(&a.value)?;
        let project = self.approved(&a.project)?;
        self.require_unlocked()?;
        let _publishing = self.publishing();
        let project = self.current(&project)?;
        self.write_group(&project, |db, group| {
            match layout::account(db, group, &a.name) {
                AccountLookup::Duplicate => return Err(duplicate_entries(&a.name)),
                AccountLookup::Found(fields)
                    if fields.len() >= MAX_ACCOUNT_FIELDS
                        && !fields.iter().any(|(f, _)| *f == a.field) =>
                {
                    return Err(bad_request(format!(
                        "an account has at most {MAX_ACCOUNT_FIELDS} fields"
                    )));
                }
                _ => {}
            }
            layout::put_account_field(db, group, &a.name, &a.field, &a.value);
            Ok(())
        })?;
        // A set answers every pending request whose fields are now all set (spec §8.4); the
        // `publish` guard above keeps that a single step with the write.
        self.answer_account_requests_for(&project, &a.name)?;
        Ok(AccountFieldSet {
            name: a.name,
            field: a.field,
        })
    }

    /// The server refuses first while the account is claimed (`ResourceInUse`).
    pub(super) fn account_remove(&self, r: &AccountRemove) -> Result<Removed, HostlerError> {
        check_account_name(&r.name)?;
        if let Some(field) = &r.field {
            check_field_name(field)?;
        }
        let project = self.approved(&r.project)?;
        self.require_unlocked()?;
        let _publishing = self.publishing();
        let Some(group) = group_of(&self.current(&project)?) else {
            return Ok(Removed { removed: false });
        };
        let field = r.field.as_deref();
        let present = self
            .with_vault(|v| {
                v.read(|db| {
                    layout::account_fields(db, group, &r.name)
                        .is_some_and(|fields| field.is_none_or(|f| fields.iter().any(|x| x == f)))
                })
            })
            .map_err(|e| e.to_hostler())?;
        if !present {
            return Ok(Removed { removed: false });
        }
        let removed = self
            .with_vault(|v| v.write(|db| Ok(layout::remove_account(db, group, &r.name, field))))
            .map_err(|e| e.to_hostler())?;
        Ok(Removed { removed })
    }

    /// `account.reveal` (gated, spec §8.5): every field of the account for the person, read
    /// under the vault mutex that admitted the grant. No claim is needed: nothing runs with it.
    /// The log names the account, never a value.
    pub(super) fn reveal_account(
        &self,
        project: &ProjectRef,
        name: &str,
        caller: &Caller,
    ) -> Result<Zeroizing<String>, HostlerError> {
        check_account_name(name)?;
        let project = self.approved(project)?;
        self.require_unlocked()?;
        self.pause_at("reveal_admit");
        let fields = self.read_account_as(&project, name, Some(caller))?;
        let answer = MaterializedAccount {
            name: name.to_owned(),
            fields: fields
                .into_iter()
                .map(|(name, value)| MaterializedSecret { name, value })
                .collect(),
        };
        let mut encoded = encode_answer(
            &answer,
            answer_bound(&answer.fields) + answer.name.len() + 32,
        )?;
        let text = String::from_utf8(std::mem::take(&mut *encoded))
            .map_err(|_| internal("cannot encode the revealed account"))?;
        tracing::info!(
            "revealed account {name} of project {} (peer pid {})",
            project.name,
            caller.pid_text()
        );
        Ok(Zeroizing::new(text))
    }

    /// A run's fields, after the server's `check_holder`. The recorded command (already masked
    /// with the run's secret values by the CLI) is masked with the field values first and cut
    /// afterwards; nothing is recorded when the run is refused.
    pub(super) fn account_materialize(
        &self,
        m: AccountMaterialize,
    ) -> Result<Zeroizing<String>, HostlerError> {
        check_account_name(&m.name)?;
        if m.command.len() > MAX_COMMAND_BYTES {
            return Err(bad_request(
                "the command is over 16 KiB; send `<command of N bytes>` instead",
            ));
        }
        if m.run_id.is_empty() {
            return Err(bad_request("a run needs a run id"));
        }
        let project = self.approved(&m.project)?;
        self.require_unlocked()?;
        let fields = self.read_account(&project, &m.name)?;

        let answer = MaterializedAccount {
            name: m.name.clone(),
            fields: fields
                .into_iter()
                .map(|(name, value)| MaterializedSecret { name, value })
                .collect(),
        };
        let mut encoded = encode_answer(
            &answer,
            answer_bound(&answer.fields) + answer.name.len() + 32,
        )?;
        let mask = MaskSet::new(
            answer
                .fields
                .iter()
                .map(|f| f.value.expose().as_bytes().to_vec()),
        );
        let masked = mask.mask_all(m.command.expose().as_bytes());
        let recorded = keep_head(&String::from_utf8_lossy(&masked), HISTORY_COMMAND_BYTES);
        let names: Vec<String> = answer
            .fields
            .iter()
            .map(|f| format!("account:{}/{}", m.name, f.name))
            .collect();
        // Its own row: a run with `--secret` records the same run id (the history's key).
        self.store()?
            .record_materialization(
                &format!("{}/account", m.run_id),
                &project.id,
                &names,
                &m.workspace,
                &recorded,
                self.now(),
            )
            .map_err(store_error)?;
        let text = String::from_utf8(std::mem::take(&mut *encoded))
            .map_err(|_| internal("cannot encode the account for the run"))?;
        Ok(Zeroizing::new(text))
    }
}

#[cfg(test)]
mod tests {
    use keepass::db::fields;
    use serde_json::json;

    use super::super::Reply;
    use super::super::tests::{World, outside_write, world};
    use super::*;
    use crate::contract::ErrorCode;
    use crate::contract::wire::Request;
    use crate::procs::ProcessIdentity;
    use crate::secrets::value::RawCommand;

    const CANARY: &str = "acct-canary-5e1b";

    fn set(
        w: &World,
        p: &ProjectRef,
        name: &str,
        field: &str,
        value: &str,
    ) -> Result<(), HostlerError> {
        w.call(Request::AccountSet(AccountSet {
            project: p.clone(),
            name: name.into(),
            field: field.into(),
            value: SecretValue::new(value.into()),
        }))
        .map(|_| ())
    }

    fn materialize(
        w: &World,
        p: &ProjectRef,
        name: &str,
        command: &str,
    ) -> Result<MaterializedAccount, HostlerError> {
        materialize_run(w, p, name, command, &uuid::Uuid::new_v4().to_string())
    }

    fn materialize_run(
        w: &World,
        p: &ProjectRef,
        name: &str,
        command: &str,
        run_id: &str,
    ) -> Result<MaterializedAccount, HostlerError> {
        let reply = w
            .engine
            .reply(Request::AccountMaterialize(AccountMaterialize {
                run_id: run_id.into(),
                claim_id: "c1".into(),
                requester: ProcessIdentity {
                    pid: 42,
                    boot_id: "b1".into(),
                    start_time: 420,
                },
                project: p.clone(),
                name: name.into(),
                command: RawCommand::new(command.into()),
                workspace: "/work/app".into(),
            }))?;
        match reply {
            Reply::Sensitive(json) => Ok(serde_json::from_str(&json).unwrap()),
            Reply::Value(_) => panic!("account.materialize must answer sensitive"),
        }
    }

    fn check(w: &World, p: &ProjectRef, name: &str) -> Result<AccountInfo, HostlerError> {
        w.call_as(Request::AccountCheck {
            project: p.clone(),
            name: name.into(),
        })
    }

    fn unlocked_project(w: &World) -> ProjectRef {
        w.unlock_new();
        w.approved_repo("app")
    }

    #[test]
    fn list_never_contains_values() {
        let w = world();
        let p = unlocked_project(&w);
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        set(&w, &p, "qa-user", "env", "stage").unwrap();
        set(&w, &p, "admin", "session", CANARY).unwrap();
        let list = w.call(Request::AccountList { project: p.clone() }).unwrap();
        let infos: Vec<AccountInfo> = serde_json::from_value(list.clone()).unwrap();
        assert_eq!(
            infos,
            vec![
                AccountInfo {
                    name: "admin".into(),
                    fields: vec!["session".into()],
                    holder: None,
                },
                AccountInfo {
                    name: "qa-user".into(),
                    fields: vec!["env".into(), "session".into()],
                    holder: None,
                },
            ]
        );
        let info = check(&w, &p, "qa-user").unwrap();
        for printed in [list.to_string(), format!("{info:?}"), format!("{infos:?}")] {
            assert!(
                !printed.contains(CANARY) && !printed.contains("stage"),
                "{printed}"
            );
        }
    }

    #[test]
    fn materialize_masks_and_records_fields() {
        let w = world();
        let p = unlocked_project(&w);
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        set(&w, &p, "qa-user", "env", "stage-1").unwrap();
        let got = materialize(
            &w,
            &p,
            "qa-user",
            &format!("login --as {CANARY} --env stage-1"),
        )
        .unwrap();
        assert_eq!(got.name, "qa-user");
        let fields: Vec<(&str, &str)> = got
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.value.expose()))
            .collect();
        assert_eq!(fields, vec![("env", "stage-1"), ("session", CANARY)]);
        let history = w.history();
        assert_eq!(history.len(), 1);
        let (names, workspace, command) = &history[0];
        assert_eq!(
            names,
            r#"["account:qa-user/env","account:qa-user/session"]"#
        );
        assert_eq!(workspace, "/work/app");
        assert_eq!(command, "login --as *** --env ***");
    }

    #[test]
    fn invalid_field_value_is_conflict_without_value() {
        let w = world();
        let p = unlocked_project(&w);
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        let group = group_of(&w.project(&p)).unwrap();
        outside_write(&w.dir.path().join("hostler.kdbx"), |db| {
            layout::put_account_field(db, group, "qa-user", "pin", &SecretValue::new("ab".into()));
        });
        for e in [
            materialize(&w, &p, "qa-user", "make").unwrap_err(),
            check(&w, &p, "qa-user").unwrap_err(),
        ] {
            assert_eq!(e.code, ErrorCode::SecretsConflict);
            assert_eq!(
                e.details,
                json!({ "name": "qa-user.pin", "reason": "invalid_value" })
            );
            assert_eq!(
                e.recovery.as_deref(),
                Some("hostler account set qa-user --field pin")
            );
            let printed = format!("{e:?}{}", serde_json::to_string(&e).unwrap());
            assert!(
                !printed.contains(CANARY) && !printed.contains("\"ab\""),
                "{printed}"
            );
        }
        assert!(w.history().is_empty());
    }

    #[test]
    fn duplicate_tagged_entries_are_conflict() {
        let w = world();
        let p = unlocked_project(&w);
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        let group = group_of(&w.project(&p)).unwrap();
        outside_write(&w.dir.path().join("hostler.kdbx"), |db| {
            let mut g = db.group_mut(group).unwrap();
            let mut e = g.add_entry();
            e.set_unprotected(fields::TITLE, "qa-user");
            e.tags.push(layout::ACCOUNT_TAG.into());
            e.set_protected("session", "other-value");
        });
        let e = check(&w, &p, "qa-user").unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretsConflict);
        assert_eq!(e.details["reason"], "duplicate_entries");
        let e = set(&w, &p, "qa-user", "env", "stage").unwrap_err();
        assert_eq!(e.details["reason"], "duplicate_entries");
        assert!(materialize(&w, &p, "qa-user", "make").is_err());
    }

    #[test]
    fn locked_and_unapproved_are_refused() {
        let w = world();
        w.unlock_new();
        let unapproved = w.repo("other");
        let e = set(&w, &unapproved, "qa-user", "session", CANARY).unwrap_err();
        assert_eq!(e.code, ErrorCode::ProjectNotApproved);
        let p = w.approved_repo("app");
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        w.call(Request::SecretsLock).unwrap();
        let locked = [
            set(&w, &p, "qa-user", "session", CANARY).unwrap_err(),
            check(&w, &p, "qa-user").unwrap_err(),
            materialize(&w, &p, "qa-user", "make").unwrap_err(),
            w.call(Request::AccountList { project: p.clone() })
                .unwrap_err(),
            w.call(Request::AccountRemove(AccountRemove {
                project: p.clone(),
                name: "qa-user".into(),
                field: None,
            }))
            .unwrap_err(),
        ];
        for e in locked {
            assert_eq!(e.code, ErrorCode::SecretsLocked, "{e:?}");
        }
        assert_eq!(
            check(&w, &unapproved, "qa-user").unwrap_err().code,
            ErrorCode::ProjectNotApproved
        );
    }

    #[test]
    fn check_reports_missing() {
        let w = world();
        let p = unlocked_project(&w);
        // No group yet, then a group without the account.
        for _ in 0..2 {
            let e = check(&w, &p, "qa-user").unwrap_err();
            assert_eq!(e.code, ErrorCode::AccountMissing);
            assert_eq!(
                e.recovery.as_deref(),
                Some("hostler account set qa-user --field <field>")
            );
            assert_eq!(
                materialize(&w, &p, "qa-user", "make").unwrap_err().code,
                ErrorCode::AccountMissing
            );
            set(&w, &p, "admin", "session", CANARY).unwrap();
        }
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        assert_eq!(
            check(&w, &p, "qa-user").unwrap().fields,
            vec!["session".to_string()]
        );
    }

    #[test]
    fn names_values_and_field_count_are_checked() {
        let w = world();
        let p = unlocked_project(&w);
        for (name, field, message) in [
            ("QA", "session", INVALID_ACCOUNT_NAME),
            ("pasted value-1", "session", INVALID_ACCOUNT_NAME),
            ("qa-user", "Session", INVALID_FIELD_NAME),
            ("qa-user", CANARY, INVALID_FIELD_NAME),
        ] {
            let e = set(&w, &p, name, field, "value-1").unwrap_err();
            assert_eq!(
                (e.code, e.message.as_str()),
                (ErrorCode::BadRequest, message)
            );
        }
        let e = set(&w, &p, "qa-user", "session", "ab").unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretTooShort);
        for i in 0..16 {
            set(&w, &p, "qa-user", &format!("f{i}"), "value-1").unwrap();
        }
        let e = set(&w, &p, "qa-user", "f16", "value-1").unwrap_err();
        assert_eq!(e.message, "an account has at most 16 fields");
        // An existing field may still change.
        set(&w, &p, "qa-user", "f3", "value-2").unwrap();
    }

    #[test]
    fn remove_field_then_account() {
        let w = world();
        let p = unlocked_project(&w);
        let remove = |field: Option<&str>| -> bool {
            let v = w
                .call(Request::AccountRemove(AccountRemove {
                    project: p.clone(),
                    name: "qa-user".into(),
                    field: field.map(str::to_string),
                }))
                .unwrap();
            v["removed"].as_bool().unwrap()
        };
        assert!(!remove(None), "no group yet");
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        set(&w, &p, "qa-user", "env", "stage").unwrap();
        assert!(!remove(Some("pin")));
        assert!(remove(Some("env")));
        assert_eq!(
            check(&w, &p, "qa-user").unwrap().fields,
            vec!["session".to_string()]
        );
        assert!(remove(None));
        assert_eq!(
            check(&w, &p, "qa-user").unwrap_err().code,
            ErrorCode::AccountMissing
        );
        assert!(!remove(None));
    }

    /// A run with `--secret` and `--account` records both under one run id.
    #[test]
    fn secret_and_account_of_one_run_are_both_recorded() {
        let w = world();
        let p = unlocked_project(&w);
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        w.call(Request::SecretSet(crate::contract::secrets::SecretSet {
            project: p.clone(),
            name: "API_KEY".into(),
            value: SecretValue::new("value-1".into()),
        }))
        .unwrap();
        let run_id = uuid::Uuid::new_v4().to_string();
        w.engine
            .reply(Request::SecretMaterialize(
                crate::contract::secrets::Materialize {
                    run_id: run_id.clone(),
                    project: p.clone(),
                    names: vec!["API_KEY".into()],
                    command: RawCommand::new("make".into()),
                    workspace: "/work/app".into(),
                },
            ))
            .unwrap();
        materialize_run(&w, &p, "qa-user", "make", &run_id).unwrap();
        let names: Vec<String> = w.history().into_iter().map(|(names, _, _)| names).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&r#"["account:qa-user/session"]"#.to_string()));
    }

    /// L26 through the engine: the person's password and notes stay; the account is gone.
    #[test]
    fn remove_keeps_the_persons_entry_data() {
        for field in [Some("session".to_string()), None] {
            let w = world();
            let p = unlocked_project(&w);
            set(&w, &p, "qa-user", "session", CANARY).unwrap();
            let group = group_of(&w.project(&p)).unwrap();
            let kdbx = w.dir.path().join("hostler.kdbx");
            outside_write(&kdbx, |db| {
                let id = db.group(group).unwrap().entries().next().unwrap().id();
                let mut e = db.entry_mut(id).unwrap();
                e.set_protected(fields::PASSWORD, "login-pw-1");
                e.set_unprotected(fields::NOTES, "my notes");
            });
            let removed = w
                .call(Request::AccountRemove(AccountRemove {
                    project: p.clone(),
                    name: "qa-user".into(),
                    field: field.clone(),
                }))
                .unwrap();
            assert_eq!(removed["removed"], true);
            assert_eq!(
                check(&w, &p, "qa-user").unwrap_err().code,
                ErrorCode::AccountMissing
            );
            assert_eq!(
                materialize(&w, &p, "qa-user", "make").unwrap_err().code,
                ErrorCode::AccountMissing
            );
            let listed: Vec<AccountInfo> = w
                .call_as(Request::AccountList { project: p.clone() })
                .unwrap();
            assert!(listed.is_empty());
            let db = keepass::Database::parse(
                &std::fs::read(&kdbx).unwrap(),
                keepass::DatabaseKey::new().with_password("pass-1234"),
            )
            .unwrap();
            let e = db
                .group(group)
                .unwrap()
                .entries()
                .next()
                .map(|e| e.id())
                .unwrap();
            let e = db.entry(e).unwrap();
            assert_eq!(e.get(fields::PASSWORD), Some("login-pw-1"));
            assert_eq!(e.get(fields::NOTES), Some("my notes"));
            assert!(e.get("session").is_none());
        }
    }

    /// T9-2: a run that re-sends its materialize after a reconnect gets its fields again.
    #[test]
    fn retried_materialize_of_one_run_is_answered() {
        let w = world();
        let p = unlocked_project(&w);
        set(&w, &p, "qa-user", "session", CANARY).unwrap();
        let run_id = uuid::Uuid::new_v4().to_string();
        for _ in 0..2 {
            let got = materialize_run(&w, &p, "qa-user", "make", &run_id).unwrap();
            assert_eq!(got.fields.len(), 1);
        }
        assert_eq!(w.history().len(), 1);
    }
}
