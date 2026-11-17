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

// ---- P4 Task 4: ProcessType and the registered binary (pure; every OS) ----

/// The service and the emulators and adb server it starts must not be throttled (P4 call 15).
#[test]
fn launchd_plist_has_process_type() {
    let (_dir, paths) = paths();
    let plist = manager::launchd::render_definition(Path::new(EXE), &paths);
    assert!(
        plist.contains("<key>ProcessType</key>\n  <string>Interactive</string>"),
        "{plist}"
    );
    assert!(manager::launchd::definition_is_current(&plist));
}

/// A plist from before P4 (no `ProcessType`, or another type) reads `Outdated`, so `service
/// status` and doctor point at `hostler service install`.
#[test]
fn old_plist_is_outdated() {
    let (_dir, paths) = paths();
    let plist = manager::launchd::render_definition(Path::new(EXE), &paths);
    let without = plist.replace(
        "  <key>ProcessType</key>\n  <string>Interactive</string>\n",
        "",
    );
    assert_ne!(plist, without);
    assert!(!manager::launchd::definition_is_current(&without));
    let standard = plist.replace("<string>Interactive</string>", "<string>Standard</string>");
    assert!(!manager::launchd::definition_is_current(&standard));
    // Whitespace launchd or an editor may reflow is not a difference.
    let reflowed = plist.replace(
        "<key>ProcessType</key>\n  <string>Interactive</string>",
        "<key>ProcessType</key>\t<string>Interactive</string>",
    );
    assert!(manager::launchd::definition_is_current(&reflowed));
    // `Interactive` elsewhere (a path) is not a ProcessType.
    let elsewhere = without.replace(
        "<key>RunAtLoad</key>",
        "<key>Note</key>\n  <string>Interactive</string>\n  <key>RunAtLoad</key>",
    );
    assert!(!manager::launchd::definition_is_current(&elsewhere));
}

#[test]
fn launchd_exe_from_plist() {
    let (_dir, paths) = paths();
    let odd = "/Users/a & b/<bin>/hostler";
    let plist = manager::launchd::render_definition(Path::new(odd), &paths);
    assert_eq!(
        manager::launchd::exe_from_plist(&plist).as_deref(),
        Some(Path::new(odd))
    );
    assert_eq!(manager::launchd::exe_from_plist("<plist></plist>"), None);
}

#[test]
fn systemd_exe_from_unit() {
    let (_dir, paths) = paths();
    for exe in [
        EXE,
        "/home/alice/build%h/hostler",
        "/home/alice/my bin/hostler",
        "/home/alice/a\"b\\c/hostler",
    ] {
        let unit = manager::systemd::render_definition(Path::new(exe), &paths);
        assert_eq!(
            manager::systemd::exe_from_unit(&unit).as_deref(),
            Some(Path::new(exe)),
            "{unit}"
        );
    }
    assert_eq!(manager::systemd::exe_from_unit("[Service]\n"), None);
    assert_eq!(
        manager::systemd::exe_from_unit("ExecStart=\"unterminated\n"),
        None
    );
}

/// R1-10: the hostler path is the quoted token after `--headless`, never `<Command>`.
#[test]
fn windows_exe_from_arguments() {
    let program_files = r"C:\Program Files\hostler\hostler.exe";
    let from = manager::windows::exe_from_arguments;
    assert_eq!(
        from(&format!(
            "--headless \"{program_files}\" service run --managed"
        ))
        .as_deref(),
        Some(Path::new(program_files))
    );
    assert_eq!(
        from(&format!(
            "--headless &quot;{program_files}&quot; service run --managed"
        ))
        .as_deref(),
        Some(Path::new(program_files))
    );
    assert_eq!(
        from(r"--headless C:\hostler\hostler.exe service run --managed").as_deref(),
        Some(Path::new(r"C:\hostler\hostler.exe"))
    );
    assert_eq!(from("garbage"), None);
    assert_eq!(from(""), None);
    assert_eq!(from("--headless \"\" service run --managed"), None);
    // As rendered: the XML's `<Arguments>` round-trips.
    let (_dir, paths) = paths();
    let xml = manager::windows::render_definition(Path::new(program_files), &paths);
    let arguments = xml
        .split_once("<Arguments>")
        .and_then(|(_, rest)| rest.split_once("</Arguments>"))
        .unwrap()
        .0;
    assert_eq!(from(arguments).as_deref(), Some(Path::new(program_files)));
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
    use hostler::service::lifecycle::StopRule;
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
                // L58: a service this command starts stops when this test process is gone.
                .env("HOSTLER_TEST_OWNER_PID", std::process::id().to_string())
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

        let result = manager::install_service(
            &home.default_paths(),
            BIN.as_ref(),
            fake.clone(),
            StopRule::Explicit { force: false },
        )
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

        let result = manager::install_service(
            &home.default_paths(),
            BIN.as_ref(),
            fake.clone(),
            StopRule::Explicit { force: false },
        )
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
                .call(Request::ServiceShutdown {
                    if_idle: false,
                    refuse_if_unlocked: false,
                })
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

        use hostler::client::{Client, Replace};

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
        let client = Client::connect_or_start_with(
            &home.default_paths(),
            &manager,
            "true".as_ref(),
            Replace::Never,
        )
        .await;
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
        use hostler::client::{Client, Replace};

        let home = Home::new();
        let asked = Instant::now();
        // `true` stands in for a hostler binary whose directly spawned service never serves.
        let result = Client::connect_or_start_with(
            &home.default_paths(),
            &Arc::new(slow),
            "true".as_ref(),
            Replace::Never,
        )
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
        use hostler::client::{Client, Replace};

        let home = Home::new();
        // L58: a service this process started itself must watch this test process.
        #[cfg(debug_assertions)]
        hostler::client::set_test_owner_pid(std::process::id());
        let manager = Arc::new(Slow {
            appears_late: false,
            slow_check: false,
            slow_start: false,
            checks: Default::default(),
        });
        let err = match Client::connect_or_start_with(
            &home.default_paths(),
            &manager,
            BIN.as_ref(),
            Replace::Never,
        )
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

    // ---- P4 Task 4: every stop is probe → may_stop → the atomic shutdown (R2-2) ----

    /// A `FakeRegistrar` whose installed definition reads `state`.
    struct WithState {
        fake: Arc<FakeRegistrar>,
        state: manager::RegistrationState,
    }

    impl ServiceManager for WithState {
        fn is_installed(&self) -> bool {
            self.fake.is_installed()
        }

        fn start(&self) -> anyhow::Result<()> {
            self.fake.start()
        }
    }

    impl Registrar for WithState {
        fn register(&self, exe: &std::path::Path, paths: &Paths) -> anyhow::Result<()> {
            self.fake.register(exe, paths)
        }

        fn unregister(&self) -> anyhow::Result<()> {
            self.fake.unregister()
        }

        fn registration(&self) -> manager::RegistrationState {
            self.state
        }
    }

    /// A `hostler service run` speaking `contract` (only the service gets the variable).
    fn service_speaking(home: &Home, contract: &str) -> Child {
        let child = home
            .hostler()
            .env("HOSTLER_TEST_CONTRACT_VERSION", contract)
            .args(["service", "run"])
            .spawn()
            .unwrap();
        let pid = u64::from(child.id());
        let deadline = Instant::now() + Duration::from_secs(15);
        while home.status_json()["data"]["pid"].as_u64() != Some(pid) {
            assert!(Instant::now() < deadline, "the test service never answered");
            std::thread::sleep(Duration::from_millis(50));
        }
        child
    }

    /// One heavy entry marked running, held by `holder`.
    #[cfg_attr(not(unix), allow(dead_code))] // its test is Unix-only
    fn seed_running_entry_of(home: &Home, holder: hostler::procs::ProcessIdentity) {
        let store = Store::open(&home.default_paths().db).unwrap();
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
    }

    /// R1-3: a busy older service is kept with the recovery `service install`; once its work
    /// has ended, that install replaces it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_then_install_replaces() {
        use hostler::client::{Client, Replace};

        let home = Arc::new(Home::new());
        let mut work = Command::new("sleep").arg("60").spawn().unwrap();
        let table = SysProcessTable::new().unwrap();
        seed_running_entry_of(
            &home,
            hostler::procs::identity_of(&table, work.id()).unwrap(),
        );
        let mut old = service_speaking(&home, "3");
        let fake = Arc::new(FakeRegistrar::new(home.clone()));
        let paths = home.default_paths();

        let err =
            match Client::connect_or_start_with(&paths, &fake, BIN.as_ref(), Replace::OnMismatch)
                .await
            {
                Ok(_) => panic!("a busy older service was replaced"),
                Err(e) => e,
            };
        assert_eq!(err.code, hostler::contract::ErrorCode::ContractMismatch);
        let recovery = err.recovery.clone().unwrap();
        assert!(recovery.contains("hostler service install"), "{recovery}");
        assert!(fake.started.lock().unwrap().is_empty());
        assert_eq!(
            home.status_json()["data"]["pid"].as_u64(),
            Some(u64::from(old.id()))
        );

        // The work ends; the service reaps it.
        work.kill().unwrap();
        work.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while home.status_json()["data"]["running_heavy"] != 0 {
            assert!(
                Instant::now() < deadline,
                "the dead holder was never reaped"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        let installed = manager::install_service(
            &paths,
            BIN.as_ref(),
            fake.clone(),
            StopRule::Explicit { force: false },
        )
        .await;
        assert!(
            matches!(installed, Ok(manager::Installed { stopped: true })),
            "{installed:?}"
        );
        assert_eq!(*fake.changes.lock().unwrap(), ["register"]);
        assert!(exits_within(&mut old, Duration::from_secs(10)).is_some());
        let client = Client::connect(&paths).await;
        assert!(client.is_ok(), "{:?}", client.err());
        assert_eq!(home.status_json()["data"]["contract_version"], 4);
    }

    /// R2-2: an automatic install probes first; a newer service is never stopped and nothing
    /// is registered. A typed install of the same state stops it (today's behaviour).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quiesce_probes_before_stopping() {
        use hostler::service::lifecycle::{AutoPurpose, StopRefusal};

        let home = Arc::new(Home::new());
        let mut newer = service_speaking(&home, "5");
        let fake = Arc::new(FakeRegistrar::new(home.clone()));
        let paths = home.default_paths();
        for state in [
            manager::RegistrationState::Outdated,
            manager::RegistrationState::Absent,
        ] {
            let registrar = Arc::new(WithState {
                fake: fake.clone(),
                state,
            });
            let result = manager::install_service(
                &paths,
                BIN.as_ref(),
                registrar,
                StopRule::Automatic(AutoPurpose::Maintain),
            )
            .await;
            assert!(
                matches!(
                    result,
                    Err(ChangeRefused::NotStopped {
                        reason: StopRefusal::NewerService
                    })
                ),
                "{result:?}"
            );
        }
        let replace = manager::install_service(
            &paths,
            BIN.as_ref(),
            fake.clone(),
            StopRule::Automatic(AutoPurpose::Replace),
        )
        .await;
        assert!(
            matches!(
                replace,
                Err(ChangeRefused::NotStopped {
                    reason: StopRefusal::NewerService
                })
            ),
            "{replace:?}"
        );
        assert!(
            fake.changes.lock().unwrap().is_empty(),
            "register was called"
        );
        assert!(
            newer.try_wait().unwrap().is_none(),
            "the newer service stopped"
        );
        assert_eq!(
            home.status_json()["data"]["pid"].as_u64(),
            Some(u64::from(newer.id()))
        );

        let typed = manager::install_service(
            &paths,
            BIN.as_ref(),
            fake.clone(),
            StopRule::Explicit { force: false },
        )
        .await;
        assert!(
            matches!(typed, Ok(manager::Installed { stopped: true })),
            "{typed:?}"
        );
        assert!(exits_within(&mut newer, Duration::from_secs(10)).is_some());
        assert_eq!(*fake.changes.lock().unwrap(), ["register"]);
        assert_eq!(home.status_json()["data"]["contract_version"], 4);
    }

    /// R2-2: the registration-maintenance exception. A same-version, idle, locked service with
    /// an outdated definition (no `ProcessType`) is stopped atomically, re-registered and
    /// started again; with a `Current` definition nothing is touched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn maintain_same_outdated_idle_locked_reinstalls() {
        use hostler::service::lifecycle::{AutoPurpose, StopRefusal};

        let home = Arc::new(Home::new());
        home.start();
        let before = home.status_json();
        assert_eq!(before["data"]["secrets_unlocked"], false);
        let fake = Arc::new(FakeRegistrar::new(home.clone()));
        let paths = home.default_paths();
        let maintain = StopRule::Automatic(AutoPurpose::Maintain);

        for state in [
            manager::RegistrationState::Current,
            manager::RegistrationState::Unknown,
        ] {
            let registrar = Arc::new(WithState {
                fake: fake.clone(),
                state,
            });
            let result = manager::install_service(&paths, BIN.as_ref(), registrar, maintain).await;
            assert!(
                matches!(
                    result,
                    Err(ChangeRefused::NotStopped {
                        reason: StopRefusal::CurrentDefinition
                    })
                ),
                "{state:?}: {result:?}"
            );
            assert_eq!(home.status_json()["data"]["pid"], before["data"]["pid"]);
        }
        // `Replace` never stops a same-version service (P4-L8, R1-2).
        let replace = manager::install_service(
            &paths,
            BIN.as_ref(),
            fake.clone(),
            StopRule::Automatic(AutoPurpose::Replace),
        )
        .await;
        assert!(
            matches!(
                replace,
                Err(ChangeRefused::NotStopped {
                    reason: StopRefusal::NotNewer
                })
            ),
            "{replace:?}"
        );
        assert!(fake.changes.lock().unwrap().is_empty());

        let registrar = Arc::new(WithState {
            fake: fake.clone(),
            state: manager::RegistrationState::Outdated,
        });
        let installed = manager::install_service(&paths, BIN.as_ref(), registrar, maintain).await;
        assert!(
            matches!(installed, Ok(manager::Installed { stopped: true })),
            "{installed:?}"
        );
        assert_eq!(*fake.changes.lock().unwrap(), ["register"]);
        assert!(
            !fake.started.lock().unwrap().is_empty(),
            "never started again"
        );
        let after = home.status_json();
        assert_eq!(after["ok"], true, "{after}");
        assert_ne!(after["data"]["pid"], before["data"]["pid"]);
    }
}
