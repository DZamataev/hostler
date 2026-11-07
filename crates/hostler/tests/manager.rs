use std::path::Path;

use hostler::paths::Paths;
use hostler::service::manager;

const EXE: &str = "/opt/hostler/bin/hostler";

fn paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path().join("home"), false);
    (dir, paths)
}

#[test]
fn launchd_plist_contains_label_and_args() {
    let (_dir, paths) = paths();
    let plist = manager::launchd::render_definition(Path::new(EXE), &paths);

    assert!(plist.contains("<string>io.github.dzamataev.hostler</string>"));
    assert!(plist.contains(&format!("<string>{EXE}</string>")));
    assert!(plist.contains("<string>service</string>"));
    assert!(plist.contains("<string>run</string>\n    <string>--managed</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    // launchd delays a start by 10 s after a short run by default; a restart must fit in the
    // client's 5 s start window.
    assert!(plist.contains("<key>ThrottleInterval</key>\n  <integer>1</integer>"));
    assert!(plist.contains("<key>SuccessfulExit</key>"));
    assert!(plist.contains("<false/>"));
    assert!(plist.contains(&paths.log_dir.display().to_string()));
}

#[test]
fn systemd_unit_contains_execstart() {
    let (_dir, paths) = paths();
    let unit = manager::systemd::render_definition(Path::new(EXE), &paths);

    assert!(unit.contains(&format!("ExecStart={EXE} service run --managed\n")));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy=default.target"));
}

#[test]
fn systemd_unit_escapes_percent_specifiers() {
    let (_dir, paths) = paths();
    let unit =
        manager::systemd::render_definition(Path::new("/home/alice/build%h/hostler"), &paths);

    assert!(unit.contains("ExecStart=/home/alice/build%%h/hostler service run --managed"));
}

#[test]
fn windows_task_xml_runs_at_logon() {
    let (_dir, paths) = paths();
    let xml = manager::windows::render_definition(Path::new(EXE), &paths);

    assert!(xml.contains("<LogonTrigger>"));
    assert!(xml.contains("<Enabled>true</Enabled>"));
    // Through headless conhost: a console program started by the task shows a window otherwise.
    assert!(xml.contains(r"<Command>%SystemRoot%\System32\conhost.exe</Command>"));
    assert!(xml.contains(&format!(
        "<Arguments>--headless &quot;{EXE}&quot; service run --managed</Arguments>"
    )));
}

#[test]
fn install_refused_with_custom_home() {
    let dir = tempfile::tempdir().unwrap();
    let custom = Paths::under(dir.path().join("custom-home"), true);

    let err = manager::install(Path::new(EXE), &custom).unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("HOSTLER_HOME"),
        "unexpected error: {message}"
    );
}

#[test]
fn render_definition_dispatches_to_this_platform() {
    let (_dir, paths) = paths();
    let rendered = manager::render_definition(Path::new(EXE), &paths);
    assert!(rendered.contains(EXE));
}

/// Definitions from before `--managed` start a service that steps aside: they are outdated.
#[test]
fn definitions_without_managed_are_outdated() {
    let (_dir, paths) = paths();
    let exe = Path::new(EXE);

    let plist = manager::launchd::render_definition(exe, &paths);
    let old_plist = plist.replace("    <string>--managed</string>\n", "");
    assert_ne!(plist, old_plist);
    assert!(manager::launchd::definition_is_current(&plist));
    assert!(!manager::launchd::definition_is_current(&old_plist));

    let unit = manager::systemd::render_definition(exe, &paths);
    let old_unit = unit.replace(" service run --managed", " service run");
    assert_ne!(unit, old_unit);
    assert!(manager::systemd::definition_is_current(&unit));
    assert!(!manager::systemd::definition_is_current(&old_unit));

    let xml = manager::windows::render_definition(exe, &paths);
    assert!(manager::windows::definition_is_current(&xml));
    // As Task Scheduler may hand it back: quotes unescaped, other casing.
    let returned = xml.replace("&quot;", "\"").replace(
        r"%SystemRoot%\System32\conhost.exe",
        r"C:\WINDOWS\system32\CONHOST.EXE",
    );
    assert!(manager::windows::definition_is_current(&returned));
    // Before --managed, and before conhost (a visible console window): both outdated.
    let task = |command: &str, arguments: &str| {
        format!("<Exec><Command>{command}</Command><Arguments>{arguments}</Arguments></Exec>")
    };
    for old in [
        task(EXE, "service run"),
        task(EXE, "service run --managed"),
        task(
            r"C:\Windows\System32\conhost.exe",
            &format!("\"{EXE}\" service run --managed"),
        ),
        task(
            r"C:\Windows\System32\conhost.exe",
            &format!("--headless \"{EXE}\" service run"),
        ),
    ] {
        assert!(!manager::windows::definition_is_current(&old), "{old}");
    }
}

// ---- Registration changes against a real service, with an injected registrar ----

mod changes {
    use std::path::PathBuf;
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use hostler::contract::heavy::HeavyAcquire;
    use hostler::paths::Paths;
    use hostler::procs::{SysProcessTable, current_identity};
    use hostler::service::ServiceManager;
    use hostler::service::manager::{self, ChangeRefused, Registrar};
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
            c.env("HOSTLER_HOME", &self.home)
                .env(
                    "HOSTLER_LOOKY_LEDGER",
                    self.home.join("no-looky-ledger.json"),
                )
                .env_remove("HOSTLER_TEST_CONTRACT_VERSION")
                .env_remove("HOSTLER_TEST_EXIT_AFTER_COMMIT")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            c
        }

        fn run(&self, args: &[&str]) -> Output {
            self.hostler()
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .unwrap()
        }

        /// The same directory seen as the default home, so registrations apply to it.
        fn default_paths(&self) -> Paths {
            let paths = Paths::under(self.home.clone(), false);
            paths.ensure_dirs().unwrap();
            paths
        }

        fn running(&self) -> bool {
            self.run(&["service", "status"]).status.code() == Some(0)
        }

        fn status_json(&self) -> serde_json::Value {
            serde_json::from_slice(&self.run(&["service", "status", "--json"]).stdout).unwrap()
        }

        fn start(&self) {
            assert!(self.run(&["service", "start"]).status.success());
        }

        fn seed_running_entry(&self) {
            let store = Store::open(&self.default_paths().db).unwrap();
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
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = self.run(&["service", "stop", "--force"]);
        }
    }

    /// Waits for `child` to exit by itself; kills it and returns `None` after `limit`.
    fn exits_within(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }

    /// Stands in for launchd/systemd/Task Scheduler. While the registration changes, it checks
    /// that no service is live (the real bootout would stop it) and that a service the manager
    /// starts at load cannot serve.
    struct FakeRegistrar {
        home: Arc<Home>,
        changes: Mutex<Vec<&'static str>>,
        started: Mutex<Vec<Child>>,
    }

    impl FakeRegistrar {
        fn new(home: Arc<Home>) -> FakeRegistrar {
            FakeRegistrar {
                home,
                changes: Mutex::new(Vec::new()),
                started: Mutex::new(Vec::new()),
            }
        }

        fn check_window(&self, what: &'static str) {
            assert!(!self.home.running(), "{what} ran while a service was live");
            let mut at_load = self
                .home
                .hostler()
                .args(["service", "run"])
                .spawn()
                .unwrap();
            let status = exits_within(&mut at_load, Duration::from_secs(5));
            assert!(
                status.is_some_and(|s| s.success()),
                "a service started during {what} kept running: {status:?}"
            );
            assert!(!self.home.running(), "a service served during {what}");
            self.changes.lock().unwrap().push(what);
        }
    }

    impl ServiceManager for FakeRegistrar {
        fn is_installed(&self) -> bool {
            true
        }

        fn start(&self) -> anyhow::Result<()> {
            let child = self.home.hostler().args(["service", "run"]).spawn()?;
            self.started.lock().unwrap().push(child);
            Ok(())
        }
    }

    impl Registrar for FakeRegistrar {
        fn register(&self, _exe: &std::path::Path, _paths: &Paths) -> anyhow::Result<()> {
            self.check_window("register");
            Ok(())
        }

        fn unregister(&self) -> anyhow::Result<()> {
            self.check_window("unregister");
            Ok(())
        }
    }

    impl Drop for FakeRegistrar {
        fn drop(&mut self) {
            let _ = self.home.run(&["service", "stop", "--force"]);
            for mut child in self.started.lock().unwrap().drain(..) {
                exits_within(&mut child, Duration::from_secs(10));
            }
        }
    }

    /// Clients and the OS keep auto-starting services (`service run` and `service start`) for
    /// the whole install: none of them may be live when the registration changes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_never_changes_the_registration_under_a_live_service() {
        let home = Arc::new(Home::new());
        home.start();
        let old_pid = home.status_json()["data"]["pid"].as_u64().unwrap();
        let fake = Arc::new(FakeRegistrar::new(home.clone()));

        let stop = Arc::new(AtomicBool::new(false));
        let racer = {
            let (home, stop) = (home.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut children = Vec::new();
                let mut round = 0;
                while !stop.load(Ordering::SeqCst) {
                    let args: &[&str] = if round % 2 == 0 {
                        &["service", "run"]
                    } else {
                        &["service", "start"]
                    };
                    children.push(home.hostler().args(args).spawn().unwrap());
                    round += 1;
                    std::thread::sleep(Duration::from_millis(25));
                }
                children
            })
        };

        let result =
            manager::install_service(&home.default_paths(), false, BIN.as_ref(), fake.clone())
                .await;
        stop.store(true, Ordering::SeqCst);
        let racers = racer.join().unwrap();

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(*fake.changes.lock().unwrap(), ["register"]);
        assert!(
            !fake.started.lock().unwrap().is_empty(),
            "never started through the manager"
        );
        assert!(home.running());
        assert_ne!(
            home.status_json()["data"]["pid"].as_u64(),
            Some(old_pid),
            "the old service was not replaced"
        );
        let _ = home.run(&["service", "stop", "--force"]);
        for mut child in racers {
            assert!(exits_within(&mut child, Duration::from_secs(10)).is_some());
        }
    }

    #[tokio::test]
    async fn install_while_busy_touches_nothing() {
        let home = Arc::new(Home::new());
        home.seed_running_entry();
        home.start();
        let before = home.status_json();
        let fake = Arc::new(FakeRegistrar::new(home.clone()));

        let result =
            manager::install_service(&home.default_paths(), false, BIN.as_ref(), fake.clone())
                .await;
        assert!(matches!(result, Err(ChangeRefused::Busy(_))), "{result:?}");
        assert!(fake.changes.lock().unwrap().is_empty());
        assert!(fake.started.lock().unwrap().is_empty());
        assert_eq!(home.status_json()["data"]["pid"], before["data"]["pid"]);
        assert_eq!(home.status_json()["data"]["running_heavy"], 1);
    }

    #[tokio::test]
    async fn uninstall_holds_the_lock_and_leaves_the_service_stopped() {
        let home = Arc::new(Home::new());
        home.start();
        let fake = Arc::new(FakeRegistrar::new(home.clone()));

        manager::uninstall_service(&home.default_paths(), false, &*fake)
            .await
            .unwrap();
        assert_eq!(*fake.changes.lock().unwrap(), ["unregister"]);
        assert!(fake.started.lock().unwrap().is_empty());
        assert!(!home.running());
    }

    /// The refusal comes before any lifecycle request: the idle custom-home service keeps its
    /// pid and queue. (Refused before any OS manager call, so this runs the real CLI safely.)
    #[test]
    fn refused_custom_home_install_leaves_the_service_untouched() {
        let home = Home::new();
        home.start();
        let pid = home.status_json()["data"]["pid"].clone();
        for args in [
            &["service", "install"][..],
            &["service", "install", "--force"],
            &["service", "uninstall"],
            &["service", "uninstall", "--force"],
        ] {
            let out = home.run(args);
            assert_eq!(out.status.code(), Some(1), "{args:?}");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains("HOSTLER_HOME"), "{args:?}: {stderr}");
            let status = home.status_json();
            assert_eq!(status["data"]["pid"], pid, "{args:?} touched the service");
            assert_eq!(status["data"]["running_heavy"], 0);
            assert_eq!(status["data"]["waiting_heavy"], 0);
        }
    }

    struct Fixed(bool);

    impl ServiceManager for Fixed {
        fn is_installed(&self) -> bool {
            self.0
        }

        fn start(&self) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }
    }

    static REGISTERED: Fixed = Fixed(true);
    static UNREGISTERED: Fixed = Fixed(false);

    /// A client spawned `service run` just before an install registered hostler: once a
    /// registration exists, that directly started service never serves.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn direct_run_never_serves_once_a_registration_exists() {
        use hostler::service::{RunEnd, lock, run_service_with};

        let home = Home::new();
        let paths = home.default_paths();
        let end = tokio::time::timeout(
            Duration::from_secs(10),
            run_service_with(paths.clone(), false, &REGISTERED),
        )
        .await
        .expect("a stepping-aside service returns at once")
        .unwrap();
        assert_eq!(end, RunEnd::Registered);
        assert!(!lock::is_held(&paths.service_lock).unwrap());
        assert!(!home.running());
    }

    /// The registered (`--managed`) service serves; so does a direct one without a registration.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn managed_or_unregistered_run_serves() {
        use hostler::client::Client;
        use hostler::contract::wire::Request;
        use hostler::service::{RunEnd, run_service_with};

        for (managed, manager) in [(true, &REGISTERED), (false, &UNREGISTERED)] {
            let home = Home::new();
            let paths = home.default_paths();
            let serving = tokio::spawn(run_service_with(paths.clone(), managed, manager));
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut client = loop {
                if let Ok(client) = Client::connect(&paths).await {
                    break client;
                }
                assert!(Instant::now() < deadline, "managed={managed}: never served");
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            client
                .call(Request::ServiceShutdown { if_idle: false })
                .await
                .unwrap();
            assert_eq!(serving.await.unwrap().unwrap(), RunEnd::Stopped);
        }
    }

    /// The registration appears between the client's check and its direct spawn (here: the
    /// spawned program never serves). Within the same 5 s window the client switches to the
    /// manager.
    #[cfg(unix)]
    #[tokio::test]
    async fn start_switches_to_the_manager_when_a_registration_appears() {
        use std::sync::atomic::AtomicUsize;

        use hostler::client::Client;

        struct AppearsAfterFirstCheck {
            home: Arc<Home>,
            checks: AtomicUsize,
            started: Mutex<Option<Child>>,
        }

        impl ServiceManager for AppearsAfterFirstCheck {
            fn is_installed(&self) -> bool {
                self.checks.fetch_add(1, Ordering::SeqCst) > 0
            }

            fn start(&self) -> anyhow::Result<()> {
                let child = self.home.hostler().args(["service", "run"]).spawn()?;
                *self.started.lock().unwrap() = Some(child);
                Ok(())
            }
        }

        let home = Arc::new(Home::new());
        let manager = Arc::new(AppearsAfterFirstCheck {
            home: home.clone(),
            checks: AtomicUsize::new(0),
            started: Mutex::new(None),
        });
        let asked = Instant::now();
        let client =
            Client::connect_or_start_with(&home.default_paths(), &manager, "true".as_ref()).await;
        assert!(client.is_ok(), "{:?}", client.err());
        assert!(
            asked.elapsed() < Duration::from_millis(5500),
            "{:?}",
            asked.elapsed()
        );
        drop(client);
        let _ = home.run(&["service", "stop", "--force"]);
        let mut child = manager
            .started
            .lock()
            .unwrap()
            .take()
            .expect("started via the manager");
        assert!(exits_within(&mut child, Duration::from_secs(10)).is_some());
    }

    /// A fake manager whose calls block like a hung `launchctl`/`systemctl`/`schtasks`.
    struct Slow {
        /// `is_installed` answers false on the first call, true afterwards.
        appears_late: bool,
        slow_check: bool,
        slow_start: bool,
        checks: std::sync::atomic::AtomicUsize,
    }

    impl ServiceManager for Slow {
        fn is_installed(&self) -> bool {
            if self.slow_check {
                std::thread::sleep(Duration::from_secs(6));
            }
            !self.appears_late || self.checks.fetch_add(1, Ordering::SeqCst) > 0
        }

        fn start(&self) -> anyhow::Result<()> {
            if self.slow_start {
                std::thread::sleep(Duration::from_secs(6));
            }
            Ok(())
        }
    }

    async fn start_with_slow(slow: Slow) {
        use hostler::client::Client;

        let home = Home::new();
        let asked = Instant::now();
        // `true` stands in for a hostler binary whose directly spawned service never serves.
        let result =
            Client::connect_or_start_with(&home.default_paths(), &Arc::new(slow), "true".as_ref())
                .await;
        let elapsed = asked.elapsed();
        let err = match result {
            Ok(_) => panic!("no service can answer"),
            Err(e) => e,
        };
        assert_eq!(err.details["reason"], "manager_timeout", "{err:?}");
        assert!(elapsed <= Duration::from_millis(5600), "{elapsed:?}");
    }

    #[tokio::test]
    async fn a_hung_registration_check_stays_within_the_start_window() {
        start_with_slow(Slow {
            appears_late: false,
            slow_check: true,
            slow_start: false,
            checks: Default::default(),
        })
        .await;
    }

    #[tokio::test]
    async fn a_hung_manager_start_stays_within_the_start_window() {
        start_with_slow(Slow {
            appears_late: false,
            slow_check: false,
            slow_start: true,
            checks: Default::default(),
        })
        .await;
    }

    /// The reviewer's case: the registration appears after a direct spawn, then the manager's
    /// start hangs.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_late_registration_with_a_hung_start_stays_within_the_start_window() {
        start_with_slow(Slow {
            appears_late: true,
            slow_check: false,
            slow_start: true,
            checks: Default::default(),
        })
        .await;
    }

    /// The OS manager starts something (an old registration's step-aside service), but nothing
    /// serves: the error says the registration may be outdated and how to refresh it.
    #[tokio::test]
    async fn a_managed_start_without_a_service_points_to_reinstall() {
        use hostler::client::Client;

        let home = Home::new();
        let manager = Arc::new(Slow {
            appears_late: false,
            slow_check: false,
            slow_start: false,
            checks: Default::default(),
        });
        let err = match Client::connect_or_start_with(&home.default_paths(), &manager, BIN.as_ref())
            .await
        {
            Ok(_) => panic!("nothing was started"),
            Err(e) => e,
        };
        assert_eq!(err.details["reason"], "start_timeout");
        assert!(err.message.contains("older hostler"), "{}", err.message);
        assert_eq!(err.recovery.as_deref(), Some("hostler service install"));
        assert!(
            !home.running(),
            "a service was spawned despite the registration"
        );
    }
}
