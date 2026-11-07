//! The secrets engine in the real service (plan P2 Task 6): over the socket, through stops,
//! restarts and crashes. Every test has its own home; the fixture's service uses the fast KDF.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::{Home, stderr};
use hostler::client::Client;
use hostler::contract::ErrorCode;
use hostler::contract::secrets::{
    LockState, Materialize, ProjectRef, SecretList, SecretSet, SecretsStatus, Unlock,
};
use hostler::contract::wire::Request;
use hostler::secrets::value::{RawCommand, SecretValue};
use serde_json::Value;

const PASSWORD: &str = "pass-1234";
const CANARY: &str = "hlx-canary-7f3a9c";

fn project(home: &Home, name: &str) -> (PathBuf, ProjectRef) {
    let repo = home.make_repo(name);
    let project = hostler::project::current(&repo).unwrap().project;
    (repo, project)
}

fn unlock(home: &Home, create: bool) -> SecretsStatus {
    let status = home
        .call(Request::SecretsUnlock(Unlock {
            password: SecretValue::new(PASSWORD.into()),
            key_file: None,
            forget_key_file: false,
            create,
        }))
        .unwrap();
    serde_json::from_value(status).unwrap()
}

fn status(home: &Home) -> SecretsStatus {
    serde_json::from_value(home.call(Request::SecretsStatus).unwrap()).unwrap()
}

fn approve(home: &Home, p: &ProjectRef) {
    home.call(Request::ProjectApprove(p.clone())).unwrap();
}

fn set(
    home: &Home,
    p: &ProjectRef,
    name: &str,
    value: &str,
) -> Result<Value, hostler::contract::HostlerError> {
    home.call(Request::SecretSet(SecretSet {
        project: p.clone(),
        name: name.into(),
        value: SecretValue::new(value.into()),
    }))
}

fn materialize_request(p: &ProjectRef, names: &[&str], command: &str) -> Materialize {
    Materialize {
        run_id: uuid::Uuid::new_v4().to_string(),
        project: p.clone(),
        names: names.iter().map(|n| (*n).into()).collect(),
        command: RawCommand::new(command.into()),
        workspace: "/work/app".into(),
    }
}

/// `hostler service run` as a child of the test, with extra (test hook) environment.
fn service(home: &Home, env: &[(&str, &str)]) -> Child {
    let child = home
        .hostler()
        .args(["service", "run"])
        .envs(env.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !home
        .hostler()
        .args(["service", "status"])
        .output()
        .unwrap()
        .status
        .success()
    {
        assert!(Instant::now() < deadline, "the service never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}

fn stop(home: &Home) -> std::process::Output {
    home.hostler().args(["service", "stop"]).output().unwrap()
}

/// Sends raw lines after a hello and returns the answer lines (events skipped).
fn raw(home: &Home, lines: &[&str]) -> Vec<String> {
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    home.start_service();
    let paths = home.paths();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let name = hostler::service::server::socket_name(&paths.socket).unwrap();
        let stream = Stream::connect(name).await.unwrap();
        let (recv, mut send) = tokio::io::split(stream);
        let mut reader = BufReader::new(recv).lines();
        let hello = format!(
            "{{\"id\":1,\"method\":\"hello\",\"params\":{{\"contract_version\":{},\"client\":\"raw\"}}}}\n",
            hostler::contract::CONTRACT_VERSION
        );
        send.write_all(hello.as_bytes()).await.unwrap();
        for line in lines {
            send.write_all(line.as_bytes()).await.unwrap();
            send.write_all(b"\n").await.unwrap();
        }
        let mut answers = Vec::new();
        while answers.len() < lines.len() + 1 {
            let line = tokio::time::timeout(Duration::from_secs(30), reader.next_line())
                .await
                .expect("an answer within 30 s")
                .unwrap()
                .expect("the service closed the connection");
            if !line.contains("\"event\"") {
                answers.push(line);
            }
        }
        answers.remove(0);
        answers
    })
}

/// Every file under `dir`, as text.
fn files_text(dir: &Path) -> String {
    let mut text = String::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return text;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            text.push_str(&files_text(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    text
}

fn leftovers(home: &Home) -> Vec<String> {
    std::fs::read_dir(&home.home)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("hostler.kdbx.tmp-") || n.starts_with("hostler.kdbx.prev-"))
        .collect()
}

#[test]
fn unlock_create_then_restart_starts_locked() {
    let home = Home::new();
    assert_eq!(status(&home).state, LockState::Missing);
    assert_eq!(unlock(&home, true).state, LockState::Unlocked);
    assert!(home.paths().kdbx.is_file());
    let restart = home
        .hostler()
        .args(["service", "restart"])
        .output()
        .unwrap();
    assert!(restart.status.success(), "{}", stderr(&restart));
    assert_eq!(status(&home).state, LockState::Locked);
    assert_eq!(unlock(&home, false).state, LockState::Unlocked);
}

#[test]
fn secret_flow_over_the_socket() {
    let home = Home::new();
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    let e = set(&home, &p, "API_KEY", CANARY).unwrap_err();
    assert_eq!(e.code, ErrorCode::ProjectNotApproved);
    approve(&home, &p);
    set(&home, &p, "API_KEY", CANARY).unwrap();
    let list: SecretList = serde_json::from_value(
        home.call(Request::SecretList {
            project: p.clone(),
            declared: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.secrets.len(), 1);
    assert_eq!(list.secrets[0].name, "API_KEY");
    let got = home
        .materialize(materialize_request(&p, &["API_KEY"], "make"))
        .unwrap();
    assert!(got[0].value.expose() == CANARY);
    // The Value path refuses it.
    let e = home
        .call(Request::SecretMaterialize(materialize_request(
            &p,
            &["API_KEY"],
            "make",
        )))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal);
}

#[test]
fn materialize_answer_over_a_closed_connection() {
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::AsyncWriteExt;

    let home = Home::new();
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    set(&home, &p, "API_KEY", CANARY).unwrap();
    let frame = serde_json::to_string(&hostler::contract::wire::ClientFrame {
        id: 2,
        request: Request::SecretMaterialize(materialize_request(&p, &["API_KEY"], "make")),
    })
    .unwrap();
    let paths = home.paths();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let name = hostler::service::server::socket_name(&paths.socket).unwrap();
        let mut stream = Stream::connect(name).await.unwrap();
        let hello = format!(
            "{{\"id\":1,\"method\":\"hello\",\"params\":{{\"contract_version\":{},\"client\":\"raw\"}}}}\n",
            hostler::contract::CONTRACT_VERSION
        );
        stream.write_all(hello.as_bytes()).await.unwrap();
        stream.write_all(frame.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        // Gone before the answer.
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(status(&home).state, LockState::Unlocked);
    let got = home
        .materialize(materialize_request(&p, &["API_KEY"], "make"))
        .unwrap();
    assert!(got[0].value.expose() == CANARY);
    assert!(!files_text(&home.paths().log_dir).contains(CANARY));
}

#[test]
fn malformed_set_frame_does_not_echo_the_value() {
    let home = Home::new();
    let answers = raw(
        &home,
        &[
            r#"{"id":7,"method":"secret.set","params":{"value":12,"name":"hunter2-canary"}}"#,
            r#"{"id":8,"method":"secret.set","params":{"value":"hunter2-canary"}}"#,
            r#"{"id":9,"method":"secrets.unlock","params":{"password":"hunter2-canary"}}"#,
        ],
    );
    assert_eq!(answers.len(), 3);
    for (case, answer) in answers.iter().enumerate() {
        assert!(!answer.contains("hunter2"), "case {case} echoed the value");
        let frame: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(frame["error"]["code"], "bad_request", "case {case}");
        assert_eq!(
            frame["error"]["message"], "unreadable request",
            "case {case}"
        );
    }
}

#[test]
fn malformed_materialize_frame_does_not_echo_the_command() {
    let home = Home::new();
    let answers = raw(
        &home,
        &[
            r#"{"id":8,"method":"secret.materialize","params":"curl hunter2-canary"}"#,
            r#"{"id":9,"method":"secret.materialize","params":{"names":"hunter2-canary"}}"#,
        ],
    );
    for (case, answer) in answers.iter().enumerate() {
        assert!(
            !answer.contains("hunter2"),
            "case {case} echoed the command"
        );
        let frame: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(frame["id"], 8 + case as u64);
        assert_eq!(frame["error"]["message"], "unreadable request");
    }
}

#[test]
fn stop_during_a_paused_save_waits_for_it() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_VAULT_PAUSE", "before_place:3000")]);
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    let started = Instant::now();
    let save = {
        let home_dir = home.home.clone();
        let p = p.clone();
        std::thread::spawn(move || {
            let paths = hostler::paths::Paths::under(home_dir, true);
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    Client::connect(&paths)
                        .await?
                        .call(Request::SecretSet(SecretSet {
                            project: p,
                            name: "API_KEY".into(),
                            value: SecretValue::new(CANARY.into()),
                        }))
                        .await
                })
        })
    };
    std::thread::sleep(Duration::from_millis(800));
    let stopped = stop(&home);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert!(
        started.elapsed() >= Duration::from_millis(2900),
        "the stop returned before the save: {:?}",
        started.elapsed()
    );
    assert!(
        save.join().unwrap().is_ok(),
        "the paused save did not complete"
    );
    assert!(child.wait().unwrap().success());
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    // A new service starts locked; the saved value is there.
    assert_eq!(unlock(&home, false).state, LockState::Unlocked);
    let got = home
        .materialize(materialize_request(&p, &["API_KEY"], "make"))
        .unwrap();
    assert!(got[0].value.expose() == CANARY);
}

#[test]
fn stop_during_a_paused_unlock_waits_for_it() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_VAULT_PAUSE", "unlock:3000")]);
    // Creating does not pass the unlock point.
    unlock(&home, true);
    home.call(Request::SecretsLock).unwrap();
    let started = Instant::now();
    let unlocking = {
        let home_dir = home.home.clone();
        std::thread::spawn(move || {
            let paths = hostler::paths::Paths::under(home_dir, true);
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    Client::connect(&paths)
                        .await?
                        .call(Request::SecretsUnlock(Unlock {
                            password: SecretValue::new(PASSWORD.into()),
                            key_file: None,
                            forget_key_file: false,
                            create: false,
                        }))
                        .await
                })
        })
    };
    std::thread::sleep(Duration::from_millis(800));
    let stopped = stop(&home);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert!(started.elapsed() >= Duration::from_millis(2900));
    let answer = unlocking.join().unwrap().unwrap();
    assert_eq!(answer["state"], "unlocked");
    assert!(child.wait().unwrap().success());
}

#[test]
fn stopping_service_refuses_new_secrets_work() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_VAULT_PAUSE", "before_place:4000")]);
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    let save = {
        let home_dir = home.home.clone();
        let p = p.clone();
        std::thread::spawn(move || {
            let paths = hostler::paths::Paths::under(home_dir, true);
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    Client::connect(&paths)
                        .await?
                        .call(Request::SecretSet(SecretSet {
                            project: p,
                            name: "API_KEY".into(),
                            value: SecretValue::new(CANARY.into()),
                        }))
                        .await
                })
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    let stopper = {
        let mut command = home.hostler();
        std::thread::spawn(move || command.args(["service", "stop"]).output().unwrap())
    };
    std::thread::sleep(Duration::from_millis(800));
    let paths = home.paths();
    let refused = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            Client::connect(&paths)
                .await?
                .call(Request::SecretsStatus)
                .await
        })
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::ServiceUnavailable, "{refused:?}");
    assert_eq!(refused.details["reason"], "stopping");
    assert!(stopper.join().unwrap().status.success());
    assert!(save.join().unwrap().is_ok());
    assert!(child.wait().unwrap().success());
}

#[test]
fn restart_recovers_a_moved_aside_file() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_VAULT_PAUSE", "before_place:5000")]);
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    // Every save pauses: this one completes after 5 s.
    set(&home, &p, "FIRST", "value-first").unwrap();
    let save = {
        let home_dir = home.home.clone();
        let p = p.clone();
        std::thread::spawn(move || {
            let paths = hostler::paths::Paths::under(home_dir, true);
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    Client::connect(&paths)
                        .await?
                        .call(Request::SecretSet(SecretSet {
                            project: p,
                            name: "SECOND".into(),
                            value: SecretValue::new("value-second".into()),
                        }))
                        .await
                })
        })
    };
    // Paused before placing: the database is moved aside.
    let kdbx = home.paths().kdbx;
    let deadline = Instant::now() + Duration::from_secs(10);
    while kdbx.exists() {
        assert!(
            Instant::now() < deadline,
            "the save never moved the database aside"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(save.join().unwrap().is_err());
    assert!(!kdbx.exists());
    // The next service puts it back.
    assert_eq!(status(&home).state, LockState::Locked);
    assert!(kdbx.is_file());
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    unlock(&home, false);
    let got = home
        .materialize(materialize_request(&p, &["FIRST"], "make"))
        .unwrap();
    assert!(got[0].value.expose() == "value-first");
}

#[test]
fn v1_service_gives_contract_mismatch_to_secret_calls() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_CONTRACT_VERSION", "1")]);
    let paths = home.paths();
    let e = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { Client::connect(&paths).await.map(|_| ()) })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ContractMismatch);
    // The lifecycle methods are frozen: it still stops.
    assert!(stop(&home).status.success());
    assert!(child.wait().unwrap().success());
}

#[test]
fn idle_restart_still_refused_while_heavy_runs() {
    use hostler::contract::heavy::HeavyAcquire;
    use hostler::procs::{SysProcessTable, current_identity};
    use hostler::store::Store;

    let home = Home::new();
    unlock(&home, true);
    let store = Store::open(&home.paths().db).unwrap();
    let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    store
        .insert_waiting(
            &HeavyAcquire {
                id: id.clone(),
                kind: "gradle".into(),
                command: "./gradlew assemble".into(),
                cwd: "/work/app".into(),
                workspace_root: None,
                holder,
                reentrant_token: None,
                no_wait: false,
            },
            0,
        )
        .unwrap();
    store.mark_running(&id, 0).unwrap();
    let restart = home
        .hostler()
        .args(["service", "restart"])
        .output()
        .unwrap();
    assert!(!restart.status.success());
    // Still the same, unlocked service.
    assert_eq!(status(&home).state, LockState::Unlocked);
}

#[test]
fn service_log_has_no_value() {
    let home = Home::new();
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    set(&home, &p, "API_KEY", CANARY).unwrap();
    home.materialize(materialize_request(
        &p,
        &["API_KEY"],
        &format!("curl -H 'X: {CANARY}'"),
    ))
    .unwrap();
    raw(
        &home,
        &[&format!(
            r#"{{"id":5,"method":"secret.set","params":{{"value":"{CANARY}"}}}}"#
        )],
    );
    let stopped = stop(&home);
    assert!(stopped.status.success());
    let paths = home.paths();
    assert!(!files_text(&paths.log_dir).contains(CANARY));
    // The history holds the masked command only.
    let conn = rusqlite::Connection::open(&paths.db).unwrap();
    let command: String = conn
        .query_row("SELECT command FROM secret_materializations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(command, "curl -H 'X: ***'");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_service_is_not_dumpable() {
    use std::os::unix::fs::MetadataExt;

    let home = Home::new();
    let status = home.call(Request::ServiceStatus).unwrap();
    let pid = status["pid"].as_u64().unwrap();
    let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).unwrap();
    let core = limits
        .lines()
        .find(|l| l.starts_with("Max core file size"))
        .unwrap();
    let fields: Vec<&str> = core.split_whitespace().collect();
    assert_eq!(&fields[4..6], ["0", "0"], "{core}");
    // A process that is not dumpable has its /proc files owned by root.
    let owner = std::fs::metadata(format!("/proc/{pid}/environ"))
        .unwrap()
        .uid();
    assert_eq!(owner, 0);
}

/// Handoff (Task 4): NotDurable is an uncertain commit — the change may be at `K`, the vault
/// stays unlocked, and the answer never says that nothing was written.
#[test]
fn not_durable_save_keeps_the_vault_unlocked() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_VAULT_FAIL", "sync_dir")]);
    let not_durable = |e: hostler::contract::HostlerError| {
        assert_eq!(e.details["reason"], "not_durable", "{e:?}");
        assert!(!e.message.contains("nothing was written"), "{}", e.message);
    };
    not_durable(
        home.call(Request::SecretsUnlock(Unlock {
            password: SecretValue::new(PASSWORD.into()),
            key_file: None,
            forget_key_file: false,
            create: true,
        }))
        .unwrap_err(),
    );
    assert_eq!(status(&home).state, LockState::Unlocked);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    not_durable(set(&home, &p, "API_KEY", CANARY).unwrap_err());
    assert_eq!(status(&home).state, LockState::Unlocked);
    // The group the save used is recorded, and the value is at `K`.
    let info: hostler::contract::secrets::ProjectInfo =
        serde_json::from_value(home.call(Request::ProjectResolve(p.clone())).unwrap()).unwrap();
    assert!(info.group_uuid.is_some());
    let got = home
        .materialize(materialize_request(&p, &["API_KEY"], "make"))
        .unwrap();
    assert!(got[0].value.expose() == CANARY);
    assert!(stop(&home).status.success());
    assert!(child.wait().unwrap().success());
}

/// T6-5: a client that stops reading, with its output queue full behind a large answer, must
/// not hold a stop up to the 30 s forced exit: a secrets reply waiting for queue room gives up
/// its claim on the drain after 2 s.
#[test]
fn stop_is_not_held_up_by_an_unread_reply_queue() {
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::AsyncWriteExt;

    let home = Home::new();
    let mut child = service(&home, &[]);
    unlock(&home, true);
    let (_repo, p) = project(&home, "app");
    approve(&home, &p);
    set(&home, &p, "API_KEY", &"x".repeat(64 * 1024)).unwrap();
    let paths = home.paths();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    // One large answer blocks the writer (nothing is ever read); 16 ordinary answers fill the
    // output queue behind it; the last call's secrets reply then waits for queue room while
    // it holds a claim on the stop's drain.
    let p2 = p.clone();
    runtime.spawn(async move {
        let name = hostler::service::server::socket_name(&paths.socket).unwrap();
        let mut stream = Stream::connect(name).await.unwrap();
        let mut frames = vec![format!(
            "{{\"id\":1,\"method\":\"hello\",\"params\":{{\"contract_version\":{},\"client\":\"stalled\"}}}}",
            hostler::contract::CONTRACT_VERSION
        )];
        let mut requests = vec![Request::SecretMaterialize(materialize_request(&p2, &["API_KEY"], "make"))];
        requests.extend((0..16).map(|_| Request::QueueSnapshot));
        requests.push(Request::SecretsStatus);
        for (id, request) in (2..).zip(requests) {
            frames.push(
                serde_json::to_string(&hostler::contract::wire::ClientFrame { id, request }).unwrap(),
            );
        }
        for frame in frames {
            if stream.write_all(frame.as_bytes()).await.is_err()
                || stream.write_all(b"\n").await.is_err()
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(120)).await;
        drop(stream);
    });
    std::thread::sleep(Duration::from_millis(3500));
    let started = Instant::now();
    let stopped = stop(&home);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the stop took {:?}",
        started.elapsed()
    );
    assert!(
        child.wait().unwrap().success(),
        "the service took the forced exit"
    );
}

/// T6-7 over the socket: the reviewer's schedule — a request in the target project is
/// answered when the source sets the shared name again.
#[test]
fn shared_in_request_is_answered_over_the_socket() {
    use hostler::contract::secrets::{RequestState, SecretRequest, SecretRequestInfo, Share};
    use hostler::procs::{SysProcessTable, current_identity};

    let home = Home::new();
    unlock(&home, true);
    let (_s_repo, s) = project(&home, "source");
    let (_t_repo, t) = project(&home, "target");
    approve(&home, &s);
    approve(&home, &t);
    set(&home, &s, "TOKEN", "value-1").unwrap();
    let target: hostler::contract::secrets::ProjectInfo =
        serde_json::from_value(home.call(Request::ProjectResolve(t.clone())).unwrap()).unwrap();
    home.call(Request::SecretShare(Share {
        from: s.clone(),
        name: "TOKEN".into(),
        to_project_id: target.id,
    }))
    .unwrap();
    home.call(Request::SecretRemove {
        project: s.clone(),
        name: "TOKEN".into(),
    })
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let request = Request::SecretRequest(SecretRequest {
        id: id.clone(),
        project: t.clone(),
        name: "TOKEN".into(),
        reason: "integration tests".into(),
        requester: current_identity(&SysProcessTable::new().unwrap()).unwrap(),
    });
    let info: SecretRequestInfo =
        serde_json::from_value(home.call(request.clone()).unwrap()).unwrap();
    assert_eq!(info.state, RequestState::Pending);
    set(&home, &s, "TOKEN", "value-2").unwrap();
    let pending: Vec<SecretRequestInfo> =
        serde_json::from_value(home.call(Request::SecretRequests).unwrap()).unwrap();
    assert!(pending.is_empty());
    let info: SecretRequestInfo = serde_json::from_value(home.call(request).unwrap()).unwrap();
    assert_eq!(info.state, RequestState::Stored);
}
