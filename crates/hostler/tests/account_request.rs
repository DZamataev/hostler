//! P5 Task 5: account requests against the real binary and service (spec §8.4): an agent asks
//! for an account's fields by name, a person answers in the app or with `account set`, and a
//! decline is a person command behind the master-password proof. Passwords and values here are
//! throwaway literals; no assertion message prints a value.

mod common;
#[path = "common/raw.rs"]
mod raw;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::client::Client;
use hostler::contract::accounts::{
    AccountAnswer, AccountAnswerOutcome, AccountRequestInfo, AccountRequestNew,
};
use hostler::contract::secrets::{ImportEntry, ProjectRef, Prove, RequestState};
use hostler::contract::wire::{Event, Request};
use hostler::contract::{ErrorCode, HostlerError};
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use hostler::secrets::value::SecretValue;
use raw::Raw;
use serde_json::{Value, json};

const PASSWORD: &str = "master-pass-1";
const MARKER: &str = "hlx-account-answer-7b31";

/// An unlocked database and an approved project `app`.
fn ready() -> (Home, PathBuf) {
    ready_with(&[])
}

/// `ready`, with the service started under `env` (debug hooks such as
/// `HOSTLER_TEST_PAUSE_AT`); the CLI that creates and approves the project still connects to it.
fn ready_with(env: &[(&str, &str)]) -> (Home, PathBuf) {
    let home = Home::new();
    let repo = std::fs::canonicalize(home.make_repo("app")).unwrap();
    let out = home
        .hostler()
        .envs(env.iter().copied())
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    (home, repo)
}

/// Waits until a debug pause point is reached (it writes `<file>.reached`).
fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "the pause point was never reached"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn run(home: &Home, cwd: &Path, args: &[&str]) -> Output {
    home.hostler().args(args).current_dir(cwd).output().unwrap()
}

fn run_stdin(home: &Home, cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = home
        .hostler()
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// `hostler account set NAME --field FIELD` with the value on stdin.
fn set(home: &Home, repo: &Path, name: &str, field: &str, value: &str) -> Output {
    let out = run_stdin(
        home,
        repo,
        &["account", "set", name, "--field", field],
        value,
    );
    assert!(
        out.status.success(),
        "account set {name} --field {field} failed"
    );
    out
}

fn tty_file(home: &Home, lines: &[&str]) -> PathBuf {
    let path = home
        .dir
        .path()
        .join(format!("tty-{}", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    path
}

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

fn project(repo: &Path) -> ProjectRef {
    hostler::project::current(repo).unwrap().project
}

/// One `account.request` directly through the contract.
fn ask(
    home: &Home,
    repo: &Path,
    name: &str,
    fields: &[&str],
    id: &str,
    requester: ProcessIdentity,
) -> Result<AccountRequestInfo, HostlerError> {
    let reply = home.call(Request::AccountRequest(AccountRequestNew {
        id: id.into(),
        project: project(repo),
        name: name.into(),
        fields: fields.iter().map(|f| (*f).into()).collect(),
        reason: "run the login test".into(),
        requester,
    }))?;
    Ok(serde_json::from_value(reply).expect("an AccountRequestInfo"))
}

fn pending(home: &Home) -> Vec<AccountRequestInfo> {
    serde_json::from_value(home.call(Request::AccountRequests).unwrap()).unwrap()
}

fn pending_id(home: &Home, name: &str) -> String {
    let ids: Vec<String> = pending(home)
        .into_iter()
        .filter(|r| r.name == name)
        .map(|r| r.id)
        .collect();
    assert_eq!(ids.len(), 1, "pending requests for {name}: {ids:?}");
    ids[0].clone()
}

fn envelope(out: &str) -> Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"))
}

fn code<T>(r: Result<T, HostlerError>) -> ErrorCode {
    match r {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e.code,
    }
}

/// A running `hostler account request`; its stderr lines arrive on a channel.
struct Waiter {
    child: Child,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Waiter {
    fn start(home: &Home, repo: &Path, args: &[&str]) -> Waiter {
        let mut child = home
            .hostler()
            .args(["account", "request"])
            .args(args)
            .current_dir(repo)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = channel();
        let all = Arc::new(Mutex::new(String::new()));
        let (pipe, sink) = (child.stderr.take().unwrap(), all.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                let Ok(line) = line else { break };
                sink.lock().unwrap().push_str(&format!("{line}\n"));
                let _ = tx.send(line);
            }
        });
        Waiter {
            child,
            lines,
            stderr: all,
        }
    }

    fn line_with(&self, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(_) => panic!(
                    "no line with {needle:?} within {within:?}; stderr:\n{}",
                    self.stderr.lock().unwrap()
                ),
            }
        }
    }

    fn waiting(&self) -> String {
        self.line_with("Waiting for a person to provide", Duration::from_secs(20))
    }

    fn finish(mut self, within: Duration) -> (ExitStatus, String, String) {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!(
                    "the waiter did not exit within {within:?}; stderr:\n{}",
                    self.stderr.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut out = String::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let err = self.stderr.lock().unwrap().clone();
        (status, out, err)
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// One connection of its own, held across calls: a proof grants its connection.
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

    fn prove(&mut self) -> Result<Value, HostlerError> {
        self.call(Request::SecretsProve(Prove {
            password: SecretValue::new(PASSWORD.into()),
            key_file: None,
        }))
    }

    fn answer(
        &mut self,
        id: &str,
        fields: &[(&str, &str)],
    ) -> Result<AccountAnswerOutcome, HostlerError> {
        let value = self.call(Request::AccountAnswer(AccountAnswer {
            request_id: id.into(),
            fields: fields
                .iter()
                .map(|(name, value)| ImportEntry {
                    name: (*name).into(),
                    value: SecretValue::new((*value).into()),
                })
                .collect(),
        }))?;
        Ok(serde_json::from_value(value).expect("an AccountAnswerOutcome"))
    }
}

#[test]
fn request_then_answer_stores_and_reports() {
    let (home, repo) = ready();
    home.start_service();
    let waiter = Waiter::start(
        &home,
        &repo,
        &[
            "qa-user", "--field", "session", "--field", "env", "--reason", "log in",
        ],
    );
    let line = waiter.waiting();
    assert!(
        line.contains("provide session, env of qa-user for project app")
            && line.contains("hostler account set qa-user --field <field>"),
        "{line}"
    );
    let id = pending_id(&home, "qa-user");
    let mut conn = Conn::open(&home);
    conn.prove().unwrap();
    let outcome = conn
        .answer(&id, &[("session", MARKER), ("env", "stage-1")])
        .unwrap();
    assert_eq!(outcome.name, "qa-user");
    assert_eq!(outcome.stored_fields, vec!["session", "env"]);
    assert_eq!(outcome.answered_requests, vec![id.clone()]);

    let answered = Instant::now();
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(0), "{err}");
    assert_eq!(out, "stored\n");
    assert!(
        answered.elapsed() < Duration::from_secs(4),
        "{:?}",
        answered.elapsed()
    );
    assert!(!out.contains(MARKER) && !err.contains(MARKER));

    let listed: Value = home
        .call(Request::AccountList {
            project: project(&repo),
        })
        .unwrap();
    assert_eq!(listed[0]["name"], "qa-user");
    assert_eq!(listed[0]["fields"], json!(["env", "session"]));
    assert!(pending(&home).is_empty());
}

#[test]
fn answer_is_atomic() {
    let (home, repo) = ready();
    home.start_service();
    let id = uuid::Uuid::new_v4().to_string();
    ask(&home, &repo, "qa-user", &["session", "env"], &id, me()).unwrap();
    let mut conn = Conn::open(&home);
    conn.prove().unwrap();
    // One bad field (too short) stores nothing, and the request stays pending.
    let e = conn
        .answer(&id, &[("session", MARKER), ("env", "ab")])
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::SecretTooShort);
    let listed: Value = home
        .call(Request::AccountList {
            project: project(&repo),
        })
        .unwrap();
    assert_eq!(listed, json!([]));
    assert_eq!(pending(&home).len(), 1);
    assert_eq!(pending(&home)[0].state, RequestState::Pending);
    // A complete answer stores every field in one save.
    conn.answer(&id, &[("session", MARKER), ("env", "stage-1")])
        .unwrap();
    let listed: Value = home
        .call(Request::AccountList {
            project: project(&repo),
        })
        .unwrap();
    assert_eq!(listed[0]["fields"], json!(["env", "session"]));
    assert!(pending(&home).is_empty());
}

#[test]
fn set_fields_answer_request() {
    let (home, repo) = ready();
    home.start_service();
    let waiter = Waiter::start(
        &home,
        &repo,
        &[
            "qa-user", "--field", "session", "--field", "env", "--reason", "x",
        ],
    );
    waiter.waiting();
    let out = set(&home, &repo, "qa-user", "session", MARKER);
    assert_eq!(stdout(&out), "stored qa-user.session\n");
    assert!(!stdout(&out).contains(MARKER));
    assert_eq!(pending(&home).len(), 1, "one field is not the request");
    set(&home, &repo, "qa-user", "env", "stage-1");
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(0), "{err}");
    assert_eq!(out, "stored\n");
    assert!(!out.contains(MARKER) && !err.contains(MARKER));
    assert!(pending(&home).is_empty());
}

#[test]
fn decline_exits_1() {
    let (home, repo) = ready();
    home.start_service();
    let waiter = Waiter::start(
        &home,
        &repo,
        &["qa-user", "--field", "session", "--reason", "x"],
    );
    waiter.waiting();
    let id = pending_id(&home, "qa-user");
    let out = home
        .hostler()
        .args(["account", "decline", &id[..8]])
        .env("HOSTLER_TEST_TTY", tty_file(&home, &[PASSWORD]))
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("declined {} (qa-user for project app)\n", &id[..8])
    );
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1), "{err}");
    assert_eq!(out, "declined\n");
    // Nothing left to decline.
    let again = home
        .hostler()
        .args(["account", "decline", &id[..8]])
        .env("HOSTLER_TEST_TTY", tty_file(&home, &[PASSWORD]))
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(again.status.code(), Some(1));
    assert!(
        stderr(&again).contains("no pending account request"),
        "{}",
        stderr(&again)
    );
}

/// Without a terminal (and without the test tty), `account decline` refuses before the service.
#[test]
fn decline_is_a_person_command() {
    let (home, repo) = ready();
    home.start_service();
    let id = uuid::Uuid::new_v4().to_string();
    ask(&home, &repo, "qa-user", &["session"], &id, me()).unwrap();
    let out = run(&home, &repo, &["account", "decline", &id[..8]]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("needs a person at a terminal"),
        "{}",
        stderr(&out)
    );
    assert_eq!(pending(&home).len(), 1, "nothing was declined");
}

/// R1-1: an answer that has started but not yet taken the ordering cannot survive a terminal
/// transition: the decline/withdraw wins and no field is stored.
#[test]
fn a_terminal_transition_before_the_answers_write_stores_nothing() {
    for terminal in ["decline", "withdraw"] {
        let pause_dir = tempfile::tempdir().unwrap();
        let pause = pause_dir.path().join("pause");
        std::fs::write(&pause, b"").unwrap();
        let spec = format!("account_answer_admit:{}", pause.display());
        let (home, repo) = ready_with(&[("HOSTLER_TEST_PAUSE_AT", &spec)]);
        let id = uuid::Uuid::new_v4().to_string();
        ask(&home, &repo, "qa-user", &["session"], &id, me()).unwrap();
        let mut answerer = Conn::open(&home);
        answerer.prove().unwrap();
        let answering_id = id.clone();
        let answering =
            std::thread::spawn(move || answerer.answer(&answering_id, &[("session", MARKER)]));
        wait_for_file(&PathBuf::from(format!("{}.reached", pause.display())));
        match terminal {
            "decline" => {
                let mut person = Conn::open(&home);
                person.prove().unwrap();
                person
                    .call(Request::AccountDecline { id: id.clone() })
                    .expect("decline");
            }
            _ => {
                home.call(Request::AccountRequestWithdraw {
                    id: id.clone(),
                    timed_out: false,
                })
                .expect("withdraw");
            }
        }
        std::fs::remove_file(&pause).unwrap();
        let result = answering.join().unwrap();
        assert_eq!(code(result), ErrorCode::BadRequest, "{terminal}");
        let listed: Value = home
            .call(Request::AccountList {
                project: project(&repo),
            })
            .unwrap();
        assert_eq!(listed, json!([]), "{terminal}: the answer stored fields");
        assert!(pending(&home).is_empty(), "{terminal}");
    }
}

/// R1-1, the other ordering: an answer that holds the ordering when a decline arrives wins —
/// the decline waits, then refuses — and the fields are stored exactly once. Without the shared
/// guard the decline would finish during the answer's vault write and the answer would store
/// fields under a declined request.
#[test]
fn an_answer_holding_the_ordering_is_not_interleaved_by_a_decline() {
    let pause_dir = tempfile::tempdir().unwrap();
    let pause = pause_dir.path().join("pause");
    std::fs::write(&pause, b"").unwrap();
    let spec = format!("account_answer_checked:{}", pause.display());
    let (home, repo) = ready_with(&[("HOSTLER_TEST_PAUSE_AT", &spec)]);
    let id = uuid::Uuid::new_v4().to_string();
    ask(&home, &repo, "qa-user", &["session"], &id, me()).unwrap();
    let mut answerer = Conn::open(&home);
    answerer.prove().unwrap();
    let answering_id = id.clone();
    let (answered, declined) = std::thread::scope(|scope| {
        let answering = scope.spawn(move || answerer.answer(&answering_id, &[("session", MARKER)]));
        wait_for_file(&PathBuf::from(format!("{}.reached", pause.display())));
        let declining = scope.spawn(|| {
            let mut person = Conn::open(&home);
            person.prove().unwrap();
            person.call(Request::AccountDecline { id: id.clone() })
        });
        // The decline waits behind the answer's guard; then the answer finishes.
        std::thread::sleep(Duration::from_millis(500));
        std::fs::remove_file(&pause).unwrap();
        (answering.join().unwrap(), declining.join().unwrap())
    });
    let outcome = answered.expect("the answer that held the ordering");
    assert_eq!(outcome.answered_requests, vec![id.clone()]);
    let e = declined.unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, "no pending account request has this id");
    let listed: Value = home
        .call(Request::AccountList {
            project: project(&repo),
        })
        .unwrap();
    assert_eq!(listed[0]["fields"], json!(["session"]));
    assert!(pending(&home).is_empty());
}

/// R1-2: two same-id requests that both saw "unknown" cannot both succeed with different
/// payloads; the one behind the guard re-checks and answers the mismatch.
#[test]
fn concurrent_same_id_different_payloads_are_refused() {
    let pause_dir = tempfile::tempdir().unwrap();
    let pause = pause_dir.path().join("pause");
    std::fs::write(&pause, b"").unwrap();
    let spec = format!("account_request_checked:{}", pause.display());
    let (home, repo) = ready_with(&[("HOSTLER_TEST_PAUSE_AT", &spec)]);
    let id = uuid::Uuid::new_v4().to_string();
    let project_ref = project(&repo);
    let first = |id: &str, field: &str, reason: &str| {
        home.call(Request::AccountRequest(AccountRequestNew {
            id: id.into(),
            project: project_ref.clone(),
            name: "qa-user".into(),
            fields: vec![field.into()],
            reason: reason.into(),
            requester: me(),
        }))
    };
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| first(&id, "session", "one"));
        wait_for_file(&PathBuf::from(format!("{}.reached", pause.display())));
        let b = scope.spawn(|| first(&id, "env", "two"));
        // Let the second request pass its own lookup and wait behind the guard.
        std::thread::sleep(Duration::from_millis(500));
        std::fs::remove_file(&pause).unwrap();
        (a.join().unwrap(), b.join().unwrap())
    });
    let info: AccountRequestInfo = serde_json::from_value(a.expect("the first request")).unwrap();
    assert_eq!(info.state, RequestState::Pending);
    assert_eq!(info.fields, vec!["session".to_string()]);
    let e = b.unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert!(
        e.message
            .contains("another project, account, field set or reason"),
        "{e:?}"
    );
    assert_eq!(pending(&home).len(), 1);
}

#[test]
fn timeout_exits_75_and_withdraws() {
    let (home, repo) = ready();
    let out = run(
        &home,
        &repo,
        &[
            "account",
            "request",
            "qa-user",
            "--field",
            "session",
            "--reason",
            "x",
            "--max-wait",
            "0.02",
        ],
    );
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "[hostler] Gave up after 1 s waiting for qa-user; the request was withdrawn."
        ),
        "{}",
        stderr(&out)
    );
    assert!(pending(&home).is_empty());

    let out = run(
        &home,
        &repo,
        &[
            "account",
            "request",
            "qa-user",
            "--field",
            "session",
            "--reason",
            "x",
            "--max-wait",
            "0.02",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(75));
    let env = envelope(&stdout(&out));
    assert_eq!(env["data"]["state"], "timed_out");
    assert!(
        stderr(&out).contains("Waiting for a person"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn dead_requester_is_withdrawn() {
    let (home, repo) = ready();
    home.start_service();
    let id = uuid::Uuid::new_v4().to_string();
    // A requester of another boot is gone however long the test runs.
    let dead = ProcessIdentity {
        pid: 1,
        boot_id: "another-boot".into(),
        start_time: 1,
    };
    ask(&home, &repo, "qa-user", &["session"], &id, dead).unwrap();
    assert_eq!(pending(&home).len(), 1);
    // The 2 s tick reaps it; generous under load.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let response = home
            .call(Request::AccountRequests)
            .expect("account.requests");
        let list: Vec<AccountRequestInfo> = serde_json::from_value(response).unwrap();
        if list.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the dead requester's request stayed pending"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let declined = home
        .call(Request::AccountRequestWithdraw {
            id: id.clone(),
            timed_out: false,
        })
        .unwrap();
    let info: AccountRequestInfo = serde_json::from_value(declined).unwrap();
    assert_eq!(info.state, RequestState::Withdrawn);
}

#[test]
fn idempotent_by_id() {
    let (home, repo) = ready();
    home.start_service();
    let id = uuid::Uuid::new_v4().to_string();
    let first = ask(&home, &repo, "qa-user", &["session", "env"], &id, me()).unwrap();
    let second = ask(&home, &repo, "qa-user", &["session", "env"], &id, me()).unwrap();
    assert_eq!(first, second);
    assert_eq!(pending(&home).len(), 1);
    // The same id with another field set is refused, never answered.
    assert_eq!(
        code(ask(&home, &repo, "qa-user", &["session"], &id, me())),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(ask(&home, &repo, "other", &["session", "env"], &id, me())),
        ErrorCode::BadRequest
    );
    // Not a UUID v4.
    assert_eq!(
        code(ask(&home, &repo, "qa-user", &["session"], "r1", me())),
        ErrorCode::BadRequest
    );
    // An already-set account answers stored at once.
    let set_id = uuid::Uuid::new_v4().to_string();
    ask(&home, &repo, "admin", &["session"], &set_id, me()).unwrap();
    let mut conn = Conn::open(&home);
    conn.prove().unwrap();
    conn.answer(&set_id, &[("session", MARKER)]).unwrap();
    let info = ask(
        &home,
        &repo,
        "admin",
        &["session"],
        &uuid::Uuid::new_v4().to_string(),
        me(),
    )
    .unwrap();
    assert_eq!(info.state, RequestState::Stored);
    assert_eq!(info.answered_at_ms, Some(info.created_at_ms));
    assert_eq!(
        pending(&home).len(),
        1,
        "the stored-at-once request is not pending"
    );
}

#[test]
fn unapproved_project_refused() {
    let (home, repo) = ready();
    home.start_service();
    let other = home.make_repo("other");
    assert_eq!(
        code(ask(
            &home,
            &other,
            "qa-user",
            &["session"],
            &uuid::Uuid::new_v4().to_string(),
            me()
        )),
        ErrorCode::ProjectNotApproved
    );
    // Names and fields are checked without echoing them.
    let e = ask(
        &home,
        &repo,
        "QA",
        &["session"],
        &uuid::Uuid::new_v4().to_string(),
        me(),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, hostler::contract::accounts::INVALID_ACCOUNT_NAME);
    assert!(!format!("{e:?}").contains("QA"));
    let e = ask(
        &home,
        &repo,
        "qa-user",
        &["Session"],
        &uuid::Uuid::new_v4().to_string(),
        me(),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, hostler::contract::accounts::INVALID_FIELD_NAME);
}

/// The service answers every state change as `accounts.request_changed` (spec §6.2).
#[test]
fn events_carry_every_state() {
    let (home, repo) = ready();
    home.start_service();
    let rt = raw::runtime();
    let prove = format!(r#"{{"password":"{PASSWORD}","key_file":null}}"#);

    // Pending, then stored (through the app's `account.answer`).
    rt.block_on(async {
        let mut listener = Raw::connect(&home).await;
        let id = uuid::Uuid::new_v4().to_string();
        raw::blocking(|| ask(&home, &repo, "qa-user", &["session"], &id, me()).unwrap());
        let info = next_state(&mut listener, &id, RequestState::Pending).await;
        assert_eq!(info.name, "qa-user");
        assert_eq!(info.fields, vec!["session".to_string()]);

        let mut writer = Raw::connect(&home).await;
        let answer = writer.call(2, "secrets.prove", Some(&prove)).await;
        assert!(answer["error"].is_null(), "{answer}");
        let answer = writer
            .call(
                3,
                "account.answer",
                Some(&format!(
                    r#"{{"request_id":"{id}","fields":[{{"name":"session","value":"{MARKER}"}}]}}"#
                )),
            )
            .await;
        assert!(answer["error"].is_null(), "{answer}");
        let info = next_state(&mut listener, &id, RequestState::Stored).await;
        assert!(info.answered_at_ms.is_some());
    });

    // Declined, withdrawn and timed out, each on its own request.
    for (want, command) in [
        (RequestState::Declined, "decline"),
        (RequestState::Withdrawn, "withdraw"),
        (RequestState::TimedOut, "timed_out"),
    ] {
        rt.block_on(async {
            let mut listener = Raw::connect(&home).await;
            let id = uuid::Uuid::new_v4().to_string();
            raw::blocking(|| ask(&home, &repo, "qa-user", &["env"], &id, me()).unwrap());
            next_state(&mut listener, &id, RequestState::Pending).await;
            match command {
                "decline" => {
                    let mut writer = Raw::connect(&home).await;
                    let prove_reply = writer.call(2, "secrets.prove", Some(&prove)).await;
                    assert!(prove_reply["error"].is_null(), "{prove_reply}");
                    let answer = writer
                        .call(3, "account.decline", Some(&format!(r#"{{"id":"{id}"}}"#)))
                        .await;
                    assert!(answer["error"].is_null(), "{answer}");
                }
                "withdraw" | "timed_out" => {
                    let timed_out = command == "timed_out";
                    let answer = raw::blocking(|| {
                        home.call(Request::AccountRequestWithdraw {
                            id: id.clone(),
                            timed_out,
                        })
                    })
                    .unwrap();
                    assert!(answer.is_object());
                }
                other => panic!("unexpected command {other}"),
            }
            next_state(&mut listener, &id, want).await;
        });
    }
}

async fn next_state(listener: &mut Raw, id: &str, want: RequestState) -> AccountRequestInfo {
    loop {
        let (_, event) = listener.event().await;
        if let Event::AccountRequestChanged(info) = event
            && info.id == id
            && info.state == want
        {
            return info;
        }
    }
}

#[test]
fn no_value_in_any_output() {
    let (home, repo) = ready();
    home.start_service();
    let waiter = Waiter::start(
        &home,
        &repo,
        &["qa-user", "--field", "session", "--reason", "x"],
    );
    waiter.waiting();
    let id = pending_id(&home, "qa-user");
    let mut conn = Conn::open(&home);
    conn.prove().unwrap();
    conn.answer(&id, &[("session", MARKER)]).unwrap();
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(0));
    for text in [out, err] {
        assert!(!text.contains(MARKER), "{text}");
    }
    let json = run(&home, &repo, &["account", "requests", "--json"]);
    assert!(!stdout(&json).contains(MARKER));
    let listed = run(&home, &repo, &["account", "list", "--json"]);
    assert!(!stdout(&listed).contains(MARKER));
    let log = raw::files_text(&home.paths().log_dir);
    assert!(
        !log.contains(MARKER),
        "the service log holds an account value"
    );
    let everything = raw::files_text(home.dir.path());
    let kdbx = std::fs::read(home.home.join("secrets/hostler.kdbx")).unwrap_or_default();
    assert!(
        !everything.contains(MARKER),
        "a file under the home holds the answer in the clear"
    );
    assert!(!String::from_utf8_lossy(&kdbx).contains(MARKER));
}

#[test]
fn requests_lists_pending_with_sanitized_reason() {
    let (home, repo) = ready();
    home.start_service();
    let empty = run(&home, &repo, &["account", "requests"]);
    assert_eq!(stdout(&empty), "no pending account requests\n");
    let waiter = Waiter::start(
        &home,
        &repo,
        &[
            "qa-user",
            "--field",
            "session",
            "--field",
            "env",
            "--reason",
            "clear \u{1b}[2J screen",
        ],
    );
    waiter.waiting();
    let out = run(&home, &repo, &["account", "requests"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("  app  qa-user  session,env  "), "{text}");
    assert!(text.contains("clear \u{FFFD}[2J screen"), "{text}");
    let id = pending_id(&home, "qa-user");
    assert!(text.starts_with(&id[..8]), "{text}");
    let json = run(&home, &repo, &["account", "requests", "--json"]);
    let env = envelope(&stdout(&json));
    assert_eq!(env["data"][0]["id"], id.as_str());
}

#[test]
fn guide_has_the_asking_section() {
    let home = Home::new();
    let out = run(&home, home.dir.path(), &["guide", "accounts"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for needed in [
        "## Asking for an account",
        "hostler account request",
        "hostler account requests",
        "hostler account decline",
        "never ask for a field's value in chat",
    ] {
        assert!(text.contains(needed), "the accounts guide lacks {needed:?}");
    }
    assert!(!text.contains("there is no `account request` in this version"));
}
