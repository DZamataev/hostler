//! Contract 5's event delivery against the real binary (P5 Task 1, spec §6.2): `seq` on every
//! event frame, `as_of_seq` on snapshots, `instance` and `exe`, and an oversized event that
//! becomes a logged `resync`. The exact coalescing, lag and overflow deliveries are tested in
//! process (`service::server::tests`), where the writer can be held deterministically.

mod common;
#[path = "common/raw.rs"]
mod raw;

use common::{Home, stderr};
use hostler::contract::wire::{Event, Request, ResyncReason};
use raw::{Raw, blocking, files_text, runtime};
use serde_json::Value;

fn set_capacity(home: &Home, capacity: u8) {
    blocking(|| home.call(Request::QueueSetCapacity { capacity })).expect("queue.set_capacity");
}

/// Event frames carry an increasing `seq`; a snapshot event names its own `seq`; a
/// `queue.snapshot` taken between two queue events A < B has `A.seq <= as_of_seq < B.seq`.
#[test]
fn as_of_seq_orders_snapshots() {
    let home = Home::new();
    home.start_service();
    runtime().block_on(async {
        let mut raw = Raw::connect(&home).await;
        set_capacity(&home, 2);
        let (a, event) = raw.event().await;
        let Event::QueueChanged(snapshot) = event else {
            panic!("not a queue event")
        };
        assert_eq!(snapshot.as_of_seq, a);
        assert_eq!(snapshot.capacity, 2);
        let answer = raw.call(2, "queue.snapshot", None).await;
        let as_of = answer["result"]["as_of_seq"].as_u64().unwrap();
        set_capacity(&home, 3);
        let (b, event) = raw.event().await;
        assert!(matches!(event, Event::QueueChanged(s) if s.capacity == 3 && s.as_of_seq == b));
        assert!(a <= as_of && as_of < b, "{a} <= {as_of} < {b}");
    });
}

/// `service status --json` has `exe` (the binary, canonical) and the `instance` of `hello`;
/// a restart makes a new instance.
#[test]
fn status_reports_exe_and_instance() {
    let home = Home::new();
    home.start_service();
    let status = |home: &Home| {
        let out = home
            .hostler()
            .args(["service", "status", "--json"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"].clone()
    };
    let first = status(&home);
    let exe = std::fs::canonicalize(common::BIN).unwrap();
    assert_eq!(
        first["exe"].as_str().unwrap(),
        hostler::project::display_path(&exe)
    );
    let instance = first["instance"].as_str().unwrap().to_owned();
    assert_eq!(
        uuid::Uuid::parse_str(&instance).unwrap().get_version_num(),
        4
    );
    let hello = runtime().block_on(async { Raw::connect(&home).await.hello });
    assert_eq!(hello["instance"], instance.as_str());
    let restart = home
        .hostler()
        .args(["service", "restart"])
        .output()
        .unwrap();
    assert!(restart.status.success(), "{}", stderr(&restart));
    let second = status(&home);
    assert_ne!(second["instance"].as_str().unwrap(), instance);
}

/// An event over the frame limit (`HOSTLER_TEST_EVENT_PAD_BYTES`) reaches the connection as
/// one `resync{oversized}`, later events follow it, and the service log names the dropped
/// kind and size.
#[test]
fn oversized_event_sends_resync() {
    let home = Home::new();
    let out = home
        .hostler()
        .env("HOSTLER_TEST_EVENT_PAD_BYTES", "queue.changed:9000000")
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
    runtime().block_on(async {
        let mut raw = Raw::connect(&home).await;
        set_capacity(&home, 2);
        let (dropped, event) = raw.event().await;
        assert_eq!(
            event,
            Event::Resync {
                reason: ResyncReason::Oversized
            }
        );
        // A later event of another kind follows it.
        let repo = home.make_repo("app");
        blocking(|| home.workspace(&repo)).unwrap();
        let (next, event) = raw.event().await;
        assert!(next > dropped);
        assert!(matches!(event, Event::ProjectsChanged(_)), "{event:?}");
    });
    let stop = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(stop.status.success(), "{}", stderr(&stop));
    let log = files_text(&home.paths().log_dir);
    assert!(
        log.lines()
            .any(|l| l.contains("dropped a queue.changed event of")
                && l.contains("bytes over the 8388608-byte frame limit")),
        "{log}"
    );
}
