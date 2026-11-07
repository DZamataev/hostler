//! The service process, the client and the lifecycle protocol against the real binary
//! (plan Task 6). Every test has its own `HOSTLER_HOME` and stops its services on drop.

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use hostler::client::Client;
use hostler::contract::heavy::HeavyAcquire;
use hostler::contract::wire::Request;
use hostler::contract::{CONTRACT_VERSION, ErrorCode};
use hostler::paths::Paths;
use hostler::procs::{SysProcessTable, current_identity};
use hostler::service::{NoManager, ServiceManager};
use hostler::store::Store;
use serde_json::Value;
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

    /// `hostler service run` as a child of the test, with extra environment.
    fn service(&self, env: &[(&str, &str)]) -> Child {
        let mut c = self.hostler();
        c.args(["service", "run"])
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = c.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.run(&["service", "status"]).status.code() != Some(0) {
            assert!(Instant::now() < deadline, "the service never came up");
            std::thread::sleep(Duration::from_millis(50));
        }
        child
    }

    fn status_json(&self) -> Value {
        let out = self.run(&["service", "status", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn pid(&self) -> u64 {
        self.status_json()["data"]["pid"]
            .as_u64()
            .expect("a running service")
    }

    /// A running heavy entry held by this test process (alive for the whole test).
    fn seed_running_entry(&self) -> String {
        let paths = self.paths();
        let store = Store::open(&paths.db).unwrap();
        let holder = current_identity(&SysProcessTable::new().unwrap()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let req = HeavyAcquire {
            id: id.clone(),
            kind: "gradle".into(),
            command: "./gradlew assemble".into(),
            cwd: "/work/app".into(),
            workspace_root: None,
            holder,
            reentrant_token: None,
            no_wait: false,
        };
        store.insert_waiting(&req, 0).unwrap();
        store.mark_running(&id, 0).unwrap();
        id
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["service", "stop", "--force"]);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn first_call_auto_starts_service() {
    let home = Home::new();
    let before = home.run(&["service", "status"]);
    assert_eq!(before.status.code(), Some(3));
    assert!(stdout(&before).contains("not running"));

    let paths = home.paths();
    let mut client = Client::connect_or_start_with(&paths, &NoManager, BIN.as_ref())
        .await
        .unwrap();
    let status = client.call(Request::ServiceStatus).await.unwrap();
    assert_eq!(status["contract_version"], CONTRACT_VERSION);
    assert_eq!(home.pid(), status["pid"].as_u64().unwrap());
}

#[test]
fn two_concurrent_starts_leave_one_service() {
    let home = Home::new();
    let starts: Vec<Child> = (0..2)
        .map(|_| {
            home.hostler()
                .args(["service", "start"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<Output> = starts
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    for out in &outputs {
        assert!(out.status.success(), "{}", stderr(out));
    }
    let pid = home.pid();
    for out in &outputs {
        assert_eq!(
            stdout(out).trim(),
            format!("hostler service is running (pid {pid})")
        );
    }
}

#[test]
fn stop_then_status_reports_stopped() {
    let home = Home::new();
    assert!(home.run(&["service", "start"]).status.success());
    let stop = home.run(&["service", "stop"]);
    assert!(stop.status.success(), "{}", stderr(&stop));
    assert!(stdout(&stop).contains("stopped"));
    let status = home.run(&["service", "status"]);
    assert_eq!(status.status.code(), Some(3));
    assert!(stdout(&status).contains("not running"));
    assert_eq!(home.status_json()["error"]["code"], "service_unavailable");
}

#[test]
fn stop_refused_while_entry_running_without_force() {
    let home = Home::new();
    let id = home.seed_running_entry();
    assert!(home.run(&["service", "start"]).status.success());
    assert_eq!(home.status_json()["data"]["running_heavy"], 1);

    let refused = home.run(&["service", "stop"]);
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr(&refused);
    assert!(text.contains(&id), "{text}");
    assert!(text.contains("./gradlew assemble"), "{text}");
    assert!(text.contains("--force"), "{text}");
    assert!(home.run(&["service", "status"]).status.success());

    let forced = home.run(&["service", "stop", "--force"]);
    assert!(forced.status.success(), "{}", stderr(&forced));
    assert_eq!(home.run(&["service", "status"]).status.code(), Some(3));
}

#[cfg(unix)]
#[test]
fn long_home_service_binds() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("h");
    let pad = 150usize.saturating_sub(base.as_os_str().len() + 1);
    let long_home = base.join("x".repeat(pad));
    assert_eq!(long_home.as_os_str().len(), 150);
    let tmpdir = dir.path().join("t".repeat(120));
    std::fs::create_dir_all(&tmpdir).unwrap();
    let home = Home {
        home: long_home,
        _dir: dir,
    };

    let start = home
        .hostler()
        .env("TMPDIR", &tmpdir)
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(start.status.success(), "{}", stderr(&start));
    assert!(home.status_json()["ok"].as_bool().unwrap());
    let hostler::paths::SocketName::File(socket) = home.paths().socket else {
        unreachable!()
    };
    assert!(socket.starts_with("/tmp"), "{}", socket.display());
    assert!(home.run(&["service", "stop"]).status.success());
    // After the drop guard's own `stop`, which re-creates the private socket directory.
    drop(home);
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn mismatched_client_gets_contract_mismatch_not_unavailable() {
    let home = Home::new();
    let mut service = home.service(&[("HOSTLER_TEST_CONTRACT_VERSION", "999")]);
    let paths = home.paths();

    let err = match Client::connect_or_start_with(&paths, &NoManager, BIN.as_ref()).await {
        Ok(_) => panic!("a contract-999 service accepted a contract-1 client"),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::ContractMismatch);
    assert_eq!(err.details["service_contract"], 999);
    assert_eq!(err.recovery.as_deref(), Some("hostler service restart"));

    // Without hello, a versioned call is refused the same way.
    let mut raw = Client::connect_lifecycle(&paths).await.unwrap();
    let err = raw.call(Request::QueueSnapshot).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ContractMismatch);

    // Lifecycle calls still work, and no second service was started.
    assert_eq!(home.pid(), u64::from(service.id()));
    assert!(home.run(&["service", "stop"]).status.success());
    assert!(service.wait().unwrap().success());
}

#[test]
fn restart_replaces_idle_mismatched_service() {
    let home = Home::new();
    let mut old = home.service(&[("HOSTLER_TEST_CONTRACT_VERSION", "999")]);
    assert_eq!(home.status_json()["data"]["contract_version"], 999);

    let restart = home.run(&["service", "restart"]);
    assert!(restart.status.success(), "{}", stderr(&restart));
    assert!(old.wait().unwrap().success());
    let status = home.status_json();
    assert_eq!(status["data"]["contract_version"], CONTRACT_VERSION);
    assert_ne!(status["data"]["pid"].as_u64(), Some(u64::from(old.id())));
}

#[test]
fn restart_refuses_busy_mismatched_service() {
    let home = Home::new();
    let id = home.seed_running_entry();
    let mut old = home.service(&[("HOSTLER_TEST_CONTRACT_VERSION", "999")]);

    let restart = home.run(&["service", "restart"]);
    assert_eq!(restart.status.code(), Some(1));
    assert!(stderr(&restart).contains(&id), "{}", stderr(&restart));
    assert_eq!(home.pid(), u64::from(old.id()));
    assert_eq!(home.status_json()["data"]["contract_version"], 999);

    assert!(home.run(&["service", "stop", "--force"]).status.success());
    assert!(old.wait().unwrap().success());
}

#[derive(Clone)]
struct PanicManager;

impl ServiceManager for PanicManager {
    fn is_installed(&self) -> bool {
        true
    }

    fn start(&self) -> anyhow::Result<()> {
        panic!("a custom home must never use the OS service manager");
    }
}

#[tokio::test]
async fn custom_home_never_uses_manager() {
    let home = Home::new();
    let paths = home.paths();
    assert!(paths.custom_home);
    let mut client = Client::connect_or_start_with(&paths, &PanicManager, BIN.as_ref())
        .await
        .unwrap();
    assert!(client.call(Request::ServiceStatus).await.is_ok());
}

#[derive(Clone)]
struct FailingManager;

impl ServiceManager for FailingManager {
    fn is_installed(&self) -> bool {
        true
    }

    fn start(&self) -> anyhow::Result<()> {
        anyhow::bail!("launchctl kickstart failed")
    }
}

#[tokio::test]
async fn installed_manager_failure_never_spawns() {
    let home = Home::new();
    // A default-home view of the same directory: the registration applies.
    let paths = Paths::under(home.home.clone(), false);
    paths.ensure_dirs().unwrap();
    let err = match Client::connect_or_start_with(&paths, &FailingManager, BIN.as_ref()).await {
        Ok(_) => panic!("no service may run"),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::ServiceUnavailable);
    assert_eq!(err.details["reason"], "manager_start_failed");
    assert!(
        err.message.contains("launchctl kickstart failed"),
        "{}",
        err.message
    );
    assert_eq!(err.recovery.as_deref(), Some("hostler service status"));
    // Nothing was spawned instead.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(home.run(&["service", "status"]).status.code(), Some(3));
}

/// A listener that accepts and never answers: every hello times out. The whole start-and-retry
/// window still ends 5 s after the start attempt.
#[cfg(unix)]
#[tokio::test]
async fn start_retry_window_is_five_seconds_in_total() {
    use interprocess::local_socket::traits::tokio::Listener as _;

    let home = Home::new();
    let paths = home.paths();
    let listener = hostler::service::server::bind(&paths.socket).unwrap();
    let silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok(stream) = listener.accept().await {
            held.push(stream);
        }
    });

    let started = Instant::now();
    // `true` stands in for a hostler binary whose service never comes up.
    let result = Client::connect_or_start_with(&paths, &NoManager, "true".as_ref()).await;
    let elapsed = started.elapsed();
    let err = match result {
        Ok(_) => panic!("a silent listener cannot complete hello"),
        Err(e) => e,
    };
    assert_eq!(err.details["reason"], "start_timeout");
    // The first hello (2 s) plus the 5 s window; per-attempt bounds used to add up to 2 s more.
    assert!(elapsed >= Duration::from_millis(6900), "{elapsed:?}");
    assert!(elapsed <= Duration::from_millis(7800), "{elapsed:?}");
    silent.abort();
}

/// A state directory other users can read: every `service` command reports it, `--json` as an
/// Envelope, with the `chmod` that fixes it.
#[cfg(unix)]
#[test]
fn open_home_is_reported_with_its_fix() {
    use std::os::unix::fs::PermissionsExt;

    let home = Home::new();
    std::fs::create_dir_all(&home.home).unwrap();
    std::fs::set_permissions(&home.home, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fix = format!("chmod 700 {}", home.home.display());

    let out = home.run(&["service", "status", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&out.stdout).expect("an Envelope on stdout");
    assert_eq!(envelope["ok"], false);
    assert!(envelope["data"].is_null());
    assert_eq!(envelope["error"]["code"], "internal");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("accessible by other users"), "{message}");
    assert_eq!(envelope["error"]["recovery"], fix.as_str());

    for args in [
        &["service", "status"][..],
        &["service", "stop"],
        &["service", "start"],
    ] {
        let out = home.run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&out).contains(&format!("To fix: {fix}")),
            "{args:?}: {}",
            stderr(&out)
        );
    }
    // Restore so the drop guard can clean up.
    std::fs::set_permissions(&home.home, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// `stop` returns only once the service process has exited, so a start right after it (e.g. an
/// OS manager's) never meets the old process still on its way out.
#[test]
fn stop_returns_after_the_process_exited() {
    let home = Home::new();
    let mut service = home.service(&[]);
    let stop = home.run(&["service", "stop"]);
    assert!(stop.status.success(), "{}", stderr(&stop));
    let exited = service.try_wait().unwrap();
    assert!(exited.is_some_and(|s| s.success()), "{exited:?}");
}

/// Reads `reader` to EOF on a thread; true if EOF came within `limit`.
fn eof_within(mut reader: impl std::io::Read + Send + 'static, limit: Duration) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = tx.send(reader.read_to_end(&mut sink).is_ok());
    });
    rx.recv_timeout(limit).unwrap_or(false)
}

/// An agent capturing `hostler`'s output must see EOF when the command exits, even though it
/// auto-started a long-lived service: the service holds none of the caller's handles or fds.
#[test]
fn started_service_does_not_hold_the_callers_output_open() {
    let home = Home::new();
    let mut start = home.hostler();
    start
        .args(["service", "start"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Unix: also pass an extra pipe end without CLOEXEC (fd 9), as some harnesses do.
    #[cfg(unix)]
    let (extra_reader, extra_writer) = std::io::pipe().unwrap();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let fd = extra_writer.as_raw_fd();
        // SAFETY: dup2 is async-signal-safe; the copy at fd 9 has no CLOEXEC flag.
        unsafe {
            start.pre_exec(move || {
                if libc::dup2(fd, 9) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = start.spawn().unwrap();
    #[cfg(unix)]
    drop(extra_writer);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    assert!(child.wait().unwrap().success());

    assert!(
        eof_within(stdout, Duration::from_secs(5)),
        "the service holds the caller's stdout"
    );
    assert!(
        eof_within(stderr, Duration::from_secs(5)),
        "the service holds the caller's stderr"
    );
    #[cfg(unix)]
    assert!(
        eof_within(extra_reader, Duration::from_secs(5)),
        "the service holds an fd the caller passed without CLOEXEC"
    );
    assert!(
        home.run(&["service", "status"]).status.success(),
        "the service must still run"
    );
}

/// An fd the caller passed above a soft RLIMIT_NOFILE that was lowered after it was opened (the
/// reviewer's case: fd 1000, soft limit 256) must not reach the service either.
#[cfg(unix)]
#[test]
fn started_service_does_not_keep_an_fd_above_a_lowered_limit() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    const HIGH_FD: libc::c_int = 1000;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid, writable rlimit.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    if limit.rlim_max != libc::RLIM_INFINITY && limit.rlim_max <= HIGH_FD as libc::rlim_t {
        eprintln!("skipped: the hard fd limit {} is too low", limit.rlim_max);
        return;
    }
    let raised = libc::rlimit {
        rlim_cur: (HIGH_FD as libc::rlim_t + 1).max(limit.rlim_cur),
        rlim_max: limit.rlim_max,
    };
    let lowered = libc::rlimit {
        rlim_cur: 256,
        rlim_max: limit.rlim_max,
    };

    let home = Home::new();
    let (reader, writer) = std::io::pipe().unwrap();
    let fd = writer.as_raw_fd();
    let mut start = home.hostler();
    start.args(["service", "start"]);
    // SAFETY: only setrlimit and dup2 (plain syscalls) run between fork and exec.
    unsafe {
        start.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) != 0
                || libc::dup2(fd, HIGH_FD) == -1
                || libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = start.status().unwrap();
    drop(writer);
    assert!(status.success());
    assert!(
        eof_within(reader, Duration::from_secs(5)),
        "the service keeps fd {HIGH_FD}, above the lowered soft limit"
    );
    assert!(
        home.run(&["service", "status"]).status.success(),
        "the service must still run"
    );
}
