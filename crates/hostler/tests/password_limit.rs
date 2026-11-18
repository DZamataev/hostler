//! The service-wide limit on master-password checks against the real binary (P5 Task 4, spec
//! §8.5): over prove and unlock, at most one check at a time across every connection, a
//! backoff after wrong passwords that survives disconnects and restarts, and a log line per
//! failure with the peer's pid. `HOSTLER_TEST_PASSWORD_BACKOFF_SCALE_MS` scales the 1 s unit.
//! The clock rules are unit tests of `secrets::gate`.
//!
//! Passwords are throwaway literals; no assertion message prints one.

mod common;
#[path = "common/raw.rs"]
mod raw;

use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use common::{Home, stderr};
use hostler::client::Client;
use hostler::contract::secrets::{MaterializedSecret, ProjectRef, Prove, SecretSet, Unlock};
use hostler::contract::wire::Request;
use hostler::contract::{ErrorCode, HostlerError};
use hostler::secrets::value::SecretValue;
use raw::files_text;
use serde_json::Value;

const PASSWORD: &str = "pass-1234";
const WRONG: &str = "wrong-pass-5a1c";
const MARKER: &str = "limit-marker-31f0";

struct Conn {
    rt: tokio::runtime::Runtime,
    client: Client,
}

impl Conn {
    fn open(home: &Home) -> Conn {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let paths = home.paths();
        let client = rt.block_on(Client::connect(&paths)).unwrap();
        Conn { rt, client }
    }

    fn call(&mut self, req: Request) -> Result<Value, HostlerError> {
        self.rt.block_on(self.client.call(req))
    }

    fn prove(&mut self, password: &str) -> Result<Value, HostlerError> {
        self.call(Request::SecretsProve(Prove {
            password: SecretValue::new(password.into()),
            key_file: None,
        }))
    }

    fn unlock(&mut self, password: &str) -> Result<Value, HostlerError> {
        self.call(Request::SecretsUnlock(Unlock {
            password: SecretValue::new(password.into()),
            key_file: None,
            forget_key_file: false,
            create: false,
        }))
    }

    fn reveal(&mut self, p: &ProjectRef) -> Result<String, HostlerError> {
        let secret: MaterializedSecret =
            self.rt
                .block_on(self.client.call_sensitive(Request::SecretReveal {
                    project: p.clone(),
                    name: "API_KEY".into(),
                }))?;
        Ok(secret.value.expose().to_owned())
    }
}

fn start(home: &Home, scale_ms: u64) {
    let out = home
        .hostler()
        .env(
            "HOSTLER_TEST_PASSWORD_BACKOFF_SCALE_MS",
            scale_ms.to_string(),
        )
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
}

fn stop(home: &Home) {
    let out = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service stop: {}", stderr(&out));
}

/// A service with the scaled unit, a new database, an approved `app` and its `API_KEY`.
fn ready(scale_ms: u64) -> (Home, ProjectRef) {
    let home = Home::new();
    start(&home, scale_ms);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let repo = home.make_repo("app");
    let p = hostler::project::current(&repo).unwrap().project;
    home.call(Request::ProjectApprove(p.clone())).unwrap();
    home.call(Request::SecretSet(SecretSet {
        project: p.clone(),
        name: "API_KEY".into(),
        value: SecretValue::new(MARKER.into()),
    }))
    .unwrap();
    (home, p)
}

fn limited(r: Result<Value, HostlerError>) -> u64 {
    let e = r.expect_err("refused");
    assert_eq!(e.code, ErrorCode::ProofLimited, "{}", e.message);
    assert!(
        e.message
            .contains("Another process of your user may be guessing"),
        "{}",
        e.message
    );
    e.details["retry_after_ms"].as_u64().unwrap()
}

fn code(r: Result<impl Sized, HostlerError>) -> ErrorCode {
    match r {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e.code,
    }
}

/// Sends wrong proves every 20 ms until one is checked again (`unlock_failed`): the time from
/// sending the failed check before it to that answer is at least `wait` (the service records
/// the failure in between), and at most `wait` plus 2 s of slack. Every refusal before it
/// carries `retry_after_ms` within `wait`.
fn next_check_after(conn: &mut Conn, sent: Instant, wait: u64) -> Instant {
    loop {
        let attempt = Instant::now();
        match conn.prove(WRONG) {
            Err(e) if e.code == ErrorCode::ProofLimited => {
                let retry = e.details["retry_after_ms"].as_u64().unwrap();
                assert!((1..=wait).contains(&retry), "retry {retry} within {wait}");
            }
            Err(e) if e.code == ErrorCode::UnlockFailed => {
                let took = sent.elapsed();
                assert!(
                    took >= Duration::from_millis(wait),
                    "checked {took:?} after the last failure, under {wait} ms"
                );
                assert!(took < Duration::from_millis(wait + 2_000), "{took:?}");
                return attempt;
            }
            other => panic!(
                "unexpected answer: {:?}",
                other.map(drop).map_err(|e| e.code)
            ),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn backoff_schedule() {
    let (home, _p) = ready(200);
    let mut conn = Conn::open(&home);
    let first = Instant::now();
    assert_eq!(code(conn.prove(WRONG)), ErrorCode::UnlockFailed);
    let retry = limited(conn.prove(WRONG));
    assert!(retry <= 200, "{retry}");
    let second = next_check_after(&mut conn, first, 200);
    let third = next_check_after(&mut conn, second, 400);
    next_check_after(&mut conn, third, 800);
}

#[test]
fn success_resets() {
    let (home, _p) = ready(200);
    let mut conn = Conn::open(&home);
    assert_eq!(code(conn.prove(WRONG)), ErrorCode::UnlockFailed);
    limited(conn.prove(PASSWORD));
    std::thread::sleep(Duration::from_millis(400));
    conn.prove(PASSWORD).unwrap();
    // The count is 0 again: a wrong password is checked at once, then waits one unit.
    assert_eq!(code(conn.prove(WRONG)), ErrorCode::UnlockFailed);
    let retry = limited(conn.prove(PASSWORD));
    assert!(retry <= 200, "one failure, one unit: {retry}");
}

#[test]
fn unlock_counts_too() {
    let (home, _p) = ready(200);
    let mut conn = Conn::open(&home);
    conn.call(Request::SecretsLock).unwrap();
    assert_eq!(code(conn.unlock(WRONG)), ErrorCode::UnlockFailed);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(code(conn.unlock(WRONG)), ErrorCode::UnlockFailed);
    // A prove inside the backoff of two failures (two units).
    let retry = limited(conn.prove(PASSWORD));
    assert!((1..=400).contains(&retry), "{retry}");
    // And an unlock is refused the same way.
    limited(conn.unlock(PASSWORD));
}

/// 20 connections send a wrong `secrets.prove` at once: exactly one is checked (one failure
/// counted and logged), the other 19 get `proof_limited`.
#[test]
fn parallel_connections_get_one_check() {
    // A 10 s unit: no second check can fit while the 20 run.
    let (home, _p) = ready(10_000);
    let conns: Vec<Conn> = (0..20).map(|_| Conn::open(&home)).collect();
    let barrier = Arc::new(Barrier::new(conns.len()));
    let answers: Vec<Result<Value, HostlerError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = conns
            .into_iter()
            .map(|mut conn| {
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    conn.prove(WRONG)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut failed = 0;
    let mut refused = 0;
    for answer in answers {
        match answer {
            Err(e) if e.code == ErrorCode::UnlockFailed => failed += 1,
            Err(e) if e.code == ErrorCode::ProofLimited => {
                assert!(e.details["retry_after_ms"].as_u64().unwrap() >= 1);
                refused += 1;
            }
            other => panic!(
                "unexpected answer: {:?}",
                other.map(drop).map_err(|e| e.code)
            ),
        }
    }
    assert_eq!((failed, refused), (1, 19));
    stop(&home);
    let log = files_text(&home.paths().log_dir);
    let failures = log
        .lines()
        .filter(|l| l.contains("wrong master password (prove)"))
        .count();
    assert_eq!(failures, 1, "one check was evaluated");
}

/// Three failures, every connection closed, the service restarted: a check inside the window
/// is still refused (the count and the time are in the store).
#[test]
fn limit_survives_disconnect_and_restart() {
    // Waits of 2 s and 4 s between the failures, then an 8 s window for the restart.
    let (home, _p) = ready(2_000);
    let fail = |home: &Home| {
        let mut conn = Conn::open(home);
        assert_eq!(code(conn.prove(WRONG)), ErrorCode::UnlockFailed);
    };
    fail(&home);
    std::thread::sleep(Duration::from_millis(2_200));
    fail(&home);
    std::thread::sleep(Duration::from_millis(4_200));
    fail(&home);
    stop(&home);
    start(&home, 2_000);
    let mut conn = Conn::open(&home);
    let retry = limited(conn.prove(PASSWORD));
    assert!((1..=8_000).contains(&retry), "{retry}");
}

/// A grant from a prove is cleared by a wrong unlock on the same connection; a wrong prove on a
/// fresh connection installs nothing (R2-3).
#[test]
fn failed_unlock_clears_and_never_installs() {
    let (home, p) = ready(200);
    let mut a = Conn::open(&home);
    a.prove(PASSWORD).unwrap();
    assert!(a.reveal(&p).unwrap() == MARKER);
    assert_eq!(code(a.unlock(WRONG)), ErrorCode::UnlockFailed);
    assert_eq!(code(a.reveal(&p)), ErrorCode::ProofRequired);
    std::thread::sleep(Duration::from_millis(400));
    let mut b = Conn::open(&home);
    assert_eq!(code(b.prove(WRONG)), ErrorCode::UnlockFailed);
    assert_eq!(code(b.reveal(&p)), ErrorCode::ProofRequired);
}

/// Every failure is logged with the method and the peer's pid (macOS `LOCAL_PEERPID`, Linux
/// `SO_PEERCRED`), never the password.
#[test]
fn failure_is_logged_with_peer_pid() {
    let (home, _p) = ready(200);
    let mut conn = Conn::open(&home);
    assert_eq!(code(conn.prove(WRONG)), ErrorCode::UnlockFailed);
    std::thread::sleep(Duration::from_millis(400));
    conn.call(Request::SecretsLock).unwrap();
    assert_eq!(code(conn.unlock(WRONG)), ErrorCode::UnlockFailed);
    drop(conn);
    stop(&home);
    let log = files_text(&home.paths().log_dir);
    let pid = if cfg!(any(target_os = "macos", target_os = "linux")) {
        std::process::id().to_string()
    } else {
        "unknown".to_owned()
    };
    for (method, next) in [("prove", 1), ("unlock", 1)] {
        let line =
            format!("wrong master password ({method}) from peer pid {pid}; next try in {next} s");
        assert!(log.lines().any(|l| l.contains(&line)), "no line: {line}");
    }
    assert!(
        !log.contains(WRONG) && !log.contains(PASSWORD),
        "a password reached the log"
    );
}

/// While one check runs (held mid-check with `HOSTLER_TEST_PAUSE_AT=prove_checking:<file>`),
/// a prove or an unlock on another connection is refused at once as "another check is
/// running"; the held check then ends normally.
#[test]
fn a_second_check_is_refused_while_one_runs() {
    let dir = tempfile::tempdir().unwrap();
    let pause = dir.path().join("pause");
    std::fs::write(&pause, b"").unwrap();
    let home = Home::new();
    let out = home
        .hostler()
        .env(
            "HOSTLER_TEST_PAUSE_AT",
            format!("prove_checking:{}", pause.display()),
        )
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let mut a = Conn::open(&home);
    let held = std::thread::spawn(move || a.prove(PASSWORD).map(drop).map_err(|e| e.code));
    let reached = dir.path().join("pause.reached");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !reached.exists() {
        assert!(
            Instant::now() < deadline,
            "the pause point was never reached"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut b = Conn::open(&home);
    for answer in [b.prove(PASSWORD), b.unlock(PASSWORD)] {
        let e = answer.expect_err("refused");
        assert_eq!(e.code, ErrorCode::ProofLimited);
        assert!(
            e.message
                .starts_with("another master-password check is running; next try in"),
            "{}",
            e.message
        );
        assert!(e.details["retry_after_ms"].as_u64().unwrap() >= 1000);
    }
    std::fs::remove_file(&pause).unwrap();
    assert_eq!(held.join().unwrap(), Ok(()));
    // The held check succeeded: the count is 0, so the next check runs at once.
    b.prove(PASSWORD).unwrap();
}
