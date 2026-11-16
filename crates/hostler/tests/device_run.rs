//! `hostler run --device`, `hostler hold device` and `--boot` against the real binary and service,
//! on the fake device tools (P3b Task 7). No real adb, emulator, xcrun or agent-device runs here;
//! every call the fakes logged must pass the exact allowlist (`FakeTools`' drop check).
//! Commands run under `sh`: Unix only.
#![cfg(unix)]

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{BIN, Home, stderr, stdout};
use fake_tools::FakeTools;
use hostler::contract::Envelope;
use hostler::contract::claims::{Claim, ClaimsSnapshot, ResourceId};
use hostler::contract::devices::DevicesSnapshot;
use hostler::contract::wire::Request;
use serde_json::{Value, json};

const PHONE: &str = "SERIAL0001";
const PHONE2: &str = "SERIAL0002";
const WIFI: &str = "192.0.2.10:5555";
const EMULATOR: &str = "emulator-5554";
const PHONE_ID: &str = "device:android:SERIAL0001";
const AVD_ID: &str = "device:android:avd:Pixel_9";
/// Waits for `$GATE` to exist: a command that runs until the test lets it end.
const GATED: &str = "while [ ! -e \"$GATE\" ]; do sleep 0.05; done";
const ENV_LINES: &str = "env | grep -E '^(ANDROID_SERIAL|HOSTLER_DEVICE_[A-Z_]+|HOSTLER_AGENT_DEVICE_ID|HOSTLER_LEASE_ID)=' \
     | sort";

/// What the fake adb and emulator answer.
#[derive(Clone, Copy, Default)]
struct World {
    phone: bool,
    phone2: bool,
    /// The phone on Wi-Fi instead of USB.
    wifi: bool,
    /// The AVD runs as emulator-5554.
    emulator: bool,
    /// `adb devices -l` fails (a person's `adb kill-server`, another adb meanwhile).
    adb_broken: bool,
}

fn line(serial: &str, usb: Option<&str>, id: u32) -> String {
    let usb = usb.map_or_else(String::new, |u| format!(" usb:{u}"));
    format!("{serial}          device{usb} product:p model:m device:d transport_id:{id}\n")
}

fn script(w: World) -> Value {
    let mut devices = "List of devices attached\n".to_owned();
    if w.phone && !w.wifi {
        devices.push_str(&line(PHONE, Some("1-1"), 1));
    }
    if w.phone && w.wifi {
        devices.push_str(&line(WIFI, None, 3));
    }
    if w.phone2 {
        devices.push_str(&line(PHONE2, Some("1-2"), 4));
    }
    if w.emulator {
        devices.push_str(&line(EMULATOR, None, 2));
    }
    devices.push('\n');
    let phone_id = |serial: &str| format!("{serial}\nPixel 9\n15\n35\n\n\n\n");
    let mut entries = vec![
        json!({"tool": "adb", "args_prefix": ["version"],
               "stdout": "Android Debug Bridge version 1.0.41\nVersion 35.0.2-12147458\n"}),
        json!({"tool": "adb", "args_prefix": ["start-server"]}),
        if w.adb_broken {
            json!({"tool": "adb", "args_prefix": ["devices", "-l"], "rc": 1})
        } else {
            json!({"tool": "adb", "args_prefix": ["devices", "-l"], "stdout": devices})
        },
        json!({"tool": "adb", "args_prefix": ["-s", PHONE, "shell"], "stdout": phone_id(PHONE)}),
        json!({"tool": "adb", "args_prefix": ["-s", WIFI, "shell"], "stdout": phone_id(PHONE)}),
        json!({"tool": "adb", "args_prefix": ["-s", PHONE2, "shell"], "stdout": phone_id(PHONE2)}),
        json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", "getprop sys.boot_completed"],
               "stdout": "1\n"}),
        json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell"],
               "stdout": "EMULATOR35X1X1X0\nsdk_gphone64_arm64\n15\n35\nPixel_9\n1\n\n"}),
        json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "emu", "avd", "name"],
               "stdout": "Pixel_9\r\nOK\r\n"}),
        json!({"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": "Pixel_9\n"}),
        // A booted emulator keeps running (its pid is the recorded identity).
        json!({"tool": "emulator", "args_prefix": ["-avd", "Pixel_9"], "delay_ms": 20000}),
    ];
    // `emu kill` stops a running one: discovery no longer lists it.
    let mut kill = json!({"tool": "adb", "args_prefix": ["-s", EMULATOR, "emu", "kill"]});
    if w.emulator {
        kill["then_write"] = script(World {
            emulator: false,
            ..w
        });
    }
    entries.insert(0, kill);
    Value::Array(entries)
}

struct Lab {
    home: Home,
    tools: FakeTools,
    sessions: PathBuf,
    /// The service's environment, for a restart.
    env: Vec<(String, std::ffi::OsString)>,
}

impl Lab {
    fn new(world: World, extra: &[(&str, &str)]) -> Lab {
        Lab::start(world, extra, None)
    }

    /// `sessions`: the `HOSTLER_TEST_FAKE_SESSIONS` file before the service starts.
    fn start(world: World, extra: &[(&str, &str)], sessions: Option<Value>) -> Lab {
        let home = Home::new();
        let mut tools = FakeTools::new();
        tools.adb_server(41);
        tools.script(script(world));
        let path = home.dir.path().join("sessions.json");
        if let Some(file) = sessions {
            std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        }
        let sessions = path;
        let mut env = tools.env();
        for (k, v) in [
            ("HOSTLER_TEST_DEVICES_POLL_MS", "200"),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
            ("HOSTLER_TEST_BOOT_TIMEOUT_MS", "20000"),
            ("HOSTLER_TEST_SHUTDOWN_GRACE_MS", "800"),
            ("HOSTLER_TEST_TAKEOVER_GRACE_MS", "500"),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        {
            env.push((k.to_owned(), v.into()));
        }
        env.push((
            "HOSTLER_TEST_FAKE_SESSIONS".to_owned(),
            sessions.clone().into_os_string(),
        ));
        let lab = Lab {
            home,
            tools,
            sessions,
            env,
        };
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

    /// `service stop --force`, then a start with the same environment.
    fn restart(&self) {
        let out = self
            .home
            .hostler()
            .args(["service", "stop", "--force"])
            .output()
            .unwrap();
        assert!(out.status.success(), "service stop: {}", stderr(&out));
        self.start_service();
    }

    /// The emulator process hostler started ends (the person quits it): the fake sleeps until
    /// killed.
    fn emulator_process_ends(&self) {
        let bin = self.tools.bin(hostler::devices::tools::Tool::Emulator);
        let _ = Command::new("pkill")
            .args(["-f", &format!("{} -avd", bin.display())])
            .status();
    }

    fn world(&self, w: World) {
        self.tools.script(script(w));
    }

    fn list(&self) -> DevicesSnapshot {
        serde_json::from_value(self.home.call(Request::DevicesList { all: true }).unwrap()).unwrap()
    }

    fn until(&self, what: &str, done: impl Fn(&DevicesSnapshot) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let s = self.list();
            if done(&s) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: {s:#?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn device(
        &self,
        s: &DevicesSnapshot,
        id: &str,
    ) -> Option<hostler::contract::devices::DeviceInfo> {
        s.devices.iter().find(|d| d.resource.0 == id).cloned()
    }

    /// Seen, then registered (a physical device through the person's flag, as the CLI sends it).
    fn register(&self, device: &str, id: &str, alias: Option<&str>) {
        self.until(device, |s| self.device(s, id).is_some());
        self.home
            .call(Request::DevicesAdd {
                device: device.into(),
                alias: alias.map(Into::into),
                person: true,
            })
            .unwrap();
        // A first good sessions read (L52) and the catalog in the engine.
        self.until("registered and checked", |s| {
            self.device(s, id)
                .is_some_and(|d| d.registered && !d.checking_sessions)
        });
    }

    fn phone(&self) {
        self.register(PHONE, PHONE_ID, Some("phone"));
    }

    fn avd(&self) {
        self.register("Pixel_9", AVD_ID, None);
    }

    /// `{"readable": …, "sessions": […]}` for `HOSTLER_TEST_FAKE_SESSIONS`.
    fn sessions(&self, readable: bool, sessions: Value) {
        let file = json!({"readable": readable, "reason": "node not found", "sessions": sessions});
        let tmp = self.sessions.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&file).unwrap()).unwrap();
        std::fs::rename(&tmp, &self.sessions).unwrap();
    }

    fn repo(&self, name: &str) -> PathBuf {
        std::fs::canonicalize(self.home.make_repo(name)).unwrap()
    }

    fn workspace_path(&self, repo: &Path) -> String {
        self.home.workspace(repo).unwrap().path
    }

    fn run(&self, repo: &Path, args: &[&str]) -> Command {
        let mut c = self.home.hostler();
        c.arg("run")
            .args(args)
            .current_dir(repo)
            .env("HB", BIN)
            .env_remove("HOSTLER_CLAIM_TOKENS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn gated(&self, repo: &Path, flags: &[&str], gate: &Path) -> Child {
        let mut args = flags.to_vec();
        args.extend(["--", "sh", "-c", GATED]);
        self.run(repo, &args).env("GATE", gate).spawn().unwrap()
    }

    fn hold(&self, repo: &Path, args: &[&str]) -> Command {
        let mut c = self.home.hostler();
        c.arg("hold")
            .args(args)
            .current_dir(repo)
            .env_remove("HOSTLER_CLAIM_TOKENS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn snapshot(&self) -> ClaimsSnapshot {
        self.home.snapshot()
    }

    fn claims_on(&self, id: &str) -> Vec<Claim> {
        self.snapshot()
            .resources
            .into_iter()
            .filter(|s| s.resource.id.0 == id)
            .flat_map(|s| s.claims)
            .collect()
    }

    fn waiting(&self) -> usize {
        self.snapshot()
            .resources
            .iter()
            .map(|s| s.waiting.len())
            .sum()
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

    fn kills(&self) -> usize {
        self.calls_of("adb", &["-s", EMULATOR, "emu", "kill"])
    }

    /// The emulator started by `--boot` comes up: the fake adb lists it from then on.
    fn emulator_comes_up(&self, w: World) {
        assert!(
            wait_for(Duration::from_secs(20), || self
                .calls_of("emulator", &["-avd"])
                > 0),
            "no emulator -avd call: {:?}",
            self.tools.calls()
        );
        self.world(World {
            emulator: true,
            ..w
        });
    }
}

fn open(gate: &Path) {
    std::fs::write(gate, "").unwrap();
}

fn finish(mut child: Child, limit: Duration) -> Output {
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "the command did not exit within {limit:?}: {}",
                stderr(&out)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
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

fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .args([&format!("-{name}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{name}");
}

fn started(lab: &Lab, id: &str) -> Claim {
    let mut found = None;
    assert!(
        wait_for(Duration::from_secs(20), || {
            found = lab.claims_on(id).into_iter().find(|c| c.child.is_some());
            found.is_some()
        }),
        "no started lease on {id}"
    );
    found.unwrap()
}

fn phone_world() -> World {
    World {
        phone: true,
        ..World::default()
    }
}

fn session(resource: &str, name: &str, workspace: &str) -> Value {
    json!({"resource": resource, "session": name, "workspace_path": workspace,
           "platform": "android", "agent_device_id": PHONE})
}

// --- Leases --------------------------------------------------------------------------------

#[test]
fn run_device_exports_the_device() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let out = lab
        .run(
            &repo,
            &["--device", "phone", "--screen", "--", "sh", "-c", ENV_LINES],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let env = stdout(&out);
    for want in [
        "ANDROID_SERIAL=SERIAL0001",
        "HOSTLER_AGENT_DEVICE_ID=SERIAL0001",
        "HOSTLER_DEVICE_ALIAS=phone",
        "HOSTLER_DEVICE_ID=SERIAL0001",
        "HOSTLER_DEVICE_PLATFORM=android",
        "HOSTLER_LEASE_ID=",
    ] {
        assert!(env.contains(want), "{want} missing: {env}");
    }
    let text = stderr(&out);
    assert!(
        text.contains("[hostler] device phone: released; free."),
        "{text}"
    );
    assert!(text.contains("[hostler] screen: released."), "{text}");
    assert!(lab.claims_on(PHONE_ID).is_empty());
}

#[test]
fn named_unconnected_device_waits_not_substituted() {
    let lab = Lab::new(
        World {
            phone2: true,
            ..phone_world()
        },
        &[],
    );
    lab.phone();
    lab.register(PHONE2, "device:android:SERIAL0002", Some("phone-2"));
    lab.world(World {
        phone2: true,
        ..World::default()
    });
    lab.until("the phone is gone", |s| {
        lab.device(s, PHONE_ID).is_some_and(|d| !d.connected)
    });
    let repo = lab.repo("app");
    let out = lab
        .run(
            &repo,
            &["--device", "phone", "--max-wait", "0.05", "--", "true"],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains(
            "Waiting for device phone: it is not registered or not connected (see hostler \
             devices list)."
        ),
        "{text}"
    );
    assert!(
        lab.claims_on("device:android:SERIAL0002").is_empty(),
        "substituted"
    );
    // An unknown name is refused, not waited for.
    let out = lab
        .run(&repo, &["--device", "nosuch", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
}

#[test]
fn auto_takes_the_first_free_and_a_second_run_waits() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    // A registered AVD that does not run is skipped by `auto` (ready: false).
    lab.avd();
    let (a, b) = (lab.repo("a"), lab.repo("b"));
    let gate = lab.home.dir.path().join("gate");
    let first = lab.gated(&a, &["--device", "auto"], &gate);
    let lease = started(&lab, PHONE_ID);
    assert_eq!(lease.resource.display, "phone");
    let second = lab
        .run(
            &b,
            &["--device", "auto:android", "--", "sh", "-c", ENV_LINES],
        )
        .spawn()
        .unwrap();
    assert!(wait_for(Duration::from_secs(10), || lab.waiting() == 1));
    assert!(lab.claims_on(AVD_ID).is_empty());
    open(&gate);
    let first = finish(first, Duration::from_secs(20));
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(stderr(&first).contains("[hostler] device phone: released; free."));
    let second = finish(second, Duration::from_secs(20));
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert!(stdout(&second).contains("ANDROID_SERIAL=SERIAL0001"));
    assert!(stderr(&second).contains("Waiting for device auto:android: held by "));
}

#[test]
fn same_workspace_runs_share_a_device() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let gate = lab.home.dir.path().join("gate");
    let first = lab.gated(&repo, &["--device", "phone"], &gate);
    started(&lab, PHONE_ID);
    let out = lab
        .run(&repo, &["--device", "phone", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("[hostler] device phone: released; this workspace's run "),
        "{}",
        stderr(&out)
    );
    open(&gate);
    assert_eq!(
        finish(first, Duration::from_secs(20)).status.code(),
        Some(0)
    );
}

#[test]
fn distinct_holds_get_distinct_devices() {
    let lab = Lab::new(
        World {
            phone2: true,
            ..phone_world()
        },
        &[],
    );
    lab.phone();
    lab.register(PHONE2, "device:android:SERIAL0002", Some("phone-2"));
    let repo = lab.repo("e2e");
    let hold = |extra: &[&str]| {
        let mut args = vec!["device", "auto", "--distinct", "--json"];
        args.extend_from_slice(extra);
        lab.hold(&repo, &args).output().unwrap()
    };
    let mut got = Vec::new();
    for _ in 0..2 {
        let out = hold(&[]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let claim: Envelope<Claim> = serde_json::from_str(stdout(&out).trim()).unwrap();
        let claim = claim.data.unwrap();
        assert!(claim.resource.attributes.contains_key("agent_device_id"));
        got.push(claim);
    }
    assert_ne!(got[0].resource.id, got[1].resource.id);
    let out = hold(&["--no-wait"]);
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    for claim in got {
        let out = lab
            .home
            .hostler()
            .args(["release", &claim.id])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
    }
}

#[test]
fn wifi_switch_during_a_lease_keeps_it() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let gate = lab.home.dir.path().join("gate");
    let first = lab.gated(&repo, &["--device", "phone"], &gate);
    let lease = started(&lab, PHONE_ID);
    lab.world(World {
        wifi: true,
        ..phone_world()
    });
    lab.until("the phone on Wi-Fi", |s| {
        lab.device(s, PHONE_ID)
            .is_some_and(|d| d.connected && d.transports.first().is_some_and(|t| t.address == WIFI))
    });
    let now: Vec<String> = lab.claims_on(PHONE_ID).into_iter().map(|c| c.id).collect();
    assert_eq!(now, [lease.id]);
    // A new run of this workspace exports the transport of now.
    let out = lab
        .run(&repo, &["--device", "phone", "--", "sh", "-c", ENV_LINES])
        .output()
        .unwrap();
    assert!(
        stdout(&out).contains(&format!("ANDROID_SERIAL={WIFI}")),
        "{}",
        stdout(&out)
    );
    open(&gate);
    assert_eq!(
        finish(first, Duration::from_secs(20)).status.code(),
        Some(0)
    );
}

#[test]
fn takeover_of_a_device_lease_by_alias() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let (a, b) = (lab.repo("a"), lab.repo("b"));
    let gate = lab.home.dir.path().join("gate");
    let first = lab.gated(&a, &["--device", "phone"], &gate);
    started(&lab, PHONE_ID);
    // The token is the alias, not the id.
    let out = lab
        .run(
            &b,
            &[
                "--device",
                "phone",
                "--force",
                "--confirmation",
                PHONE,
                "--",
                "true",
            ],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = lab
        .run(
            &b,
            &[
                "--device",
                "phone",
                "--force",
                "--confirmation",
                "phone",
                "--",
                "true",
            ],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let first = finish(first, Duration::from_secs(20));
    assert_ne!(first.status.code(), Some(0));
    assert!(
        stderr(&first).contains("[hostler] device phone: taken over by "),
        "{}",
        stderr(&first)
    );
}

#[test]
fn exit_summary_free_reserved_shutdown() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let ws = lab.home.workspace(&repo).unwrap();
    lab.home
        .call(Request::ClaimsReserve {
            resource: ResourceId(PHONE_ID.into()),
            workspace_id: ws.id.clone(),
        })
        .unwrap();
    let out = lab
        .run(&repo, &["--device", "phone", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "[hostler] device phone: released; still reserved by this workspace (hostler \
             devices unreserve phone frees it)."
        ),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("shut down"), "{}", stderr(&out));
}

#[test]
fn no_service_refuses_78() {
    let home = Home::new();
    let repo = home.make_repo("a");
    std::fs::write(home.paths().db, b"this is not a hostler database").unwrap();
    let marker = home.dir.path().join("ran");
    let out = home
        .hostler()
        .args(["run", "--device", "phone", "--", "touch"])
        .arg(&marker)
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(!marker.exists(), "the command ran without the service");
    let out = home
        .hostler()
        .args(["hold", "device", "phone"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
}

// --- Boot ----------------------------------------------------------------------------------

fn avd_world() -> World {
    World::default()
}

#[test]
fn not_ready_without_boot_refuses_78() {
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("app");
    let marker = lab.home.dir.path().join("ran");
    let out = lab
        .run(
            &repo,
            &[
                "--device",
                "pixel_9",
                "--",
                "touch",
                marker.to_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("pixel_9 is not booted"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("rerun with --boot"),
        "{}",
        stderr(&out)
    );
    assert!(!marker.exists());
    assert!(lab.claims_on(AVD_ID).is_empty(), "the claim was released");
    let out = lab
        .hold(&repo, &["device", "pixel_9", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(lab.claims_on(AVD_ID).is_empty());
    assert_eq!(lab.calls_of("emulator", &["-avd"]), 0);
}

#[test]
fn boot_runs_and_shuts_down_after_grace() {
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("app");
    let child = lab
        .run(
            &repo,
            &["--device", "pixel_9", "--boot", "--", "sh", "-c", ENV_LINES],
        )
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    let out = finish(child, Duration::from_secs(30));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("ANDROID_SERIAL=emulator-5554"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("HOSTLER_DEVICE_ID=Pixel_9"),
        "{}",
        stdout(&out)
    );
    let text = stderr(&out);
    assert!(
        text.contains("[hostler] device pixel_9: released; free."),
        "{text}"
    );
    assert!(
        text.contains(
            "[hostler] device pixel_9: shut down in 2 min unless claimed (hostler booted it)."
        ),
        "{text}"
    );
    // The headless vector, once.
    let boots: Vec<Vec<String>> = lab
        .tools
        .calls()
        .into_iter()
        .filter(|c| c.tool == "emulator" && c.args.first().is_some_and(|a| a == "-avd"))
        .map(|c| c.args)
        .collect();
    assert_eq!(
        boots,
        [[
            "-avd",
            "Pixel_9",
            "-no-window",
            "-no-audio",
            "-no-boot-anim",
            "-no-snapshot-save"
        ]]
    );
    // After the grace: the identity check, then the shutdown; then it is gone.
    assert!(
        wait_for(Duration::from_secs(20), || lab.kills() == 1),
        "{:?}",
        lab.tools.calls()
    );
    assert!(lab.calls_of("adb", &["-s", EMULATOR, "emu", "avd", "name"]) >= 1);
    lab.until("shut down and forgotten", |s| {
        lab.device(s, AVD_ID)
            .is_some_and(|d| !d.ready && !d.booted_by_hostler && !d.shutting_down)
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(lab.kills(), 1);
}

#[test]
fn hand_boot_after_hostler_boot_is_never_shut_down() {
    let lab = Lab::new(avd_world(), &[("HOSTLER_TEST_SHUTDOWN_GRACE_MS", "3000")]);
    lab.avd();
    let repo = lab.repo("app");
    let child = lab
        .run(&repo, &["--device", "pixel_9", "--boot", "--", "true"])
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    assert_eq!(
        finish(child, Duration::from_secs(30)).status.code(),
        Some(0)
    );
    // The person stops it (its process ends), then boots it again by hand.
    lab.emulator_process_ends();
    lab.world(avd_world());
    lab.until("stopped and forgotten", |s| {
        lab.device(s, AVD_ID)
            .is_some_and(|d| d.transports.is_empty() && !d.booted_by_hostler)
    });
    lab.world(World {
        emulator: true,
        ..avd_world()
    });
    lab.until("running again", |s| {
        lab.device(s, AVD_ID).is_some_and(|d| d.ready)
    });
    std::thread::sleep(Duration::from_millis(4500));
    assert_eq!(lab.kills(), 0, "{:?}", lab.tools.calls());
}

#[test]
fn boot_not_shut_down_while_a_session_lives() {
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("app");
    let path = lab.workspace_path(&repo);
    let child = lab
        .run(&repo, &["--device", "pixel_9", "--boot", "--", "true"])
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    lab.sessions(true, json!([session(AVD_ID, "e2e", &path)]));
    assert_eq!(
        finish(child, Duration::from_secs(30)).status.code(),
        Some(0)
    );
    lab.until("the session is seen", |s| {
        lab.device(s, AVD_ID)
            .is_some_and(|d| !d.sessions.is_empty())
    });
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(lab.kills(), 0, "shut down under a live session");
    lab.sessions(true, json!([]));
    assert!(wait_for(Duration::from_secs(20), || lab.kills() == 1));
}

#[test]
fn ctrl_c_during_boot_releases_and_cleans_up() {
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("app");
    let marker = lab.home.dir.path().join("ran");
    let child = lab
        .run(
            &repo,
            &[
                "--device",
                "pixel_9",
                "--boot",
                "--",
                "touch",
                marker.to_str().unwrap(),
            ],
        )
        .spawn()
        .unwrap();
    assert!(wait_for(Duration::from_secs(20), || {
        lab.calls_of("emulator", &["-avd"]) > 0
    }));
    let signalled = Instant::now();
    signal(&child, "INT");
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    // The release never queues behind the boot (P3bT7-2).
    assert!(
        signalled.elapsed() < Duration::from_secs(2),
        "{:?}",
        signalled.elapsed()
    );
    assert!(!stderr(&out).contains("not answering"), "{}", stderr(&out));
    assert!(!marker.exists());
    assert!(lab.claims_on(AVD_ID).is_empty(), "the claim was released");
    // The service finishes the boot; nobody claims it; the shutdown rule cleans up.
    lab.until("recorded as hostler's", |s| {
        lab.device(s, AVD_ID).is_some_and(|d| d.booted_by_hostler)
    });
    lab.world(World {
        emulator: true,
        ..avd_world()
    });
    assert!(
        wait_for(Duration::from_secs(20), || lab.kills() == 1),
        "{:?}",
        lab.tools.calls()
    );
}

#[test]
fn hold_json_after_boot_has_the_emulator_serial() {
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("e2e");
    let child = lab
        .hold(&repo, &["device", "pixel_9", "--boot", "--json"])
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    let out = finish(child, Duration::from_secs(30));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let claim: Envelope<Claim> = serde_json::from_str(stdout(&out).trim()).unwrap();
    let claim = claim.data.unwrap();
    assert_eq!(claim.resource.attributes["serial"], EMULATOR);
    assert_eq!(claim.resource.attributes["agent_device_id"], EMULATOR);
    assert_eq!(claim.resource.attributes["ready"], "true");
    assert_eq!(claim.resource.display, "pixel_9");
    // Held: never shut down under it.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(lab.kills(), 0);
    let out = lab
        .home
        .hostler()
        .args(["release", &claim.id])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(wait_for(Duration::from_secs(20), || lab.kills() == 1));
}

// --- agent-device sessions -----------------------------------------------------------------

#[test]
fn checking_blocker_renders_with_the_fix_text() {
    // Unreadable from the start: no good read, so every device is closed (L52, L54).
    let lab = Lab::start(
        phone_world(),
        &[("HOSTLER_TEST_DEVICES_STALE_MS", "300")],
        Some(json!({"readable": false, "reason": "node not found"})),
    );
    lab.until("the phone", |s| lab.device(s, PHONE_ID).is_some());
    lab.home
        .call(Request::DevicesAdd {
            device: PHONE.into(),
            alias: Some("phone".into()),
            person: true,
        })
        .unwrap();
    lab.until("sessions unknown", |s| !s.tools.sessions_known);
    let repo = lab.repo("app");
    let out = lab
        .run(
            &repo,
            &[
                "--device",
                "phone",
                "--force",
                "--confirmation",
                "phone",
                "--max-wait",
                "0.03",
                "--",
                "true",
            ],
        )
        .output()
        .unwrap();
    // --force waits on it too (P3bT1-8): nothing to take over.
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains(
            "Waiting for device phone: checking for agent-device sessions on phone \
             (agent-device status unreadable: node not found; fix: hostler devices tools \
             --detect, or hostler devices tools --agent-device <path>; a person may release \
             this device: hostler devices forget-sessions phone)."
        ),
        "{text}"
    );
}

#[test]
fn no_wait_on_checking_only_waits_one_read_bound() {
    let lab = Lab::new(phone_world(), &[("HOSTLER_TEST_DEVICES_STALE_MS", "300")]);
    lab.phone();
    let (a, b) = (lab.repo("a"), lab.repo("b"));
    // Readable: B asks right after A's end, with --no-wait; the read that clears it comes.
    let out = lab
        .run(&a, &["--device", "phone", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let out = lab
        .run(&b, &["--device", "phone", "--no-wait", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    lab.until("read after B's end", |s| {
        lab.device(s, PHONE_ID)
            .is_some_and(|d| !d.checking_sessions)
    });
    // Unreadable: Busy at once, with the fix.
    lab.sessions(false, json!([]));
    lab.until("sessions unknown", |s| !s.tools.sessions_known);
    let out = lab
        .run(&a, &["--device", "phone", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let asked = Instant::now();
    let out = lab
        .run(&b, &["--device", "phone", "--no-wait", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "{:?}",
        asked.elapsed()
    );
    assert!(
        stderr(&out).contains("hostler devices forget-sessions phone"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn run_end_warns_about_an_open_session() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let path = lab.workspace_path(&repo);
    lab.sessions(true, json!([session(PHONE_ID, "default", &path)]));
    lab.until("the session", |s| {
        lab.device(s, PHONE_ID)
            .is_some_and(|d| !d.sessions.is_empty())
    });
    let out = lab
        .run(&repo, &["--device", "phone", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "[hostler] agent-device session default is still open on phone: it blocks other \
             workspaces until closed: agent-device close --session default --platform android \
             --serial SERIAL0001"
        ),
        "{}",
        stderr(&out)
    );
    // `hostler release` of a hold says the same.
    let out = lab
        .hold(&repo, &["device", "phone", "--json"])
        .output()
        .unwrap();
    let claim: Envelope<Claim> = serde_json::from_str(stdout(&out).trim()).unwrap();
    let out = lab
        .home
        .hostler()
        .args(["release", &claim.data.unwrap().id])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("agent-device close --session default"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn run_end_warns_about_a_session_from_another_cwd_first_seen_in_the_window() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let gate = lab.home.dir.path().join("gate");
    let child = lab.gated(&repo, &["--device", "phone"], &gate);
    started(&lab, PHONE_ID);
    // The agent `cd`'d elsewhere and opened a session there, inside the claim's window.
    lab.sessions(true, json!([session(PHONE_ID, "cdd", "/elsewhere/tmp")]));
    lab.until("the new session", |s| {
        lab.device(s, PHONE_ID)
            .is_some_and(|d| d.sessions.len() == 1)
    });
    open(&gate);
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains("agent-device session cdd is still open on phone"),
        "{text}"
    );
}

#[test]
fn run_end_with_status_unknown_prints_sessions_unknown_at_once() {
    let lab = Lab::new(phone_world(), &[("HOSTLER_TEST_DEVICES_STALE_MS", "300")]);
    lab.phone();
    let repo = lab.repo("app");
    let gate = lab.home.dir.path().join("gate");
    let child = lab.gated(&repo, &["--device", "phone"], &gate);
    started(&lab, PHONE_ID);
    lab.sessions(false, json!([]));
    lab.until("sessions unknown", |s| !s.tools.sessions_known);
    let opened = Instant::now();
    open(&gate);
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("[hostler] agent-device sessions unknown: node not found"),
        "{}",
        stderr(&out)
    );
    assert!(
        opened.elapsed() < Duration::from_secs(5),
        "{:?}",
        opened.elapsed()
    );
}

#[test]
fn no_disallowed_calls() {
    // A boot, a shutdown and the discovery around them: every call on the allowlist (the
    // fakes' drop check), and no state change but the boot and the shutdown.
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("app");
    let child = lab
        .run(&repo, &["--device", "pixel_9", "--boot", "--", "true"])
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    assert_eq!(
        finish(child, Duration::from_secs(30)).status.code(),
        Some(0)
    );
    assert!(wait_for(Duration::from_secs(20), || lab.kills() == 1));
    for call in lab.tools.calls() {
        let changes = (call.tool == "emulator" && call.args.first().is_some_and(|a| a == "-avd"))
            || call
                .args
                .iter()
                .any(|a| a == "kill" || a == "shutdown" || a == "boot");
        if changes {
            assert!(
                call.args == ["-s", EMULATOR, "emu", "kill"] || call.args[0] == "-avd",
                "{call:?}"
            );
        }
    }
}

// --- Fix round 1 -----------------------------------------------------------------------------

#[test]
fn boot_record_survives_a_service_restart() {
    // P3bT7-1: the first tick after a start runs before any discovery.
    let lab = Lab::new(avd_world(), &[("HOSTLER_TEST_SHUTDOWN_GRACE_MS", "4000")]);
    lab.avd();
    let repo = lab.repo("app");
    let child = lab
        .run(&repo, &["--device", "pixel_9", "--boot", "--", "true"])
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    assert_eq!(
        finish(child, Duration::from_secs(30)).status.code(),
        Some(0)
    );
    lab.restart();
    lab.until("hostler's again after the start", |s| {
        lab.device(s, AVD_ID)
            .is_some_and(|d| d.booted_by_hostler && d.ready)
    });
    assert_eq!(lab.kills(), 0, "not before the grace");
    assert!(
        wait_for(Duration::from_secs(20), || lab.kills() == 1),
        "{:?}",
        lab.tools.calls()
    );
}

#[test]
fn adb_failing_during_the_grace_keeps_the_record() {
    let lab = Lab::new(
        avd_world(),
        &[
            ("HOSTLER_TEST_SHUTDOWN_GRACE_MS", "3000"),
            ("HOSTLER_TEST_DEVICES_STALE_MS", "300"),
        ],
    );
    lab.avd();
    let repo = lab.repo("app");
    let child = lab
        .run(&repo, &["--device", "pixel_9", "--boot", "--", "true"])
        .spawn()
        .unwrap();
    lab.emulator_comes_up(avd_world());
    assert_eq!(
        finish(child, Duration::from_secs(30)).status.code(),
        Some(0)
    );
    // A person's `adb kill-server`: adb fails, the emulator vanishes from the cache.
    lab.world(World {
        emulator: true,
        adb_broken: true,
        ..avd_world()
    });
    lab.until("no transport while adb fails", |s| {
        lab.device(s, AVD_ID)
            .is_some_and(|d| d.transports.is_empty())
    });
    std::thread::sleep(Duration::from_millis(4000));
    assert_eq!(lab.kills(), 0);
    lab.until("still hostler's", |s| {
        lab.device(s, AVD_ID).is_some_and(|d| d.booted_by_hostler)
    });
    // adb is back: shut down after its grace, as always.
    lab.world(World {
        emulator: true,
        ..avd_world()
    });
    assert!(
        wait_for(Duration::from_secs(20), || lab.kills() == 1),
        "{:?}",
        lab.tools.calls()
    );
}

#[test]
fn hold_ctrl_c_during_boot_releases_the_hold() {
    let lab = Lab::new(avd_world(), &[]);
    lab.avd();
    let repo = lab.repo("e2e");
    let child = lab
        .hold(&repo, &["device", "pixel_9", "--boot", "--json"])
        .spawn()
        .unwrap();
    assert!(wait_for(Duration::from_secs(20), || {
        lab.calls_of("emulator", &["-avd"]) > 0
    }));
    let signalled = Instant::now();
    signal(&child, "INT");
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(
        signalled.elapsed() < Duration::from_secs(2),
        "{:?}",
        signalled.elapsed()
    );
    assert!(
        stderr(&out).contains("[hostler] Interrupted; the hold was released."),
        "{}",
        stderr(&out)
    );
    assert!(lab.claims_on(AVD_ID).is_empty(), "a hold was left behind");
}

#[test]
fn device_gone_at_spawn_refuses_78_and_does_not_run() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let (a, b) = (lab.repo("a"), lab.repo("b"));
    // The heavy slot is busy: the device run waits there after its grant.
    let gate = lab.home.dir.path().join("gate");
    let heavy = lab.gated(&a, &["--heavy"], &gate);
    assert!(wait_for(Duration::from_secs(20), || {
        lab.home
            .call(Request::QueueSnapshot)
            .ok()
            .is_some_and(|q| q["running"].as_array().is_some_and(|e| !e.is_empty()))
    }));
    let marker = lab.home.dir.path().join("ran");
    let run = lab
        .run(
            &b,
            &[
                "--device",
                "phone",
                "--heavy",
                "--",
                "touch",
                marker.to_str().unwrap(),
            ],
        )
        .spawn()
        .unwrap();
    started_or_granted(&lab, PHONE_ID);
    // Unplugged while it waits for the slot.
    lab.world(World::default());
    lab.until("the phone is gone", |s| {
        lab.device(s, PHONE_ID).is_some_and(|d| !d.connected)
    });
    open(&gate);
    assert_eq!(
        finish(heavy, Duration::from_secs(20)).status.code(),
        Some(0)
    );
    let out = finish(run, Duration::from_secs(30));
    assert_eq!(out.status.code(), Some(78), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("phone is not connected now; the command was not run"),
        "{}",
        stderr(&out)
    );
    assert!(!marker.exists(), "the command ran without its device");
    assert!(lab.claims_on(PHONE_ID).is_empty());
}

/// A lease on `id` exists (granted, its command not started yet).
fn started_or_granted(lab: &Lab, id: &str) {
    assert!(
        wait_for(Duration::from_secs(20), || !lab.claims_on(id).is_empty()),
        "no lease on {id}"
    );
}

#[test]
fn inherited_android_serial_is_replaced() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let out = lab
        .run(&repo, &["--device", "phone", "--", "sh", "-c", ENV_LINES])
        .env("ANDROID_SERIAL", "emulator-5556")
        .env(
            "HOSTLER_DEVICE_UDID",
            "00000000-0000-4000-8000-000000000009",
        )
        .env("HOSTLER_DEVICE_STALE", "x")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let env = stdout(&out);
    assert!(env.contains("ANDROID_SERIAL=SERIAL0001"), "{env}");
    assert!(!env.contains("emulator-5556"), "{env}");
    assert!(!env.contains("HOSTLER_DEVICE_UDID"), "{env}");
    assert!(!env.contains("HOSTLER_DEVICE_STALE"), "{env}");
}

#[test]
fn expired_hold_with_an_open_session_is_logged() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("e2e");
    let path = lab.workspace_path(&repo);
    lab.sessions(true, json!([session(PHONE_ID, "default", &path)]));
    lab.until("the session", |s| {
        lab.device(s, PHONE_ID)
            .is_some_and(|d| !d.sessions.is_empty())
    });
    let out = lab
        .hold(&repo, &["device", "phone", "--ttl", "0.02", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let claim: Envelope<Claim> = serde_json::from_str(stdout(&out).trim()).unwrap();
    let id = claim.data.unwrap().id;
    let mut detail = None;
    assert!(wait_for(Duration::from_secs(20), || {
        let history: Vec<hostler::contract::claims::ClaimHistoryItem> = serde_json::from_value(
            lab.home
                .call(Request::ClaimsHistory(
                    hostler::contract::heavy::HistoryQuery {
                        since_ms: None,
                        limit: 100,
                    },
                ))
                .unwrap(),
        )
        .unwrap();
        detail = history
            .into_iter()
            .find(|h| h.id == id)
            .and_then(|h| h.detail);
        detail.is_some()
    }));
    assert_eq!(
        detail.as_deref(),
        Some(
            "agent-device session default is still open on phone: it blocks other workspaces \
             until closed: agent-device close --session default --platform android --serial \
             SERIAL0001"
        )
    );
    let log = std::fs::read_dir(lab.home.paths().log_dir)
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect::<String>();
    assert!(
        log.contains("agent-device close --session default"),
        "not in the log"
    );
}

// --- CRB-4 (L63) ------------------------------------------------------------------------------

#[test]
fn boot_not_shut_down_while_sessions_are_unknown() {
    let lab = Lab::new(
        avd_world(),
        &[
            ("HOSTLER_TEST_SHUTDOWN_GRACE_MS", "1500"),
            ("HOSTLER_TEST_DEVICES_STALE_MS", "300"),
        ],
    );
    lab.avd();
    let repo = lab.repo("app");
    let gate = lab.home.dir.path().join("gate");
    let child = lab.gated(&repo, &["--device", "pixel_9", "--boot"], &gate);
    lab.emulator_comes_up(avd_world());
    started(&lab, AVD_ID);
    // After good (empty) reads the status turns unreadable while the run still holds the
    // device: the cache still says "no session", which says nothing (no race with the grace).
    lab.sessions(false, json!([]));
    lab.until("sessions unknown", |s| !s.tools.sessions_known);
    open(&gate);
    assert_eq!(
        finish(child, Duration::from_secs(30)).status.code(),
        Some(0)
    );
    std::thread::sleep(Duration::from_millis(4000));
    assert_eq!(lab.kills(), 0, "shut down while a session may live on it");
    lab.until("still hostler's", |s| {
        lab.device(s, AVD_ID).is_some_and(|d| d.booted_by_hostler)
    });
    // A good read: shut down after the grace, as always.
    lab.sessions(true, json!([]));
    assert!(
        wait_for(Duration::from_secs(20), || lab.kills() == 1),
        "{:?}",
        lab.tools.calls()
    );
}

#[test]
fn run_end_whose_fresh_read_fails_says_sessions_unknown() {
    // No 30 s wait: the read at the end fails at once, and the cached lines alone would claim
    // a fresh answer (CRB-4).
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("app");
    let path = lab.workspace_path(&repo);
    lab.sessions(true, json!([session(PHONE_ID, "default", &path)]));
    lab.until("the session", |s| {
        lab.device(s, PHONE_ID)
            .is_some_and(|d| !d.sessions.is_empty())
    });
    let gate = lab.home.dir.path().join("gate");
    let child = lab.gated(&repo, &["--device", "phone"], &gate);
    started(&lab, PHONE_ID);
    lab.sessions(false, json!([]));
    std::thread::sleep(Duration::from_millis(600));
    open(&gate);
    let out = finish(child, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stderr(&out);
    let unknown = text
        .find("[hostler] agent-device sessions unknown: node not found")
        .unwrap_or_else(|| panic!("no sessions-unknown line: {text}"));
    // The last known session may follow, never stand alone.
    if let Some(cached) = text.find("agent-device session default is still open on phone") {
        assert!(unknown < cached, "{text}");
    }
}

#[test]
fn release_whose_fresh_read_fails_says_sessions_unknown() {
    let lab = Lab::new(phone_world(), &[]);
    lab.phone();
    let repo = lab.repo("e2e");
    let out = lab
        .hold(&repo, &["device", "phone", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let claim: Envelope<Claim> = serde_json::from_str(stdout(&out).trim()).unwrap();
    lab.sessions(false, json!([]));
    std::thread::sleep(Duration::from_millis(600));
    let out = lab
        .home
        .hostler()
        .args(["release", &claim.data.unwrap().id])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("[hostler] agent-device sessions unknown: node not found"),
        "{}",
        stderr(&out)
    );
}
