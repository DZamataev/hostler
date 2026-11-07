//! `hostler queue …` against the real binary and service (plan Task 9). Entries are made through
//! the client with this test process as their (live) holder.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use hostler::client::Client;
use hostler::contract::heavy::{AcquireOutcome, HeavyAcquire, HeavyOutcome, HeavyRelease};
use hostler::contract::wire::Request;
use hostler::paths::Paths;
use hostler::procs::{SysProcessTable, current_identity};
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_hostler");

struct Home {
    home: PathBuf,
    _dir: TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        Home {
            home: dir.path().join("home"),
            _dir: dir,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        // A ledger that does not exist: this machine's real rn-looky work must not block tests.
        Command::new(BIN)
            .env("HOSTLER_HOME", &self.home)
            .env(
                "HOSTLER_LOOKY_LEDGER",
                self.home.join("no-looky-ledger.json"),
            )
            .env_remove("HOSTLER_TEST_CONTRACT_VERSION")
            .env_remove("HOSTLER_TEST_EXIT_AFTER_COMMIT")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    async fn client(&self) -> Client {
        let paths = Paths::under(self.home.clone(), true);
        paths.ensure_dirs().unwrap();
        // Started by `service start` with the ledger override, never by this process.
        let started = self.run(&["service", "start"]);
        assert!(started.status.success(), "{started:?}");
        Client::connect(&paths).await.unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["service", "stop", "--force"]);
    }
}

/// Acquires a slot named `command` held by this test process; returns its id and outcome.
async fn acquire(client: &mut Client, command: &str) -> (String, AcquireOutcome) {
    let req = HeavyAcquire {
        id: uuid::Uuid::new_v4().to_string(),
        kind: "gradle".into(),
        command: command.into(),
        cwd: "/work/app".into(),
        workspace_root: None,
        holder: current_identity(&SysProcessTable::new().unwrap()).unwrap(),
        reentrant_token: None,
        no_wait: false,
    };
    let value = client
        .call(Request::HeavyAcquire(req.clone()))
        .await
        .unwrap();
    (req.id, serde_json::from_value(value).unwrap())
}

async fn release(client: &mut Client, id: &str, exit_code: i32) {
    let outcome = if exit_code == 0 {
        HeavyOutcome::Ok
    } else {
        HeavyOutcome::Failed
    };
    let release = HeavyRelease {
        id: id.into(),
        exit_code: Some(exit_code),
        outcome,
    };
    client.call(Request::HeavyRelease(release)).await.unwrap();
}

#[tokio::test]
async fn queue_json_lists_running_and_waiting() {
    let home = Home::new();
    let mut client = home.client().await;
    let (running, granted) = acquire(&mut client, "build one").await;
    assert!(matches!(granted, AcquireOutcome::Granted { .. }));
    let (waiting, queued) = acquire(&mut client, "build two").await;
    assert!(matches!(queued, AcquireOutcome::Queued { .. }));

    let v = home.json(&["queue", "--json"]);
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["capacity"], 1);
    assert_eq!(v["data"]["running"][0]["id"], running.as_str());
    assert_eq!(v["data"]["waiting"][0]["id"], waiting.as_str());
    assert_eq!(v["data"]["foreign_running"], serde_json::json!([]));

    let out = home.run(&["queue"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("capacity 1, 1 running, 1 waiting"), "{text}");
    assert!(
        text.contains(&format!("PID {}", std::process::id())),
        "{text}"
    );
    assert!(text.contains(&format!("1. {waiting}")), "{text}");
}

#[tokio::test]
async fn history_shows_outcomes() {
    let home = Home::new();
    let mut client = home.client().await;
    let (ok, _) = acquire(&mut client, "ok").await;
    release(&mut client, &ok, 0).await;
    let (stopped, _) = acquire(&mut client, "stopped").await;
    let released = home.run(&["queue", "release", "--id", &stopped, "--force"]);
    assert!(released.status.success());
    let (failed, _) = acquire(&mut client, "failed").await;
    let (withdrawn, _) = acquire(&mut client, "withdrawn").await;
    assert!(
        home.run(&["queue", "withdraw", "--id", &withdrawn])
            .status
            .success()
    );
    release(&mut client, &failed, 3).await;

    let v = home.json(&["queue", "history", "--since", "1h", "--json"]);
    let outcomes: Vec<(String, String, Value)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_owned(),
                i["outcome"].as_str().unwrap().to_owned(),
                i["exit_code"].clone(),
            )
        })
        .collect();
    for (id, outcome, exit_code) in [
        (&ok, "ok", Value::from(0)),
        (&stopped, "stopped", Value::Null),
        (&withdrawn, "withdrawn", Value::Null),
        (&failed, "failed", Value::from(3)),
    ] {
        assert!(
            outcomes.contains(&(id.clone(), outcome.to_owned(), exit_code)),
            "{outcome}: {outcomes:?}"
        );
    }
    let limited = home.json(&["queue", "history", "--limit", "2", "--json"]);
    assert_eq!(limited["data"].as_array().unwrap().len(), 2);

    let text = String::from_utf8_lossy(&home.run(&["queue", "history"]).stdout).into_owned();
    for word in ["ok", "stopped", "withdrawn", "failed", "exit 3"] {
        assert!(text.contains(word), "{word}: {text}");
    }
}

#[test]
fn release_without_force_is_refused_with_recovery() {
    let home = Home::new();
    let id = uuid::Uuid::new_v4().to_string();
    let out = home.run(&["queue", "release", "--id", &id]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("operator's explicit permission"), "{err}");
    assert!(
        err.contains(&format!("To fix: hostler queue release --id {id} --force")),
        "{err}"
    );
}

#[test]
fn capacity_set_17_is_rejected() {
    let home = Home::new();
    let out = home.run(&["queue", "capacity", "set", "17"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("capacity must be 1–16, not 17"), "{err}");
    assert!(
        home.run(&["queue", "capacity", "set", "2"])
            .status
            .success()
    );
    assert_eq!(home.json(&["queue", "--json"])["data"]["capacity"], 2);
}
