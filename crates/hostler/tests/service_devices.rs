//! The device service in a real service process, against the fake tools (P3b Task 5). No real
//! adb, emulator, xcrun or ideviceinfo ever runs here; every call the fakes logged must pass the
//! exact allowlist (`FakeTools`' drop check).

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::time::{Duration, Instant};

use common::{Home, stderr};
use fake_tools::{FakeCall, FakeTools};
use hostler::client::Client;
use hostler::contract::ErrorCode;
use hostler::contract::claims::{ClaimOutcome, ClaimRequest, Purpose, ResourceId, Selector};
use hostler::contract::devices::{AdbState, DevicesSnapshot};
use hostler::contract::wire::Request;
use hostler::devices::tools::{SNAPSHOT, TELEMETRY};
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use serde_json::{Value, json};

const PHONE: &str = "SERIAL0001";
const EMULATOR: &str = "emulator-5554";

fn adb_version() -> Value {
    json!({"tool": "adb", "args_prefix": ["version"],
           "stdout": "Android Debug Bridge version 1.0.41\nVersion 35.0.2-12147458\n"})
}

/// A USB phone and a booted emulator, each with its identity; the AVD exists.
fn android(devices_delay_ms: Option<u64>) -> Value {
    let mut devices = json!({"tool": "adb", "args_prefix": ["devices", "-l"],
        "stdout": format!("List of devices attached\n\
            {PHONE}             device usb:1-1 product:p model:Pixel_9 device:d transport_id:1\n\
            {EMULATOR}          device product:sdk model:sdk_gphone64 device:emu64a transport_id:2\n\n")});
    if let Some(ms) = devices_delay_ms {
        devices["delay_ms"] = json!(ms);
    }
    json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["start-server"]},
        devices,
        // The phone's last three getprop lines are empty (never trimmed).
        {"tool": "adb", "args_prefix": ["-s", PHONE, "shell"],
         "stdout": format!("{PHONE}\nPixel 9\n15\n35\n\n\n\n")},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", "getprop sys.boot_completed"],
         "stdout": "1\n"},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell"],
         "stdout": "EMULATOR35X1X1X0\nsdk_gphone64_arm64\n15\n35\nPixel_9\n1\n\n"},
        {"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": "Pixel_9\n"},
    ])
}

/// Starts the service with the fake tools and fast polling.
fn start(home: &Home, tools: &FakeTools, extra: &[(&str, &str)]) {
    let mut start = home.hostler();
    start.envs(tools.env());
    start.env("HOSTLER_TEST_DEVICES_POLL_MS", "200");
    // Off macOS the iOS loop does not run; on macOS it runs against the fake xcrun.
    for (k, v) in extra {
        start.env(k, v);
    }
    let out = start.args(["service", "start"]).output().unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
}

fn list(home: &Home, all: bool) -> DevicesSnapshot {
    serde_json::from_value(home.call(Request::DevicesList { all }).unwrap()).unwrap()
}

/// Polls `devices.list --all` until `done` holds, at most 20 s.
fn until(home: &Home, what: &str, done: impl Fn(&DevicesSnapshot) -> bool) -> DevicesSnapshot {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snapshot = list(home, true);
        if done(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "{what}: {snapshot:#?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn has(snapshot: &DevicesSnapshot, resource: &str) -> bool {
    snapshot.devices.iter().any(|d| d.resource.0 == resource)
}

fn adb_calls(tools: &FakeTools) -> Vec<FakeCall> {
    tools
        .calls()
        .into_iter()
        .filter(|c| c.tool == "adb")
        .collect()
}

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

fn timed(home: &Home, request: Request) -> Duration {
    let paths = home.paths();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut client = Client::connect(&paths).await.unwrap();
            let start = Instant::now();
            client.call(request).await.unwrap();
            start.elapsed()
        })
}

#[test]
fn devices_appear_after_a_poll_and_register() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(None));
    // A claims tick only every minute: the catalog must reach the engine through the device
    // service's tick notify.
    start(&home, &tools, &[("HOSTLER_TEST_CLAIMS_TICK_MS", "60000")]);
    let snapshot = until(&home, "the phone and the AVD", |s| {
        has(s, "device:android:SERIAL0001")
            && s.devices
                .iter()
                .any(|d| d.resource.0 == "device:android:avd:Pixel_9" && d.ready)
    });
    assert_eq!(snapshot.tools.adb_state, AdbState::Ours);
    let phone = snapshot
        .devices
        .iter()
        .find(|d| d.resource.0 == "device:android:SERIAL0001")
        .unwrap();
    assert!(!phone.registered && phone.connected);
    assert_eq!(phone.model, "Pixel 9");
    // Without --all: the new physical phone, not the unregistered AVD.
    let plain = list(&home, false);
    assert!(has(&plain, "device:android:SERIAL0001"));
    assert!(!has(&plain, "device:android:avd:Pixel_9"));

    // L49: the phone needs a person; the AVD does not.
    let e = home
        .call(Request::DevicesAdd {
            device: PHONE.into(),
            alias: None,
            person: false,
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    let avd: Value = home
        .call(Request::DevicesAdd {
            device: "Pixel_9".into(),
            alias: None,
            person: false,
        })
        .unwrap();
    assert_eq!(avd["alias"], "pixel_9");
    home.call(Request::DevicesAdd {
        device: PHONE.into(),
        alias: Some("pixel-9".into()),
        person: true,
    })
    .unwrap();

    // Both are resources now: the claims engine's catalog lists them (at once: a catalog change
    // ticks the engine).
    let claims = until_claims(&home, |ids| ids.len() == 3);
    let ids: Vec<&str> = claims
        .resources
        .iter()
        .map(|r| r.resource.id.0.as_str())
        .collect();
    assert!(ids.contains(&"device:android:SERIAL0001"), "{ids:?}");
    assert!(ids.contains(&"device:android:avd:Pixel_9"), "{ids:?}");
    let phone = claims
        .resources
        .iter()
        .find(|r| r.resource.id.0 == "device:android:SERIAL0001")
        .unwrap();
    assert_eq!(phone.resource.display, "pixel-9");
    assert_eq!(phone.resource.attributes["serial"], PHONE);

    // `devices.resolve`: registered only.
    let info: Value = home
        .call(Request::DevicesResolve {
            device: "PIXEL-9".into(),
        })
        .unwrap();
    assert_eq!(info["resource"], "device:android:SERIAL0001");
}

/// A hung `adb devices -l` never delays the claims engine or `status`.
#[test]
fn hung_adb_never_delays_claims() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(Some(15_000)));
    start(&home, &tools, &[]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !adb_calls(&tools)
        .iter()
        .any(|c| c.args == ["devices", "-l"])
    {
        if Instant::now() >= deadline {
            let mut log = String::new();
            for e in std::fs::read_dir(home.home.join("logs")).unwrap().flatten() {
                log.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
            }
            panic!(
                "adb devices -l never ran: {:?}\n{log}",
                adb_calls(&tools)
                    .iter()
                    .map(|c| c.args.join(" "))
                    .collect::<Vec<_>>()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let repo = home.make_repo("app");
    let ws = home.workspace(&repo).unwrap().id;
    for request in [
        Request::ClaimsAcquire(ClaimRequest {
            id: uuid::Uuid::new_v4().to_string(),
            selector: Selector::Exact {
                resource: ResourceId::screen(),
            },
            workspace_id: ws.clone(),
            requester: me(),
            purpose: Purpose::Hold {
                ttl_ms: Some(60_000),
                bound_to: None,
                distinct: false,
            },
            reentrant_tokens: Vec::new(),
            no_wait: true,
        }),
        Request::StatusOverview,
        Request::ClaimsSnapshot,
        Request::DevicesList { all: true },
    ] {
        // The best of three: the machine may be busy, the call must not wait on adb.
        let best = (0..3).map(|_| timed(&home, request.clone())).min().unwrap();
        assert!(best < Duration::from_millis(200), "{request:?}: {best:?}");
    }
}

/// P2-2: a server of another version is never met by an adb client: no adb call but `version`.
#[test]
fn foreign_version_server_is_never_killed() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(40);
    tools.script(android(None));
    start(&home, &tools, &[]);
    let snapshot = until(&home, "adb is foreign", |s| {
        s.tools.adb_state == AdbState::Foreign { version: 40 }
    });
    assert!(
        snapshot
            .tools
            .problems
            .iter()
            .any(|p| p.starts_with("another adb (version 40) owns the adb server")),
        "{:?}",
        snapshot.tools.problems
    );
    std::thread::sleep(Duration::from_millis(1_000));
    for call in adb_calls(&tools) {
        assert_eq!(call.args, ["version"], "{call:?}");
    }
    assert!(!has(&list(&home, true), "device:android:SERIAL0001"));
}

#[test]
fn same_version_server_is_used() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(None));
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));
    let calls = adb_calls(&tools);
    assert!(calls.iter().any(|c| c.args == ["devices", "-l"]));
    assert!(
        !calls.iter().any(|c| c.args == ["start-server"]),
        "a server answers: no start-server"
    );
}

/// No server answers: `start-server` runs first, with null stdio (P-7).
#[test]
fn no_server_starts_one_with_null_stdio() {
    let home = Home::new();
    let tools = FakeTools::new();
    tools.script(android(None));
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));
    let calls = adb_calls(&tools);
    let started = calls
        .iter()
        .find(|c| c.args == ["start-server"])
        .expect("start-server ran");
    assert_eq!(started.stdout, "null");
    let first_start = calls
        .iter()
        .position(|c| c.args == ["start-server"])
        .unwrap();
    let first_devices = calls
        .iter()
        .position(|c| c.args == ["devices", "-l"])
        .unwrap();
    assert!(first_start < first_devices);
}

/// P-9: an unregistered device gets discovery and identity only.
#[test]
fn unregistered_device_is_never_snapshotted_or_polled() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(None));
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));
    std::thread::sleep(Duration::from_millis(1_500));
    for call in tools.calls() {
        assert!(
            !call.args.iter().any(|a| a == SNAPSHOT || a == TELEMETRY),
            "{call:?}"
        );
        assert_ne!(call.tool, "ideviceinfo", "{call:?}");
    }
}

/// P-14, P2-14: `devices remove` asks the claims engine and is refused while a claim holds it.
#[test]
fn remove_refused_while_held() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(None));
    start(&home, &tools, &[]);
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    home.call(Request::DevicesAdd {
        device: "Pixel_9".into(),
        alias: None,
        person: false,
    })
    .unwrap();
    let repo = home.make_repo("app");
    let ws = home.workspace(&repo).unwrap().id;
    let hold = ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId("device:android:avd:Pixel_9".into()),
        },
        workspace_id: ws.clone(),
        requester: me(),
        purpose: Purpose::Hold {
            ttl_ms: Some(60_000),
            bound_to: None,
            distinct: false,
        },
        reentrant_tokens: Vec::new(),
        no_wait: false,
    };
    let granted: ClaimOutcome =
        serde_json::from_value(home.call(Request::ClaimsAcquire(hold.clone())).unwrap()).unwrap();
    let ClaimOutcome::Granted { claim } = granted else {
        panic!("{granted:?}");
    };
    let e = home
        .call(Request::DevicesRemove {
            device: "pixel_9".into(),
            person: false,
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ResourceInUse, "{e:?}");
    assert!(list(&home, false).devices.iter().any(|d| d.registered));
    home.call(Request::ClaimsRelease(
        hostler::contract::claims::ClaimRelease {
            claim_id: claim.id,
            workspace_id: ws,
            requester: me(),
            exit_code: None,
        },
    ))
    .unwrap();
    home.call(Request::DevicesRemove {
        device: "pixel_9".into(),
        person: false,
    })
    .unwrap();
    let after = list(&home, true);
    let avd = after
        .devices
        .iter()
        .find(|d| d.resource.0 == "device:android:avd:Pixel_9")
        .unwrap();
    assert!(!avd.registered && avd.alias.is_none(), "kept as seen");
}

/// iOS through the fake xcrun (`HOSTLER_TEST_PLATFORM_IOS`): devicectl's file answer and
/// simctl's; an unreachable network phone is listed disconnected and gets no apps call.
#[test]
fn ios_devices_and_simulators_appear() {
    let home = Home::new();
    let tools = FakeTools::new();
    let devicectl = tools.fixture("devicectl-list.json");
    let simctl = tools.fixture("simctl-list-devices.json");
    tools.script(json!([
        {"tool": "xcrun", "args_prefix": ["devicectl", "list", "devices"],
         "json_output_file": devicectl},
        {"tool": "xcrun", "args_prefix": ["simctl", "list", "devices"], "stdout_file": simctl},
    ]));
    start(&home, &tools, &[("HOSTLER_TEST_PLATFORM_IOS", "1")]);
    let snapshot = until(&home, "the iPhones and simulators", |s| {
        has(s, "device:ios:00008101-000A00000000001E")
            && s.devices
                .iter()
                .any(|d| d.kind == hostler::contract::devices::DeviceKind::Simulator)
    });
    let wired = snapshot
        .devices
        .iter()
        .find(|d| d.resource.0 == "device:ios:00008101-000A00000000001E")
        .unwrap();
    assert!(wired.connected);
    assert_eq!(wired.model, "iPhone 12 Pro");
    let network = snapshot
        .devices
        .iter()
        .find(|d| d.resource.0 == "device:ios:00008101-000A00000000000E")
        .unwrap();
    assert!(!network.connected, "its tunnel is down");
    // Every devicectl call wrote to a fresh absolute path.
    let deadline = Instant::now() + Duration::from_secs(10);
    while tools
        .calls()
        .iter()
        .filter(|c| c.args.first().map(String::as_str) == Some("devicectl"))
        .count()
        < 2
    {
        assert!(Instant::now() < deadline, "a second devicectl round");
        std::thread::sleep(Duration::from_millis(50));
    }
    let paths: Vec<String> = tools
        .calls()
        .iter()
        .filter(|c| c.args.starts_with(&["devicectl".into(), "list".into()]))
        .map(|c| c.args[4].clone())
        .collect();
    assert!(paths.len() >= 2);
    assert_ne!(paths[0], paths[1]);
    assert!(
        !tools
            .calls()
            .iter()
            .any(|c| c.args.iter().any(|a| a == "apps")),
        "no apps call (Task 8; and never for an unreachable phone)"
    );
    // The person registers the wired phone.
    let added: Value = home
        .call(Request::DevicesAdd {
            device: "00008101-000A00000000001E".into(),
            alias: None,
            person: true,
        })
        .unwrap();
    assert_eq!(added["alias"], "iphone-12-pro");
    let claims = until_claims(&home, |ids| {
        ids.contains(&"device:ios:00008101-000A00000000001E".to_owned())
    });
    assert!(
        claims
            .resources
            .iter()
            .any(|r| r.resource.display == "iphone-12-pro" && r.resource.available)
    );
}

/// The claims snapshot once the resource ids satisfy `done`, at most 5 s.
fn until_claims(
    home: &Home,
    done: impl Fn(&[String]) -> bool,
) -> hostler::contract::claims::ClaimsSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = home.snapshot();
        let ids: Vec<String> = snapshot
            .resources
            .iter()
            .map(|r| r.resource.id.0.clone())
            .collect();
        if done(&ids) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "{ids:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `HOSTLER_TEST_FAKE_SESSIONS` (P3-6): sessions reach the claims engine through the device
/// cache. Another workspace's session blocks; the device frees once a read no longer lists it.
#[test]
fn fake_sessions_feed_the_claims_engine() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(android(None));
    let sessions = home.dir.path().join("sessions.json");
    std::fs::write(&sessions, r#"{"sessions": []}"#).unwrap();
    start(
        &home,
        &tools,
        &[("HOSTLER_TEST_FAKE_SESSIONS", sessions.to_str().unwrap())],
    );
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    home.call(Request::DevicesAdd {
        device: "Pixel_9".into(),
        alias: None,
        person: false,
    })
    .unwrap();
    std::fs::write(
        &sessions,
        json!({"sessions": [{"resource": "device:android:avd:Pixel_9", "session": "s1",
            "workspace_path": "/somewhere/else", "agent_device_id": EMULATOR}]})
        .to_string(),
    )
    .unwrap();
    until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.alias.as_deref() == Some("pixel_9") && d.sessions.len() == 1)
    });
    let repo = home.make_repo("app");
    let ws = home.workspace(&repo).unwrap().id;
    let request = |no_wait| ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId("device:android:avd:Pixel_9".into()),
        },
        workspace_id: ws.clone(),
        requester: me(),
        purpose: Purpose::Hold {
            ttl_ms: Some(60_000),
            bound_to: None,
            distinct: false,
        },
        reentrant_tokens: Vec::new(),
        no_wait,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let busy: ClaimOutcome =
            serde_json::from_value(home.call(Request::ClaimsAcquire(request(true))).unwrap())
                .unwrap();
        match busy {
            ClaimOutcome::Busy { blockers, .. }
                if matches!(blockers.as_slice(),
                    [hostler::contract::claims::Blocker::Foreign { claim }]
                        if claim.source == "agent-device") =>
            {
                break;
            }
            other => {
                assert!(Instant::now() < deadline, "{other:?}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    // The session closes: the next good read frees the device.
    std::fs::write(&sessions, r#"{"sessions": []}"#).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let outcome: ClaimOutcome =
            serde_json::from_value(home.call(Request::ClaimsAcquire(request(true))).unwrap())
                .unwrap();
        if matches!(outcome, ClaimOutcome::Granted { .. }) {
            break;
        }
        assert!(Instant::now() < deadline, "{outcome:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---- Task 5 R1 (P3bT5-*) ----

/// A hung identity read: longer than the 10 s shell bound, which ends it.
const SLOW_MS: u64 = 30_000;

/// A phone line for `adb devices -l` and its identity entry; `delay_ms` on the identity.
fn phone_line(serial: &str, usb: &str, tid: u32) -> String {
    format!(
        "{serial}             device usb:{usb} product:p model:Phone device:d transport_id:{tid}\n"
    )
}

fn devices_l(lines: &[String]) -> String {
    format!("List of devices attached\n{}\n", lines.concat())
}

fn identity(serial: &str, delay_ms: Option<u64>) -> Value {
    let mut entry = json!({"tool": "adb", "args_prefix": ["-s", serial, "shell"],
        "stdout": format!("{serial}\nPhone {serial}\n15\n35\n\n\n\n")});
    if let Some(ms) = delay_ms {
        entry["delay_ms"] = json!(ms);
    }
    entry
}

/// P3bT5-1: a server that is silent past the bound, or answers `FAIL`, is not "no server":
/// no adb call but `version` ever runs, and the problem says why.
#[test]
fn silent_or_failing_server_gets_no_adb_call() {
    use std::io::{Read, Write};
    for fail in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for mut stream in listener.incoming().flatten() {
                if fail {
                    let mut req = [0u8; 16];
                    let _ = stream.read_exact(&mut req);
                    let _ = stream.write_all(b"FAIL0007unknown");
                }
                held.push(stream);
            }
        });
        let home = Home::new();
        let tools = FakeTools::new();
        tools.script(android(None));
        start(&home, &tools, &[("HOSTLER_TEST_ADB_SERVER_PORT", &port)]);
        let snapshot = until(&home, "the guard refuses", |s| {
            s.tools.problems.iter().any(|p| p.contains("host:version"))
        });
        assert_eq!(snapshot.tools.adb_state, AdbState::Missing);
        std::thread::sleep(Duration::from_millis(1_500));
        for call in adb_calls(&tools) {
            assert_eq!(call.args, ["version"], "fail={fail}: {call:?}");
        }
        assert!(!has(&list(&home, true), "device:android:SERIAL0001"));
    }
}

/// P3bT5-1: without hostler's own adb version nothing is compared, so not even
/// `start-server` runs.
#[test]
fn no_start_server_without_a_client_version() {
    let home = Home::new();
    let tools = FakeTools::new();
    let mut script = android(None);
    // No answer to `adb version`: the fake exits 1.
    script.as_array_mut().unwrap().remove(0);
    tools.script(script);
    start(&home, &tools, &[]);
    until(&home, "no client version", |s| {
        s.tools
            .problems
            .iter()
            .any(|p| p.starts_with("hostler cannot read its own adb's version"))
    });
    std::thread::sleep(Duration::from_millis(1_000));
    for call in adb_calls(&tools) {
        assert_eq!(call.args, ["version"], "{call:?}");
    }
}

/// P3bT5-3: a phone whose identity read hangs never holds back another phone.
#[test]
fn hung_identity_never_delays_other_phones() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    tools.script(json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["devices", "-l"],
         "stdout": devices_l(&[phone_line("FAST0001", "1-1", 1), phone_line("SLOW0001", "1-2", 2)])},
        identity("FAST0001", None),
        identity("SLOW0001", Some(SLOW_MS)),
    ]));
    start(&home, &tools, &[]);
    // From the first listing: before this change the fast phone waited for the slow read's
    // 10 s bound.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !adb_calls(&tools)
        .iter()
        .any(|c| c.args == ["devices", "-l"])
    {
        assert!(Instant::now() < deadline, "adb devices -l never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    let listed = Instant::now();
    until(&home, "the fast phone", |s| {
        has(s, "device:android:FAST0001")
    });
    assert!(
        listed.elapsed() < Duration::from_secs(5),
        "{:?}",
        listed.elapsed()
    );
    assert!(!has(&list(&home, true), "device:android:SLOW0001"));
}

/// P3bT5-3: with the Probe lane full of slow identity reads, a phone that leaves
/// `adb devices -l` reads disconnected at once.
#[test]
fn unplug_seen_while_probe_lane_is_full() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let all = devices_l(&[
        phone_line("FAST0001", "1-1", 1),
        phone_line("SLOW0001", "1-2", 2),
        phone_line("SLOW0002", "1-3", 3),
    ]);
    let without = devices_l(&[
        phone_line("SLOW0001", "1-2", 2),
        phone_line("SLOW0002", "1-3", 3),
    ]);
    // Once the fast phone is known: one listing of all three (their slow reads fill the lane),
    // then the fast phone is gone.
    let phase2 = json!([
        adb_version(),
        // Held 2 s, so the test sees the fast phone connected before it goes.
        {"tool": "adb", "args_prefix": ["devices", "-l"], "stdout": all, "once": true,
         "delay_ms": 2_000},
        {"tool": "adb", "args_prefix": ["devices", "-l"], "stdout": without},
        identity("FAST0001", None),
        identity("SLOW0001", Some(SLOW_MS)),
        identity("SLOW0002", Some(SLOW_MS)),
    ]);
    let mut fast = identity("FAST0001", None);
    fast["then_write"] = phase2;
    tools.script(json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["devices", "-l"],
         "stdout": devices_l(&[phone_line("FAST0001", "1-1", 1)])},
        fast,
    ]));
    start(&home, &tools, &[]);
    let connected = |s: &DevicesSnapshot| {
        s.devices
            .iter()
            .any(|d| d.resource.0 == "device:android:FAST0001" && d.connected)
    };
    until(&home, "the fast phone", connected);
    let seen = Instant::now();
    until(&home, "the fast phone unplugged", |s| !connected(s));
    // The 2 s hold plus a round; before this change also the slow reads' 10 s bound.
    assert!(
        seen.elapsed() < Duration::from_secs(7),
        "{:?}",
        seen.elapsed()
    );
    // The slow reads were still running then.
    assert!(
        adb_calls(&tools)
            .iter()
            .any(|c| c.args.first().map(String::as_str) == Some("-s")
                && c.args.get(1).map(String::as_str) == Some("SLOW0001")),
        "the slow reads started"
    );
}

/// P3bT5-2: a booted AVD on `emulator-5554` says nothing about the next emulator there (a
/// new transport id) whose `sys.boot_completed` is still 0.
#[test]
fn reused_emulator_serial_is_not_ready_until_boot_completed() {
    let home = Home::new();
    let mut tools = FakeTools::new();
    tools.adb_server(41);
    let emulator_line = |tid: u32| {
        format!(
            "{EMULATOR}          device product:sdk model:sdk_gphone64 device:emu64a transport_id:{tid}\n"
        )
    };
    let avds =
        json!({"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": "Pixel_9\nTablet\n"});
    let identity = |avd: &str| {
        json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell"],
               "stdout": format!("EMULATOR35X1X1X0\nsdk_gphone64_arm64\n15\n35\n{avd}\n1\n\n")})
    };
    let boot = |answer: &str| {
        json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", "getprop sys.boot_completed"],
               "stdout": answer})
    };
    let tablet = json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["devices", "-l"], "stdout": devices_l(&[emulator_line(3)])},
        boot("0\n"),
        identity("Tablet"),
        avds.clone(),
    ]);
    let mut booted = boot("1\n");
    booted["then_write"] = tablet;
    tools.script(json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["devices", "-l"], "stdout": devices_l(&[emulator_line(2)])},
        booted,
        identity("Pixel_9"),
        avds,
    ]));
    start(&home, &tools, &[]);
    let info = |s: &DevicesSnapshot, avd: &str| {
        s.devices
            .iter()
            .find(|d| d.resource.0 == format!("device:android:avd:{avd}"))
            .map(|d| (d.connected, d.ready))
    };
    until(&home, "the tablet runs", |s| {
        info(s, "Tablet").is_some_and(|(connected, _)| connected)
    });
    let deadline = Instant::now() + Duration::from_millis(1_500);
    while Instant::now() < deadline {
        let s = list(&home, true);
        assert_eq!(info(&s, "Tablet"), Some((true, false)), "{s:#?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}
