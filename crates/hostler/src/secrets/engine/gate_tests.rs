//! The person gate in the engine (P5 Task 4, spec §8.5): prove, the grant's admission under the
//! vault mutex, the limiter over prove and unlock, reveal and the preserved-file cleanup.
//! Passwords and values here are throwaway literals; no assertion message prints a value.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use serde_json::json;

use super::tests::{World, outside_write, world};
use super::*;
use crate::contract::accounts::{AccountAnswer, AccountSet, MaterializedAccount};
use crate::contract::secrets::{ImportEntry, SecretSet, Share};

const PASSWORD: &str = "pass-1234";
const MARKER: &str = "gate-marker-value-9c2e";

fn prove_with(w: &World, password: &str) -> Result<SecretsAnswer, HostlerError> {
    w.engine.dispatch_as(
        Request::SecretsProve(Prove {
            password: SecretValue::new(password.into()),
            key_file: None,
        }),
        &Caller::default(),
    )
}

/// A caller holding a fresh proof.
fn proven(w: &World) -> Caller {
    let answer = prove_with(w, PASSWORD).expect("prove");
    assert!(answer.grant.is_some());
    Caller {
        peer_pid: Some(4242),
        grant: answer.grant,
    }
}

fn unlock_with(w: &World, password: &str) -> Result<SecretsAnswer, HostlerError> {
    w.engine.dispatch_as(
        Request::SecretsUnlock(Unlock {
            password: SecretValue::new(password.into()),
            key_file: None,
            forget_key_file: false,
            create: false,
        }),
        &Caller::default(),
    )
}

fn set(w: &World, p: &ProjectRef, name: &str, value: &str) {
    w.call(Request::SecretSet(SecretSet {
        project: p.clone(),
        name: name.into(),
        value: SecretValue::new(value.into()),
    }))
    .expect("secret.set");
}

/// `secret.reveal`'s value (decoded from the sensitive answer), or its refusal.
fn reveal(w: &World, p: &ProjectRef, name: &str, caller: &Caller) -> Result<String, HostlerError> {
    let answer = w.engine.dispatch_as(
        Request::SecretReveal {
            project: p.clone(),
            name: name.into(),
        },
        caller,
    )?;
    match answer.reply {
        Reply::Sensitive(json) => {
            let secret: MaterializedSecret = serde_json::from_str(&json).unwrap();
            assert_eq!(secret.name, name);
            Ok(secret.value.expose().to_owned())
        }
        Reply::Value(_) => panic!("reveal must answer sensitive"),
    }
}

fn code(r: Result<impl Sized, HostlerError>) -> ErrorCode {
    match r {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e.code,
    }
}

/// An unlocked world with an approved project `app` holding `API_KEY`.
fn with_secret() -> (World, ProjectRef) {
    let w = world();
    w.unlock_new();
    let p = w.approved_repo("app");
    set(&w, &p, "API_KEY", MARKER);
    (w, p)
}

fn kdbx(w: &World) -> PathBuf {
    w.dir.path().join("hostler.kdbx")
}

#[test]
fn reveal_without_proof_is_refused() {
    let (w, p) = with_secret();
    let before = w.engine.vault_calls.load(Ordering::SeqCst);
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &Caller::default())),
        ErrorCode::ProofRequired
    );
    // An expired grant and a grant of an older generation are no grant either.
    let generation = w.engine.generation.load(Ordering::SeqCst);
    let expired = Caller {
        peer_pid: None,
        grant: Some(GrantMeta {
            expires_at_ms: w.engine.now(),
            generation_at_grant: generation,
        }),
    };
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &expired)),
        ErrorCode::ProofRequired
    );
    let stale = Caller {
        peer_pid: None,
        grant: Some(GrantMeta {
            expires_at_ms: w.engine.now() + 60_000,
            generation_at_grant: generation.wrapping_sub(1),
        }),
    };
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &stale)),
        ErrorCode::ProofRequired
    );
    assert_eq!(
        w.engine.vault_calls.load(Ordering::SeqCst),
        before,
        "the vault is not read"
    );
}

#[test]
fn proof_then_reveal() {
    let (w, p) = with_secret();
    let caller = proven(&w);
    assert!(reveal(&w, &p, "API_KEY", &caller).unwrap() == MARKER);
    // The proof changes nothing: still unlocked, same generation.
    assert_eq!(w.engine.status().unwrap().state, LockState::Unlocked);
    assert_eq!(
        caller.grant.unwrap().generation_at_grant,
        w.engine.generation.load(Ordering::SeqCst)
    );
}

#[test]
fn unlock_grants_with_the_generation_after_it() {
    let (w, p) = with_secret();
    w.call(Request::SecretsLock).unwrap();
    let answer = unlock_with(&w, PASSWORD).unwrap();
    let grant = answer.grant.expect("a grant");
    assert_eq!(
        grant.generation_at_grant,
        w.engine.vault.lock().unwrap().generation()
    );
    let caller = Caller {
        peer_pid: None,
        grant: Some(grant),
    };
    assert!(reveal(&w, &p, "API_KEY", &caller).unwrap() == MARKER);
}

#[test]
fn lock_voids_the_proof() {
    let (w, p) = with_secret();
    let caller = proven(&w);
    w.call(Request::SecretsLock).unwrap();
    unlock_with(&w, PASSWORD).unwrap();
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &caller)),
        ErrorCode::ProofRequired
    );
}

/// A lock alone voids the proof too: a gated method that works on a locked database
/// (`secrets.remove_preserved`, `heavy.stop`'s admission) is refused after it.
#[test]
fn lock_alone_voids_the_proof() {
    let (w, _p) = with_secret();
    let caller = proven(&w);
    w.engine.admit_grant(caller.grant.as_ref()).unwrap();
    w.call(Request::SecretsLock).unwrap();
    assert_eq!(
        code(w.engine.admit_grant(caller.grant.as_ref())),
        ErrorCode::ProofRequired
    );
    let prev = w
        .dir
        .path()
        .join(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
    std::fs::write(&prev, b"kept").unwrap();
    assert_eq!(
        code(w.engine.dispatch_as(
            Request::SecretsRemovePreserved {
                path: display_path(&prev)
            },
            &caller
        )),
        ErrorCode::ProofRequired
    );
    assert!(prev.exists());
}

/// A `secrets.lock` of a locked database is no transition (Lead, P5 Task 4 follow-up): a grant
/// made while locked stays live; only a real lock or unlock voids it.
#[test]
fn lock_of_a_locked_vault_keeps_the_grant() {
    let (w, _p) = with_secret();
    w.call(Request::SecretsLock).unwrap();
    let generation = w.engine.generation.load(Ordering::SeqCst);
    let caller = proven(&w);
    w.call(Request::SecretsLock).unwrap();
    assert_eq!(w.engine.generation.load(Ordering::SeqCst), generation);
    w.engine.admit_grant(caller.grant.as_ref()).unwrap();
    let prev = w
        .dir
        .path()
        .join(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
    std::fs::write(&prev, b"kept").unwrap();
    w.engine
        .dispatch_as(
            Request::SecretsRemovePreserved {
                path: display_path(&prev),
            },
            &caller,
        )
        .unwrap();
    assert!(
        !prev.exists(),
        "the grant still admits after the no-op lock"
    );
    // The shutdown lock of a locked vault is no transition either.
    w.engine.lock_for_shutdown();
    assert_eq!(w.engine.generation.load(Ordering::SeqCst), generation);
}

/// Holds the vault mutex while `request` runs on another thread: once it has passed the gate's
/// pre-check (hook `point`), the wall clock moves past the grant's expiry, then the vault is
/// released (review t4-r1 R1-1). The admission must read the clock with the vault held.
fn expire_while_waiting_for_the_vault<T: Send + 'static>(
    w: &World,
    point: &'static str,
    request: impl FnOnce(&SecretsEngine) -> T + Send + 'static,
) -> T {
    let reached = Arc::new(AtomicBool::new(false));
    let r = reached.clone();
    *w.engine.hook.lock().unwrap() = Some(Arc::new(move |at| {
        if at == point {
            r.store(true, Ordering::SeqCst);
        }
    }));
    let held = w.engine.vault.lock().unwrap();
    let engine = w.engine.clone();
    let running = std::thread::spawn(move || request(&engine));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !reached.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "{point} was never reached"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Not needed for the fixed code's outcome (the vault is held until after the advance);
    // it lets the request reach the mutex, so a clock read before it would be caught.
    std::thread::sleep(std::time::Duration::from_millis(100));
    w.clock.advance(gate::proof_ttl_ms() + 1);
    drop(held);
    running.join().unwrap()
}

#[test]
fn grant_expiring_while_a_reveal_waits_for_the_vault_is_refused() {
    let (w, p) = with_secret();
    let caller = proven(&w);
    let result = expire_while_waiting_for_the_vault(&w, "reveal_admit", move |engine| {
        engine
            .dispatch_as(
                Request::SecretReveal {
                    project: p,
                    name: "API_KEY".into(),
                },
                &caller,
            )
            .map(|answer| matches!(answer.reply, Reply::Sensitive(_)))
    });
    assert_eq!(code(result), ErrorCode::ProofRequired, "no value was read");
}

#[test]
fn grant_expiring_while_the_stop_admission_waits_for_the_vault_is_refused() {
    let (w, _p) = with_secret();
    let caller = proven(&w);
    // `heavy.stop` stops only after `admit_grant` answers `Ok`.
    let result = expire_while_waiting_for_the_vault(&w, "grant_admit", move |engine| {
        engine.admit_grant(caller.grant.as_ref())
    });
    assert_eq!(code(result), ErrorCode::ProofRequired);
}

/// A lock and an unlock between the gate's pre-check and the admission under the vault mutex:
/// the reveal is refused and no value leaves.
#[test]
fn lock_between_grant_and_reveal() {
    let (w, p) = with_secret();
    let caller = proven(&w);
    let engine: Weak<SecretsEngine> = Arc::downgrade(&w.engine);
    let armed = Arc::new(AtomicBool::new(true));
    let a = armed.clone();
    *w.engine.hook.lock().unwrap() = Some(Arc::new(move |point| {
        if point == "reveal_admit" && a.swap(false, Ordering::SeqCst) {
            let engine = engine.upgrade().unwrap();
            engine.reply(Request::SecretsLock).unwrap();
            engine
                .reply(Request::SecretsUnlock(Unlock {
                    password: SecretValue::new(PASSWORD.into()),
                    key_file: None,
                    forget_key_file: false,
                    create: false,
                }))
                .unwrap();
        }
    }));
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &caller)),
        ErrorCode::ProofRequired
    );
    assert!(!armed.load(Ordering::SeqCst), "the pause point was reached");
}

/// A lock and an unlock while a prove's check runs: its grant is of the older generation.
#[test]
fn lock_during_prove_voids_its_grant() {
    let (w, p) = with_secret();
    let engine: Weak<SecretsEngine> = Arc::downgrade(&w.engine);
    let armed = Arc::new(AtomicBool::new(true));
    let a = armed.clone();
    *w.engine.hook.lock().unwrap() = Some(Arc::new(move |point| {
        if point == "prove_checking" && a.swap(false, Ordering::SeqCst) {
            let engine = engine.upgrade().unwrap();
            engine.reply(Request::SecretsLock).unwrap();
        }
    }));
    let caller = proven(&w);
    assert!(!armed.load(Ordering::SeqCst));
    // The unlock is a check too: it runs once the prove's check is done (one at a time).
    unlock_with(&w, PASSWORD).unwrap();
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &caller)),
        ErrorCode::ProofRequired
    );
}

#[test]
fn prove_on_missing_database_refused() {
    let w = world();
    let e = prove_with(&w, PASSWORD).err().unwrap();
    assert_eq!(e.code, ErrorCode::SecretsLocked);
    assert_eq!(
        e.message,
        "no secrets database yet; create one in the hostler app or with: hostler unlock --create"
    );
    assert_eq!(
        w.engine.store().unwrap().password_limit().unwrap(),
        None,
        "no check, no count"
    );
}

#[test]
fn prove_works_while_locked() {
    let (w, p) = with_secret();
    w.call(Request::SecretsLock).unwrap();
    let caller = proven(&w);
    assert_eq!(
        w.engine.status().unwrap().state,
        LockState::Locked,
        "prove unlocks nothing"
    );
    // A gated method still needs what it needs: a reveal of a locked database is refused.
    assert_eq!(
        code(reveal(&w, &p, "API_KEY", &caller)),
        ErrorCode::SecretsLocked
    );
}

#[test]
fn wrong_proof_keeps_the_database_unlocked() {
    let (w, p) = with_secret();
    let generation = w.engine.generation.load(Ordering::SeqCst);
    let e = prove_with(&w, "wrong-pass").err().unwrap();
    assert_eq!(e.code, ErrorCode::UnlockFailed);
    assert_eq!(
        e.message,
        errors::unlock_failed().message,
        "the unlock's fixed text"
    );
    assert_eq!(w.engine.status().unwrap().state, LockState::Unlocked);
    assert_eq!(w.engine.generation.load(Ordering::SeqCst), generation);
    assert_eq!(
        w.engine.store().unwrap().password_limit().unwrap(),
        Some((1, Some(w.engine.now())))
    );
    assert!(
        reveal(&w, &p, "API_KEY", &Caller::default()).is_err(),
        "and no grant"
    );
}

#[test]
fn a_wrong_unlock_counts_for_prove() {
    let (w, _p) = with_secret();
    w.call(Request::SecretsLock).unwrap();
    assert_eq!(code(unlock_with(&w, "wrong-pass")), ErrorCode::UnlockFailed);
    let e = prove_with(&w, PASSWORD).err().unwrap();
    assert_eq!(e.code, ErrorCode::ProofLimited);
    assert!(
        e.message
            .contains("Another process of your user may be guessing")
    );
    assert!(e.details["retry_after_ms"].as_u64().unwrap() >= 1);
    // A refused check is not a failure: the count stays 1.
    assert_eq!(
        w.engine
            .store()
            .unwrap()
            .password_limit()
            .unwrap()
            .unwrap()
            .0,
        1
    );
}

#[test]
fn success_resets_the_count() {
    let (w, _p) = with_secret();
    assert_eq!(code(prove_with(&w, "wrong-pass")), ErrorCode::UnlockFailed);
    assert_eq!(
        w.engine.store().unwrap().password_limit().unwrap(),
        Some((1, Some(w.engine.now())))
    );
    // Two failures in a row, the last long ago (this world's wall clock stands still): the wait
    // has passed on both clocks once the monotonic one has (1 s after the failure above).
    w.engine
        .store()
        .unwrap()
        .set_password_limit(2, Some(w.engine.now() - 60_000))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    proven(&w);
    assert_eq!(
        w.engine.store().unwrap().password_limit().unwrap(),
        Some((0, None))
    );
}

#[test]
fn create_is_not_a_check() {
    let w = world();
    w.unlock_new();
    assert_eq!(w.engine.store().unwrap().password_limit().unwrap(), None);
}

#[test]
fn store_failure_fails_closed() {
    let (w, p) = with_secret();
    w.call(Request::SecretsLock).unwrap();
    let generation = w.engine.generation.load(Ordering::SeqCst);
    w.engine.store().unwrap().set_query_only(true);
    assert_eq!(code(unlock_with(&w, PASSWORD)), ErrorCode::Internal);
    assert_eq!(
        w.engine.status().unwrap().state,
        LockState::Locked,
        "nothing unlocked"
    );
    assert_eq!(w.engine.generation.load(Ordering::SeqCst), generation);
    assert_eq!(code(prove_with(&w, PASSWORD)), ErrorCode::Internal);
    w.engine.store().unwrap().set_query_only(false);
    let answer = unlock_with(&w, PASSWORD).unwrap();
    let caller = Caller {
        peer_pid: None,
        grant: answer.grant,
    };
    assert!(reveal(&w, &p, "API_KEY", &caller).unwrap() == MARKER);
}

#[test]
fn every_gated_method_refuses_without_grant() {
    let (w, p) = with_secret();
    let prev = w
        .dir
        .path()
        .join(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
    std::fs::write(&prev, b"kept").unwrap();
    let gated = || {
        vec![
            Request::SecretReveal {
                project: p.clone(),
                name: "API_KEY".into(),
            },
            Request::AccountReveal {
                project: p.clone(),
                name: "qa-user".into(),
            },
            Request::SecretsRemovePreserved {
                path: display_path(&prev),
            },
            Request::AccountAnswer(AccountAnswer {
                request_id: uuid::Uuid::new_v4().to_string(),
                fields: vec![ImportEntry {
                    name: "session".into(),
                    value: SecretValue::new(MARKER.into()),
                }],
            }),
            Request::AccountDecline {
                id: uuid::Uuid::new_v4().to_string(),
            },
        ]
    };
    for req in gated() {
        let method = crate::contract::methods::method_name(&req);
        let e = w.engine.dispatch_as(req, &Caller::default()).err().unwrap();
        assert_eq!(e.code, ErrorCode::ProofRequired, "{method}");
    }
    assert!(prev.exists(), "the preserved file is still there");
    // With a grant, each gets past the gate (Task 5's two still answer not_available_yet).
    let caller = proven(&w);
    for req in gated() {
        let method = crate::contract::methods::method_name(&req);
        let result = w.engine.dispatch_as(req, &caller);
        if let Err(e) = result {
            assert_ne!(e.code, ErrorCode::ProofRequired, "{method}");
        }
    }
    assert!(!prev.exists(), "removed once proven");
}

#[test]
fn reveal_follows_project_rules() {
    let (w, p) = with_secret();
    let other = w.approved_repo("other");
    set(&w, &other, "OTHER_KEY", "value-of-other");
    let caller = proven(&w);
    assert_eq!(
        code(reveal(&w, &p, "OTHER_KEY", &caller)),
        ErrorCode::SecretNotInProject
    );
    assert_eq!(
        code(reveal(&w, &p, "NOPE_KEY", &caller)),
        ErrorCode::SecretMissing
    );
    assert_eq!(
        code(reveal(&w, &p, "bad name", &caller)),
        ErrorCode::BadRequest
    );
    let unapproved = w.repo("unapproved");
    assert_eq!(
        code(reveal(&w, &unapproved, "API_KEY", &caller)),
        ErrorCode::ProjectNotApproved
    );
    // A share is followed.
    w.call(Request::SecretShare(Share {
        from: other.clone(),
        name: "OTHER_KEY".into(),
        to_project_id: w.project(&p).id,
    }))
    .unwrap();
    assert!(reveal(&w, &p, "OTHER_KEY", &caller).unwrap() == "value-of-other");
    // A value edited outside hostler that breaks the value rules (L19).
    let group = group_of(&w.project(&p)).unwrap();
    outside_write(&kdbx(&w), |db| {
        layout::put(db, group, "BAD_KEY", &SecretValue::new("x".into()));
    });
    let e = reveal(&w, &p, "BAD_KEY", &caller).err().unwrap();
    assert_eq!(e.code, ErrorCode::SecretsConflict);
    assert_eq!(
        e.details,
        json!({ "name": "BAD_KEY", "reason": "invalid_value" })
    );
}

#[test]
fn account_reveal_returns_every_field() {
    let (w, p) = with_secret();
    for (field, value) in [("session", "session-value-1"), ("note", "note-value-22")] {
        w.call(Request::AccountSet(AccountSet {
            project: p.clone(),
            name: "qa-user".into(),
            field: field.into(),
            value: SecretValue::new(value.into()),
        }))
        .unwrap();
    }
    let reveal_account = |caller: &Caller| {
        w.engine.dispatch_as(
            Request::AccountReveal {
                project: p.clone(),
                name: "qa-user".into(),
            },
            caller,
        )
    };
    assert_eq!(
        code(reveal_account(&Caller::default())),
        ErrorCode::ProofRequired
    );
    let caller = proven(&w);
    let Reply::Sensitive(json) = reveal_account(&caller).unwrap().reply else {
        panic!("account.reveal must answer sensitive");
    };
    let account: MaterializedAccount = serde_json::from_str(&json).unwrap();
    assert_eq!(account.name, "qa-user");
    let mut fields: Vec<(String, String)> = account
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.value.expose().to_owned()))
        .collect();
    fields.sort();
    assert!(
        fields
            == [
                ("note".to_owned(), "note-value-22".to_owned()),
                ("session".to_owned(), "session-value-1".to_owned())
            ]
    );
    // Missing account.
    let e = w
        .engine
        .dispatch_as(
            Request::AccountReveal {
                project: p.clone(),
                name: "nobody".into(),
            },
            &caller,
        )
        .err()
        .unwrap();
    assert_eq!(e.code, ErrorCode::AccountMissing);
}

#[test]
fn remove_preserved_only_listed() {
    let (w, _p) = with_secret();
    let dir = w.dir.path();
    let listed = dir.join(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
    std::fs::write(&listed, b"kept").unwrap();
    let elsewhere_dir = tempfile::tempdir().unwrap();
    let outside = elsewhere_dir
        .path()
        .join(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
    std::fs::write(&outside, b"outside").unwrap();
    let unlisted = dir.join("notes.txt");
    std::fs::write(&unlisted, b"notes").unwrap();
    let caller = proven(&w);
    let remove = |path: String| {
        w.engine
            .dispatch_as(Request::SecretsRemovePreserved { path }, &caller)
    };
    // Refused, nothing deleted.
    for path in [
        display_path(&outside),
        display_path(&unlisted),
        display_path(&kdbx(&w)),
        display_path(&dir.join("sub").join("..").join(listed.file_name().unwrap())),
        listed.file_name().unwrap().to_string_lossy().into_owned(),
    ] {
        assert_eq!(code(remove(path.clone())), ErrorCode::BadRequest, "{path}");
    }
    #[cfg(unix)]
    {
        let link = dir.join(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(kdbx(&w), &link).unwrap();
        assert_eq!(
            code(remove(display_path(&link))),
            ErrorCode::BadRequest,
            "a symlink"
        );
        assert!(link.symlink_metadata().is_ok());
        std::fs::remove_file(&link).unwrap();
    }
    assert!(outside.exists() && unlisted.exists() && kdbx(&w).exists() && listed.exists());
    // The listed one goes, and the new status no longer lists it.
    let status: SecretsStatus = match remove(display_path(&listed)).unwrap().reply {
        Reply::Value(v) => serde_json::from_value(v).unwrap(),
        Reply::Sensitive(_) => panic!("a status is not sensitive"),
    };
    assert!(!listed.exists());
    assert!(
        status.preserved_files.is_empty(),
        "{:?}",
        status.preserved_files
    );
    assert!(kdbx(&w).exists());
}
