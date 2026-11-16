//! agent-device sessions against the fake tools (P3b Task 9b): the stopper, and the reader in a
//! real service. No real agent-device, node, adb or xcrun runs here; every logged call must
//! pass the exact allowlist (`FakeTools`' drop check).
//!

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use common::{Home, stderr};
use fake_tools::FakeTools;
use hostler::cli::takeover::{HolderStopper, StopError};
use hostler::contract::ErrorCode;
use hostler::contract::claims::{
    Blocker, ClaimOutcome, ClaimRequest, Purpose, ResourceId, Selector, StopTarget,
};
use hostler::contract::devices::DevicesSnapshot;
use hostler::contract::wire::Request;
use hostler::devices::agent_device::{AgentDeviceStopper, STATUS_FIX};
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use serde_json::{Value, json};

const EMULATOR: &str = "emulator-5554";
const AVD: &str = "device:android:avd:Pixel_9";

/// `device status --json` listing `sessions` (`(session, device id, workspace)`). The owner is
/// this test process, recorded as started later than it did: alive and not newer (L53).
fn status(sessions: &[(&str, &str, &str)]) -> String {
    status_owned(sessions, std::process::id())
}

fn status_owned(sessions: &[(&str, &str, &str)], pid: u32) -> String {
    let claims: Vec<Value> = sessions
        .iter()
        .map(|(session, id, workspace)| {
            json!({
                "deviceKey": format!("local:android:none:{id}"),
                "classification": "live",
                "device": {"id": id, "kind": "emulator", "name": "Pixel_9", "platform": "android"},
                "owner": {"session": session, "workspace": workspace,
                          "stateDir": "/tmp/agent-device-state", "pid": pid,
                          "startTime": "Fri Jan  1 00:00:00 2100"}
            })
        })
        .collect();
    json!({"success": true, "data": {"claims": claims, "hiddenStaleClaims": 0}}).to_string()
}

fn status_entry(tools: &FakeTools, sessions: &[(&str, &str, &str)]) -> Value {
    json!({"tool": "node",
           "args_prefix": [tools.agent_device_script().to_str().unwrap(), "device", "status"],
           "stdout": status(sessions)})
}

/// The stop target the service would plan for session `s1` on the emulator.
fn target(tools: &FakeTools) -> StopTarget {
    StopTarget::Foreign {
        source: "agent-device".into(),
        resource: ResourceId(AVD.into()),
        data: json!({
            "session": "s1",
            "state_dir": "/tmp/agent-device-state",
            "platform": "android",
            "id": EMULATOR,
            "node": tools.bin(hostler::devices::tools::Tool::Node).to_str().unwrap(),
            "script": tools.agent_device_script().to_str().unwrap(),
        }),
    }
}

fn stop(tools: &FakeTools) -> Result<(hostler::contract::claims::StopEvidence, bool), StopError> {
    AgentDeviceStopper.stop_counted(
        &target(tools),
        Duration::from_secs(1),
        &AtomicBool::new(false),
    )
}

#[test]
fn stopper_uses_the_services_node_and_script() {
    let tools = FakeTools::new();
    let script = tools.agent_device_script();
    let script = script.to_str().unwrap();
    let gone = status_entry(&tools, &[]);
    tools.script(json!([
        {"tool": "node", "args_prefix": [script, "close"], "stdout": "{\"success\": true}",
         "then_write": [gone]},
        status_entry(&tools, &[("s1", EMULATOR, "/work/app")]),
    ]));
    let (_, counted) = stop(&tools).unwrap();
    assert!(counted, "it signalled");
    let calls = tools.calls();
    let close = calls
        .iter()
        .find(|c| c.args.get(1).map(String::as_str) == Some("close"))
        .unwrap();
    assert_eq!(
        close.args,
        [
            script,
            "close",
            "--session",
            "s1",
            "--platform",
            "android",
            "--serial",
            EMULATOR,
            "--json"
        ]
    );
    assert_eq!(
        close.env,
        ["AGENT_DEVICE_STATE_DIR"],
        "its own daemon's state dir"
    );
    assert!(calls.iter().all(|c| c.tool == "node"));
}

#[test]
fn a_session_already_closed_is_not_counted() {
    let tools = FakeTools::new();
    tools.script(json!([status_entry(&tools, &[])]));
    let (_, counted) = stop(&tools).unwrap();
    assert!(!counted);
    assert!(
        tools
            .calls()
            .iter()
            .all(|c| c.args.get(1).map(String::as_str) != Some("close"))
    );
}

/// P3bT9b-9: a slow status (3.2 s a call after the first) never stretches the 20 s: each call
/// gets what is left of it (`stopper_calls_are_bounded_by_what_is_left` checks the bound).
#[test]
fn stopper_gives_up_at_20_s() {
    let tools = FakeTools::new();
    let script = tools.agent_device_script();
    let mut first = status_entry(&tools, &[("s1", EMULATOR, "/work/app")]);
    first["once"] = json!(true);
    let mut listed = status_entry(&tools, &[("s1", EMULATOR, "/work/app")]);
    listed["delay_ms"] = json!(3200);
    tools.script(json!([
        {"tool": "node", "args_prefix": [script.to_str().unwrap(), "close"],
         "stdout": "{\"success\": true}"},
        first,
        listed,
    ]));
    let start = Instant::now();
    let e = stop(&tools).unwrap_err();
    assert!(matches!(e, StopError::StillAlive), "{e:?}");
    let took = start.elapsed();
    // P3bT9b-9: every call within what is left of the 20 s.
    assert!(
        took >= Duration::from_secs(19) && took < Duration::from_secs(21),
        "{took:?}"
    );
}

#[test]
fn older_daemon_owner_is_reported() {
    let tools = FakeTools::new();
    let script = tools.agent_device_script();
    tools.script(json!([
        {"tool": "node", "args_prefix": [script.to_str().unwrap(), "close"], "rc": 1,
         "stderr": "Error: device is held under another registration ownership\n"},
        status_entry(&tools, &[("s1", EMULATOR, "/work/app")]),
    ]));
    let e = stop(&tools).unwrap_err();
    let StopError::Unknown(why) = e else {
        panic!("{e:?}");
    };
    assert_eq!(
        why,
        "an older agent-device daemon owns this device; stop it with the agent-device version \
         that started it"
    );
    // Any other failure names its first line, sanitized.
    tools.script(json!([
        {"tool": "node", "args_prefix": [script.to_str().unwrap(), "close"], "rc": 2,
         "stderr": "\n  boom \u{1b}[2J\nmore\n"},
        status_entry(&tools, &[("s1", EMULATOR, "/work/app")]),
    ]));
    let StopError::Unknown(why) = stop(&tools).unwrap_err() else {
        panic!("not Unknown");
    };
    assert!(why.starts_with("agent-device close failed: boom"), "{why}");
    assert!(!why.contains('\u{1b}'));
}

// ---- the reader in a real service ----

fn android(tools: &FakeTools, sessions: Value) -> Value {
    android_with(tools, sessions, "0.21.36", 0)
}

/// `android`, with agent-device answering `--version` with `version` after `version_delay_ms`.
fn android_with(tools: &FakeTools, sessions: Value, version: &str, version_delay_ms: u64) -> Value {
    // `{"__status": <stdout>, "__delay_ms"?: n}`: the status entry, built here with the tools'
    // script path.
    let sessions = match sessions.get("__status") {
        Some(stdout) => json!({"tool": "node",
            "args_prefix": [tools.agent_device_script().to_str().unwrap(), "device", "status"],
            "stdout": stdout, "delay_ms": sessions.get("__delay_ms").cloned().unwrap_or(json!(0))}),
        None => sessions,
    };
    json!([
        {"tool": "adb", "args_prefix": ["version"],
         "stdout": "Android Debug Bridge version 1.0.41\n"},
        {"tool": "adb", "args_prefix": ["devices", "-l"],
         "stdout": format!("List of devices attached\n{EMULATOR}          device product:sdk model:sdk_gphone64 device:emu64a transport_id:2\n\n")},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", "getprop sys.boot_completed"],
         "stdout": "1\n"},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell"],
         "stdout": "EMULATOR35X1X1X0\nsdk_gphone64_arm64\n15\n35\nPixel_9\n1\n\n"},
        {"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": "Pixel_9\n"},
        sessions,
        {"tool": "node", "args_prefix": [tools.agent_device_script().to_str().unwrap(), "--version"],
         "stdout": format!("{version}\n"), "delay_ms": version_delay_ms},
    ])
}

/// A service with the fake tools, an agent-device home (with its claims dir: in use), and a
/// claims tick of a minute: grants come from the sessions read's tick, not the timer.
fn start(home: &Home, tools: &FakeTools, agent_home: &std::path::Path) {
    start_with(home, tools, agent_home, true, true);
}

/// `start`, with or without the claims dir (agent-device in use), and with the test's 200 ms
/// polls or the production intervals.
fn start_with(
    home: &Home,
    tools: &FakeTools,
    agent_home: &std::path::Path,
    in_use: bool,
    fast_polls: bool,
) {
    if in_use {
        std::fs::create_dir_all(agent_home.join("device-claims")).unwrap();
    }
    let mut start = home.hostler();
    start.envs(tools.env());
    if fast_polls {
        start.env("HOSTLER_TEST_DEVICES_POLL_MS", "200");
    }
    start.env("HOSTLER_TEST_AGENT_DEVICE_HOME", agent_home);
    start.env("HOSTLER_TEST_CLAIMS_TICK_MS", "60000");
    let out = start.args(["service", "start"]).output().unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
}

fn list(home: &Home) -> DevicesSnapshot {
    serde_json::from_value(home.call(Request::DevicesList { all: true }).unwrap()).unwrap()
}

fn until(home: &Home, what: &str, done: impl Fn(&DevicesSnapshot) -> bool) -> DevicesSnapshot {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = list(home);
        if done(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "{what}: {s:#?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

fn hold(ws: &str) -> ClaimRequest {
    ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId(AVD.into()),
        },
        workspace_id: ws.into(),
        requester: me(),
        purpose: Purpose::Hold {
            ttl_ms: Some(60_000),
            bound_to: None,
            distinct: false,
        },
        reentrant_tokens: Vec::new(),
        no_wait: true,
    }
}

fn try_hold(home: &Home, ws: &str) -> ClaimOutcome {
    serde_json::from_value(home.call(Request::ClaimsAcquire(hold(ws))).unwrap()).unwrap()
}

/// Waits until a `--no-wait` hold answers as `want` says, at most 10 s.
fn hold_until(
    home: &Home,
    ws: &str,
    what: &str,
    want: impl Fn(&ClaimOutcome) -> bool,
) -> ClaimOutcome {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let o = try_hold(home, ws);
        if want(&o) {
            return o;
        }
        assert!(Instant::now() < deadline, "{what}: {o:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn blocked_by_session(o: &ClaimOutcome) -> bool {
    matches!(o, ClaimOutcome::Busy { blockers, .. }
        if blockers.iter().any(|b| matches!(b, Blocker::Foreign { claim } if claim.source == "agent-device")))
}

fn register(home: &Home) {
    until(home, "the AVD", |s| {
        s.devices.iter().any(|d| d.resource.0 == AVD)
    });
    home.call(Request::DevicesAdd {
        device: "Pixel_9".into(),
        alias: None,
        person: false,
    })
    .unwrap();
}

/// D13, L50: another workspace's live session blocks the device and is listed on it.
#[test]
fn foreign_session_blocks_grants_and_shows_testing_in_progress() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    let elsewhere = home.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let entry = status_entry(&tools, &[("s1", EMULATOR, elsewhere.to_str().unwrap())]);
    tools.script(android(&tools, entry));
    start(&home, &tools, &agent);
    register(&home);
    let snapshot = until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
    });
    let avd = snapshot
        .devices
        .iter()
        .find(|d| d.resource.0 == AVD)
        .unwrap();
    assert_eq!(avd.sessions[0].session, "s1");
    assert_eq!(avd.sessions[0].agent_device_id, EMULATOR);
    assert!(!avd.sessions[0].kept);
    assert!(snapshot.tools.agent_device_in_use);
    // The hourly version check runs on its own lane, after the read (P3bT9b-4).
    until(&home, "the version", |s| {
        s.tools.agent_device.as_deref() == Some("0.21.36")
    });
    let ws = home.workspace(&home.make_repo("app")).unwrap().id;
    hold_until(&home, &ws, "blocked by the session", blocked_by_session);
}

/// L51: the status becomes unreadable: the last known session keeps blocking, marked kept;
/// a good read that no longer lists it frees the device.
#[test]
fn unreadable_status_keeps_last_known_sessions_blocking() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    let entry = status_entry(&tools, &[("s1", EMULATOR, "/nowhere")]);
    tools.script(android(&tools, entry));
    start(&home, &tools, &agent);
    register(&home);
    until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
    });
    let script = tools.agent_device_script();
    let failing = json!({"tool": "node",
        "args_prefix": [script.to_str().unwrap(), "device", "status"], "rc": 1,
        "stderr": "boom"});
    tools.script(android(&tools, failing));
    until(&home, "kept", |s| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == AVD && d.sessions.first().is_some_and(|x| x.kept))
    });
    let ws = home.workspace(&home.make_repo("app")).unwrap().id;
    let busy = hold_until(&home, &ws, "still blocked", blocked_by_session);
    let ClaimOutcome::Busy { blockers, .. } = busy else {
        unreachable!()
    };
    let Blocker::Foreign { claim } = &blockers[0] else {
        panic!("{blockers:?}");
    };
    assert!(
        claim.description.contains("last known"),
        "{}",
        claim.description
    );
    // L54, P3bT9b-2: the fix, from the first failed read.
    assert!(
        claim.description.contains(STATUS_FIX),
        "{}",
        claim.description
    );
    // Recovery: a good read without the session.
    tools.script(android(&tools, status_entry(&tools, &[])));
    hold_until(&home, &ws, "granted", |o| {
        matches!(o, ClaimOutcome::Granted { .. })
    });
}

/// L52, L54: started while agent-device cannot be read, no device is granted to anyone; a
/// person may release one with `forget-sessions`, which stops nothing.
#[test]
fn startup_with_unreadable_status_keeps_devices_closed_until_forgotten() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    let script = tools.agent_device_script();
    let failing = json!({"tool": "node",
        "args_prefix": [script.to_str().unwrap(), "device", "status"], "rc": 1});
    tools.script(android(&tools, failing));
    start(&home, &tools, &agent);
    register(&home);
    let ws = home.workspace(&home.make_repo("app")).unwrap().id;
    hold_until(&home, &ws, "checking", |o| {
        matches!(o, ClaimOutcome::Busy { blockers, .. }
            if blockers.iter().any(|b| matches!(b, Blocker::Checking { .. })))
    });
    let e = home
        .call(Request::DevicesForgetSessions {
            device: "pixel_9".into(),
            person: false,
            by_workspace_path: None,
            by_pid: None,
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    let record: Value = home
        .call(Request::DevicesForgetSessions {
            device: "pixel_9".into(),
            person: true,
            by_workspace_path: Some("/work/app".into()),
            by_pid: Some(42),
        })
        .unwrap();
    assert_eq!(record["resource"], AVD);
    assert!(record["reason"].as_str().unwrap().contains("unreadable"));
    hold_until(&home, &ws, "granted", |o| {
        matches!(o, ClaimOutcome::Granted { .. })
    });
    assert!(
        tools
            .calls()
            .iter()
            .all(|c| c.args.get(1).map(String::as_str) != Some("close")),
        "it stopped nothing"
    );
}

/// P3-4: without a claims dir agent-device is not in use: a good read with no sessions, and
/// agent-device is never run.
#[test]
fn no_claims_dir_is_a_good_read_without_running_agent_device() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(
        &tools,
        json!({"tool": "nothing", "args_prefix": []}),
    ));
    let agent = home.dir.path().join("agent-device");
    start(&home, &tools, &agent);
    std::fs::remove_dir(agent.join("device-claims")).unwrap();
    register(&home);
    let ws = home.workspace(&home.make_repo("app")).unwrap().id;
    hold_until(&home, &ws, "granted", |o| {
        matches!(o, ClaimOutcome::Granted { .. })
    });
    let snapshot = list(&home);
    assert!(!snapshot.tools.agent_device_in_use);
    // At most the first read, which may have run before the dir was removed.
    let status_calls = tools
        .calls()
        .iter()
        .filter(|c| c.tool == "node" && c.args.get(1).map(String::as_str) == Some("device"))
        .count();
    assert!(status_calls <= 1, "{status_calls}");
}

fn failing_status(tools: &FakeTools) -> Value {
    json!({"tool": "node",
           "args_prefix": [tools.agent_device_script().to_str().unwrap(), "device", "status"],
           "rc": 1})
}

fn session_kept(s: &DevicesSnapshot) -> bool {
    s.devices
        .iter()
        .any(|d| d.resource.0 == AVD && d.sessions.first().is_some_and(|x| x.kept))
}

/// L53: a kept session whose owner daemon is gone (no such pid) is dropped while the status is
/// unreadable.
#[test]
fn kept_session_of_a_dead_daemon_is_dropped() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    // A pid no process has (above every pid_max).
    let entry = json!({"tool": "node",
        "args_prefix": [tools.agent_device_script().to_str().unwrap(), "device", "status"],
        "stdout": status_owned(&[("s1", EMULATOR, "/nowhere")], 99_999_999)});
    tools.script(android(&tools, entry));
    start(&home, &tools, &agent);
    register(&home);
    until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
    });
    tools.script(android(&tools, failing_status(&tools)));
    until(&home, "dropped", |s| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == AVD && d.sessions.is_empty())
    });
}

/// L51: the last good read is stored; a restart while the status cannot be read keeps its
/// sessions blocking.
#[test]
fn kept_sessions_survive_a_service_restart() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    tools.script(android(
        &tools,
        status_entry(&tools, &[("s1", EMULATOR, "/nowhere")]),
    ));
    start(&home, &tools, &agent);
    register(&home);
    until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
    });
    let out = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    tools.script(android(&tools, failing_status(&tools)));
    start(&home, &tools, &agent);
    until(&home, "kept after the restart", session_kept);
    let ws = home.workspace(&home.make_repo("app")).unwrap().id;
    hold_until(
        &home,
        &ws,
        "blocked",
        |o| matches!(o, ClaimOutcome::Busy { blockers, .. } if !blockers.is_empty()),
    );
}

/// L52, P3-15, P3bT5-7: after a device claim ends, the next good read (at once, on the end)
/// ticks the claims engine: with a one-minute claims tick, the waiting workspace gets the
/// device within about one read.
#[test]
fn read_after_end_ticks_at_once() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    tools.script(android(&tools, status_entry(&tools, &[])));
    start(&home, &tools, &agent);
    register(&home);
    let a = home.workspace(&home.make_repo("a")).unwrap().id;
    let b = home.workspace(&home.make_repo("b")).unwrap().id;
    let granted = hold_until(&home, &a, "A's hold", |o| {
        matches!(o, ClaimOutcome::Granted { .. })
    });
    let ClaimOutcome::Granted { claim } = granted else {
        unreachable!()
    };
    let waiting = ClaimRequest {
        no_wait: false,
        ..hold(&b)
    };
    let queued: ClaimOutcome =
        serde_json::from_value(home.call(Request::ClaimsAcquire(waiting.clone())).unwrap())
            .unwrap();
    assert!(matches!(queued, ClaimOutcome::Queued { .. }), "{queued:?}");
    let released = Instant::now();
    home.call(Request::ClaimsRelease(
        hostler::contract::claims::ClaimRelease {
            claim_id: claim.id,
            workspace_id: a,
            requester: me(),
            exit_code: None,
        },
    ))
    .unwrap();
    let deadline = released + Duration::from_secs(10);
    loop {
        let state: ClaimOutcome = serde_json::from_value(
            home.call(Request::ClaimsAttach {
                request_id: waiting.id.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        if matches!(state, ClaimOutcome::Granted { .. }) {
            break;
        }
        assert!(Instant::now() < deadline, "{state:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        released.elapsed() < Duration::from_millis(1_500),
        "{:?}",
        released.elapsed()
    );
}

fn ready_and_checked(s: &DevicesSnapshot) -> bool {
    s.devices
        .iter()
        .any(|d| d.resource.0 == AVD && d.registered && d.ready && !d.checking_sessions)
}

fn granted(o: &ClaimOutcome) -> bool {
    matches!(o, ClaimOutcome::Granted { .. })
}

/// L54, P3bT9b-2: a device a claim ended on stays closed while the status cannot be read, and
/// the answer names the fix and `forget-sessions` from the first failed read; `devices list`
/// says the sessions are unknown, with the fix.
#[test]
fn unreadable_status_keeps_ended_devices_closed_with_the_fix_text() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    tools.script(android(&tools, status_entry(&tools, &[])));
    start(&home, &tools, &agent);
    register(&home);
    until(&home, "registered and checked", ready_and_checked);
    let a = home.workspace(&home.make_repo("a")).unwrap().id;
    let b = home.workspace(&home.make_repo("b")).unwrap().id;
    tools.script(android(&tools, failing_status(&tools)));
    let snapshot = until(&home, "unreadable", |s| !s.tools.sessions_known);
    let reason = "agent-device device status exited with 1";
    let problem = format!("agent-device sessions unknown: {reason}; {STATUS_FIX}");
    assert!(
        snapshot.tools.problems.contains(&problem),
        "{:?}",
        snapshot.tools.problems
    );
    let ClaimOutcome::Granted { claim } = hold_until(&home, &a, "A's hold", granted) else {
        unreachable!()
    };
    home.call(Request::ClaimsRelease(
        hostler::contract::claims::ClaimRelease {
            claim_id: claim.id,
            workspace_id: a,
            requester: me(),
            exit_code: None,
        },
    ))
    .unwrap();
    let busy = try_hold(&home, &b);
    let ClaimOutcome::Busy { blockers, .. } = &busy else {
        panic!("{busy:?}");
    };
    let [Blocker::Checking { resource, text }] = &blockers[..] else {
        panic!("{blockers:?}");
    };
    assert_eq!(resource, "pixel_9");
    assert_eq!(
        *text,
        format!(
            "checking for agent-device sessions on pixel_9 (agent-device status unreadable: \
             {reason}; {STATUS_FIX}; a person may release this device: hostler devices \
             forget-sessions pixel_9)"
        )
    );
}

/// L52: the first sessions read runs before any other poller call.
#[test]
fn startup_reads_sessions_before_anything_else() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    let slow = json!({"tool": "node",
        "args_prefix": [tools.agent_device_script().to_str().unwrap(), "device", "status"],
        "stdout": status(&[]), "delay_ms": 500});
    tools.script(android(&tools, slow));
    start(&home, &tools, &agent);
    until(&home, "the first read", |s| s.tools.agent_device_in_use);
    let calls = tools.calls();
    let first_status = calls
        .iter()
        .position(|c| c.tool == "node" && c.args.get(1).map(String::as_str) == Some("device"))
        .unwrap();
    let first_poll = calls
        .iter()
        .position(|c| {
            (c.tool == "adb" && c.args.first().map(String::as_str) != Some("version"))
                || c.tool == "emulator"
        })
        .unwrap_or(usize::MAX);
    assert!(first_status < first_poll, "{calls:#?}");
}

/// L52: after a restart nothing is granted before the first good read, even on a device
/// discovery already shows ready.
#[test]
fn startup_waits_for_the_first_sessions_read() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    tools.script(android(&tools, status_entry(&tools, &[])));
    start(&home, &tools, &agent);
    register(&home);
    until(&home, "registered and checked", ready_and_checked);
    let out = home.hostler().args(["service", "stop"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let slow = json!({"tool": "node",
        "args_prefix": [tools.agent_device_script().to_str().unwrap(), "device", "status"],
        "stdout": status(&[]), "delay_ms": 3000});
    let repo = home.make_repo("app");
    tools.script(android(&tools, slow));
    let started = Instant::now();
    start(&home, &tools, &agent);
    let ws = home.workspace(&repo).unwrap().id;
    // The first read is in flight (3 s): discovery waits for it, and nothing is granted.
    let first = try_hold(&home, &ws);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "too slow to see it"
    );
    assert!(matches!(first, ClaimOutcome::Busy { .. }), "{first:?}");
    hold_until(&home, &ws, "granted after the read", granted);
    assert!(started.elapsed() >= Duration::from_secs(3));
}

/// P3-9: hung per-device probes never delay the sessions reads (their own lane).
#[test]
fn sessions_read_not_delayed_by_hung_probes() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    let mut script = android(&tools, status_entry(&tools, &[]));
    for entry in script.as_array_mut().unwrap() {
        if entry["tool"] == "adb" && entry["args_prefix"][0] == "-s" {
            entry["delay_ms"] = json!(8000);
        }
    }
    tools.script(script);
    start(&home, &tools, &agent);
    until(&home, "the first read", |s| s.tools.agent_device_in_use);
    let reads = || {
        tools
            .calls()
            .iter()
            .filter(|c| c.tool == "node" && c.args.get(1).map(String::as_str) == Some("device"))
            .count()
    };
    // A probe has started and hangs (8 s); count the reads meanwhile.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !tools
        .calls()
        .iter()
        .any(|c| c.args.first().map(String::as_str) == Some("-s"))
    {
        assert!(Instant::now() < deadline, "no probe started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let before = reads();
    std::thread::sleep(Duration::from_secs(3));
    assert!(reads() - before >= 3, "{} reads in 3 s", reads() - before);
}

/// P-3: an older agent-device warns (`devices list`) and its sessions still block.
#[test]
fn old_agent_device_version_warns_and_still_blocks() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let agent = home.dir.path().join("agent-device");
    let elsewhere = home.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let entry = status_entry(&tools, &[("s1", EMULATOR, elsewhere.to_str().unwrap())]);
    tools.script(android_with(&tools, entry, "0.20.1", 0));
    start(&home, &tools, &agent);
    register(&home);
    let snapshot = until(&home, "the version", |s| {
        s.tools.agent_device.as_deref() == Some("0.20.1")
    });
    assert!(
        snapshot
            .tools
            .problems
            .iter()
            .any(|p| p.contains("agent-device 0.20.1 is older than 0.21.36")),
        "{:?}",
        snapshot.tools.problems
    );
    let ws = home.workspace(&home.make_repo("app")).unwrap().id;
    hold_until(&home, &ws, "blocked by the session", blocked_by_session);
}

// ---- `run --device` / `hold device` with agent-device sessions (Task 7's CLI) ----

#[cfg(unix)]
mod cli {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use super::*;

    /// The service, the AVD registered and a first good read done; `status` is what the fake
    /// agent-device answers; the claims dir exists unless `in_use` is false.
    struct Lab {
        home: Home,
        tools: FakeTools,
        agent: PathBuf,
    }

    impl Lab {
        fn new(status: Value, in_use: bool) -> Lab {
            Lab::new_with(android_for(status), in_use, true)
        }

        /// `script`: the whole fake script, `{"__status"}` placeholders expanded.
        fn new_with(script: Value, in_use: bool, fast_polls: bool) -> Lab {
            let home = Home::new();
            let mut tools = FakeTools::new();
            tools.adb_server(41);
            tools.script(expand(&tools, script));
            let agent = home.dir.path().join("agent-device");
            start_with(&home, &tools, &agent, in_use, fast_polls);
            register(&home);
            until(&home, "booted, registered and checked", |s| {
                s.devices
                    .iter()
                    .any(|d| d.resource.0 == AVD && d.registered && d.ready && !d.checking_sessions)
            });
            Lab { home, tools, agent }
        }

        fn repo(&self, name: &str) -> PathBuf {
            std::fs::canonicalize(self.home.make_repo(name)).unwrap()
        }

        fn path_of(&self, repo: &Path) -> String {
            self.home.workspace(repo).unwrap().path
        }

        /// The fake agent-device answers `status` from now on.
        fn answers(&self, status: Value) {
            self.tools.script(android(&self.tools, status));
        }

        /// A script file a run's command swaps in (`cp` + `mv`, so a reader never sees half).
        fn prepared(&self, name: &str, status: Value) -> PathBuf {
            let path = self.tools.dir().join(name);
            std::fs::write(
                &path,
                serde_json::to_vec(&android(&self.tools, status)).unwrap(),
            )
            .unwrap();
            path
        }

        fn hostler(&self, repo: &Path, args: &[&str]) -> Command {
            let mut c = self.home.hostler();
            c.args(args)
                .current_dir(repo)
                .env_remove("HOSTLER_CLAIM_TOKENS");
            c
        }

        fn run(&self, repo: &Path, args: &[&str]) -> Output {
            let mut all = vec!["run"];
            all.extend_from_slice(args);
            self.hostler(repo, &all).output().unwrap()
        }

        /// `hostler run --device pixel_9 -- <swap in next>`: the run's command changes what
        /// agent-device answers, as an agent that opens a session inside the run does.
        fn run_that_opens(&self, repo: &Path, next: &Path) -> Output {
            let script = self.tools.dir().join("script.json");
            self.hostler(
                repo,
                &[
                    "run",
                    "--device",
                    "pixel_9",
                    "--",
                    "sh",
                    "-c",
                    "mkdir -p \"$CLAIMS\" && cp \"$NEXT\" \"$SCRIPT.tmp\" && mv \"$SCRIPT.tmp\" \"$SCRIPT\"",
                ],
            )
            .env("NEXT", next)
            .env("SCRIPT", &script)
            .env("CLAIMS", self.agent.join("device-claims"))
            .output()
            .unwrap()
        }

        fn closes(&self) -> usize {
            self.tools
                .calls()
                .iter()
                .filter(|c| c.tool == "node" && c.args.get(1).map(String::as_str) == Some("close"))
                .count()
        }
    }

    fn code(out: &Output) -> Option<i32> {
        out.status.code()
    }

    /// L52 (deterministic): A's run opens a session and ends; B is never granted before a good
    /// read that started after A's end, and that read lists the session, so B waits until it
    /// closes. A itself is not blocked by its own session (L50).
    #[test]
    fn session_opened_in_the_last_run_blocks_the_next_workspace() {
        let lab = Lab::new(status_entry_for(&[]), true);
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let a_path = lab.path_of(&a);
        let next = lab.prepared("next.json", status_entry_for(&[("s1", EMULATOR, &a_path)]));
        let out = lab.run_that_opens(&a, &next);
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        // L49, P2-15: the session opened in the run's last moment is in its end warning (a read
        // that started after the release).
        assert!(
            stderr(&out).contains("agent-device session s1 is still open on pixel_9"),
            "{}",
            stderr(&out)
        );
        let out = lab.run(
            &b,
            &["--device", "pixel_9", "--max-wait", "0.05", "--", "true"],
        );
        assert_eq!(code(&out), Some(75), "B must wait: {}", stderr(&out));
        assert!(
            stderr(&out).contains("agent-device session s1"),
            "{}",
            stderr(&out)
        );
        // A's own session never blocks A.
        let out = lab.run(
            &a,
            &["--device", "pixel_9", "--max-wait", "0.05", "--", "true"],
        );
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        // The session closes: B gets the device after the next read.
        lab.answers(status_entry_for(&[]));
        let out = lab.run(
            &b,
            &["--device", "pixel_9", "--max-wait", "0.2", "--", "true"],
        );
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    }

    /// P3-4: agent-device was not in use (no claims dir) until A's run used it: the read after
    /// A's end checks the dir again and sees the session.
    #[test]
    fn first_agent_device_use_inside_a_run_blocks_the_next_workspace() {
        let lab = Lab::new(status_entry_for(&[]), false);
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let a_path = lab.path_of(&a);
        let next = lab.prepared("next.json", status_entry_for(&[("s1", EMULATOR, &a_path)]));
        let out = lab.run_that_opens(&a, &next);
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        let out = lab.run(
            &b,
            &["--device", "pixel_9", "--max-wait", "0.05", "--", "true"],
        );
        assert_eq!(code(&out), Some(75), "B must wait: {}", stderr(&out));
    }

    /// D9: a takeover the operator permitted closes the holder's session through the
    /// agent-device stopper (`close`, then the status until it is gone), and moves the device.
    #[test]
    fn takeover_of_a_hold_closes_its_agent_device_session() {
        let lab = Lab::new(status_entry_for(&[]), true);
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let a_path = lab.path_of(&a);
        let out = lab
            .hostler(&a, &["hold", "device", "pixel_9", "--ttl", "5"])
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        // A's agent opens a session; `close` drops it (the fake rewrites its answer).
        let script = lab.tools.agent_device_script();
        let closed = android(&lab.tools, status_entry_for(&[]));
        let mut open = android(&lab.tools, status_entry_for(&[("s1", EMULATOR, &a_path)]));
        open.as_array_mut().unwrap().insert(
            0,
            json!({"tool": "node", "args_prefix": [script.to_str().unwrap(), "close"],
                   "stdout": "{\"success\": true}", "then_write": closed}),
        );
        lab.tools.script(open);
        until(&lab.home, "A's session", |s| {
            s.devices
                .iter()
                .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
        });
        let out = lab
            .hostler(
                &b,
                &[
                    "hold",
                    "device",
                    "pixel_9",
                    "--force",
                    "--confirmation",
                    "pixel_9",
                ],
            )
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("Took pixel_9 over"),
            "{}",
            stderr(&out)
        );
        assert_eq!(lab.closes(), 1);
        let close = lab
            .tools
            .calls()
            .into_iter()
            .find(|c| c.args.get(1).map(String::as_str) == Some("close"))
            .unwrap();
        assert_eq!(
            &close.args[2..8],
            [
                "--session",
                "s1",
                "--platform",
                "android",
                "--serial",
                EMULATOR
            ]
        );
    }

    /// L54, P3bT1-8: `--force` never waits silently behind the sessions check: the waiting line
    /// names the fix and the person's escape; and a kept session cannot be taken over while the
    /// status is unreadable: refused (78), naming `forget-sessions`.
    #[test]
    fn force_does_not_override_checking_or_kept_sessions_while_unreadable() {
        let lab = Lab::new(status_entry_for(&[]), true);
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let a_path = lab.path_of(&a);
        // A's run ends while the status cannot be read: B is checked, and --force waits on it.
        lab.answers(failing_status(&lab.tools));
        until(&lab.home, "unreadable", |s| !s.tools.sessions_known);
        let out = lab.run(&a, &["--device", "pixel_9", "--", "true"]);
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        for args in [
            &[
                "run",
                "--device",
                "pixel_9",
                "--force",
                "--confirmation",
                "pixel_9",
                "--max-wait",
                "0.03",
                "--",
                "true",
            ][..],
            &[
                "hold",
                "device",
                "pixel_9",
                "--force",
                "--confirmation",
                "pixel_9",
                "--max-wait",
                "0.03",
            ],
        ] {
            let out = lab.hostler(&b, args).output().unwrap();
            assert_eq!(code(&out), Some(75), "{args:?}: {}", stderr(&out));
            assert!(
                stderr(&out).contains("hostler devices forget-sessions pixel_9"),
                "{args:?}: {}",
                stderr(&out)
            );
            assert!(
                stderr(&out).contains(
                    "agent-device status unreadable: agent-device device status exited with 1"
                ),
                "the reason: {}",
                stderr(&out)
            );
        }
        // A session known from the last good read, kept while unreadable: no takeover.
        lab.answers(status_entry_for(&[("s1", EMULATOR, &a_path)]));
        until(&lab.home, "the session", |s| {
            s.devices
                .iter()
                .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
        });
        lab.answers(failing_status(&lab.tools));
        until(&lab.home, "kept", session_kept);
        let out = lab
            .hostler(
                &b,
                &[
                    "hold",
                    "device",
                    "pixel_9",
                    "--force",
                    "--confirmation",
                    "pixel_9",
                    "--max-wait",
                    "0.05",
                ],
            )
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(78), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("hostler devices forget-sessions pixel_9"),
            "{}",
            stderr(&out)
        );
        assert_eq!(lab.closes(), 0, "nothing was closed");
    }

    /// L51: after a restart with the status unreadable, the last known session keeps the device
    /// from other workspaces, and the waiting line says it is the last known one.
    #[test]
    fn kept_session_blocks_runs_after_a_restart() {
        let lab = Lab::new(status_entry_for(&[]), true);
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let a_path = lab.path_of(&a);
        lab.answers(status_entry_for(&[("s1", EMULATOR, &a_path)]));
        until(&lab.home, "the session", |s| {
            s.devices
                .iter()
                .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
        });
        let out = lab
            .home
            .hostler()
            .args(["service", "stop"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        lab.answers(failing_status(&lab.tools));
        start(&lab.home, &lab.tools, &lab.agent);
        until(&lab.home, "kept", session_kept);
        let out = lab.run(
            &b,
            &["--device", "pixel_9", "--max-wait", "0.05", "--", "true"],
        );
        assert_eq!(code(&out), Some(75), "{}", stderr(&out));
        assert!(stderr(&out).contains("last known"), "{}", stderr(&out));
    }

    /// L54 end to end: unreadable since the start, every device is closed; a person releases
    /// one with `hostler devices forget-sessions`, typing its alias; nothing is stopped.
    #[test]
    fn forget_sessions_releases_one_device_and_stops_nothing() {
        let lab = Lab::new(status_entry_for(&[]), true);
        let b = lab.repo("b");
        lab.answers(failing_status(&lab.tools));
        // A restart: no good read in this process yet.
        let out = lab
            .home
            .hostler()
            .args(["service", "stop"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        start(&lab.home, &lab.tools, &lab.agent);
        let out = lab.run(
            &b,
            &["--device", "pixel_9", "--max-wait", "0.05", "--", "true"],
        );
        assert_eq!(code(&out), Some(75), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("forget-sessions pixel_9"),
            "{}",
            stderr(&out)
        );
        let tty = lab.home.dir.path().join("tty");
        std::fs::write(&tty, "pixel_9\n").unwrap();
        let out = lab
            .hostler(&b, &["devices", "forget-sessions", "pixel_9"])
            .env("HOSTLER_TEST_TTY", &tty)
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        let out = lab.run(
            &b,
            &["--device", "pixel_9", "--max-wait", "0.1", "--", "true"],
        );
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        assert_eq!(lab.closes(), 0, "it stopped nothing");
    }

    /// `status_entry` without the tools at hand: `Lab::new` scripts it.
    fn status_entry_for(sessions: &[(&str, &str, &str)]) -> Value {
        json!({"__status": status(sessions)})
    }

    /// `android`'s script with its `{"__status"}` entry left for `expand`.
    fn android_for(status: Value) -> Value {
        json!({"__android": status, "__version": "0.21.36", "__version_delay_ms": 0})
    }

    fn expand(tools: &FakeTools, script: Value) -> Value {
        match script.get("__android") {
            Some(status) => android_with(
                tools,
                status.clone(),
                script["__version"].as_str().unwrap(),
                script["__version_delay_ms"].as_u64().unwrap(),
            ),
            None => script,
        }
    }

    fn release(home: &Home, claim_id: String, ws: &str) {
        home.call(Request::ClaimsRelease(
            hostler::contract::claims::ClaimRelease {
                claim_id,
                workspace_id: ws.into(),
                requester: me(),
                exit_code: None,
            },
        ))
        .unwrap();
    }

    fn status_calls(lab: &Lab) -> usize {
        lab.tools
            .calls()
            .iter()
            .filter(|c| c.tool == "node" && c.args.get(1).map(String::as_str) == Some("device"))
            .count()
    }

    /// P3bT9b-3: a takeover while the status is unreadable is refused before anything is
    /// stopped: the holder's run keeps running and no session is closed.
    #[test]
    fn takeover_while_unreadable_stops_nothing() {
        let lab = Lab::new(status_entry_for(&[]), true);
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let mut run = lab
            .hostler(&a, &["run", "--device", "pixel_9", "--", "sleep", "30"])
            .spawn()
            .unwrap();
        let b_ws = lab.home.workspace(&b).unwrap().id;
        hold_until(&lab.home, &b_ws, "A's run holds it", |o| {
            matches!(o, ClaimOutcome::Busy { blockers, .. }
                if blockers.iter().any(|b| matches!(b, Blocker::Claim { .. })))
        });
        lab.answers(failing_status(&lab.tools));
        until(&lab.home, "unreadable", |s| !s.tools.sessions_known);
        let out = lab
            .hostler(
                &b,
                &[
                    "hold",
                    "device",
                    "pixel_9",
                    "--force",
                    "--confirmation",
                    "pixel_9",
                    "--max-wait",
                    "0.05",
                ],
            )
            .output()
            .unwrap();
        let alive = run.try_wait().unwrap().is_none();
        let _ = Command::new("kill")
            .args(["-INT", &run.id().to_string()])
            .status();
        let _ = run.wait();
        assert_eq!(code(&out), Some(78), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("agent-device status unreadable"),
            "{}",
            stderr(&out)
        );
        assert!(stderr(&out).contains(STATUS_FIX), "{}", stderr(&out));
        assert!(alive, "A's run was not stopped");
        assert_eq!(lab.closes(), 0, "nothing was closed");
    }

    /// D9 through the CLI: A's session is closed by the takeover, and the commit's fresh read
    /// passes.
    fn takeover_closes_the_session(lab: &Lab) {
        let (a, b) = (lab.repo("a"), lab.repo("b"));
        let a_path = lab.path_of(&a);
        let out = lab
            .hostler(&a, &["hold", "device", "pixel_9", "--ttl", "60"])
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        let script = lab.tools.agent_device_script();
        let closed = android(&lab.tools, status_entry_for(&[]));
        let mut open = android(&lab.tools, status_entry_for(&[("s1", EMULATOR, &a_path)]));
        open.as_array_mut().unwrap().insert(
            0,
            json!({"tool": "node", "args_prefix": [script.to_str().unwrap(), "close"],
                   "stdout": "{\"success\": true}", "then_write": closed}),
        );
        lab.tools.script(open);
        until(&lab.home, "A's session", |s| {
            s.devices
                .iter()
                .any(|d| d.resource.0 == AVD && d.sessions.len() == 1)
        });
        let out = lab
            .hostler(
                &b,
                &[
                    "hold",
                    "device",
                    "pixel_9",
                    "--force",
                    "--confirmation",
                    "pixel_9",
                ],
            )
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("Took pixel_9 over"),
            "{}",
            stderr(&out)
        );
        assert_eq!(lab.closes(), 1);
    }

    /// The production intervals (sessions every 5 s, no test poll): the commit's own read
    /// passes, so a takeover of a session-holding device succeeds.
    #[test]
    fn foreign_commit_passes_with_production_poll_interval() {
        let lab = Lab::new_with(android_for(status_entry_for(&[])), true, false);
        takeover_closes_the_session(&lab);
    }

    /// P3bT9b-4: a slow status (4.5 s) in flight and the hourly version check due (4.5 s,
    /// slow): the commit's fresh read waits for the cycle in flight and runs its own, and the
    /// takeover fits the client's 15 s commit bound.
    #[test]
    fn seize_commit_refresh_reuses_an_inflight_read_and_fits_the_15_s_bound() {
        let slow = json!({"__status": status(&[]), "__delay_ms": 4500});
        let lab = Lab::new_with(
            json!({"__android": slow, "__version": "0.21.36", "__version_delay_ms": 4500}),
            false,
            true,
        );
        let a = lab.repo("a");
        let b = lab.repo("b");
        let out = lab
            .hostler(&a, &["hold", "device", "pixel_9", "--ttl", "60"])
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        // First use: the version check is due, and a slow read starts.
        std::fs::create_dir_all(lab.agent.join("device-claims")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while status_calls(&lab) == 0 {
            assert!(Instant::now() < deadline, "no read started");
            std::thread::sleep(Duration::from_millis(50));
        }
        let started = Instant::now();
        let out = lab
            .hostler(
                &b,
                &[
                    "hold",
                    "device",
                    "pixel_9",
                    "--force",
                    "--confirmation",
                    "pixel_9",
                ],
            )
            .output()
            .unwrap();
        let took = started.elapsed();
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        assert!(took < Duration::from_secs(15), "{took:?}");
        assert!(
            lab.tools
                .calls()
                .iter()
                .any(|c| c.args.get(1).map(String::as_str) == Some("--version")),
            "the version check ran"
        );
    }

    /// P3-15: a claim end while a read is in flight: that read (it started before the end)
    /// cannot open the device; exactly one more read runs at once, and the grant comes after it.
    #[test]
    fn end_during_an_inflight_read_triggers_another_read() {
        let slow = json!({"__status": status(&[]), "__delay_ms": 1500});
        let lab = Lab::new_with(android_for(slow), true, false);
        let a = lab.home.workspace(&lab.repo("a")).unwrap().id;
        let b = lab.home.workspace(&lab.repo("b")).unwrap().id;
        let ClaimOutcome::Granted { claim } = hold_until(&lab.home, &a, "A's hold", |o| {
            matches!(o, ClaimOutcome::Granted { .. })
        }) else {
            unreachable!()
        };
        let waiting = ClaimRequest {
            no_wait: false,
            ..hold(&b)
        };
        let queued: ClaimOutcome = serde_json::from_value(
            lab.home
                .call(Request::ClaimsAcquire(waiting.clone()))
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(queued, ClaimOutcome::Queued { .. }), "{queued:?}");
        // A read starts: end A's claim while it is in flight.
        let before = status_calls(&lab);
        let deadline = Instant::now() + Duration::from_secs(10);
        while status_calls(&lab) == before {
            assert!(Instant::now() < deadline, "no read started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let at_end = status_calls(&lab);
        release(&lab.home, claim.id, &a);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state: ClaimOutcome = serde_json::from_value(
                lab.home
                    .call(Request::ClaimsAttach {
                        request_id: waiting.id.clone(),
                    })
                    .unwrap(),
            )
            .unwrap();
            if matches!(state, ClaimOutcome::Granted { .. }) {
                break;
            }
            assert!(Instant::now() < deadline, "{state:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(status_calls(&lab) - at_end, 1, "exactly one more read");
    }
}
