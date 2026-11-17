//! P4 Task 9 F-1: a client that may not read the boot id (both agent sandboxes deny `sysctl
//! kern.bootsessionuuid`) still holds, releases and queues, once its socket is allowed: it sends
//! its identity without a boot id, and the service completes it with its own. The sandbox is
//! stood in for by `HOSTLER_TEST_BOOTID_ERRNO=EPERM` on the client alone (debug builds); the real
//! seatbelt run is in the task report.

mod common;

use std::process::Output;
use std::time::Duration;

use common::{Home, stderr};
use hostler::procs::{ProcessTable, SysProcessTable};
use serde_json::Value;

const DENIED: (&str, &str) = ("HOSTLER_TEST_BOOTID_ERRNO", "EPERM");

fn sandboxed(home: &Home, repo: &std::path::Path, args: &[&str]) -> Output {
    home.hostler()
        .env(DENIED.0, DENIED.1)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

fn claims(home: &Home) -> Vec<Value> {
    let snapshot = serde_json::to_value(home.snapshot()).unwrap();
    snapshot["resources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|state| state["claims"].as_array().unwrap().clone())
        .collect()
}

#[test]
fn a_client_without_the_boot_id_holds_releases_and_queues() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    let boot = SysProcessTable::new().unwrap().boot_id();
    assert!(!boot.is_empty());

    // A TTL hold, then its release.
    let out = sandboxed(&home, &repo, &["hold", "screen", "--ttl", "10", "--json"]);
    assert!(out.status.success(), "{}{}", json(&out), stderr(&out));
    let hold = json(&out)["data"].clone();
    assert_eq!(hold["holder"]["boot_id"], boot.as_str(), "{hold}");
    let id = hold["id"].as_str().unwrap().to_owned();
    let out = sandboxed(&home, &repo, &["release", &id, "--json"]);
    assert!(out.status.success(), "{}", json(&out));
    assert!(claims(&home).is_empty());

    // A hold bound to this (test) process, the client's parent: the service completes the
    // binding's boot id too, so it is not reaped as "another boot" on the next ticks.
    let out = sandboxed(
        &home,
        &repo,
        &["hold", "screen", "--while-parent", "--json"],
    );
    assert!(out.status.success(), "{}{}", json(&out), stderr(&out));
    let hold = json(&out)["data"].clone();
    assert_eq!(hold["binding"]["kind"], "process", "{hold}");
    assert_eq!(
        hold["binding"]["holder"]["boot_id"],
        boot.as_str(),
        "{hold}"
    );
    std::thread::sleep(Duration::from_secs(5));
    let held = claims(&home);
    assert_eq!(held.len(), 1, "the bound hold was reaped: {held:?}");
    let id = hold["id"].as_str().unwrap().to_owned();
    assert!(sandboxed(&home, &repo, &["release", &id]).status.success());

    // `run --heavy` queues normally: no fail-open warning, and the queue recorded it.
    let out = sandboxed(&home, &repo, &["run", "--heavy", "--", "true"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("Warning"), "{}", stderr(&out));
    let out = sandboxed(&home, &repo, &["queue", "history", "--json"]);
    let history = json(&out)["data"].as_array().unwrap().clone();
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["command"], "true");
}

/// The service itself needs its boot id: in a place that denies it, it refuses to run.
#[test]
fn the_service_requires_its_boot_id() {
    let home = Home::new();
    let out = home
        .hostler()
        .env(DENIED.0, DENIED.1)
        .args(["service", "run"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("may not read this machine's boot id"),
        "{}",
        stderr(&out)
    );
}

/// Review f1-r1: a sandboxed `--secret-file` run learns the boot id from the service before it
/// writes `owner.json`, so the stored owner carries the machine's real boot id, never an empty
/// one a later sweep could not place.
#[cfg(unix)]
#[test]
fn a_sandboxed_secret_file_run_stores_the_real_boot_id() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    std::fs::write(
        repo.join("hostler.toml"),
        "[secrets.S]\npurpose = \"a test secret\"\n",
    )
    .unwrap();
    home.unlock_new("a test password, not a secret");
    home.approve(&repo);
    home.set(&repo, "S", "a value that is never printed");
    let boot = SysProcessTable::new().unwrap().boot_id();
    let copy = home.dir.path().join("owner-copy.json");
    let script = format!("cp \"$(dirname \"$S\")/owner.json\" '{}'", copy.display());
    let out = sandboxed(
        &home,
        &repo,
        &["run", "--secret-file", "S", "--", "sh", "-c", &script],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let owner: Value = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
    assert_eq!(owner["wrapper"]["boot_id"], boot.as_str(), "{owner}");
    assert_eq!(owner["child"]["boot_id"], boot.as_str(), "{owner}");
}
