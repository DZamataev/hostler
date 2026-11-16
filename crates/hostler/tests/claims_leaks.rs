//! The account leak canary (plan P3a, Task 10): an account value must never appear in anything
//! hostler prints, records, broadcasts or stores — only the command's own output, masked.
//! Portable (`hold` example invocations, no shell): it runs in the galactica suite too.
//! Assertion messages never print the canary.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::client::Client;
use hostler::contract::accounts::AccountMaterialize;
use hostler::contract::claims::{Claim, ClaimKind, ClaimsSnapshot};
use hostler::contract::heavy::HistoryQuery;
use hostler::contract::wire::Request;
use hostler::contract::{ErrorCode, HostlerError};
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use hostler::secrets::value::RawCommand;
use serde_json::Value;

const CANARY: &str = "hlx-acct-canary-5d1e";
const PASSWORD: &str = "master-pass-1";
const TICK: &str = "HOSTLER_TEST_CLAIMS_TICK_MS";

/// The `hold` example: prints an environment variable and then sleeps, so the canary runs on
/// Windows too (P2's `secrets_leaks` does the same).
fn hold_exe() -> &'static Path {
    static HOLD: OnceLock<PathBuf> = OnceLock::new();
    HOLD.get_or_init(|| {
        escargot::CargoBuild::new()
            .example("hold")
            .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .run()
            .expect("building the hold example")
            .path()
            .to_path_buf()
    })
}

fn hold(args: &[&str]) -> Vec<std::ffi::OsString> {
    let mut argv = vec![hold_exe().as_os_str().to_owned()];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    argv
}

fn start_with(home: &Home, env: &[(&str, &str)]) {
    let mut command = home.hostler();
    for (name, value) in env {
        command.env(name, value);
    }
    let out = command.args(["service", "start"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}

fn run(home: &Home, repo: &Path, args: &[&str]) -> Output {
    home.hostler()
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn with_stdin(home: &Home, repo: &Path, args: &[&str], input: &str) -> Output {
    let mut child = home
        .hostler()
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// A checkout whose project is approved, in a home with an unlocked new database.
fn approved_repo(home: &Home, name: &str) -> PathBuf {
    let repo = std::fs::canonicalize(home.make_repo(name)).unwrap();
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    repo
}

/// A second workspace of `main`'s project: a git worktree laid out as git makes it.
fn worktree(home: &Home, main: &Path, name: &str) -> PathBuf {
    let tree = home.dir.path().join("repos").join(name);
    std::fs::create_dir_all(&tree).unwrap();
    let tree = std::fs::canonicalize(tree).unwrap();
    let git_dir = main.join(".git").join("worktrees").join(name);
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
    std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    std::fs::write(
        git_dir.join("gitdir"),
        format!("{}\n", tree.join(".git").display()),
    )
    .unwrap();
    std::fs::write(
        tree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    tree
}

fn envelope(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("not JSON: {}", stdout(out)))
}

fn hold_id(out: &Output) -> String {
    let value = envelope(out);
    assert_eq!(value["ok"], true, "hold refused");
    let claim: Claim = serde_json::from_value(value["data"].clone()).unwrap();
    claim.id
}

fn release(home: &Home, repo: &Path, id: &str) -> Output {
    run(home, repo, &["release", id])
}

fn snapshot(home: &Home) -> ClaimsSnapshot {
    home.snapshot()
}

fn claims_on(home: &Home, prefix: &str) -> Vec<Claim> {
    snapshot(home)
        .resources
        .into_iter()
        .filter(|s| s.resource.id.0.starts_with(prefix))
        .flat_map(|s| s.claims)
        .collect()
}

fn waiting_count(home: &Home) -> usize {
    snapshot(home)
        .resources
        .iter()
        .map(|s| s.waiting.len())
        .sum()
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn finish(mut child: Child, limit: Duration) -> Output {
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the run did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// Every event another client receives, as JSON (`queue.changed`, `claims.changed`, seizures…).
fn collect_events(home: &Home) -> Arc<Mutex<Vec<String>>> {
    home.start_service();
    let paths = home.paths();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let into = seen.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut client = Client::connect(&paths).await.unwrap();
            let until = tokio::time::Instant::now() + Duration::from_secs(30);
            while let Ok(Some(event)) = tokio::time::timeout_at(until, client.next_event()).await {
                into.lock()
                    .unwrap()
                    .push(serde_json::to_string(&event).unwrap());
            }
        });
    });
    seen
}

fn has_bytes(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn assert_clean(what: &str, bytes: &[u8]) {
    assert!(!has_bytes(bytes, CANARY), "{what} holds the canary");
}

fn files_under(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            files_under(&path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path, bytes));
        }
    }
}

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

/// `account.materialize` without a claim of our own: refused, and the refusal never echoes the
/// command it was given.
fn materialize_without_a_claim(home: &Home, repo: &Path) -> Result<Value, HostlerError> {
    home.start_service();
    let paths = home.paths();
    let project = hostler::project::current(repo).unwrap().project;
    let request = AccountMaterialize {
        run_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        requester: me(),
        project,
        name: "qa-user".into(),
        command: RawCommand::new(format!("echo {CANARY}")),
        workspace: hostler::project::display_path(repo),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        Client::connect(&paths)
            .await?
            .call_sensitive(Request::AccountMaterialize(request))
            .await
    })
}

#[test]
fn no_account_value_leaks_anywhere() {
    let home = Home::new();
    start_with(&home, &[(TICK, "100")]);
    let main = approved_repo(&home, "app");
    let other = worktree(&home, &main, "wt");
    let events = collect_events(&home);
    let mut outputs: Vec<(&str, Output)> = Vec::new();

    // A person stores the value; it goes in on stdin, never as an argument.
    let set = with_stdin(
        &home,
        &main,
        &["account", "set", "qa-user", "--field", "session"],
        CANARY,
    );
    assert_eq!(set.status.code(), Some(0), "{}", stderr(&set));
    outputs.push(("account set", set));

    let list = run(&home, &main, &["account", "list", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert_eq!(envelope(&list)["data"][0]["fields"][0], "session");
    outputs.push(("account list", list));

    let status = run(&home, &main, &["status", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(envelope(&status)["ok"], true);
    outputs.push(("status", status));

    let held = run(
        &home,
        &main,
        &["hold", "account", "qa-user", "--ttl", "5", "--json"],
    );
    assert!(held.status.success(), "{}", stderr(&held));
    let hold_id = hold_id(&held);
    outputs.push(("hold account", held));

    // Another workspace waits for the account: its command is `<command pending>` — the fields
    // are not known yet — and never the value.
    let marker = home.dir.path().join("pid");
    let waiting = home
        .hostler()
        .arg("run")
        .args(["--account", "qa-user", "--"])
        .args(hold(&[
            "--print",
            CANARY,
            "--print-env",
            "HOSTLER_ACCOUNT_SESSION",
            marker.to_str().unwrap(),
            "3000",
        ]))
        .current_dir(&other)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || waiting_count(&home) == 1),
        "the run never queued"
    );
    assert!(
        !serde_json::to_string(&snapshot(&home))
            .unwrap()
            .contains(CANARY),
        "a waiting request's command holds the value"
    );
    let status = run(&home, &main, &["status", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    outputs.push(("status while a run waits", status));

    let released = release(&home, &main, &hold_id);
    assert!(released.status.success(), "{}", stderr(&released));
    outputs.push(("release", released));

    // It runs: the claim's command is now masked with the field value.
    assert!(
        wait_for(Duration::from_secs(20), || {
            claims_on(&home, "account:").iter().any(|c| {
                c.kind == ClaimKind::Lease
                    && c.command.as_deref().is_some_and(|c| c.contains("***"))
            })
        }),
        "the running claim's command was never masked"
    );
    assert!(
        !serde_json::to_string(&snapshot(&home))
            .unwrap()
            .contains(CANARY),
        "a running claim's command holds the value"
    );
    let status = run(&home, &main, &["status", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    outputs.push(("status while a run runs", status));

    let ran = finish(waiting, Duration::from_secs(20));
    assert_eq!(ran.status.code(), Some(0), "{}", stderr(&ran));
    assert!(marker.exists(), "the command never ran");
    assert!(
        stdout(&ran).contains("***"),
        "the child's value was not masked: {}",
        stdout(&ran)
    );
    assert!(
        !stdout(&ran).contains(CANARY),
        "the child's output holds the value"
    );
    outputs.push(("run --account", ran));

    // A heavy run whose command line holds the value: the heavy display is masked too.
    let heavy = home
        .hostler()
        .arg("run")
        .args(["--heavy", "--account", "qa-user", "--"])
        .args(hold(&["--print", CANARY]))
        .current_dir(&main)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(heavy.status.code(), Some(0), "{}", stderr(&heavy));
    assert!(
        stdout(&heavy).contains("***"),
        "the heavy run's output was not masked"
    );
    outputs.push(("run --heavy --account", heavy));

    let history = run(&home, &main, &["queue", "history", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let value = envelope(&history);
    assert!(
        serde_json::to_string(&value).unwrap().contains("***"),
        "the heavy history has no masked command"
    );
    outputs.push(("queue history", history));

    let history: Value = home
        .call(Request::ClaimsHistory(HistoryQuery {
            since_ms: None,
            limit: 100,
        }))
        .unwrap();
    assert!(
        !serde_json::to_string(&history).unwrap().contains(CANARY),
        "the claims history holds the value"
    );
    assert!(
        serde_json::to_string(&history).unwrap().contains("***"),
        "the claims history has no masked command"
    );

    // A value passed as an argument is a usage error and is never echoed.
    let argument = run(
        &home,
        &main,
        &["account", "set", "qa-user", "--field", "env", CANARY],
    );
    assert_eq!(argument.status.code(), Some(2));
    outputs.push(("account set with a value as an argument", argument));

    // A materialize without a claim is refused; the refusal never repeats its command.
    let refused = materialize_without_a_claim(&home, &main).unwrap_err();
    assert_eq!(refused.code, ErrorCode::ClaimNotHeld);
    assert!(!format!("{refused:?}").contains(CANARY));

    std::thread::sleep(Duration::from_millis(300));
    let frames = events.lock().unwrap().clone();
    assert!(!frames.is_empty(), "no event was captured");
    assert!(
        !frames.iter().any(|frame| frame.contains(CANARY)),
        "an event holds the value"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("<command pending>")),
        "no event showed the pending command"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("queue.changed") && frame.contains("***")),
        "a queue.changed event never showed the masked heavy command"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("claims.changed") && frame.contains("***")),
        "a claims.changed event never showed the masked command"
    );

    for (what, out) in &outputs {
        assert_clean(&format!("{what} stdout"), &out.stdout);
        assert_clean(&format!("{what} stderr"), &out.stderr);
    }

    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(stop.status.success(), "{}", stderr(&stop));
    outputs.push(("service stop", stop));
    let (what, out) = outputs.last().unwrap();
    assert_clean(&format!("{what} stdout"), &out.stdout);
    assert_clean(&format!("{what} stderr"), &out.stderr);

    let paths = home.paths();
    let mut files = Vec::new();
    files_under(&paths.log_dir, &mut files);
    files_under(&paths.runs, &mut files);
    for name in ["hostler.db", "hostler.db-wal", "hostler.db-shm"] {
        let db = paths.home.join(name);
        if let Ok(bytes) = std::fs::read(&db) {
            files.push((db, bytes));
        }
    }
    for (path, bytes) in &files {
        assert_clean(&path.display().to_string(), bytes);
    }
}
