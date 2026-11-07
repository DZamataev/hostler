//! `hostler run --heavy` against the real binary and a real service (plan Task 8). Every test has
//! its own `HOSTLER_HOME` and stops its service on drop. The command is the `hold` example:
//! it appends `start <ms>` / `end <ms>` to a file.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use hostler::client::Client;
use hostler::contract::heavy::{HeavyHistoryItem, HeavyOutcome, HistoryQuery, QueueSnapshot};
use hostler::contract::wire::Request;
use hostler::paths::Paths;
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_hostler");

fn hold_exe() -> &'static Path {
    static HOLD: OnceLock<PathBuf> = OnceLock::new();
    HOLD.get_or_init(|| {
        escargot::CargoBuild::new()
            .example("hold")
            .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .run()
            .expect("building the hold example")
            .path()
            .to_path_buf()
    })
}

struct Home {
    home: PathBuf,
    dir: TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        Home {
            home: dir.path().join("home"),
            dir,
        }
    }

    fn hostler(&self) -> Command {
        let mut c = Command::new(BIN);
        // A ledger that does not exist: this machine's real rn-looky work must not block tests.
        // Services started by these processes inherit it.
        c.env("HOSTLER_HOME", &self.home)
            .env(
                "HOSTLER_LOOKY_LEDGER",
                self.home.join("no-looky-ledger.json"),
            )
            .env_remove("HOSTLER_HEAVY_TOKEN")
            .env_remove("HOSTLER_HEAVY_MAX_WAIT")
            .env_remove("HOSTLER_TEST_CONTRACT_VERSION")
            .env_remove("HOSTLER_TEST_EXIT_AFTER_COMMIT")
            .env_remove("HOSTLER_TEST_KILL_BEFORE_GATE")
            .current_dir(self.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// `hostler run --heavy <flags> -- <command>`.
    fn heavy(&self, flags: &[&str], command: Vec<OsString>) -> Command {
        let mut c = self.hostler();
        c.args(["run", "--heavy"])
            .args(flags)
            .arg("--")
            .args(command);
        c
    }

    /// The hold example writing to `<tempdir>/<name>`.
    fn hold(&self, name: &str, ms: u64, extra: &[&str]) -> Vec<OsString> {
        let mut argv = vec![
            hold_exe().as_os_str().to_owned(),
            self.file(name).into_os_string(),
            ms.to_string().into(),
        ];
        argv.extend(extra.iter().map(OsString::from));
        argv
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn paths(&self) -> Paths {
        let paths = Paths::under(self.home.clone(), true);
        paths.ensure_dirs().unwrap();
        paths
    }

    fn stop(&self) {
        let _ = self.hostler().args(["service", "stop", "--force"]).output();
    }

    fn call(&self, request: Request) -> Value {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let paths = self.paths();
        runtime.block_on(async {
            // Never auto-start from this process: a service started here would lack the
            // per-test ledger override. `service start` does it with the test's environment.
            let mut client = match Client::connect(&paths).await {
                Ok(client) => client,
                Err(_) => {
                    let started = self.hostler().args(["service", "start"]).output().unwrap();
                    assert!(started.status.success(), "{started:?}");
                    Client::connect(&paths).await.unwrap()
                }
            };
            client.call(request).await.unwrap()
        })
    }

    fn snapshot(&self) -> QueueSnapshot {
        serde_json::from_value(self.call(Request::QueueSnapshot)).unwrap()
    }

    fn history(&self) -> Vec<HeavyHistoryItem> {
        serde_json::from_value(self.call(Request::QueueHistory(HistoryQuery::default()))).unwrap()
    }

    /// The history item whose command mentions the file `name`.
    fn item(&self, name: &str) -> Option<HeavyHistoryItem> {
        let file = self.file(name).display().to_string();
        self.history()
            .into_iter()
            .find(|i| i.command.contains(&file))
    }

    /// `start` and `end` times written by the hold example.
    fn marks(&self, name: &str) -> (Option<u64>, Option<u64>) {
        let text = std::fs::read_to_string(self.file(name)).unwrap_or_default();
        let find = |what: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(what)?.trim().parse().ok())
        };
        (find("start"), find("end"))
    }

    fn wait_started(&self, name: &str) {
        wait_until(&format!("{name} to start"), || self.marks(name).0.is_some());
    }

    fn wait_waiting(&self, n: usize) -> QueueSnapshot {
        let mut snapshot = None;
        wait_until(&format!("{n} waiting"), || {
            let s = self.snapshot();
            let done = s.waiting.len() == n;
            snapshot = Some(s);
            done
        });
        snapshot.unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn finish(child: Child) -> Output {
    child.wait_with_output().unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_code(out: &Output, code: i32) {
    assert_eq!(out.status.code(), Some(code), "stderr: {}", stderr(out));
}

#[test]
fn two_runs_are_serialized_with_capacity_one() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 1500, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = finish(home.heavy(&[], home.hold("b", 10, &[])).spawn().unwrap());
    assert_code(&finish(a), 0);
    assert_code(&b, 0);
    let (_, a_end) = home.marks("a");
    let (b_start, _) = home.marks("b");
    assert!(
        b_start.unwrap() >= a_end.unwrap(),
        "b started before a ended"
    );
    let err = stderr(&b);
    assert!(
        err.contains("Heavy queue (capacity 1): you are 1 of 1."),
        "{err}"
    );
    assert!(err.contains("Ctrl+C withdraws the request."), "{err}");
    assert!(err.contains("[hostler] Slot granted after"), "{err}");
}

#[test]
fn capacity_two_overlaps() {
    let home = Home::new();
    home.call(Request::QueueSetCapacity { capacity: 2 });
    let a = home.heavy(&[], home.hold("a", 2000, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = finish(home.heavy(&[], home.hold("b", 10, &[])).spawn().unwrap());
    assert_code(&b, 0);
    assert!(!stderr(&b).contains("Heavy queue"), "{}", stderr(&b));
    assert_code(&finish(a), 0);
    assert!(home.marks("b").1.unwrap() < home.marks("a").1.unwrap());
}

#[test]
fn no_wait_exits_75_when_busy() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 1500, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = finish(
        home.heavy(&["--no-wait"], home.hold("b", 10, &[]))
            .spawn()
            .unwrap(),
    );
    assert_code(&b, 75);
    assert!(
        stderr(&b).contains(
            "[hostler] The heavy slot is busy (you would be 1 of 1); not waiting (--no-wait)."
        ),
        "{}",
        stderr(&b)
    );
    assert_eq!(home.marks("b"), (None, None));
    assert_code(&finish(a), 0);
}

#[test]
fn max_wait_exits_75_and_withdraws() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 4000, &[])).spawn().unwrap();
    home.wait_started("a");
    // 0.03 min = 1.8 s.
    let b = finish(
        home.heavy(&["--max-wait", "0.03"], home.hold("b", 10, &[]))
            .spawn()
            .unwrap(),
    );
    assert_code(&b, 75);
    let err = stderr(&b);
    assert!(err.contains("Waited 0 s of 1 s"), "{err}");
    assert!(
        err.contains("[hostler] Gave up after 1 s in the heavy queue; the request was withdrawn."),
        "{err}"
    );
    assert_eq!(home.marks("b"), (None, None));
    assert_eq!(home.item("b").unwrap().outcome, HeavyOutcome::TimedOut);
    assert!(home.snapshot().waiting.is_empty());
    assert_code(&finish(a), 0);
}

#[test]
fn max_wait_falls_back_to_the_environment() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 4000, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = finish(
        home.heavy(&[], home.hold("b", 10, &[]))
            .env("HOSTLER_HEAVY_MAX_WAIT", "0.02")
            .spawn()
            .unwrap(),
    );
    assert_code(&b, 75);
    assert!(stderr(&b).contains("Gave up after 1 s"), "{}", stderr(&b));
    assert_code(&finish(a), 0);
}

#[test]
fn exit_code_propagates() {
    let home = Home::new();
    let out = finish(home.heavy(&[], home.hold("a", 10, &["3"])).spawn().unwrap());
    assert_code(&out, 3);
    let item = home.item("a").unwrap();
    assert_eq!(
        (item.outcome, item.exit_code),
        (HeavyOutcome::Failed, Some(3))
    );
}

#[test]
fn nested_run_does_not_wait() {
    let home = Home::new();
    let mut inner: Vec<OsString> = vec![
        BIN.into(),
        "run".into(),
        "--heavy".into(),
        "--no-wait".into(),
        "--".into(),
    ];
    inner.extend(home.hold("inner", 10, &[]));
    let out = finish(home.heavy(&[], inner).spawn().unwrap());
    assert_code(&out, 0);
    assert!(home.marks("inner").1.is_some());
    // Only the outer request ever had an entry.
    assert_eq!(home.history().len(), 1);
}

#[test]
fn fail_open_when_service_cannot_start() {
    let home = Home::new();
    // A directory where the lock file belongs: no service can start.
    std::fs::create_dir(home.paths().service_lock).unwrap();
    let out = finish(home.heavy(&[], home.hold("a", 10, &[])).spawn().unwrap());
    assert_code(&out, 0);
    let err = stderr(&out);
    assert_eq!(err.matches("[hostler] Warning:").count(), 1, "{err}");
    assert!(
        err.contains("[hostler] Warning: the hostler service is unavailable ("),
        "{err}"
    );
    assert!(err.contains("); running without the heavy queue."), "{err}");
    assert!(home.marks("a").1.is_some());
}

#[test]
fn fail_open_when_service_never_answers() {
    use interprocess::local_socket::tokio::prelude::*;

    let home = Home::new();
    let paths = home.paths();
    // Hold the lock so no real service starts, and accept connections without answering.
    let mut lock = hostler::service::lock::open(&paths.service_lock).unwrap();
    let _guard = hostler::service::lock::try_acquire(&mut lock)
        .unwrap()
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = {
        let _enter = runtime.enter();
        hostler::service::server::bind(&paths.socket).unwrap()
    };
    runtime.spawn(async move {
        let mut silent = Vec::new();
        while let Ok(stream) = listener.accept().await {
            silent.push(stream);
        }
    });
    let started = Instant::now();
    let out = finish(home.heavy(&[], home.hold("a", 10, &[])).spawn().unwrap());
    assert_code(&out, 0);
    let err = stderr(&out);
    assert_eq!(err.matches("[hostler] Warning:").count(), 1, "{err}");
    assert!(err.contains("running without the heavy queue."), "{err}");
    assert!(home.marks("a").1.is_some());
    assert!(started.elapsed() < Duration::from_secs(20));
    drop(runtime);
}

#[cfg(unix)] // replaces the lock file while the service holds it open
#[test]
fn fail_open_when_service_dies_while_queued() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 3000, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = home.heavy(&[], home.hold("b", 10, &[])).spawn().unwrap();
    home.wait_waiting(1);
    // No service can start again here, then the running one stops.
    let lock = home.paths().service_lock;
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    home.stop();
    let b = finish(b);
    assert_code(&b, 0);
    let err = stderr(&b);
    assert_eq!(err.matches("[hostler] Warning:").count(), 1, "{err}");
    assert!(err.contains("running without the heavy queue."), "{err}");
    assert!(home.marks("b").1.is_some());
    assert_code(&finish(a), 0);
}

fn operator_withdraw(restart: bool) {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 2500, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = home.heavy(&[], home.hold("b", 10, &[])).spawn().unwrap();
    let id = home.wait_waiting(1).waiting[0].id.clone();
    home.call(Request::HeavyWithdraw {
        id,
        timed_out: false,
    });
    if restart {
        home.stop();
    }
    let b = finish(b);
    assert_code(&b, 75);
    assert!(
        stderr(&b).contains("[hostler] The request was withdrawn by an operator; not running."),
        "{}",
        stderr(&b)
    );
    assert_code(&finish(a), 0);
    assert_eq!(home.marks("b"), (None, None));
    assert_eq!(home.item("b").unwrap().outcome, HeavyOutcome::Withdrawn);
}

#[test]
fn operator_withdraw_stops_waiter() {
    operator_withdraw(false);
}

#[test]
fn operator_withdraw_then_service_restart_stops_waiter() {
    operator_withdraw(true);
}

#[test]
fn service_restart_during_wait_keeps_ticket() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 2500, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = home.heavy(&[], home.hold("b", 10, &[])).spawn().unwrap();
    let ticket = home.wait_waiting(1).waiting[0].ticket;
    home.stop();
    let b = finish(b);
    assert_code(&b, 0);
    assert!(!stderr(&b).contains("Warning"), "{}", stderr(&b));
    assert_code(&finish(a), 0);
    assert!(home.marks("b").0.unwrap() >= home.marks("a").1.unwrap());
    let item = home.item("b").unwrap();
    assert_eq!((item.ticket, item.outcome), (ticket, HeavyOutcome::Ok));
    assert_eq!(home.history().len(), 2);
}

#[test]
fn service_restart_during_run_records_real_exit() {
    let home = Home::new();
    home.call(Request::QueueSetCapacity { capacity: 2 });
    let ok = home.heavy(&[], home.hold("ok", 2000, &[])).spawn().unwrap();
    let failing = home
        .heavy(&[], home.hold("failing", 2000, &["3"]))
        .spawn()
        .unwrap();
    home.wait_started("ok");
    home.wait_started("failing");
    home.stop();
    assert_code(&finish(ok), 0);
    assert_code(&finish(failing), 3);
    let ok = home.item("ok").unwrap();
    assert_eq!((ok.outcome, ok.exit_code), (HeavyOutcome::Ok, Some(0)));
    let failing = home.item("failing").unwrap();
    assert_eq!(
        (failing.outcome, failing.exit_code),
        (HeavyOutcome::Failed, Some(3))
    );
}

#[cfg(unix)]
#[test]
fn grandchild_keeps_slot_after_wrapper_killed() {
    let home = Home::new();
    let mut a = home
        .heavy(&[], home.hold("a", 30_000, &["0", "3000"]))
        .spawn()
        .unwrap();
    home.wait_started("a.grandchild");
    let mut snapshot = home.snapshot();
    wait_until("the child to be registered", || {
        snapshot = home.snapshot();
        snapshot.running.first().is_some_and(|e| e.child.is_some())
    });
    let child = snapshot.running[0].child.as_ref().unwrap().pid;
    a.kill().unwrap();
    a.wait().unwrap();
    // SAFETY: a plain kill of the test's own grandchild process.
    assert_eq!(unsafe { libc::kill(child as i32, libc::SIGKILL) }, 0);

    let b = finish(home.heavy(&[], home.hold("b", 10, &[])).spawn().unwrap());
    assert_code(&b, 0);
    let (_, grandchild_end) = home.marks("a.grandchild");
    assert!(home.marks("a").1.is_none(), "the child was not killed");
    assert!(home.marks("b").0.unwrap() >= grandchild_end.unwrap());
    assert!(stderr(&b).contains("Heavy queue"), "{}", stderr(&b));
}

#[test]
fn unregistered_child_never_runs() {
    let home = Home::new();
    let out = finish(
        home.heavy(&[], home.hold("a", 10, &[]))
            .env("HOSTLER_TEST_KILL_BEFORE_GATE", "1")
            .spawn()
            .unwrap(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(libc::SIGKILL));
    }
    #[cfg(windows)]
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    // Once the launcher is gone too, the entry is reaped.
    wait_until("the entry to be reaped", || home.item("a").is_some());
    assert_eq!(home.item("a").unwrap().outcome, HeavyOutcome::Reaped);
    assert_eq!(home.marks("a"), (None, None));
}

#[test]
fn gate_exec_failure_exits_127() {
    let home = Home::new();
    let program = home.file("no-such-program");
    let out = finish(
        home.heavy(&[], vec![program.clone().into_os_string(), "x".into()])
            .spawn()
            .unwrap(),
    );
    assert_code(&out, 127);
    let expected = format!("[hostler] cannot run {}: ", program.display());
    assert!(stderr(&out).contains(&expected), "{}", stderr(&out));
    let file = program.display().to_string();
    let item = home
        .history()
        .into_iter()
        .find(|i| i.command.contains(&file))
        .unwrap();
    assert_eq!(
        (item.outcome, item.exit_code),
        (HeavyOutcome::Failed, Some(127))
    );
}

#[cfg(unix)]
#[test]
fn ordinary_command_runs_through_gate() {
    let home = Home::new();
    let out = finish(
        home.heavy(&[], vec!["echo".into(), "hello".into()])
            .spawn()
            .unwrap(),
    );
    assert_code(&out, 0);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
    let out = finish(
        home.heavy(&[], vec!["echo a && echo b".into()])
            .spawn()
            .unwrap(),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb\n");
}

/// The service goes away while `b` waits; its reattach is held up by the debug delay, so the
/// slot would be granted to it once the delay ends (a has finished by then).
fn waiter_in_delayed_reattach(
    home: &Home,
    flags: &[&str],
    a_ms: u64,
    delay_ms: u64,
) -> (Child, Child) {
    let a = home.heavy(&[], home.hold("a", a_ms, &[])).spawn().unwrap();
    home.wait_started("a");
    let b = home
        .heavy(flags, home.hold("b", 10, &[]))
        .env("HOSTLER_TEST_DELAY_REATTACH_MS", delay_ms.to_string())
        .spawn()
        .unwrap();
    home.wait_waiting(1);
    home.stop();
    (a, b)
}

#[cfg(unix)]
#[test]
fn ctrl_c_during_reattach_stops_the_command() {
    let home = Home::new();
    let (a, b) = waiter_in_delayed_reattach(&home, &[], 1500, 4000);
    std::thread::sleep(Duration::from_millis(300));
    let interrupted = Instant::now();
    // SAFETY: a plain kill of the test's own child process.
    assert_eq!(unsafe { libc::kill(b.id() as i32, libc::SIGINT) }, 0);
    let b = finish(b);
    assert_code(&b, 130);
    assert!(
        interrupted.elapsed() < Duration::from_secs(3),
        "Ctrl+C waited for the reattach: {:?}",
        interrupted.elapsed()
    );
    assert!(
        stderr(&b).contains("[hostler] Interrupted; the request was withdrawn."),
        "{}",
        stderr(&b)
    );
    assert_code(&finish(a), 0);
    assert_eq!(home.marks("b"), (None, None));
}

#[test]
fn max_wait_during_reattach_stops_the_command() {
    let home = Home::new();
    // 0.05 min = 3 s, well before the 6 s reattach delay ends.
    let (a, b) = waiter_in_delayed_reattach(&home, &["--max-wait", "0.05"], 3500, 6000);
    let b = finish(b);
    assert_code(&b, 75);
    assert!(
        stderr(&b).contains("[hostler] Gave up after 3 s in the heavy queue"),
        "{}",
        stderr(&b)
    );
    assert_code(&finish(a), 0);
    assert_eq!(home.marks("b"), (None, None));
}

#[test]
fn fail_open_without_process_identity_still_runs() {
    let home = Home::new();
    let out = finish(
        home.heavy(&[], home.hold("a", 10, &["4"]))
            .env("HOSTLER_TEST_NO_PROCESS_TABLE", "1")
            .spawn()
            .unwrap(),
    );
    assert_code(&out, 4);
    let err = stderr(&out);
    assert_eq!(err.matches("[hostler] Warning:").count(), 1, "{err}");
    assert!(err.contains("HOSTLER_TEST_NO_PROCESS_TABLE"), "{err}");
    assert!(home.marks("a").1.is_some());
    assert!(home.history().is_empty());
}

#[cfg(unix)]
#[test]
fn ctrl_c_between_grant_and_spawn_never_starts_the_command() {
    let home = Home::new();
    let a = home.heavy(&[], home.hold("a", 1000, &[])).spawn().unwrap();
    home.wait_started("a");
    let gate = home.file("b-may-spawn");
    let b = home
        .heavy(&[], home.hold("b", 10, &[]))
        .env("HOSTLER_TEST_PAUSE_BEFORE_SPAWN_UNTIL", &gate)
        .spawn()
        .unwrap();
    home.wait_waiting(1);
    assert_code(&finish(a), 0);
    // b holds the slot and blocks between its last stop check and the spawn until `gate`.
    let reached = home.file("b-may-spawn.reached");
    wait_until("b to reach the spawn", || reached.exists());
    // SAFETY: a plain kill of the test's own child process.
    assert_eq!(unsafe { libc::kill(b.id() as i32, libc::SIGINT) }, 0);
    // The kernel delivers it at once; tokio's signal driver (another runtime thread, not the
    // blocked one) records it for the wrapper's pending check.
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(&gate, "").unwrap();
    let b = finish(b);
    assert_code(&b, 130);
    assert!(
        stderr(&b).contains("interrupted before the command started"),
        "{}",
        stderr(&b)
    );
    assert_eq!(home.marks("b"), (None, None));
}

#[test]
fn withdrawal_before_started_never_runs_the_command() {
    let home = Home::new();
    let b = home
        .heavy(&[], home.hold("b", 10, &[]))
        .env("HOSTLER_TEST_DELAY_STARTED_MS", "3000")
        .spawn()
        .unwrap();
    // Granted at once; the wrapper has launched it but not yet reported `heavy.started`.
    let mut id = None;
    wait_until("b to be granted", || {
        id = home.snapshot().running.first().map(|e| e.id.clone());
        id.is_some()
    });
    let id = id.unwrap();
    let withdrawn = home
        .hostler()
        .args(["queue", "withdraw", "--id", &id])
        .output()
        .unwrap();
    assert!(withdrawn.status.success(), "{}", stderr(&withdrawn));
    let b = finish(b);
    assert_code(&b, 75);
    assert!(
        stderr(&b).contains("[hostler] The request was withdrawn by an operator; not running."),
        "{}",
        stderr(&b)
    );
    assert_eq!(home.marks("b"), (None, None));
    assert_eq!(home.item("b").unwrap().outcome, HeavyOutcome::Withdrawn);
}

/// The wrapper forwards the signal to the still-gated launcher's group, so the command never
/// starts. (Windows: console Ctrl+C cannot be sent to one test process; the decision is
/// unit-tested in `cli::spawn`.)
#[cfg(unix)]
#[test]
fn ctrl_c_while_started_is_pending_never_starts_the_command() {
    let home = Home::new();
    let b = home
        .heavy(&[], home.hold("b", 10, &[]))
        .env("HOSTLER_TEST_DELAY_STARTED_MS", "3000")
        .spawn()
        .unwrap();
    // Granted at once; the launcher waits at its gate while `heavy.started` is delayed.
    wait_until("b to be granted", || !home.snapshot().running.is_empty());
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: a plain kill of the test's own child process.
    assert_eq!(unsafe { libc::kill(b.id() as i32, libc::SIGINT) }, 0);
    let b = finish(b);
    assert_code(&b, 130);
    assert_eq!(home.marks("b"), (None, None));
}

/// Windows: the launcher records a Ctrl+C after the command started (debug hook, the console
/// handler's own path); the command never hears of it and runs on, so after the 5 s grace the
/// job is terminated and the run exits 130.
#[cfg(windows)]
#[test]
fn interrupted_command_that_runs_on_is_stopped_after_the_grace() {
    let home = Home::new();
    let started = Instant::now();
    let out = finish(
        home.heavy(&[], home.hold("a", 30_000, &[]))
            .env("HOSTLER_TEST_GATE_INTERRUPT_AFTER_MS", "500")
            .spawn()
            .unwrap(),
    );
    assert_code(&out, 130);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(5) && took < Duration::from_secs(25),
        "{took:?}"
    );
    let (start, end) = home.marks("a");
    assert!(start.is_some() && end.is_none(), "{start:?} {end:?}");
}

/// Windows: a command that finishes within the grace keeps its own exit code.
#[cfg(windows)]
#[test]
fn interrupted_command_that_exits_within_the_grace_keeps_its_code() {
    let home = Home::new();
    let out = finish(
        home.heavy(&[], home.hold("a", 1500, &["3"]))
            .env("HOSTLER_TEST_GATE_INTERRUPT_AFTER_MS", "300")
            .spawn()
            .unwrap(),
    );
    assert_code(&out, 3);
    assert!(home.marks("a").1.is_some());
}
