//! `hostler hold | release | hold renew` against the real binary and service (plan P3a, Task 7).
//! A waiting hold is a real child process: Ctrl+C/SIGTERM/SIGKILL and TTL expiry are exercised on
//! it, and the service's tick is shortened through `HOSTLER_TEST_CLAIMS_TICK_MS`.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use common::BIN;
use common::{Home, stderr, stdout};
#[cfg(unix)]
use hostler::contract::claims::SeizureRecord;
use hostler::contract::claims::{
    Claim, ClaimHistoryItem, ClaimKind, ClaimOutcome, ClaimRelease, ClaimRequest, ClaimsSnapshot,
    EndReason, Purpose, ResourceId, ResourceKind, Selector,
};
use hostler::contract::heavy::HistoryQuery;
use hostler::contract::wire::Request;
use hostler::contract::{Envelope, ErrorCode};
#[cfg(unix)]
use hostler::procs::identity_of;
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use serde_json::Value;

const TICK: &str = "HOSTLER_TEST_CLAIMS_TICK_MS";
const PASSWORD: &str = "master-pass-1";

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

/// Starts the service with test hooks (the first `start` wins; later calls are no-ops).
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

fn hold_json(home: &Home, repo: &Path, args: &[&str]) -> (Output, Claim) {
    let out = home.hold(repo, args);
    assert!(out.status.success(), "hold {args:?}: {}", stderr(&out));
    let envelope: Envelope<Claim> =
        serde_json::from_str(stdout(&out).trim()).expect("a claim envelope");
    assert!(envelope.ok, "{:?}", envelope.error);
    (out, envelope.data.expect("a claim"))
}

fn envelope<T: serde::de::DeserializeOwned>(out: &Output) -> Envelope<T> {
    serde_json::from_str(stdout(out).trim()).expect("an envelope")
}

fn release(home: &Home, repo: &Path, args: &[&str]) -> Output {
    home.hostler()
        .arg("release")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn spawn_hold(home: &Home, repo: &Path, args: &[&str]) -> Child {
    home.hostler()
        .arg("hold")
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn finish(mut child: Child, limit: Duration) -> Output {
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the child did not exit within {limit:?}");
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

fn claims_on(home: &Home, resource: &str) -> Vec<Claim> {
    snapshot(home)
        .resources
        .into_iter()
        .filter(|state| state.resource.id.0 == resource)
        .flat_map(|state| state.claims)
        .collect()
}

fn waiting_count(home: &Home) -> usize {
    snapshot(home)
        .resources
        .iter()
        .map(|state| state.waiting.len())
        .sum()
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

fn ended(home: &Home, id: &str) -> Option<EndReason> {
    history(home)
        .into_iter()
        .find(|item| item.id == id)
        .map(|item| item.reason)
}

fn acquire(home: &Home, request: &ClaimRequest) -> ClaimOutcome {
    serde_json::from_value(
        home.call(Request::ClaimsAcquire(request.clone()))
            .expect("claims.acquire"),
    )
    .unwrap()
}

fn granted(outcome: ClaimOutcome) -> Claim {
    match outcome {
        ClaimOutcome::Granted { claim } | ClaimOutcome::Joined { claim } => claim,
        other => panic!("not granted: {other:?}"),
    }
}

// --- Basics ---------------------------------------------------------------------------------

#[test]
fn hold_and_release_screen() {
    let home = Home::new();
    let repo = home.make_repo("a");
    let (_out, claim) = hold_json(&home, &repo, &["screen", "--ttl", "1", "--json"]);
    assert_eq!(claim.kind, ClaimKind::Hold);
    assert_eq!(claim.resource.id, ResourceId::screen());
    assert_eq!(claim.resource.display, "screen");
    assert!(matches!(
        claim.binding,
        Some(hostler::contract::claims::HoldBinding::Ttl { .. })
    ));
    assert_eq!(claims_on(&home, "screen").len(), 1);

    let out = release(&home, &repo, &[&claim.id]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(claims_on(&home, "screen").is_empty());
    assert_eq!(ended(&home, &claim.id), Some(EndReason::Released));
}

#[test]
fn hold_json_is_the_claim() {
    let home = Home::new();
    let repo = home.make_repo("a");
    let out = home.hold(&repo, &["screen", "--ttl", "1", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["data"]["resource"]["display"], "screen", "{value}");
    assert_eq!(value["data"]["kind"], "hold", "{value}");
    assert!(value["data"]["id"].is_string(), "{value}");
}

#[test]
fn second_hold_from_another_workspace_waits_then_gets_it() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, first) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);

    let mut waiter = spawn_hold(&home, &b, &["screen", "--ttl", "5"]);
    assert!(
        wait_for(Duration::from_secs(10), || waiting_count(&home) > 0),
        "the second hold never queued"
    );
    assert!(
        waiter.try_wait().unwrap().is_none(),
        "the second hold did not wait"
    );

    let out = release(&home, &a, &[&first.id]);
    assert!(out.status.success(), "{}", stderr(&out));
    let done = finish(waiter, Duration::from_secs(15));
    assert!(done.status.success(), "{}", stderr(&done));
    let claims = claims_on(&home, "screen");
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_ne!(claims[0].id, first.id);
    let out = release(&home, &b, &[&claims[0].id]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn max_wait_exits_75_and_withdraws() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, _held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let started = Instant::now();
    let out = home.hold(&b, &["screen", "--max-wait", "0.05"]);
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert!(stderr(&out).contains("Gave up after"), "{}", stderr(&out));
    assert!(
        wait_for(Duration::from_secs(5), || waiting_count(&home) == 0),
        "the timed-out request was not withdrawn"
    );
}

#[test]
fn no_wait_exits_75_with_position() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, _held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let out = home.hold(&b, &["screen", "--no-wait"]);
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("1 of"), "{text}");
    assert!(text.contains("Not waiting (--no-wait)"), "{text}");
    assert_eq!(
        waiting_count(&home),
        0,
        "no request may queue under --no-wait"
    );
}

// --- Withdrawal -----------------------------------------------------------------------------

#[cfg(unix)]
fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .args([&format!("-{name}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{name}");
}

#[cfg(unix)]
#[test]
fn ctrl_c_withdraws() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, _held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let waiter = spawn_hold(&home, &b, &["screen", "--ttl", "5"]);
    assert!(wait_for(Duration::from_secs(10), || waiting_count(&home) > 0));
    signal(&waiter, "INT");
    let done = finish(waiter, Duration::from_secs(10));
    assert_eq!(done.status.code(), Some(130), "{}", stderr(&done));
    assert!(
        wait_for(Duration::from_secs(5), || waiting_count(&home) == 0),
        "the interrupted request was not withdrawn"
    );
}

#[cfg(unix)]
#[test]
fn sigterm_withdraws() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, _held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let waiter = spawn_hold(&home, &b, &["screen", "--ttl", "5"]);
    assert!(wait_for(Duration::from_secs(10), || waiting_count(&home) > 0));
    signal(&waiter, "TERM");
    let done = finish(waiter, Duration::from_secs(10));
    assert_eq!(done.status.code(), Some(143), "{}", stderr(&done));
    assert!(wait_for(Duration::from_secs(5), || waiting_count(&home) == 0));
}

/// A waiter killed outright is reaped before its turn: SIGKILL on Unix, `TerminateProcess` on
/// Windows (the e2e-on-Windows path). The grant never happens.
fn killed_waiter_is_reaped_and_never_granted() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let b_workspace = home.workspace(&b).unwrap().id;
    let (_, held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let mut waiter = spawn_hold(&home, &b, &["screen", "--ttl", "5"]);
    assert!(wait_for(Duration::from_secs(10), || waiting_count(&home) > 0));
    waiter.kill().unwrap();
    waiter.wait().unwrap();
    let out = release(&home, &a, &[&held.id]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        wait_for(Duration::from_secs(10), || {
            waiting_count(&home) == 0 && claims_on(&home, "screen").is_empty()
        }),
        "a dead waiter was granted or kept queued: {:?}",
        snapshot(&home)
    );
    assert!(
        !snapshot(&home)
            .resources
            .iter()
            .flat_map(|state| state.claims.iter())
            .any(|claim| claim.workspace.id == b_workspace),
        "a dead waiter's workspace got the screen"
    );
}

#[cfg(unix)]
#[test]
fn sigkilled_waiter_is_reaped_and_never_granted() {
    killed_waiter_is_reaped_and_never_granted();
}

/// The Windows variant: `Child::kill` is `TerminateProcess`.
#[cfg(windows)]
#[test]
fn terminated_waiter_is_reaped_and_never_granted() {
    killed_waiter_is_reaped_and_never_granted();
}

// --- TTL and binding ------------------------------------------------------------------------

#[test]
fn ttl_expiry_frees_it() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, first) = hold_json(&home, &a, &["screen", "--ttl", "0.02", "--json"]);
    let (_, second) = hold_json(
        &home,
        &b,
        &["screen", "--ttl", "1", "--max-wait", "0.2", "--json"],
    );
    assert_ne!(first.id, second.id);
    assert!(claims_on(&home, "screen").iter().any(|c| c.id == second.id));
    let out = release(&home, &b, &[&second.id]);
    assert!(out.status.success(), "{}", stderr(&out));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn renew_extends() {
    let home = Home::new();
    fast_tick(&home);
    let repo = home.make_repo("a");
    // 12 s: a loaded machine still has time to renew five seconds in.
    let (_, first) = hold_json(&home, &repo, &["screen", "--ttl", "0.2", "--json"]);
    let old_expiry = match first.binding {
        Some(hostler::contract::claims::HoldBinding::Ttl { expires_at_ms, .. }) => expires_at_ms,
        other => panic!("{other:?}"),
    };
    // Five seconds after the grant; no bare sleep races the renew.
    while now_ms() < first.granted_at_ms + 5_000 {
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = home
        .hostler()
        .args(["hold", "renew", &first.id, "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let renewed = envelope::<Claim>(&out).data.unwrap();
    let new_expiry = match renewed.binding {
        Some(hostler::contract::claims::HoldBinding::Ttl { expires_at_ms, .. }) => expires_at_ms,
        other => panic!("{other:?}"),
    };
    assert!(new_expiry > old_expiry, "{new_expiry} <= {old_expiry}");
    // Renewed at grant+5 s, so the new expiry is about four seconds beyond the first one.
    while now_ms() < old_expiry + 1_000 {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        claims_on(&home, "screen").iter().any(|c| c.id == first.id),
        "the renewed hold expired with its first TTL"
    );
    let out = release(&home, &repo, &[&first.id]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[cfg(unix)]
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(unix)]
#[test]
fn while_parent_hold_ends_with_the_parent() {
    let home = Home::new();
    fast_tick(&home);
    let repo = home.make_repo("a");
    let script = format!(
        "{} hold screen --while-parent --json; sleep 2",
        shell_quote(BIN)
    );
    let out = Command::new("sh")
        .args(["-c", &script])
        .current_dir(&repo)
        .env("HOSTLER_HOME", &home.home)
        .env(
            "HOSTLER_LOOKY_LEDGER",
            home.home.join("no-looky-ledger.json"),
        )
        .env("HOSTLER_TEST_FAST_KDF", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let claim: Claim = envelope(&out).data.expect("a claim envelope");
    assert!(matches!(
        claim.binding,
        Some(hostler::contract::claims::HoldBinding::Process { .. })
    ));
    // `sh` sleeps 2 s and exits; the tick reaps the process-bound hold.
    assert!(
        wait_for(Duration::from_secs(10), || {
            ended(&home, &claim.id) == Some(EndReason::Reaped)
        }),
        "the hold did not end with its parent: {:?}",
        ended(&home, &claim.id)
    );
}

#[test]
fn while_pid_1_is_refused() {
    let home = Home::new();
    let repo = home.make_repo("a");
    let out = home.hold(&repo, &["screen", "--while-pid", "1"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("must name a process that runs this hostler command"),
        "{}",
        stderr(&out)
    );
}

// --- Sharing on the fake device catalog (through the service) --------------------------------

fn any_device(workspace_id: &str, purpose: Purpose) -> ClaimRequest {
    ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Any {
            kind: ResourceKind::Device,
            attributes: Default::default(),
        },
        workspace_id: workspace_id.into(),
        requester: me(),
        purpose,
        reentrant_tokens: Vec::new(),
        no_wait: false,
    }
}

fn ttl_hold() -> Purpose {
    Purpose::Hold {
        ttl_ms: Some(60_000),
        bound_to: None,
        distinct: false,
    }
}

#[test]
fn second_device_hold_of_a_workspace_joins() {
    let home = Home::new();
    start_with(&home, &[(TICK, "100"), ("HOSTLER_TEST_FAKE_CATALOG", "2")]);
    let repo = home.make_repo("a");
    let workspace = home.workspace(&repo).unwrap().id;
    let first = granted(acquire(&home, &any_device(&workspace, ttl_hold())));
    let second = acquired_joined(acquire(&home, &any_device(&workspace, ttl_hold())));
    assert_eq!(second.parent.as_deref(), Some(first.id.as_str()));
    assert_eq!(second.resource.id, first.resource.id);
    assert_eq!(
        second.workspace.id, workspace,
        "a join belongs to the joining workspace"
    );
}

fn acquired_joined(outcome: ClaimOutcome) -> Claim {
    match outcome {
        ClaimOutcome::Joined { claim } => claim,
        other => panic!("not joined: {other:?}"),
    }
}

#[test]
fn distinct_device_holds_get_distinct_resources() {
    let home = Home::new();
    start_with(&home, &[(TICK, "100"), ("HOSTLER_TEST_FAKE_CATALOG", "2")]);
    let repo = home.make_repo("a");
    let workspace = home.workspace(&repo).unwrap().id;
    let distinct = || Purpose::Hold {
        ttl_ms: Some(60_000),
        bound_to: None,
        distinct: true,
    };
    let first = granted(acquire(&home, &any_device(&workspace, distinct())));
    let second = granted(acquire(&home, &any_device(&workspace, distinct())));
    assert_ne!(first.resource.id, second.resource.id);
    assert!(matches!(
        acquire(&home, &any_device(&workspace, distinct())),
        ClaimOutcome::Queued { .. }
    ));
}

// --- Releasing ------------------------------------------------------------------------------

#[test]
fn release_of_another_workspaces_hold_needs_force() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);

    let refused = release(&home, &b, &[&held.id]);
    assert_eq!(refused.status.code(), Some(78), "{}", stderr(&refused));
    let text = stderr(&refused);
    assert!(
        text.contains(&format!(
            "To take it over: hostler release {} --force --confirmation screen",
            held.id
        )),
        "{text}"
    );
    assert!(
        text.contains("An agent uses --force only with the operator's explicit permission."),
        "{text}"
    );
    assert_eq!(claims_on(&home, "screen").len(), 1);

    let taken = release(
        &home,
        &b,
        &[&held.id, "--force", "--confirmation", "screen"],
    );
    assert!(taken.status.success(), "{}", stderr(&taken));
    assert!(claims_on(&home, "screen").is_empty());
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
}

#[cfg(unix)]
fn spawn_holder() -> std::process::Child {
    Command::new("sleep")
        .arg("300")
        .stdin(Stdio::null())
        .spawn()
        .unwrap()
}

/// A live screen lease of `repo`'s workspace, held by a real process; killed on drop.
#[cfg(unix)]
struct Leased {
    holder: std::process::Child,
    claim: Claim,
}

#[cfg(unix)]
impl Drop for Leased {
    fn drop(&mut self) {
        let _ = self.holder.kill();
        let _ = self.holder.wait();
    }
}

#[cfg(unix)]
fn lease_screen(home: &Home, repo: &Path) -> Leased {
    let workspace = home.workspace(repo).unwrap().id;
    let holder = spawn_holder();
    let holder_id = identity_of(&SysProcessTable::new().unwrap(), holder.id()).unwrap();
    let request = ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId::screen(),
        },
        workspace_id: workspace,
        requester: holder_id,
        purpose: Purpose::Lease {
            command: "sleep 300".into(),
        },
        reentrant_tokens: Vec::new(),
        no_wait: false,
    };
    Leased {
        claim: granted(acquire(home, &request)),
        holder,
    }
}

#[cfg(unix)]
#[test]
fn forced_release_of_a_lease_ends_it_without_stopping() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let mut leased = lease_screen(&home, &a);
    let out = release(
        &home,
        &b,
        &[
            &leased.claim.id,
            "--force",
            "--confirmation",
            "screen",
            "--json",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let envelope: Envelope<SeizureRecord> = envelope(&out);
    let record = envelope.data.expect("a seizure record");
    assert!(
        record.forced,
        "the seizure was not recorded as forced: {record:?}"
    );
    assert_eq!(record.from_kind, ClaimKind::Lease);
    assert_eq!(record.resource, "screen");
    assert!(record.from_pid.is_some(), "{record:?}");
    assert_eq!(ended(&home, &leased.claim.id), Some(EndReason::Seized));
    assert!(claims_on(&home, "screen").is_empty());
    // The holder's command was not stopped (L23).
    assert!(
        leased.holder.try_wait().unwrap().is_none(),
        "the holder's command was stopped"
    );
    assert!(stdout(&out).contains("\"forced\":true"), "{}", stdout(&out));
}

#[cfg(unix)]
#[test]
fn release_of_a_lease_names_the_run_for_each_workspace() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let leased = lease_screen(&home, &a);

    // This workspace's own run: it ends with its command; a stuck one may be force-released.
    let own = release(&home, &a, &[&leased.claim.id]);
    assert_eq!(own.status.code(), Some(78), "{}", stderr(&own));
    let text = stderr(&own);
    assert!(text.contains("this workspace's run"), "{text}");
    assert!(text.contains("ends when that run ends"), "{text}");
    assert!(
        text.contains(&format!(
            "hostler release {} --force --confirmation screen",
            leased.claim.id
        )),
        "{text}"
    );
    assert!(
        text.contains("An agent uses --force only with the operator's explicit permission."),
        "{text}"
    );
    assert!(
        !text.contains("To take it over"),
        "an own claim is not a takeover: {text}"
    );

    // Another workspace's run: the command that stops it and runs the caller's command.
    let other = release(&home, &b, &[&leased.claim.id]);
    assert_eq!(other.status.code(), Some(78), "{}", stderr(&other));
    let text = stderr(&other);
    assert!(
        text.contains("hostler run --screen --force --confirmation screen -- <command>"),
        "{text}"
    );
    assert!(
        text.contains("An agent uses --force only with the operator's explicit permission."),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn a_wrong_confirmation_is_refused() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let wrong = release(
        &home,
        &b,
        &[&held.id, "--force", "--confirmation", "SCREEN"],
    );
    assert_eq!(wrong.status.code(), Some(2), "{}", stderr(&wrong));
    assert!(
        stderr(&wrong).contains("--confirmation must be screen"),
        "{}",
        stderr(&wrong)
    );
    assert_eq!(claims_on(&home, "screen").len(), 1);

    let leased = lease_screen(&home, &a);
    let wrong = release(
        &home,
        &b,
        &[&leased.claim.id, "--force", "--confirmation", "other"],
    );
    assert_eq!(wrong.status.code(), Some(2), "{}", stderr(&wrong));
    assert!(
        stderr(&wrong).contains("--confirmation must be screen"),
        "{}",
        stderr(&wrong)
    );
    assert!(
        claims_on(&home, "screen")
            .iter()
            .any(|c| c.id == leased.claim.id)
    );
}

#[test]
fn reentrant_hold_names_the_enclosing_run() {
    let home = Home::new();
    fast_tick(&home);
    let repo = home.make_repo("a");
    let workspace = home.workspace(&repo).unwrap().id;
    // The test process is the holder, so it is an ancestor of the hold it spawns.
    let request = ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId::screen(),
        },
        workspace_id: workspace.clone(),
        requester: me(),
        purpose: Purpose::Lease {
            command: "the enclosing test run".into(),
        },
        reentrant_tokens: Vec::new(),
        no_wait: false,
    };
    let lease = granted(acquire(&home, &request));

    let out = home
        .hostler()
        .env("HOSTLER_CLAIM_TOKENS", &lease.id)
        .args(["hold", "screen"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "screen is already claimed by your run {}",
            lease.id
        )),
        "{text}"
    );
    assert!(!text.contains("until released"), "{text}");
    assert!(!text.contains("Release it with"), "{text}");

    // The plan's envelope: the enclosing run's claim.
    let out = home
        .hostler()
        .env("HOSTLER_CLAIM_TOKENS", &lease.id)
        .args(["hold", "screen", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let envelope: Envelope<Claim> = envelope(&out);
    assert_eq!(envelope.data.as_ref().map(|c| &c.id), Some(&lease.id));
    assert_eq!(
        envelope.data.as_ref().map(|c| c.kind),
        Some(ClaimKind::Lease)
    );

    let released = home.call(Request::ClaimsRelease(ClaimRelease {
        claim_id: lease.id,
        workspace_id: workspace,
        requester: me(),
        exit_code: None,
    }));
    assert!(released.is_ok(), "{released:?}");
}

#[test]
fn hold_force_takes_over_another_workspaces_hold() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let (_, taken) = hold_json(
        &home,
        &b,
        &[
            "screen",
            "--ttl",
            "5",
            "--force",
            "--confirmation",
            "screen",
            "--json",
        ],
    );
    assert_ne!(taken.id, held.id);
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
    let claims = claims_on(&home, "screen");
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].id, taken.id);
    // `--no-wait` does not defer an immediate takeover.
    let (_, back) = hold_json(
        &home,
        &a,
        &[
            "screen",
            "--ttl",
            "5",
            "--force",
            "--confirmation",
            "screen",
            "--no-wait",
            "--json",
        ],
    );
    assert_ne!(back.id, taken.id);
    assert_eq!(ended(&home, &taken.id), Some(EndReason::Seized));
    let out = release(&home, &a, &[&back.id]);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// A failed `hold --force` takeover withdraws the taker's request and exits 78 (the task's
/// addition to Task 6's takeover).
#[cfg(unix)]
#[test]
fn failed_forced_hold_withdraws_and_exits_78() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let workspace = home.workspace(&a).unwrap().id;
    let mut holder = spawn_holder();
    let holder_id = identity_of(&SysProcessTable::new().unwrap(), holder.id()).unwrap();
    let request = ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId::screen(),
        },
        workspace_id: workspace,
        requester: holder_id,
        purpose: Purpose::Lease {
            command: "sleep 300".into(),
        },
        reentrant_tokens: Vec::new(),
        no_wait: false,
    };
    let claim = granted(acquire(&home, &request));

    let out = home
        .hostler()
        .env("HOSTLER_TEST_STOPPER_FAILS", "1")
        .args(["hold", "screen", "--force", "--confirmation", "screen"])
        .current_dir(&b)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("stays with its holder"),
        "{}",
        stderr(&out)
    );
    assert!(
        wait_for(Duration::from_secs(5), || waiting_count(&home) == 0),
        "the refused takeover's request was not withdrawn"
    );
    assert_eq!(
        claims_on(&home, "screen")
            .iter()
            .map(|c| &c.id)
            .collect::<Vec<_>>(),
        vec![&claim.id]
    );
    let _ = holder.kill();
    let _ = holder.wait();
}

/// T10-1/L40: a refused takeover through `release --force` exits 78, as through `hold --force`,
/// and the hold stays with its workspace.
#[cfg(unix)]
#[test]
fn failed_forced_release_exits_78() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, held) = hold_json(&home, &a, &["screen", "--ttl", "30", "--json"]);
    // A joined run under the hold: the takeover has a process to stop.
    let mut joined = home
        .hostler()
        .args(["run", "--screen", "--", "sleep", "30"])
        .current_dir(&a)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(20), || {
            claims_on(&home, "screen")
                .iter()
                .any(|c| c.kind == ClaimKind::Lease && c.child.is_some())
        }),
        "the joined run never started"
    );

    let out = home
        .hostler()
        .env("HOSTLER_TEST_STOPPER_FAILS", "1")
        .args(["release", &held.id, "--force", "--confirmation", "screen"])
        .current_dir(&b)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("stays with its holder"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        claims_on(&home, "screen").len(),
        2,
        "the refused takeover moved a claim"
    );
    assert_eq!(ended(&home, &held.id), None, "the hold ended");

    let _ = joined.kill();
    let _ = joined.wait();
}

/// L40: a claim command outside a git checkout exits 78.
#[test]
fn not_in_project_exits_78() {
    let home = Home::new();
    let plain = home.dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = home.hold(&plain, &["screen"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("not inside a git checkout"),
        "{}",
        stderr(&out)
    );
    let out = release(&home, &plain, &["some-claim-id"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
}

// --- Accounts and failure -------------------------------------------------------------------

fn approved_repo(home: &Home, name: &str) -> PathBuf {
    let repo = std::fs::canonicalize(home.make_repo(name)).unwrap();
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    repo
}

fn set_account(home: &Home, repo: &Path, name: &str, field: &str, value: &str) {
    let mut child = home
        .hostler()
        .args(["account", "set", name, "--field", field])
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
        .write_all(value.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    // Never print the value.
    assert!(out.status.success(), "account set {name}.{field} failed");
}

#[test]
fn account_hold_refused_when_locked_or_missing() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    set_account(&home, &repo, "qa-user", "session", "session-value-1");

    let (_, claim) = hold_json(
        &home,
        &repo,
        &["account", "qa-user", "--ttl", "1", "--json"],
    );
    assert_eq!(claim.kind, ClaimKind::Hold);
    assert_eq!(
        claim.resource.kind,
        ResourceKind::Account,
        "{:?}",
        claim.resource
    );
    assert!(
        claim.resource.id.0.ends_with("/qa-user"),
        "{}",
        claim.resource.id.0
    );
    // L13: a second hold of the same workspace joins the first one.
    let joined = home.hold(&repo, &["account", "qa-user", "--ttl", "1"]);
    assert!(joined.status.success(), "{}", stderr(&joined));
    let text = stdout(&joined);
    assert!(
        text.contains(&format!("(joined this workspace's hold {})", claim.id)),
        "{text}"
    );
    let joined_claim = claims_on(&home, &claim.resource.id.0)
        .into_iter()
        .find(|c| c.parent.as_deref() == Some(claim.id.as_str()))
        .expect("the joined hold");
    let out = release(&home, &repo, &[&joined_claim.id]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = release(&home, &repo, &[&claim.id]);
    assert!(out.status.success(), "{}", stderr(&out));

    let missing = home.hold(&repo, &["account", "nope", "--ttl", "1", "--json"]);
    assert_eq!(missing.status.code(), Some(78), "{}", stderr(&missing));
    let missing_envelope: Envelope<Claim> = envelope(&missing);
    assert_eq!(
        missing_envelope.error.as_ref().map(|e| e.code),
        Some(ErrorCode::AccountMissing),
        "{:?}",
        missing_envelope.error
    );

    let locked = home.hostler().arg("lock").output().unwrap();
    assert!(locked.status.success(), "{}", stderr(&locked));
    let refused = home.hold(&repo, &["account", "qa-user", "--ttl", "1", "--json"]);
    assert_eq!(refused.status.code(), Some(78), "{}", stderr(&refused));
    let refused_envelope: Envelope<Claim> = envelope(&refused);
    assert_eq!(
        refused_envelope.error.as_ref().map(|e| e.code),
        Some(ErrorCode::SecretsLocked),
        "{:?}",
        refused_envelope.error
    );
}

#[test]
fn no_service_fails_closed() {
    let home = Home::new();
    let repo = home.make_repo("a");
    let paths = home.paths();
    // A broken database makes every service start fail: the command must refuse, not run on.
    std::fs::write(&paths.db, b"this is not a hostler database").unwrap();
    let out = home.hold(&repo, &["screen"]);
    // Fail closed with `run`'s refusal code.
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("service"), "{text}");
    assert!(
        text.contains("hostler service start"),
        "no recovery command in {text}"
    );
}

// --- Takeover deferred by Task 6 ------------------------------------------------------------

#[test]
fn stranded_hold_is_taken_without_force() {
    let home = Home::new();
    // L41: a gone path strands after 2 minutes in a row; 0.5 s here.
    start_with(
        &home,
        &[(TICK, "100"), ("HOSTLER_TEST_PATH_GONE_MS", "500")],
    );
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, stranded) = hold_json(&home, &a, &["screen", "--ttl", "10", "--json"]);
    std::fs::remove_dir_all(&a).unwrap();

    let waiter = spawn_hold(
        &home,
        &b,
        &["screen", "--ttl", "1", "--max-wait", "0.2", "--json"],
    );
    let out = finish(waiter, Duration::from_secs(20));
    assert!(out.status.success(), "{}", stderr(&out));
    let taken: Claim = envelope(&out).data.expect("a claim envelope");
    assert_ne!(taken.id, stranded.id);
    assert_eq!(
        ended(&home, &stranded.id),
        Some(EndReason::Stranded),
        "the hold of a gone workspace was not reaped as stranded"
    );
    let out = release(&home, &b, &[&taken.id]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn without_force_the_refusal_names_the_command_and_the_sentence() {
    let home = Home::new();
    fast_tick(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let (_, _held) = hold_json(&home, &a, &["screen", "--ttl", "5", "--json"]);
    let out = home.hold(&b, &["screen", "--no-wait"]);
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains("hostler hold screen --force --confirmation screen"),
        "{text}"
    );
    assert!(
        text.contains("An agent uses --force only with the operator's explicit permission."),
        "{text}"
    );
}
