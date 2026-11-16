//! Claim windows, telemetry and install attribution against the real binary and service, on the
//! fake device tools (P3b Task 8). No real adb, xcrun, plutil or ideviceinfo runs here; every
//! call the fakes logged must pass the exact allowlist (`FakeTools`' drop check). Commands run
//! under `sh`: Unix only.
#![cfg(unix)]

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{BIN, Home, stderr};
use fake_tools::FakeTools;
use hostler::contract::claims::ResourceId;
use hostler::contract::devices::{
    DeviceDetail, DevicesSnapshot, InstallChange, InstallEvent, InstallSource, InstallTime,
};
use hostler::contract::heavy::HistoryQuery;
use hostler::contract::wire::Request;
use hostler::devices::tools::{IDENTITY, SNAPSHOT, TELEMETRY};
use hostler::devices::windows::ClaimWindow;
use hostler::store::Store;
use serde_json::{Value, json};

const PHONE: &str = "SERIAL0001";
const PHONE_ID: &str = "device:android:SERIAL0001";
const EMULATOR: &str = "emulator-5554";
const AVD_ID: &str = "device:android:avd:Pixel_9";
const SIM: &str = "00000000-0000-4000-8000-0000000000A1";
const SIM_ID: &str = "device:ios:00000000-0000-4000-8000-0000000000A1";
const COMMIT: &str = "1234567890abcdef1234567890abcdef12345678";
/// A Wi-Fi name that must appear nowhere but in `devices list` / `show`.
const SSID: &str = "HostlerTestNet-7f3a";
/// An install of `com.example.new` at `@T@` (filled by the command that installs it).
const NEW_APP: (&str, &str) = ("com.example.new", "@T@");

/// What the fakes answer, and which files hold the moving parts.
struct Lab {
    home: Home,
    tools: FakeTools,
    env: Vec<(String, std::ffi::OsString)>,
}

#[derive(Clone, Copy, Default)]
struct Opts {
    /// The Android snapshot takes this long.
    snapshot_delay_ms: u64,
    /// The iOS simulator is listed (booted) too.
    simulator: bool,
    /// The simulator is listed shut down; `simctl boot` takes 1.5 s, then it lists booted.
    simulator_shut_down: bool,
    /// The AVD `Pixel_9` is listed (shut down; `emulator -avd` keeps running 20 s).
    avd: bool,
    /// ... and runs as `emulator-5554`, its apps from `avd-apps.txt`.
    avd_running: bool,
}

impl Lab {
    fn new(opts: Opts, extra: &[(&str, &str)]) -> Lab {
        let home = Home::new();
        let mut tools = FakeTools::new();
        tools.adb_server(41);
        tools.script(Value::Array(script(opts, "simctl.json")));
        let lab_files = |tools: &FakeTools| {
            write_atomic(&tools.dir().join("apps.txt"), &apps_text(&[OLD_APP]));
            write_atomic(&tools.dir().join("telemetry.txt"), &telemetry_text());
            write_atomic(
                &tools.dir().join("devicectl.json"),
                r#"{"info": {"outcome": "success"}, "result": {"devices": []}}"#,
            );
            let sims = |state: &str| {
                let sims = if opts.simulator || opts.simulator_shut_down {
                    json!([{"udid": SIM, "name": "iPhone Air", "state": state,
                            "isAvailable": true, "lastBootedAt": "2026-01-01T00:00:00Z"}])
                } else {
                    json!([])
                };
                json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-26-5": sims}})
                    .to_string()
            };
            let first = if opts.simulator_shut_down {
                "Shutdown"
            } else {
                "Booted"
            };
            write_atomic(&tools.dir().join("simctl.json"), &sims(first));
            write_atomic(&tools.dir().join("simctl-booted.json"), &sims("Booted"));
            write_atomic(
                &tools.dir().join("listapps.json"),
                &listapps(&["com.example.sim0"]),
            );
        };
        lab_files(&tools);
        let sessions = home.dir.path().join("sessions.json");
        std::fs::write(&sessions, r#"{"readable": true, "sessions": []}"#).unwrap();
        let mut env = tools.env();
        for (k, v) in [
            ("HOSTLER_TEST_DEVICES_POLL_MS", "200"),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
            ("HOSTLER_TEST_TAKEOVER_GRACE_MS", "500"),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        {
            env.push((k.to_owned(), v.into()));
        }
        env.push((
            "HOSTLER_TEST_FAKE_SESSIONS".into(),
            sessions.into_os_string(),
        ));
        let lab = Lab { home, tools, env };
        lab.start_service();
        lab
    }

    fn start_service(&self) {
        let out = self
            .home
            .hostler()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .args(["service", "start"])
            .output()
            .unwrap();
        assert!(out.status.success(), "service start: {}", stderr(&out));
    }

    fn file(&self, name: &str) -> PathBuf {
        self.tools.dir().join(name)
    }

    fn list(&self) -> DevicesSnapshot {
        serde_json::from_value(self.home.call(Request::DevicesList { all: true }).unwrap()).unwrap()
    }

    fn show(&self, device: &str) -> DeviceDetail {
        serde_json::from_value(
            self.home
                .call(Request::DevicesShow {
                    device: device.into(),
                    fresh_sessions: false,
                })
                .unwrap(),
        )
        .unwrap()
    }

    /// Seen, registered, checked, and its first (seeding) snapshot taken.
    fn register(&self, device: &str, alias: &str, id: &str, person: bool, seeded: bool) {
        until("the device", || {
            self.list().devices.iter().any(|d| d.resource.0 == id)
        });
        self.home
            .call(Request::DevicesAdd {
                device: device.into(),
                alias: Some(alias.into()),
                person,
            })
            .unwrap();
        until("registered and checked", || {
            self.list()
                .devices
                .iter()
                .any(|d| d.resource.0 == id && d.registered && !d.checking_sessions)
        });
        if seeded {
            until("the seeding snapshot", || {
                self.show(id).snapshot_at_ms.is_some()
            });
        }
    }

    fn phone(&self) {
        self.register(PHONE, "phone", PHONE_ID, true, true);
    }

    fn repo(&self, name: &str) -> PathBuf {
        let repo = std::fs::canonicalize(self.home.make_repo(name)).unwrap();
        let heads = repo.join(".git/refs/heads");
        std::fs::create_dir_all(&heads).unwrap();
        std::fs::write(heads.join("main"), format!("{COMMIT}\n")).unwrap();
        repo
    }

    fn hostler(&self, repo: &Path, args: &[&str]) -> Command {
        let mut c = self.home.hostler();
        c.args(args)
            .current_dir(repo)
            .env("HB", BIN)
            .env_remove("HOSTLER_CLAIM_TOKENS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// `hostler run --device phone -- sh -c <script>` with `$APPS`/`$TPL` set for `install`.
    fn run(&self, repo: &Path, device: &str, script: &str) -> Output {
        self.hostler(repo, &["run", "--device", device, "--", "sh", "-c", script])
            .env("APPS", self.file("apps.txt"))
            .env("TPL", self.file("next.txt"))
            .output()
            .unwrap()
    }

    fn hold(&self, repo: &Path, device: &str, extra: &[&str]) -> String {
        let mut args = vec!["hold", "device", device, "--json"];
        args.extend_from_slice(extra);
        let out = self.hostler(repo, &args).output().unwrap();
        assert!(
            out.status.success(),
            "hold: {}{}",
            stderr(&out),
            String::from_utf8_lossy(&out.stdout)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["data"]["id"].as_str().unwrap().to_owned()
    }

    fn release(&self, repo: &Path, id: &str) {
        let out = self.hostler(repo, &["release", id]).output().unwrap();
        assert!(out.status.success(), "release: {}", stderr(&out));
    }

    fn windows(&self, id: &str) -> Vec<ClaimWindow> {
        let store = Store::open(&self.home.paths().db).unwrap();
        store
            .claim_windows_on(&ResourceId(id.into()), i64::MIN, i64::MAX)
            .unwrap()
    }

    fn window_of(&self, device: &str, claim: &str) -> Option<ClaimWindow> {
        self.windows(device)
            .into_iter()
            .find(|w| w.claim_id == claim)
    }

    fn installs(&self, device: &str) -> Vec<InstallEvent> {
        let v = self
            .home
            .call(Request::DevicesInstalls {
                device: Some(device.into()),
                query: HistoryQuery {
                    since_ms: None,
                    limit: 100,
                },
            })
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    /// Waits for an install event of `app` with `change`.
    fn install_of(&self, device: &str, app: &str, change: InstallChange) -> InstallEvent {
        let mut found = None;
        until(&format!("{change:?} of {app}"), || {
            found = self
                .installs(device)
                .into_iter()
                .find(|e| e.app_id == app && e.change == change);
            found.is_some()
        });
        found.unwrap()
    }

    /// The next snapshot answers `apps` (the files are swapped whole).
    fn apps(&self, apps: &[(&str, &str)]) {
        write_atomic(&self.file("apps.txt"), &apps_text(apps));
    }

    /// The template the installing command fills with its own `date -u` time.
    fn next_apps(&self, apps: &[(&str, &str)]) {
        write_atomic(&self.file("next.txt"), &apps_text(apps));
    }

    fn calls_of(&self, tool: &str, prefix: &[&str]) -> usize {
        self.tools
            .calls()
            .iter()
            .filter(|c| {
                c.tool == tool
                    && c.args
                        .starts_with(&prefix.iter().map(|a| a.to_string()).collect::<Vec<_>>())
            })
            .count()
    }

    fn telemetry_calls(&self) -> usize {
        self.calls_of("adb", &["-s", PHONE, "shell", TELEMETRY])
    }

    /// The next `n` `simctl listapps` calls fail. The script is swapped whole; the simulator
    /// script never rewrites itself, so no fake's write can race this one.
    fn fail_listapps(&self, n: usize) {
        let mut entries = script(
            Opts {
                simulator: true,
                ..Opts::default()
            },
            "simctl.json",
        );
        for _ in 0..n {
            entries.insert(
                0,
                json!({"tool": "xcrun", "args_prefix": ["simctl", "listapps", SIM], "rc": 1,
                       "once": true}),
            );
        }
        write_atomic(
            &self.file("script.json"),
            &Value::Array(entries).to_string(),
        );
    }

    /// When each `simctl listapps` call started.
    fn listapps_times(&self) -> Vec<i64> {
        self.tools
            .calls()
            .iter()
            .filter(|c| {
                c.tool == "xcrun" && c.args.starts_with(&["simctl".into(), "listapps".into()])
            })
            .map(|c| c.at_ms)
            .collect()
    }
}

/// The fakes' answers; `simctl list` from `simctl_file`. A shut-down simulator boots in 1.5 s and
/// then lists from `simctl-booted.json`.
fn script(opts: Opts, simctl_file: &str) -> Vec<Value> {
    let mut entries = vec![
        json!({"tool": "adb", "args_prefix": ["version"],
               "stdout": "Android Debug Bridge version 1.0.41\nVersion 35.0.2-12147458\n"}),
        json!({"tool": "adb", "args_prefix": ["start-server"]}),
        json!({"tool": "adb", "args_prefix": ["devices", "-l"],
               "stdout": format!("List of devices attached\n{PHONE}          device usb:1-1 product:p model:m device:d transport_id:1\n\n")}),
        json!({"tool": "adb", "args_prefix": ["-s", PHONE, "shell", SNAPSHOT],
               "stdout_file": "apps.txt", "delay_ms": opts.snapshot_delay_ms}),
        json!({"tool": "adb", "args_prefix": ["-s", PHONE, "shell", TELEMETRY],
               "stdout_file": "telemetry.txt"}),
        json!({"tool": "adb", "args_prefix": ["-s", PHONE, "shell", IDENTITY],
               "stdout": format!("{PHONE}\nPixel 9\n15\n35\n\n\n\n")}),
        json!({"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": ""}),
        json!({"tool": "xcrun", "args_prefix": ["devicectl", "list", "devices"],
               "json_output_file": "devicectl.json"}),
        json!({"tool": "xcrun", "args_prefix": ["simctl", "list", "devices", "--json"],
               "stdout_file": simctl_file}),
        json!({"tool": "xcrun", "args_prefix": ["simctl", "listapps", SIM],
               "stdout": "{ \"plist\" = 1; }"}),
        json!({"tool": "plutil", "args_prefix": ["-convert", "json"], "stdout_file": "listapps.json"}),
        json!({"tool": "plutil", "args_prefix": ["-extract"], "stdout": "1.0\n"}),
    ];
    if opts.avd {
        entries.insert(
            0,
            json!({"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": "Pixel_9\n"}),
        );
        entries.insert(
            0,
            json!({"tool": "emulator", "args_prefix": ["-avd", "Pixel_9"], "delay_ms": 20000}),
        );
    }
    if opts.avd_running {
        let running = [
            json!({"tool": "adb", "args_prefix": ["devices", "-l"],
                   "stdout": format!("List of devices attached\n{PHONE}          device usb:1-1 product:p model:m device:d transport_id:1\n{EMULATOR}          device product:p model:m device:d transport_id:2\n\n")}),
            json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", "getprop sys.boot_completed"],
                   "stdout": "1\n"}),
            json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", SNAPSHOT],
                   "stdout_file": "avd-apps.txt"}),
            json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", TELEMETRY],
                   "stdout_file": "telemetry.txt"}),
            json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell"],
                   "stdout": "EMULATOR35X1X1X0\nsdk_gphone64_arm64\n15\n35\nPixel_9\n1\n\n"}),
            json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "emu", "avd", "name"],
                   "stdout": "Pixel_9\r\nOK\r\n"}),
        ];
        for (i, e) in running.into_iter().enumerate() {
            entries.insert(i, e);
        }
    }
    if opts.simulator_shut_down {
        let booted = Opts {
            simulator_shut_down: false,
            ..opts
        };
        let mut after = script(booted, "simctl-booted.json");
        after.insert(
            0,
            json!({"tool": "xcrun", "args_prefix": ["simctl", "boot", SIM]}),
        );
        after.insert(
            0,
            json!({"tool": "xcrun", "args_prefix": ["simctl", "bootstatus", SIM, "-b"]}),
        );
        entries.insert(
            0,
            json!({"tool": "xcrun", "args_prefix": ["simctl", "boot", SIM], "delay_ms": 1500,
                   "then_write": after}),
        );
        entries.insert(
            0,
            json!({"tool": "xcrun", "args_prefix": ["simctl", "bootstatus", SIM, "-b"]}),
        );
    }
    entries
}

/// Installs `com.example.new` at the command's own UTC time, then keeps the claim 2 s.
const INSTALL: &str = "T=$(date -u '+%Y-%m-%d %H:%M:%S'); \
    sed \"s/@T@/$T/g\" \"$TPL\" > \"$APPS.tmp\" && mv \"$APPS.tmp\" \"$APPS\"; sleep 2";

const OLD_APP: (&str, &str) = ("com.example.old", "2026-01-01 00:00:00");

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// The SNAPSHOT script's output: the clock (`@NOW_S@`, +0000), the `pm` list, the dump. Each
/// app is `(id, time)`: both install and update times; `update@<time>` on an id moves only
/// `lastUpdateTime`.
fn apps_text(apps: &[(&str, &str)]) -> String {
    let mut pm = String::new();
    let mut dump = String::new();
    for (id, time) in apps {
        let (first, last) = time.split_once('|').unwrap_or((time, time));
        pm.push_str(&format!("package:{id} versionCode:7\n"));
        dump.push_str(&format!(
            "  Package [{id}] (abc):\n    versionCode=7 minSdk=24 targetSdk=35\n    \
             versionName=1.0\n    firstInstallTime={first}\n    lastUpdateTime={last}\n"
        ));
    }
    format!("@NOW_S@\n+0000\n{pm}===\nPackages:\n{dump}")
}

fn telemetry_text() -> String {
    format!(
        "Current Battery Service state:\n  AC powered: false\n  USB powered: true\n  status: 2\n  \
         present: true\n  level: 80\n  scale: 100\n  NetworkAgentInfo{{ni{{WIFI CONNECTED \
         extra: }}  nc{{[ Transports: WIFI Capabilities: NOT_METERED&INTERNET&NOT_VPN ]}}}}\n\
         mWifiInfo SSID: \"{SSID}\", BSSID: 02:00:00:00:00:00, MAC: 02:00:00:00:00:00\n"
    )
}

/// `plutil -convert json` of `simctl listapps`: a System app and these User apps.
fn listapps(user: &[&str]) -> String {
    let mut apps = serde_json::Map::new();
    apps.insert(
        "com.apple.Preferences".into(),
        json!({"CFBundleIdentifier": "com.apple.Preferences", "ApplicationType": "System",
               "CFBundleVersion": "1"}),
    );
    for id in user {
        apps.insert(
            (*id).into(),
            json!({"CFBundleIdentifier": id, "ApplicationType": "User", "CFBundleVersion": "1",
                   "Path": format!("/tmp/sim/{id}.app")}),
        );
    }
    Value::Object(apps).to_string()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// `YYYY-MM-DD HH:MM:SS` in UTC (Hinnant's `civil_from_days`).
fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn claim_of(e: &InstallEvent) -> (Vec<String>, Option<String>, Option<String>) {
    match &e.source {
        InstallSource::Claim {
            claim_ids,
            branch,
            commit,
            ..
        } => (claim_ids.clone(), branch.clone(), commit.clone()),
        other => panic!("not a claim: {other:?} in {e:?}"),
    }
}

/// A run in its own process group, so the whole of it can be killed.
fn spawn_grouped(mut c: Command) -> Child {
    c.process_group(0);
    c.spawn().unwrap()
}

fn kill_group(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-9", &format!("-{}", child.id())])
        .status();
    let _ = child.wait();
}

/// The command of a killable run: it records its pid, then sleeps.
const SLEEPER: &str = "echo $$ > \"$PIDF\"; exec sleep 30";

impl Lab {
    /// `run --device phone` of `SLEEPER`, in its own process group.
    fn sleeper(&self, repo: &Path) -> Child {
        let mut c = self.hostler(
            repo,
            &["run", "--device", "phone", "--", "sh", "-c", SLEEPER],
        );
        c.env("PIDF", self.file("sleeper.pid"));
        spawn_grouped(c)
    }

    /// The wrapper and its command (in the run's own group) die at once.
    fn kill_sleeper(&self, run: &mut Child) {
        let pid = std::fs::read_to_string(self.file("sleeper.pid")).unwrap();
        let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        kill_group(run);
    }
}

#[test]
fn install_during_a_lease_is_attributed_with_branch_and_commit() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    lab.next_apps(&[OLD_APP, NEW_APP]);
    let out = lab.run(&repo, "phone", INSTALL);
    assert!(out.status.success(), "{}", stderr(&out));
    let event = lab.install_of(PHONE_ID, "com.example.new", InstallChange::Installed);
    assert!(
        matches!(event.when, InstallTime::Device { .. }),
        "{event:?}"
    );
    let (ids, branch, commit) = claim_of(&event);
    assert_eq!(ids.len(), 1, "{event:?}");
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(commit.as_deref(), Some(COMMIT));
    let window = lab.window_of(PHONE_ID, &ids[0]).unwrap();
    assert!(window.to_ms.is_some(), "{window:?}");
    assert!(window.command.unwrap().contains("sh -c"));
}

#[test]
fn install_between_claims_is_not_through_hostler() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    let id = lab.hold(&repo, "phone", &[]);
    lab.release(&repo, &id);
    // 3.5 s from either claim: beyond the 1 s slack, the time's whole-second rounding and the
    // fake clock line's skew (up to 1 s each).
    std::thread::sleep(Duration::from_millis(3_500));
    lab.apps(&[OLD_APP, ("com.example.new", &utc(now_ms()))]);
    std::thread::sleep(Duration::from_millis(3_500));
    let id = lab.hold(&repo, "phone", &[]);
    lab.release(&repo, &id);
    let event = lab.install_of(PHONE_ID, "com.example.new", InstallChange::Installed);
    assert_eq!(event.source, InstallSource::NotThroughHostler, "{event:?}");
}

#[test]
fn ttl_hold_window_closes_at_expiry() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    // 0.05 min = 3 s.
    let id = lab.hold(&repo, "phone", &["--ttl", "0.05"]);
    let mut window = None;
    until("the window closes", || {
        window = lab.window_of(PHONE_ID, &id).filter(|w| w.to_ms.is_some());
        window.is_some()
    });
    let w = window.unwrap();
    let length = w.to_ms.unwrap() - w.from_ms;
    assert!((3_000..6_000).contains(&length), "{w:?}");
}

#[test]
fn while_parent_hold_window_closes_when_the_parent_dies() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    let out = lab.file("hold.json");
    let mut parent = spawn_grouped({
        let mut c = Command::new("sh");
        c.args([
            "-c",
            "\"$HB\" hold device phone --while-parent --json > \"$OUT\"; sleep 30",
        ])
        .current_dir(&repo)
        .envs(
            lab.home
                .hostler()
                .get_envs()
                .filter_map(|(k, v)| Some((k, v?))),
        )
        .env("HB", BIN)
        .env("OUT", &out)
        .env_remove("HOSTLER_CLAIM_TOKENS");
        c
    });
    let mut id = String::new();
    until("the hold", || {
        let v: Option<Value> = std::fs::read(&out)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        id = v
            .and_then(|v| v["data"]["id"].as_str().map(str::to_owned))
            .unwrap_or_default();
        !id.is_empty()
    });
    assert!(lab.window_of(PHONE_ID, &id).unwrap().to_ms.is_none());
    let killed = now_ms();
    kill_group(&mut parent);
    let mut window = None;
    until("the window closes", || {
        window = lab.window_of(PHONE_ID, &id).filter(|w| w.to_ms.is_some());
        window.is_some()
    });
    let to = window.unwrap().to_ms.unwrap();
    assert!(to >= killed - 1_000, "{to} vs {killed}");
}

#[test]
fn killed_run_window_closes_at_reap() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    let mut run = lab.sleeper(&repo);
    let mut id = String::new();
    until("the run's window", || {
        id = lab
            .windows(PHONE_ID)
            .into_iter()
            .find(|w| w.to_ms.is_none())
            .map(|w| w.claim_id)
            .unwrap_or_default();
        !id.is_empty()
    });
    let killed = now_ms();
    lab.kill_sleeper(&mut run);
    let mut window = None;
    until("the window closes", || {
        window = lab.window_of(PHONE_ID, &id).filter(|w| w.to_ms.is_some());
        window.is_some()
    });
    let to = window.unwrap().to_ms.unwrap();
    assert!(
        (killed - 1_000..killed + 10_000).contains(&to),
        "{to}: killed {killed}"
    );
}

#[test]
fn seized_lease_window_closes() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let (a, b) = (lab.repo("a"), lab.repo("b"));
    let mut run = lab
        .hostler(&a, &["run", "--device", "phone", "--", "sleep", "30"])
        .spawn()
        .unwrap();
    let mut seized = String::new();
    until("A's window", || {
        seized = lab
            .windows(PHONE_ID)
            .into_iter()
            .find(|w| w.to_ms.is_none())
            .map(|w| w.claim_id)
            .unwrap_or_default();
        !seized.is_empty()
    });
    let taker = lab.hold(&b, "phone", &["--force", "--confirmation", "phone"]);
    let _ = run.wait();
    until("A's window closes", || {
        lab.window_of(PHONE_ID, &seized)
            .is_some_and(|w| w.to_ms.is_some())
    });
    until("B's window opens", || {
        lab.window_of(PHONE_ID, &taker)
            .is_some_and(|w| w.to_ms.is_none())
    });
}

#[test]
fn window_closed_after_a_restart_for_a_claim_that_ended_meanwhile() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    let id = lab.hold(&repo, "phone", &["--ttl", "0.05"]);
    until("its window", || lab.window_of(PHONE_ID, &id).is_some());
    let out = lab
        .home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let stopped = now_ms();
    // It expires while no service runs.
    std::thread::sleep(Duration::from_millis(3_500));
    lab.start_service();
    let mut window = None;
    until("the window closes", || {
        window = lab.window_of(PHONE_ID, &id).filter(|w| w.to_ms.is_some());
        window.is_some()
    });
    let w = window.unwrap();
    // At the last alive time before the stop, never at the restart.
    let to = w.to_ms.unwrap();
    assert!(
        to >= w.from_ms && to <= stopped,
        "{w:?}, stopped at {stopped}"
    );
}

#[test]
fn run_device_spawn_is_not_delayed_by_snapshots() {
    let lab = Lab::new(
        Opts {
            snapshot_delay_ms: 20_000,
            ..Opts::default()
        },
        &[],
    );
    // Registered, its 20 s seeding snapshot under way.
    lab.register(PHONE, "phone", PHONE_ID, true, false);
    until("a snapshot under way", || {
        lab.calls_of("adb", &["-s", PHONE, "shell", SNAPSHOT]) > 0
    });
    let repo = lab.repo("a");
    let started = Instant::now();
    let child = lab
        .hostler(
            &repo,
            &["run", "--device", "phone", "--", "sh", "-c", "echo started"],
        )
        .spawn()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("started"));
    // The whole run, start to end, far below the 20 s snapshot.
    assert!(took < Duration::from_secs(8), "{took:?}");
    assert!(
        lab.show(PHONE_ID).snapshot_at_ms.is_none(),
        "still under way"
    );
}

#[test]
fn ios_change_in_a_short_hold_is_attributed_with_branch_and_commit() {
    let lab = Lab::new(
        Opts {
            simulator: true,
            ..Opts::default()
        },
        &[],
    );
    lab.register(SIM, "iphone-air", SIM_ID, false, true);
    let repo = lab.repo("a");
    let converts = || lab.calls_of("plutil", &["-convert", "json"]);
    let before = converts();
    let id = lab.hold(&repo, "iphone-air", &[]);
    // The snapshot at the window's start.
    until("the open snapshot", || converts() > before);
    write_atomic(
        &lab.file("listapps.json"),
        &listapps(&["com.example.sim0", "com.example.sim1"]),
    );
    lab.release(&repo, &id);
    let event = lab.install_of(SIM_ID, "com.example.sim1", InstallChange::Installed);
    assert!(
        matches!(event.when, InstallTime::ChangedBetween { .. }),
        "{event:?}"
    );
    let (ids, branch, commit) = claim_of(&event);
    assert_eq!(ids, vec![id]);
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(commit.as_deref(), Some(COMMIT));
    assert_eq!(event.version_name.as_deref(), Some("1.0"));
}

#[test]
fn android_install_then_outside_update_keeps_the_claim_attribution() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    lab.next_apps(&[OLD_APP, NEW_APP]);
    let out = lab.run(&repo, "phone", INSTALL);
    assert!(out.status.success(), "{}", stderr(&out));
    // The close snapshot saw the install inside the lease.
    let installed = lab.install_of(PHONE_ID, "com.example.new", InstallChange::Installed);
    let (ids, _, _) = claim_of(&installed);
    let InstallTime::Device { at_ms: first } = installed.when else {
        panic!("{installed:?}")
    };
    // Updated later, outside any claim; the next claim's close snapshot sees it.
    // 3.5 s from either claim, as in `install_between_claims_is_not_through_hostler`.
    std::thread::sleep(Duration::from_millis(3_500));
    let update = format!("{}|{}", utc(first), utc(now_ms()));
    lab.apps(&[OLD_APP, ("com.example.new", &update)]);
    std::thread::sleep(Duration::from_millis(3_500));
    let id = lab.hold(&repo, "phone", &[]);
    lab.release(&repo, &id);
    let updated = lab.install_of(PHONE_ID, "com.example.new", InstallChange::Updated);
    assert_eq!(
        updated.source,
        InstallSource::NotThroughHostler,
        "{updated:?}"
    );
    let again = lab
        .installs(PHONE_ID)
        .into_iter()
        .find(|e| e.change == InstallChange::Installed)
        .unwrap();
    assert_eq!(claim_of(&again).0, ids, "the install keeps its claim");
}

#[test]
fn startup_reap_closes_at_the_last_alive_time() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    let mut run = lab.sleeper(&repo);
    let mut id = String::new();
    until("the run's window", || {
        id = lab
            .windows(PHONE_ID)
            .into_iter()
            .find(|w| w.to_ms.is_none())
            .map(|w| w.claim_id)
            .unwrap_or_default();
        !id.is_empty()
    });
    // A second of alive marks, then the service and the run die at once.
    std::thread::sleep(Duration::from_millis(1_000));
    let status: Value = lab.home.call(Request::ServiceStatus).unwrap();
    let pid = status["pid"].as_u64().unwrap().to_string();
    let killed = now_ms();
    let _ = Command::new("kill").args(["-9", &pid]).status();
    lab.kill_sleeper(&mut run);
    std::thread::sleep(Duration::from_millis(2_000));
    let restarted = now_ms();
    lab.start_service();
    let mut window = None;
    until("the window closes", || {
        window = lab.window_of(PHONE_ID, &id).filter(|w| w.to_ms.is_some());
        window.is_some()
    });
    let to = window.unwrap().to_ms.unwrap();
    // The last alive mark before the kill, not the reap at the restart.
    assert!(to <= killed && to < restarted, "{to}: killed {killed}");
    assert!(to >= killed - 2_000, "{to}: killed {killed}");
}

#[test]
fn telemetry_is_quiet_after_a_claim_starts() {
    let lab = Lab::new(
        Opts::default(),
        &[("HOSTLER_TEST_TELEMETRY_QUIET_MS", "4000")],
    );
    lab.phone();
    until("telemetry polls", || lab.telemetry_calls() >= 2);
    let repo = lab.repo("a");
    let id = lab.hold(&repo, "phone", &[]);
    // At most one call that was already under way at the grant.
    let at_grant = lab.telemetry_calls();
    std::thread::sleep(Duration::from_millis(3_000));
    let quiet = lab.telemetry_calls();
    assert!(quiet <= at_grant + 1, "{at_grant} -> {quiet}");
    // After the quiet time, every (test-shortened) 5 min again.
    until("telemetry resumes", || lab.telemetry_calls() > quiet + 1);
    lab.release(&repo, &id);
}

#[test]
fn ssid_never_in_logs_or_history() {
    let lab = Lab::new(Opts::default(), &[("RUST_LOG", "hostler=debug")]);
    lab.phone();
    until("the Wi-Fi name in devices list", || {
        lab.list().devices.iter().any(|d| {
            d.telemetry
                .as_ref()
                .is_some_and(|t| t.wifi_name.as_deref() == Some(SSID))
        })
    });
    let repo = lab.repo("a");
    lab.next_apps(&[OLD_APP, NEW_APP]);
    let out = lab.run(&repo, "phone", INSTALL);
    assert!(out.status.success(), "{}", stderr(&out));
    lab.install_of(PHONE_ID, "com.example.new", InstallChange::Installed);
    let history = lab
        .home
        .call(Request::ClaimsHistory(HistoryQuery {
            since_ms: None,
            limit: 100,
        }))
        .unwrap();
    assert!(!history.to_string().contains(SSID), "{history}");
    let installs = serde_json::to_string(&lab.installs(PHONE_ID)).unwrap();
    assert!(!installs.contains(SSID), "{installs}");
    let logs = std::fs::read_dir(lab.home.home.join("logs")).unwrap();
    let mut read = 0;
    for f in logs.filter_map(Result::ok) {
        let text = std::fs::read_to_string(f.path()).unwrap_or_default();
        read += text.len();
        assert!(!text.contains(SSID), "{}", f.path().display());
    }
    assert!(read > 0, "a debug log was written");
}

/// R1-2: a short hold, released at once: its install event names the branch and commit of the
/// grant, though the checkout moved to another branch right after it.
#[test]
fn short_hold_event_names_the_head_at_the_grant() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let repo = lab.repo("a");
    let id = lab.hold(&repo, "phone", &[]);
    let mut window = None;
    until("its window", || {
        window = lab.window_of(PHONE_ID, &id);
        window.is_some()
    });
    let window = window.unwrap();
    assert_eq!(
        (window.branch.as_deref(), window.head_commit.as_deref()),
        (Some("main"), Some(COMMIT)),
        "recorded with the window"
    );
    // The checkout moves on; an install happens; the hold ends at once.
    let other = "fedcba9876543210fedcba9876543210fedcba98";
    std::fs::write(repo.join(".git/refs/heads/other"), format!("{other}\n")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
    lab.apps(&[OLD_APP, ("com.example.new", &utc(now_ms()))]);
    lab.release(&repo, &id);
    let event = lab.install_of(PHONE_ID, "com.example.new", InstallChange::Installed);
    let (ids, branch, commit) = claim_of(&event);
    assert_eq!(ids, vec![id]);
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(commit.as_deref(), Some(COMMIT));
}

/// Task 13 F-2: the device's `date +%s` jitters (and real time passes) between two snapshots of
/// the same apps, so every converted time moves: still nothing is reported.
#[test]
fn clock_jitter_between_snapshots_reports_nothing() {
    let lab = Lab::new(Opts::default(), &[]);
    let apps = [OLD_APP, ("com.example.other", "2026-02-01 12:00:00")];
    let epoch = now_ms() / 1000;
    let at = |epoch: i64| apps_text(&apps).replacen("@NOW_S@", &epoch.to_string(), 1);
    write_atomic(&lab.file("apps.txt"), &at(epoch));
    lab.phone();
    let seeded = lab.show(PHONE_ID).snapshot_at_ms.unwrap();
    // A second behind, read 1.5 s later: the skew moves by ~2.5 s.
    std::thread::sleep(Duration::from_millis(1_500));
    write_atomic(&lab.file("apps.txt"), &at(epoch - 1));
    let repo = lab.repo("a");
    let id = lab.hold(&repo, "phone", &[]);
    lab.release(&repo, &id);
    until("the close snapshot", || {
        lab.show(PHONE_ID)
            .snapshot_at_ms
            .is_some_and(|t| t > seeded)
    });
    assert!(
        lab.installs(PHONE_ID).is_empty(),
        "{:?}",
        lab.installs(PHONE_ID)
    );
}

/// L69: an Android snapshot without its clock lines is a failed read: nothing is stored or
/// diffed, the stored clock stays; the next whole snapshot still sees a same-version update.
#[test]
fn a_snapshot_without_its_clock_is_a_failed_read() {
    let lab = Lab::new(Opts::default(), &[]);
    lab.phone();
    let seeded = lab.show(PHONE_ID).snapshot_at_ms.unwrap();
    let key = format!("devices.clock.{PHONE_ID}");
    let clock = || {
        Store::open(&lab.home.paths().db)
            .unwrap()
            .setting(&key)
            .unwrap()
    };
    let stored = clock();
    assert!(stored.is_some());
    // The same app updated (same version), its clock lines missing.
    let moved = [(OLD_APP.0, "2026-01-01 00:00:00|2026-01-05 00:00:00")];
    let without = apps_text(&moved).replacen("@NOW_S@\n+0000\n", "", 1);
    write_atomic(&lab.file("apps.txt"), &without);
    let calls = || lab.calls_of("adb", &["-s", PHONE, "shell", SNAPSHOT]);
    let before = calls();
    let repo = lab.repo("a");
    let id = lab.hold(&repo, "phone", &[]);
    lab.release(&repo, &id);
    until("the close snapshot's read", || calls() > before);
    // Nothing comes of it, checked over 3 s (well past the fake's answer and the diff).
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        assert_eq!(
            lab.show(PHONE_ID).snapshot_at_ms,
            Some(seeded),
            "nothing stored"
        );
        assert_eq!(clock(), stored, "the clock stays");
        assert!(lab.installs(PHONE_ID).is_empty());
        std::thread::sleep(Duration::from_millis(100));
    }
    // Whole again: the update is seen.
    lab.apps(&moved);
    let id = lab.hold(&repo, "phone", &[]);
    lab.release(&repo, &id);
    lab.install_of(PHONE_ID, OLD_APP.0, InstallChange::Updated);
}

/// Task 13 F-3: a `--boot` hold on a shut-down simulator: its window opens while the simulator
/// still boots (the open snapshot finds it not ready), so the claim-open snapshot is taken once
/// it is booted, exactly once; an install after it, inside the hold, is the claim's.
#[test]
fn a_boot_hold_gets_its_claim_open_snapshot_once_booted() {
    let lab = Lab::new(
        Opts {
            simulator_shut_down: true,
            ..Opts::default()
        },
        &[],
    );
    // Registered while shut down: no seeding snapshot yet.
    lab.register(SIM, "iphone-air", SIM_ID, false, false);
    let repo = lab.repo("a");
    let converts = || lab.calls_of("plutil", &["-convert", "json"]);
    let before = converts();
    let id = lab.hold(&repo, "iphone-air", &["--boot"]);
    assert_eq!(lab.calls_of("xcrun", &["simctl", "boot", SIM]), 1);
    // The device's first snapshot (it seeds) is the claim-open one.
    until("the claim-open snapshot", || converts() > before);
    until("it is recorded", || {
        lab.show(SIM_ID).snapshot_at_ms.is_some()
    });
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(converts(), before + 1, "one claim-open snapshot");
    write_atomic(
        &lab.file("listapps.json"),
        &listapps(&["com.example.sim0", "com.example.sim1"]),
    );
    lab.release(&repo, &id);
    let event = lab.install_of(SIM_ID, "com.example.sim1", InstallChange::Installed);
    let (ids, branch, commit) = claim_of(&event);
    assert_eq!(ids, vec![id]);
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(commit.as_deref(), Some(COMMIT));
}

/// R3-1: a claim-open snapshot whose read keeps failing is tried again with growing gaps (the
/// test-shortened 200 ms, doubling per failure), not on every 200 ms pass; after the good read
/// it is owed no more.
#[test]
fn a_failing_claim_open_snapshot_backs_off() {
    let lab = Lab::new(
        Opts {
            simulator: true,
            ..Opts::default()
        },
        &[],
    );
    lab.register(SIM, "iphone-air", SIM_ID, false, true);
    let repo = lab.repo("a");
    lab.fail_listapps(4);
    let converts = || lab.calls_of("plutil", &["-convert", "json"]);
    let before = converts();
    let tried = lab.listapps_times().len();
    let id = lab.hold(&repo, "iphone-air", &[]);
    until("the claim-open snapshot", || converts() > before);
    let times = lab.listapps_times()[tried..].to_vec();
    assert_eq!(
        times.len(),
        5,
        "four failed reads, then the good one: {times:?}"
    );
    // Each try starts at least the backoff after the failed one began (a lower bound: load
    // only makes the gaps longer).
    for (k, pair) in times.windows(2).enumerate() {
        assert!(pair[1] - pair[0] >= 200 << k, "gap {k}: {times:?}");
    }
    // No more owed tries: wait past the next backoff step (200 ms << 4 = 3.2 s), with slack
    // (review R4-1).
    std::thread::sleep(Duration::from_millis(5_500));
    assert_eq!(lab.listapps_times().len(), tried + 5, "owed no more");
    lab.release(&repo, &id);
}

/// R3-2: the close snapshot's read fails once. It is retried soon, within the plan's 30 s close
/// rule, and books the close, so the change inside the short hold is the claim's.
#[test]
fn a_failed_close_snapshot_is_retried_and_attributes() {
    let lab = Lab::new(
        Opts {
            simulator: true,
            ..Opts::default()
        },
        &[],
    );
    lab.register(SIM, "iphone-air", SIM_ID, false, true);
    let repo = lab.repo("a");
    let converts = || lab.calls_of("plutil", &["-convert", "json"]);
    let before = converts();
    let id = lab.hold(&repo, "iphone-air", &[]);
    until("the open snapshot", || converts() > before);
    write_atomic(
        &lab.file("listapps.json"),
        &listapps(&["com.example.sim0", "com.example.sim1"]),
    );
    lab.fail_listapps(1);
    let tried = lab.listapps_times().len();
    lab.release(&repo, &id);
    let event = lab.install_of(SIM_ID, "com.example.sim1", InstallChange::Installed);
    let (ids, branch, commit) = claim_of(&event);
    assert_eq!(ids, vec![id]);
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(commit.as_deref(), Some(COMMIT));
    let times = lab.listapps_times()[tried..].to_vec();
    assert_eq!(
        times.len(),
        2,
        "the failed close read, then one retry: {times:?}"
    );
    assert!(times[1] - times[0] >= 200, "{times:?}");
}

/// F-4: an AVD added while shut down gets no seed then (no read ran, so none is recorded as
/// tried); a `--boot` hold boots it, and the boot-ready wake takes its seed at once, inside the
/// claim. An install after that seed, in the claim, is the claim's. (An app installed before
/// the seed is the device's state at registration: the seed records it without an event.)
#[test]
fn an_avd_added_shut_down_is_seeded_after_its_boot() {
    let opts = Opts {
        avd: true,
        ..Opts::default()
    };
    let lab = Lab::new(opts, &[("HOSTLER_TEST_BOOT_TIMEOUT_MS", "20000")]);
    write_atomic(&lab.file("avd-apps.txt"), &apps_text(&[OLD_APP]));
    lab.register("Pixel_9", "pixel9", AVD_ID, false, false);
    let repo = lab.repo("a");
    let hold = lab
        .hostler(&repo, &["hold", "device", "pixel9", "--boot", "--json"])
        .spawn()
        .unwrap();
    until("the boot", || lab.calls_of("emulator", &["-avd"]) > 0);
    let running = Opts {
        avd_running: true,
        ..opts
    };
    write_atomic(
        &lab.file("script.json"),
        &Value::Array(script(running, "simctl.json")).to_string(),
    );
    let out = hold.wait_with_output().unwrap();
    assert!(out.status.success(), "hold: {}", stderr(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v["data"]["id"].as_str().unwrap().to_owned();
    // The seed, inside the claim: before the fix it waited for the close (or 30 min).
    until("the seed after the boot", || {
        lab.show(AVD_ID).snapshot_at_ms.is_some()
    });
    assert!(lab.installs(AVD_ID).is_empty(), "a seed reports nothing");
    // An install a second later, inside the claim, then the release.
    std::thread::sleep(Duration::from_millis(1_100));
    write_atomic(
        &lab.file("avd-apps.txt"),
        &apps_text(&[OLD_APP, ("com.example.new", &utc(now_ms()))]),
    );
    lab.release(&repo, &id);
    let event = lab.install_of(AVD_ID, "com.example.new", InstallChange::Installed);
    assert!(
        matches!(event.when, InstallTime::Device { .. }),
        "{event:?}"
    );
    let (ids, branch, commit) = claim_of(&event);
    assert_eq!(ids, vec![id]);
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(commit.as_deref(), Some(COMMIT));
}
