//! The heavy-queue engine inside a real service (plan Task 7): crash between commit and reply,
//! and a run group outliving its wrapper across a service restart.

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use hostler::client::Client;
use hostler::contract::ErrorCode;
use hostler::contract::heavy::{AcquireOutcome, HeavyAcquire, QueueSnapshot};
use hostler::contract::wire::Request;
use hostler::heavy::engine::{MAX_COMMAND_BYTES, MAX_LIVE_ENTRIES, MAX_PATH_BYTES, json_len};
use hostler::paths::Paths;
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use hostler::service::NoManager;
use hostler::store::Store;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_hostler");

struct Home {
    home: PathBuf,
    _dir: TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        Home {
            home: dir.path().join("home"),
            _dir: dir,
        }
    }

    fn hostler(&self) -> Command {
        let mut c = Command::new(BIN);
        // A ledger that does not exist: this machine's real rn-looky work must not block tests.
        c.env("HOSTLER_HOME", &self.home)
            .env(
                "HOSTLER_LOOKY_LEDGER",
                self.home.join("no-looky-ledger.json"),
            )
            .env_remove("HOSTLER_TEST_CONTRACT_VERSION")
            .env_remove("HOSTLER_TEST_EXIT_AFTER_COMMIT")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.hostler().args(args).output().unwrap()
    }

    fn paths(&self) -> Paths {
        let paths = Paths::under(self.home.clone(), true);
        paths.ensure_dirs().unwrap();
        paths
    }

    fn service(&self, env: &[(&str, &str)]) -> Child {
        let child = self
            .hostler()
            .args(["service", "run"])
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.run(&["service", "status"]).status.code() != Some(0) {
            assert!(Instant::now() < deadline, "the service never came up");
            std::thread::sleep(Duration::from_millis(50));
        }
        child
    }

    /// Starts the service through the CLI (so it gets the test environment), then connects.
    async fn client(&self) -> Client {
        let start = self.run(&["service", "start"]);
        assert!(
            start.status.success(),
            "{}",
            String::from_utf8_lossy(&start.stderr)
        );
        Client::connect_or_start_with(&self.paths(), &NoManager, BIN.as_ref())
            .await
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["service", "stop", "--force"]);
    }
}

fn request(holder: ProcessIdentity) -> HeavyAcquire {
    HeavyAcquire {
        id: uuid::Uuid::new_v4().to_string(),
        kind: "gradle".into(),
        command: "./gradlew assemble".into(),
        cwd: "/work/app".into(),
        workspace_root: None,
        holder,
        reentrant_token: None,
        no_wait: false,
    }
}

async fn acquire(client: &mut Client, req: &HeavyAcquire) -> AcquireOutcome {
    let value = client
        .call(Request::HeavyAcquire(req.clone()))
        .await
        .unwrap();
    serde_json::from_value(value).unwrap()
}

async fn snapshot(client: &mut Client) -> QueueSnapshot {
    serde_json::from_value(client.call(Request::QueueSnapshot).await.unwrap()).unwrap()
}

#[tokio::test]
async fn lost_acquire_reply_is_recovered_without_duplicate() {
    let home = Home::new();
    let mut crashing = home.service(&[("HOSTLER_TEST_EXIT_AFTER_COMMIT", "heavy.acquire")]);
    let req = request(current_identity(&SysProcessTable::new().unwrap()).unwrap());

    let mut client = Client::connect(&home.paths()).await.unwrap();
    let lost = client
        .call(Request::HeavyAcquire(req.clone()))
        .await
        .unwrap_err();
    assert_eq!(lost.code, ErrorCode::ServiceUnavailable, "{lost:?}");
    assert!(!crashing.wait().unwrap().success());

    // The client retries the same id against a fresh service.
    let mut client = home.client().await;
    for _ in 0..2 {
        match acquire(&mut client, &req).await {
            AcquireOutcome::Granted { entry } => {
                assert_eq!(entry.id, req.id);
                assert_eq!(entry.ticket, 1);
            }
            other => panic!("{other:?}"),
        }
    }
    let queue = snapshot(&mut client).await;
    assert_eq!(queue.running.len(), 1);
    assert!(queue.waiting.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn group_survivor_keeps_slot_across_service_restart() {
    use std::os::unix::process::CommandExt;

    use hostler::contract::heavy::{HeavyHistoryItem, HeavyOutcome, HeavyStarted, HistoryQuery};
    use hostler::procs::{ProcessTable, RunGroup, identity_of};

    fn gated(group: bool) -> Child {
        let mut c = Command::new("cat");
        c.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if group {
            c.process_group(0);
        }
        c.spawn().unwrap()
    }

    fn finish(mut child: Child) {
        drop(child.stdin.take());
        child.wait().unwrap();
    }

    let home = Home::new();
    let table = SysProcessTable::new().unwrap();
    let mut client = home.client().await;

    // The wrapper (holder) is granted the slot and starts a child in its own group, which
    // starts a grandchild in the same group.
    let wrapper = gated(false);
    let a = request(identity_of(&table, wrapper.id()).unwrap());
    assert!(matches!(
        acquire(&mut client, &a).await,
        AcquireOutcome::Granted { .. }
    ));
    let child = gated(true);
    let pgid = child.id();
    let mut grandchild = Command::new("sleep")
        .arg("30")
        .process_group(pgid as i32)
        .spawn()
        .unwrap();
    let started = HeavyStarted {
        id: a.id.clone(),
        child: identity_of(&table, child.id()).unwrap(),
        group: RunGroup::UnixGroup {
            pgid,
            boot_id: table.boot_id(),
        },
    };
    client.call(Request::HeavyStarted(started)).await.unwrap();

    // Wrapper and child die; then the service restarts.
    finish(wrapper);
    finish(child);
    drop(client);
    assert!(home.run(&["service", "stop", "--force"]).status.success());
    let mut client = home.client().await;

    let b = request(current_identity(&table).unwrap());
    assert!(matches!(
        acquire(&mut client, &b).await,
        AcquireOutcome::Queued { .. }
    ));
    let attach_a = Request::HeavyAttach { id: a.id.clone() };
    let still: AcquireOutcome =
        serde_json::from_value(client.call(attach_a).await.unwrap()).unwrap();
    assert!(matches!(still, AcquireOutcome::Granted { .. }), "{still:?}");

    // The survivor ends: the slot goes to the next request.
    grandchild.kill().unwrap();
    grandchild.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let attach_b = Request::HeavyAttach { id: b.id.clone() };
        let outcome: AcquireOutcome =
            serde_json::from_value(client.call(attach_b).await.unwrap()).unwrap();
        if matches!(outcome, AcquireOutcome::Granted { .. }) {
            break;
        }
        assert!(Instant::now() < deadline, "the slot was never freed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let history: Vec<HeavyHistoryItem> = serde_json::from_value(
        client
            .call(Request::QueueHistory(HistoryQuery::default()))
            .await
            .unwrap(),
    )
    .unwrap();
    let item = history.iter().find(|i| i.id == a.id).unwrap();
    assert_eq!(item.outcome, HeavyOutcome::Reaped);
}

/// A request with every variable field at its admitted maximum.
fn max_request(holder: ProcessIdentity) -> HeavyAcquire {
    HeavyAcquire {
        command: "c".repeat(MAX_COMMAND_BYTES),
        cwd: "/".repeat(MAX_PATH_BYTES),
        workspace_root: Some("/".repeat(MAX_PATH_BYTES)),
        ..request(holder)
    }
}

/// `n` live max-size entries held by this test process, written before the service starts.
fn seed_max_entries(home: &Home, n: usize) {
    let store = Store::open(&home.paths().db).unwrap();
    let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
    for _ in 0..n {
        store
            .insert_waiting(&max_request(holder.clone()), 0)
            .unwrap();
    }
}

#[tokio::test]
async fn max_size_snapshot_reaches_a_fresh_client() {
    let home = Home::new();
    seed_max_entries(&home, MAX_LIVE_ENTRIES);
    let mut client = home.client().await;
    let queue = snapshot(&mut client).await;
    assert_eq!(queue.running.len(), 1);
    assert_eq!(queue.waiting.len(), MAX_LIVE_ENTRIES - 1);

    let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
    let err = client
        .call(Request::HeavyAcquire(max_request(holder)))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::QueueBusy, "{err:?}");
}

/// Both sides write large frames at once: the client a 2 MiB request, the service queue
/// snapshots of ~1 MiB (far beyond socket buffers). Neither may wait for the other to read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_request_while_snapshots_are_pushed() {
    let home = Home::new();
    seed_max_entries(&home, 200);
    let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
    let mut observer = home.client().await;

    // Another client keeps the queue changing, so snapshots are pushed to `observer`.
    let mut churn = home.client().await;
    let churn_holder = holder.clone();
    let churning = tokio::spawn(async move {
        loop {
            let req = max_request(churn_holder.clone());
            acquire(&mut churn, &req).await;
            let withdraw = Request::HeavyWithdraw {
                id: req.id,
                timed_out: false,
            };
            churn.call(withdraw).await.unwrap();
        }
    });

    let mut huge = max_request(holder);
    // 2 MiB: far beyond any socket buffer, which is all the stall needs, while keeping each
    // call well inside the 5 s bound on a heavily loaded machine.
    huge.command = "x".repeat(2 << 20);
    for _ in 0..5 {
        let started = Instant::now();
        let err = observer
            .call(Request::HeavyAcquire(huge.clone()))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest, "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    assert!(observer.next_event().await.is_some());
    churning.abort();
}

/// Exactly `budget` JSON bytes of the costliest characters to escape (U+0001 is 6 bytes as
/// JSON, `"` is 2).
fn escaped(budget: usize) -> String {
    let mut s = "\u{1}".repeat(budget / 6);
    s.push_str(&"\"".repeat(budget % 6 / 2));
    if budget % 2 == 1 {
        s.push('a');
    }
    assert_eq!(json_len(&s), budget);
    s
}

fn escaped_max_request(holder: ProcessIdentity) -> HeavyAcquire {
    HeavyAcquire {
        command: escaped(MAX_COMMAND_BYTES),
        cwd: escaped(MAX_PATH_BYTES),
        workspace_root: Some(escaped(MAX_PATH_BYTES)),
        ..request(holder)
    }
}

#[tokio::test]
async fn escaped_max_snapshot_and_history_fit_one_frame() {
    use hostler::contract::heavy::{HeavyHistoryItem, HeavyOutcome, HistoryQuery};
    use hostler::service::server::MAX_FRAME_BYTES;

    let home = Home::new();
    {
        let store = Store::open(&home.paths().db).unwrap();
        let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
        // Recent enough to survive the service's 30-day pruning.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        for _ in 0..1000 {
            let req = escaped_max_request(holder.clone());
            store.insert_waiting(&req, now_ms).unwrap();
            store
                .finish(&req.id, Some(0), HeavyOutcome::Ok, now_ms)
                .unwrap();
        }
        for _ in 0..MAX_LIVE_ENTRIES {
            store
                .insert_waiting(&escaped_max_request(holder.clone()), 0)
                .unwrap();
        }
    }
    let mut client = home.client().await;

    let raw = client.call(Request::QueueSnapshot).await.unwrap();
    assert!(serde_json::to_string(&raw).unwrap().len() < MAX_FRAME_BYTES);
    let queue: QueueSnapshot = serde_json::from_value(raw).unwrap();
    assert_eq!(queue.running.len() + queue.waiting.len(), MAX_LIVE_ENTRIES);
    assert_eq!(json_len(&queue.waiting[0].command), MAX_COMMAND_BYTES);

    let query = HistoryQuery {
        since_ms: None,
        limit: 1000,
    };
    let raw = client.call(Request::QueueHistory(query)).await.unwrap();
    assert!(serde_json::to_string(&raw).unwrap().len() < MAX_FRAME_BYTES);
    let history: Vec<HeavyHistoryItem> = serde_json::from_value(raw).unwrap();
    assert_eq!(history.len(), 1000);
}

/// A subscriber that stops reading fills its socket and its outbound queue with snapshots;
/// its own accepted stop must still stop the service.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_stop_completes_while_a_subscriber_never_reads() {
    use hostler::service::lock;
    use interprocess::local_socket::tokio::Stream;
    use interprocess::local_socket::traits::tokio::Stream as _;
    use tokio::io::AsyncWriteExt;

    let home = Home::new();
    seed_max_entries(&home, 20); // ~100 KiB per snapshot
    let mut service = home.service(&[]);
    let paths = home.paths();

    let name = hostler::service::server::socket_name(&paths.socket).unwrap();
    let mut silent = Stream::connect(name).await.unwrap();
    silent
        .write_all(
            b"{\"id\":1,\"method\":\"hello\",\"params\":{\"contract_version\":2,\"client\":\"silent\"}}\n",
        )
        .await
        .unwrap();

    // 400 queue changes: far more snapshot bytes than the socket and 16 queued frames hold.
    let mut churn = Client::connect(&paths).await.unwrap();
    let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
    for _ in 0..200 {
        let req = max_request(holder.clone());
        acquire(&mut churn, &req).await;
        let withdraw = Request::HeavyWithdraw {
            id: req.id,
            timed_out: false,
        };
        churn.call(withdraw).await.unwrap();
    }

    silent
        .write_all(b"{\"id\":2,\"method\":\"service.shutdown\",\"params\":{\"if_idle\":false}}\n")
        .await
        .unwrap();
    let asked = Instant::now();
    while lock::is_held(&paths.service_lock).unwrap() {
        assert!(
            asked.elapsed() < Duration::from_secs(8),
            "the accepted stop never completed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(service.wait().unwrap().success());
}

/// Rows stored under older, looser limits (written straight into SQLite): responses cut their
/// display strings, never their ids or tickets, so every snapshot and history page still fits.
#[tokio::test]
async fn old_oversized_rows_are_clamped_in_responses() {
    use hostler::contract::heavy::{HeavyHistoryItem, HeavyOutcome, HeavyStarted, HistoryQuery};
    use hostler::heavy::engine::MAX_IDENTITY_BYTES;
    use hostler::procs::RunGroup;
    use hostler::service::server::MAX_FRAME_BYTES;

    let home = Home::new();
    let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
    // ~40 KiB of JSON per entry: 512 of them would be ~20 MiB unclamped.
    let oversized = |holder: ProcessIdentity| HeavyAcquire {
        command: "\u{1}".repeat(4096),
        cwd: "\"".repeat(4096),
        workspace_root: Some("\"".repeat(4096)),
        ..request(holder)
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let stored = {
        let store = Store::open(&home.paths().db).unwrap();
        for _ in 0..1000 {
            let req = oversized(holder.clone());
            store.insert_waiting(&req, now_ms).unwrap();
            store
                .finish(&req.id, Some(0), HeavyOutcome::Ok, now_ms)
                .unwrap();
        }
        for _ in 0..MAX_LIVE_ENTRIES {
            store
                .insert_waiting(&oversized(holder.clone()), now_ms)
                .unwrap();
        }
        let first = store.entries().unwrap()[0].id.clone();
        let huge_group = HeavyStarted {
            id: first,
            child: ProcessIdentity {
                boot_id: "\u{1}".repeat(4096),
                ..holder.clone()
            },
            group: RunGroup::WindowsJob {
                name: "\u{1}".repeat(4096),
                boot_id: "\u{1}".repeat(4096),
            },
        };
        store
            .set_started(&huge_group.id, &huge_group.child, &huge_group.group)
            .unwrap();
        store.entries().unwrap()
    };
    let mut client = home.client().await;

    let raw = client.call(Request::QueueSnapshot).await.unwrap();
    assert!(serde_json::to_string(&raw).unwrap().len() < MAX_FRAME_BYTES);
    let queue: QueueSnapshot = serde_json::from_value(raw).unwrap();
    let shown: Vec<_> = queue.running.iter().chain(&queue.waiting).collect();
    assert_eq!(shown.len(), MAX_LIVE_ENTRIES);
    for (shown, stored) in shown.iter().zip(&stored) {
        assert_eq!((&shown.id, shown.ticket), (&stored.id, stored.ticket));
        assert_eq!(shown.holder.pid, stored.holder.pid);
        assert_eq!(shown.holder.start_time, stored.holder.start_time);
        assert!(json_len(&shown.command) <= MAX_COMMAND_BYTES);
        assert!(stored.command.starts_with(&shown.command));
        assert!(json_len(&shown.cwd) <= MAX_PATH_BYTES);
    }
    let first = &queue.running[0];
    assert!(json_len(&first.child.as_ref().unwrap().boot_id) <= MAX_IDENTITY_BYTES);
    let Some(RunGroup::WindowsJob { name, boot_id }) = &first.group else {
        panic!("{:?}", first.group)
    };
    assert!(json_len(name) <= MAX_IDENTITY_BYTES && json_len(boot_id) <= MAX_IDENTITY_BYTES);

    // A single-entry answer is cut the same way.
    let attach = Request::HeavyAttach {
        id: first.id.clone(),
    };
    match serde_json::from_value(client.call(attach).await.unwrap()).unwrap() {
        AcquireOutcome::Granted { entry } => {
            assert!(json_len(&entry.command) <= MAX_COMMAND_BYTES)
        }
        other => panic!("{other:?}"),
    }

    let query = HistoryQuery {
        since_ms: None,
        limit: 1000,
    };
    let raw = client.call(Request::QueueHistory(query)).await.unwrap();
    assert!(serde_json::to_string(&raw).unwrap().len() < MAX_FRAME_BYTES);
    let history: Vec<HeavyHistoryItem> = serde_json::from_value(raw).unwrap();
    assert_eq!(history.len(), 1000);
    assert!(
        history.iter().all(
            |i| json_len(&i.command) <= MAX_COMMAND_BYTES && json_len(&i.cwd) <= MAX_PATH_BYTES
        )
    );
}
