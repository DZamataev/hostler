//! Plan P4 Task 6: `hostler doctor [--fix] [--json]`.
//!
//! Every service runs under its own temp home. Tests that need an OS registration run doctor
//! in-process (`doctor::run_with`) with a fake registrar over the same temp home seen as a
//! default home; nothing here touches the machine's real hostler service, its registration or
//! `~/.hostler`. A recording proxy in front of a test service shows every `service.shutdown`
//! doctor sends.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{BIN, Home, stderr, stdout};
use hostler::contract::doctor::{Check, CheckStatus, DoctorReport, FixResult, Who};
use hostler::doctor::{DoctorEnv, DoctorOptions, run_with};
use hostler::paths::Paths;
use hostler::service::ServiceManager;
use hostler::service::manager::{Registrar, RegistrationState};
use serde_json::{Value, json};

const OLDER_CONTRACT: [(&str, &str); 1] = [("HOSTLER_TEST_CONTRACT_VERSION", "3")];
const NEWER_CONTRACT: [(&str, &str); 1] = [("HOSTLER_TEST_CONTRACT_VERSION", "6")];
const OLDER_BINARY: [(&str, &str); 1] = [("HOSTLER_TEST_SERVICE_VERSION", "0.0.1")];
const NEWER_BINARY: [(&str, &str); 1] = [("HOSTLER_TEST_SERVICE_VERSION", "99.0.0")];

// ---------------------------------------------------------------------------------------------
// Helpers

/// A `hostler service run` with test variables of its own (doctor never has them); stopped and
/// reaped on drop.
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
            wait_for(Duration::from_secs(15), || service_pid(home) == Some(pid)),
            "the test service never answered"
        );
        service
    }

    fn pid(&self) -> u64 {
        u64::from(self.child.id())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new(BIN)
                .env("HOSTLER_HOME", &self.home)
                .args(["service", "stop", "--force"])
                .output();
            if !wait_for(Duration::from_secs(10), || {
                self.child.try_wait().unwrap().is_some()
            }) {
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

fn run(home: &Home, dir: &Path, args: &[&str]) -> Output {
    home.hostler().args(args).current_dir(dir).output().unwrap()
}

fn status(home: &Home) -> Value {
    serde_json::from_slice(&run(home, home.dir.path(), &["service", "status", "--json"]).stdout)
        .unwrap_or(Value::Null)
}

fn service_pid(home: &Home) -> Option<u64> {
    status(home)["data"]["pid"].as_u64()
}

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

/// `hostler doctor --json [args]` in `dir`: the report and the exit code.
fn doctor_json(home: &Home, dir: &Path, extra: &[&str]) -> (DoctorReport, Option<i32>) {
    let mut args = vec!["doctor", "--json"];
    args.extend_from_slice(extra);
    let out = run(home, dir, &args);
    let value = json_of(&out);
    assert_eq!(value["ok"], true, "{value}");
    (
        serde_json::from_value(value["data"].clone()).unwrap(),
        out.status.code(),
    )
}

fn check<'a>(report: &'a DoctorReport, id: &str) -> &'a Check {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no check {id}: {:?}", ids(report)))
}

fn ids(report: &DoctorReport) -> Vec<&str> {
    report.checks.iter().map(|c| c.id.as_str()).collect()
}

fn status_of(report: &DoctorReport, id: &str) -> CheckStatus {
    check(report, id).status
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

/// Stands in for launchd/systemd/Task Scheduler: a registration state in memory; `start` runs
/// `hostler service run` in the test home.
struct FakeRegistrar {
    home: PathBuf,
    state: Mutex<RegistrationState>,
    registered: AtomicUsize,
    started: Mutex<Vec<Child>>,
}

impl FakeRegistrar {
    fn new(home: &Home, state: RegistrationState) -> Arc<FakeRegistrar> {
        Arc::new(FakeRegistrar {
            home: home.home.clone(),
            state: Mutex::new(state),
            registered: AtomicUsize::new(0),
            started: Mutex::new(Vec::new()),
        })
    }

    fn registered(&self) -> usize {
        self.registered.load(Ordering::SeqCst)
    }
}

/// `hostler` in `home` with no inherited test hook, as `Home::hostler` builds it.
fn bare_hostler(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("HOSTLER_TEST_") {
            c.env_remove(&name);
        }
    }
    c.env("HOSTLER_HOME", home)
        .env("HOSTLER_LOOKY_LEDGER", home.join("no-looky-ledger.json"))
        .env("HOSTLER_TEST_FAST_KDF", "1")
        .env("HOSTLER_TEST_OWNER_PID", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

impl ServiceManager for FakeRegistrar {
    fn is_installed(&self) -> bool {
        *self.state.lock().unwrap() != RegistrationState::Absent
    }

    fn start(&self) -> anyhow::Result<()> {
        let child = bare_hostler(&self.home).args(["service", "run"]).spawn()?;
        self.started.lock().unwrap().push(child);
        Ok(())
    }
}

impl Registrar for FakeRegistrar {
    fn register(&self, _exe: &Path, _paths: &Paths) -> anyhow::Result<()> {
        self.registered.fetch_add(1, Ordering::SeqCst);
        *self.state.lock().unwrap() = RegistrationState::Current;
        Ok(())
    }

    fn unregister(&self) -> anyhow::Result<()> {
        *self.state.lock().unwrap() = RegistrationState::Absent;
        Ok(())
    }

    fn registration(&self) -> RegistrationState {
        *self.state.lock().unwrap()
    }
}

impl Drop for FakeRegistrar {
    fn drop(&mut self) {
        let _ = bare_hostler(&self.home)
            .args(["service", "stop", "--force"])
            .output();
        for mut child in self.started.lock().unwrap().drain(..) {
            if !wait_for(Duration::from_secs(10), || {
                child.try_wait().unwrap().is_some()
            }) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

/// No OS registration may be touched (a custom home).
struct NoRegistrar;

impl ServiceManager for NoRegistrar {
    fn is_installed(&self) -> bool {
        false
    }

    fn start(&self) -> anyhow::Result<()> {
        anyhow::bail!("not registered")
    }
}

impl Registrar for NoRegistrar {
    fn register(&self, _exe: &Path, _paths: &Paths) -> anyhow::Result<()> {
        panic!("doctor registered a service under a custom home")
    }

    fn unregister(&self) -> anyhow::Result<()> {
        panic!("doctor unregistered a service")
    }
}

/// The test home seen as the default home (registrations apply), with `registrar`.
fn default_env(home: &Home, registrar: Arc<dyn Registrar>) -> DoctorEnv {
    let paths = Paths::under(home.home.clone(), false);
    paths.ensure_dirs().unwrap();
    DoctorEnv {
        paths,
        paths_error: None,
        registrar,
        exe: BIN.into(),
        registered_exe: Arc::new(|| None),
        dev_build: false,
    }
}

/// A custom home at `home` (no registration).
fn custom_env(home: &Path) -> DoctorEnv {
    let paths = Paths::under(home.to_path_buf(), true);
    paths.ensure_dirs().unwrap();
    DoctorEnv {
        paths,
        paths_error: None,
        registrar: Arc::new(NoRegistrar),
        exe: BIN.into(),
        registered_exe: Arc::new(|| None),
        dev_build: false,
    }
}

async fn doctor(env: &DoctorEnv, cwd: &Path, fix: bool) -> DoctorReport {
    run_with(
        DoctorOptions {
            cwd: cwd.to_path_buf(),
            fix,
        },
        env,
    )
    .await
}

/// A checkout with `hostler.toml` (when given).
fn repo_with(home: &Home, name: &str, toml: Option<&str>) -> PathBuf {
    let repo = home.make_repo(name);
    if let Some(toml) = toml {
        std::fs::write(repo.join("hostler.toml"), toml).unwrap();
    }
    repo
}

/// Every line a client sent through a `Proxy`, as JSON.
type Sent = Arc<Mutex<Vec<Value>>>;

/// A socket at `front` that forwards to the service at `back` and records every client line.
/// After it forwards a `service.shutdown` it stops listening, so a new service can take `front`.
struct Proxy {
    sent: Sent,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    fn start(front: &Paths, back: &Paths) -> Proxy {
        use interprocess::local_socket::tokio::{Stream, prelude::*};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = hostler::service::server::bind(&front.socket).unwrap();
        let sent: Sent = Arc::default();
        let back_socket = back.socket.clone();
        let front_socket = front.socket.clone();
        let recorded = sent.clone();
        let task = tokio::spawn(async move {
            let (closing, mut closed) = tokio::sync::watch::channel(false);
            loop {
                let stream = tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok(stream) => stream,
                        Err(_) => break,
                    },
                    _ = closed.changed() => break,
                };
                let name = hostler::service::server::socket_name(&back_socket).unwrap();
                let Ok(upstream) = Stream::connect(name).await else {
                    continue;
                };
                let recorded = recorded.clone();
                let closing = closing.clone();
                tokio::spawn(async move {
                    let (client_read, mut client_write) = tokio::io::split(stream);
                    let (mut up_read, mut up_write) = tokio::io::split(upstream);
                    let down = tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut up_read, &mut client_write).await;
                    });
                    let mut lines = BufReader::new(client_read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let frame: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                        let shutdown = frame["method"] == "service.shutdown";
                        recorded.lock().unwrap().push(frame);
                        let mut text = line;
                        text.push('\n');
                        if up_write.write_all(text.as_bytes()).await.is_err() {
                            break;
                        }
                        if shutdown {
                            let _ = closing.send(true);
                        }
                    }
                    let _ = down.await;
                });
            }
            drop(listener);
            if let hostler::paths::SocketName::File(path) = &front_socket {
                // Only our own socket file: a service that took over `front` binds afresh.
                let _ = std::fs::remove_file(path);
            }
        });
        Proxy { sent, task }
    }

    fn methods(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|f| f["method"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    fn shutdowns(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["method"] == "service.shutdown")
            .map(|f| f["params"].clone())
            .collect()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A second home whose socket the proxy serves.
fn front_home(home: &Home) -> PathBuf {
    // `custom_env` creates it private (`ensure_dirs`).
    home.dir.path().join("front")
}

/// The fixed ids for a checkout with one declared secret `S` and one account `qa`.
const FULL_IDS: &[&str] = &[
    "service.registered",
    "service.reachable",
    "service.contract",
    "service.binary",
    "service.linux_session",
    "secrets.database",
    "project.config",
    "project.approved",
    "project.docs",
    "project.device_rules",
    "secrets.required.S",
    "accounts.required.qa",
    "tools.adb",
    "tools.emulator",
    "tools.xcrun",
    "tools.ideviceinfo",
    "tools.agent_device",
    "claims.workspace",
    "workspace.registered",
];

const FULL_TOML: &str = "[secrets.S]\npurpose = \"the test secret\"\n\n\
                         [accounts.qa]\npurpose = \"the test account\"\nfields = [\"session\"]\n";

// ---------------------------------------------------------------------------------------------
// The report's shape

/// A full state's id list, in order (Task 8's guide documents these).
#[test]
fn json_ids_are_stable() {
    let home = Home::new();
    home.start_service();
    let repo = repo_with(&home, "app", Some(FULL_TOML));
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(ids(&report), FULL_IDS);
    // Outside a checkout the project checks are skipped, the ids stay.
    let (outside, _) = doctor_json(&home, home.dir.path(), &[]);
    for id in [
        "project.config",
        "project.approved",
        "project.docs",
        "project.device_rules",
        "claims.workspace",
        "workspace.registered",
    ] {
        assert_eq!(status_of(&outside, id), CheckStatus::Skipped, "{id}");
    }
    // Every id the report can hold is a pattern of `doctor::CHECK_IDS`.
    for id in FULL_IDS {
        assert!(
            hostler::doctor::CHECK_IDS.iter().any(|known| {
                known == id
                    || known
                        .split_once('<')
                        .is_some_and(|(p, _)| id.starts_with(p))
            }),
            "{id} is not in CHECK_IDS"
        );
    }
}

/// Exit codes: 0 without a problem, 1 with one; human lines and the summary.
#[test]
fn exit_codes_and_human_output() {
    let home = Home::new();
    let outside = home.dir.path();
    // Not running: a problem.
    let out = run(&home, outside, &["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(
        text.contains("FAIL  service.reachable  the hostler service is not running"),
        "{text}"
    );
    assert!(
        text.contains("  fix: hostler service start (doctor --fix runs it)"),
        "{text}"
    );
    assert!(text.contains("problems, "), "{text}");
    // Doctor itself starts nothing.
    assert_eq!(service_pid(&home), None);

    home.start_service();
    let out = run(&home, outside, &["doctor"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("ok    service.reachable"),
        "{}",
        stdout(&out)
    );
}

/// No home at all: 78 (P1's setup failure).
#[test]
fn no_home_exits_78() {
    let home = Home::new();
    let out = home
        .hostler()
        .env("HOSTLER_HOME", "relative/home")
        .args(["doctor", "--json"])
        .current_dir(home.dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert_eq!(json_of(&out)["ok"], false);
}

// ---------------------------------------------------------------------------------------------
// The service checks

/// `service.registered`: every state, the custom home, and the dev-build refusal (D-Q2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_states() {
    let home = Home::new();
    let outside = home.dir.path().to_path_buf();
    for (state, expected, message) in [
        (RegistrationState::Current, CheckStatus::Ok, "registered"),
        (
            RegistrationState::Unknown,
            CheckStatus::Warn,
            "cannot read the registration",
        ),
        (
            RegistrationState::Absent,
            CheckStatus::Problem,
            "not registered",
        ),
        (
            RegistrationState::Outdated,
            CheckStatus::Problem,
            "ProcessType",
        ),
    ] {
        let fake = FakeRegistrar::new(&home, state);
        let report = doctor(&default_env(&home, fake.clone()), &outside, false).await;
        let found = check(&report, "service.registered");
        assert_eq!(found.status, expected, "{state:?}: {found:?}");
        assert!(
            found.message.contains(message),
            "{state:?}: {}",
            found.message
        );
        if expected == CheckStatus::Problem {
            let fix = found.fix.as_ref().unwrap();
            assert_eq!(fix.command.as_deref(), Some("hostler service install"));
            // The service is stopped: nothing to protect.
            assert!(fix.safe, "{fix:?}");
        }
        assert_eq!(fake.registered(), 0);
    }
    let report = doctor(&custom_env(&home.home), &outside, false).await;
    assert_eq!(
        status_of(&report, "service.registered"),
        CheckStatus::Skipped
    );

    // D-Q2, C-3: a dev build's install is never offered as an automatic fix, and --fix does
    // not run it.
    let fake = FakeRegistrar::new(&home, RegistrationState::Absent);
    let mut env = default_env(&home, fake.clone());
    env.dev_build = true;
    let report = doctor(&env, &outside, false).await;
    let fix = check(&report, "service.registered").fix.clone().unwrap();
    assert!(!fix.safe, "{fix:?}");
    assert_eq!(fix.command.as_deref(), Some("hostler service install"));
    assert!(fix.text.contains("dev build"), "{}", fix.text);
    let text = hostler::cli::doctor::human(&report);
    assert!(
        !text.contains("hostler service install (doctor --fix runs it)"),
        "{text}"
    );
    let report = doctor(&env, &outside, true).await;
    assert!(
        report.fixed.iter().all(|o| o.check != "service.registered"),
        "{:?}",
        report.fixed
    );
    assert_eq!(fake.registered(), 0);
}

/// `fix_runs_only_safe_fixes`: a missing registration, a stopped service and a stale docs
/// block are fixed; a missing secret and an unapproved project remain, for a person.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_runs_only_safe_fixes() {
    let home = Home::new();
    let toml = "[secrets.API_KEY]\npurpose = \"the API key\"\n";
    let repo = repo_with(&home, "app", Some(toml));
    let agents = format!(
        "# Agents\n\nkeep this\n\n{}\nold text\n{}\n\nand this\n",
        hostler::init::docs::BEGIN,
        hostler::init::docs::END
    );
    std::fs::write(repo.join("AGENTS.md"), &agents).unwrap();
    let fake = FakeRegistrar::new(&home, RegistrationState::Absent);
    let env = default_env(&home, fake.clone());

    let before = doctor(&env, &repo, false).await;
    assert_eq!(
        status_of(&before, "service.registered"),
        CheckStatus::Problem
    );
    assert_eq!(
        status_of(&before, "service.reachable"),
        CheckStatus::Problem
    );
    assert_eq!(status_of(&before, "project.docs"), CheckStatus::Warn);
    assert!(check(&before, "project.docs").fix.as_ref().unwrap().safe);
    // A read starts nothing and registers nothing.
    assert_eq!(service_pid(&home), None);
    assert_eq!(fake.registered(), 0);

    let after = doctor(&env, &repo, true).await;
    for id in ["service.registered", "service.reachable", "project.docs"] {
        assert_eq!(status_of(&after, id), CheckStatus::Ok, "{id}: {after:#?}");
    }
    assert_eq!(fake.registered(), 1);
    let fixed: Vec<(&str, FixResult)> = after
        .fixed
        .iter()
        .map(|o| (o.check.as_str(), o.result))
        .collect();
    // The start was moot once the install had started the service.
    assert_eq!(
        fixed,
        [
            ("service.registered", FixResult::Fixed),
            ("project.docs", FixResult::Fixed)
        ]
    );
    // The block was replaced in place; the rest of the file is untouched.
    let text = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(text.starts_with("# Agents\n\nkeep this\n\n"), "{text}");
    assert!(text.ends_with("\n\nand this\n"), "{text}");
    assert!(!text.contains("old text"), "{text}");
    // hostler.toml is never written.
    assert_eq!(
        std::fs::read_to_string(repo.join("hostler.toml")).unwrap(),
        toml
    );

    // The person's steps remain, and nothing ran them.
    let approved = check(&after, "project.approved");
    assert_eq!(approved.status, CheckStatus::Problem);
    assert_eq!(approved.fix.as_ref().unwrap().who, Who::Person);
    assert_eq!(
        status_of(&after, "secrets.required.API_KEY"),
        CheckStatus::Skipped,
        "a secret is read only in an approved project"
    );
    let database = check(&after, "secrets.database");
    assert_eq!(database.status, CheckStatus::Warn);
    assert_eq!(database.fix.as_ref().unwrap().who, Who::Person);
    let out = run(&home, home.dir.path(), &["project", "list", "--json"]);
    assert_eq!(
        json_of(&out)["data"],
        json!([]),
        "doctor approved or registered a project"
    );
}

/// `fix_refused_when_busy`: an older, locked, busy service is not replaced, on both fix paths
/// (the install of the default home, `service start` of a custom one); the task keeps running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_refused_when_busy() {
    let home = Home::new();
    seed_running_heavy(&home);
    let old = Service::start(&home, &OLDER_CONTRACT);
    let outside = home.dir.path().to_path_buf();

    let fake = FakeRegistrar::new(&home, RegistrationState::Current);
    let env = default_env(&home, fake.clone());
    let report = doctor(&env, &outside, false).await;
    let contract = check(&report, "service.contract");
    assert_eq!(contract.status, CheckStatus::Problem);
    let fix = contract.fix.as_ref().unwrap();
    assert_eq!(fix.command.as_deref(), Some("hostler service install"));
    assert!(fix.safe, "older, locked: safe to try ({fix:?})");

    let report = doctor(&env, &outside, true).await;
    let outcome = report
        .fixed
        .iter()
        .find(|o| o.check == "service.contract")
        .expect("the install was tried");
    assert_eq!(outcome.result, FixResult::Refused, "{outcome:?}");
    assert!(
        outcome.message.contains("heavy task"),
        "{}",
        outcome.message
    );
    assert_eq!(fake.registered(), 0);
    assert_eq!(service_pid(&home), Some(old.pid()));

    // A custom home: the fix is `service start`, refused the same way.
    let out = run(&home, &outside, &["doctor", "--fix", "--json"]);
    let report: DoctorReport = serde_json::from_value(json_of(&out)["data"].clone()).unwrap();
    let outcome = report
        .fixed
        .iter()
        .find(|o| o.check == "service.contract")
        .expect("the start was tried");
    assert_eq!(outcome.command, "hostler service start");
    assert_eq!(outcome.result, FixResult::Refused, "{outcome:?}");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(service_pid(&home), Some(old.pid()));
    assert_eq!(
        status(&home)["data"]["running_heavy"],
        1,
        "the task stopped"
    );
}

/// An older, idle, locked service is replaced: by the install (default home), and by
/// `service start` (custom home).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_idle_locked_service_is_replaced() {
    let home = Home::new();
    let outside = home.dir.path().to_path_buf();
    let fake = FakeRegistrar::new(&home, RegistrationState::Current);
    let env = default_env(&home, fake.clone());
    {
        let old = Service::start(&home, &OLDER_CONTRACT);
        let report = doctor(&env, &outside, true).await;
        let outcome = &report.fixed[0];
        assert_eq!(outcome.check, "service.contract");
        assert_eq!(outcome.result, FixResult::Fixed, "{outcome:?}");
        assert_eq!(status_of(&report, "service.contract"), CheckStatus::Ok);
        assert_eq!(fake.registered(), 1);
        assert_ne!(service_pid(&home), Some(old.pid()));
    }
    let _ = run(&home, &outside, &["service", "stop"]);
    let old = Service::start(&home, &OLDER_CONTRACT);
    let (report, code) = doctor_json(&home, &outside, &["--fix"]);
    assert_eq!(report.fixed[0].command, "hostler service start");
    assert_eq!(report.fixed[0].result, FixResult::Fixed, "{report:#?}");
    assert_eq!(code, Some(0), "{report:#?}");
    assert_ne!(service_pid(&home), Some(old.pid()));
    assert_eq!(
        status(&home)["data"]["contract_version"],
        hostler::contract::CONTRACT_VERSION
    );
}

/// R1-2: an older client never stops a newer service, by contract or by binary version; no
/// `service.shutdown` reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_never_stops_a_newer_service() {
    for (vars, id) in [
        (&NEWER_CONTRACT, "service.contract"),
        (&NEWER_BINARY, "service.binary"),
    ] {
        let home = Home::new();
        let service = Service::start(&home, vars);
        let env = custom_env(&front_home(&home));
        let proxy = Proxy::start(&env.paths, &home.paths());
        let outside = home.dir.path().to_path_buf();

        let report = doctor(&env, &outside, true).await;
        let found = check(&report, id);
        assert_ne!(found.status, CheckStatus::Ok, "{found:?}");
        let fix = found.fix.as_ref().unwrap();
        assert_eq!(fix.command, None, "{fix:?}");
        assert!(!fix.safe);
        assert!(fix.text.contains("cargo install hostler"), "{}", fix.text);
        assert!(report.fixed.is_empty(), "{:?}", report.fixed);
        assert!(proxy.methods().contains(&"service.status".to_owned()));
        assert!(
            !proxy.methods().iter().any(|m| m == "service.shutdown"),
            "{:?}",
            proxy.methods()
        );
        assert_eq!(service_pid(&home), Some(service.pid()));
    }
}

/// Unlocks the database of an older-contract service across the mismatch (a contract-3 hello).
async fn unlock_older(paths: &Paths) {
    use hostler::contract::secrets::Unlock;
    use hostler::contract::wire::{ClientFrame, Request};
    use hostler::secrets::value::SecretValue;
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let name = hostler::service::server::socket_name(&paths.socket).unwrap();
    let stream = Stream::connect(name).await.unwrap();
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let requests = [
        Request::Hello {
            contract_version: 3,
            client: "test".into(),
        },
        Request::SecretsUnlock(Unlock {
            password: SecretValue::new("a test password, not a secret".into()),
            key_file: None,
            forget_key_file: false,
            create: true,
        }),
    ];
    for (id, request) in requests.into_iter().enumerate() {
        let id = id as u64 + 1;
        let frame = serde_json::to_string(&ClientFrame { id, request }).unwrap();
        write.write_all(frame.as_bytes()).await.unwrap();
        write.write_all(b"\n").await.unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                assert!(value["error"].is_null(), "{value}");
                break;
            }
        }
    }
}

/// R1-6: an older service with an unlocked database is live work: the fix is not safe, --fix
/// skips it, and the database stays unlocked (by contract and by binary version).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_never_locks_an_unlocked_database() {
    let outside_of = |home: &Home| home.dir.path().to_path_buf();
    {
        let home = Home::new();
        let old = Service::start(&home, &OLDER_CONTRACT);
        unlock_older(&home.paths()).await;
        assert_eq!(status(&home)["data"]["secrets_unlocked"], true);
        let fake = FakeRegistrar::new(&home, RegistrationState::Current);
        let env = default_env(&home, fake.clone());
        let report = doctor(&env, &outside_of(&home), true).await;
        let fix = check(&report, "service.contract").fix.clone().unwrap();
        assert!(!fix.safe, "{fix:?}");
        assert!(fix.text.contains("locks it"), "{}", fix.text);
        assert!(report.fixed.is_empty(), "{:?}", report.fixed);
        assert_eq!(service_pid(&home), Some(old.pid()));
        assert_eq!(status(&home)["data"]["secrets_unlocked"], true);
        assert_eq!(fake.registered(), 0);
    }
    {
        let home = Home::new();
        let old = Service::start(&home, &OLDER_BINARY);
        home.unlock_new("a test password, not a secret");
        let (report, _) = doctor_json(&home, &outside_of(&home), &["--fix"]);
        let found = check(&report, "service.binary");
        assert_eq!(found.status, CheckStatus::Warn);
        let fix = found.fix.clone().unwrap();
        assert_eq!(fix.command.as_deref(), Some("hostler service restart"));
        assert!(!fix.safe, "{fix:?}");
        assert!(fix.text.contains("locks it"), "{}", fix.text);
        assert!(report.fixed.is_empty(), "{:?}", report.fixed);
        assert_eq!(service_pid(&home), Some(old.pid()));
        assert_eq!(status(&home)["data"]["secrets_unlocked"], true);
    }
}

/// `fix_never_forces`: every shutdown doctor sends is `if_idle: true, refuse_if_unlocked:
/// true`, never `force` (the restart of an older binary, and the replacement of an older
/// contract), and doctor's sources name no explicit stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_never_forces() {
    for (vars, id, command) in [
        (&OLDER_BINARY, "service.binary", "hostler service restart"),
        (&OLDER_CONTRACT, "service.contract", "hostler service start"),
    ] {
        let home = Home::new();
        let old = Service::start(&home, vars);
        let front = front_home(&home);
        let env = custom_env(&front);
        let proxy = Proxy::start(&env.paths, &home.paths());
        let outside = home.dir.path().to_path_buf();

        let report = doctor(&env, &outside, true).await;
        let outcome = report
            .fixed
            .iter()
            .find(|o| o.check == id)
            .unwrap_or_else(|| panic!("{id} was not fixed: {report:#?}"));
        assert_eq!(outcome.command, command);
        assert_eq!(outcome.result, FixResult::Fixed, "{outcome:?}");
        let shutdowns = proxy.shutdowns();
        assert_eq!(shutdowns.len(), 1, "{:?}", proxy.methods());
        for params in &shutdowns {
            assert_eq!(
                params,
                &json!({ "if_idle": true, "refuse_if_unlocked": true }),
                "{params}"
            );
        }
        // The old service is gone; the new one serves the front home.
        assert!(wait_for(Duration::from_secs(10), || service_pid(&home)
            != Some(old.pid())));
        let _ = bare_hostler(&front).args(["service", "stop"]).output();
    }

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor");
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for needle in [
            "Explicit",
            "force: true",
            "if_idle: false",
            "ServiceShutdown",
        ] {
            assert!(!text.contains(needle), "{} names {needle}", path.display());
        }
    }
}

/// `person_only_fix_has_no_command` (R1-13) and `sandbox_denied_check`: a seatbelt's `EPERM`
/// at the connect, and an `EACCES` on the state directory, are `sandbox_denied`, with
/// `command: null`, `who: person`, and nothing started.
#[cfg(unix)]
#[test]
fn sandbox_denied_check() {
    use std::os::unix::fs::PermissionsExt;

    let home = Home::new();
    home.start_service();
    let pid = service_pid(&home);
    let out = home
        .hostler()
        .env("HOSTLER_TEST_CONNECT_ERRNO", "EPERM")
        .args(["doctor", "--json", "--fix"])
        .current_dir(home.dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let value = json_of(&out);
    let reachable = value["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "service.reachable")
        .unwrap()
        .clone();
    assert_eq!(reachable["status"], "problem");
    assert_eq!(reachable["details"]["reason"], "sandbox_denied");
    let fix = &reachable["fix"];
    assert_eq!(fix["command"], Value::Null, "{fix}");
    assert_eq!(fix["who"], "person");
    assert_eq!(fix["safe"], false);
    let text = fix["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "allow hostler's socket in the agent's sandbox, or run hostler outside the sandbox"
        ),
        "{text}"
    );
    assert!(text.ends_with("see: hostler guide sandbox"), "{text}");
    assert_eq!(value["data"]["fixed"], json!([]));
    assert_eq!(service_pid(&home), pid, "the service changed");

    // EACCES: the home may not be searched (Task 4's setup).
    let _ = run(&home, home.dir.path(), &["service", "stop"]);
    std::fs::set_permissions(&home.home, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = home
        .hostler()
        .args(["doctor", "--json", "--fix"])
        .current_dir(home.dir.path())
        .output()
        .unwrap();
    std::fs::set_permissions(&home.home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let value = json_of(&out);
    let reachable = value["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "service.reachable")
        .unwrap()
        .clone();
    assert_eq!(
        reachable["details"]["reason"], "sandbox_denied",
        "{reachable}"
    );
    assert_eq!(
        reachable["details"]["dir"],
        home.home.display().to_string(),
        "{reachable}"
    );
    let text = reachable["fix"]["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("check the permissions of {}", home.home.display())),
        "{text}"
    );
    assert_eq!(value["data"]["fixed"], json!([]));
    assert_eq!(service_pid(&home), None, "doctor started a service");
}

// ---------------------------------------------------------------------------------------------
// The project checks

/// `project.config`: absent, invalid (one check per line), TODO purposes, valid.
#[test]
fn project_config_states() {
    let home = Home::new();
    home.start_service();
    let absent = repo_with(&home, "absent", None);
    let (report, _) = doctor_json(&home, &absent, &[]);
    let found = check(&report, "project.config");
    assert_eq!(found.status, CheckStatus::Warn);
    assert_eq!(
        found.fix.as_ref().unwrap().command.as_deref(),
        Some("hostler init")
    );

    let invalid = repo_with(
        &home,
        "invalid",
        Some("[secrets.S]\npurpose = \"x\"\nbogus = 1\n\n[nonsense]\n"),
    );
    let (report, code) = doctor_json(&home, &invalid, &[]);
    assert_eq!(code, Some(1));
    let lines: Vec<&Check> = report
        .checks
        .iter()
        .filter(|c| c.id.starts_with("project.config."))
        .collect();
    assert_eq!(lines.len(), 2, "{:?}", ids(&report));
    assert!(lines.iter().all(|c| c.status == CheckStatus::Problem));
    assert_eq!(lines[0].id, "project.config.3");
    assert!(
        lines[0].message.starts_with("hostler.toml line 3: "),
        "{}",
        lines[0].message
    );
    assert_eq!(status_of(&report, "project.docs"), CheckStatus::Skipped);

    let todo = repo_with(
        &home,
        "todo",
        Some("[secrets.S]\npurpose = \"TODO: say what S is for\"\n"),
    );
    let (report, _) = doctor_json(&home, &todo, &[]);
    let found = check(&report, "project.config");
    assert_eq!(found.status, CheckStatus::Warn);
    assert!(found.message.contains('S'), "{}", found.message);

    let valid = repo_with(&home, "valid", Some(FULL_TOML));
    let (report, _) = doctor_json(&home, &valid, &[]);
    assert_eq!(status_of(&report, "project.config"), CheckStatus::Ok);
}

/// `project.docs` (missing, current, broken) and `project.device_rules` (unset, missing file,
/// missing heading, found).
#[test]
fn docs_and_device_rules_states() {
    let home = Home::new();
    home.start_service();
    let repo = repo_with(&home, "app", Some(""));
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "project.docs"), CheckStatus::Warn);
    assert_eq!(
        status_of(&report, "project.device_rules"),
        CheckStatus::Warn
    );
    // --fix writes the block (AGENTS.md is created), then it is current.
    let (report, _) = doctor_json(&home, &repo, &["--fix"]);
    assert_eq!(
        status_of(&report, "project.docs"),
        CheckStatus::Ok,
        "{report:#?}"
    );
    assert!(repo.join("AGENTS.md").is_file());
    // A plain CLAUDE.md gets its own block; one importing AGENTS.md does not.
    std::fs::write(repo.join("CLAUDE.md"), "notes\n@AGENTS.md\n").unwrap();
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "project.docs"), CheckStatus::Ok);
    std::fs::write(repo.join("CLAUDE.md"), "notes\n").unwrap();
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "project.docs"), CheckStatus::Warn);

    // Broken markers: a problem, never rewritten.
    let broken = format!("{}\nhalf\n", hostler::init::docs::BEGIN);
    std::fs::write(repo.join("CLAUDE.md"), &broken).unwrap();
    let (report, code) = doctor_json(&home, &repo, &["--fix"]);
    let found = check(&report, "project.docs");
    assert_eq!(found.status, CheckStatus::Problem);
    assert!(found.message.contains("line 1"), "{}", found.message);
    assert_eq!(code, Some(1));
    assert_eq!(
        std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
        broken
    );
    std::fs::remove_file(repo.join("CLAUDE.md")).unwrap();

    let rules = |value: &str| {
        std::fs::write(
            repo.join("hostler.toml"),
            format!("[agents]\ndevice_rules = \"{value}\"\n"),
        )
        .unwrap();
        let (report, _) = doctor_json(&home, &repo, &[]);
        check(&report, "project.device_rules").clone()
    };
    let found = rules("RULES.md#Devices");
    assert_eq!(found.status, CheckStatus::Problem);
    assert!(
        found.message.contains("does not exist"),
        "{}",
        found.message
    );
    std::fs::write(repo.join("RULES.md"), "# Other\n").unwrap();
    let found = rules("RULES.md#Devices");
    assert_eq!(found.status, CheckStatus::Problem);
    assert!(found.message.contains("no heading"), "{}", found.message);
    std::fs::write(repo.join("RULES.md"), "# Other\n\n## Devices\nrules\n").unwrap();
    assert_eq!(rules("RULES.md#Devices").status, CheckStatus::Ok);
}

/// `secrets.database`, `project.approved`, `secrets.required.<NAME>` and
/// `accounts.required.<NAME>` through their states; a secret's fixes name no env file.
#[test]
fn secrets_and_accounts_states() {
    let home = Home::new();
    home.start_service();
    let repo = repo_with(&home, "app", Some(FULL_TOML));
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "secrets.database"), CheckStatus::Warn);
    assert!(
        check(&report, "secrets.database")
            .message
            .contains("no secrets database")
    );
    assert_eq!(status_of(&report, "project.approved"), CheckStatus::Problem);
    assert_eq!(
        status_of(&report, "secrets.required.S"),
        CheckStatus::Skipped
    );
    assert_eq!(
        status_of(&report, "accounts.required.qa"),
        CheckStatus::Skipped
    );

    home.unlock_new("a test password, not a secret");
    home.approve(&repo);
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "secrets.database"), CheckStatus::Ok);
    assert_eq!(status_of(&report, "project.approved"), CheckStatus::Ok);
    let secret = check(&report, "secrets.required.S");
    assert_eq!(secret.status, CheckStatus::Problem);
    let fix = secret.fix.as_ref().unwrap();
    assert_eq!(fix.who, Who::Agent);
    assert!(!fix.safe);
    assert_eq!(
        fix.command.as_deref(),
        Some(if cfg!(windows) {
            "hostler secret request S --reason \"the test secret\""
        } else {
            "hostler secret request S --reason 'the test secret'"
        })
    );
    assert_eq!(
        secret.details["person_fix"]["command"], "hostler secret set S",
        "{}",
        secret.details
    );
    assert!(
        secret.details["person_fix"]["text"]
            .as_str()
            .unwrap()
            .contains("if you keep it in an env file, import that file")
    );
    let account = check(&report, "accounts.required.qa");
    assert_eq!(account.status, CheckStatus::Problem);
    assert_eq!(
        account.fix.as_ref().unwrap().command.as_deref(),
        Some("hostler account set qa --field session")
    );
    assert_eq!(account.fix.as_ref().unwrap().who, Who::Person);

    home.set(&repo, "S", "a value that is not printed");
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "secrets.required.S"), CheckStatus::Ok);

    let out = run(&home, home.dir.path(), &["lock"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "secrets.database"), CheckStatus::Warn);
    assert!(
        check(&report, "secrets.database")
            .message
            .contains("locked")
    );
    assert_eq!(
        status_of(&report, "secrets.required.S"),
        CheckStatus::Skipped
    );
}

/// `claims.workspace` (a TTL hold past the limit, here shortened) and `workspace.registered`.
#[test]
fn claims_and_workspace_states() {
    let home = Home::new();
    home.start_service();
    let repo = repo_with(&home, "app", None);
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(
        status_of(&report, "workspace.registered"),
        CheckStatus::Warn
    );
    assert_eq!(status_of(&report, "claims.workspace"), CheckStatus::Ok);

    let out = home.hold(&repo, &["screen", "--ttl", "600", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = json_of(&out)["data"]["id"].as_str().unwrap().to_owned();
    let (report, _) = doctor_json(&home, &repo, &[]);
    assert_eq!(status_of(&report, "workspace.registered"), CheckStatus::Ok);
    assert_eq!(status_of(&report, "claims.workspace"), CheckStatus::Ok);

    let out = home
        .hostler()
        .env("HOSTLER_TEST_DOCTOR_STALE_MS", "0")
        .args(["doctor", "--json", "--fix"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let report: DoctorReport = serde_json::from_value(json_of(&out)["data"].clone()).unwrap();
    let found = check(&report, "claims.workspace");
    assert_eq!(found.status, CheckStatus::Warn);
    assert!(found.message.contains(&id), "{}", found.message);
    let fix = found.fix.as_ref().unwrap();
    assert_eq!(fix.command, Some(format!("hostler release {id}")));
    assert!(!fix.safe, "a release is never automatic");
    // --fix released nothing.
    let held: usize = home
        .snapshot()
        .resources
        .iter()
        .map(|state| state.claims.len())
        .sum();
    assert_eq!(held, 1);
}

/// Doctor registers no checkout and no project: reads only.
#[test]
fn doctor_never_registers_a_checkout() {
    use hostler::contract::wire::Request;

    let home = Home::new();
    home.start_service();
    let repo = repo_with(&home, "app", Some(FULL_TOML));
    let (_, _) = doctor_json(&home, &repo, &[]);
    let (_, _) = doctor_json(&home, &repo, &["--fix"]);
    let out = run(&home, &repo, &["doctor"]);
    assert!(out.status.code().is_some());
    assert_eq!(home.call(Request::WorkspaceList).unwrap(), json!([]));
    assert_eq!(home.call(Request::ProjectList).unwrap(), json!([]));
}

// ---------------------------------------------------------------------------------------------
// No env file, no value

/// R1-4, structurally: no doctor source names the env-file reader, an env-file name, or
/// init's detection.
#[test]
fn doctor_source_reads_no_env_file() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("cli/doctor.rs")];
    for entry in std::fs::read_dir(src.join("doctor")).unwrap() {
        files.push(entry.unwrap().path());
    }
    assert!(files.len() >= 6, "{files:?}");
    let quoted_env = format!("\"{}", ".env");
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        for needle in ["dotenv", quoted_env.as_str(), "init::detect"] {
            assert!(!text.contains(needle), "{} names {needle}", path.display());
        }
    }
}

/// R1-4, behaviourally: an env file holding the declared name changes nothing in the report,
/// readable or not.
#[cfg(unix)]
#[test]
fn an_env_file_does_not_change_the_report() {
    use std::os::unix::fs::PermissionsExt;

    let home = Home::new();
    home.start_service();
    let repo = repo_with(&home, "app", Some(FULL_TOML));
    let env_file = repo.join(".env.local");
    std::fs::write(&env_file, "S=a-value-doctor-never-reads\n").unwrap();
    let readable = run(&home, &repo, &["doctor", "--json"]).stdout;
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = run(&home, &repo, &["doctor", "--json"]).stdout;
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&readable),
        String::from_utf8_lossy(&unreadable)
    );
    // An `[agents] docs` entry naming an env file is refused, not read.
    std::fs::write(
        repo.join("hostler.toml"),
        "[agents]\ndocs = [\".env.local\"]\ndevice_rules = \".env.local#S\"\n",
    )
    .unwrap();
    let (report, _) = doctor_json(&home, &repo, &["--fix"]);
    for id in ["project.docs", "project.device_rules"] {
        let found = check(&report, id);
        assert_eq!(found.status, CheckStatus::Problem, "{found:?}");
        assert!(found.message.contains("env file"), "{}", found.message);
    }
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        "S=a-value-doctor-never-reads\n"
    );
}

/// `no_value_in_report`: a value-shaped marker in `hostler.toml` and in an env file beside it
/// never reaches stdout, stderr or the JSON.
#[test]
fn no_value_in_report() {
    const MARKER: &str = "hunter2-canary-7f3a9c";
    let home = Home::new();
    home.start_service();
    let repo = repo_with(
        &home,
        "app",
        Some(&format!(
            "[secrets.S]\npurpose = \"x\"\nvalue = \"{MARKER}\"\n\n[stray]\nkey = \"{MARKER}\"\n"
        )),
    );
    std::fs::write(repo.join(".env.local"), format!("S={MARKER}\n")).unwrap();
    for args in [
        &["doctor"][..],
        &["doctor", "--json"],
        &["doctor", "--fix"],
        &["doctor", "--fix", "--json"],
    ] {
        let out = run(&home, &repo, args);
        assert!(!stdout(&out).contains(MARKER), "{args:?}: stdout");
        assert!(!stderr(&out).contains(MARKER), "{args:?}: stderr");
    }
    // The env file alone, with a valid config.
    std::fs::write(repo.join("hostler.toml"), FULL_TOML).unwrap();
    home.unlock_new("a test password, not a secret");
    home.approve(&repo);
    for args in [&["doctor", "--json"][..], &["doctor", "--fix"]] {
        let out = run(&home, &repo, args);
        assert!(!stdout(&out).contains(MARKER), "{args:?}: stdout");
        assert!(!stderr(&out).contains(MARKER), "{args:?}: stderr");
    }
}

// ---------------------------------------------------------------------------------------------
// Linux logind (W-6)

/// `service.linux_session` from fake `busctl`/`loginctl` and a `logind.conf` fixture.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linux_session_states() {
    use std::os::unix::fs::PermissionsExt;

    // Doctor reads the seams in this process: set once, before any thread reads them.
    let home = Home::new();
    let bin = home.dir.path().join("doctor-bin");
    let conf = home.dir.path().join("logind");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(conf.join("logind.conf.d")).unwrap();
    let script = |name: &str, body: &str| {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    // SAFETY: set before any doctor call in this test binary's only Linux-seam test.
    unsafe {
        std::env::set_var("HOSTLER_TEST_DOCTOR_BIN", &bin);
        std::env::set_var("HOSTLER_TEST_DOCTOR_LOGIND", &conf);
    }
    let outside = home.dir.path().to_path_buf();

    // No busctl: logind.conf says yes; not registered → a problem, fixed by the install.
    std::fs::write(conf.join("logind.conf"), "[Login]\nKillUserProcesses=no\n").unwrap();
    std::fs::write(
        conf.join("logind.conf.d/10-kill.conf"),
        "[Login]\nKillUserProcesses=yes\n",
    )
    .unwrap();
    let fake = FakeRegistrar::new(&home, RegistrationState::Absent);
    let report = doctor(&default_env(&home, fake.clone()), &outside, false).await;
    let found = check(&report, "service.linux_session");
    assert_eq!(found.status, CheckStatus::Problem, "{found:?}");
    assert!(found.fix.as_ref().unwrap().safe);

    // busctl says false: fine without a registration.
    script("busctl", "echo 'b false'");
    let report = doctor(&default_env(&home, fake.clone()), &outside, false).await;
    assert_eq!(status_of(&report, "service.linux_session"), CheckStatus::Ok);

    // Registered, no linger: a warning for the person.
    script("loginctl", "echo 'Linger=no'");
    let fake = FakeRegistrar::new(&home, RegistrationState::Current);
    let report = doctor(&default_env(&home, fake.clone()), &outside, false).await;
    let found = check(&report, "service.linux_session");
    assert_eq!(found.status, CheckStatus::Warn);
    let fix = found.fix.as_ref().unwrap();
    assert_eq!(fix.who, Who::Person);
    assert!(
        fix.command
            .as_deref()
            .unwrap()
            .starts_with("loginctl enable-linger ")
    );
    script("loginctl", "echo 'Linger=yes'");
    let report = doctor(&default_env(&home, fake.clone()), &outside, false).await;
    assert_eq!(status_of(&report, "service.linux_session"), CheckStatus::Ok);
}

// ---------------------------------------------------------------------------------------------
// Fix round 1 (review t6-r1)

/// A lifecycle server that speaks this contract as hostler 0.0.1 (older than this binary), locked
/// and idle, under this test process's pid: it accepts `service.shutdown` and never goes away.
/// Every other method gets an error. It records the methods it is sent.
fn stubborn_service(
    paths: &Paths,
    methods: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<()> {
    use interprocess::local_socket::traits::tokio::Listener as _;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = hostler::service::server::bind(&paths.socket).unwrap();
    tokio::spawn(async move {
        while let Ok(stream) = listener.accept().await {
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
                        "hello" => json!({ "id": id, "result": {
                            "contract_version": hostler::contract::CONTRACT_VERSION, "service_version": "0.0.1",
                        }}),
                        "service.status" => json!({ "id": id, "result": {
                            "service_version": "0.0.1",
                            "contract_version": hostler::contract::CONTRACT_VERSION,
                            "pid": std::process::id(),
                            "running_heavy": 0,
                            "waiting_heavy": 0,
                            "secrets_unlocked": false,
                        }}),
                        "service.shutdown" => json!({ "id": id, "result": { "stopping": true } }),
                        _ => json!({ "id": id, "error": {
                            "code": "internal",
                            "message": "not served by this stub",
                            "details": {},
                            "recovery": null,
                        }}),
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

/// R1-1: one deadline for the whole command. A service that accepts the stop and never goes
/// away (`wait_gone` would wait 40 s) does not hold `doctor --fix` past its 20 s: the restart is
/// `still_failing`, with the "ran out of time" note, and the report returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_is_bounded_by_the_command_deadline() {
    let home = Home::new();
    let env = custom_env(&home.home);
    let methods = Arc::new(Mutex::new(Vec::new()));
    let stub = stubborn_service(&env.paths, methods.clone());
    let outside = home.dir.path().to_path_buf();

    let started = Instant::now();
    let report = doctor(&env, &outside, true).await;
    let took = started.elapsed();
    assert!(
        took < hostler::doctor::COMMAND_BOUND + Duration::from_secs(3),
        "doctor --fix took {took:?}"
    );
    let outcome = report
        .fixed
        .iter()
        .find(|o| o.check == "service.binary")
        .unwrap_or_else(|| panic!("the restart was not tried: {report:#?}"));
    assert_eq!(outcome.result, FixResult::StillFailing, "{outcome:?}");
    assert!(
        outcome.message.contains("doctor ran out of time"),
        "{}",
        outcome.message
    );
    let methods = methods.lock().unwrap().clone();
    assert_eq!(
        methods.iter().filter(|m| *m == "service.shutdown").count(),
        1,
        "{methods:?}"
    );
    stub.abort();
    let _ = stub.await;
}

/// A registrar whose lookups hang (a stuck `schtasks`).
struct SlowRegistrar;

impl ServiceManager for SlowRegistrar {
    fn is_installed(&self) -> bool {
        std::thread::sleep(Duration::from_secs(60));
        false
    }

    fn start(&self) -> anyhow::Result<()> {
        anyhow::bail!("not registered")
    }
}

impl Registrar for SlowRegistrar {
    fn register(&self, _exe: &Path, _paths: &Paths) -> anyhow::Result<()> {
        panic!("doctor registered over an unreadable registration")
    }

    fn unregister(&self) -> anyhow::Result<()> {
        panic!("doctor unregistered a service")
    }

    fn registration(&self) -> RegistrationState {
        std::thread::sleep(Duration::from_secs(60));
        RegistrationState::Absent
    }
}

/// R1-4, R1-2: hanging registration lookups are abandoned at their bound; the registration
/// then reads as unknown, which is a warning with nothing to run, never "absent".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_registration_lookups_are_bounded() {
    let home = Home::new();
    let mut env = default_env(&home, Arc::new(SlowRegistrar));
    env.registered_exe = Arc::new(|| {
        std::thread::sleep(Duration::from_secs(60));
        Some(PathBuf::from("/slow/hostler"))
    });
    let outside = home.dir.path().to_path_buf();
    let started = Instant::now();
    let report = doctor(&env, &outside, false).await;
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );
    let found = check(&report, "service.registered");
    assert_eq!(found.status, CheckStatus::Warn, "{found:?}");
    assert!(
        found
            .message
            .starts_with("cannot read the registration: the OS service manager did not answer"),
        "{}",
        found.message
    );
    assert!(found.fix.is_none());
}

/// R1-3: an agent doc or a device-rules file that is a symlink out of the checkout is neither
/// read nor written; the symlink stays.
#[cfg(unix)]
#[test]
fn symlinked_docs_outside_are_not_read_or_written() {
    let home = Home::new();
    home.start_service();
    let repo = repo_with(
        &home,
        "app",
        Some("[agents]\ndevice_rules = \"RULES.md#Devices\"\n"),
    );
    let outside = home.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let stale = format!(
        "{}\nold\n{}\n",
        hostler::init::docs::BEGIN,
        hostler::init::docs::END
    );
    std::fs::write(outside.join("AGENTS.md"), &stale).unwrap();
    std::fs::write(outside.join("RULES.md"), "# Devices\n").unwrap();
    std::os::unix::fs::symlink(outside.join("AGENTS.md"), repo.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(outside.join("RULES.md"), repo.join("RULES.md")).unwrap();

    let (report, code) = doctor_json(&home, &repo, &["--fix"]);
    assert_eq!(code, Some(1));
    for id in ["project.docs", "project.device_rules"] {
        let found = check(&report, id);
        assert_eq!(found.status, CheckStatus::Problem, "{found:?}");
        assert!(
            found.message.contains("points outside the checkout"),
            "{}",
            found.message
        );
    }
    assert!(report.fixed.iter().all(|o| o.check != "project.docs"));
    assert_eq!(
        std::fs::read_to_string(outside.join("AGENTS.md")).unwrap(),
        stale
    );
    assert!(
        std::fs::symlink_metadata(repo.join("AGENTS.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // A symlink inside the checkout is read, but its block is never written through it.
    std::fs::remove_file(repo.join("AGENTS.md")).unwrap();
    std::fs::write(repo.join("real.md"), &stale).unwrap();
    std::os::unix::fs::symlink("real.md", repo.join("AGENTS.md")).unwrap();
    let (report, _) = doctor_json(&home, &repo, &["--fix"]);
    let found = check(&report, "project.docs");
    assert_eq!(found.status, CheckStatus::Problem, "{found:?}");
    assert!(found.message.contains("symlink"), "{}", found.message);
    assert_eq!(
        std::fs::read_to_string(repo.join("real.md")).unwrap(),
        stale
    );
}

/// R1-5: a mismatched contract whose version does not parse gets the plan's recovery: a newer
/// binary, or an upgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_version_contract_says_upgrade() {
    let home = Home::new();
    let service = Service::start(
        &home,
        &[
            ("HOSTLER_TEST_CONTRACT_VERSION", "3"),
            ("HOSTLER_TEST_SERVICE_VERSION", "dev"),
        ],
    );
    let outside = home.dir.path().to_path_buf();
    let report = doctor(&custom_env(&home.home), &outside, true).await;
    let found = check(&report, "service.contract");
    assert_eq!(found.status, CheckStatus::Problem);
    assert_eq!(found.details["direction"], "unknown");
    let fix = found.fix.as_ref().unwrap();
    assert_eq!(fix.command, None);
    assert!(!fix.safe);
    assert_eq!(
        fix.text,
        "use the newer hostler binary, or upgrade: cargo install hostler"
    );
    assert!(report.fixed.is_empty(), "{:?}", report.fixed);
    assert_eq!(service_pid(&home), Some(service.pid()));
}

// ---------------------------------------------------------------------------------------------
// Fix round 2 (review t6-r2)

/// R2-2: an older, locked service under an unreadable OS registration is not replaced by an
/// automatic install: the contract fix is the person's, nothing is registered or stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_registration_never_auto_installs() {
    let home = Home::new();
    let old = Service::start(&home, &OLDER_CONTRACT);
    let fake = FakeRegistrar::new(&home, RegistrationState::Unknown);
    let env = default_env(&home, fake.clone());
    let outside = home.dir.path().to_path_buf();

    let report = doctor(&env, &outside, true).await;
    let contract = check(&report, "service.contract");
    assert_eq!(contract.status, CheckStatus::Problem);
    let fix = contract.fix.as_ref().unwrap();
    assert_eq!(fix.command.as_deref(), Some("hostler service install"));
    assert_eq!(fix.who, Who::Person);
    assert!(!fix.safe, "{fix:?}");
    assert!(
        fix.text.starts_with("cannot read the registration: "),
        "{}",
        fix.text
    );
    assert_eq!(status_of(&report, "service.registered"), CheckStatus::Warn);
    assert!(report.fixed.is_empty(), "{:?}", report.fixed);
    assert_eq!(fake.registered(), 0);
    assert_eq!(
        service_pid(&home),
        Some(old.pid()),
        "the service was stopped"
    );
    assert_eq!(status(&home)["data"]["contract_version"], 3);
}

// ---------------------------------------------------------------------------------------------
// Clean read (P4-L28)

/// C-2: an older same-contract service under a registration whose binary cannot be read is not
/// restarted (the restart might start the old binary again): the install is the person's. With
/// the registered binary known to be this hostler, the restart is the safe fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_registered_binary_is_never_restarted() {
    let home = Home::new();
    let old = Service::start(&home, &OLDER_BINARY);
    let fake = FakeRegistrar::new(&home, RegistrationState::Current);
    let outside = home.dir.path().to_path_buf();

    let env = default_env(&home, fake.clone());
    let report = doctor(&env, &outside, true).await;
    let found = check(&report, "service.binary");
    assert_eq!(found.status, CheckStatus::Warn, "{found:?}");
    assert_eq!(found.details["registered_binary"], "unknown");
    let fix = found.fix.as_ref().unwrap();
    assert_eq!(fix.who, Who::Person);
    assert_eq!(fix.command.as_deref(), Some("hostler service install"));
    assert!(!fix.safe);
    assert!(report.fixed.is_empty(), "{:?}", report.fixed);
    assert_eq!(
        service_pid(&home),
        Some(old.pid()),
        "the service was restarted"
    );

    // The registration is known to run this hostler: the restart is safe again.
    let mut env = default_env(&home, fake.clone());
    env.registered_exe = Arc::new(|| Some(PathBuf::from(BIN)));
    let report = doctor(&env, &outside, false).await;
    let found = check(&report, "service.binary");
    assert_eq!(found.details["registered_binary"], "this");
    let fix = found.fix.as_ref().unwrap();
    assert_eq!(fix.command.as_deref(), Some("hostler service restart"));
    assert!(fix.safe, "{fix:?}");
}
