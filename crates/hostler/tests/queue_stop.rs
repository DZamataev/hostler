//! `heavy.stop` and `hostler queue stop` against the real binary (P5 Task 6, spec §11.1): the
//! service stops a running heavy task itself, checks that its processes are gone, and only then
//! records `stopped`; the method is gated (§8.5), refuses the service's own processes (R2-9),
//! and a task it cannot stop keeps its slot.
//!
//! Every process a test starts is its own (`sleep`, `hostler run`) and is ended by a guard; the
//! service is a test service under a temporary home. Unix only: the targets are process groups.
#![cfg(unix)]

mod common;
#[path = "common/raw.rs"]
mod raw;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::client::Client;
use hostler::contract::heavy::{
    AcquireOutcome, HeavyAcquire, HeavyEntry, HeavyHistoryItem, HeavyOutcome, HeavyStarted,
    HeavyState, HistoryQuery, QueueSnapshot, QueueStopOutcome, ServiceStatus,
};
use hostler::contract::secrets::{Prove, Unlock};
use hostler::contract::wire::Request;
use hostler::contract::{ErrorCode, HostlerError};
use hostler::procs::{
    self, Liveness, ProcessIdentity, RunGroup, SysProcessTable, current_identity,
};
use hostler::secrets::value::SecretValue;
use raw::files_text;
use serde_json::{Value, json};

const PASSWORD: &str = "pass-1234";

/// One connection of its own, held across calls (a grant belongs to its connection).
struct Conn {
    rt: tokio::runtime::Runtime,
    client: Client,
}

impl Conn {
    fn open(home: &Home) -> Conn {
        let rt = raw::runtime();
        let client = rt.block_on(Client::connect(&home.paths())).unwrap();
        Conn { rt, client }
    }

    fn call(&mut self, req: Request) -> Result<Value, HostlerError> {
        self.rt.block_on(self.client.call(req))
    }

    fn prove(&mut self) {
        self.call(Request::SecretsProve(Prove {
            password: SecretValue::new(PASSWORD.into()),
            key_file: None,
        }))
        .unwrap();
    }

    fn stop(&mut self, id: &str, confirmation: &str) -> Result<QueueStopOutcome, HostlerError> {
        let answer = self.call(Request::HeavyStop {
            id: id.into(),
            confirmation: confirmation.into(),
        })?;
        Ok(serde_json::from_value(answer).unwrap())
    }

    fn snapshot(&mut self) -> QueueSnapshot {
        serde_json::from_value(self.call(Request::QueueSnapshot).unwrap()).unwrap()
    }

    fn history(&mut self, id: &str) -> Option<HeavyHistoryItem> {
        let items: Vec<HeavyHistoryItem> = serde_json::from_value(
            self.call(Request::QueueHistory(HistoryQuery {
                since_ms: None,
                limit: 100,
            }))
            .unwrap(),
        )
        .unwrap();
        items.into_iter().find(|i| i.id == id)
    }

    fn acquire(&mut self, holder: ProcessIdentity) -> (String, AcquireOutcome) {
        let id = uuid::Uuid::new_v4().to_string();
        let outcome = self
            .call(Request::HeavyAcquire(HeavyAcquire {
                id: id.clone(),
                kind: "gradle".into(),
                command: "./gradlew assemble".into(),
                cwd: "/work/app".into(),
                workspace_root: None,
                holder,
                reentrant_token: None,
                no_wait: false,
            }))
            .unwrap();
        (id, serde_json::from_value(outcome).unwrap())
    }

    fn status(&mut self) -> ServiceStatus {
        serde_json::from_value(self.call(Request::ServiceStatus).unwrap()).unwrap()
    }
}

fn start(home: &Home, env: &[(&str, &str)]) {
    let out = home
        .hostler()
        .envs(env.iter().copied())
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
}

/// A started service (with extra test environment) and a new database.
fn ready(env: &[(&str, &str)]) -> Home {
    let home = Home::new();
    start(&home, env);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    home
}

fn table() -> SysProcessTable {
    SysProcessTable::new().unwrap()
}

fn alive(p: &ProcessIdentity) -> bool {
    procs::liveness(p, &table()) == Liveness::Alive
}

fn until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Processes this test started; each is killed (its group too, while its identity holds) and
/// reaped on drop.
#[derive(Default)]
struct Started {
    children: Vec<(Child, Option<ProcessIdentity>)>,
}

impl Started {
    /// `sleep 300`, leading its own process group when `group`.
    fn sleep(&mut self, group: bool) -> ProcessIdentity {
        let mut c = Command::new("sleep");
        c.arg("300").stdin(Stdio::null());
        if group {
            c.process_group(0);
        }
        let child = c.spawn().unwrap();
        let id = procs::identity_of(&table(), child.id()).unwrap();
        self.children.push((child, group.then(|| id.clone())));
        id
    }

    /// `hostler run --heavy -- sleep 300`; its own command's group is ended on drop.
    fn heavy_run(&mut self, home: &Home) -> usize {
        let child = home
            .hostler()
            .args(["run", "--heavy", "--", "sleep", "300"])
            .current_dir(home.dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.children.push((child, None));
        self.children.len() - 1
    }

    /// Records the group of child `k`'s command, so drop ends it.
    fn owns_group(&mut self, k: usize, leader: ProcessIdentity) {
        self.children[k].1 = Some(leader);
    }

    fn pid(&self, k: usize) -> u32 {
        self.children[k].0.id()
    }

    /// Waits up to 20 s for child `k` to exit; its exit status.
    fn exited(&mut self, k: usize) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.children[k].0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child {k} did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn running(&mut self, k: usize) -> bool {
        self.children[k].0.try_wait().unwrap().is_none()
    }

    fn kill(&mut self, k: usize) {
        let (child, _) = &mut self.children[k];
        let _ = child.kill();
        let _ = child.wait();
    }

    /// SIGKILL to the group child `k`'s command leads, while that leader is the recorded one.
    fn kill_group(&mut self, k: usize) {
        if let Some(leader) = &self.children[k].1
            && alive(leader)
        {
            // SAFETY: killpg has no memory preconditions; the leader is our own command,
            // checked by its identity just now.
            unsafe { libc::killpg(leader.pid as libc::pid_t, libc::SIGKILL) };
        }
    }

    /// `hostler service run` as this test's own child, leading its own group (so a heavy entry
    /// may legitimately report it as its command, P5 T6-R1-1).
    fn service(&mut self, home: &Home) -> ProcessIdentity {
        let child = home
            .hostler()
            .args(["service", "run"])
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let id = procs::identity_of(&table(), child.id()).unwrap();
        self.children.push((child, Some(id.clone())));
        let paths = home.paths();
        until("the service to answer", Duration::from_secs(20), || {
            raw::runtime().block_on(Client::connect(&paths)).is_ok()
        });
        id
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        for (child, leader) in &mut self.children {
            if let Some(leader) = leader
                && alive(leader)
            {
                // SAFETY: killpg has no memory preconditions; the group's leader is our own
                // command, checked by its identity just now.
                unsafe { libc::killpg(leader.pid as libc::pid_t, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The running entry whose command has started, once there is one (`hostler run` reports it).
fn started_entry(conn: &mut Conn, not: &[&str]) -> HeavyEntry {
    let mut found = None;
    until("a started heavy task", Duration::from_secs(20), || {
        found = conn
            .snapshot()
            .running
            .into_iter()
            .find(|e| e.child.is_some() && !not.contains(&e.id.as_str()));
        found.is_some()
    });
    found.unwrap()
}

/// This test process, the holder of the entries it registers itself.
fn me() -> ProcessIdentity {
    current_identity(&table()).unwrap()
}

/// A granted entry held by this test process, its command `child` (this process's own child,
/// leading its own group) reported from this connection, as `hostler run` reports its own.
fn registered(conn: &mut Conn, child: &ProcessIdentity) -> String {
    let (id, outcome) = conn.acquire(me());
    assert!(
        matches!(outcome, AcquireOutcome::Granted { .. }),
        "{outcome:?}"
    );
    conn.call(Request::HeavyStarted(HeavyStarted {
        id: id.clone(),
        child: child.clone(),
        group: group_of(child),
    }))
    .unwrap();
    id
}

fn state(conn: &mut Conn, id: &str) -> Option<HeavyState> {
    let s = conn.snapshot();
    s.running
        .iter()
        .chain(&s.waiting)
        .find(|e| e.id == id)
        .map(|e| e.state)
}

fn code<T: std::fmt::Debug>(r: Result<T, HostlerError>) -> HostlerError {
    match r {
        Ok(v) => panic!("expected a refusal, got {v:?}"),
        Err(e) => e,
    }
}

fn group_of(child: &ProcessIdentity) -> RunGroup {
    RunGroup::UnixGroup {
        pgid: child.pid,
        boot_id: child.boot_id.clone(),
    }
}

/// The pause hook's release file, created; `heavy.stop` holds after marking while it exists.
fn pause_file(home: &Home) -> PathBuf {
    let pause = home.dir.path().join("pause-heavy-stop");
    std::fs::write(&pause, b"").unwrap();
    pause
}

fn reached(pause: &Path) -> PathBuf {
    let mut reached = pause.to_path_buf().into_os_string();
    reached.push(".reached");
    PathBuf::from(reached)
}

fn service_log(home: &Home) -> String {
    files_text(&home.home.join("logs"))
}

#[test]
fn stops_a_running_task_and_records_stopped() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let a = started.heavy_run(&home);
    let entry = started_entry(&mut conn, &[]);
    let child = entry.child.clone().unwrap();
    started.owns_group(a, child.clone());
    let b = started.heavy_run(&home);
    until("a waiting second task", Duration::from_secs(20), || {
        conn.snapshot().waiting.len() == 1
    });
    conn.prove();
    let outcome = conn.stop(&entry.id, &entry.id).unwrap();
    assert_eq!(
        outcome,
        QueueStopOutcome {
            id: entry.id.clone(),
            stopped: true,
            pids: vec![child.pid],
        }
    );
    assert!(!alive(&child));
    assert!(!procs::group_liveness(&group_of(&child), &table()));
    // `hostler run` ends with its command's code (SIGTERM: 128 + 15) and released meanwhile.
    assert_eq!(started.exited(a).code(), Some(143));
    let item = conn.history(&entry.id).unwrap();
    assert_eq!(item.outcome, HeavyOutcome::Stopped);
    assert_eq!(item.exit_code, Some(143));
    // The next waiter is granted and starts its command.
    let next = started_entry(&mut conn, &[&entry.id]);
    started.owns_group(b, next.child.clone().unwrap());
    assert_eq!(next.holder.pid, started.pid(b));
    assert!(
        service_log(&home).contains(&format!("stopping heavy task {} for peer pid", entry.id)),
        "the stop is logged"
    );
}

#[test]
fn without_grant_nothing_stops() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let child = started.sleep(true);
    let id = registered(&mut conn, &child);
    let e = code(conn.stop(&id, &id));
    assert_eq!(e.code, ErrorCode::ProofRequired);
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert!(alive(&child));
}

#[test]
fn wrong_confirmation_refused() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let child = started.sleep(true);
    let id = registered(&mut conn, &child);
    conn.prove();
    let e = code(conn.stop(&id, "yes"));
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, "type the task's id to confirm");
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert!(alive(&child));
}

#[test]
fn waiting_entry_refused() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let child = started.sleep(true);
    registered(&mut conn, &child);
    let (waiting, outcome) = conn.acquire(me());
    assert!(matches!(outcome, AcquireOutcome::Queued { .. }));
    conn.prove();
    let e = code(conn.stop(&waiting, &waiting));
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(
        e.message,
        format!("heavy task {waiting} is waiting, not running")
    );
    assert_eq!(
        e.recovery,
        Some(format!("hostler queue withdraw --id {waiting}"))
    );
    assert_eq!(state(&mut conn, &waiting), Some(HeavyState::Waiting));
}

/// The boot id rn-looky writes: `boot-<seconds of the boot>`.
fn looky_boot() -> String {
    #[cfg(target_os = "macos")]
    let seconds = {
        let out = Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let rest = text.split("sec =").nth(1).unwrap().trim().to_owned();
        rest.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
    };
    #[cfg(target_os = "linux")]
    let seconds = std::fs::read_to_string("/proc/stat")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("btime ").map(|s| s.trim().to_owned()))
        .unwrap();
    format!("boot-{seconds}")
}

#[test]
fn foreign_entry_refused() {
    let home = Home::new();
    let mut started = Started::default();
    let owner = started.sleep(false);
    let lstart = Command::new("ps")
        .args(["-o", "lstart=", "-p", &owner.pid.to_string()])
        .output()
        .unwrap();
    let ledger = home.dir.path().join("looky-ledger.json");
    let contents = json!({"heavy": {"entries": [{
        "id": "h1-abc123",
        "ticket": 1,
        "kind": "deps",
        "command": "yarn deps",
        "path": "/work/app",
        "owner": {
            "pid": owner.pid,
            "bootId": looky_boot(),
            "startTime": String::from_utf8(lstart.stdout).unwrap().trim(),
        },
        "state": "running",
        "enqueuedAt": "2026-01-01T00:00:00.000Z",
    }]}});
    std::fs::write(&ledger, contents.to_string()).unwrap();
    let ledger = ledger.to_str().unwrap().to_owned();
    start(&home, &[("HOSTLER_LOOKY_LEDGER", &ledger)]);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let mut conn = Conn::open(&home);
    until("the ledger's task", Duration::from_secs(20), || {
        conn.snapshot().foreign_running.len() == 1
    });
    conn.prove();
    let e = code(conn.stop("looky:h1-abc123", "looky:h1-abc123"));
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(
        e.message,
        "heavy task looky:h1-abc123 is rn-looky's; rn-looky's tasks are never stopped by hostler"
    );
    assert!(alive(&owner));
    assert_eq!(conn.snapshot().foreign_running.len(), 1);
}

/// R2-9: a task whose recorded command and group are the service's own is refused before
/// anything is marked, and the service lives on. The service runs as this test's own child,
/// leading its own group, so the entry passes `heavy.started`'s checks (T6-R1-1).
#[test]
fn service_never_targets_itself() {
    let home = Home::new();
    let mut started = Started::default();
    let service = started.service(&home);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let mut conn = Conn::open(&home);
    assert_eq!(conn.status().pid, service.pid);
    let id = registered(&mut conn, &service);
    conn.prove();
    let e = code(conn.stop(&id, &id));
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, "refusing to stop the hostler service itself");
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert!(alive(&service));
    assert_eq!(conn.status().pid, service.pid);
    conn.call(Request::HeavyForceRelease { id }).unwrap();
}

/// R2-4, spec §11.1: while held at the debug pause a real `hostler run --heavy` is shown as
/// stopping, refuses every other change and keeps its slot; a release meanwhile is accepted
/// and the entry stays until the stop records it, with its holder's exit code.
#[test]
fn stopping_lifecycle() {
    let home = Home::new();
    let pause = pause_file(&home);
    let hook = format!("heavy_stop:{}", pause.display());
    start(&home, &[("HOSTLER_TEST_PAUSE_AT", &hook)]);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let a = started.heavy_run(&home);
    let entry = started_entry(&mut conn, &[]);
    let child = entry.child.clone().unwrap();
    started.owns_group(a, child.clone());
    let id = entry.id.clone();
    let (waiter, outcome) = conn.acquire(me());
    assert!(matches!(outcome, AcquireOutcome::Queued { .. }));
    let stopping = {
        let home_paths = home.paths();
        let id = id.clone();
        std::thread::spawn(move || {
            let rt = raw::runtime();
            rt.block_on(async {
                let mut client = Client::connect(&home_paths).await.unwrap();
                client
                    .call(Request::SecretsProve(Prove {
                        password: SecretValue::new(PASSWORD.into()),
                        key_file: None,
                    }))
                    .await
                    .unwrap();
                client
                    .call(Request::HeavyStop {
                        id: id.clone(),
                        confirmation: id,
                    })
                    .await
            })
        })
    };
    until("the stop's pause", Duration::from_secs(20), || {
        reached(&pause).exists()
    });
    let snapshot = conn.snapshot();
    let stopping_entry = snapshot.running.iter().find(|e| e.id == id).unwrap();
    assert_eq!(stopping_entry.state, HeavyState::Stopping);
    assert_eq!(snapshot.waiting.len(), 1, "the next waiter is not granted");
    let being_stopped = format!("heavy entry {id}: the task is being stopped");
    let mut other = Conn::open(&home);
    other.prove();
    for e in [
        code(conn.call(Request::HeavyWithdraw {
            id: id.clone(),
            timed_out: false,
        })),
        code(conn.call(Request::HeavyForceRelease { id: id.clone() })),
        code(other.stop(&id, &id)),
    ] {
        assert_eq!(
            (e.code, e.message),
            (ErrorCode::BadRequest, being_stopped.clone())
        );
    }
    assert!(alive(&child), "nothing is signalled before the pause ends");
    // A release meanwhile is accepted; the entry stays until the stop records it.
    conn.call(Request::HeavyRelease(
        hostler::contract::heavy::HeavyRelease {
            id: id.clone(),
            exit_code: Some(7),
            outcome: HeavyOutcome::Failed,
        },
    ))
    .unwrap();
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Stopping));
    std::fs::remove_file(&pause).unwrap();
    let outcome: QueueStopOutcome =
        serde_json::from_value(stopping.join().unwrap().unwrap()).unwrap();
    assert_eq!(
        outcome,
        QueueStopOutcome {
            id: id.clone(),
            stopped: true,
            pids: vec![child.pid],
        }
    );
    assert!(!alive(&child));
    // The holder (`hostler run`) released with its command's code (SIGTERM: 143), the last
    // release before the stop recorded the entry.
    assert_eq!(started.exited(a).code(), Some(143));
    let item = conn.history(&id).unwrap();
    assert_eq!(
        (item.outcome, item.exit_code),
        (HeavyOutcome::Stopped, Some(143))
    );
    assert_eq!(state(&mut conn, &waiter), Some(HeavyState::Running));
    conn.call(Request::HeavyForceRelease { id: waiter })
        .unwrap();
}

/// Spec §11.1: a service killed during a stop leaves the task `running` after its restart; once
/// its processes are gone it is reaped, never recorded as stopped.
#[test]
fn restart_abandons_a_stop() {
    let home = Home::new();
    let pause = pause_file(&home);
    let hook = format!("heavy_stop:{}", pause.display());
    start(&home, &[("HOSTLER_TEST_PAUSE_AT", &hook)]);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let a = started.heavy_run(&home);
    let entry = started_entry(&mut conn, &[]);
    let child = entry.child.clone().unwrap();
    started.owns_group(a, child.clone());
    let id = entry.id.clone();
    let service = conn.status().pid;
    let stopping = {
        let home_paths = home.paths();
        let id = id.clone();
        std::thread::spawn(move || {
            let rt = raw::runtime();
            rt.block_on(async {
                let mut client = Client::connect(&home_paths).await.unwrap();
                client
                    .call(Request::SecretsProve(Prove {
                        password: SecretValue::new(PASSWORD.into()),
                        key_file: None,
                    }))
                    .await
                    .unwrap();
                client
                    .call(Request::HeavyStop {
                        id: id.clone(),
                        confirmation: id,
                    })
                    .await
            })
        })
    };
    until("the stop's pause", Duration::from_secs(20), || {
        reached(&pause).exists()
    });
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Stopping));
    let service_id = procs::identity_of(&table(), service).unwrap();
    assert!(alive(&service_id));
    // SAFETY: kill has no memory preconditions; the pid is this test's own service, checked
    // by its identity just now.
    assert_eq!(
        unsafe { libc::kill(service as libc::pid_t, libc::SIGKILL) },
        0
    );
    assert!(stopping.join().unwrap().is_err(), "the stop never answered");
    until(
        "the killed service to be gone",
        Duration::from_secs(20),
        || !alive(&service_id),
    );
    std::fs::remove_file(&pause).unwrap();
    start(&home, &[]);
    let mut conn = Conn::open(&home);
    assert_ne!(conn.status().pid, service);
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert!(alive(&child), "a restart signals nothing");
    assert!(
        service_log(&home).contains(&format!("abandoned an unfinished stop of {id}")),
        "the abandoned stop is logged"
    );
    // Its processes end without a release (the holder first): the usual reaping.
    started.kill(a);
    started.kill_group(a);
    until("the reaping", Duration::from_secs(20), || {
        conn.history(&id).is_some()
    });
    assert_eq!(conn.history(&id).unwrap().outcome, HeavyOutcome::Reaped);
}

/// A stop that fails (the takeover tests' hook makes every stopper fail) answers `stop_failed`
/// with what still runs; the task runs on and keeps its slot.
#[test]
fn unkillable_keeps_the_slot() {
    let home = ready(&[("HOSTLER_TEST_STOPPER_FAILS", "1")]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let holder = me();
    let child = started.sleep(true);
    let id = registered(&mut conn, &child);
    let (waiter, _) = conn.acquire(me());
    conn.prove();
    let e = code(conn.stop(&id, &id));
    assert_eq!(e.code, ErrorCode::StopFailed);
    assert_eq!(e.details["pids"], json!([holder.pid, child.pid]));
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert_eq!(state(&mut conn, &waiter), Some(HeavyState::Waiting));
    assert!(alive(&child));
    assert_eq!(conn.history(&id), None);
}

/// P5 T6-R1-1: a `heavy.started` that names anything but the holder's own live command leading
/// its own group is refused, and nothing is recorded for a later stop to signal. (A pid reused
/// after a valid report is never signalled either: `procs::stop`'s identity checks before every
/// signal, unit-tested with an injected change.)
#[test]
fn started_naming_a_foreign_process_is_refused() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let ours = started.sleep(true);
    let other = started.sleep(true);
    let (id, _) = conn.acquire(me());
    let service = procs::identity_of(&table(), conn.status().pid).unwrap();
    let stale = ProcessIdentity {
        start_time: ours.start_time + 1,
        ..ours.clone()
    };
    for (child, group, why) in [
        (
            stale,
            group_of(&ours),
            format!(
                "its command {} is not running with the reported identity",
                ours.pid
            ),
        ),
        (
            service.clone(),
            group_of(&service),
            format!(
                "its command {} is not a child of the entry's holder {}",
                service.pid,
                me().pid
            ),
        ),
        (
            ours.clone(),
            group_of(&other),
            format!(
                "its process group {} is not led by its command {}",
                other.pid, ours.pid
            ),
        ),
    ] {
        let e = code(conn.call(Request::HeavyStarted(HeavyStarted {
            id: id.clone(),
            child,
            group,
        })));
        assert_eq!(
            (e.code, e.message),
            (ErrorCode::BadRequest, format!("heavy entry {id}: {why}"))
        );
    }
    let entry = conn
        .snapshot()
        .running
        .into_iter()
        .find(|e| e.id == id)
        .unwrap();
    assert_eq!((entry.child, entry.group), (None, None));
    // The real report is accepted.
    conn.call(Request::HeavyStarted(HeavyStarted {
        id: id.clone(),
        child: ours.clone(),
        group: group_of(&ours),
    }))
    .unwrap();
    assert!(alive(&ours) && alive(&other) && alive(&service));
}

/// Spec §11.1 "nothing of it is alive": a holder outside the group that stays after its
/// command is gone keeps the task from counting as stopped; it keeps its slot.
#[test]
fn a_holder_that_stays_keeps_the_slot() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let holder = me();
    let child = started.sleep(true);
    let id = registered(&mut conn, &child);
    conn.prove();
    let began = Instant::now();
    let e = code(conn.stop(&id, &id));
    assert!(
        began.elapsed() >= Duration::from_secs(5),
        "the holder had its wait"
    );
    assert_eq!(e.code, ErrorCode::StopFailed);
    assert_eq!(e.details["pids"], json!([holder.pid]));
    assert!(!started.running(0), "the command's group was stopped");
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert_eq!(conn.history(&id), None);
}

/// R1-4: no client chooses an outcome. A release cannot claim `stopped`, and a `heavy.stop`
/// frame that claims success does not make a failed stop one.
#[test]
fn clients_cannot_forge_an_outcome() {
    let home = ready(&[("HOSTLER_TEST_STOPPER_FAILS", "1")]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let rt = raw::runtime();
    let mut wire = rt.block_on(raw::Raw::connect(&home));
    let child = started.sleep(true);
    let id = registered(&mut conn, &child);
    let release = rt.block_on(wire.call(
        2,
        "heavy.release",
        Some(&format!(
            r#"{{"id":"{id}","exit_code":0,"outcome":"stopped"}}"#
        )),
    ));
    assert_eq!(release["error"]["code"], "bad_request", "{release}");
    let proved = rt.block_on(wire.call(
        3,
        "secrets.prove",
        Some(&format!(r#"{{"password":"{PASSWORD}","key_file":null}}"#)),
    ));
    assert!(proved.get("result").is_some(), "{}", proved["error"]);
    let stop = rt.block_on(wire.call(
        4,
        "heavy.stop",
        Some(&format!(
            r#"{{"id":"{id}","confirmation":"{id}","outcome":"stopped","stopped":true,"pids":[]}}"#
        )),
    ));
    assert_eq!(stop["error"]["code"], "stop_failed", "{stop}");
    assert_eq!(state(&mut conn, &id), Some(HeavyState::Running));
    assert_eq!(conn.history(&id), None);
}

/// P5-L10: a force-release frees the slot without stopping anything and records
/// `force_released`; a frame that adds `outcome: "stopped"` cannot make it `stopped`.
#[test]
fn force_release_cannot_forge_stopped() {
    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let rt = raw::runtime();
    let mut wire = rt.block_on(raw::Raw::connect(&home));
    let child = started.sleep(true);
    let id = registered(&mut conn, &child);
    let forced = rt.block_on(wire.call(
        2,
        "heavy.force_release",
        Some(&format!(
            r#"{{"id":"{id}","outcome":"stopped","exit_code":0}}"#
        )),
    ));
    assert!(forced.get("result").is_some(), "{forced}");
    let item = conn.history(&id).unwrap();
    assert_eq!(
        (item.outcome, item.exit_code),
        (HeavyOutcome::ForceReleased, None)
    );
    assert!(alive(&child), "nothing was stopped");
}

/// P5-L10: a task whose processes were all gone before the stop signalled anything ended on
/// its own: `reaped`, with its holder's exit code, `stopped: false`; the CLI's `--json` says
/// `stopped: false` too (P5-L14, R1-3), and exits 0.
#[test]
fn a_task_that_ended_by_itself_is_reaped() {
    let home = Home::new();
    let pause = pause_file(&home);
    let hook = format!("heavy_stop:{}", pause.display());
    start(&home, &[("HOSTLER_TEST_PAUSE_AT", &hook)]);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let a = started.heavy_run(&home);
    let entry = started_entry(&mut conn, &[]);
    let child = entry.child.clone().unwrap();
    started.owns_group(a, child.clone());
    let id = entry.id.clone();
    let tty = home.dir.path().join("tty-queue-stop");
    std::fs::write(&tty, format!("{PASSWORD}\n")).unwrap();
    let cli = home
        .hostler()
        .args([
            "queue",
            "stop",
            &id,
            "--force",
            "--confirmation",
            &id,
            "--json",
        ])
        .env("HOSTLER_TEST_TTY", &tty)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    until("the stop's pause", Duration::from_secs(20), || {
        reached(&pause).exists()
    });
    // It ends on its own meanwhile: its command dies, its holder releases and exits.
    started.kill_group(a);
    assert_eq!(started.exited(a).code(), Some(137));
    std::fs::remove_file(&pause).unwrap();
    let out = cli.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let envelope: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        envelope,
        json!({"ok": true, "data": {"id": id, "stopped": false, "pids": [child.pid]}, "error": null})
    );
    let item = conn.history(&id).unwrap();
    assert_eq!(
        (item.outcome, item.exit_code),
        (HeavyOutcome::Reaped, Some(137))
    );
}

/// `hostler queue stop` is a person command: `--force` and the confirmation first, then a
/// terminal (none here: refused), then the master password (scripted: a wrong one, then the
/// right one after the service's short wait).
#[test]
fn cli_queue_stop_is_a_person_command() {
    let home = ready(&[("HOSTLER_TEST_PASSWORD_BACKOFF_SCALE_MS", "100")]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let a = started.heavy_run(&home);
    let entry = started_entry(&mut conn, &[]);
    let child = entry.child.clone().unwrap();
    started.owns_group(a, child.clone());
    let id = entry.id.as_str();
    let stop = |args: &[&str]| {
        home.hostler()
            .args(["queue", "stop", id])
            .args(args)
            .output()
            .unwrap()
    };

    let out = stop(&["--confirmation", id]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!(
            "stopping {id} ends its processes; An agent uses --force only with the operator's \
             explicit permission."
        )),
        "{}",
        stderr(&out)
    );
    let out = stop(&["--force", "--confirmation", "yes", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let envelope: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(envelope["error"]["code"], "bad_request");
    assert_eq!(
        envelope["error"]["message"],
        "type the task's id to confirm"
    );
    let out = stop(&["--force", "--confirmation", id, "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let envelope: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(envelope["error"]["code"], "not_interactive");
    assert!(alive(&child), "nothing was stopped without a person");
    assert_eq!(state(&mut conn, id), Some(HeavyState::Running));

    let tty = home.dir.path().join("tty-queue-stop");
    std::fs::write(&tty, format!("wrong-password-1\n{PASSWORD}\n")).unwrap();
    let out = home
        .hostler()
        .args([
            "queue",
            "stop",
            id,
            "--force",
            "--confirmation",
            id,
            "--json",
        ])
        .env("HOSTLER_TEST_TTY", &tty)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        !stdout(&out).contains(PASSWORD) && !stderr(&out).contains(PASSWORD),
        "the password is never printed"
    );
    let envelope: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        envelope,
        json!({"ok": true, "data": {"id": id, "stopped": true, "pids": [child.pid]}, "error": null})
    );
    assert!(!alive(&child));
    assert_eq!(started.exited(a).code(), Some(143));
    assert_eq!(conn.history(id).unwrap().outcome, HeavyOutcome::Stopped);
}

/// R2-9 from the CLI's side: a takeover whose plan would stop a run recorded with the service's
/// own pid and group is refused before anything is signalled, and the service lives on.
#[test]
fn takeover_never_targets_the_service() {
    use hostler::contract::claims::{
        ClaimOutcome, ClaimRequest, ClaimStarted, Purpose, ResourceId, Selector,
    };

    let home = ready(&[]);
    let mut started = Started::default();
    let mut conn = Conn::open(&home);
    let (a, b) = (home.make_repo("a"), home.make_repo("b"));
    let workspace = home.workspace(&a).unwrap().id;
    let holder = started.sleep(false);
    let status = conn.status();
    let service = procs::identity_of(&table(), status.pid).unwrap();
    let request = ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId::screen(),
        },
        workspace_id: workspace,
        requester: holder,
        purpose: Purpose::Lease {
            command: "sleep 300".into(),
        },
        reentrant_tokens: Vec::new(),
        no_wait: false,
    };
    let outcome: ClaimOutcome =
        serde_json::from_value(conn.call(Request::ClaimsAcquire(request)).unwrap()).unwrap();
    let ClaimOutcome::Granted { claim } = outcome else {
        panic!("the screen was not granted: {outcome:?}");
    };
    conn.call(Request::ClaimsStarted(ClaimStarted {
        claim_id: claim.id.clone(),
        child: service.clone(),
        group: group_of(&service),
    }))
    .unwrap();
    let out = home
        .hostler()
        .args(["hold", "screen", "--force", "--confirmation", "screen"])
        .current_dir(&b)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("contains the hostler service, which hostler never stops"),
        "{}",
        stderr(&out)
    );
    assert!(alive(&service));
    assert_eq!(conn.status().pid, status.pid);
    let held = home
        .snapshot()
        .resources
        .into_iter()
        .flat_map(|r| r.claims)
        .any(|c| c.id == claim.id);
    assert!(held, "the claim stays");
}
