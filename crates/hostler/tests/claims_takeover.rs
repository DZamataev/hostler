//! Recorded takeovers through the real service (plan P3a, Task 6). The holders are real
//! processes this test spawns and registers as runs (`claims.acquire` + `claims.started`), and
//! `cli::takeover::take_over` stops them for real: process groups on Unix, jobs on Windows.
//! The `hostler run --screen --force` / `hostler hold` end-to-end variants come with Tasks 7–8.

mod common;

use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use common::Home;
use hostler::cli::takeover::{
    HolderStopper, RunGroupStopper, StopError, TAKEOVER_GRACE, take_over_with,
};
#[cfg(unix)]
use hostler::cli::takeover::{Interrupt, take_over_until};
use hostler::client::Client;
use hostler::contract::claims::{
    ClaimOutcome, ClaimRequest, ClaimStarted, EndReason, Purpose, ResourceId, Selector,
    StopEvidence, StopTarget,
};
#[cfg(unix)]
use hostler::contract::claims::{SeizeBegin, SeizeCommit, SeizePlan};
use hostler::contract::heavy::HistoryQuery;
use hostler::contract::wire::Request;
use hostler::contract::{ErrorCode, HostlerError};
#[cfg(unix)]
use hostler::procs::{Liveness, liveness};
use hostler::procs::{
    ProcessIdentity, ProcessTable, RunGroup, SysProcessTable, current_identity, identity_of,
};

fn table() -> SysProcessTable {
    SysProcessTable::new().unwrap()
}

fn me() -> ProcessIdentity {
    current_identity(&table()).unwrap()
}

fn workspace(home: &Home, name: &str) -> String {
    home.workspace(&home.make_repo(name)).unwrap().id
}

fn request(ws: &str, requester: &ProcessIdentity, r: ResourceId, purpose: Purpose) -> ClaimRequest {
    ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact { resource: r },
        workspace_id: ws.into(),
        requester: requester.clone(),
        purpose,
        reentrant_tokens: Vec::new(),
        no_wait: false,
    }
}

fn lease(ws: &str, requester: &ProcessIdentity, r: ResourceId) -> ClaimRequest {
    request(
        ws,
        requester,
        r,
        Purpose::Lease {
            command: "make test".into(),
        },
    )
}

fn ttl_hold(ws: &str, r: ResourceId) -> ClaimRequest {
    request(
        ws,
        &me(),
        r,
        Purpose::Hold {
            ttl_ms: Some(30 * 60_000),
            bound_to: None,
            distinct: false,
        },
    )
}

fn outcome(home: &Home, r: Request) -> ClaimOutcome {
    serde_json::from_value(home.call(r).unwrap()).unwrap()
}

fn acquire(home: &Home, r: &ClaimRequest) -> ClaimOutcome {
    outcome(home, Request::ClaimsAcquire(r.clone()))
}

fn granted(o: ClaimOutcome) -> String {
    match o {
        ClaimOutcome::Granted { claim } | ClaimOutcome::Joined { claim } => claim.id,
        other => panic!("not granted: {other:?}"),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// `take_over_with` on a fresh connection.
fn take(
    home: &Home,
    r: &ResourceId,
    confirmation: &str,
    taker: &ClaimRequest,
    stoppers: Vec<Box<dyn HolderStopper>>,
    grace: Duration,
) -> Result<ClaimOutcome, HostlerError> {
    home.start_service();
    let paths = home.paths();
    runtime().block_on(async {
        let mut client = Client::connect(&paths).await?;
        take_over_with(&mut client, r, confirmation, taker.clone(), stoppers, grace).await
    })
}

fn real() -> Vec<Box<dyn HolderStopper>> {
    vec![Box::new(RunGroupStopper)]
}

fn ended(home: &Home, id: &str) -> Option<EndReason> {
    let items: Vec<hostler::contract::claims::ClaimHistoryItem> = serde_json::from_value(
        home.call(Request::ClaimsHistory(HistoryQuery {
            since_ms: None,
            limit: 500,
        }))
        .unwrap(),
    )
    .unwrap();
    items.into_iter().find(|i| i.id == id).map(|i| i.reason)
}

fn holders(home: &Home, r: &ResourceId) -> Vec<String> {
    home.snapshot()
        .resources
        .into_iter()
        .filter(|s| s.resource.id == *r)
        .flat_map(|s| s.claims.into_iter().map(|c| c.id))
        .collect()
}

fn seizing(home: &Home, r: &ResourceId) -> bool {
    home.snapshot()
        .resources
        .iter()
        .any(|s| s.resource.id == *r && s.seizing)
}

/// Fails every target whose holder or child has one of these pids; stops the rest for real.
#[cfg(unix)]
struct FailFor(Vec<u32>);

#[cfg(unix)]
impl HolderStopper for FailFor {
    fn handles(&self, t: &StopTarget) -> bool {
        RunGroupStopper.handles(t)
    }

    fn stop(&self, t: &StopTarget, grace: Duration) -> Result<StopEvidence, StopError> {
        let pids: Vec<u32> = match t {
            StopTarget::RunGroup { holder, child, .. } => std::iter::once(holder.pid)
                .chain(child.iter().map(|c| c.pid))
                .collect(),
            StopTarget::Process { process, .. } => vec![process.pid],
            StopTarget::Foreign { .. } => Vec::new(),
        };
        if pids.iter().any(|p| self.0.contains(p)) {
            return Err(StopError::PermissionDenied("not this one".into()));
        }
        RunGroupStopper.stop(t, grace)
    }
}

/// Removes the fake source's claim from its file, as `HOSTLER_TEST_FOREIGN_STOPPER` does.
struct FileForeignStopper(std::path::PathBuf);

impl HolderStopper for FileForeignStopper {
    fn handles(&self, t: &StopTarget) -> bool {
        matches!(t, StopTarget::Foreign { source, .. } if source == "fake")
    }

    fn stop(&self, _t: &StopTarget, _grace: Duration) -> Result<StopEvidence, StopError> {
        std::fs::write(&self.0, "[]").unwrap();
        Ok(StopEvidence::Gone { target: 0 })
    }
}

#[cfg(unix)]
mod unix {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use super::*;

    /// A spawned process, killed and reaped on drop.
    pub struct Proc {
        pub child: Child,
        pub id: ProcessIdentity,
    }

    impl Proc {
        pub fn spawn(script: &str, own_group: bool) -> Proc {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped());
            if own_group {
                cmd.process_group(0);
            }
            let child = cmd.spawn().unwrap();
            let id = identity_of(&table(), child.id()).unwrap();
            Proc { child, id }
        }

        /// `spawn` with these environment variables (a nested `hostler` needs its home).
        pub fn spawn_env(script: &str, env: &[(&str, &std::ffi::OsStr)]) -> Proc {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            for (name, value) in env {
                cmd.env(name, value);
            }
            let child = cmd.spawn().unwrap();
            let id = identity_of(&table(), child.id()).unwrap();
            Proc { child, id }
        }

        /// A stand-in for `hostler run`'s wrapper: it outlives its command.
        pub fn wrapper() -> Proc {
            Proc::spawn("exec sleep 300", false)
        }

        pub fn group(&self) -> RunGroup {
            RunGroup::UnixGroup {
                pgid: self.id.pid,
                boot_id: table().boot_id(),
            }
        }

        pub fn alive(&self) -> bool {
            liveness(&self.id, &table()) == Liveness::Alive
        }

        /// Waits up to 5 s for the process to end (reaping it).
        pub fn ended_within(&mut self, limit: Duration) -> bool {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                if self.child.try_wait().unwrap().is_some() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        }
    }

    impl Drop for Proc {
        fn drop(&mut self) {
            // SAFETY: kill has no memory preconditions; the group may be gone already.
            unsafe { libc::killpg(self.id.pid as libc::pid_t, libc::SIGKILL) };
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// A granted lease of `ws` on `r` whose command runs `script` in its own group.
    pub fn run(home: &Home, ws: &str, r: ResourceId, script: &str) -> (ClaimRequest, Proc, Proc) {
        let wrapper = Proc::wrapper();
        let req = lease(ws, &wrapper.id, r);
        granted(acquire(home, &req));
        let command = Proc::spawn(script, true);
        start(home, &req.id, &command);
        (req, wrapper, command)
    }

    pub fn start_run(home: &Home, claim_id: &str, command: &Proc) {
        start(home, claim_id, command)
    }

    fn start(home: &Home, claim_id: &str, command: &Proc) {
        home.call(Request::ClaimsStarted(ClaimStarted {
            claim_id: claim_id.into(),
            child: command.id.clone(),
            group: command.group(),
        }))
        .unwrap();
    }

    pub fn perl() -> bool {
        Command::new("perl")
            .arg("-v")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[cfg(unix)]
use unix::{Proc, run};

#[cfg(unix)]
#[test]
fn takeover_stops_the_holder_and_moves_the_screen() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, wrapper, mut command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let taker = lease(&b, &me(), ResourceId::screen());
    assert!(matches!(
        acquire(&home, &taker),
        ClaimOutcome::Queued { .. }
    ));
    let started = Instant::now();
    let got = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        real(),
        TAKEOVER_GRACE,
    )
    .unwrap();
    assert_eq!(granted(got), taker.id);
    // SIGTERM ends `sleep` at once: well within the grace.
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(command.ended_within(Duration::from_secs(5)));
    // The wrapper is not in the run's group: it lives on to report the takeover.
    assert!(wrapper.alive());
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
    assert_eq!(
        holders(&home, &ResourceId::screen()),
        vec![taker.id.clone()]
    );
    // Its release now finds the claim gone (the wrapper's "taken over" summary, Task 8).
    let release = home.call(Request::ClaimsRelease(
        hostler::contract::claims::ClaimRelease {
            claim_id: held.id.clone(),
            workspace_id: a,
            requester: held.requester.clone(),
            exit_code: Some(143),
        },
    ));
    let outcome: hostler::contract::claims::ReleaseOutcome =
        serde_json::from_value(release.unwrap()).unwrap();
    let b_path = home.workspace(&home.make_repo("b")).unwrap().path;
    assert_eq!(outcome.seized_by, Some(b_path));
}

#[cfg(unix)]
#[test]
fn takeover_of_a_hold_stops_its_runs() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let hold = ttl_hold(&a, ResourceId::screen());
    granted(acquire(&home, &hold));
    let (inner, _wrapper, mut command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let taker = lease(&b, &me(), ResourceId::screen());
    granted(
        take(
            &home,
            &ResourceId::screen(),
            "screen",
            &taker,
            real(),
            TAKEOVER_GRACE,
        )
        .unwrap(),
    );
    assert!(command.ended_within(Duration::from_secs(5)));
    assert_eq!(ended(&home, &hold.id), Some(EndReason::Seized));
    assert_eq!(ended(&home, &inner.id), Some(EndReason::Seized));
}

#[cfg(unix)]
#[test]
fn takeover_of_a_process_bound_hold_stops_the_bound_process() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let mut bound = Proc::spawn("exec sleep 300", false);
    let hold = request(
        &a,
        &me(),
        ResourceId::screen(),
        Purpose::Hold {
            ttl_ms: None,
            bound_to: Some(bound.id.clone()),
            distinct: false,
        },
    );
    granted(acquire(&home, &hold));
    let taker = lease(&b, &me(), ResourceId::screen());
    granted(
        take(
            &home,
            &ResourceId::screen(),
            "screen",
            &taker,
            real(),
            TAKEOVER_GRACE,
        )
        .unwrap(),
    );
    assert!(bound.ended_within(Duration::from_secs(5)));
    assert_eq!(ended(&home, &hold.id), Some(EndReason::Seized));
}

#[cfg(unix)]
#[test]
fn sigterm_ignoring_holder_is_killed_after_grace() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, mut command) = run(
        &home,
        &a,
        ResourceId::screen(),
        "trap '' TERM; sleep 300 & wait",
    );
    std::thread::sleep(Duration::from_millis(200));
    let taker = lease(&b, &me(), ResourceId::screen());
    let started = Instant::now();
    let grace = Duration::from_millis(300);
    granted(
        take(
            &home,
            &ResourceId::screen(),
            "screen",
            &taker,
            real(),
            grace,
        )
        .unwrap(),
    );
    let took = started.elapsed();
    assert!(took >= grace, "SIGKILL came before the grace: {took:?}");
    assert!(took < grace + Duration::from_secs(5), "{took:?}");
    assert!(command.ended_within(Duration::from_secs(5)));
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
}

#[cfg(unix)]
#[test]
fn stopper_failure_keeps_the_holder() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let taker = lease(&b, &me(), ResourceId::screen());
    assert!(matches!(
        acquire(&home, &taker),
        ClaimOutcome::Queued { .. }
    ));
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(FailFor(vec![command.id.pid]))],
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed);
    assert!(
        e.message
            .starts_with("screen stays with its holder: hostler may not stop the run of"),
        "{}",
        e.message
    );
    assert!(command.alive());
    assert_eq!(holders(&home, &ResourceId::screen()), vec![held.id.clone()]);
    // The plan was aborted; the taker's request is still the caller's to withdraw.
    assert!(!seizing(&home, &ResourceId::screen()));
    assert!(matches!(
        outcome(
            &home,
            Request::ClaimsAttach {
                request_id: taker.id.clone()
            }
        ),
        ClaimOutcome::Queued { .. }
    ));
}

#[cfg(unix)]
#[test]
fn partial_stop_reports_what_was_stopped() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let hold = ttl_hold(&a, ResourceId::screen());
    granted(acquire(&home, &hold));
    let (_, _w1, mut first) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let (_, _w2, second) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let taker = lease(&b, &me(), ResourceId::screen());
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(FailFor(vec![second.id.pid]))],
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed);
    // Not atomic: the first run is stopped, the second still runs, the hold stays (R2-11).
    assert!(first.ended_within(Duration::from_secs(5)));
    assert!(second.alive());
    assert!(holders(&home, &ResourceId::screen()).contains(&hold.id));
    assert_eq!(ended(&home, &hold.id), None);
}

#[cfg(unix)]
#[test]
fn seized_before_spawn_the_wrappers_started_gets_claim_not_held() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    // Granted, not started yet: the wrapper itself is the target.
    let mut wrapper = Proc::wrapper();
    let held = lease(&a, &wrapper.id, ResourceId::screen());
    granted(acquire(&home, &held));
    let taker = lease(&b, &me(), ResourceId::screen());
    granted(
        take(
            &home,
            &ResourceId::screen(),
            "screen",
            &taker,
            real(),
            TAKEOVER_GRACE,
        )
        .unwrap(),
    );
    assert!(wrapper.ended_within(Duration::from_secs(5)));
    // A wrapper that got past the stop (it was paused, say) is refused at `claims.started`, so
    // its gate never opens.
    let command = Proc::spawn("sleep 300", true);
    let e = home
        .call(Request::ClaimsStarted(ClaimStarted {
            claim_id: held.id.clone(),
            child: command.id.clone(),
            group: command.group(),
        }))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ClaimNotHeld);
}

/// L17: during a takeover a new run of the holder's workspace does not join; after the commit it
/// queues behind the taker.
#[cfg(unix)]
#[test]
fn join_during_a_seizure_is_not_admitted() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let hold = ttl_hold(&a, ResourceId::screen());
    granted(acquire(&home, &hold));
    // Wrappers of their own, as `hostler run` has: a waiter descending from the holder would
    // be a deadlock.
    let taker_wrapper = Proc::wrapper();
    let taker = lease(&b, &taker_wrapper.id, ResourceId::screen());
    let plan: SeizePlan = serde_json::from_value(
        home.call(Request::ClaimsSeizeBegin(SeizeBegin {
            resource: ResourceId::screen(),
            confirmation: "screen".into(),
            request: taker.clone(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(plan.stops.is_empty());
    let late_wrapper = Proc::wrapper();
    let late = lease(&a, &late_wrapper.id, ResourceId::screen());
    match acquire(&home, &late) {
        ClaimOutcome::Queued { blockers, .. } => assert!(
            blockers
                .iter()
                .any(|b| matches!(b, hostler::contract::claims::Blocker::Seizing { .. })),
            "{blockers:?}"
        ),
        other => panic!("joined during a seizure: {other:?}"),
    }
    let got: hostler::contract::claims::SeizeOutcome = serde_json::from_value(
        home.call(Request::ClaimsSeizeCommit(SeizeCommit {
            plan_id: plan.plan_id,
            requester: taker.requester.clone(),
            evidence: Vec::new(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!got.attested);
    assert_eq!(granted(got.outcome), taker.id);
    assert_eq!(ended(&home, &hold.id), Some(EndReason::Seized));
    assert!(matches!(
        outcome(
            &home,
            Request::ClaimsAttach {
                request_id: late.id.clone()
            }
        ),
        ClaimOutcome::Queued { .. }
    ));
}

/// L18: a run nested under the holder's run, in its own process group (so the outer group's
/// signals miss it) and ignoring SIGTERM, is a target of its own; the commit passes only after it
/// is gone.
#[cfg(unix)]
#[test]
fn takeover_stops_nested_claim_runs_of_the_holder() {
    if !unix::perl() {
        eprintln!("skipped: no perl to make a nested process group");
        return;
    }
    let home = Home::new();
    home.hostler()
        .env("HOSTLER_TEST_FAKE_CATALOG", "1")
        .args(["service", "start"])
        .output()
        .unwrap();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let device = ResourceId("device:test:0".into());
    // The outer run's command starts a nested run in a new group and prints its pid.
    let (outer, _wrapper, mut command) = run(
        &home,
        &a,
        device.clone(),
        r#"perl -e '$SIG{TERM}="IGNORE"; setpgrp(0,0); exec("sleep","300")' & echo $!; wait"#,
    );
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(command.child.stdout.as_mut().unwrap()),
        &mut line,
    )
    .unwrap();
    let nested_pid: u32 = line.trim().parse().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let nested_id = identity_of(&table(), nested_pid).unwrap();
    let nested_group = RunGroup::UnixGroup {
        pgid: nested_pid,
        boot_id: table().boot_id(),
    };
    // The nested run holds the screen (a claim of its own, on another resource).
    let inner = lease(&a, &nested_id, ResourceId::screen());
    granted(acquire(&home, &inner));
    home.call(Request::ClaimsStarted(ClaimStarted {
        claim_id: inner.id.clone(),
        child: nested_id.clone(),
        group: nested_group.clone(),
    }))
    .unwrap();

    let taker = lease(&b, &me(), device.clone());
    let started = Instant::now();
    let grace = Duration::from_millis(300);
    granted(take(&home, &device, "fake-0", &taker, real(), grace).unwrap());
    // The nested group ignored SIGTERM: SIGKILL after the grace ended it.
    assert!(started.elapsed() >= grace);
    assert!(!hostler::procs::group_liveness(&nested_group, &table()));
    assert!(command.ended_within(Duration::from_secs(5)));
    assert_eq!(ended(&home, &outer.id), Some(EndReason::Seized));
    // Only the device's claims moved; the nested run's own claim ends through its own release
    // or reaping.
    assert_ne!(ended(&home, &inner.id), Some(EndReason::Seized));
    // SAFETY: kill has no memory preconditions; the group is gone.
    unsafe { libc::killpg(nested_pid as libc::pid_t, libc::SIGKILL) };
}

#[test]
fn foreign_claim_takeover_through_the_fake_source() {
    let home = Home::new();
    let file = home.dir.path().join("foreign.json");
    let claim = serde_json::json!([{
        "resource": "device:test:0",
        "source": "fake",
        "description": "an agent-device session",
        "workspace_path": null,
        "since_ms": null,
        "stop": { "session": 1 }
    }]);
    std::fs::write(&file, claim.to_string()).unwrap();
    let start = home
        .hostler()
        .env("HOSTLER_TEST_FAKE_CATALOG", "1")
        .env("HOSTLER_TEST_FAKE_FOREIGN", &file)
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(start.status.success(), "{}", common::stderr(&start));
    let b = workspace(&home, "b");
    let device = ResourceId("device:test:0".into());
    let taker = lease(&b, &me(), device.clone());
    // Without a stopper for it, nothing moves.
    let e = take(&home, &device, "fake-0", &taker, real(), TAKEOVER_GRACE).unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed);
    assert!(
        e.message.ends_with("hostler cannot stop this holder"),
        "{}",
        e.message
    );
    assert!(!seizing(&home, &device));
    let got = take(
        &home,
        &device,
        "fake-0",
        &taker,
        vec![Box::new(FileForeignStopper(file.clone()))],
        TAKEOVER_GRACE,
    )
    .unwrap();
    assert_eq!(granted(got), taker.id);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[]");
}

#[test]
fn wrong_confirmation_and_free_resource_are_refused() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let taker = lease(&b, &me(), ResourceId::screen());
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        real(),
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, "nothing to take over: screen is free or yours");
    granted(acquire(&home, &ttl_hold(&a, ResourceId::screen())));
    let e = take(
        &home,
        &ResourceId::screen(),
        "SCREEN",
        &taker,
        real(),
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.message, "--confirmation must be screen");
    assert!(!seizing(&home, &ResourceId::screen()));
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};

    use super::*;

    /// A job with one long-running process in it, as `hostler run` makes it.
    pub struct Job {
        pub handle: HANDLE,
        pub name: String,
        pub child: Child,
        pub id: ProcessIdentity,
    }

    impl Job {
        pub fn new() -> Job {
            let name = format!(r"Local\hostler-test-{}", uuid::Uuid::new_v4());
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: default security attributes and a NUL-terminated name.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), wide.as_ptr()) };
            assert!(!handle.is_null());
            let child = Command::new("ping")
                .args(["-n", "300", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            // SAFETY: both handles are open.
            let ok = unsafe { AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) };
            assert_ne!(ok, 0);
            let id = identity_of(&table(), child.id()).unwrap();
            Job {
                handle,
                name,
                child,
                id,
            }
        }

        pub fn group(&self, session: Option<u32>) -> RunGroup {
            RunGroup::WindowsJob {
                name: self.name.clone(),
                boot_id: table().boot_id(),
                session,
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            // SAFETY: the handle from CreateJobObjectW, closed once.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_takeover_terminates_the_job() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let mut job = windows::Job::new();
    // The run is the job's own process, not this test (the taker): a taker under its own
    // target is refused.
    let held = lease(&a, &job.id, ResourceId::screen());
    granted(acquire(&home, &held));
    home.call(Request::ClaimsStarted(ClaimStarted {
        claim_id: held.id.clone(),
        child: job.id.clone(),
        group: job.group(hostler::procs::current_session()),
    }))
    .unwrap();
    let taker = lease(&b, &me(), ResourceId::screen());
    granted(
        take(
            &home,
            &ResourceId::screen(),
            "screen",
            &taker,
            real(),
            TAKEOVER_GRACE,
        )
        .unwrap(),
    );
    assert!(job.child.wait().is_ok());
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
}

#[cfg(windows)]
#[test]
fn windows_other_session_job_is_refused() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let job = windows::Job::new();
    // The run is the job's own process, not this test (the taker): a taker under its own
    // target is refused.
    let held = lease(&a, &job.id, ResourceId::screen());
    granted(acquire(&home, &held));
    let other = hostler::procs::current_session().unwrap() + 1;
    home.call(Request::ClaimsStarted(ClaimStarted {
        claim_id: held.id.clone(),
        child: job.id.clone(),
        group: job.group(Some(other)),
    }))
    .unwrap();
    let taker = lease(&b, &me(), ResourceId::screen());
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        real(),
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed);
    assert!(
        e.message
            .contains(&format!("logon session {other}; stop it there")),
        "{}",
        e.message
    );
    assert_eq!(holders(&home, &ResourceId::screen()), vec![held.id.clone()]);
    let _ = &job;
}

// ---- fix round 1 (L27–L30, T6-7) ----

#[cfg(unix)]
fn begin_raw(home: &Home, r: &ResourceId, confirmation: &str, taker: &ClaimRequest) -> SeizePlan {
    serde_json::from_value(
        home.call(Request::ClaimsSeizeBegin(SeizeBegin {
            resource: r.clone(),
            confirmation: confirmation.into(),
            request: taker.clone(),
        }))
        .unwrap(),
    )
    .unwrap()
}

/// L27: a wrapper that reaches `claims.started` during the plan is refused, so its gated
/// command never runs; were it accepted, the commit would check the command it registered.
/// Never both a moved claim and a live command.
#[cfg(unix)]
#[test]
fn a_seized_claim_never_also_runs_its_command() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let wrapper = Proc::wrapper();
    let held = lease(&a, &wrapper.id, ResourceId::screen());
    granted(acquire(&home, &held));
    let taker_wrapper = Proc::wrapper();
    let taker = lease(&b, &taker_wrapper.id, ResourceId::screen());
    let plan = begin_raw(&home, &ResourceId::screen(), "screen", &taker);
    // The holder's command waits at its gate, ignoring SIGTERM, while the wrapper reports it.
    let command = Proc::spawn("trap '' TERM; sleep 300 & wait", true);
    let started = home.call(Request::ClaimsStarted(ClaimStarted {
        claim_id: held.id.clone(),
        child: command.id.clone(),
        group: command.group(),
    }));
    let command = match started {
        // The gate never opens: the wrapper abandons the gated command.
        Err(e) => {
            assert_eq!(e.code, ErrorCode::TakenOver, "{e:?}");
            drop(command);
            None
        }
        Ok(_) => Some(command),
    };
    // The taker stops what its plan names (here: the wrapper), then commits.
    let mut evidence = Vec::new();
    for (k, target) in plan.stops.iter().enumerate() {
        let gone = RunGroupStopper
            .stop(target, Duration::from_millis(300))
            .unwrap();
        evidence.push(match gone {
            StopEvidence::Gone { .. } => StopEvidence::Gone { target: k },
            StopEvidence::JobGone { session, .. } => StopEvidence::JobGone { target: k, session },
        });
    }
    let moved = home
        .call(Request::ClaimsSeizeCommit(SeizeCommit {
            plan_id: plan.plan_id,
            requester: taker.requester.clone(),
            evidence,
        }))
        .is_ok();
    let alive = command.as_ref().is_some_and(Proc::alive);
    assert!(
        !(moved && alive),
        "the claim moved while its command still runs"
    );
    assert!(
        moved,
        "the wrapper refused at `started` is all there was to stop"
    );
}

/// L27/L28 through the service: under a run being taken over, a heavy entry may not start
/// its command, and neither a heavy task nor a claim may be queued.
#[cfg(unix)]
#[test]
fn nothing_new_starts_under_a_run_being_taken_over() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (_held, _wrapper, mut command) = run(
        &home,
        &a,
        ResourceId::screen(),
        "sleep 300 & echo $!; sleep 300 & echo $!; wait",
    );
    let mut out = std::io::BufReader::new(command.child.stdout.take().unwrap());
    let mut pid = || {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut out, &mut line).unwrap();
        identity_of(&table(), line.trim().parse().unwrap()).unwrap()
    };
    let (first, second) = (pid(), pid());
    let heavy = |holder: &ProcessIdentity| {
        Request::HeavyAcquire(hostler::contract::heavy::HeavyAcquire {
            id: uuid::Uuid::new_v4().to_string(),
            kind: "gradle".into(),
            command: "./gradlew build".into(),
            cwd: "/work".into(),
            workspace_root: None,
            holder: holder.clone(),
            reentrant_token: None,
            no_wait: false,
        })
    };
    // A heavy task of the run, granted before the takeover.
    let entry = match heavy(&first) {
        Request::HeavyAcquire(h) => h,
        _ => unreachable!(),
    };
    home.call(Request::HeavyAcquire(entry.clone())).unwrap();
    let taker_wrapper = Proc::wrapper();
    let taker = lease(&b, &taker_wrapper.id, ResourceId::screen());
    let plan = begin_raw(&home, &ResourceId::screen(), "screen", &taker);
    let heavy_target = format!("heavy:{}", entry.id);
    assert!(
        plan.stops.iter().any(|t| matches!(t,
            StopTarget::RunGroup { claim_id, .. } if *claim_id == heavy_target)),
        "{:?}",
        plan.stops
    );
    let start = Request::HeavyStarted(hostler::contract::heavy::HeavyStarted {
        id: entry.id.clone(),
        child: first.clone(),
        group: RunGroup::UnixGroup {
            pgid: first.pid,
            boot_id: table().boot_id(),
        },
    });
    let e = home.call(start.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::TakenOver, "{e:?}");
    let e = home.call(heavy(&second)).unwrap_err();
    assert_eq!(e.code, ErrorCode::TakenOver, "{e:?}");
    let e = home
        .call(Request::ClaimsAcquire(lease(
            &a,
            &second,
            ResourceId::screen(),
        )))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::TakenOver, "{e:?}");
    // Once the plan is gone, all of it works again.
    home.call(Request::ClaimsSeizeAbort {
        plan_id: plan.plan_id,
    })
    .unwrap();
    home.call(start).unwrap();
}

/// T6-7: an idle stop is refused while a takeover is in progress.
#[cfg(unix)]
#[test]
fn idle_stop_is_refused_while_a_takeover_is_in_progress() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    granted(acquire(&home, &ttl_hold(&a, ResourceId::screen())));
    let taker = lease(&b, &me(), ResourceId::screen());
    let plan = begin_raw(&home, &ResourceId::screen(), "screen", &taker);
    let paths = home.paths();
    let e = runtime()
        .block_on(async {
            Client::connect_lifecycle(&paths)
                .await?
                .call(Request::ServiceShutdown { if_idle: true })
                .await
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ServiceBusy);
    assert_eq!(e.details["plans"].as_array().unwrap().len(), 1, "{e:?}");
    assert_eq!(e.details["plans"][0]["resource"], "screen");
    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(!stop.status.success());
    assert!(
        common::stderr(&stop).contains("a takeover of screen by"),
        "{}",
        common::stderr(&stop)
    );
    home.call(Request::ClaimsSeizeAbort {
        plan_id: plan.plan_id,
    })
    .unwrap();
    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(stop.status.success(), "{}", common::stderr(&stop));
}

/// Claims every target gone without stopping anything.
#[cfg(unix)]
struct Liar;

#[cfg(unix)]
impl HolderStopper for Liar {
    fn handles(&self, _t: &StopTarget) -> bool {
        true
    }

    fn stop(&self, _t: &StopTarget, _grace: Duration) -> Result<StopEvidence, StopError> {
        Ok(StopEvidence::Gone { target: 0 })
    }
}

/// L29: a commit refused after the stops says how many were stopped.
#[cfg(unix)]
#[test]
fn a_commit_refused_after_stops_reports_them() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (_held, _wrapper, command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let taker = lease(&b, &me(), ResourceId::screen());
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(Liar)],
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed, "{e:?}");
    assert!(e.message.contains("is still running"), "{}", e.message);
    assert_eq!(
        (e.details["stopped"].clone(), e.details["of"].clone()),
        (1.into(), 1.into())
    );
    assert!(command.alive());
}

// ---- fix round 2 (L33) ----

/// Before its first stop, opens `trigger` and waits (up to 10 s) for the nested run to report
/// its exit code in `exit`; then stops for real.
#[cfg(unix)]
struct AfterNested {
    trigger: std::path::PathBuf,
    exit: std::path::PathBuf,
    waited: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl HolderStopper for AfterNested {
    fn handles(&self, t: &StopTarget) -> bool {
        RunGroupStopper.handles(t)
    }

    fn stop(&self, t: &StopTarget, grace: Duration) -> Result<StopEvidence, StopError> {
        if !self.waited.swap(true, std::sync::atomic::Ordering::SeqCst) {
            std::fs::write(&self.trigger, "").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.exit.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        RunGroupStopper.stop(t, grace)
    }
}

/// A's screen run, whose command (its own group) starts `hostler run --heavy` once `trigger`
/// exists — with `nested_env` — and records its exit code; the nested command writes its pid.
#[cfg(unix)]
fn run_with_a_nested_heavy_run(
    home: &Home,
    ws: &str,
    nested_env: &str,
) -> (
    ClaimRequest,
    Proc,
    Proc,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = home.dir.path();
    let (trigger, pid, exit) = (
        dir.join("trigger"),
        dir.join("pid"),
        dir.join("nested-exit"),
    );
    let script = format!(
        "while [ ! -e '{trigger}' ]; do sleep 0.05; done; \
         {nested_env} '{bin}' run --heavy -- sh -c 'echo $$ > \"{pid}\"; trap \"\" TERM; \
         exec sleep 300'; echo $? > '{exit}.tmp' && mv '{exit}.tmp' '{exit}'; sleep 300",
        trigger = trigger.display(),
        bin = common::BIN,
        pid = pid.display(),
        exit = exit.display(),
    );
    let wrapper = Proc::wrapper();
    let req = lease(ws, &wrapper.id, ResourceId::screen());
    granted(acquire(home, &req));
    let ledger = home.home.join("no-looky-ledger.json");
    let command = Proc::spawn_env(
        &script,
        &[
            ("HOSTLER_HOME", home.home.as_os_str()),
            ("HOSTLER_LOOKY_LEDGER", ledger.as_os_str()),
        ],
    );
    unix::start_run(home, &req.id, &command);
    (req, wrapper, command, trigger, pid, exit)
}

/// L28/L33 (the reviewer's case A): a `hostler run --heavy` started under a run being taken
/// over is refused, and P1's wrapper does not fall back to running it unqueued.
#[cfg(unix)]
#[test]
fn nested_heavy_run_started_during_a_takeover_never_runs() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, _command, trigger, pid, exit) = run_with_a_nested_heavy_run(&home, &a, "");
    let taker = lease(&b, &me(), ResourceId::screen());
    let stopper = AfterNested {
        trigger,
        exit: exit.clone(),
        waited: Default::default(),
    };
    let got = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(stopper)],
        Duration::from_millis(300),
    )
    .unwrap();
    assert_eq!(granted(got), taker.id);
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!pid.exists(), "the nested command ran");
    assert_eq!(std::fs::read_to_string(&exit).unwrap().trim(), "78");
}

/// L27/L33 (the reviewer's case B): a nested heavy run granted before the takeover reaches
/// `heavy.started` during it, is refused, abandons its gated command and gives its slot back.
#[cfg(unix)]
#[test]
fn nested_heavy_run_refused_at_started_never_runs() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, _command, trigger, pid, exit) =
        run_with_a_nested_heavy_run(&home, &a, "HOSTLER_TEST_DELAY_STARTED_MS=3000");
    std::fs::write(&trigger, "").unwrap();
    // Granted, its `heavy.started` still delayed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let entry = loop {
        let queue: hostler::contract::heavy::QueueSnapshot =
            serde_json::from_value(home.call(Request::QueueSnapshot).unwrap()).unwrap();
        if let Some(e) = queue.running.into_iter().next() {
            break e;
        }
        assert!(
            Instant::now() < deadline,
            "the nested heavy run was never granted"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(entry.child.is_none());
    let taker = lease(&b, &me(), ResourceId::screen());
    let stopper = AfterNested {
        trigger: home.dir.path().join("trigger-unused"),
        exit: exit.clone(),
        waited: Default::default(),
    };
    let got = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(stopper)],
        Duration::from_millis(300),
    )
    .unwrap();
    assert_eq!(granted(got), taker.id);
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!pid.exists(), "the nested command ran");
    assert_eq!(std::fs::read_to_string(&exit).unwrap().trim(), "78");
    // Its slot was given back, not left to a doomed entry.
    let queue: hostler::contract::heavy::QueueSnapshot =
        serde_json::from_value(home.call(Request::QueueSnapshot).unwrap()).unwrap();
    assert!(queue.running.iter().all(|e| e.id != entry.id), "{queue:?}");
}

// ---- T7 review: a taker under its own target; an interrupted takeover ----

/// Counts its calls; claims every target gone.
#[cfg(unix)]
struct Counting(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(unix)]
impl HolderStopper for Counting {
    fn handles(&self, _t: &StopTarget) -> bool {
        true
    }

    fn stop(&self, _t: &StopTarget, _grace: Duration) -> Result<StopEvidence, StopError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(StopEvidence::Gone { target: 0 })
    }
}

/// A taker running under a target of its own plan would stop itself: refused, nothing stopped,
/// the plan aborted.
#[cfg(unix)]
#[test]
fn a_taker_under_its_own_target_is_refused() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, mut command) =
        run(&home, &a, ResourceId::screen(), "sleep 300 & echo $!; wait");
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(command.child.stdout.as_mut().unwrap()),
        &mut line,
    )
    .unwrap();
    let inner = identity_of(&table(), line.trim().parse().unwrap()).unwrap();
    let taker = lease(&b, &inner, ResourceId::screen());
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(Counting(calls.clone()))],
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed, "{e:?}");
    assert_eq!(e.details["reason"], "taker_is_a_target");
    assert!(
        e.message.contains("which a takeover would stop"),
        "{}",
        e.message
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!seizing(&home, &ResourceId::screen()));
    assert_eq!(holders(&home, &ResourceId::screen()), vec![held.id.clone()]);
    assert!(command.alive());
}

/// Signals the test once its stop begins, then takes 5 s.
#[cfg(unix)]
struct SlowStop(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>);

#[cfg(unix)]
impl HolderStopper for SlowStop {
    fn handles(&self, _t: &StopTarget) -> bool {
        true
    }

    fn stop(&self, _t: &StopTarget, _grace: Duration) -> Result<StopEvidence, StopError> {
        if let Some(begun) = self.0.lock().unwrap().take() {
            let _ = begun.send(());
        }
        std::thread::sleep(Duration::from_secs(5));
        Ok(StopEvidence::Gone { target: 0 })
    }
}

/// T7-8: a Ctrl+C during the stops aborts the plan at once and says what was stopped; the
/// holder keeps the resource.
#[cfg(unix)]
#[test]
fn an_interrupted_takeover_aborts_its_plan() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    let taker = lease(&b, &me(), ResourceId::screen());
    let (begun, stopping) = tokio::sync::oneshot::channel();
    home.start_service();
    let paths = home.paths();
    let started = Instant::now();
    let e = runtime()
        .block_on(async {
            let mut client = Client::connect(&paths).await?;
            take_over_until(
                &mut client,
                &ResourceId::screen(),
                "screen",
                taker.clone(),
                vec![Box::new(SlowStop(std::sync::Mutex::new(Some(begun))))],
                TAKEOVER_GRACE,
                async {
                    let _ = stopping.await;
                    Interrupt::Interrupt
                },
            )
            .await
        })
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(e.code, ErrorCode::Withdrawn, "{e:?}");
    assert_eq!(e.details["exit_code"], 130);
    assert_eq!(e.details["interrupted"], "SIGINT");
    assert_eq!(
        (e.details["stopped"].clone(), e.details["of"].clone()),
        (0.into(), 1.into())
    );
    assert!(!seizing(&home, &ResourceId::screen()));
    assert_eq!(holders(&home, &ResourceId::screen()), vec![held.id.clone()]);
    assert!(command.alive());
}

// ---- fix round 3 ----

/// Before its first stop, kills the process holding the only heavy slot (the queue reaps it and
/// grants the nested run) and waits up to 15 s for the nested run's exit code; then stops.
#[cfg(unix)]
struct FreeTheSlot {
    blocker: u32,
    exit: std::path::PathBuf,
    done: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl HolderStopper for FreeTheSlot {
    fn handles(&self, t: &StopTarget) -> bool {
        RunGroupStopper.handles(t)
    }

    fn stop(&self, t: &StopTarget, grace: Duration) -> Result<StopEvidence, StopError> {
        if !self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            // SAFETY: kill has no memory preconditions.
            unsafe { libc::kill(self.blocker as libc::pid_t, libc::SIGKILL) };
            let deadline = Instant::now() + Duration::from_secs(15);
            while !self.exit.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        RunGroupStopper.stop(t, grace)
    }
}

/// T6-R3-1 (the reviewer's case): a nested heavy run queued when the takeover began and
/// granted during it never starts its command; its wrapper exits 78 and gives the slot back,
/// and the takeover commits.
#[cfg(unix)]
#[test]
fn nested_heavy_run_granted_during_a_takeover_never_runs() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    home.call(Request::QueueSetCapacity { capacity: 1 })
        .unwrap();
    // The only slot is taken by somebody else.
    let blocker = Proc::wrapper();
    let blocking = hostler::contract::heavy::HeavyAcquire {
        id: uuid::Uuid::new_v4().to_string(),
        kind: "gradle".into(),
        command: "./gradlew build".into(),
        cwd: "/work".into(),
        workspace_root: None,
        holder: blocker.id.clone(),
        reentrant_token: None,
        no_wait: false,
    };
    home.call(Request::HeavyAcquire(blocking.clone())).unwrap();
    let (held, _wrapper, _command, trigger, pid, exit) = run_with_a_nested_heavy_run(&home, &a, "");
    std::fs::write(&trigger, "").unwrap();
    // The nested run waits for the slot.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let queue: hostler::contract::heavy::QueueSnapshot =
            serde_json::from_value(home.call(Request::QueueSnapshot).unwrap()).unwrap();
        if !queue.waiting.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the nested heavy run never queued"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let taker = lease(&b, &me(), ResourceId::screen());
    let stopper = FreeTheSlot {
        blocker: blocker.id.pid,
        exit: exit.clone(),
        done: Default::default(),
    };
    let got = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(stopper)],
        Duration::from_millis(300),
    )
    .unwrap();
    assert_eq!(granted(got), taker.id);
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!pid.exists(), "the nested command ran");
    assert_eq!(std::fs::read_to_string(&exit).unwrap().trim(), "78");
    // Its slot was given back.
    let queue: hostler::contract::heavy::QueueSnapshot =
        serde_json::from_value(home.call(Request::QueueSnapshot).unwrap()).unwrap();
    assert!(
        queue.running.is_empty() && queue.waiting.is_empty(),
        "{queue:?}"
    );
}

/// Edge: a taker inside a target's process group, without descending from it, is refused too
/// (`killpg` would reach it).
#[cfg(unix)]
#[test]
fn a_taker_inside_a_targets_group_is_refused() {
    use std::os::unix::process::CommandExt;

    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    // A process of this test (not under the run) joined to the run's group.
    let mut inside = std::process::Command::new("sleep")
        .arg("300")
        .process_group(command.id.pid as i32)
        .spawn()
        .unwrap();
    let inside_id = identity_of(&table(), inside.id()).unwrap();
    let taker = lease(&b, &inside_id, ResourceId::screen());
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let e = take(
        &home,
        &ResourceId::screen(),
        "screen",
        &taker,
        vec![Box::new(Counting(calls.clone()))],
        TAKEOVER_GRACE,
    )
    .unwrap_err();
    let _ = inside.kill();
    let _ = inside.wait();
    assert_eq!(e.details["reason"], "taker_is_a_target", "{e:?}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!seizing(&home, &ResourceId::screen()));
    assert_eq!(holders(&home, &ResourceId::screen()), vec![held.id.clone()]);
}

/// T6-R3-2: an interrupt during a stop names the target whose stop had begun, and that stop
/// never escalates to SIGKILL.
#[cfg(unix)]
#[test]
fn an_interrupted_stop_never_escalates() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let (held, _wrapper, command) = run(
        &home,
        &a,
        ResourceId::screen(),
        "trap '' TERM; sleep 300 & wait",
    );
    std::thread::sleep(Duration::from_millis(200));
    let taker = lease(&b, &me(), ResourceId::screen());
    home.start_service();
    let paths = home.paths();
    let e = runtime()
        .block_on(async {
            let mut client = Client::connect(&paths).await?;
            take_over_until(
                &mut client,
                &ResourceId::screen(),
                "screen",
                taker.clone(),
                vec![Box::new(RunGroupStopper)],
                Duration::from_secs(1),
                async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Interrupt::Interrupt
                },
            )
            .await
        })
        .unwrap_err();
    assert_eq!(e.details["signalled"], 1, "{e:?}");
    assert_eq!(e.details["stopped"], 0);
    assert!(
        e.message.contains(&format!(
            "the stop of the run of claim {} (PID {}) had begun",
            held.id, command.id.pid
        )),
        "{}",
        e.message
    );
    // Past the grace and the SIGKILL wait's start: still alive, never killed.
    std::thread::sleep(Duration::from_secs(2));
    assert!(command.alive(), "the interrupted stop escalated");
    assert!(!seizing(&home, &ResourceId::screen()));
}

/// P3bT10-1: the takeover's line counts only the targets it signalled. A run whose command has
/// already ended (its wrapper still lives, so nothing was reaped) is taken over with nothing
/// stopped, and says so.
#[cfg(unix)]
#[test]
fn a_takeover_of_a_target_that_already_ended_counts_none_stopped() {
    let home = Home::new();
    let a = workspace(&home, "a");
    let (held, wrapper, mut command) = run(&home, &a, ResourceId::screen(), "sleep 300");
    // SAFETY: kill has no memory preconditions.
    unsafe { libc::killpg(command.id.pid as libc::pid_t, libc::SIGKILL) };
    assert!(command.ended_within(Duration::from_secs(5)));
    assert!(wrapper.alive());
    let repo = home.make_repo("c");
    let out = home
        .hostler()
        .args([
            "hold",
            "screen",
            "--force",
            "--confirmation",
            "screen",
            "--ttl",
            "1",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let text = common::stderr(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("(0 process(es) stopped)"), "{text}");
    assert_eq!(ended(&home, &held.id), Some(EndReason::Seized));
}
