use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use crate::claims::ResourceCatalog;
use crate::claims::engine::ClaimsEngine;
use crate::contract::ErrorCode;
use crate::contract::claims::{
    ClaimOutcome, ClaimRelease, ClaimRequest, Freshness, Purpose, Selector,
};
use crate::devices::android::{AdbLine, Identity};
use crate::devices::catalog::DeviceCatalog;
use crate::devices::ios::{PhysicalIos, Simulator};
use crate::devices::service::{DevicesService, SessionRead};
use crate::devices::tools::ToolPaths;
use crate::heavy::engine::Clock;
use crate::procs::ProcessIdentity;
use crate::procs::fake::FakeTable;
use crate::store::Store;

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/devices")
            .join(name),
    )
    .unwrap()
}

fn service_on(store: Store) -> Arc<DevicesService> {
    let (tx, _) = crate::service::events::EventBus::channel(256);
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    DevicesService::with_clock(store, ToolPaths::default(), tx, clock).0
}

fn service() -> Arc<DevicesService> {
    service_on(Store::open_in_memory().unwrap())
}

fn adb(serial: &str, usb: bool) -> AdbLine {
    AdbLine {
        serial: serial.into(),
        state: "device".into(),
        usb: usb.then(|| "1-1".into()),
        model: None,
        product: None,
        device: None,
        transport_id: Some(1),
    }
}

fn phone(serialno: &str) -> Identity {
    Identity {
        serialno: serialno.into(),
        model: "Pixel 9".into(),
        release: "15".into(),
        sdk: Some(35),
        avd_name: None,
        qemu: false,
    }
}

fn claim(device_id: &str, platform: Platform, session: &str) -> AdClaim {
    AdClaim {
        device_id: device_id.into(),
        platform: Some(platform),
        platform_text: match platform {
            Platform::Android => "android".into(),
            Platform::Ios => "ios".into(),
        },
        kind: "device".into(),
        name: "Test device".into(),
        session: session.into(),
        workspace: "/work/app".into(),
        state_dir: "/work/app/.agent-device".into(),
        pid: 4242,
        start_time_text: "Sun Nov  8 03:37:51 2026".into(),
    }
}

fn no_canonical(raw: &str) -> Option<String> {
    Some(raw.to_owned())
}

/// A good read of `claims` (seq: the counter now): the sessions as the service maps them.
fn read_claims(svc: &DevicesService, claims: &[AdClaim]) -> Vec<ForeignClaim> {
    let seq = svc.cache().ends();
    svc.apply_session_read(
        SessionRead::Good {
            raw: to_raw(claims, None, &no_canonical),
            in_use: true,
        },
        seq,
        svc.now(),
    );
    svc.cache().sessions()
}

fn unmapped(svc: &DevicesService) -> Vec<String> {
    svc.cache().snapshot().unmapped_sessions
}

#[test]
fn status_live_only_any_version() {
    for name in [
        "agent-device-status-0.21.36.json",
        "agent-device-status-iphone.json",
    ] {
        let claims = parse_status(&fixture(name)).unwrap();
        assert_eq!(claims.len(), 2, "{name}");
        assert!(claims.iter().all(|c| !c.session.is_empty() && c.pid != 0));
    }
    let claims = parse_status(&fixture("agent-device-status-0.21.36.json")).unwrap();
    assert_eq!(claims[0].platform, Some(Platform::Ios));
    assert_eq!(claims[0].kind, "simulator");
    assert_eq!(claims[1].platform, Some(Platform::Android));
    assert_eq!(claims[1].session, "p3b-envtest");
    assert_eq!(claims[1].state_dir, "/Users/dev/work/app");
    assert_eq!(claims[1].start_time_text, "Mon Jan  5 11:00:00 2026");
    assert_eq!(claims[1].pid, 22222);
    // Only live claims; incomplete claims are left out; another platform is kept (a problem,
    // P3bT9b-6), and `appleOs: "ios"` is iOS whatever `platform` says.
    let mixed = br#"{"success": true, "data": {"claims": [
        {"classification": "stale", "device": {"id": "A", "platform": "android"}, "owner": {"session": "s"}},
        {"classification": "live", "device": {"id": "B", "platform": "macos"}, "owner": {"session": "s"}},
        {"classification": "live", "device": {"id": "", "platform": "android"}, "owner": {"session": "s"}},
        {"classification": "live", "device": {"id": "C", "platform": "android"}, "owner": {"session": "s"}},
        {"classification": "live", "device": {"id": "D", "platform": "apple", "appleOs": "ios"}, "owner": {"session": "s"}}
    ]}}"#;
    let claims = parse_status(mixed).unwrap();
    assert_eq!(claims.len(), 3);
    assert_eq!(
        (
            claims[0].device_id.as_str(),
            claims[0].platform,
            claims[0].platform_text.as_str()
        ),
        ("B", None, "macos")
    );
    assert_eq!((claims[1].device_id.as_str(), claims[1].pid), ("C", 0));
    assert_eq!(claims[1].platform, Some(Platform::Android));
    assert_eq!(claims[2].platform, Some(Platform::Ios));
    // No claims at all is a good, empty read; anything else is unreadable.
    assert_eq!(
        parse_status(br#"{"success": true, "data": {"claims": []}}"#),
        Some(Vec::new())
    );
    for bad in [
        &br#"{"success": false, "data": {"claims": []}}"#[..],
        b"not json",
        b"{}",
        br#"{"data": {"claims": {}}}"#,
    ] {
        assert_eq!(parse_status(bad), None, "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn owner_start_time_parses_host_local_ctime() {
    assert_eq!(
        parse_ctime("Thu Nov  5 23:29:41 2026"),
        Some(Civil {
            year: 2026,
            month: 11,
            day: 5,
            hour: 23,
            minute: 29,
            second: 41
        })
    );
    assert_eq!(
        parse_ctime("Sun Nov 08 03:37:51 2026").map(|c| c.day),
        Some(8)
    );
    for bad in [
        "",
        "2026-11-05T23:29:41Z",
        "Thu Nox  5 23:29:41 2026",
        "Thu Nov 32 23:29:41 2026",
        "Thu Nov  5 25:29:41 2026",
    ] {
        assert_eq!(parse_ctime(bad), None, "{bad}");
    }
    // On this host the time has a reading (the zone's), and it round-trips.
    if cfg!(unix) {
        let c = parse_ctime("Thu Jul  2 12:00:00 2026").unwrap();
        assert!(!local_readings(&c).is_empty());
    }
}

/// P-10: by the current, a recent, or a stored adb serial.
#[test]
fn android_claim_maps_by_current_recent_and_stored_serial() {
    let svc = service();
    svc.test_android(&[
        (adb("SERIAL0001", true), phone("SERIAL0001")),
        (adb("192.0.2.10:5555", false), phone("SERIAL0001")),
    ]);
    svc.add("SERIAL0001", None, true).unwrap();
    // The phone drops off Wi-Fi: that serial is now only a stored one.
    svc.test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    let claims = [
        claim("SERIAL0001", Platform::Android, "usb"),
        claim("192.0.2.10:5555", Platform::Android, "stored"),
    ];
    let foreign = read_claims(&svc, &claims);
    assert!(unmapped(&svc).is_empty(), "{:?}", unmapped(&svc));
    assert_eq!(foreign.len(), 2);
    assert!(
        foreign
            .iter()
            .all(|f| f.resource.0 == "device:android:SERIAL0001")
    );
    let stop = foreign[0].stop.as_ref().unwrap();
    assert_eq!(stop["session"], "usb");
    assert_eq!(stop["platform"], "android");
    assert_eq!(stop["id"], "SERIAL0001");
    assert_eq!(stop["state_dir"], "/work/app/.agent-device");
    assert_eq!(foreign[0].source, "agent-device");
    assert_eq!(foreign[0].workspace_path.as_deref(), Some("/work/app"));
    assert_eq!(
        foreign[0].workspace_id, None,
        "the engine resolves the owner"
    );
}

fn iphone(udid: &str) -> PhysicalIos {
    PhysicalIos {
        udid: udid.into(),
        coredevice_id: "00000000-0000-4000-8000-0000000000CD".into(),
        name: "Test iPhone 1".into(),
        model: "iPhone 12 Pro".into(),
        os_version: Some("26.6".into()),
        transport: crate::contract::devices::TransportKind::Wired,
        reachable: true,
        paired: true,
    }
}

/// The round-1 iPhone fixture: a simulator claim and a physical-iPhone claim, by UDID, or by
/// CoreDevice id.
#[test]
fn ios_claim_maps_by_udid_or_coredevice_id() {
    let svc = service();
    svc.test_ios(
        vec![iphone("00008101-000A00000000001E")],
        vec![Simulator {
            udid: "00000000-0000-4000-8000-00000000000A".into(),
            name: "iPhone 17 Pro".into(),
            runtime: "iOS 26.5".into(),
            booted: true,
            last_booted_at: None,
        }],
    );
    svc.add("00008101-000A00000000001E", None, true).unwrap();
    svc.add("00000000-0000-4000-8000-00000000000A", None, false)
        .unwrap();
    let claims = parse_status(&fixture("agent-device-status-iphone.json")).unwrap();
    let foreign = read_claims(&svc, &claims);
    assert!(unmapped(&svc).is_empty(), "{:?}", unmapped(&svc));
    let resources: Vec<&str> = foreign.iter().map(|f| f.resource.0.as_str()).collect();
    assert_eq!(
        resources,
        [
            "device:ios:00000000-0000-4000-8000-00000000000A",
            "device:ios:00008101-000A00000000001E"
        ]
    );
    let by_coredevice = [claim(
        "00000000-0000-4000-8000-0000000000CD",
        Platform::Ios,
        "cd",
    )];
    let foreign = read_claims(&svc, &by_coredevice);
    assert_eq!(
        foreign[0].resource.0,
        "device:ios:00008101-000A00000000001E"
    );
    assert_eq!(foreign[0].stop.as_ref().unwrap()["platform"], "ios");
}

/// P-10: a live session that maps to no device is a problem; one on an unregistered device is
/// left out quietly.
#[test]
fn unmapped_claim_is_a_problem() {
    let svc = service();
    svc.test_android(&[(adb("SERIAL0002", true), phone("SERIAL0002"))]);
    let claims = [
        claim("SERIAL0002", Platform::Android, "unregistered"),
        claim("SERIAL9999", Platform::Android, "s\u{1b}[2J"),
    ];
    let foreign = read_claims(&svc, &claims);
    assert!(foreign.is_empty());
    let unmapped = unmapped(&svc);
    assert_eq!(unmapped.len(), 1);
    assert!(unmapped[0].contains("SERIAL9999"), "{}", unmapped[0]);
    assert!(!unmapped[0].contains('\u{1b}'), "sanitized");
}

#[test]
fn owner_workspace_is_canonicalized() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let expected = crate::project::display_path(&std::fs::canonicalize(&real).unwrap());
    assert_eq!(
        canonical_workspace(real.to_str().unwrap(), CANONICAL_BOUND).as_deref(),
        Some(expected.as_str())
    );
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            canonical_workspace(link.to_str().unwrap(), CANONICAL_BOUND).as_deref(),
            Some(expected.as_str())
        );
    }
    #[cfg(target_os = "macos")]
    if std::fs::canonicalize("/tmp").is_ok_and(|p| p == Path::new("/private/tmp")) {
        assert_eq!(
            canonical_workspace("/tmp", CANONICAL_BOUND).as_deref(),
            Some("/private/tmp")
        );
    }
}

/// P3-9: bounded, cached per raw path; an error, a relative path or a timeout is `None` (the
/// session then blocks every workspace).
#[test]
fn canonical_workspace_is_bounded_and_cached() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone");
    std::fs::create_dir(&gone).unwrap();
    let raw = gone.to_str().unwrap().to_owned();
    let first = canonical_workspace(&raw, CANONICAL_BOUND);
    assert!(first.is_some());
    std::fs::remove_dir(&gone).unwrap();
    assert_eq!(canonical_workspace(&raw, CANONICAL_BOUND), first, "cached");
    assert_eq!(
        canonical_workspace(dir.path().join("never").to_str().unwrap(), CANONICAL_BOUND),
        None
    );
    assert_eq!(canonical_workspace("relative/path", CANONICAL_BOUND), None);
    assert_eq!(canonical_workspace("", CANONICAL_BOUND), None);
    let start = Instant::now();
    let _ = canonical_workspace(
        dir.path().join("fresh").to_str().unwrap(),
        Duration::from_millis(1),
    );
    assert!(start.elapsed() < Duration::from_millis(500));
}

/// P3-2: the sessions and their freshness come out of one guard, as one read put them in.
#[test]
fn collect_answers_sessions_and_since_under_one_guard() {
    let svc = service();
    svc.test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    svc.add("SERIAL0001", None, true).unwrap();
    let source = AgentDeviceSource(svc.cache().clone());
    let seq = svc.cache().ends();
    svc.apply_session_read(
        SessionRead::Good {
            raw: to_raw(
                &[claim("SERIAL0001", Platform::Android, "s1")],
                None,
                &no_canonical,
            ),
            in_use: true,
        },
        seq,
        svc.now(),
    );
    let view = source.collect();
    assert_eq!(view.freshness, Freshness::Since(seq));
    assert_eq!(view.claims.len(), 1);
    assert_eq!(view.claims, source.foreign());
}

/// P3-3: the counter starts at 1 and every end answers the value after its increment; the
/// device shows `checking` at once (P3bT5-7).
#[test]
fn claim_ended_increments_and_wakes() {
    let svc = service();
    svc.test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    svc.add("SERIAL0001", None, true).unwrap();
    svc.apply_session_read(
        SessionRead::Good {
            raw: Vec::new(),
            in_use: false,
        },
        1,
        svc.now(),
    );
    let source = AgentDeviceSource(svc.cache().clone());
    let r = ResourceId("device:android:SERIAL0001".into());
    assert!(!svc.resolve("pixel-9").unwrap().checking_sessions);
    assert_eq!(source.claim_ended(&r), 2);
    assert!(svc.resolve("pixel-9").unwrap().checking_sessions, "at once");
    assert_eq!(source.claim_ended(&r), 3);
    assert_eq!(svc.cache().ends(), 3);
}

// ---- L53 ----

const RECORDED: &str = "Sun Nov  8 03:37:51 2026";
/// The recorded time's reading in these tests: 1_794_000_000 s.
const AT: i64 = 1_794_000_000;

fn one_reading(_: &Civil) -> Vec<i64> {
    vec![AT]
}

fn table_with(pid: u32, start_s: i64, os: Os) -> FakeTable {
    let table = FakeTable::new("b1");
    let start = match os {
        Os::MacOs => start_s as u64 * 1_000_000,
        Os::Linux => (start_s - 1_700_000_000) as u64 * 100,
        Os::Windows => (start_s + 11_644_473_600) as u64 * 10_000_000,
    };
    table.insert(pid, None, Some(start));
    table
}

fn check(table: &FakeTable, cmd: Option<&str>, os: Os) -> OwnerCheck {
    let cmd = cmd.map(str::to_owned);
    owner_check_with(
        table,
        &move |_| cmd.clone(),
        &one_reading,
        Some((1_700_000_000, 100)),
        42,
        RECORDED,
        os,
    )
}

#[test]
fn owner_check_no_process_is_dead() {
    let table = FakeTable::new("b1");
    assert_eq!(check(&table, None, Os::MacOs), OwnerCheck::CertainlyDead);
}

#[test]
fn owner_check_newer_other_program_is_dead() {
    for os in [Os::MacOs, Os::Linux] {
        let table = table_with(42, AT + 60, os);
        assert_eq!(
            check(&table, Some("/usr/bin/vim notes.txt"), os),
            OwnerCheck::CertainlyDead,
            "{os:?}"
        );
    }
    // Windows: an image that is not `node` and started later.
    let table = table_with(42, AT + 60, Os::Windows);
    table.set_name(42, "notepad.exe");
    assert_eq!(check(&table, None, Os::Windows), OwnerCheck::CertainlyDead);
    table.set_name(42, "node.exe");
    assert_eq!(
        check(&table, None, Os::Windows),
        OwnerCheck::Alive,
        "node cannot tell"
    );
}

#[test]
fn owner_check_unreadable_or_unparseable_keeps_the_session() {
    let table = table_with(42, AT + 60, Os::MacOs);
    // No command line (another user's process, a race).
    assert_eq!(check(&table, None, Os::MacOs), OwnerCheck::Alive);
    // agent-device's own daemon.
    assert_eq!(
        check(
            &table,
            Some("node /usr/local/lib/node_modules/agent-device/dist/daemon.js"),
            Os::MacOs
        ),
        OwnerCheck::Alive
    );
    // An unparseable recorded time, or no reading of it.
    let cmd = Some("/usr/bin/vim".to_owned());
    for (text, readings) in [
        (
            "2026-11-08T03:37:51Z",
            one_reading as fn(&Civil) -> Vec<i64>,
        ),
        (RECORDED, |_: &Civil| Vec::new()),
    ] {
        let cmd = cmd.clone();
        assert_eq!(
            owner_check_with(
                &table,
                &move |_| cmd.clone(),
                &readings,
                None,
                42,
                text,
                Os::MacOs
            ),
            OwnerCheck::Alive,
            "{text}"
        );
    }
    // An unreadable start time is not dead here (unlike P3a's `StartTimeUnreadable`).
    let table = FakeTable::new("b1");
    table.insert(42, None, None);
    assert_eq!(
        check(&table, Some("/usr/bin/vim"), Os::MacOs),
        OwnerCheck::Alive
    );
    // Linux without `btime`: no start in seconds.
    let table = table_with(42, AT + 60, Os::Linux);
    let cmd = Some("/usr/bin/vim".to_owned());
    assert_eq!(
        owner_check_with(
            &table,
            &move |_| cmd.clone(),
            &one_reading,
            None,
            42,
            RECORDED,
            Os::Linux
        ),
        OwnerCheck::Alive
    );
}

/// `btime` jitters by a second or so: within 5 s is never "newer".
#[test]
fn owner_check_tolerates_btime_jitter() {
    for skew in [-2, 0, 1, 5] {
        let table = table_with(42, AT + skew, Os::Linux);
        assert_eq!(
            check(&table, Some("/usr/bin/vim"), Os::Linux),
            OwnerCheck::Alive,
            "{skew}"
        );
    }
    let table = table_with(42, AT + 6, Os::Linux);
    assert_eq!(
        check(&table, Some("/usr/bin/vim"), Os::Linux),
        OwnerCheck::CertainlyDead
    );
}

/// P3-7: Linux's `btime` moves with a wall-clock step, so a live daemon can look hours newer;
/// its command line keeps it.
#[test]
fn owner_check_linux_clock_step_keeps_a_live_daemon() {
    let table = table_with(42, AT + 3 * 3600, Os::Linux);
    assert_eq!(
        check(
            &table,
            Some(
                "/usr/bin/node /home/dev/.npm-global/lib/node_modules/agent-device/bin/agent-device.mjs daemon"
            ),
            Os::Linux
        ),
        OwnerCheck::Alive
    );
}

/// CRA-1: a pid whose process cannot be read (another user's or an elevated daemon) is not
/// gone; dead only when the OS says no process has it. A zombie is dead.
#[test]
fn owner_check_unreadable_owner_is_alive_and_a_zombie_dead() {
    let table = FakeTable::new("b1");
    table.set_exists(42, Some(true));
    assert_eq!(check(&table, None, Os::MacOs), OwnerCheck::Alive, "exists");
    table.set_exists(42, None);
    assert_eq!(
        check(&table, None, Os::Windows),
        OwnerCheck::Alive,
        "cannot tell"
    );
    table.set_exists(42, Some(false));
    assert_eq!(check(&table, None, Os::Linux), OwnerCheck::CertainlyDead);
    let table = table_with(42, AT, Os::MacOs);
    table.set_zombie(42, true);
    assert_eq!(
        check(&table, Some("node agent-device daemon"), Os::MacOs),
        OwnerCheck::CertainlyDead
    );
}

/// CRA-1 through `dead_owners`: while the status is unreadable, a kept session whose owner
/// exists but cannot be read keeps blocking another workspace; once the pid is gone it goes.
#[tokio::test]
async fn kept_session_of_an_unreadable_owner_keeps_blocking() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    read_claims(&b.svc, &[claim("SERIAL0001", Platform::Android, "s1")]);
    let owners = FakeTable::new("b1");
    owners.set_exists(4242, Some(true));
    let failed = |dead| SessionRead::Failed {
        reason: "node not found".into(),
        dead,
    };
    b.read(failed(dead_owners_in(&owners, &b.svc.raw_sessions())));
    assert_eq!(b.svc.sessions_on(&ResourceId(P1.into())), ["s1"]);
    let other = b.workspace("b");
    let w = b.hold(&other, 200, P1);
    assert!(blocked(&b.acquire(&w).await));
    owners.set_exists(4242, Some(false));
    let dead = dead_owners_in(&owners, &b.svc.raw_sessions());
    assert_eq!(dead, ["android|SERIAL0001|s1"]);
    b.read(failed(dead));
    assert!(b.svc.sessions_on(&ResourceId(P1.into())).is_empty());
}

/// The DST fall-back hour has two readings: dead only when newer than both.
#[test]
fn owner_check_dst_ambiguous_keeps() {
    let two = |_: &Civil| vec![AT, AT + 3600];
    let cmd = Some("/usr/bin/vim".to_owned());
    for (start, expected) in [
        (AT + 60, OwnerCheck::Alive),
        (AT + 3600 + 6, OwnerCheck::CertainlyDead),
    ] {
        let table = table_with(42, start, Os::MacOs);
        let cmd = cmd.clone();
        assert_eq!(
            owner_check_with(
                &table,
                &move |_| cmd.clone(),
                &two,
                None,
                42,
                RECORDED,
                Os::MacOs
            ),
            expected
        );
    }
}

#[test]
fn versions_parse_and_compare() {
    assert_eq!(parse_version("0.21.36"), Some((0, 21, 36)));
    assert_eq!(parse_version("agent-device v0.21.0\n"), Some((0, 21, 0)));
    assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
    assert_eq!(parse_version("nope"), None);
    assert!(parse_version("0.21.0").unwrap() < TARGET_VERSION);
}

// ---- L54: forget-sessions ----

/// An engine and a device service on one store, the engine reading the service's catalog and
/// sessions as the real service wires them.
struct Both {
    svc: Arc<DevicesService>,
    engine: tokio::sync::Mutex<ClaimsEngine>,
    table: Arc<FakeTable>,
    /// The engine's clock.
    clock: Arc<TestClock>,
    store: Store,
    _dir: tempfile::TempDir,
}

fn both() -> Both {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hostler.db");
    let svc = service_on(Store::open(&path).unwrap());
    let table = Arc::new(FakeTable::new("b1"));
    let (tx, _) = crate::service::events::EventBus::channel(256);
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    let engine = ClaimsEngine::new(
        Store::open(&path).unwrap(),
        table.clone(),
        clock.clone(),
        vec![Box::new(DeviceCatalog(svc.cache().clone()))],
        vec![Box::new(AgentDeviceSource(svc.cache().clone()))],
        tx,
    );
    Both {
        svc,
        engine: tokio::sync::Mutex::new(engine),
        table,
        clock,
        store: Store::open(&path).unwrap(),
        _dir: dir,
    }
}

impl Both {
    fn workspace(&self, name: &str) -> String {
        let project = self
            .store
            .upsert_project(&format!("/work/{name}/.git"), name, 1)
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        self.store
            .upsert_workspace(
                &id,
                &project.id,
                &format!("/work/{name}"),
                &format!("/work/{name}/.git"),
                1,
            )
            .unwrap();
        id
    }

    fn me(&self, pid: u32) -> ProcessIdentity {
        self.table.insert(pid, None, Some(u64::from(pid)));
        ProcessIdentity {
            pid,
            boot_id: "b1".into(),
            start_time: u64::from(pid),
        }
    }

    fn hold(&self, ws: &str, pid: u32, r: &str) -> ClaimRequest {
        ClaimRequest {
            id: uuid::Uuid::new_v4().to_string(),
            selector: Selector::Exact {
                resource: ResourceId(r.into()),
            },
            workspace_id: ws.into(),
            requester: self.me(pid),
            purpose: Purpose::Hold {
                ttl_ms: Some(60_000),
                bound_to: None,
                distinct: false,
            },
            reentrant_tokens: Vec::new(),
            no_wait: false,
        }
    }

    async fn acquire(&self, r: &ClaimRequest) -> ClaimOutcome {
        self.engine
            .lock()
            .await
            .acquire(r.clone(), &crate::claims::engine::no_heavy())
            .unwrap()
    }

    async fn state_of(&self, r: &ClaimRequest) -> ClaimOutcome {
        self.engine.lock().await.attach(&r.id).unwrap()
    }

    fn read(&self, read: SessionRead) {
        let seq = self.svc.cache().ends();
        self.svc.apply_session_read(read, seq, self.svc.now());
    }

    fn good(&self) {
        self.read(SessionRead::Good {
            raw: Vec::new(),
            in_use: true,
        });
    }

    async fn tick(&self) {
        self.engine
            .lock()
            .await
            .tick(&crate::claims::engine::no_heavy());
    }

    fn unreadable(&self) {
        self.read(SessionRead::Failed {
            reason: "node not found".into(),
            dead: Vec::new(),
        });
    }
}

const P1: &str = "device:android:SERIAL0001";
const P2: &str = "device:android:SERIAL0002";

/// L54: a person releases one device from the wait while the status is unreadable; another
/// closed device stays closed; nothing is stopped (no tool runs here at all).
#[tokio::test]
async fn forget_sessions_releases_one_device_and_stops_nothing() {
    let b = both();
    b.svc.test_android(&[
        (adb("SERIAL0001", true), phone("SERIAL0001")),
        (adb("SERIAL0002", true), phone("SERIAL0002")),
    ]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    b.svc.add("SERIAL0002", Some("two"), true).unwrap();
    assert_eq!(DeviceCatalog(b.svc.cache().clone()).resources().len(), 2);
    b.good();
    let (a, other) = (b.workspace("a"), b.workspace("b"));
    // A holds both and releases them: each end closes its device to B until a good read.
    for (pid, r) in [(100, P1), (101, P2)] {
        let hold = b.hold(&a, pid, r);
        let ClaimOutcome::Granted { claim } = b.acquire(&hold).await else {
            panic!("A's hold");
        };
        b.engine
            .lock()
            .await
            .release(ClaimRelease {
                claim_id: claim.id,
                workspace_id: a.clone(),
                requester: hold.requester.clone(),
                exit_code: None,
            })
            .unwrap();
    }
    b.unreadable();
    let w1 = b.hold(&other, 200, P1);
    let w2 = b.hold(&other, 201, P2);
    for w in [&w1, &w2] {
        assert!(matches!(b.acquire(w).await, ClaimOutcome::Queued { .. }));
    }

    // Without the person: refused, nothing changes.
    let e = forget_sessions(&b.svc, &b.engine, "one", false, None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    let record = forget_sessions(&b.svc, &b.engine, "one", true, None, None)
        .await
        .unwrap();
    assert_eq!(record.resource.0, P1);
    assert!(record.reason.contains("node not found"));
    assert!(matches!(
        b.state_of(&w1).await,
        ClaimOutcome::Granted { .. }
    ));
    assert!(matches!(b.state_of(&w2).await, ClaimOutcome::Queued { .. }));
    // Recorded.
    assert_eq!(b.store.session_forgets(&record.resource).unwrap(), [record]);
    // Again on the same device: not closed any more.
    let e = forget_sessions(&b.svc, &b.engine, "one", true, None, None)
        .await
        .unwrap_err();
    assert!(e.message.contains("is not closed"), "{}", e.message);
    // With a good read there is nothing to forget.
    b.good();
    let e = forget_sessions(&b.svc, &b.engine, "two", true, None, None)
        .await
        .unwrap_err();
    assert_eq!(
        e.message,
        "nothing to forget: agent-device status reads fine"
    );
}

/// L51, L54: a kept session on a device is dropped by `forget-sessions` too.
#[tokio::test]
async fn forget_sessions_drops_the_kept_sessions_of_that_device() {
    let b = both();
    b.svc.test_android(&[
        (adb("SERIAL0001", true), phone("SERIAL0001")),
        (adb("SERIAL0002", true), phone("SERIAL0002")),
    ]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    b.svc.add("SERIAL0002", Some("two"), true).unwrap();
    b.read(SessionRead::Good {
        raw: to_raw(
            &[
                claim("SERIAL0001", Platform::Android, "s1"),
                claim("SERIAL0002", Platform::Android, "s2"),
            ],
            None,
            &|_| None,
        ),
        in_use: true,
    });
    b.unreadable();
    let ws = b.workspace("a");
    let w1 = b.hold(&ws, 100, P1);
    assert!(matches!(b.acquire(&w1).await, ClaimOutcome::Queued { .. }));
    let record = forget_sessions(&b.svc, &b.engine, "one", true, None, None)
        .await
        .unwrap();
    assert_eq!(record.sessions, ["s1"]);
    b.engine
        .lock()
        .await
        .tick(&crate::claims::engine::no_heavy());
    assert!(matches!(
        b.state_of(&w1).await,
        ClaimOutcome::Granted { .. }
    ));
    // The other device keeps its kept session.
    assert_eq!(b.svc.sessions_on(&ResourceId(P2.into())), ["s2"]);
}

// ---- P3bT9b-1: sessions are mapped in every rebuild ----

fn emulator(avd: &str) -> Identity {
    Identity {
        serialno: "EMULATOR35X1X1X0".into(),
        model: "sdk_gphone64_arm64".into(),
        release: "15".into(),
        sdk: Some(35),
        avd_name: Some(avd.into()),
        qemu: true,
    }
}

fn blocked(o: &ClaimOutcome) -> bool {
    matches!(o, ClaimOutcome::Queued { blockers, .. } if !blockers.is_empty())
}

/// A session seen on an unregistered device blocks it the moment it is registered: `add`
/// neither ends a claim nor wakes the sessions lane, so only the re-map can see it.
#[tokio::test]
async fn session_on_a_device_registered_after_the_read_blocks_at_once() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    let foreign = read_claims(&b.svc, &[claim("SERIAL0001", Platform::Android, "s1")]);
    assert!(foreign.is_empty(), "unregistered: left out");
    assert!(unmapped(&b.svc).is_empty(), "known: no problem");
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    assert_eq!(b.svc.sessions_on(&ResourceId(P1.into())), ["s1"], "at once");
    let other = b.workspace("b");
    assert!(blocked(&b.acquire(&b.hold(&other, 200, P1)).await));
}

/// A session on a transport discovery has not seen yet (Wi-Fi) blocks the phone as soon as
/// discovery sees that transport, with no new read.
#[tokio::test]
async fn session_on_a_transport_discovered_after_the_read_blocks_at_once() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    let foreign = read_claims(
        &b.svc,
        &[claim("192.0.2.10:5555", Platform::Android, "wifi")],
    );
    assert!(foreign.is_empty());
    assert_eq!(unmapped(&b.svc).len(), 1, "never silent");
    b.svc.test_android(&[
        (adb("SERIAL0001", true), phone("SERIAL0001")),
        (adb("192.0.2.10:5555", false), phone("SERIAL0001")),
    ]);
    assert_eq!(b.svc.sessions_on(&ResourceId(P1.into())), ["wifi"]);
    assert!(unmapped(&b.svc).is_empty());
    let other = b.workspace("b");
    assert!(blocked(&b.acquire(&b.hold(&other, 200, P1)).await));
}

/// The first read after a restart comes before discovery: an `emulator-5554` session maps to
/// every AVD that ever ran there, never to the first; discovery then adds the one running now,
/// and the next good read keeps that one only.
#[test]
fn startup_read_before_discovery_never_attributes_an_emulator_session_to_another_avd() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hostler.db");
    let (pixel_9, pixel_8) = (
        super::super::service::avd_resource("Pixel_9"),
        super::super::service::avd_resource("Pixel_8"),
    );
    {
        let svc = service_on(Store::open(&path).unwrap());
        for avd in ["Pixel_9", "Pixel_8"] {
            svc.test_android(&[(adb("emulator-5554", false), emulator(avd))]);
            svc.add("emulator-5554", None, false).unwrap();
        }
        assert!(svc.cache().is_registered(&pixel_9) && svc.cache().is_registered(&pixel_8));
    }
    let svc = service_on(Store::open(&path).unwrap());
    let on = |svc: &DevicesService| {
        let mut on: Vec<String> = svc
            .cache()
            .sessions()
            .iter()
            .map(|f| f.resource.0.clone())
            .collect();
        on.sort();
        on
    };
    read_claims(&svc, &[claim("emulator-5554", Platform::Android, "x")]);
    assert_eq!(on(&svc), [pixel_8.0.clone(), pixel_9.0.clone()], "both");
    // Discovery: Pixel_8 runs on 5554. Still both until the next good read (fail closed).
    svc.test_android(&[(adb("emulator-5554", false), emulator("Pixel_8"))]);
    assert_eq!(on(&svc), [pixel_8.0.clone(), pixel_9.0.clone()]);
    read_claims(&svc, &[claim("emulator-5554", Platform::Android, "x")]);
    assert_eq!(on(&svc), std::slice::from_ref(&pixel_8.0));
}

/// A session on a serial no device has yet is a problem; once a registered device has that
/// serial, it blocks the device and the problem goes.
#[tokio::test]
async fn unmapped_session_blocks_once_its_serial_maps() {
    let b = both();
    read_claims(&b.svc, &[claim("SERIAL0003", Platform::Android, "s3")]);
    assert_eq!(unmapped(&b.svc).len(), 1);
    b.svc
        .test_android(&[(adb("SERIAL0003", true), phone("SERIAL0003"))]);
    b.svc.add("SERIAL0003", Some("three"), true).unwrap();
    let r = "device:android:SERIAL0003";
    assert_eq!(b.svc.sessions_on(&ResourceId(r.into())), ["s3"]);
    assert!(unmapped(&b.svc).is_empty());
    let other = b.workspace("b");
    assert!(blocked(&b.acquire(&b.hold(&other, 200, r)).await));
}

// ---- P3bT9b-2, -3: the fix text and the takeover refusal while unreadable ----

/// The `Checking` blocker names the fix and `forget-sessions` from the first failed read.
#[tokio::test]
async fn unreadable_status_names_the_fix_in_the_checking_text() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    b.good();
    let (a, other) = (b.workspace("a"), b.workspace("b"));
    let hold = b.hold(&a, 100, P1);
    let ClaimOutcome::Granted { claim } = b.acquire(&hold).await else {
        panic!("A's hold");
    };
    b.engine
        .lock()
        .await
        .release(ClaimRelease {
            claim_id: claim.id,
            workspace_id: a.clone(),
            requester: hold.requester.clone(),
            exit_code: None,
        })
        .unwrap();
    b.unreadable();
    let ClaimOutcome::Queued { blockers, .. } = b.acquire(&b.hold(&other, 200, P1)).await else {
        panic!("B waits");
    };
    let [crate::contract::claims::Blocker::Checking { text, .. }] = &blockers[..] else {
        panic!("{blockers:?}");
    };
    assert_eq!(
        *text,
        format!(
            "checking for agent-device sessions on one (agent-device status unreadable: node \
             not found; {STATUS_FIX}; a person may release this device: hostler devices \
             forget-sessions one)"
        )
    );
}

/// P3bT9b-3: a takeover while the status is unreadable is refused at begin, before anything is
/// stopped, with the fix as its recovery.
#[tokio::test]
async fn takeover_while_unreadable_is_refused_at_begin() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    b.good();
    let (a, other) = (b.workspace("a"), b.workspace("b"));
    assert!(matches!(
        b.acquire(&b.hold(&a, 100, P1)).await,
        ClaimOutcome::Granted { .. }
    ));
    b.unreadable();
    let begin = |request| crate::contract::claims::SeizeBegin {
        resource: ResourceId(P1.into()),
        confirmation: "one".into(),
        request,
    };
    let e = b
        .engine
        .lock()
        .await
        .seize_begin(
            begin(b.hold(&other, 200, P1)),
            &crate::claims::engine::no_heavy(),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::SeizureFailed);
    assert!(
        e.message
            .contains("agent-device status unreadable: node not found"),
        "{}",
        e.message
    );
    assert_eq!(
        e.recovery.as_deref(),
        Some(format!("{STATUS_FIX}; then take it over again").as_str())
    );
    // Readable again: the takeover is planned.
    b.good();
    assert!(
        b.engine
            .lock()
            .await
            .seize_begin(
                begin(b.hold(&other, 201, P1)),
                &crate::claims::engine::no_heavy()
            )
            .is_ok()
    );
}

/// L51, L54: a device no claim ended on stays grantable while the status is unreadable, with
/// the warning; a good read that no longer lists a kept session drops it and the warning.
#[tokio::test]
async fn unreadable_status_leaves_session_free_devices_grantable_and_recovery_clears_it() {
    let b = both();
    b.svc.test_android(&[
        (adb("SERIAL0001", true), phone("SERIAL0001")),
        (adb("SERIAL0002", true), phone("SERIAL0002")),
    ]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    b.svc.add("SERIAL0002", Some("two"), true).unwrap();
    read_claims(&b.svc, &[claim("SERIAL0002", Platform::Android, "s2")]);
    b.unreadable();
    let warning = format!("agent-device sessions unknown: node not found; {STATUS_FIX}");
    let tools = b.svc.list(false).tools;
    assert!(!tools.sessions_known);
    assert!(tools.problems.contains(&warning), "{:?}", tools.problems);
    let ws = b.workspace("b");
    assert!(matches!(
        b.acquire(&b.hold(&ws, 200, P1)).await,
        ClaimOutcome::Granted { .. }
    ));
    let w2 = b.hold(&ws, 201, P2);
    assert!(blocked(&b.acquire(&w2).await), "the kept session blocks");
    // Recovery: a good read without s2.
    b.good();
    let tools = b.svc.list(false).tools;
    assert!(tools.sessions_known);
    assert!(!tools.problems.contains(&warning));
    assert!(b.svc.sessions_on(&ResourceId(P2.into())).is_empty());
    b.engine
        .lock()
        .await
        .tick(&crate::claims::engine::no_heavy());
    assert!(matches!(
        b.state_of(&w2).await,
        ClaimOutcome::Granted { .. }
    ));
}

// ---- P3bT9b-5 ----

/// The status reads again while `forget-sessions` waits for the claims lock: the good read's
/// live sessions are never dropped.
#[tokio::test]
async fn forget_after_status_recovered_drops_nothing() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    let live = [claim("SERIAL0001", Platform::Android, "s1")];
    read_claims(&b.svc, &live);
    b.unreadable();
    let guard = b.engine.lock().await;
    let forget = forget_sessions(&b.svc, &b.engine, "one", true, None, None);
    let recover = async {
        tokio::task::yield_now().await;
        read_claims(&b.svc, &live);
        drop(guard);
    };
    let (forgot, ()) = tokio::join!(forget, recover);
    let e = forgot.unwrap_err();
    assert!(e.message.contains("reads again"), "{}", e.message);
    assert_eq!(b.svc.sessions_on(&ResourceId(P1.into())), ["s1"]);
    assert!(
        b.store
            .session_forgets(&ResourceId(P1.into()))
            .unwrap()
            .is_empty()
    );
    // Directly: nothing to drop once it reads.
    assert!(b.svc.drop_sessions_on(&ResourceId(P1.into())).is_err());
}

// ---- P3bT9b-6: fail closed ----

#[tokio::test]
async fn a_panicking_mapping_is_a_failed_read() {
    let svc = service();
    svc.test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    svc.add("SERIAL0001", None, true).unwrap();
    read_claims(&svc, &[claim("SERIAL0001", Platform::Android, "s1")]);
    let panicked = tokio::task::spawn_blocking(|| -> Vec<RawSession> { panic!("mapping") }).await;
    let read = read_of_mapping(panicked);
    assert!(matches!(read, SessionRead::Failed { .. }), "{read:?}");
    let seq = svc.cache().ends();
    let poll = svc.apply_session_read(read, seq, svc.now());
    assert!(!poll.good);
    assert_eq!(
        svc.sessions_on(&ResourceId(P1.into())),
        ["s1"],
        "the last good sessions stay"
    );
    assert!(svc.sessions_unreadable().is_some());
}

#[test]
fn unreadable_claims_dir_is_a_failed_read() {
    assert!(in_use_at(ClaimsDir::Unknown).is_err(), "no home");
    assert_eq!(in_use_at(ClaimsDir::NotHere), Ok(false));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(in_use_at(ClaimsDir::At(dir.path().join("none"))), Ok(false));
    std::fs::create_dir(dir.path().join("device-claims")).unwrap();
    assert_eq!(
        in_use_at(ClaimsDir::At(dir.path().join("device-claims"))),
        Ok(true)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads through any mode: only a refused check is asserted.
        let refused = std::fs::read_dir(&locked).is_err();
        let answer = in_use_at(ClaimsDir::At(locked.join("device-claims")));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if refused {
            assert!(answer.is_err(), "{answer:?}");
        }
    }
}

#[test]
fn unknown_platform_live_claim_is_a_problem() {
    let svc = service();
    let status = br#"{"success": true, "data": {"claims": [
        {"classification": "live", "device": {"id": "TV0001", "platform": "tvos", "name": "Den"},
         "owner": {"session": "tv"}}
    ]}}"#;
    let claims = parse_status(status).unwrap();
    read_claims(&svc, &claims);
    let unmapped = unmapped(&svc);
    assert_eq!(unmapped.len(), 1);
    assert!(
        unmapped[0].contains("tv") && unmapped[0].contains("does not know that platform"),
        "{}",
        unmapped[0]
    );
}

// ---- P3bT9b-7 ----

static CANONICAL_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn slow_canonical(p: &Path) -> Option<String> {
    CANONICAL_CALLS.fetch_add(1, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    Some(p.display().to_string())
}

fn failing_canonical(_: &Path) -> Option<String> {
    CANONICAL_CALLS.fetch_add(1, Ordering::SeqCst);
    None
}

/// One thread per pending path; a failure is not asked again within 30 s; an answer is reused.
#[test]
fn canonical_workspace_never_spawns_twice_for_a_pending_path() {
    let pending = "/hostler-test/canonical/pending";
    let short = Duration::from_millis(10);
    assert_eq!(canonical_with(pending, short, slow_canonical), None);
    assert_eq!(canonical_with(pending, short, slow_canonical), None);
    assert_eq!(CANONICAL_CALLS.load(Ordering::SeqCst), 1, "one thread");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        canonical_with(pending, short, slow_canonical).as_deref(),
        Some(pending),
        "the late answer is kept"
    );
    assert_eq!(CANONICAL_CALLS.load(Ordering::SeqCst), 1);
    let failing = "/hostler-test/canonical/failing";
    assert_eq!(
        canonical_with(failing, CANONICAL_BOUND, failing_canonical),
        None
    );
    assert_eq!(
        canonical_with(failing, CANONICAL_BOUND, failing_canonical),
        None
    );
    assert_eq!(CANONICAL_CALLS.load(Ordering::SeqCst), 2, "negative cache");
}

// ---- P3bT9b-8 ----

fn service_at(clock: Arc<TestClock>) -> Arc<DevicesService> {
    let (tx, _) = crate::service::events::EventBus::channel(256);
    DevicesService::with_clock(
        Store::open_in_memory().unwrap(),
        ToolPaths::default(),
        tx,
        clock,
    )
    .0
}

/// No agent-device found: searched for at most once a minute, and the search leaves adb's
/// guard alone; a recorded path that went away is searched for at once.
#[test]
fn agent_device_search_is_rate_limited_and_leaves_adb_alone() {
    let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
    let svc = service_at(clock.clone());
    assert!(needs_search(None, &svc));
    assert!(!needs_search(None, &svc), "within the minute");
    clock.0.fetch_add(SEARCH_EVERY_MS, Ordering::SeqCst);
    assert!(needs_search(None, &svc));
    let gone = AgentDeviceCmd {
        node: PathBuf::from("/hostler-test/gone/node"),
        script: PathBuf::from("/hostler-test/gone/agent-device.js"),
    };
    assert!(needs_search(Some(&gone), &svc), "a recorded path went away");
    assert!(needs_search(Some(&gone), &svc), "every time it is gone");
    let guard = svc.adb_guard();
    svc.search_agent_device();
    assert!(Arc::ptr_eq(&guard, &svc.adb_guard()), "adb's guard stays");
}

// ---- the plan's tests (P3bT9b-10) ----

fn checking(o: &ClaimOutcome) -> bool {
    matches!(o, ClaimOutcome::Queued { blockers, .. }
        if blockers.iter().any(|b| matches!(b, crate::contract::claims::Blocker::Checking { .. })))
}

/// L52: an expired hold and a reaped one end their claims like a release: the device waits
/// for a read that started after the end.
#[tokio::test]
async fn reaped_and_expired_device_claims_wait_for_a_fresh_read() {
    let b = both();
    b.svc
        .test_android(&[(adb("SERIAL0001", true), phone("SERIAL0001"))]);
    b.svc.add("SERIAL0001", Some("one"), true).unwrap();
    b.good();
    let (a, other) = (b.workspace("a"), b.workspace("b"));
    // Expired: its TTL runs out.
    assert!(matches!(
        b.acquire(&b.hold(&a, 100, P1)).await,
        ClaimOutcome::Granted { .. }
    ));
    b.clock.0.fetch_add(61_000, Ordering::SeqCst);
    b.tick().await;
    let w1 = b.hold(&other, 200, P1);
    assert!(checking(&b.acquire(&w1).await), "expired: checked");
    b.good();
    b.tick().await;
    let ClaimOutcome::Granted { claim } = b.state_of(&w1).await else {
        panic!("granted after the read");
    };
    // Reaped: a lease's holder is gone.
    b.engine
        .lock()
        .await
        .release(ClaimRelease {
            claim_id: claim.id,
            workspace_id: other.clone(),
            requester: w1.requester.clone(),
            exit_code: None,
        })
        .unwrap();
    b.good();
    let lease = ClaimRequest {
        purpose: Purpose::Lease {
            command: "test".into(),
        },
        ..b.hold(&a, 101, P1)
    };
    assert!(matches!(
        b.acquire(&lease).await,
        ClaimOutcome::Granted { .. }
    ));
    b.table.remove(101);
    b.tick().await;
    let w2 = b.hold(&other, 201, P1);
    assert!(checking(&b.acquire(&w2).await), "reaped: checked");
    b.good();
    b.tick().await;
    assert!(matches!(
        b.state_of(&w2).await,
        ClaimOutcome::Granted { .. }
    ));
}

/// L54, P3bT9b-8: nvm moves Node to a new version: the recorded path is gone, a search runs at
/// once and finds the new one.
#[cfg(unix)]
#[test]
fn tool_path_gone_triggers_a_new_search() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let prefix = t.path().join("prefix");
    let pkg = prefix.join("lib/node_modules/agent-device");
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    std::fs::write(pkg.join("bin/agent-device.mjs"), "").unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"agent-device","bin":{"agent-device":"bin/agent-device.mjs"}}"#,
    )
    .unwrap();
    let launcher = prefix.join("bin/agent-device");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(
        "../lib/node_modules/agent-device/bin/agent-device.mjs",
        &launcher,
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".nvm/alias")).unwrap();
    std::fs::write(home.join(".nvm/alias/default"), "22\n").unwrap();
    let node_at = |v: &str| {
        let node = home.join(format!(".nvm/versions/node/{v}/bin/node"));
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(&node, "").unwrap();
        node
    };
    let old = node_at("v22.4.1");
    let settings = crate::devices::tools::ToolSettings {
        agent_device_path: Some(launcher),
        ..Default::default()
    };
    let find = || tools::find_tools(&settings, &|_: &str| None, &home).agent_device;
    let first = find().unwrap();
    assert_eq!(first.node, old);
    let svc = service();
    assert!(!needs_search(Some(&first), &svc), "the path is there");
    // An upgrade: v22.4.1 goes, v22.5.0 comes.
    std::fs::remove_dir_all(home.join(".nvm/versions/node/v22.4.1")).unwrap();
    let new = node_at("v22.5.0");
    assert!(needs_search(Some(&first), &svc), "at once");
    assert_eq!(find().unwrap().node, new);
}

/// P3bT9b-1 (an AVD whose port was never stored, or whose identity is not read yet): a session
/// on such an `emulator-N` blocks every AVD not on a known port, never none; discovery and the
/// next good read narrow it to the AVD that runs there.
#[test]
fn unidentified_emulator_session_blocks_every_avd_not_on_a_known_port() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hostler.db");
    let (pixel_9, pixel_8) = (
        super::super::service::avd_resource("Pixel_9"),
        super::super::service::avd_resource("Pixel_8"),
    );
    {
        let svc = service_on(Store::open(&path).unwrap());
        for (port, avd) in [("emulator-5554", "Pixel_9"), ("emulator-5556", "Pixel_8")] {
            svc.test_android(&[(adb(port, false), emulator(avd))]);
            svc.add(port, None, false).unwrap();
        }
    }
    let svc = service_on(Store::open(&path).unwrap());
    let on = |svc: &DevicesService| {
        let mut on: Vec<String> = svc
            .cache()
            .sessions()
            .iter()
            .map(|f| f.resource.0.clone())
            .collect();
        on.sort();
        on
    };
    read_claims(&svc, &[claim("emulator-5560", Platform::Android, "x")]);
    assert_eq!(on(&svc), [pixel_8.0.clone(), pixel_9.0.clone()], "both");
    assert!(unmapped(&svc).is_empty());
    // Pixel_8 turns out to run on 5560, Pixel_9 on 5554.
    svc.test_android(&[
        (adb("emulator-5554", false), emulator("Pixel_9")),
        (adb("emulator-5560", false), emulator("Pixel_8")),
    ]);
    read_claims(&svc, &[claim("emulator-5560", Platform::Android, "x")]);
    assert_eq!(on(&svc), std::slice::from_ref(&pixel_8.0));
}

/// P3bT9b-9: a stopper call never runs past the 20 s.
#[test]
fn stopper_calls_are_bounded_by_what_is_left() {
    let now = Instant::now();
    let cap = Duration::from_secs(5);
    assert_eq!(
        call_bound(now + Duration::from_secs(12), cap, now).unwrap(),
        cap
    );
    assert_eq!(
        call_bound(now + Duration::from_millis(1500), cap, now).unwrap(),
        Duration::from_millis(1500)
    );
    assert!(matches!(
        call_bound(now, cap, now),
        Err(crate::cli::takeover::StopError::StillAlive)
    ));
}
