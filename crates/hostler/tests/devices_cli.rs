//! `hostler devices` commands and the device lines of `hostler status` (P3b Task 6), against the
//! real binary, a real service and Task 2's fake tools. No real adb, xcrun, ideviceinfo or
//! agent-device ever runs here; every logged call must pass the allowlist (`FakeTools`' check).

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use fake_tools::FakeTools;
use hostler::contract::claims::{
    ClaimKind, ClaimOutcome, ClaimRelease, ClaimRequest, ClaimsSnapshot, Purpose, ResourceId,
    Selector,
};
use hostler::contract::devices::{AdbState, DeviceKind, DevicesSnapshot};
use hostler::contract::wire::Request;
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use serde_json::{Value, json};

const PHONE: &str = "SERIAL0001";
const EMULATOR: &str = "emulator-5554";
const AVD: &str = "Pixel_9";
const SIMULATOR: &str = "00000000-0000-4000-8000-0000000000D1";

fn adb_version() -> Value {
    json!({"tool": "adb", "args_prefix": ["version"],
           "stdout": "Android Debug Bridge version 1.0.41\nVersion 35.0.2-12147458\n"})
}

/// A USB phone and a booted emulator, each with its identity; the AVD exists.
fn android() -> Value {
    android_model("Pixel 9")
}

/// `android()` with the phone's device-reported model.
fn android_model(model: &str) -> Value {
    json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["start-server"]},
        {"tool": "adb", "args_prefix": ["devices", "-l"],
         "stdout": format!("List of devices attached\n\
            {PHONE}             device usb:1-1 product:p model:Pixel_9 device:d transport_id:1\n\
            {EMULATOR}          device product:sdk model:sdk_gphone64 device:emu64a transport_id:2\n\n")},
        // The phone's last three getprop lines are empty (never trimmed).
        {"tool": "adb", "args_prefix": ["-s", PHONE, "shell"],
         "stdout": format!("{PHONE}\n{model}\n15\n35\n\n\n\n")},
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

fn run(home: &Home, args: &[&str]) -> Output {
    home.hostler()
        .args(args)
        .current_dir(home.dir.path())
        .output()
        .unwrap()
}

fn run_in(home: &Home, cwd: &Path, args: &[&str]) -> Output {
    home.hostler().args(args).current_dir(cwd).output().unwrap()
}

fn tty_file(home: &Home, lines: &[&str]) -> PathBuf {
    let path = home
        .dir
        .path()
        .join(format!("tty-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn run_tty(home: &Home, args: &[&str], lines: &[&str]) -> Output {
    run_tty_in(home, home.dir.path(), args, lines)
}

fn run_tty_in(home: &Home, cwd: &Path, args: &[&str], lines: &[&str]) -> Output {
    home.hostler()
        .args(args)
        .current_dir(cwd)
        .env("HOSTLER_TEST_TTY", tty_file(home, lines))
        .output()
        .unwrap()
}

fn envelope(out: &Output) -> Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| {
        panic!(
            "not an envelope ({e}): {:?}; stderr: {}",
            stdout(out),
            stderr(out)
        )
    })
}

fn add_avd(home: &Home) {
    let out = run(home, &["devices", "add", AVD]);
    assert!(out.status.success(), "add the AVD: {}", stderr(&out));
}

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

#[test]
fn list_shows_new_physical_by_default_virtual_with_all() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone and the AVD", |s| {
        has(s, "device:android:SERIAL0001") && has(s, "device:android:avd:Pixel_9")
    });

    let plain = run(&home, &["devices", "list"]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    let text = stdout(&plain);
    assert!(text.contains("SERIAL0001"), "{text}");
    assert!(text.contains("Pixel 9"), "{text}");
    assert!(text.contains("android physical"), "{text}");
    assert!(text.contains("usb"), "{text}");
    assert!(
        text.contains("new: a person registers it with hostler devices add SERIAL0001"),
        "{text}"
    );
    assert!(!text.contains("Pixel_9"), "the unregistered AVD: {text}");

    let all = run(&home, &["devices", "list", "--all"]);
    assert!(all.status.success(), "{}", stderr(&all));
    let text = stdout(&all);
    assert!(text.contains("Pixel_9"), "{text}");
    assert!(text.contains("new: hostler devices add Pixel_9"), "{text}");
}

#[test]
fn add_physical_needs_the_typed_model() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));

    let wrong = run_tty(&home, &["devices", "add", PHONE], &["not the model"]);
    assert_eq!(wrong.status.code(), Some(1), "{}", stderr(&wrong));
    assert!(
        stderr(&wrong).contains("the model did not match"),
        "{}",
        stderr(&wrong)
    );
    let info = list(&home, true)
        .devices
        .into_iter()
        .find(|d| d.resource.0 == "device:android:SERIAL0001")
        .unwrap();
    assert!(!info.registered, "a wrong model registered it");

    let right = run_tty(&home, &["devices", "add", PHONE], &["Pixel 9"]);
    assert!(right.status.success(), "{}", stderr(&right));
    assert!(
        stdout(&right).contains("Registered Pixel 9 as pixel-9"),
        "{}",
        stdout(&right)
    );
    let info = list(&home, false)
        .devices
        .into_iter()
        .find(|d| d.resource.0 == "device:android:SERIAL0001")
        .unwrap();
    assert!(info.registered);
    assert_eq!(info.alias.as_deref(), Some("pixel-9"));
    assert!(info.alias_by_person);
}

#[test]
fn add_physical_without_terminal_refuses_78() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));

    let out = run(&home, &["devices", "add", PHONE]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("needs a person at a terminal"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("To fix: hostler devices add SERIAL0001"),
        "{}",
        stderr(&out)
    );
    let json = envelope(&run(&home, &["devices", "add", PHONE, "--json"]));
    assert_eq!(json["ok"], false, "{json}");
    assert_eq!(json["error"]["code"], "not_interactive", "{json}");
    assert_eq!(
        json["error"]["recovery"], "hostler devices add SERIAL0001",
        "{json}"
    );
    assert!(
        !list(&home, true)
            .devices
            .into_iter()
            .find(|d| d.resource.0 == "device:android:SERIAL0001")
            .unwrap()
            .registered
    );
}

/// A device-reported model never reaches the person's terminal raw.
#[test]
fn add_physical_prompt_is_sanitized() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android_model("Pix\u{1b}]0;PWNED\u{7}el"));
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));

    let out = run_tty(&home, &["devices", "add", PHONE], &["nothing"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(!err.contains('\u{1b}'), "raw escape in the prompt: {err:?}");
    assert!(!err.contains('\u{7}'), "raw bell in the prompt: {err:?}");
    assert!(err.contains('\u{FFFD}'), "the model was not shown: {err:?}");
    assert!(
        !list(&home, true)
            .devices
            .into_iter()
            .find(|d| d.resource.0 == "device:android:SERIAL0001")
            .unwrap()
            .registered
    );
}

/// Physical `rename` and `remove` are person-only too: exit 78, nothing changed, a runnable
/// recovery.
#[test]
fn physical_rename_and_remove_without_terminal_refuse_78() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));
    let add = run_tty(&home, &["devices", "add", PHONE], &["Pixel 9"]);
    assert!(add.status.success(), "{}", stderr(&add));

    let out = run(&home, &["devices", "rename", "pixel-9", "left"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("To fix: hostler devices rename pixel-9 left"),
        "{}",
        stderr(&out)
    );
    let json = envelope(&run(
        &home,
        &["devices", "rename", "pixel-9", "left", "--json"],
    ));
    assert_eq!(
        json["error"]["recovery"], "hostler devices rename pixel-9 left",
        "{json}"
    );
    let info = list(&home, false)
        .devices
        .into_iter()
        .find(|d| d.resource.0 == "device:android:SERIAL0001")
        .unwrap();
    assert_eq!(info.alias.as_deref(), Some("pixel-9"));

    let out = run(&home, &["devices", "remove", "pixel-9"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("To fix: hostler devices remove pixel-9"),
        "{}",
        stderr(&out)
    );
    let json = envelope(&run(&home, &["devices", "remove", "pixel-9", "--json"]));
    assert_eq!(
        json["error"]["recovery"], "hostler devices remove pixel-9",
        "{json}"
    );
    assert!(
        list(&home, false)
            .devices
            .into_iter()
            .find(|d| d.resource.0 == "device:android:SERIAL0001")
            .unwrap()
            .registered
    );
}

#[test]
fn add_simulator_needs_no_person() {
    let tools = FakeTools::new();
    let home = Home::new();
    let devicectl = tools.fixture("devicectl-list.json");
    let simctl = tools.fixture("simctl-list-devices.json");
    tools.script(json!([
        {"tool": "xcrun", "args_prefix": ["devicectl", "list", "devices"],
         "json_output_file": devicectl},
        {"tool": "xcrun", "args_prefix": ["simctl", "list", "devices"], "stdout_file": simctl},
    ]));
    start(&home, &tools, &[("HOSTLER_TEST_PLATFORM_IOS", "1")]);
    until(&home, "the simulator", |s| {
        has(s, &format!("device:ios:{SIMULATOR}"))
    });

    let out = run(&home, &["devices", "add", SIMULATOR]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("iphone-17-pro-max"),
        "{}",
        stdout(&out)
    );
    let info = list(&home, false)
        .devices
        .into_iter()
        .find(|d| d.resource.0 == format!("device:ios:{SIMULATOR}"))
        .unwrap();
    assert!(info.registered);
    assert_eq!(info.kind, DeviceKind::Simulator);
    assert!(!info.alias_by_person, "an agent may add a simulator");
}

#[test]
fn rename_conflict_names_the_holder() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone and the AVD", |s| {
        has(s, "device:android:SERIAL0001") && has(s, "device:android:avd:Pixel_9")
    });
    add_avd(&home);
    let renamed = run(&home, &["devices", "rename", "pixel_9", "avd-9"]);
    assert!(renamed.status.success(), "{}", stderr(&renamed));
    let phone = run_tty(&home, &["devices", "add", PHONE], &["Pixel 9"]);
    assert!(phone.status.success(), "{}", stderr(&phone));

    let out = run_tty(
        &home,
        &["devices", "rename", "pixel-9", "avd-9"],
        &["Pixel 9"],
    );
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(stderr(&out).contains("taken by avd-9"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("hostler devices rename avd-9 <other>"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn remove_refused_while_held() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    add_avd(&home);
    let repo = home.make_repo("app");
    let ws = home.workspace(&repo).unwrap().id;
    let request = ClaimRequest {
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
        serde_json::from_value(home.call(Request::ClaimsAcquire(request)).unwrap()).unwrap();
    let ClaimOutcome::Granted { claim } = granted else {
        panic!("{granted:?}");
    };

    let out = run(&home, &["devices", "remove", "pixel_9"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(stderr(&out).contains("claimed by"), "{}", stderr(&out));
    let workspace = home.workspace(&repo).unwrap();
    assert!(stderr(&out).contains(&workspace.path), "{}", stderr(&out));
    assert!(list(&home, false).devices.iter().any(|d| d.registered));

    home.call(Request::ClaimsRelease(ClaimRelease {
        claim_id: claim.id,
        workspace_id: ws,
        requester: me(),
        exit_code: None,
    }))
    .unwrap();
    let out = run(&home, &["devices", "remove", "pixel_9"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let info = list(&home, true)
        .devices
        .into_iter()
        .find(|d| d.resource.0 == "device:android:avd:Pixel_9")
        .unwrap();
    assert!(!info.registered && info.alias.is_none(), "kept as seen");
}

#[test]
fn reserve_and_unreserve_per_workspace() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    add_avd(&home);
    let repo = home.make_repo("app");
    let ws = home.workspace(&repo).unwrap().id;
    let other = home.make_repo("other");
    home.workspace(&other).unwrap();

    let out = run_in(&home, &repo, &["devices", "reserve", "pixel_9"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Reserved pixel_9"),
        "{}",
        stdout(&out)
    );
    let reservation = |snapshot: &ClaimsSnapshot| {
        snapshot
            .resources
            .iter()
            .filter(|r| r.resource.id.0 == "device:android:avd:Pixel_9")
            .flat_map(|r| r.claims.clone())
            .find(|c| c.kind == ClaimKind::Reservation)
    };
    let held = reservation(&home.snapshot()).expect("a reservation");
    assert_eq!(held.workspace.id, ws);

    // Another workspace may not reserve it while the first holds the reservation.
    let out = run_in(&home, &other, &["devices", "reserve", "pixel_9"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(stderr(&out).contains("claimed by"), "{}", stderr(&out));

    let out = run_in(&home, &repo, &["devices", "unreserve", "pixel_9"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(reservation(&home.snapshot()).is_none());
    let out = run_in(&home, &repo, &["devices", "unreserve", "pixel_9"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
}

#[test]
fn testing_in_progress_line() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    let sessions = home.dir.path().join("sessions.json");
    std::fs::write(
        &sessions,
        json!({"sessions": [{"resource": "device:android:avd:Pixel_9", "session": "s1",
            "workspace_path": "/somewhere/else", "agent_device_id": EMULATOR}]})
        .to_string(),
    )
    .unwrap();
    start(
        &home,
        &tools,
        &[("HOSTLER_TEST_FAKE_SESSIONS", sessions.to_str().unwrap())],
    );
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    add_avd(&home);
    until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.alias.as_deref() == Some("pixel_9") && d.sessions.len() == 1)
    });

    let out = run(&home, &["devices", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("testing in progress: agent-device session s1 in /somewhere/else"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn unauthorized_hint() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(json!([
        adb_version(),
        {"tool": "adb", "args_prefix": ["devices", "-l"],
         "stdout": "List of devices attached\nSERIAL0009 unauthorized usb:1-3 transport_id:9\n\n"},
    ]));
    start(&home, &tools, &[]);
    until(&home, "the unauthorized transport", |s| {
        !s.unidentified.is_empty()
    });

    let all = run(&home, &["devices", "list", "--all"]);
    assert!(all.status.success(), "{}", stderr(&all));
    let text = stdout(&all);
    assert!(text.contains("SERIAL0009"), "{text}");
    assert!(text.contains("unauthorized"), "{text}");
    assert!(text.contains("accept the prompt on the phone"), "{text}");
    let plain = run(&home, &["devices", "list"]);
    let text = stdout(&plain);
    assert!(!text.contains("SERIAL0009"), "{text}");
    assert!(
        text.contains("1 transport cannot be identified: hostler devices list --all"),
        "{text}"
    );
}

#[test]
fn list_shows_closed_until_agent_device_can_be_read() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    let sessions = home.dir.path().join("sessions.json");
    std::fs::write(
        &sessions,
        json!({"readable": false, "reason": "node not found"}).to_string(),
    )
    .unwrap();
    start(
        &home,
        &tools,
        &[
            ("HOSTLER_TEST_FAKE_SESSIONS", sessions.to_str().unwrap()),
            ("HOSTLER_TEST_DEVICES_STALE_MS", "1"),
        ],
    );
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    add_avd(&home);
    until(&home, "a closed device", |s| {
        s.devices
            .iter()
            .any(|d| d.alias.as_deref() == Some("pixel_9") && d.checking_sessions)
    });

    let out = run(&home, &["devices", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("closed until agent-device can be read"),
        "{text}"
    );
    assert!(
        text.contains("problem: agent-device sessions unknown: node not found"),
        "{text}"
    );
}

#[test]
fn status_includes_devices_and_problems() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    // Another adb owns the server: a visible problem, and AVDs are still discovered.
    tools.adb_server(40);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    until(&home, "the foreign adb problem", |s| {
        matches!(
            s.tools.adb_state,
            hostler::contract::devices::AdbState::Foreign { version: 40 }
        )
    });
    add_avd(&home);

    let out = run(&home, &["status"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("pixel_9:"), "{text}");
    assert!(
        text.contains("problem: another adb (version 40) owns the adb server"),
        "{text}"
    );
}

#[test]
fn status_hides_unregistered_phones() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));

    let out = run(&home, &["status"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("SERIAL0001"), "{text}");
    assert!(
        text.contains("1 new device: hostler devices list shows it"),
        "{text}"
    );
    assert!(
        !text.lines().any(|line| line.contains(": usb;")),
        "an unregistered phone is a resource line: {text}"
    );

    // `status --json` stays the frozen Overview.
    let json = envelope(&run(&home, &["status", "--json"]));
    assert_eq!(json["ok"], true, "{json}");
    let data = &json["data"];
    for key in ["service_version", "heavy", "claims", "secrets"] {
        assert!(data.get(key).is_some(), "missing {key}: {json}");
    }
    assert!(data.get("devices").is_none(), "{json}");
    assert!(data["claims"]["resources"].is_array(), "{json}");
}

#[test]
fn other_agents_text_is_sanitized() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android_model("Pix\u{1b}]0;PWNED\u{7}el"));
    let sessions = home.dir.path().join("sessions.json");
    std::fs::write(
        &sessions,
        json!({"sessions": [{"resource": "device:android:avd:Pixel_9",
            "session": "s\u{1b}1",
            "workspace_path": "/work/\u{1b}]0;PWNED\u{7}", "agent_device_id": EMULATOR}]})
        .to_string(),
    )
    .unwrap();
    start(
        &home,
        &tools,
        &[("HOSTLER_TEST_FAKE_SESSIONS", sessions.to_str().unwrap())],
    );
    until(&home, "the phone and the AVD", |s| {
        has(s, "device:android:SERIAL0001") && has(s, "device:android:avd:Pixel_9")
    });
    add_avd(&home);
    until(&home, "the session", |s| {
        s.devices
            .iter()
            .any(|d| d.alias.as_deref() == Some("pixel_9") && d.sessions.len() == 1)
    });

    for args in [
        &["devices", "list"][..],
        &["devices", "show", "pixel_9"],
        &["status"],
    ] {
        let out = run(&home, args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        let text = stdout(&out);
        assert!(!text.contains('\u{1b}'), "{args:?}: {text:?}");
        assert!(!text.contains('\u{7}'), "{args:?}: {text:?}");
        assert!(text.contains('\u{FFFD}'), "{args:?}: {text:?}");
    }
}

#[test]
fn json_envelopes() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the phone", |s| has(s, "device:android:SERIAL0001"));

    let listed = envelope(&run(&home, &["devices", "list", "--json"]));
    assert_eq!(listed["ok"], true, "{listed}");
    assert!(listed["data"]["devices"].is_array(), "{listed}");
    assert!(listed["data"]["tools"].is_object(), "{listed}");

    let shown = envelope(&run(&home, &["devices", "show", PHONE, "--json"]));
    assert_eq!(shown["ok"], true, "{shown}");
    assert_eq!(
        shown["data"]["info"]["resource"],
        "device:android:SERIAL0001"
    );

    let missing = run(&home, &["devices", "show", "nope", "--json"]);
    assert_eq!(missing.status.code(), Some(78), "{}", stderr(&missing));
    let missing = envelope(&missing);
    assert_eq!(missing["ok"], false, "{missing}");
    assert_eq!(missing["error"]["code"], "device_not_found", "{missing}");
    assert_eq!(missing["error"]["recovery"], "hostler devices list --all");

    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    let added = envelope(&run(&home, &["devices", "add", AVD, "--json"]));
    assert_eq!(added["ok"], true, "{added}");
    assert_eq!(added["data"]["alias"], "pixel_9", "{added}");
    let renamed = envelope(&run(
        &home,
        &["devices", "rename", "pixel_9", "avd-9", "--json"],
    ));
    assert_eq!(renamed["data"]["alias"], "avd-9", "{renamed}");
    let noted = envelope(&run(
        &home,
        &["devices", "note", "avd-9", "on the left", "--json"],
    ));
    assert_eq!(noted["data"]["note"], "on the left", "{noted}");

    let repo = home.make_repo("app");
    let reserved = envelope(&run_in(
        &home,
        &repo,
        &["devices", "reserve", "avd-9", "--json"],
    ));
    assert_eq!(reserved["ok"], true, "{reserved}");
    assert_eq!(
        reserved["data"]["resource"]["display"], "avd-9",
        "{reserved}"
    );
    let unreserved = envelope(&run_in(
        &home,
        &repo,
        &["devices", "unreserve", "avd-9", "--json"],
    ));
    assert_eq!(unreserved["ok"], true, "{unreserved}");
    assert_eq!(unreserved["data"], Value::Null, "{unreserved}");

    let removed = envelope(&run(&home, &["devices", "remove", "avd-9", "--json"]));
    assert_eq!(removed["ok"], true, "{removed}");
    assert_eq!(
        removed["data"]["removed"], "device:android:avd:Pixel_9",
        "{removed}"
    );
    let missing = run(&home, &["devices", "remove", "avd-9", "--json"]);
    assert_eq!(missing.status.code(), Some(78), "{}", stderr(&missing));
    let missing = envelope(&missing);
    assert_eq!(missing["error"]["code"], "device_not_found", "{missing}");

    let installs = envelope(&run(&home, &["devices", "installs", "--json"]));
    assert_eq!(installs["ok"], true, "{installs}");
    assert_eq!(installs["data"], json!([]), "{installs}");

    let tools = envelope(&run(&home, &["devices", "tools", "--json"]));
    assert_eq!(tools["ok"], true, "{tools}");
    assert!(tools["data"]["adb"].is_string(), "{tools}");
}

#[test]
fn tools_detect_records_absolute_paths() {
    let root = tempfile::tempdir().unwrap();
    let global = npm_layout(root.path(), "global");
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let local = npm_layout(&project, "checkout");

    // The T2 duty: an entry inside a registered workspace is dropped, and a later global one is
    // found instead.
    let workspace = hostler::project::display_path(&std::fs::canonicalize(&project).unwrap());
    let path = std::env::join_paths([local.clone(), global.clone()]).unwrap();
    let mut env = std::collections::BTreeMap::new();
    env.insert("ANDROID_HOME".to_owned(), "/sdk".to_owned());
    let (set, skipped) =
        hostler::cli::devices::detect_set(&path, &[("ws-1".to_owned(), workspace)], env.clone());
    assert_eq!(skipped, vec![local.to_string_lossy().into_owned()]);
    let agent_device = set.agent_device.as_deref().unwrap();
    assert_eq!(Path::new(agent_device), global.join("agent-device"));
    assert!(Path::new(agent_device).is_absolute());
    let script = set.detected_script.as_deref().unwrap();
    assert_eq!(
        Path::new(script),
        root.path()
            .join("global/lib/node_modules/agent-device/bin/agent-device.mjs")
    );
    assert!(Path::new(script).is_absolute());
    assert_eq!(
        set.node.as_deref(),
        Some(global.join("node").to_str().unwrap())
    );
    assert_eq!(set.android_env, env);

    // The command records them through the service (the fakes answer the status) and says what
    // it recorded, so the person can see the absolute paths it sent.
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    let global_path = std::env::join_paths([global.clone()]).unwrap();
    let out = home
        .hostler()
        .args(["devices", "tools", "--detect"])
        .current_dir(home.dir.path())
        .env("PATH", &global_path)
        .env("ANDROID_HOME", "/sdk")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains(&format!(
            "recorded agent-device: {}",
            global.join("agent-device").display()
        )),
        "{err}"
    );
    assert!(
        err.contains(&format!("recorded node: {}", global.join("node").display())),
        "{err}"
    );
    assert!(
        err.contains("lib/node_modules/agent-device/bin/agent-device.mjs"),
        "{err}"
    );
    assert!(err.contains("recorded ANDROID_HOME: /sdk"), "{err}");

    let out = home
        .hostler()
        .args(["devices", "tools", "--detect", "--json"])
        .current_dir(home.dir.path())
        .env("PATH", &global_path)
        .env("ANDROID_HOME", "/sdk")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(envelope(&out)["ok"], true);
}

#[test]
fn tools_detect_finds_fnm_nvm_volta_asdf_installs() {
    let home = tempfile::tempdir().unwrap();
    for kind in ["nvm", "fnm", "volta", "asdf"] {
        let (bin, script) = manager_layout(home.path(), kind);
        let path = std::env::join_paths([bin.clone()]).unwrap();
        let (set, skipped) =
            hostler::cli::devices::detect_set(&path, &[], std::collections::BTreeMap::new());
        assert!(skipped.is_empty(), "{kind}: {skipped:?}");
        assert_eq!(
            set.agent_device.as_deref(),
            Some(bin.join("agent-device").to_str().unwrap()),
            "{kind}"
        );
        assert_eq!(
            set.node.as_deref(),
            Some(bin.join("node").to_str().unwrap()),
            "{kind}"
        );
        assert_eq!(
            set.detected_script.as_deref(),
            script.as_deref().map(|p| p.to_str().unwrap()),
            "{kind}"
        );
    }
}

#[test]
fn detect_set_skips_node_modules_bin_and_relative_entries() {
    let root = tempfile::tempdir().unwrap();
    let global = npm_layout(root.path(), "global");
    let local = root.path().join("project/node_modules/.bin");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("agent-device"), b"#!/bin/sh\n").unwrap();
    std::fs::write(local.join("node"), b"").unwrap();
    let path = std::env::join_paths([
        std::path::PathBuf::from("relative"),
        std::path::PathBuf::new(),
        local.clone(),
        global.clone(),
    ])
    .unwrap();
    let (set, skipped) =
        hostler::cli::devices::detect_set(&path, &[], std::collections::BTreeMap::new());
    assert_eq!(skipped, vec![local.to_string_lossy().into_owned()]);
    assert_eq!(
        set.agent_device.as_deref(),
        Some(global.join("agent-device").to_str().unwrap())
    );
    assert_eq!(
        set.node.as_deref(),
        Some(global.join("node").to_str().unwrap())
    );
}

#[test]
fn detect_set_keeps_absolute_android_paths_and_empty_clears() {
    let mut env = std::collections::BTreeMap::new();
    env.insert("ANDROID_HOME".to_owned(), "sdk".to_owned());
    env.insert("ANDROID_SDK_ROOT".to_owned(), "/sdk".to_owned());
    env.insert("ANDROID_AVD_HOME".to_owned(), String::new());
    env.insert("ANDROID_ADB_SERVER_PORT".to_owned(), "5038".to_owned());
    let (set, _) = hostler::cli::devices::detect_set(std::ffi::OsStr::new(""), &[], env);
    assert!(!set.android_env.contains_key("ANDROID_HOME"));
    assert_eq!(set.android_env["ANDROID_SDK_ROOT"], "/sdk");
    assert_eq!(set.android_env["ANDROID_AVD_HOME"], "");
    assert_eq!(set.android_env["ANDROID_ADB_SERVER_PORT"], "5038");
}

/// A manager's real install layout, in a fake home: the shim/bin dir and the script it resolves
/// to (None for asdf's shell shims, which do not lead to the package).
fn manager_layout(home: &Path, kind: &str) -> (PathBuf, Option<PathBuf>) {
    match kind {
        "nvm" => npm_layout_at(&home.join(".nvm/versions/node/v22.22.3")),
        "fnm" => npm_layout_at(&home.join("Library/Application Support/fnm/aliases/default")),
        "volta" => {
            let bin = home.join(".volta/bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("node"), b"").unwrap();
            std::fs::write(bin.join("agent-device"), b"#!/bin/sh\n").unwrap();
            let package =
                home.join(".volta/tools/image/packages/agent-device/node_modules/agent-device");
            write_package(&package);
            (bin, Some(package.join("bin/agent-device.mjs")))
        }
        "asdf" => {
            let bin = home.join(".asdf/shims");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("node"), b"#!/bin/sh\n").unwrap();
            std::fs::write(bin.join("agent-device"), b"#!/bin/sh\n").unwrap();
            (bin, None)
        }
        other => panic!("unknown manager {other}"),
    }
}

#[test]
fn tools_read_keeps_the_foreign_adb() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(40);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the foreign adb", |s| {
        matches!(s.tools.adb_state, AdbState::Foreign { version: 40 })
    });

    let out = run(&home, &["devices", "tools", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json = envelope(&out);
    assert_eq!(
        json["data"]["adb_state"]["foreign"]["version"], 40,
        "{json}"
    );
    assert!(
        json["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("another adb")),
        "{json}"
    );
    // The read did not reset the observation.
    let after = list(&home, true);
    assert!(
        matches!(after.tools.adb_state, AdbState::Foreign { version: 40 }),
        "{:?}",
        after.tools.adb_state
    );
}

/// P22-1: another adb's server (40) is replaced by hostler's version (41). With no request at
/// all, the discovery asks again within its 10 s re-probe and runs adb again.
#[test]
fn foreign_adb_server_gone_is_noticed_within_seconds() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(40);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the foreign adb", |s| {
        matches!(s.tools.adb_state, AdbState::Foreign { version: 40 })
    });
    let lists = || {
        tools
            .calls()
            .iter()
            .filter(|c| c.tool == "adb" && c.args.starts_with(&["devices".into(), "-l".into()]))
            .count()
    };
    assert_eq!(lists(), 0, "no adb call while foreign");
    tools.set_adb_server_version(41);
    let replaced = Instant::now();
    // Only the fakes' log is read here: no request re-probes for the discovery.
    while lists() == 0 {
        assert!(
            replaced.elapsed() < Duration::from_secs(20),
            "adb never ran again: {:?}",
            tools.calls()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    until(&home, "adb ours again", |s| {
        s.tools.adb_state == AdbState::Ours && has(s, "device:android:SERIAL0001")
    });
}

/// P22-1: a `devices tools` call answers with the server as it is now: the read (no flags)
/// and `--adb <the same path>` both clear a stale `Foreign` in their own answer.
#[test]
fn devices_tools_clears_a_stale_foreign_at_once() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(40);
    tools.script(android());
    start(&home, &tools, &[]);
    until(&home, "the foreign adb", |s| {
        matches!(s.tools.adb_state, AdbState::Foreign { version: 40 })
    });
    let state = |out: &Output| {
        assert!(out.status.success(), "{}", stderr(out));
        let json = envelope(out);
        let foreign = json["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("another adb"));
        (json["data"]["adb_state"].clone(), foreign)
    };
    tools.set_adb_server_version(41);
    // Past the guard's 1 s reuse of its last answer (P3-10), far inside the 10 s re-probe.
    std::thread::sleep(Duration::from_millis(1_200));
    let read = run(&home, &["devices", "tools", "--json"]);
    assert_eq!(state(&read), (json!("ours"), false), "{}", stdout(&read));
    // Foreign again; then `--adb` with the path it already has.
    tools.set_adb_server_version(40);
    until(&home, "foreign again", |s| {
        matches!(s.tools.adb_state, AdbState::Foreign { version: 40 })
    });
    let adb = envelope(&read)["data"]["adb"].as_str().unwrap().to_owned();
    tools.set_adb_server_version(41);
    let set = run(&home, &["devices", "tools", "--adb", &adb, "--json"]);
    assert_eq!(state(&set), (json!("ours"), false), "{}", stdout(&set));
}

#[test]
fn tools_explicit_agent_device_without_a_script_refuses() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);
    let not_a_launcher = home.dir.path().join("not-agent-device");
    std::fs::write(&not_a_launcher, b"").unwrap();

    let out = run(
        &home,
        &[
            "devices",
            "tools",
            "--agent-device",
            not_a_launcher.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let json = envelope(&out);
    assert_eq!(json["error"]["code"], "bad_request", "{json}");
    assert!(
        json["error"]["recovery"]
            .as_str()
            .unwrap()
            .contains("--agent-device"),
        "{json}"
    );
}

#[test]
fn tools_relative_flag_paths_become_absolute_and_warn_only_when_project_local() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    start(&home, &tools, &[]);

    let out = run(
        &home,
        &["devices", "tools", "--adb", "./adb", "--node", "./node"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("not an absolute path"),
        "{}",
        stderr(&out)
    );

    // A global path gets no warning.
    let root = tempfile::tempdir().unwrap();
    let global = npm_layout(root.path(), "global");
    let out = run(
        &home,
        &[
            "devices",
            "tools",
            "--agent-device",
            global.join("agent-device").to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("goes away with its checkout"),
        "{}",
        stderr(&out)
    );

    // A checkout's copy does.
    let project = home.dir.path().join("project");
    let local = npm_layout_at(&project.join("node_modules/agent-device")).0;
    let out = run(
        &home,
        &[
            "devices",
            "tools",
            "--agent-device",
            local.join("agent-device").to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("this path goes away with its checkout"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn forget_sessions_needs_the_typed_alias_and_a_terminal() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    let sessions = home.dir.path().join("sessions.json");
    std::fs::write(
        &sessions,
        json!({"readable": false, "reason": "node not found"}).to_string(),
    )
    .unwrap();
    start(
        &home,
        &tools,
        &[
            ("HOSTLER_TEST_FAKE_SESSIONS", sessions.to_str().unwrap()),
            ("HOSTLER_TEST_DEVICES_STALE_MS", "1"),
        ],
    );
    until(&home, "the phone and the AVD", |s| {
        has(s, "device:android:SERIAL0001") && has(s, "device:android:avd:Pixel_9")
    });
    add_avd(&home);
    let phone = run_tty(&home, &["devices", "add", PHONE], &["Pixel 9"]);
    assert!(phone.status.success(), "{}", stderr(&phone));
    until(&home, "closed devices", |s| {
        s.devices
            .iter()
            .filter(|d| d.registered && d.checking_sessions)
            .count()
            >= 2
    });

    let out = run(&home, &["devices", "forget-sessions", "pixel_9"]);
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("needs a person at a terminal"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("To fix: hostler devices forget-sessions pixel_9"),
        "{}",
        stderr(&out)
    );
    let json = envelope(&run(
        &home,
        &["devices", "forget-sessions", "pixel_9", "--json"],
    ));
    assert_eq!(
        json["error"]["recovery"], "hostler devices forget-sessions pixel_9",
        "{json}"
    );

    let out = run_tty(
        &home,
        &["devices", "forget-sessions", "pixel_9"],
        &["not-the-alias"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("the alias did not match"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("To fix: hostler devices forget-sessions pixel_9"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("closed until agent-device can be read (node not found)"),
        "{}",
        stderr(&out)
    );

    // The prompt asks for the resolved alias, not the transport the person typed.
    let repo = home.make_repo("app");
    let workspace = home.workspace(&repo).unwrap();
    let out = run_tty_in(
        &home,
        &repo,
        &["devices", "forget-sessions", EMULATOR],
        &["pixel_9"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("stops nothing: if a session still runs on pixel_9"),
        "{}",
        stdout(&out)
    );
    let record = envelope(&run(&home, &["devices", "show", "pixel_9", "--json"]));
    let forget = &record["data"]["forgets"][0];
    assert_eq!(forget["resource"], "device:android:avd:Pixel_9");
    assert_eq!(forget["sessions"], json!([]));
    assert!(forget["by_pid"].is_number(), "{record}");
    assert_eq!(forget["by_workspace_path"], workspace.path);

    let listed = run(&home, &["devices", "list"]);
    let text = stdout(&listed);
    let line = |name: &str| {
        text.lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} line: {text}"))
            .to_owned()
    };
    assert!(
        !line("pixel_9").contains("closed until agent-device can be read"),
        "the forgotten device stays closed: {text}"
    );
    assert!(
        line("pixel-9").contains("closed until agent-device can be read"),
        "an unfogotten device opened: {text}"
    );
    let shown = run(&home, &["devices", "show", "pixel_9"]);
    assert!(
        stdout(&shown).contains("forget-sessions"),
        "{}",
        stdout(&shown)
    );
}

/// A device whose sessions read fine is refused before the person is prompted.
#[test]
fn forget_sessions_refuses_before_prompting_when_not_closed() {
    let mut tools = FakeTools::new();
    let home = Home::new();
    tools.adb_server(41);
    tools.script(android());
    let sessions = home.dir.path().join("sessions.json");
    std::fs::write(&sessions, r#"{"sessions": []}"#).unwrap();
    start(
        &home,
        &tools,
        &[("HOSTLER_TEST_FAKE_SESSIONS", sessions.to_str().unwrap())],
    );
    until(&home, "the AVD", |s| has(s, "device:android:avd:Pixel_9"));
    add_avd(&home);

    let out = run_tty(
        &home,
        &["devices", "forget-sessions", "pixel_9"],
        &["pixel_9"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("nothing to forget: agent-device status reads fine"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("Type the alias"),
        "a prompt nobody should answer: {}",
        stderr(&out)
    );
}

/// A global npm install: `<root>/<name>/bin/{agent-device,node}` and the package's bin script.
fn npm_layout(root: &Path, name: &str) -> PathBuf {
    npm_layout_at(&root.join(name)).0
}

/// The npm global layout under `base`: `bin/{node,agent-device}` and the package's bin script.
fn npm_layout_at(base: &Path) -> (PathBuf, Option<PathBuf>) {
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("node"), b"").unwrap();
    std::fs::write(bin.join("agent-device"), b"#!/bin/sh\n").unwrap();
    let package = base.join("lib/node_modules/agent-device");
    write_package(&package);
    (bin, Some(package.join("bin/agent-device.mjs")))
}

fn write_package(package: &Path) {
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"bin":"bin/agent-device.mjs"}"#,
    )
    .unwrap();
    std::fs::write(package.join("bin/agent-device.mjs"), "// fake\n").unwrap();
}
