//! The e2e.com `DeviceProvider` repo file (`integrations/e2e/hostler-provider.mjs`, P3b Task 12)
//! against the real CLI and service on the fake device tools. A node harness
//! (`tests/fixtures/e2e/harness.mjs`) drives it as e2e's `LeasedDevices` does. Without `node` on
//! `PATH` every test here is skipped with a note. No real adb, xcrun, agent-device or e2e runs.
//!
//! The tests that look at what the provider spawned run `hostler` through a small `sh` wrapper,
//! so they and the signal-based abort are Unix only.
#![cfg_attr(not(unix), allow(dead_code))]

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use common::{BIN, Home, stderr};
use fake_tools::FakeTools;
use hostler::contract::claims::{Claim, ClaimKind, HoldBinding};
use hostler::contract::devices::DevicesSnapshot;
use hostler::contract::wire::Request;
use serde_json::{Value, json};

const SIM_A: &str = "00000000-0000-4000-8000-0000000000D0";
const SIM_B: &str = "00000000-0000-4000-8000-0000000000D1";
const SIM_C: &str = "00000000-0000-4000-8000-0000000000D3";
/// Shut down until a `--boot` hold boots it.
const SIM_AIR: &str = "00000000-0000-4000-8000-0000000000D2";
const EMULATOR: &str = "emulator-5554";
const AVD_ID: &str = "device:android:avd:Pixel_9";
/// Generous: the Mac is shared with other builds.
const LIMIT: Duration = Duration::from_secs(60);

fn sim_id(udid: &str) -> String {
    format!("device:ios:{udid}")
}

fn provider_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/e2e/hostler-provider.mjs")
}

fn harness_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e/harness.mjs")
}

/// `node` on `PATH`, else `None` and a note: the test is skipped.
fn node() -> Option<PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let found = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        })
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    if found.is_none() {
        eprintln!("note: node is not on PATH; skipping the e2e provider test");
    }
    found
}

/// `simctl list devices --json` with these simulators, `(udid, name, booted)`.
fn simctl(sims: &[(&str, &str, bool)]) -> String {
    let devices: Vec<Value> = sims
        .iter()
        .map(|(udid, name, booted)| {
            json!({"udid": udid, "name": name, "isAvailable": true,
                   "state": if *booted { "Booted" } else { "Shutdown" },
                   "lastBootedAt": if *booted { "2026-11-05T13:18:37Z" } else { "2026-10-28T12:08:12Z" }})
        })
        .collect();
    json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-26-5": devices}}).to_string()
}

fn sims_script(tools: &FakeTools, sims: &[(&str, &str, bool)]) -> Value {
    let list = tools.file("simctl.json", simctl(sims));
    let devicectl = tools.fixture("devicectl-list.json");
    json!([
        {"tool": "xcrun", "args_prefix": ["devicectl", "list", "devices"],
         "json_output_file": devicectl},
        {"tool": "xcrun", "args_prefix": ["simctl", "list", "devices"], "stdout_file": list},
        {"tool": "xcrun", "args_prefix": ["simctl", "bootstatus"]},
        {"tool": "xcrun", "args_prefix": ["simctl", "shutdown"]},
    ])
}

/// The AVD `Pixel_9`, shut down; `emulator`: it runs as emulator-5554.
fn avd_script(emulator: bool) -> Value {
    let mut devices = "List of devices attached\n".to_owned();
    if emulator {
        devices.push_str(&format!(
            "{EMULATOR}          device product:p model:m device:d transport_id:2\n"
        ));
    }
    devices.push('\n');
    json!([
        {"tool": "adb", "args_prefix": ["version"],
         "stdout": "Android Debug Bridge version 1.0.41\nVersion 35.0.2-12147458\n"},
        {"tool": "adb", "args_prefix": ["start-server"]},
        {"tool": "adb", "args_prefix": ["devices", "-l"], "stdout": devices},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell", "getprop sys.boot_completed"],
         "stdout": "1\n"},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "shell"],
         "stdout": "EMULATOR35X1X1X0\nsdk_gphone64_arm64\n15\n35\nPixel_9\n1\n\n"},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "emu", "avd", "name"],
         "stdout": "Pixel_9\r\nOK\r\n"},
        {"tool": "adb", "args_prefix": ["-s", EMULATOR, "emu", "kill"]},
        {"tool": "emulator", "args_prefix": ["-list-avds"], "stdout": "Pixel_9\n"},
        // A booted emulator keeps running (its pid is the recorded identity).
        {"tool": "emulator", "args_prefix": ["-avd", "Pixel_9"], "delay_ms": 20000},
    ])
}

struct Lab {
    home: Home,
    tools: FakeTools,
    env: Vec<(String, std::ffi::OsString)>,
    node: PathBuf,
}

impl Lab {
    /// `None` without node. `script` builds the fake tools' answers.
    fn new(script: impl FnOnce(&FakeTools) -> Value) -> Option<Lab> {
        let node = node()?;
        let home = Home::new();
        let mut tools = FakeTools::new();
        tools.adb_server(41);
        tools.script(script(&tools));
        let mut env = tools.env();
        for (k, v) in [
            ("HOSTLER_TEST_DEVICES_POLL_MS", "200"),
            ("HOSTLER_TEST_CLAIMS_TICK_MS", "100"),
            ("HOSTLER_TEST_BOOT_TIMEOUT_MS", "30000"),
            ("HOSTLER_TEST_SHUTDOWN_GRACE_MS", "600000"),
            ("HOSTLER_TEST_PLATFORM_IOS", "1"),
        ] {
            env.push((k.to_owned(), v.into()));
        }
        let lab = Lab {
            home,
            tools,
            env,
            node,
        };
        let out = lab
            .home
            .hostler()
            .envs(lab.env.iter().map(|(k, v)| (k, v)))
            .args(["service", "start"])
            .output()
            .unwrap();
        assert!(out.status.success(), "service start: {}", stderr(&out));
        Some(lab)
    }

    fn sims(sims: &[(&str, &str, bool)]) -> Option<Lab> {
        let lab = Lab::new(|tools| sims_script(tools, sims))?;
        for (udid, _, _) in sims {
            lab.register(udid, &sim_id(udid));
        }
        Some(lab)
    }

    fn list(&self) -> DevicesSnapshot {
        serde_json::from_value(self.home.call(Request::DevicesList { all: true }).unwrap()).unwrap()
    }

    fn until(&self, what: &str, done: impl Fn(&DevicesSnapshot) -> bool) {
        let deadline = Instant::now() + LIMIT;
        loop {
            let s = self.list();
            if done(&s) {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: {s:#?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Seen, then added (simulators and AVDs need no person), then checked for sessions (L52).
    fn register(&self, device: &str, id: &str) {
        self.until(device, |s| s.devices.iter().any(|d| d.resource.0 == id));
        self.home
            .call(Request::DevicesAdd {
                device: device.into(),
                alias: None,
                person: false,
            })
            .unwrap();
        self.until("registered and checked", |s| {
            s.devices
                .iter()
                .any(|d| d.resource.0 == id && d.registered && !d.checking_sessions)
        });
    }

    fn repo(&self, name: &str) -> PathBuf {
        std::fs::canonicalize(self.home.make_repo(name)).unwrap()
    }

    fn claims_on(&self, id: &str) -> Vec<Claim> {
        self.home
            .snapshot()
            .resources
            .into_iter()
            .filter(|s| s.resource.id.0 == id)
            .flat_map(|s| s.claims)
            .collect()
    }

    fn all_claims(&self) -> Vec<Claim> {
        self.home
            .snapshot()
            .resources
            .into_iter()
            .flat_map(|s| s.claims)
            .collect()
    }

    fn waiting(&self) -> usize {
        self.home
            .snapshot()
            .resources
            .iter()
            .map(|s| s.waiting.len())
            .sum()
    }

    fn calls_of(&self, tool: &str, prefix: &[&str]) -> usize {
        self.tools
            .calls()
            .iter()
            .filter(|c| c.tool == tool && c.args.iter().take(prefix.len()).eq(prefix.iter()))
            .count()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.dir.path().join(name)
    }

    /// The `sh` wrapper the provider may run as `hostler`: it logs `<cwd>\t<tokens set>\t<args>`
    /// to `wrap.log` and runs the real binary in its place (`exec`, so `--while-parent` still
    /// binds to node), except as `wrap` says.
    #[cfg(unix)]
    fn wrapper(&self, wrap: Wrap) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = self.path("wrap.log");
        let (name, tail) = match wrap {
            Wrap::Plain => ("hostler-wrap", r#"exec "$HB" "$@""#.to_owned()),
            // A hold binds to the wrapper's parent (node) and its answer goes through `filter`.
            Wrap::Strip | Wrap::Pretty => {
                let filter = if matches!(wrap, Wrap::Strip) {
                    r#"sed 's/"agent_device_id":"[^"]*",//'"#.to_owned()
                } else {
                    format!(
                        "'{}' -e 'let s=\"\";process.stdin.on(\"data\",d=>s+=d).on(\"end\",()=>console.log(JSON.stringify(JSON.parse(s),null,2)))'",
                        self.node.display()
                    )
                };
                let tail = format!(
                    r#"if [ "$1" != hold ]; then exec "$HB" "$@"; fi
parent=$PPID
for a do
  shift
  if [ "$a" = --while-parent ]; then set -- "$@" --while-pid "$parent"; else set -- "$@" "$a"; fi
done
out=$("$HB" "$@")
rc=$?
printf '%s\n' "$out" | {filter}
exit $rc"#
                );
                (
                    if matches!(wrap, Wrap::Strip) {
                        "hostler-strip"
                    } else {
                        "hostler-pretty"
                    },
                    tail,
                )
            }
            // The real hold runs first (bound to node) and its answer is printed at once; then
            // `printed` exists, and a grandchild (pid in `holder.pid`) keeps the pipe open.
            Wrap::LateClaim => (
                "hostler-late",
                format!(
                    r#"if [ "$1" != hold ]; then exec "$HB" "$@"; fi
trap '' TERM
parent=$PPID
for a do
  shift
  if [ "$a" = --while-parent ]; then set -- "$@" --while-pid "$parent"; else set -- "$@" "$a"; fi
done
out=$("$HB" "$@")
printf '%s\n' "$out"
sleep 120 &
echo $! > '{}'
touch '{}'
exec sleep 120"#,
                    self.path("holder.pid").display(),
                    self.path("printed").display()
                ),
            ),
            // `release` ignores SIGTERM and never ends; its pid goes to `release.pid`.
            Wrap::HangRelease => (
                "hostler-hang",
                format!(
                    r#"if [ "$1" = release ]; then echo $$ > '{}'; trap '' TERM; exec sleep 120; fi
exec "$HB" "$@""#,
                    self.path("release.pid").display()
                ),
            ),
        };
        let script = format!(
            "#!/bin/sh\nprintf '%s\\t%s\\t%s\\n' \"$(pwd -P)\" \"${{HOSTLER_CLAIM_TOKENS+set}}\" \"$*\" >> '{}'\n{tail}\n",
            log.display()
        );
        let path = self.path(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// `wrap.log` lines: `(cwd, tokens set, args)`.
    fn wrapped(&self) -> Vec<(String, bool, String)> {
        std::fs::read_to_string(self.path("wrap.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let mut parts = l.splitn(3, '\t');
                let cwd = parts.next().unwrap().to_owned();
                let tokens = parts.next().unwrap() == "set";
                (cwd, tokens, parts.next().unwrap_or("").to_owned())
            })
            .collect()
    }

    /// `node harness.mjs <provider> <config>` in `cwd`, with the home's and the fakes' env.
    fn node_command(&self, config: &Value, cwd: &Path) -> Command {
        let template = self.home.hostler();
        let mut c = Command::new(&self.node);
        for (k, v) in template.get_envs() {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        c.envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("HB", BIN)
            .env_remove("HOSTLER_CLAIM_TOKENS")
            .arg(harness_file())
            .arg(provider_file())
            .arg(config.to_string())
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn harness(&self, config: Value, cwd: &Path) -> Harness {
        Harness::spawn(self.node_command(&config, cwd))
    }
}

/// What the `hostler` wrapper changes (`Lab::wrapper`).
#[derive(Clone, Copy)]
enum Wrap {
    /// Nothing: it only logs.
    Plain,
    /// A hold's answer loses `agent_device_id`.
    Strip,
    /// A hold's answer is pretty-printed over several lines.
    Pretty,
    /// `release` ignores SIGTERM and hangs.
    HangRelease,
    /// A hold prints its claim and then never exits: it ignores SIGTERM, and a grandchild keeps
    /// its stdout open after it is killed.
    LateClaim,
}

/// A running harness and its events.
struct Harness {
    child: Child,
    events: Receiver<Value>,
}

impl Harness {
    fn spawn(mut c: Command) -> Harness {
        let mut child = c.spawn().unwrap();
        let events = read_events(child.stdout.take().unwrap());
        Harness { child, events }
    }

    /// The next event named `name`, skipping log lines; panics with stderr after `LIMIT`.
    fn event(&mut self, name: &str) -> Value {
        self.next(name, |e| e["event"] == name)
    }

    /// The next event `matches` takes; panics with stderr after `LIMIT`.
    fn next(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + LIMIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(e) if matches(&e) => return e,
                Ok(_) => {}
                Err(_) => {
                    let _ = self.child.kill();
                    let mut err = String::new();
                    if let Some(mut s) = self.child.stderr.take() {
                        let _ = std::io::Read::read_to_string(&mut s, &mut err);
                    }
                    panic!("no {what} event from the harness; stderr: {err}");
                }
            }
        }
    }

    fn exits_ok(mut self) {
        let deadline = Instant::now() + LIMIT;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the harness did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = self.child.wait_with_output().unwrap();
        assert!(out.status.success(), "harness: {}", stderr(&out));
    }
}

fn read_events(stdout: ChildStdout) -> Receiver<Value> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let event: Value = serde_json::from_str(&line)
                .unwrap_or_else(|_| json!({"event": "text", "line": line}));
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    rx
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

fn open(gate: &Path) {
    std::fs::write(gate, "").unwrap();
}

fn leases(acquired: &Value) -> Vec<Value> {
    acquired["leases"].as_array().unwrap().clone()
}

fn errors(event: &Value) -> Vec<Value> {
    event["errors"].as_array().unwrap().clone()
}

/// The one failed acquire's error.
fn only_error(acquired: &Value) -> Value {
    assert!(leases(acquired).is_empty(), "{acquired}");
    let errors = errors(acquired);
    assert_eq!(errors.len(), 1, "{acquired}");
    errors[0].clone()
}

fn device_id(lease: &Value) -> String {
    lease["deviceId"].as_str().unwrap().to_owned()
}

fn simulators(sims: &[&str]) -> Vec<(&'static str, &'static str, bool)> {
    let all = [
        (SIM_A, "iPhone 17 Pro", true),
        (SIM_B, "iPhone 17 Pro Max", true),
        (SIM_C, "iPhone 17", true),
        (SIM_AIR, "iPhone Air", false),
    ];
    all.into_iter().filter(|s| sims.contains(&s.0)).collect()
}

// --- The contract ---------------------------------------------------------------------------

#[test]
fn e2e_provider_file_parses() {
    let Some(node) = node() else { return };
    let out = Command::new(node)
        .arg("--check")
        .arg(provider_file())
        .output()
        .unwrap();
    assert!(out.status.success(), "node --check: {}", stderr(&out));
}

/// Two slots at once get two simulators, each lease is `{id, deviceId: <UDID>, client: {}}`
/// with no `device`, each hold is bound to the runner, and `release` frees them (twice is fine).
#[test]
fn e2e_provider_contract() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A, SIM_B])) else {
        return;
    };
    let repo = lab.repo("app");
    let gate = lab.path("release");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": BIN}, "platform": "ios", "slots": 2,
               "projectRoot": repo, "releaseGate": gate, "releaseTimes": 2}),
        &repo,
    );
    let start = h.event("start");
    assert_eq!(start["name"], "hostler");
    let node_pid = start["pid"].as_u64().unwrap() as u32;
    let acquired = h.event("acquired");
    assert!(errors(&acquired).is_empty(), "{acquired}");
    let leases = leases(&acquired);
    assert_eq!(leases.len(), 2, "{acquired}");
    let mut ids: Vec<String> = leases.iter().map(device_id).collect();
    ids.sort();
    assert_eq!(ids, [SIM_A, SIM_B], "two simulators, by UDID");
    for lease in &leases {
        let mut keys: Vec<&str> = lease.as_object().unwrap().keys().map(|k| &**k).collect();
        keys.sort();
        assert_eq!(
            keys,
            ["client", "deviceId", "id"],
            "no device field: {lease}"
        );
        assert_eq!(lease["client"], json!({}));
        let udid = device_id(lease);
        let claims = lab.claims_on(&sim_id(&udid));
        assert_eq!(claims.len(), 1, "{claims:?}");
        let claim = &claims[0];
        assert_eq!(claim.id, lease["id"].as_str().unwrap());
        assert_eq!(claim.kind, ClaimKind::Hold);
        match &claim.binding {
            Some(HoldBinding::Process { holder }) => {
                assert_eq!(holder.pid, node_pid, "bound to the runner (--while-parent)");
            }
            other => panic!("not process-bound: {other:?}"),
        }
    }
    open(&gate);
    let released = h.event("released");
    assert!(errors(&released).is_empty(), "{released}");
    h.exits_ok();
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
}

/// An acquire that waits behind another workspace's hold is aborted: the hold command is
/// stopped, its request withdrawn, and `acquire` throws an `AbortError`.
#[cfg(unix)]
#[test]
fn abort_before_grant_withdraws() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let other = lab.repo("other");
    let out = lab
        .home
        .hostler()
        .args([
            "hold",
            "device",
            "auto:ios:simulator",
            "--ttl",
            "30",
            "--json",
        ])
        .current_dir(&other)
        .env_remove("HOSTLER_CLAIM_TOKENS")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let repo = lab.repo("app");
    let gate = lab.path("abort");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": BIN}, "platform": "ios",
               "projectRoot": repo, "abortGate": gate}),
        &repo,
    );
    assert!(
        wait_for(LIMIT, || lab.waiting() == 1),
        "the provider's hold never waited"
    );
    open(&gate);
    let acquired = h.event("acquired");
    let error = only_error(&acquired);
    assert_eq!(error["name"], "AbortError", "{acquired}");
    h.exits_ok();
    assert_eq!(lab.waiting(), 0, "the waiting request was withdrawn");
    assert_eq!(
        lab.claims_on(&sim_id(SIM_A)).len(),
        1,
        "only the other hold"
    );
}

/// Aborted once the claim has arrived: the provider releases it itself, then throws.
#[test]
fn abort_after_grant_self_releases() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": BIN}, "platform": "ios",
               "projectRoot": repo, "onGrant": "abort"}),
        &repo,
    );
    let acquired = h.event("acquired");
    let error = only_error(&acquired);
    assert_eq!(error["name"], "AbortError", "{acquired}");
    // Released before `acquire` threw: the hold is bound to node, which still runs.
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
    h.exits_ok();
}

/// Anything that throws after the claim arrived (here the run's `log`) releases it first.
#[test]
fn error_after_grant_self_releases() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": BIN}, "platform": "ios",
               "projectRoot": repo, "onGrant": "throw"}),
        &repo,
    );
    let acquired = h.event("acquired");
    let error = only_error(&acquired);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("the log failed"),
        "{acquired}"
    );
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
    h.exits_ok();
}

/// `boot: true` passes `--boot`: a shut-down simulator is booted and leased.
#[cfg(unix)]
#[test]
fn boot_option_passes_boot() {
    let Some(lab) = Lab::new(|tools| {
        let mut script = sims_script(tools, &simulators(&[SIM_AIR]));
        // `simctl boot` boots it: discovery lists it booted from then on.
        let booted = tools.file(
            "simctl-booted.json",
            simctl(&[(SIM_AIR, "iPhone Air", true)]),
        );
        let mut after = script.clone();
        after[1]["stdout_file"] = json!(booted);
        script.as_array_mut().unwrap().insert(
            0,
            json!({"tool": "xcrun", "args_prefix": ["simctl", "boot", SIM_AIR],
                              "then_write": after}),
        );
        script
    }) else {
        return;
    };
    lab.register(SIM_AIR, &sim_id(SIM_AIR));
    let repo = lab.repo("app");
    let gate = lab.path("release");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "boot": true, "hostler": lab.wrapper(Wrap::Plain)},
               "platform": "ios", "projectRoot": repo, "releaseGate": gate}),
        &repo,
    );
    let acquired = h.event("acquired");
    let leases = leases(&acquired);
    assert_eq!(leases.len(), 1, "{acquired}");
    assert_eq!(device_id(&leases[0]), SIM_AIR);
    assert_eq!(lab.calls_of("xcrun", &["simctl", "boot", SIM_AIR]), 1);
    let hold = &lab.wrapped()[0].2;
    assert!(
        hold.starts_with("hold device auto:ios:simulator ") && hold.contains(" --boot"),
        "{hold}"
    );
    open(&gate);
    assert!(errors(&h.event("released")).is_empty());
    h.exits_ok();
}

/// Hold and every release run in the request's `projectRoot`, whatever the runner's cwd (a
/// repeated release too); a `projectRoot` outside any checkout is the CLI's refusal, thrown with
/// its message.
#[cfg(unix)]
#[test]
fn cwd_is_project_root() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let elsewhere = lab.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = std::fs::canonicalize(elsewhere).unwrap();
    let gate = lab.path("release");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": lab.wrapper(Wrap::Plain)},
               "platform": "ios", "projectRoot": repo, "releaseGate": gate, "releaseTimes": 2}),
        &elsewhere,
    );
    let acquired = h.event("acquired");
    assert_eq!(leases(&acquired).len(), 1, "{acquired}");
    open(&gate);
    assert!(errors(&h.event("released")).is_empty());
    h.exits_ok();
    // Released twice (R1-2): the second one too runs in the project root, and succeeds.
    let wrapped = lab.wrapped();
    assert_eq!(wrapped.len(), 3, "{wrapped:?}");
    assert!(wrapped[0].2.starts_with("hold "), "{wrapped:?}");
    assert!(wrapped[1].2.starts_with("release "), "{wrapped:?}");
    assert_eq!(wrapped[1].2, wrapped[2].2, "{wrapped:?}");
    for (cwd, _, _) in &wrapped {
        assert_eq!(Path::new(cwd), repo, "{wrapped:?}");
    }

    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": BIN}, "platform": "ios",
               "projectRoot": elsewhere}),
        &repo,
    );
    let error = only_error(&h.event("acquired"));
    assert_eq!(error["code"], "not_in_project", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("is not inside a git checkout"),
        "{error}"
    );
    h.exits_ok();
}

/// P2-8: `--boot` of an AVD answers with the emulator's `emulator-N`, agent-device's id.
#[test]
fn android_emulator_with_boot_gets_the_emulator_serial() {
    let Some(lab) = Lab::new(|_| avd_script(false)) else {
        return;
    };
    lab.register("Pixel_9", AVD_ID);
    let repo = lab.repo("app");
    let gate = lab.path("release");
    let mut h = lab.harness(
        json!({"options": {"kind": "emulator", "boot": true, "hostler": BIN},
               "platform": "android", "projectRoot": repo, "releaseGate": gate}),
        &repo,
    );
    assert!(
        wait_for(LIMIT, || lab.calls_of("emulator", &["-avd"]) > 0),
        "no emulator -avd call: {:?}",
        lab.tools.calls()
    );
    lab.tools.script(avd_script(true));
    let acquired = h.event("acquired");
    let leases = leases(&acquired);
    assert_eq!(leases.len(), 1, "{acquired}");
    assert_eq!(device_id(&leases[0]), EMULATOR);
    assert_eq!(lab.claims_on(AVD_ID).len(), 1);
    open(&gate);
    assert!(errors(&h.event("released")).is_empty());
    h.exits_ok();
    assert!(lab.claims_on(AVD_ID).is_empty());
}

/// P2-6: inside `hostler run --device`, the provider drops `HOSTLER_CLAIM_TOKENS` and its
/// distinct holds take the other simulators, never the run's.
#[cfg(unix)]
#[test]
fn inside_an_enclosing_device_run_slots_get_distinct_devices() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A, SIM_B, SIM_C])) else {
        return;
    };
    let repo = lab.repo("app");
    let gate = lab.path("release");
    let config = json!({"options": {"kind": "simulator", "hostler": lab.wrapper(Wrap::Plain)},
                        "platform": "ios", "slots": 2, "projectRoot": repo,
                        "releaseGate": gate});
    let inner = lab.node_command(&config, &repo);
    let mut c = lab.home.hostler();
    c.args(["run", "--device", "auto:ios:simulator", "--"])
        .arg(inner.get_program())
        .args(inner.get_args())
        .envs(lab.env.iter().map(|(k, v)| (k, v)))
        .env("HB", BIN)
        .env_remove("HOSTLER_CLAIM_TOKENS")
        .current_dir(&repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut h = Harness::spawn(c);
    assert_eq!(
        h.event("start")["tokens"],
        true,
        "the run exported its token"
    );
    let acquired = h.event("acquired");
    let leases = leases(&acquired);
    assert_eq!(leases.len(), 2, "{acquired}");
    let run_lease: Vec<Claim> = lab
        .all_claims()
        .into_iter()
        .filter(|c| c.kind == ClaimKind::Lease)
        .collect();
    assert_eq!(run_lease.len(), 1, "{run_lease:?}");
    let run_device = run_lease[0]
        .resource
        .id
        .0
        .trim_start_matches("device:ios:")
        .to_owned();
    let mut ids: Vec<String> = leases.iter().map(device_id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2, "two distinct devices: {ids:?}");
    assert!(!ids.contains(&run_device), "never the run's {run_device}");
    for (_, tokens, args) in lab.wrapped() {
        assert!(!tokens, "HOSTLER_CLAIM_TOKENS reached `hostler {args}`");
    }
    open(&gate);
    assert!(errors(&h.event("released")).is_empty());
    h.exits_ok();
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
}

/// P2-8: a claim whose device has no `agent_device_id` is released at once and refused.
#[cfg(unix)]
#[test]
fn lease_without_agent_device_id_is_released_and_refused() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": lab.wrapper(Wrap::Strip)},
               "platform": "ios", "projectRoot": repo}),
        &repo,
    );
    let error = only_error(&h.event("acquired"));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("agent_device_id"),
        "{error}"
    );
    let wrapped = lab.wrapped();
    assert_eq!(wrapped.len(), 2, "hold, then its release: {wrapped:?}");
    assert!(wrapped[1].2.starts_with("release "), "{wrapped:?}");
    // The hold was bound to node (still running), so only the release ended it.
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
    h.exits_ok();
}

/// R1-1: a self-release whose `hostler release` ignores SIGTERM and hangs is killed, and the
/// acquire rejects within 10 s of the grant (bound 15 s under load), naming the manual release.
#[cfg(unix)]
#[test]
fn self_release_is_bounded_when_release_hangs() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": lab.wrapper(Wrap::HangRelease)},
               "platform": "ios", "projectRoot": repo, "onGrant": "throw"}),
        &repo,
    );
    let grant = h.next("the grant line", |e| {
        e["event"] == "log"
            && e["line"]
                .as_str()
                .is_some_and(|l| l.starts_with("holding "))
    });
    let granted = Instant::now();
    // The claim id from the grant line: once the acquire rejects, the harness exits with no
    // lease, and the service may end the claim of its dead owner before it is looked up.
    let line = grant["line"].as_str().unwrap();
    let id = line.rsplit(" as ").next().unwrap().trim().to_string();
    let acquired = h.event("acquired");
    let took = granted.elapsed();
    assert!(took < Duration::from_secs(15), "took {took:?}");
    assert!(
        took >= Duration::from_secs(7),
        "the release never hung: {took:?}"
    );
    let error = only_error(&acquired);
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("the log failed"), "{message}");
    assert!(
        message.contains(&format!("hostler release {id}")),
        "{message}"
    );
    // The hung release was killed, not left running.
    let pid = std::fs::read_to_string(lab.path("release.pid")).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the release {pid} still runs");
    h.exits_ok();
}

/// R1-3: an envelope printed over several lines is still read.
#[cfg(unix)]
#[test]
fn multiline_envelope_is_parsed() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": lab.wrapper(Wrap::Pretty)},
               "platform": "ios", "projectRoot": repo}),
        &repo,
    );
    let acquired = h.event("acquired");
    let leases = leases(&acquired);
    assert_eq!(leases.len(), 1, "{acquired}");
    assert_eq!(device_id(&leases[0]), SIM_A);
    assert!(errors(&h.event("released")).is_empty());
    h.exits_ok();
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
}

/// R2-1, R3-1: an aborted hold that printed its claim but ignores SIGTERM is killed after its
/// grace while a grandchild still holds its stdout; the claim already in the pipe is read and
/// released, the acquire rejects, and the grandchild never stalls it past the bound.
#[cfg(unix)]
#[test]
fn claim_printed_at_the_kill_is_drained_and_released() {
    let Some(lab) = Lab::sims(&simulators(&[SIM_A])) else {
        return;
    };
    let repo = lab.repo("app");
    let gate = lab.path("abort");
    let mut h = lab.harness(
        json!({"options": {"kind": "simulator", "hostler": lab.wrapper(Wrap::LateClaim),
                           "_holdKillGraceMs": 1000},
               "platform": "ios", "projectRoot": repo, "abortGate": gate}),
        &repo,
    );
    // The claim is in the pipe before the abort: nothing below depends on scheduling.
    assert!(
        wait_for(LIMIT, || lab.path("printed").exists()),
        "the wrapped hold never printed its claim"
    );
    assert_eq!(lab.claims_on(&sim_id(SIM_A)).len(), 1);
    let opened = Instant::now();
    open(&gate);
    let acquired = h.event("acquired");
    let took = opened.elapsed();
    let error = only_error(&acquired);
    assert_eq!(error["name"], "AbortError", "{acquired}");
    // The 1 s grace, then at most 1.3 s to settle with the pipe held, then the self-release.
    assert!(
        took >= Duration::from_secs(1),
        "the grace was skipped: {took:?}"
    );
    assert!(took < Duration::from_secs(15), "took {took:?}");
    assert!(lab.all_claims().is_empty(), "{:?}", lab.all_claims());
    let wrapped = lab.wrapped();
    assert!(
        wrapped
            .iter()
            .any(|(_, _, args)| args.starts_with("release ")),
        "{wrapped:?}"
    );
    h.exits_ok();
    let holder = std::fs::read_to_string(lab.path("holder.pid")).unwrap();
    // It ignores SIGTERM, as the wrapper does.
    let _ = Command::new("kill").args(["-9", holder.trim()]).status();
}
