//! Plan P4 Task 4: a contract mismatch on the start paths (direction-aware replacement, R1-2,
//! R1-3, R1-6, D-Q1) and `sandbox_denied` (R1-1). Every service runs under its own
//! `HOSTLER_HOME` (a custom home: nothing here touches an OS registration). An "older" or
//! "newer" service is this binary with `HOSTLER_TEST_CONTRACT_VERSION` set on it alone.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{BIN, Home, stderr};
use hostler::client::{Client, Replace};
use hostler::contract::ErrorCode;
use hostler::contract::wire::Request;
use hostler::paths::Paths;
use hostler::service::{NoManager, ServiceManager};
use serde_json::{Value, json};

const OLDER: [(&str, &str); 1] = [("HOSTLER_TEST_CONTRACT_VERSION", "3")];
const NEWER: [(&str, &str); 1] = [("HOSTLER_TEST_CONTRACT_VERSION", "5")];

/// A `hostler service run` started with test variables of its own (the client never has them).
/// Dropped: stopped if it still runs, then reaped.
struct Service {
    child: Child,
    home: PathBuf,
}

impl Service {
    fn start(home: &Home, vars: &[(&str, &str)]) -> Service {
        let child = home
            .hostler()
            .envs(vars.iter().copied())
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let service = Service {
            child,
            home: home.home.clone(),
        };
        let pid = u64::from(service.child.id());
        assert!(
            wait_for(Duration::from_secs(15), || status(home)["data"]["pid"]
                .as_u64()
                == Some(pid)),
            "the test service never answered: {}",
            status(home)
        );
        service
    }

    fn pid(&self) -> u64 {
        u64::from(self.child.id())
    }

    /// Whether the process ended within `limit`.
    fn exits_within(&mut self, limit: Duration) -> bool {
        wait_for(limit, || self.child.try_wait().unwrap().is_some())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = std::process::Command::new(BIN)
                .env("HOSTLER_HOME", &self.home)
                .args(["service", "stop", "--force"])
                .output();
            if !self.exits_within(Duration::from_secs(10)) {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn run(home: &Home, args: &[&str]) -> Output {
    home.hostler().args(args).output().unwrap()
}

/// `hostler service status --json`: lifecycle only, so it reads any contract.
fn status(home: &Home) -> Value {
    serde_json::from_slice(&run(home, &["service", "status", "--json"]).stdout)
        .unwrap_or(Value::Null)
}

fn pid(home: &Home) -> Option<u64> {
    status(home)["data"]["pid"].as_u64()
}

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

/// The in-process start path, as `hostler service start` (`OnMismatch`) or an ordinary command
/// (`Never`) runs it; the error, or `None` when it connected.
async fn start_path(paths: &Paths, replace: Replace) -> Option<hostler::contract::HostlerError> {
    Client::connect_or_start_with(paths, &NoManager, BIN.as_ref(), replace)
        .await
        .err()
}

/// One heavy entry marked running in the store, held by the live test process.
fn seed_running_heavy(home: &Home) {
    use hostler::contract::heavy::HeavyAcquire;
    use hostler::procs::{SysProcessTable, current_identity};
    use hostler::store::Store;

    let store = Store::open(&home.paths().db).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let req = HeavyAcquire {
        id: id.clone(),
        kind: "gradle".into(),
        command: "./gradlew assemble".into(),
        cwd: "/work/app".into(),
        workspace_root: None,
        holder: current_identity(&SysProcessTable::new().unwrap()).unwrap(),
        reentrant_token: None,
        no_wait: false,
    };
    store.insert_waiting(&req, 0).unwrap();
    store.mark_running(&id, 0).unwrap();
}

/// A connection that says `hello` with `contract` (as an older or newer client would), then
/// sends `requests`; the answers in order.
async fn raw_calls(paths: &Paths, contract: u32, requests: Vec<Request>) -> Vec<Value> {
    use hostler::contract::wire::ClientFrame;
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let name = hostler::service::server::socket_name(&paths.socket).unwrap();
    let stream = Stream::connect(name).await.unwrap();
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let hello = Request::Hello {
        contract_version: contract,
        client: "test".into(),
    };
    let mut answers = Vec::new();
    for (id, request) in std::iter::once(hello).chain(requests).enumerate() {
        let id = id as u64 + 1;
        let frame = serde_json::to_string(&ClientFrame { id, request }).unwrap();
        write.write_all(frame.as_bytes()).await.unwrap();
        write.write_all(b"\n").await.unwrap();
        // Skip pushed events: only the answer to this id counts.
        loop {
            let line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
                .await
                .expect("an answer")
                .unwrap()
                .expect("an open connection");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                answers.push(value);
                break;
            }
        }
    }
    answers.remove(0);
    answers
}

/// Unlocks (creates) the database of an older service across the mismatch: a contract-3 hello.
async fn unlock_older(paths: &Paths) {
    use hostler::contract::secrets::Unlock;
    use hostler::secrets::value::SecretValue;

    let unlock = Request::SecretsUnlock(Unlock {
        password: SecretValue::new("a test password, not a secret".into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    });
    let answers = raw_calls(paths, 3, vec![unlock]).await;
    assert!(
        answers[0]["error"].is_null(),
        "unlock failed: {}",
        answers[0]
    );
}

/// D-Q1, R1-4: `service start` replaces an idle, locked, older service.
#[test]
fn older_idle_service_is_replaced() {
    let home = Home::new();
    let mut old = Service::start(&home, &OLDER);
    let before = status(&home);
    assert_eq!(before["data"]["contract_version"], 3);
    // The database is missing, so it is locked.
    assert_eq!(before["data"]["secrets_unlocked"], false);

    let out = run(&home, &["service", "start"]);
    assert!(out.status.success(), "service start: {}", stderr(&out));
    assert!(
        stderr(&out).contains("replaced an older hostler service (contract 3 → 4)"),
        "{}",
        stderr(&out)
    );
    let after = status(&home);
    assert_eq!(after["data"]["contract_version"], 4, "{after}");
    assert_ne!(after["data"]["pid"].as_u64(), Some(old.pid()));
    assert!(
        old.exits_within(Duration::from_secs(10)),
        "the old service still runs"
    );
}

/// D-Q1 (`REPLACE_ON_START = Never`), R1-4: an ordinary command never replaces; its recovery
/// is `hostler service start`, which does.
#[test]
fn ordinary_command_does_not_replace() {
    let home = Home::new();
    let old = Service::start(&home, &OLDER);
    let out = home
        .hostler()
        .args(["status", "--json"])
        .current_dir(home.dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = &json_of(&out)["error"];
    assert_eq!(error["code"], "contract_mismatch", "{error}");
    assert_eq!(error["recovery"], "hostler service start", "{error}");
    assert_eq!(pid(&home), Some(old.pid()), "the service was replaced");
    assert_eq!(status(&home)["data"]["contract_version"], 3);
}

/// R1-3: a busy older service is kept, and the recovery is `service install`, never `service
/// restart` (which would start the same old registered binary again).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_busy_service_is_kept() {
    let home = Home::new();
    seed_running_heavy(&home);
    let old = Service::start(&home, &OLDER);
    assert_eq!(status(&home)["data"]["running_heavy"], 1);

    let err = start_path(&home.paths(), Replace::OnMismatch)
        .await
        .expect("a busy service is kept");
    assert_eq!(err.code, ErrorCode::ContractMismatch);
    assert!(err.message.contains("has live work"), "{}", err.message);
    let running = err.details["busy"]["running"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{}", err.details);
    let recovery = err.recovery.unwrap();
    assert!(recovery.contains("hostler service install"), "{recovery}");
    assert!(!recovery.contains("service restart"), "{recovery}");

    let out = run(&home, &["service", "start"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("hostler service install"),
        "{}",
        stderr(&out)
    );
    assert_eq!(pid(&home), Some(old.pid()));
    assert_eq!(
        status(&home)["data"]["running_heavy"],
        1,
        "the task stopped"
    );
}

/// R1-6: an unlocked database is live work; the automatic path stops nothing, and the atomic
/// shutdown itself refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlocked_older_service_is_kept() {
    let home = Home::new();
    let old = Service::start(&home, &OLDER);
    let paths = home.paths();
    unlock_older(&paths).await;
    assert_eq!(status(&home)["data"]["secrets_unlocked"], true);

    let err = start_path(&paths, Replace::OnMismatch)
        .await
        .expect("an unlocked service is kept");
    assert_eq!(err.code, ErrorCode::ContractMismatch);
    assert_eq!(
        err.details["refusal"], "unlocked_or_unknown",
        "{}",
        err.details
    );
    assert!(
        err.message.contains("unlocked (or it cannot say)"),
        "{}",
        err.message
    );
    let out = run(&home, &["service", "start"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unlocked"), "{}", stderr(&out));
    assert_eq!(pid(&home), Some(old.pid()));
    // Still unlocked: nothing locked it.
    assert_eq!(status(&home)["data"]["secrets_unlocked"], true);

    // The atomic stop refuses by itself.
    let mut client = Client::connect_lifecycle(&paths).await.unwrap();
    let busy = client
        .call(Request::ServiceShutdown {
            if_idle: true,
            refuse_if_unlocked: true,
        })
        .await
        .unwrap_err();
    assert_eq!(busy.code, ErrorCode::ServiceBusy);
    assert_eq!(busy.details["secrets"], "unlocked", "{}", busy.details);
    assert_eq!(busy.recovery.as_deref(), Some("hostler service restart"));
    assert_eq!(pid(&home), Some(old.pid()));
    assert_eq!(status(&home)["data"]["secrets_unlocked"], true);
}

/// A 0.0.3-shaped lifecycle server: hello mismatches (contract 3); `service.status` has no
/// `secrets_unlocked`. It records every method it is sent.
async fn fake_old_service(
    paths: &Paths,
    methods: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<()> {
    use interprocess::local_socket::traits::tokio::Listener as _;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = hostler::service::server::bind(&paths.socket).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok(stream) = listener.accept().await else {
                return;
            };
            let methods = methods.clone();
            tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(stream);
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let frame: Value = serde_json::from_str(&line).unwrap();
                    let method = frame["method"].as_str().unwrap_or("").to_owned();
                    methods.lock().unwrap().push(method.clone());
                    let id = frame["id"].clone();
                    let answer = match method.as_str() {
                        "hello" => json!({ "id": id, "error": {
                            "code": "contract_mismatch",
                            "message": "the running hostler service speaks contract 3",
                            "details": {
                                "client_contract": 4,
                                "service_contract": 3,
                                "service_version": "0.0.3",
                            },
                            "recovery": "hostler service restart",
                        }}),
                        "service.status" => json!({ "id": id, "result": {
                            "service_version": "0.0.3",
                            "contract_version": 3,
                            "pid": std::process::id(),
                            "running_heavy": 0,
                            "waiting_heavy": 0,
                        }}),
                        _ => json!({ "id": id, "result": { "stopping": true } }),
                    };
                    let mut text = answer.to_string();
                    text.push('\n');
                    if write.write_all(text.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    })
}

/// R1-6: a 0.0.3 service cannot say whether it is unlocked, so it is never stopped
/// automatically.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_without_secrets_field_is_kept() {
    let home = Home::new();
    let paths = home.paths();
    let methods = Arc::new(Mutex::new(Vec::new()));
    let fake = fake_old_service(&paths, methods.clone()).await;

    for replace in [Replace::OnMismatch, Replace::Never] {
        let err = start_path(&paths, replace).await.expect("a mismatch");
        assert_eq!(err.code, ErrorCode::ContractMismatch, "{err:?}");
        if replace == Replace::OnMismatch {
            assert_eq!(
                err.details["refusal"], "unlocked_or_unknown",
                "{}",
                err.details
            );
        }
    }
    let methods = methods.lock().unwrap().clone();
    assert!(
        methods.contains(&"service.status".to_owned()),
        "{methods:?}"
    );
    assert!(
        !methods.iter().any(|m| m == "service.shutdown"),
        "the client sent a shutdown: {methods:?}"
    );
    // Gone before the drop guard's own stop, which would wait for this (live) pid.
    fake.abort();
    let _ = fake.await;
}

/// R1-2: an older client never stops a newer service.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newer_service_is_never_stopped() {
    let home = Home::new();
    let newer = Service::start(&home, &NEWER);
    let err = start_path(&home.paths(), Replace::OnMismatch)
        .await
        .expect("a newer service is kept");
    assert_eq!(err.code, ErrorCode::ContractMismatch);
    assert_eq!(err.details["refusal"], "newer_service", "{}", err.details);
    assert!(
        err.message.contains("older than the running service"),
        "{}",
        err.message
    );
    assert!(
        err.recovery
            .as_deref()
            .unwrap()
            .contains("cargo install hostler"),
        "{:?}",
        err.recovery
    );

    let out = run(&home, &["service", "start"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("older than the running service"),
        "{}",
        stderr(&out)
    );
    let out = home
        .hostler()
        .args(["status", "--json"])
        .current_dir(home.dir.path())
        .output()
        .unwrap();
    let error = &json_of(&out)["error"];
    assert_eq!(error["code"], "contract_mismatch");
    assert!(
        error["recovery"]
            .as_str()
            .unwrap()
            .contains("cargo install hostler"),
        "{error}"
    );
    assert_eq!(pid(&home), Some(newer.pid()));
    assert_eq!(status(&home)["data"]["contract_version"], 5);
}

/// Holds never block a stop; a TTL hold survives the replacement in the store.
#[test]
fn holds_do_not_block_replacement() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    let out = home.hold(&repo, &["screen", "--ttl", "600", "--json"]);
    assert!(out.status.success(), "hold: {}", stderr(&out));
    assert!(run(&home, &["service", "stop"]).status.success());

    // The older service reloads the hold from the store.
    let mut old = Service::start(&home, &OLDER);
    let out = run(&home, &["service", "start"]);
    assert!(out.status.success(), "service start: {}", stderr(&out));
    assert_ne!(pid(&home), Some(old.pid()));
    assert!(old.exits_within(Duration::from_secs(10)));
    let holds: usize = home
        .snapshot()
        .resources
        .iter()
        .map(|state| state.claims.len())
        .sum();
    assert_eq!(holds, 1, "the hold did not survive");
}

/// R1-6: an explicit restart keeps working on an unlocked database and says it locks it.
#[test]
fn explicit_restart_warns_and_stops_when_unlocked() {
    let home = Home::new();
    home.start_service();
    home.unlock_new("a test password, not a secret");
    let before = status(&home);
    assert_eq!(before["data"]["secrets_unlocked"], true);

    let out = run(&home, &["service", "restart"]);
    assert!(out.status.success(), "restart: {}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "this locks the secrets database; a person unlocks it again with: hostler unlock"
        ),
        "{}",
        stderr(&out)
    );
    let after = status(&home);
    assert_ne!(after["data"]["pid"], before["data"]["pid"]);
    assert_eq!(after["data"]["secrets_unlocked"], false);

    // A locked database: no warning.
    let out = run(&home, &["service", "stop"]);
    assert!(out.status.success());
    assert!(!stderr(&out).contains("this locks"), "{}", stderr(&out));
}

// ---- sandbox_denied (R1-1) ----

/// A manager that must never be asked.
#[cfg_attr(not(unix), allow(dead_code))] // its tests are Unix-only
#[derive(Clone)]
struct NeverAsked;

impl ServiceManager for NeverAsked {
    fn is_installed(&self) -> bool {
        panic!("a sandbox_denied start path asked the manager")
    }

    fn start(&self) -> anyhow::Result<()> {
        panic!("a sandbox_denied start path started the manager")
    }
}

/// A "hostler" that only records that it was started.
#[cfg(unix)]
fn marker_exe(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = dir.join("spawned");
    let exe = dir.join("fake-hostler");
    std::fs::write(&exe, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    (exe, marker)
}

/// How many times a service of this home logged its start.
#[cfg_attr(not(unix), allow(dead_code))] // its tests are Unix-only
fn start_lines(home: &Path) -> usize {
    let Ok(dir) = std::fs::read_dir(home.join("logs")) else {
        return 0;
    };
    dir.filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .map(|text| text.matches(" listening on ").count())
        .sum()
}

/// The `sandbox_denied` recovery naming `dir` (P4-L18).
#[cfg_attr(not(unix), allow(dead_code))] // its tests are Unix-only
fn sandbox_recovery(dir: &Path) -> String {
    format!(
        "allow hostler's socket in the agent's sandbox, or run hostler outside the sandbox; if no \
         sandbox is involved, check the permissions of {}; see: hostler guide sandbox",
        dir.display()
    )
}

/// Restores a directory's mode on drop.
#[cfg(unix)]
struct ModeGuard(PathBuf);

#[cfg(unix)]
impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

/// R1-1, the real path: a socket directory this process may not search fails the owner
/// check's `symlink_metadata` with `EACCES`. Never "not running", never a start attempt.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandbox_denied_on_metadata() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root may search any directory");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Over 100 bytes: the socket moves to /tmp/hostler-<uid>-<hash>/.
    let home = Home {
        home: dir.path().join("h".repeat(110)),
        dir,
    };
    let paths = home.paths();
    let hostler::paths::SocketName::File(socket) = paths.socket.clone() else {
        unreachable!()
    };
    let socket_dir = socket.parent().unwrap().to_path_buf();
    assert!(socket_dir.starts_with("/tmp"), "{}", socket_dir.display());
    home.start_service();
    let service_pid = pid(&home).unwrap();
    let starts = start_lines(&home.home);
    assert_eq!(starts, 1);

    let deny = ModeGuard(socket_dir.clone());
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = run(&home, &["service", "status", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let error = &json_of(&out)["error"];
    assert_eq!(error["details"]["reason"], "sandbox_denied", "{error}");
    // The directory that may not be searched is named (P4-L18).
    assert_eq!(error["recovery"], sandbox_recovery(&socket_dir), "{error}");
    assert_eq!(error["details"]["dir"], socket_dir.display().to_string());
    let out = run(&home, &["service", "status"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("hostler cannot reach its service from here"),
        "{text}"
    );
    assert!(!text.contains("not running"), "{text}");

    let out = home
        .hostler()
        .args(["status", "--json"])
        .current_dir(home.dir.path())
        .output()
        .unwrap();
    assert_eq!(
        json_of(&out)["error"]["details"]["reason"],
        "sandbox_denied"
    );

    // In-process, every start path: no manager call, no spawn.
    let (exe, marker) = marker_exe(home.dir.path());
    for replace in [Replace::Never, Replace::OnMismatch] {
        let err = Client::connect_or_start_with(&paths, &NeverAsked, &exe, replace)
            .await
            .err()
            .expect("denied");
        assert_eq!(err.details["reason"], "sandbox_denied", "{err:?}");
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "a start path spawned a service");

    drop(deny);
    assert!(hostler::service::lock::is_held(&paths.service_lock).unwrap());
    assert_eq!(pid(&home), Some(service_pid));
    assert_eq!(start_lines(&home.home), starts, "a second service started");
    // The fallback directory outlives the temp home.
    drop(home);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

/// R1-1, the seatbelt's case: the socket's `stat` passes and `connect()` fails with `EPERM`
/// (the debug seam; Task 0 and Task 9 cover the real sandboxes). Never "not running", never a
/// second service; `hostler run --heavy` fails open with the sandbox's own words. (With no
/// socket file at all, `stat` says "not running" truthfully, as it does under the seatbelt.)
#[cfg(unix)]
#[test]
fn sandbox_denied_on_connect() {
    let home = Home::new();
    home.start_service();
    let service_pid = pid(&home);
    assert_eq!(start_lines(&home.home), 1);
    let denied = |args: &[&str]| {
        home.hostler()
            .env("HOSTLER_TEST_CONNECT_ERRNO", "EPERM")
            .args(args)
            .current_dir(home.dir.path())
            .output()
            .unwrap()
    };

    for args in [&["status", "--json"][..], &["service", "status", "--json"]] {
        let out = denied(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        let error = &json_of(&out)["error"];
        assert_eq!(
            error["details"]["reason"], "sandbox_denied",
            "{args:?}: {error}"
        );
        assert_eq!(
            error["message"],
            "this process may not reach hostler's socket (an agent sandbox?)"
        );
        // Nothing denies a search: the socket's own directory is named (P4-L18).
        assert_eq!(error["recovery"], sandbox_recovery(&home.home), "{args:?}");
    }
    let out = denied(&["service", "status"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("not running"));
    let out = denied(&["service", "start"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("hostler guide sandbox"),
        "{}",
        stderr(&out)
    );
    let out = denied(&["run", "--heavy", "--", "echo", "ran"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ran");
    assert!(
        stderr(&out).contains("an agent sandbox"),
        "{}",
        stderr(&out)
    );
    // The fail-open warning names the fix (ruling P4-L13), never "start the service".
    assert!(
        stderr(&out).contains(&format!(
            "[hostler] To fix: {}",
            sandbox_recovery(&home.home)
        )),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("service start"), "{}", stderr(&out));

    assert_eq!(pid(&home), service_pid);
    assert_eq!(start_lines(&home.home), 1, "a second service started");
}

// ---- Fix round 1 (review t4-r1, ruling P4-L15) ----

/// Answers for `HOSTLER_TEST_TTY`: create a database with `password`.
fn create_tty(home: &Home, password: &str) -> PathBuf {
    let path = home
        .dir
        .path()
        .join(format!("tty-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, format!("y\n{password}\n{password}\n")).unwrap();
    path
}

/// R1-1: an unlock admitted before an automatic shutdown decides, and paused before it takes
/// the vault (`HOSTLER_TEST_UNLOCK_PAUSE`), is live work: `service.shutdown { if_idle: true,
/// refuse_if_unlocked: true }` refuses, the service stays up, and the unlock then completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_unlock_refuses_the_automatic_shutdown() {
    let home = Home::new();
    let pause = home.dir.path().join("unlock-pause");
    std::fs::write(&pause, b"").unwrap();
    let admitted = home.dir.path().join("unlock-pause.admitted");
    let out = home
        .hostler()
        .env("HOSTLER_TEST_UNLOCK_PAUSE", &pause)
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let service_pid = pid(&home);
    assert_eq!(status(&home)["data"]["secrets_unlocked"], false);

    let mut unlock = home
        .hostler()
        .arg("unlock")
        .env(
            "HOSTLER_TEST_TTY",
            create_tty(&home, "a test password, not a secret"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(30), || admitted.exists()),
        "the unlock was never admitted"
    );
    // Admitted, not yet at the vault: the flag is still locked, the vault mutex free.
    let mut client = Client::connect_lifecycle(&home.paths()).await.unwrap();
    let busy = client
        .call(Request::ServiceShutdown {
            if_idle: true,
            refuse_if_unlocked: true,
        })
        .await
        .unwrap_err();
    assert_eq!(busy.code, ErrorCode::ServiceBusy, "{busy:?}");
    assert_eq!(busy.details["secrets"], "unlocked", "{}", busy.details);
    drop(client);
    assert_eq!(pid(&home), service_pid, "the service stopped");
    // The probe says so too.
    assert_eq!(status(&home)["data"]["secrets_unlocked"], true);

    std::fs::remove_file(&pause).unwrap();
    assert!(
        wait_for(Duration::from_secs(60), || unlock
            .try_wait()
            .unwrap()
            .is_some()),
        "the unlock never finished"
    );
    let finished = unlock.wait_with_output().unwrap();
    assert!(finished.status.success(), "unlock: {}", stderr(&finished));
    assert_eq!(pid(&home), service_pid);
    assert_eq!(status(&home)["data"]["secrets_unlocked"], true);
    // Admission was reopened: a later automatic stop still refuses, for the lock itself.
    let mut client = Client::connect_lifecycle(&home.paths()).await.unwrap();
    let busy = client
        .call(Request::ServiceShutdown {
            if_idle: true,
            refuse_if_unlocked: true,
        })
        .await
        .unwrap_err();
    assert_eq!(busy.details["secrets"], "unlocked");
}

/// R1-2: a sandbox that may not search the socket's directory chain fails in `Paths::resolve`
/// (`ensure_private_dir`), before any connect: `service status` (JSON and human), plain `status`
/// and `run --heavy`'s fail-open warning all say `sandbox_denied` with its fix; nothing starts.
#[cfg(unix)]
#[test]
fn sandbox_denied_on_state_dirs() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root may search any directory");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("p");
    let home = Home {
        home: parent.join("home"),
        dir,
    };
    let paths = home.paths();
    let hostler::paths::SocketName::File(socket) = paths.socket.clone() else {
        unreachable!()
    };
    assert!(socket.starts_with(&home.home), "{}", socket.display());
    let elsewhere = tempfile::tempdir().unwrap();
    let in_sandbox = |args: &[&str]| {
        home.hostler()
            .args(args)
            .current_dir(elsewhere.path())
            .output()
            .unwrap()
    };
    // A checkout outside the denied directory, for the claim commands.
    let repo = home.make_repo("app");

    let deny = ModeGuard(parent.clone());
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
    // The denied directory is `parent`, the one that may not be searched (P4-L18).
    let recovery = sandbox_recovery(&parent);
    for args in [&["service", "status", "--json"][..], &["status", "--json"]] {
        let out = in_sandbox(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        let error = &json_of(&out)["error"];
        assert_eq!(
            error["details"]["reason"], "sandbox_denied",
            "{args:?}: {error}"
        );
        assert_eq!(error["recovery"], recovery, "{args:?}");
        assert_eq!(
            error["details"]["socket"],
            socket.display().to_string(),
            "{args:?}"
        );
        assert_eq!(error["details"]["dir"], parent.display().to_string());
    }
    // P4 R2-2: the claim and device families keep the classification too.
    let out = home
        .hostler()
        .args(["hold", "screen", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = &json_of(&out)["error"];
    assert_eq!(
        error["details"]["reason"], "sandbox_denied",
        "hold: {error}"
    );
    assert_eq!(error["recovery"], recovery, "hold");
    let out = in_sandbox(&["devices", "list", "--json"]);
    assert!(!out.status.success());
    let error = &json_of(&out)["error"];
    assert_eq!(
        error["details"]["reason"], "sandbox_denied",
        "devices: {error}"
    );
    assert_eq!(error["recovery"], recovery, "devices");
    let out = in_sandbox(&["service", "status"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("hostler cannot reach its service from here"),
        "{text}"
    );
    assert!(!text.contains("not running"), "{text}");
    let out = in_sandbox(&["status"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("hostler guide sandbox"),
        "{}",
        stderr(&out)
    );
    let out = in_sandbox(&["run", "--heavy", "--", "echo", "ran"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ran");
    assert!(
        stderr(&out).contains(&format!("[hostler] To fix: {recovery}")),
        "{}",
        stderr(&out)
    );

    drop(deny);
    assert_eq!(run(&home, &["service", "status"]).status.code(), Some(3));
    assert_eq!(start_lines(&home.home), 0, "a service was started");
}

/// R1-3: a heavy run whose release finds the socket denied (its service went away, and the
/// reconnect hits `EACCES`) gives up at once instead of retrying for the 10 s budget.
#[cfg(unix)]
#[test]
fn heavy_release_does_not_retry_sandbox_denied() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root may search any directory");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Over 100 bytes: the socket's directory is its own, under /tmp.
    let home = Home {
        home: dir.path().join("h".repeat(110)),
        dir,
    };
    let paths = home.paths();
    let hostler::paths::SocketName::File(socket) = paths.socket.clone() else {
        unreachable!()
    };
    let socket_dir = socket.parent().unwrap().to_path_buf();
    home.start_service();
    let go = home.dir.path().join("go");
    let ended = home.dir.path().join("ended");
    let script = format!(
        "while [ ! -e '{go}' ]; do sleep 0.05; done; touch '{ended}'",
        go = go.display(),
        ended = ended.display()
    );
    let run = home
        .hostler()
        .args(["run", "--heavy", "--", "sh", "-c", &script])
        .current_dir(home.dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_for(
            Duration::from_secs(15),
            || status(&home)["data"]["running_heavy"] == 1
        ),
        "the run never got its slot"
    );
    // The service goes away, and from here on the socket is denied.
    assert!(run_ok(&home, &["service", "stop", "--force"]));
    let deny = ModeGuard(socket_dir.clone());
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::write(&go, b"").unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || ended.exists()),
        "the command never ended"
    );
    let command_ended = Instant::now();
    let mut run = run;
    let exited = wait_for(Duration::from_secs(20), || {
        run.try_wait().unwrap().is_some()
    });
    let release_took = command_ended.elapsed();
    let out = run.wait_with_output().unwrap();
    assert!(exited, "the run never exited");
    // Retrying would take the whole 10 s budget; at least 2 s under it.
    assert!(
        release_took < Duration::from_secs(8),
        "the release retried a sandbox denial for {release_took:?}: {}",
        stderr(&out)
    );
    drop(deny);
    assert_eq!(start_lines(&home.home), 1, "a service was started");
    drop(home);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[cfg_attr(not(unix), allow(dead_code))] // its tests are Unix-only
fn run_ok(home: &Home, args: &[&str]) -> bool {
    run(home, args).status.success()
}

/// P4 Task 9 Step 4: `service start` keeps a same-contract service of an older version, and
/// says so in one line naming `hostler doctor --fix` and the typed restart.
#[test]
fn service_start_names_an_older_same_contract_service() {
    let home = Home::new();
    let old = Service::start(&home, &[("HOSTLER_TEST_SERVICE_VERSION", "0.0.1")]);
    let out = run(&home, &["service", "start"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stderr(&out);
    assert_eq!(
        text.lines()
            .filter(|l| l.contains("older than this hostler"))
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("hostler doctor --fix"), "{text}");
    assert!(text.contains("hostler service restart"), "{text}");
    assert_eq!(pid(&home), Some(old.pid()), "the service was restarted");
}
