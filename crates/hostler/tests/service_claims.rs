//! The claims engine in the real service (plan P3a Task 5): restarts, the idle-stop rule and the
//! contract version.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Home, stderr};
use hostler::client::Client;
use hostler::contract::ErrorCode;
use hostler::contract::claims::{
    ClaimOutcome, ClaimRelease, ClaimRequest, Purpose, ResourceId, Selector,
};
use hostler::contract::wire::Request;
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use serde_json::Value;

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

fn workspace(home: &Home, name: &str) -> String {
    let repo = home.make_repo(name);
    home.workspace(&repo).unwrap().id
}

fn request(workspace_id: &str, purpose: Purpose) -> ClaimRequest {
    ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId::screen(),
        },
        workspace_id: workspace_id.into(),
        requester: me(),
        purpose,
        reentrant_tokens: Vec::new(),
        no_wait: false,
    }
}

fn lease(workspace_id: &str) -> ClaimRequest {
    request(
        workspace_id,
        Purpose::Lease {
            command: "make test".into(),
        },
    )
}

fn hold(workspace_id: &str) -> ClaimRequest {
    request(
        workspace_id,
        Purpose::Hold {
            ttl_ms: Some(30 * 60 * 1000),
            bound_to: None,
            distinct: false,
        },
    )
}

fn outcome(home: &Home, request: Request) -> ClaimOutcome {
    serde_json::from_value(home.call(request).unwrap()).unwrap()
}

fn idle_stop(home: &Home) -> Result<Value, hostler::contract::HostlerError> {
    let paths = home.paths();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            Client::connect_lifecycle(&paths)
                .await?
                .call(Request::ServiceShutdown {
                    if_idle: true,
                    refuse_if_unlocked: false,
                })
                .await
        })
}

#[test]
fn claims_survive_restart() {
    let home = Home::new();
    let (a, b) = (workspace(&home, "a"), workspace(&home, "b"));
    let held = hold(&a);
    assert!(matches!(
        outcome(&home, Request::ClaimsAcquire(held.clone())),
        ClaimOutcome::Granted { .. }
    ));
    let waiter = lease(&b);
    assert!(matches!(
        outcome(&home, Request::ClaimsAcquire(waiter.clone())),
        ClaimOutcome::Queued { .. }
    ));

    // A hold never blocks a stop; the restart keeps both rows.
    let restart = home
        .hostler()
        .args(["service", "restart"])
        .output()
        .unwrap();
    assert!(restart.status.success(), "{}", stderr(&restart));
    let snapshot = home.snapshot();
    let screen = &snapshot.resources[0];
    assert_eq!(screen.claims.len(), 1);
    assert_eq!(screen.claims[0].id, held.id);
    assert_eq!(screen.waiting[0].request_id, waiter.id);
    assert!(matches!(
        outcome(
            &home,
            Request::ClaimsAttach {
                request_id: waiter.id.clone()
            }
        ),
        ClaimOutcome::Queued { .. }
    ));

    // The waiter reattaches by id and gets it once the hold is released.
    home.call(Request::ClaimsRelease(ClaimRelease {
        claim_id: held.id.clone(),
        workspace_id: a,
        requester: me(),
        exit_code: None,
    }))
    .unwrap();
    match outcome(
        &home,
        Request::ClaimsAttach {
            request_id: waiter.id.clone(),
        },
    ) {
        ClaimOutcome::Granted { claim } => assert_eq!(claim.id, waiter.id),
        other => panic!("{other:?}"),
    }
}

#[test]
fn idle_stop_refused_while_a_lease_runs() {
    let home = Home::new();
    let a = workspace(&home, "a");
    let run = lease(&a);
    assert!(matches!(
        outcome(&home, Request::ClaimsAcquire(run.clone())),
        ClaimOutcome::Granted { .. }
    ));
    let e = idle_stop(&home).unwrap_err();
    assert_eq!(e.code, ErrorCode::ServiceBusy);
    let leases = e.details["leases"].as_array().unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0]["id"], run.id.as_str());
    assert!(e.details["running"].as_array().unwrap().is_empty());
    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(!stop.status.success());
    // Still serving; --force stops it.
    assert!(home.call(Request::ClaimsSnapshot).is_ok());
    let forced = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(forced.status.success(), "{}", stderr(&forced));
}

#[test]
fn idle_stop_not_refused_by_holds() {
    let home = Home::new();
    let a = workspace(&home, "a");
    assert!(matches!(
        outcome(&home, Request::ClaimsAcquire(hold(&a))),
        ClaimOutcome::Granted { .. }
    ));
    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(stop.status.success(), "{}", stderr(&stop));
}

#[test]
fn v2_client_gets_contract_mismatch() {
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let home = Home::new();
    home.start_service();
    let paths = home.paths();
    let answer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let name = hostler::service::server::socket_name(&paths.socket).unwrap();
            let stream = Stream::connect(name).await.unwrap();
            let (recv, mut send) = tokio::io::split(stream);
            send.write_all(
                b"{\"id\":1,\"method\":\"hello\",\"params\":{\"contract_version\":2,\"client\":\"p2\"}}\n",
            )
            .await
            .unwrap();
            let mut lines = BufReader::new(recv).lines();
            lines.next_line().await.unwrap().unwrap()
        });
    let frame: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(frame["error"]["code"], "contract_mismatch", "{frame}");
    assert_eq!(frame["error"]["details"]["service_contract"], 4);
}

#[test]
fn status_overview_has_every_part() {
    let home = Home::new();
    let a = workspace(&home, "a");
    assert!(matches!(
        outcome(&home, Request::ClaimsAcquire(hold(&a))),
        ClaimOutcome::Granted { .. }
    ));
    let overview: hostler::contract::claims::Overview =
        serde_json::from_value(home.call(Request::StatusOverview).unwrap()).unwrap();
    assert_eq!(overview.claims.resources[0].claims.len(), 1);
    assert!(overview.heavy.running.is_empty());
    assert_eq!(
        overview.secrets.state,
        hostler::contract::secrets::LockState::Missing
    );
    let _ = Path::new(&overview.secrets.database);
}

fn start_with(home: &Home, env: &[(&str, &str)]) {
    let mut start = home.hostler();
    for (name, value) in env {
        start.env(name, value);
    }
    let out = start.args(["service", "start"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}

/// T5-1/T5-2: a workspace on a dead mount — its path probe hangs — neither freezes the claims
/// engine nor blocks a stop, and its hold is not stranded (unknown counts as existing).
#[test]
fn blocked_path_probe_leaves_claims_and_stop_answering() {
    let home = Home::new();
    let dead = format!("deadmount-{}", uuid::Uuid::new_v4().simple());
    start_with(
        &home,
        &[
            ("HOSTLER_TEST_PATH_PROBE_BLOCK", dead.as_str()),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
        ],
    );
    let a = workspace(&home, &dead);
    let held = hold(&a);
    assert!(matches!(
        outcome(&home, Request::ClaimsAcquire(held.clone())),
        ClaimOutcome::Granted { .. }
    ));
    // Many ticks and probe rounds, each probe of that path hanging.
    std::thread::sleep(Duration::from_millis(1500));
    let started = Instant::now();
    let snapshot = home.snapshot();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(snapshot.resources[0].claims[0].id, held.id);
    let started = Instant::now();
    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(stop.status.success(), "{}", stderr(&stop));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// T5-1: a slow checkout (a network mount) stalls only its own `workspace.resolve`.
#[test]
fn slow_workspace_resolve_leaves_claims_answering() {
    let home = Home::new();
    start_with(&home, &[("HOSTLER_TEST_RESOLVE_PAUSE_MS", "3000")]);
    let repo = home.make_repo("slow");
    std::thread::scope(|scope| {
        let resolving = scope.spawn(|| home.workspace(&repo));
        std::thread::sleep(Duration::from_millis(500));
        let started = Instant::now();
        home.call(Request::ClaimsSnapshot).unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "claims.snapshot waited {:?} behind a resolve",
            started.elapsed()
        );
        let info = resolving.join().unwrap().unwrap();
        assert_eq!(
            info.outcome,
            hostler::contract::claims::WorkspaceOutcome::Registered
        );
    });
}

/// T5-1: a stop never waits on the claims engine without a bound: an idle stop is refused
/// after 3 s instead of hanging (with the heavy lock held), and `--force` stops anyway.
#[test]
fn stop_is_bounded_while_the_claims_engine_is_stuck() {
    let home = Home::new();
    let marker = home.dir.path().join("stall");
    start_with(
        &home,
        &[
            ("HOSTLER_TEST_CLAIMS_STALL", marker.to_str().unwrap()),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
        ],
    );
    std::fs::write(&marker, "").unwrap();
    // The next tick takes it and holds the claims lock for 8 s.
    let taken = Instant::now();
    while marker.exists() && taken.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!marker.exists());
    let started = Instant::now();
    let e = idle_stop(&home).unwrap_err();
    assert_eq!(e.code, ErrorCode::ServiceBusy, "{e:?}");
    assert!(e.message.contains("has not answered"), "{}", e.message);
    assert!(
        e.message
            .contains("An agent uses --force only with the operator's explicit permission."),
        "{}",
        e.message
    );
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "{:?}",
        started.elapsed()
    );
    // T5-R2: the CLI shows that refusal in its own words, with the D9 sentence.
    let stop = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(!stop.status.success());
    let text = stderr(&stop);
    assert!(text.contains("has not answered"), "{text}");
    assert!(
        text.contains("An agent uses --force only with the operator's explicit permission."),
        "{text}"
    );
    // Not the heavy-queue wording, which would be wrong here.
    assert!(!text.contains("The tasks keep running"), "{text}");
    let forced = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(forced.status.success(), "{}", stderr(&forced));
}

/// T6-R2-3: a `heavy.acquire` waiting for a stuck claims engine never holds up the heavy queue.
#[test]
fn heavy_queue_answers_while_an_acquire_waits_for_claims() {
    let home = Home::new();
    let marker = home.dir.path().join("stall");
    start_with(
        &home,
        &[
            ("HOSTLER_TEST_CLAIMS_STALL", marker.to_str().unwrap()),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
        ],
    );
    std::fs::write(&marker, "").unwrap();
    let taken = Instant::now();
    while marker.exists() && taken.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!marker.exists());
    std::thread::scope(|scope| {
        let acquiring = scope.spawn(|| {
            home.call(Request::HeavyAcquire(
                hostler::contract::heavy::HeavyAcquire {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind: "gradle".into(),
                    command: "./gradlew build".into(),
                    cwd: "/work".into(),
                    workspace_root: None,
                    holder: me(),
                    reentrant_token: None,
                    no_wait: false,
                },
            ))
        });
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        home.call(Request::QueueSnapshot).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "queue.snapshot waited {:?} behind a heavy.acquire",
            started.elapsed()
        );
        // The acquire itself goes through once the claims engine's bound has passed.
        assert!(acquiring.join().unwrap().is_ok());
    });
}

/// L44 (CR-B-1): a service started by a hostler command inside a run never inherits the run's
/// values. Its environment is read with `ps eww` (macOS) or `/proc/<pid>/environ` (Linux, where
/// it is readable: the service is not dumpable, so only as root).
#[cfg(unix)]
#[test]
fn a_self_started_service_never_inherits_a_runs_values() {
    let home = Home::new();
    let start = home
        .hostler()
        .env("HOSTLER_ACCOUNT_SESSION", "acct-canary-77")
        .env("API_KEY", "secret-canary-88")
        .env("HOSTLER_SECRET_RUN", "1")
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(start.status.success(), "{}", stderr(&start));
    let status: hostler::contract::heavy::ServiceStatus =
        serde_json::from_value(home.call(Request::ServiceStatus).unwrap()).unwrap();
    let pid = status.pid;
    let environment = if cfg!(target_os = "linux") {
        std::fs::read(format!("/proc/{pid}/environ"))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).replace('\0', "\n"))
    } else {
        std::process::Command::new("ps")
            .args(["eww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(environment) = environment.filter(|e| e.contains("HOSTLER_HOME=")) else {
        eprintln!("skipped: the service's environment is not readable here");
        return;
    };
    for canary in ["acct-canary-77", "secret-canary-88", "HOSTLER_SECRET_RUN"] {
        assert!(
            !environment.contains(canary),
            "{canary} reached the service"
        );
    }
}

/// L45 (W-13): a relative `HOSTLER_HOME` is refused before any socket name is derived.
#[test]
fn a_relative_hostler_home_is_refused() {
    let home = Home::new();
    let out = home
        .hostler()
        .env("HOSTLER_HOME", "relative-home")
        .current_dir(home.dir.path())
        .args(["service", "start"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("HOSTLER_HOME must be an absolute path"),
        "{}",
        stderr(&out)
    );
    assert!(!home.dir.path().join("relative-home").exists());
}

/// L48 (FR-4): `run --secret` refuses a name the self-started service would keep (exit 2).
#[test]
fn a_secret_under_a_name_the_service_keeps_is_refused() {
    let home = Home::new();
    let repo = home.make_repo("app");
    for env in ["PATH", "LC_ALL", "HOSTLER_X"] {
        let out = home
            .hostler()
            .args(["run", "--secret", &format!("API_KEY={env}"), "--", "true"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{env}: {}", stderr(&out));
        assert!(
            stderr(&out).contains(&format!("the environment variable {env} is reserved")),
            "{}",
            stderr(&out)
        );
    }
}

/// FR-6: `hold` and `release` refuse a service of another contract version with 78, as
/// `run --screen` does.
#[test]
fn hold_and_release_exit_78_on_a_contract_mismatch() {
    let home = Home::new();
    let repo = home.make_repo("app");
    // `service start` itself reports the mismatch; the service it started keeps running.
    let _ = home
        .hostler()
        .env("HOSTLER_TEST_CONTRACT_VERSION", "2")
        .args(["service", "start"])
        .output()
        .unwrap();
    for args in [&["hold", "screen"][..], &["release", "some-claim-id"]] {
        let out = home
            .hostler()
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(78), "{args:?}: {}", stderr(&out));
    }
}

/// This machine's boot instant as rn-looky records it (`boot-<kern.boottime seconds>`).
#[cfg(unix)]
fn looky_boot_id() -> String {
    #[cfg(target_os = "macos")]
    let seconds = {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let rest = text.split("sec =").nth(1).unwrap().trim().to_owned();
        rest.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
    };
    #[cfg(not(target_os = "macos"))]
    let seconds = std::fs::read_to_string("/proc/stat")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("btime "))
        .unwrap()
        .trim()
        .to_owned();
    format!("boot-{seconds}")
}

/// O-1, Global Constraint 3: rn-looky's ledger is checked with `ps` (and `sysctl` on macOS),
/// never under the heavy lock that `claims.acquire` and every tick take. Once each of those takes
/// 2.6 s (a loaded machine), an acquire still answers within the client's 5 s, and rn-looky's
/// running task still counts against the heavy capacity. O-1b: the slow read (two or three of
/// them, ≥ 5 s) is in the service log.
#[cfg(unix)]
#[test]
fn slow_looky_ledger_checks_never_hold_up_claims() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let path = std::env::var("PATH").unwrap_or_default();
    let shims = home.dir.path().join("slow-tools");
    let slow = home.dir.path().join("slow");
    std::fs::create_dir_all(&shims).unwrap();
    for tool in ["ps", "sysctl"] {
        let shim = shims.join(tool);
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n[ -e '{}' ] && sleep 2.6\nPATH='{path}' exec {tool} \"$@\"\n",
                slow.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // This test process as rn-looky's running heavy task.
    let lstart = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    let ledger = home.dir.path().join("ledger.json");
    let entry = serde_json::json!({
        "id": "t1", "ticket": 1, "kind": "deps", "command": "yarn deps", "path": "/work/app",
        "owner": {"pid": std::process::id(), "bootId": looky_boot_id(),
                  "startTime": String::from_utf8(lstart.stdout).unwrap().trim()},
        "state": "running",
    });
    let contents = serde_json::json!({"heavy": {"capacity": 1, "entries": [entry]}});
    std::fs::write(&ledger, contents.to_string()).unwrap();
    let start = home
        .hostler()
        .env("PATH", format!("{}:{path}", shims.display()))
        .env("HOSTLER_LOOKY_LEDGER", &ledger)
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(start.status.success(), "service start: {}", stderr(&start));
    let ws = workspace(&home, "a");
    std::fs::write(&slow, "").unwrap();
    for _ in 0..3 {
        let started = Instant::now();
        let req = hold(&ws);
        let outcome = home.call(Request::ClaimsAcquire(req.clone()));
        let took = started.elapsed();
        let outcome: ClaimOutcome = serde_json::from_value(outcome.unwrap()).unwrap();
        assert!(took < Duration::from_secs(3), "{took:?}");
        let ClaimOutcome::Granted { claim } = outcome else {
            panic!("{outcome:?}")
        };
        home.call(Request::ClaimsRelease(ClaimRelease {
            claim_id: claim.id,
            workspace_id: ws.clone(),
            requester: me(),
            exit_code: None,
        }))
        .unwrap();
    }
    let queue: hostler::contract::heavy::QueueSnapshot =
        serde_json::from_value(home.call(Request::QueueSnapshot).unwrap()).unwrap();
    assert!(
        queue.foreign_running.iter().any(|e| e.id == "looky:t1"),
        "{queue:?}"
    );
    log_until(&home, "slow rn-looky ledger read: ");
}

/// The service log so far (every daily file).
fn service_log(home: &Home) -> String {
    let Ok(files) = std::fs::read_dir(home.home.join("logs")) else {
        return String::new();
    };
    files
        .filter_map(Result::ok)
        .filter(|f| f.file_name().to_string_lossy().starts_with("service.log"))
        .map(|f| std::fs::read_to_string(f.path()).unwrap_or_default())
        .collect()
}

/// Waits up to 20 s for a log line containing `needle`; answers the log.
fn log_until(home: &Home, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let log = service_log(home);
        if log.contains(needle) {
            return log;
        }
        assert!(Instant::now() < deadline, "no {needle:?} in:\n{log}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// O-1b: a stalled claims tick and a request that waited behind it are logged, with the time
/// spent waiting for each lock and the method only; a fast request is not.
#[test]
fn slow_ticks_and_requests_are_logged_with_their_lock_waits() {
    let home = Home::new();
    let marker = home.dir.path().join("stall");
    start_with(
        &home,
        &[
            ("HOSTLER_TEST_CLAIMS_STALL", marker.to_str().unwrap()),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
        ],
    );
    home.call(Request::WorkspaceList).unwrap();
    std::fs::write(&marker, "").unwrap();
    let taken = Instant::now();
    while marker.exists() && taken.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!marker.exists());
    // Behind the 8 s stall: the client gives up after 5 s, the service answers later.
    let e = home.call(Request::ClaimsSnapshot).unwrap_err();
    assert!(e.message.contains("did not answer within 5s"), "{e:?}");
    let log = log_until(&home, "slow request claims.snapshot: ");
    let line = log
        .lines()
        .find(|l| l.contains("slow request claims.snapshot: "))
        .unwrap();
    let claims_ms: u64 = line
        .split("claims lock ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|ms| ms.parse().ok())
        .unwrap_or_else(|| panic!("{line}"));
    assert!(claims_ms >= 2000, "{line}");
    assert!(line.contains("(heavy lock 0 ms, claims lock "), "{line}");
    let log = log_until(&home, "slow claims tick: ");
    assert!(!log.contains("slow request workspace.list"), "{log}");
}
