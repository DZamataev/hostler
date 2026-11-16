//! `hostler run --screen` / `--account` against the real binary and service (plan P3a, Task 8):
//! joining and re-entrancy, composition with `--heavy` and `--secret`, the masking hand-off,
//! deadlocks, interruption, takeover and the exit summary. Assertion messages never print a
//! canary value.
#![cfg_attr(windows, allow(dead_code, unused_imports))]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{BIN, Home, stderr, stdout};
use hostler::client::Client;
use hostler::contract::claims::{
    Claim, ClaimHistoryItem, ClaimKind, ClaimsSnapshot, EndReason, ResourceId,
};
use hostler::contract::heavy::HistoryQuery;
use hostler::contract::wire::{Event, Request};
#[cfg(windows)]
use hostler::procs::RunGroup;

const TICK: &str = "HOSTLER_TEST_CLAIMS_TICK_MS";
const PASSWORD: &str = "master-pass-1";
/// Waits for `$GATE` to exist: a command that runs until the test lets it end.
const GATED: &str = "while [ ! -e \"$GATE\" ]; do sleep 0.05; done";

/// Starts the service with test hooks (the first start wins).
fn start_with(home: &Home, env: &[(&str, &str)]) {
    let mut command = home.hostler();
    for (name, value) in env {
        command.env(name, value);
    }
    let out = command.args(["service", "start"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}

fn fast_tick(home: &Home) {
    start_with(home, &[(TICK, "100")]);
}

/// `hostler run ARGS…` in `repo`, with `$HB` naming the binary for nested runs.
fn run(home: &Home, repo: &Path, args: &[&str]) -> Command {
    let mut c = home.hostler();
    c.arg("run")
        .args(args)
        .current_dir(repo)
        .env("HB", BIN)
        // A test run inside a hostler run must not look nested.
        .env_remove("HOSTLER_CLAIM_TOKENS")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// A gated run: `GATE` points at a file the test creates to let the command end.
fn gated(home: &Home, repo: &Path, flags: &[&str], gate: &Path) -> Child {
    let mut args = flags.to_vec();
    args.extend(["--", "sh", "-c", GATED]);
    run(home, repo, &args).env("GATE", gate).spawn().unwrap()
}

fn open(gate: &Path) {
    std::fs::write(gate, "").unwrap();
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

/// Waits until a lease on `prefix` has registered its command (`claims.started`).
fn started_lease(home: &Home, prefix: &str) -> Claim {
    let mut found = None;
    assert!(
        wait_for(Duration::from_secs(20), || {
            found = claims_on(home, prefix)
                .into_iter()
                .find(|c| c.kind == ClaimKind::Lease && c.child.is_some());
            found.is_some()
        }),
        "no started lease on {prefix}: {:?}",
        snapshot(home)
    );
    found.unwrap()
}

fn history(home: &Home) -> Vec<ClaimHistoryItem> {
    serde_json::from_value(
        home.call(Request::ClaimsHistory(HistoryQuery {
            since_ms: None,
            limit: 500,
        }))
        .unwrap(),
    )
    .unwrap()
}

fn hold_id(home: &Home, repo: &Path, args: &[&str]) -> String {
    let out = home.hold(repo, args);
    assert!(out.status.success(), "hold {args:?}: {}", stderr(&out));
    let envelope: hostler::contract::Envelope<Claim> =
        serde_json::from_str(stdout(&out).trim()).unwrap();
    envelope.data.unwrap().id
}

fn release(home: &Home, repo: &Path, id: &str) {
    let out = home
        .hostler()
        .args(["release", id])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}

/// A checkout whose project is approved, in a home with an unlocked new database (once per home).
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

fn set_account(home: &Home, repo: &Path, name: &str, field: &str, value: &str) {
    let out = with_stdin(
        home,
        repo,
        &["account", "set", name, "--field", field],
        value,
    );
    assert!(
        out.status.success(),
        "account set {name} --field {field} failed"
    );
}

/// The recorded materializations: `(names_json, command)`.
fn materializations(home: &Home) -> Vec<(String, String)> {
    let db = rusqlite::Connection::open(home.paths().db).unwrap();
    let mut query = db
        .prepare("SELECT names_json, command FROM secret_materializations")
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[cfg(unix)]
fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .args([&format!("-{name}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{name}");
}

// --- Screen and joining ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn screen_run_serializes_two_runs() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let gate = home.dir.path().join("gate");
    let first = gated(&home, &a, &["--screen"], &gate);
    started_lease(&home, "screen");
    let second = run(&home, &b, &["--screen", "--", "true"]).spawn().unwrap();
    assert!(wait_for(Duration::from_secs(10), || waiting_count(&home) == 1));
    open(&gate);
    let first = finish(first, Duration::from_secs(20));
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(
        stderr(&first).contains("[hostler] screen: released."),
        "{}",
        stderr(&first)
    );
    let second = finish(second, Duration::from_secs(20));
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    let text = stderr(&second);
    assert!(text.contains("Waiting for screen: held by "), "{text}");
    assert!(
        text.contains(
            "[hostler] To take it over: rerun with --force --confirmation screen. An agent uses \
             --force only with the operator's explicit permission."
        ),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn second_screen_run_of_one_workspace_waits_without_a_takeover_hint() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let gate = home.dir.path().join("gate");
    let first = gated(&home, &a, &["--screen"], &gate);
    started_lease(&home, "screen");
    // L13: the screen is not shared by a workspace's runs; L32: no takeover of your own run.
    let out = run(&home, &a, &["--screen", "--no-wait", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("held by this workspace's run of "), "{text}");
    assert!(!text.contains("--force"), "{text}");
    // CR-B-5: --no-wait says it did not wait, with the position, and claims no wait.
    assert!(text.contains("you would be 1 of 1"), "{text}");
    assert!(
        !text.contains("this is waiting") && !text.contains("Ctrl+C"),
        "{text}"
    );
    open(&gate);
    finish(first, Duration::from_secs(20));
}

#[cfg(unix)]
#[test]
fn run_under_own_hold_joins_and_keeps_the_hold() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let hold = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let gate = home.dir.path().join("gate");
    let child = gated(&home, &a, &["--screen"], &gate);
    let joined = started_lease(&home, "screen");
    assert_eq!(joined.parent.as_deref(), Some(hold.as_str()));
    assert!(joined.group.is_some(), "{joined:?}");
    open(&gate);
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains(&format!(
            "[hostler] screen: still held by your hold {hold} until "
        )),
        "{text}"
    );
    assert!(claims_on(&home, "screen").iter().any(|c| c.id == hold));
    release(&home, &a, &hold);
}

#[cfg(unix)]
#[test]
fn hold_released_during_the_run_changes_the_summary() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let hold = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let gate = home.dir.path().join("gate");
    let child = gated(&home, &a, &["--screen"], &gate);
    started_lease(&home, "screen");
    release(&home, &a, &hold);
    open(&gate);
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!(
            "[hostler] screen: released (your hold {hold} ended while the command ran)."
        )),
        "{}",
        stderr(&out)
    );
}

#[cfg(unix)]
#[test]
fn nested_screen_run_is_reentrant() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let out = run(
        &home,
        &a,
        &[
            "--screen",
            "--max-wait",
            "0.1",
            "--",
            "sh",
            "-c",
            "\"$HB\" run --screen -- echo inner-ran",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("inner-ran"));
    let text = stderr(&out);
    assert!(
        text.contains("[hostler] screen: held by the enclosing run."),
        "{text}"
    );
    assert!(text.contains("[hostler] screen: released."), "{text}");
    assert!(!text.contains("Waiting"), "{text}");
}

#[cfg(unix)]
#[test]
fn nested_run_under_hold_in_another_checkout_is_reentrant() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let hold = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let out = run(
        &home,
        &a,
        &[
            "--screen",
            "--max-wait",
            "0.1",
            "--",
            "sh",
            "-c",
            "cd \"$B\" && \"$HB\" run --screen -- echo inner-ran",
        ],
    )
    .env("B", &b)
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("inner-ran"));
    let text = stderr(&out);
    assert!(text.contains("held by the enclosing run."), "{text}");
    assert!(
        text.contains(&format!("still held by your hold {hold}")),
        "{text}"
    );
    release(&home, &a, &hold);
}

#[cfg(unix)]
#[test]
fn two_account_runs_of_one_workspace_share_it() {
    let home = Home::new();
    fast_tick(&home);
    let repo = approved_repo(&home, "app");
    set_account(&home, &repo, "qa-user", "session", "hlx-share-session-1");
    let gate = home.dir.path().join("gate");
    let first = gated(&home, &repo, &["--account", "qa-user"], &gate);
    let lease = started_lease(&home, "account:");
    // L13: an account is shared by its workspace's runs; the second joins at once.
    let second = run(
        &home,
        &repo,
        &["--account", "qa-user", "--no-wait", "--", "true"],
    )
    .output()
    .unwrap();
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert!(
        stderr(&second).contains(&format!(
            "[hostler] account qa-user: released; this workspace's run {} still uses it.",
            lease.id
        )),
        "{}",
        stderr(&second)
    );
    open(&gate);
    let first = finish(first, Duration::from_secs(20));
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(stderr(&first).contains("[hostler] account qa-user: released."));
}

#[cfg(unix)]
#[test]
fn account_run_of_another_workspace_waits() {
    let home = Home::new();
    fast_tick(&home);
    let main = approved_repo(&home, "app");
    let other = worktree(&home, &main, "wt");
    set_account(&home, &main, "qa-user", "session", "hlx-wait-session-2");
    let hold = hold_id(
        &home,
        &other,
        &["account", "qa-user", "--ttl", "5", "--json"],
    );
    let out = run(
        &home,
        &main,
        &["--account", "qa-user", "--max-wait", "0.02", "--", "true"],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains("Waiting for account qa-user: held by "),
        "{text}"
    );
    assert!(text.contains("--confirmation qa-user"), "{text}");
    assert!(text.contains("Gave up after"), "{text}");
    assert_eq!(waiting_count(&home), 0, "the request was not withdrawn");
    release(&home, &other, &hold);
}

// --- Composition ----------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn screen_and_heavy_compose() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let out = run(&home, &a, &["--screen", "--heavy", "--", "echo", "hi"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        history(&home)
            .iter()
            .any(|i| i.resource == "screen" && i.reason == EndReason::Released),
        "{:?}",
        history(&home)
    );
    let heavy: Vec<serde_json::Value> = serde_json::from_value(
        home.call(Request::QueueHistory(HistoryQuery {
            since_ms: None,
            limit: 50,
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(heavy.len(), 1, "{heavy:?}");
}

#[cfg(unix)]
#[test]
fn account_run_injects_masked_fields() {
    const CANARY: &str = "hlx-run-acct-7f3a";
    let home = Home::new();
    fast_tick(&home);
    let repo = approved_repo(&home, "app");
    set_account(&home, &repo, "qa-user", "session", CANARY);
    let script = format!("echo \"s=$HOSTLER_ACCOUNT_SESSION n=$HOSTLER_ACCOUNT_NAME\" # {CANARY}");
    let out = run(
        &home,
        &repo,
        &["--account", "qa-user", "--", "sh", "-c", &script],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "s=*** n=qa-user");
    assert!(!stderr(&out).contains(CANARY), "the value reached stderr");
    let rows = materializations(&home);
    let (names, command) = rows
        .iter()
        .find(|(names, _)| names.contains("account:qa-user/session"))
        .expect("an account materialization row");
    assert!(names.contains("account:qa-user/session"));
    assert!(
        !command.contains(CANARY),
        "the recorded command holds the value"
    );
    assert!(
        command.contains("***"),
        "the recorded command is not masked"
    );
    let item = history(&home)
        .into_iter()
        .find(|i| i.resource == "account qa-user")
        .expect("the account claim's history");
    let recorded = item.command.unwrap_or_default();
    assert!(
        !recorded.contains(CANARY),
        "the claim's command holds the value"
    );
    assert!(
        recorded.contains("***"),
        "the claim's command is not masked"
    );
}

#[cfg(unix)]
#[test]
fn account_and_secret_compose() {
    let home = Home::new();
    fast_tick(&home);
    let repo = approved_repo(&home, "app");
    set_account(&home, &repo, "qa-user", "session", "hlx-compose-acct-1");
    home.set(&repo, "API_KEY", "hlx-compose-secret-2");
    let out = run(
        &home,
        &repo,
        &[
            "--secret",
            "API_KEY",
            "--account",
            "qa-user",
            "--",
            "sh",
            "-c",
            "echo \"$API_KEY $HOSTLER_ACCOUNT_SESSION\"",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "*** ***");
}

// --- The masking hand-off -------------------------------------------------------------------

/// Every pushed `claims.changed` while it lives, as JSON text.
fn collect_events(home: &Home) -> (Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    home.start_service();
    let paths = home.paths();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let into = seen.clone();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut client = Client::connect(&paths).await.unwrap();
            let until = tokio::time::Instant::now() + Duration::from_secs(30);
            while let Ok(Some(event)) = tokio::time::timeout_at(until, client.next_event()).await {
                if let Event::ClaimsChanged(snapshot) = event {
                    into.lock()
                        .unwrap()
                        .push(serde_json::to_string(&snapshot).unwrap());
                }
            }
        });
    });
    (seen, handle)
}

#[cfg(unix)]
#[test]
fn claim_command_is_masked_for_other_agents() {
    const CANARY: &str = "hlx-handoff-acct-91c2";
    let home = Home::new();
    fast_tick(&home);
    let main = approved_repo(&home, "app");
    let other = worktree(&home, &main, "wt");
    set_account(&home, &main, "qa-user", "session", CANARY);
    let (events, _collector) = collect_events(&home);
    let hold = hold_id(
        &home,
        &other,
        &["account", "qa-user", "--ttl", "5", "--json"],
    );
    let gate = home.dir.path().join("gate");
    let script = format!("{GATED} # {CANARY}");
    let child = run(
        &home,
        &main,
        &["--account", "qa-user", "--", "sh", "-c", &script],
    )
    .env("GATE", &gate)
    .spawn()
    .unwrap();
    let leaks = |home: &Home| {
        serde_json::to_string(&snapshot(home))
            .unwrap()
            .contains(CANARY)
    };
    // While it waits.
    assert!(wait_for(Duration::from_secs(10), || waiting_count(&home) == 1));
    assert!(!leaks(&home), "a waiting request's command holds the value");
    release(&home, &other, &hold);
    // While it runs: the claim's command is masked.
    let lease = started_lease(&home, "account:");
    assert!(
        wait_for(Duration::from_secs(10), || {
            claims_on(&home, "account:").iter().any(|c| {
                c.id == lease.id && c.command.as_deref().is_some_and(|c| c.contains("***"))
            })
        }),
        "the running claim's command was never masked"
    );
    assert!(!leaks(&home), "a running claim's command holds the value");
    open(&gate);
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    std::thread::sleep(Duration::from_millis(300));
    let events = events.lock().unwrap().clone();
    assert!(!events.is_empty(), "no claims.changed event was captured");
    assert!(
        !events.iter().any(|e| e.contains(CANARY)),
        "a claims.changed event holds the value"
    );
    assert!(
        events
            .iter()
            .any(|e| e.contains("<command pending>") || e.contains("***")),
        "no event showed the masked command"
    );
    assert!(
        !serde_json::to_string(&history(&home))
            .unwrap()
            .contains(CANARY),
        "the claim history holds the value"
    );
}

#[cfg(unix)]
#[test]
fn secret_literal_never_reaches_the_account_history() {
    const SECRET: &str = "hlx-sec-literal-55aa";
    let home = Home::new();
    fast_tick(&home);
    let repo = approved_repo(&home, "app");
    set_account(&home, &repo, "qa-user", "session", "hlx-acct-other-66bb");
    home.set(&repo, "API_KEY", SECRET);
    let script = format!("true # {SECRET}");
    let out = run(
        &home,
        &repo,
        &[
            "--secret",
            "API_KEY",
            "--account",
            "qa-user",
            "--",
            "sh",
            "-c",
            &script,
        ],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rows = materializations(&home);
    assert!(
        rows.iter()
            .any(|(names, _)| names.contains("account:qa-user/")),
        "no account materialization row"
    );
    assert!(
        !rows.iter().any(|(_, command)| command.contains(SECRET)),
        "a materialization row holds the secret"
    );
}

// --- Deadlocks ------------------------------------------------------------------------------

/// Two runs that wait for each other: one of them is refused with `claim_deadlock` within 3 s
/// of the cycle closing at `closed` (T8-6), and the other finishes.
#[cfg(unix)]
fn assert_one_refused(closed: Instant, first: Child, second: Child) {
    let mut runs = [(first, None), (second, None)];
    let deadline = Instant::now() + Duration::from_secs(30);
    while runs.iter().any(|(_, at)| at.is_none()) {
        for (child, at) in runs.iter_mut() {
            if at.is_none() && child.try_wait().unwrap().is_some() {
                *at = Some(Instant::now());
            }
        }
        assert!(Instant::now() < deadline, "the runs did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    let ended: Vec<(Output, Instant)> = runs
        .into_iter()
        .map(|(child, at)| (child.wait_with_output().unwrap(), at.unwrap()))
        .collect();
    let text = format!("{}\n---\n{}", stderr(&ended[0].0), stderr(&ended[1].0));
    let codes: Vec<Option<i32>> = ended.iter().map(|(out, _)| out.status.code()).collect();
    assert!(codes.contains(&Some(0)), "{codes:?}: {text}");
    let (refused, at) = ended
        .iter()
        .find(|(out, _)| out.status.code() == Some(78))
        .unwrap_or_else(|| panic!("{codes:?}: {text}"));
    assert!(stderr(refused).contains("would never end"), "{text}");
    let took = at.saturating_duration_since(closed);
    assert!(
        took < Duration::from_secs(3),
        "the cycle was not refused at once: {took:?}"
    );
}

#[cfg(unix)]
#[test]
fn heavy_screen_cycle_is_refused_at_once() {
    for max_wait in ["5", "0"] {
        let home = Home::new();
        fast_tick(&home);
        let (a, b) = (home.make_repo("a"), home.make_repo("b"));
        let gate = home.dir.path().join("gate");
        // R1 holds the heavy slot and, once R2 has the screen, asks for the screen.
        let r1 = run(
            &home,
            &a,
            &[
                "--heavy",
                "--max-wait",
                max_wait,
                "--",
                "sh",
                "-c",
                &format!("{GATED}; \"$HB\" run --screen --max-wait {max_wait} -- true"),
            ],
        )
        .env("GATE", &gate)
        .spawn()
        .unwrap();
        assert!(wait_for(Duration::from_secs(10), || {
            !serde_json::from_value::<hostler::contract::heavy::QueueSnapshot>(
                home.call(Request::QueueSnapshot).unwrap(),
            )
            .unwrap()
            .running
            .is_empty()
        }));
        // R2 holds the screen and waits for the slot.
        let r2 = run(
            &home,
            &b,
            &["--screen", "--heavy", "--max-wait", max_wait, "--", "true"],
        )
        .spawn()
        .unwrap();
        started_or_waiting(&home);
        open(&gate);
        assert_one_refused(Instant::now(), r1, r2);
    }
}

/// Waits until the screen is claimed by a granted request.
fn started_or_waiting(home: &Home) {
    assert!(wait_for(Duration::from_secs(10), || !claims_on(
        home, "screen"
    )
    .is_empty()));
    std::thread::sleep(Duration::from_millis(300));
}

#[cfg(unix)]
#[test]
fn account_screen_cycle_is_refused_at_once() {
    let home = Home::new();
    fast_tick(&home);
    let main = approved_repo(&home, "app");
    let other = worktree(&home, &main, "wt");
    set_account(&home, &main, "qa-user", "session", "hlx-cycle-session-3");
    let (gate_a, gate_b) = (
        home.dir.path().join("gate-a"),
        home.dir.path().join("gate-b"),
    );
    // R1: the account, then the screen; R2: the screen, then the account.
    let r1 = run(
        &home,
        &main,
        &[
            "--account",
            "qa-user",
            "--",
            "sh",
            "-c",
            &format!("{GATED}; \"$HB\" run --screen -- true"),
        ],
    )
    .env("GATE", &gate_a)
    .spawn()
    .unwrap();
    started_lease(&home, "account:");
    let r2 = run(
        &home,
        &other,
        &[
            "--screen",
            "--",
            "sh",
            "-c",
            &format!("{GATED}; \"$HB\" run --account qa-user -- true"),
        ],
    )
    .env("GATE", &gate_b)
    .spawn()
    .unwrap();
    started_lease(&home, "screen");
    open(&gate_a);
    std::thread::sleep(Duration::from_millis(300));
    open(&gate_b);
    assert_one_refused(Instant::now(), r1, r2);
}

// --- Interruption and failure ---------------------------------------------------------------

#[cfg(unix)]
#[test]
fn ctrl_c_while_waiting_for_the_second_resource_releases_the_first() {
    let home = Home::new();
    fast_tick(&home);
    let repo = approved_repo(&home, "app");
    let b = home.make_repo("b");
    set_account(&home, &repo, "qa-user", "session", "hlx-int-session-4");
    let hold = hold_id(&home, &b, &["screen", "--ttl", "5", "--json"]);
    let marker = home.dir.path().join("ran");
    let child = run(
        &home,
        &repo,
        &[
            "--screen",
            "--account",
            "qa-user",
            "--",
            "touch",
            marker.to_str().unwrap(),
        ],
    )
    .spawn()
    .unwrap();
    // The account first (ascending id), then the screen, which waits.
    assert!(wait_for(Duration::from_secs(10), || {
        waiting_count(&home) == 1 && !claims_on(&home, "account:").is_empty()
    }));
    signal(&child, "INT");
    let out = finish(child, Duration::from_secs(10));
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(
        wait_for(Duration::from_secs(5), || {
            waiting_count(&home) == 0 && claims_on(&home, "account:").is_empty()
        }),
        "the account was kept or the request left waiting: {:?}",
        snapshot(&home)
    );
    assert!(!marker.exists(), "the command ran");
    assert!(
        stderr(&out).contains("[hostler] account qa-user: released."),
        "{}",
        stderr(&out)
    );
    release(&home, &b, &hold);
}

#[cfg(unix)]
#[test]
fn sigterm_while_waiting_exits_143() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let hold = hold_id(&home, &b, &["screen", "--ttl", "5", "--json"]);
    let child = run(&home, &a, &["--screen", "--", "true"]).spawn().unwrap();
    assert!(wait_for(Duration::from_secs(10), || waiting_count(&home) == 1));
    signal(&child, "TERM");
    let out = finish(child, Duration::from_secs(10));
    assert_eq!(out.status.code(), Some(143), "{}", stderr(&out));
    assert!(wait_for(Duration::from_secs(5), || waiting_count(&home) == 0));
    release(&home, &b, &hold);
}

#[test]
fn no_service_refuses_78_even_with_heavy() {
    let home = Home::new();
    let repo = home.make_repo("a");
    // A broken database makes every service start fail: nothing may run.
    std::fs::write(home.paths().db, b"this is not a hostler database").unwrap();
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &[
            "--screen",
            "--heavy",
            "--",
            "touch",
            marker.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(!marker.exists(), "the command ran without the service");
    assert!(stderr(&out).contains("service"), "{}", stderr(&out));
}

#[test]
fn outside_a_checkout_refuses_78() {
    let home = Home::new();
    let dir = home.dir.path().join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    let out = run(&home, &dir, &["--screen", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
}

#[test]
fn bad_account_name_and_confirmation_are_usage_errors() {
    let home = Home::new();
    let repo = home.make_repo("a");
    let out = run(&home, &repo, &["--account", "sk-LIVE-x", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!stderr(&out).contains("sk-LIVE-x"), "the name was echoed");
    let out = run(
        &home,
        &repo,
        &[
            "--screen",
            "--force",
            "--confirmation",
            "scren",
            "--",
            "true",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    // L39: --max-wait and --no-wait need something to wait for.
    for flag in [&["--no-wait"][..], &["--max-wait", "1"]] {
        let mut args = vec!["--secret", "API_KEY"];
        args.extend_from_slice(flag);
        args.extend(["--", "true"]);
        let out = run(&home, &repo, &args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
    // --force needs a claim to take over.
    let out = run(
        &home,
        &repo,
        &[
            "--heavy",
            "--force",
            "--confirmation",
            "screen",
            "--",
            "true",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[cfg(unix)]
#[test]
fn killed_after_grant_is_reaped() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &a,
        &["--screen", "--", "touch", marker.to_str().unwrap()],
    )
    .env("HOSTLER_TEST_KILL_AFTER_CLAIM_GRANT", "1")
    .output()
    .unwrap();
    assert_eq!(out.status.code(), None, "the wrapper was not killed");
    assert!(!marker.exists(), "the command ran");
    assert!(
        wait_for(Duration::from_secs(15), || {
            claims_on(&home, "screen").is_empty()
                && history(&home)
                    .iter()
                    .any(|i| i.resource == "screen" && i.reason == EndReason::Reaped)
        }),
        "the dead run's claim was not reaped: {:?}",
        snapshot(&home)
    );
}

#[cfg(unix)]
#[test]
fn exit_code_passes_through() {
    let home = Home::new();
    let a = home.make_repo("a");
    let out = run(&home, &a, &["--screen", "--", "sh", "-c", "exit 7"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    assert!(stderr(&out).contains("[hostler] screen: released."));
    let item = history(&home)
        .into_iter()
        .find(|i| i.resource == "screen")
        .unwrap();
    assert_eq!(item.exit_code, Some(7));
}

#[cfg(unix)]
#[test]
fn one_deadline_for_the_whole_run() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b, c) = (
        home.make_repo("a"),
        home.make_repo("b"),
        home.make_repo("c"),
    );
    // The heavy slot stays busy; the screen frees after 5 s of the 9 s allowed.
    let gate = home.dir.path().join("gate");
    let heavy = gated(&home, &c, &["--heavy"], &gate);
    assert!(wait_for(Duration::from_secs(10), || {
        !serde_json::from_value::<hostler::contract::heavy::QueueSnapshot>(
            home.call(Request::QueueSnapshot).unwrap(),
        )
        .unwrap()
        .running
        .is_empty()
    }));
    let hold = hold_id(&home, &b, &["screen", "--ttl", "5", "--json"]);
    let started = Instant::now();
    let child = run(
        &home,
        &a,
        &["--screen", "--heavy", "--max-wait", "0.15", "--", "true"],
    )
    .spawn()
    .unwrap();
    std::thread::sleep(Duration::from_secs(5));
    release(&home, &b, &hold);
    let out = finish(child, Duration::from_secs(30));
    let took = started.elapsed();
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    assert!(
        took < Duration::from_secs(13),
        "the slot got a deadline of its own: {took:?}"
    );
    assert!(
        stderr(&out).contains("[hostler] screen: released."),
        "{}",
        stderr(&out)
    );
    open(&gate);
    finish(heavy, Duration::from_secs(20));
}

// --- Takeover -------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn run_force_takes_the_screen_over_from_another_workspaces_run() {
    let home = Home::new();
    start_with(&home, &[(TICK, "100")]);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let b_path = home.workspace(&b).unwrap().path;
    let gate = home.dir.path().join("gate");
    let holder = gated(&home, &a, &["--screen"], &gate);
    started_lease(&home, "screen");
    let out = run(
        &home,
        &b,
        &[
            "--screen",
            "--force",
            "--confirmation",
            "screen",
            "--",
            "echo",
            "taker-ran",
        ],
    )
    .env("HOSTLER_TEST_TAKEOVER_GRACE_MS", "500")
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("taker-ran"));
    assert!(
        stderr(&out).contains("[hostler] Took screen over from "),
        "{}",
        stderr(&out)
    );
    let held = finish(holder, Duration::from_secs(20));
    assert!(
        stderr(&held).contains(&format!(
            "[hostler] screen: taken over by {b_path} while the command ran."
        )),
        "{}",
        stderr(&held)
    );
}

#[cfg(unix)]
#[test]
fn seizure_between_grant_and_spawn_never_starts_the_command() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let pause = home.dir.path().join("pause");
    let marker = home.dir.path().join("side-effect");
    let holder = run(
        &home,
        &a,
        &["--screen", "--", "touch", marker.to_str().unwrap()],
    )
    .env("HOSTLER_TEST_PAUSE_BEFORE_SPAWN_UNTIL", &pause)
    .spawn()
    .unwrap();
    let mut reached = pause.clone().into_os_string();
    reached.push(".reached");
    assert!(wait_for(Duration::from_secs(10), || Path::new(&reached).exists()));
    // The taker plans, then pauses before stopping anything: the holder resumes meanwhile.
    let taker = run(
        &home,
        &b,
        &[
            "--screen",
            "--force",
            "--confirmation",
            "screen",
            "--",
            "true",
        ],
    )
    .env("HOSTLER_TEST_SEIZE_PAUSE_MS", "4000")
    .env("HOSTLER_TEST_TAKEOVER_GRACE_MS", "500")
    .spawn()
    .unwrap();
    assert!(wait_for(Duration::from_secs(10), || {
        snapshot(&home).resources.iter().any(|s| s.seizing)
    }));
    open(&pause);
    let held = finish(holder, Duration::from_secs(20));
    assert_eq!(held.status.code(), Some(78), "{}", stderr(&held));
    assert!(
        stderr(&held)
            .contains("[hostler] screen was taken over before the command started; not running."),
        "{}",
        stderr(&held)
    );
    assert!(!marker.exists(), "the seized run's command ran");
    let taken = finish(taker, Duration::from_secs(30));
    assert_eq!(taken.status.code(), Some(0), "{}", stderr(&taken));
}

/// L28/L33: a nested claim run that starts while its enclosing run is being taken over is
/// refused (78) with the service's text and never runs its command.
#[cfg(unix)]
#[test]
fn nested_run_under_a_takeover_is_refused() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let gate = home.dir.path().join("gate");
    let marker = home.dir.path().join("inner-ran");
    let script = format!("{GATED}; \"$HB\" run --screen -- touch \"$MARK\"; sleep 30");
    let holder = run(&home, &a, &["--screen", "--", "sh", "-c", &script])
        .env("GATE", &gate)
        .env("MARK", &marker)
        .spawn()
        .unwrap();
    started_lease(&home, "screen");
    let taker = run(
        &home,
        &b,
        &[
            "--screen",
            "--force",
            "--confirmation",
            "screen",
            "--",
            "true",
        ],
    )
    .env("HOSTLER_TEST_SEIZE_PAUSE_MS", "3000")
    .env("HOSTLER_TEST_TAKEOVER_GRACE_MS", "500")
    .spawn()
    .unwrap();
    assert!(wait_for(Duration::from_secs(10), || {
        snapshot(&home).resources.iter().any(|s| s.seizing)
    }));
    open(&gate);
    let taken = finish(taker, Duration::from_secs(30));
    assert_eq!(taken.status.code(), Some(0), "{}", stderr(&taken));
    let held = finish(holder, Duration::from_secs(20));
    let text = stderr(&held);
    assert!(
        text.contains(
            "[hostler] The run this command comes from is being taken over; not running."
        ),
        "{text}"
    );
    assert!(
        !marker.exists(),
        "the nested command ran under the takeover"
    );
}

/// T8-1: a takeover with two targets stops the first run long before it commits; that run must
/// wait for the plan instead of releasing under it, and both runs say who took the screen.
#[cfg(unix)]
#[test]
fn takeover_of_two_joined_runs_reports_it_in_both() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let b_path = home.workspace(&b).unwrap().path;
    let hold = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let stubborn = format!("trap '' TERM; {GATED}");
    let never = home.dir.path().join("never");
    let joined: Vec<Child> = (0..2)
        .map(|_| {
            run(&home, &a, &["--screen", "--", "sh", "-c", &stubborn])
                .env("GATE", &never)
                .spawn()
                .unwrap()
        })
        .collect();
    assert!(wait_for(Duration::from_secs(20), || {
        claims_on(&home, "screen")
            .iter()
            .filter(|c| c.parent.as_deref() == Some(hold.as_str()) && c.child.is_some())
            .count()
            == 2
    }));
    // Each target ignores SIGTERM: 6 s of grace, then SIGKILL, one after the other; the first
    // run's command is gone about 6 s before the commit.
    let taker = run(
        &home,
        &b,
        &[
            "--screen",
            "--force",
            "--confirmation",
            "screen",
            "--",
            "true",
        ],
    )
    .env("HOSTLER_TEST_TAKEOVER_GRACE_MS", "6000")
    .output()
    .unwrap();
    assert_eq!(taker.status.code(), Some(0), "{}", stderr(&taker));
    for child in joined {
        let out = finish(child, Duration::from_secs(30));
        let text = stderr(&out);
        assert!(
            text.contains(&format!(
                "[hostler] screen: taken over by {b_path} while the command ran."
            )),
            "{text}"
        );
        assert!(
            !text.contains("still held") && !text.contains("released"),
            "{text}"
        );
    }
}

/// T8-3: with the service gone at the end, every release shares one 10 s budget, and the run
/// says it is waiting.
#[cfg(unix)]
#[test]
fn service_gone_at_the_end_exits_within_the_budget() {
    let home = Home::new();
    fast_tick(&home);
    let repo = approved_repo(&home, "app");
    set_account(&home, &repo, "qa-user", "session", "hlx-gone-session-5");
    let gate = home.dir.path().join("gate");
    let child = gated(&home, &repo, &["--screen", "--account", "qa-user"], &gate);
    started_lease(&home, "screen");
    let out = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let opened = Instant::now();
    open(&gate);
    let out = finish(child, Duration::from_secs(60));
    let took = opened.elapsed();
    let text = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(took < Duration::from_secs(14), "{took:?}: {text}");
    assert_eq!(
        text.matches("the claim ends when this process is gone.")
            .count(),
        2,
        "{text}"
    );
    assert_eq!(
        text.matches("The service is not answering; retrying the release")
            .count(),
        1,
        "{text}"
    );
}

/// T8-5: a run refused after spawning still says what its resources are left in.
#[cfg(unix)]
#[test]
fn started_failure_still_summarizes() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let pause = home.dir.path().join("pause");
    let marker = home.dir.path().join("ran");
    let child = run(
        &home,
        &a,
        &["--screen", "--", "touch", marker.to_str().unwrap()],
    )
    .env("HOSTLER_TEST_PAUSE_BEFORE_SPAWN_UNTIL", &pause)
    .spawn()
    .unwrap();
    let mut reached = pause.clone().into_os_string();
    reached.push(".reached");
    assert!(wait_for(Duration::from_secs(10), || Path::new(&reached).exists()));
    let out = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    open(&pause);
    let out = finish(child, Duration::from_secs(60));
    let text = stderr(&out);
    assert_eq!(out.status.code(), Some(78), "{text}");
    assert!(!marker.exists(), "the command ran without `claims.started`");
    assert!(
        text.contains("[hostler] screen: the service did not answer; the claim ends when this process is gone."),
        "{text}"
    );
}

/// Two runs started at the same moment in one checkout under its hold both join and run (the
/// T8 R1 probe that hit T6-R2-2).
#[cfg(unix)]
#[test]
fn two_runs_at_once_under_one_hold_both_run() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let hold = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let runs: Vec<Child> = (0..2)
        .map(|i| {
            run(&home, &a, &["--screen", "--", "echo", &format!("ran-{i}")])
                .spawn()
                .unwrap()
        })
        .collect();
    for (i, child) in runs.into_iter().enumerate() {
        let out = finish(child, Duration::from_secs(30));
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert!(stdout(&out).contains(&format!("ran-{i}")));
        assert!(
            stderr(&out).contains(&format!("still held by your hold {hold}")),
            "{}",
            stderr(&out)
        );
    }
    release(&home, &a, &hold);
}

/// L43 (CR-A-2): a `--secret` run nested in a lease registers nothing, so it runs in the lease's
/// group: a takeover's SIGKILL reaches its command, and the commit passes only once it is gone.
#[cfg(unix)]
#[test]
fn takeover_stops_a_nested_secret_run() {
    let home = Home::new();
    fast_tick(&home);
    let a = approved_repo(&home, "app");
    let b = home.make_repo("b");
    home.set(&a, "API_KEY", "hlx-nested-secret-77");
    let pid_file = home.dir.path().join("inner.pid");
    let inner = home.dir.path().join("inner.sh");
    std::fs::write(&inner, "trap '' TERM; echo $$ > \"$PIDF\"; sleep 600\n").unwrap();
    let holder = run(
        &home,
        &a,
        &[
            "--screen",
            "--",
            "sh",
            "-c",
            "\"$HB\" run --secret API_KEY -- sh \"$INNER\"",
        ],
    )
    .env("PIDF", &pid_file)
    .env("INNER", &inner)
    .spawn()
    .unwrap();
    assert!(wait_for(Duration::from_secs(20), || {
        std::fs::read_to_string(&pid_file).is_ok_and(|p| !p.trim().is_empty())
    }));
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    let taker = run(
        &home,
        &b,
        &[
            "--screen",
            "--force",
            "--confirmation",
            "screen",
            "--",
            "true",
        ],
    )
    .env("HOSTLER_TEST_TAKEOVER_GRACE_MS", "200")
    .output()
    .unwrap();
    assert_eq!(taker.status.code(), Some(0), "{}", stderr(&taker));
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&state.stdout).trim().to_owned();
    assert!(
        state.is_empty() || state.starts_with('Z'),
        "the nested command still runs ({state}) after the takeover"
    );
    finish(holder, Duration::from_secs(20));
}

/// L43: outside any hostler run a `--secret` run keeps its own process group; nested in a claim
/// run it joins that run's group.
#[cfg(unix)]
#[test]
fn a_plain_secret_run_keeps_its_own_group() {
    let home = Home::new();
    fast_tick(&home);
    let a = approved_repo(&home, "app");
    home.set(&a, "API_KEY", "hlx-group-secret-88");
    let probe = "echo \"$$ $(ps -o pgid= -p $$)\"";
    let groups = |out: &Output| -> (String, String) {
        let text = stdout(out);
        let mut words = text.split_whitespace();
        (
            words.next().unwrap_or_default().to_owned(),
            words.next().unwrap_or_default().to_owned(),
        )
    };
    let plain = run(&home, &a, &["--secret", "API_KEY", "--", "sh", "-c", probe])
        .output()
        .unwrap();
    assert_eq!(plain.status.code(), Some(0), "{}", stderr(&plain));
    let (pid, pgid) = groups(&plain);
    assert_eq!(pid, pgid, "a plain secret run lost its own group");
    let nested = run(
        &home,
        &a,
        &[
            "--screen",
            "--",
            "sh",
            "-c",
            &format!("\"$HB\" run --secret API_KEY -- sh -c '{probe}'"),
        ],
    )
    .output()
    .unwrap();
    assert_eq!(nested.status.code(), Some(0), "{}", stderr(&nested));
    let (pid, pgid) = groups(&nested);
    assert!(
        !pid.is_empty() && pid != pgid,
        "the nested run has a group of its own"
    );
}

/// CR-B-5: `hold --no-wait` behind another workspace's claim claims no wait either.
#[test]
fn hold_no_wait_claims_no_wait() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let held = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let out = home.hold(&b, &["screen", "--no-wait"]);
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("you would be 1 of 1"), "{text}");
    assert!(
        !text.contains("this is waiting") && !text.contains("Ctrl+C"),
        "{text}"
    );
    release(&home, &a, &held);
}

/// CR-B-6: a wrong `--confirmation` is refused (exit 2) even when no takeover would happen.
#[test]
fn hold_and_release_check_the_confirmation_always() {
    let home = Home::new();
    fast_tick(&home);
    let a = home.make_repo("a");
    let out = home.hold(&a, &["screen", "--force", "--confirmation", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        claims_on(&home, "screen").is_empty(),
        "the free screen was taken"
    );
    let held = hold_id(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let out = home
        .hostler()
        .args(["release", &held, "--force", "--confirmation", "nope"])
        .current_dir(&a)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(claims_on(&home, "screen").iter().any(|c| c.id == held));
    release(&home, &a, &held);
}

/// L42: `hold` and `release` without the service refuse with 78, like `run`.
#[test]
fn hold_and_release_fail_closed_with_78() {
    let home = Home::new();
    let repo = home.make_repo("a");
    std::fs::write(home.paths().db, b"this is not a hostler database").unwrap();
    let out = home.hold(&repo, &["screen"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    let out = home
        .hostler()
        .args(["release", "6f1e6a9a-2d0f-4b7c-9a41-6f0f2f7c1a11"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
}

// --- Windows --------------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn windows_claims_only_run_is_in_a_job() {
    let home = Home::new();
    let a = home.make_repo("a");
    let gate = home.dir.path().join("gate");
    let child = run(
        &home,
        &a,
        &[
            "--screen",
            "--",
            "powershell",
            "-NoProfile",
            "-Command",
            "while (-not (Test-Path $env:GATE)) { Start-Sleep -Milliseconds 50 }",
        ],
    )
    .env("GATE", &gate)
    .spawn()
    .unwrap();
    let lease = started_lease(&home, "screen");
    match lease.group {
        Some(RunGroup::WindowsJob { name, session, .. }) => {
            assert!(name.starts_with(r"Local\hostler-heavy-"), "{name}");
            assert!(session.is_some(), "no session recorded");
        }
        other => panic!("not a job: {other:?}"),
    }
    open(&gate);
    let out = finish(child, Duration::from_secs(30));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn resource_ids_sort_account_before_screen() {
    // The order runs take claims in (plan Task 8, step 4), as the contract compares ids.
    assert!(ResourceId::account("p", "qa-user") < ResourceId::screen());
}
