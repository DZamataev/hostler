//! Fake device tools for the integration tests (P3b Task 2). Include it with
//! `#[path = "common/fake_tools.rs"] mod fake_tools;`.
//!
//! `FakeTools::new()` copies the `fake_tool` example to every tool under `<dir>/bin`; `.env()`
//! points hostler at them (`HOSTLER_TEST_FAKE_TOOLS`). `.script(…)` sets the answers (see
//! `examples/fake_tool.rs`), `.calls()` reads what was called. Every logged call must pass the
//! exact allowlist (anti-goal 2), and a node call must run the fake agent-device script: checked by
//! `.calls()` and again on drop, the suite-wide check.
//!
//! Declare a `FakeTools` **before** the service or home handle it serves, so that handle drops
//! (and its service stops) first and every call it made is checked; never keep one in a static.
#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use hostler::devices::tools::{FAKE_TOOLS_ENV, Tool, allowed};
use serde_json::Value;
use tempfile::TempDir;

pub const ADB_SERVER_PORT_ENV: &str = "HOSTLER_TEST_ADB_SERVER_PORT";
pub const LOCKDOWN_DIR_ENV: &str = "HOSTLER_TEST_LOCKDOWN_DIR";

/// One logged call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeCall {
    pub tool: String,
    pub args: Vec<String>,
    /// Bytes read from stdin.
    pub stdin: usize,
    /// What the tool's stdout was: `null`, `pipe`, `file` or `other`.
    pub stdout: String,
    /// Which of the fake's watched environment names the tool got (names only).
    pub env: Vec<String>,
    /// When it started (Unix ms).
    pub at_ms: i64,
}

pub struct FakeTools {
    dir: TempDir,
    adb_server_port: Option<u16>,
    /// What the fake adb server answers (`set_adb_server_version` changes it).
    adb_server_version: Arc<AtomicU32>,
    lockdown: Option<TempDir>,
}

/// The built example, once per test binary.
pub fn fake_tool_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        escargot::CargoBuild::new()
            .example("fake_tool")
            .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .run()
            .expect("building the fake_tool example")
            .path()
            .to_path_buf()
    })
}

impl FakeTools {
    pub fn new() -> FakeTools {
        let exe = fake_tool_exe();
        // Copies, never hard links: on macOS, fresh processes of a hard-linked, ad-hoc-signed
        // binary were killed (SIGKILL) at random under load. Beside the build, so a copy is a
        // cheap clone where the file system has them.
        let dir = tempfile::Builder::new()
            .prefix("fake-tools-")
            .tempdir_in(exe.parent().unwrap())
            .unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for tool in Tool::ALL {
            let to = bin.join(tool.file_name());
            std::fs::copy(exe, &to).unwrap();
        }
        // Only its path matters: the fake `node` answers the agent-device calls itself.
        std::fs::write(bin.join("agent-device.mjs"), "// fake agent-device\n").unwrap();
        let tools = FakeTools {
            dir,
            adb_server_port: None,
            adb_server_version: Arc::new(AtomicU32::new(0)),
            lockdown: None,
        };
        tools.script(serde_json::json!([]));
        tools
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn bin(&self, tool: Tool) -> PathBuf {
        self.dir().join("bin").join(tool.file_name())
    }

    /// The fake agent-device script, run by the fake `node`.
    pub fn agent_device_script(&self) -> PathBuf {
        self.dir().join("bin").join("agent-device.mjs")
    }

    /// Replaces the script (a JSON array of entries).
    pub fn script(&self, entries: Value) -> &Self {
        assert!(entries.is_array(), "a script is an array of entries");
        std::fs::write(
            self.dir().join("script.json"),
            serde_json::to_vec_pretty(&entries).unwrap(),
        )
        .unwrap();
        self
    }

    /// Writes `<dir>/<name>` for a `stdout_file` entry; returns `name`.
    pub fn file(&self, name: &str, content: impl AsRef<[u8]>) -> String {
        std::fs::write(self.dir().join(name), content).unwrap();
        name.to_owned()
    }

    /// A checked-in fixture from `tests/fixtures/devices`, as a `stdout_file` entry value.
    pub fn fixture(&self, name: &str) -> String {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/devices")
            .join(name);
        std::fs::copy(from, self.dir().join(name)).unwrap();
        name.to_owned()
    }

    /// Every call so far; panics on one outside the allowlist.
    pub fn calls(&self) -> Vec<FakeCall> {
        let calls = self.logged();
        for call in &calls {
            self.check(call);
        }
        calls
    }

    fn logged(&self) -> Vec<FakeCall> {
        let log = std::fs::read_to_string(self.dir().join("calls.log")).unwrap_or_default();
        log.lines()
            .map(|line| {
                let v: Value = serde_json::from_str(line).expect("a calls.log line");
                FakeCall {
                    tool: v["tool"].as_str().unwrap().to_owned(),
                    args: v["args"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|a| a.as_str().unwrap().to_owned())
                        .collect(),
                    stdin: v["stdin"].as_u64().unwrap() as usize,
                    stdout: v["stdout"].as_str().unwrap().to_owned(),
                    env: v["env"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_str().unwrap().to_owned())
                        .collect(),
                    at_ms: v["at_ms"].as_i64().unwrap(),
                }
            })
            .collect()
    }

    /// The pids of the holding grandchildren started so far.
    pub fn held_pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.dir().join("held.pids"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect()
    }

    /// The variables that point hostler (service or CLI) at these tools.
    pub fn env(&self) -> Vec<(String, OsString)> {
        let mut env = vec![(FAKE_TOOLS_ENV.to_owned(), self.dir().as_os_str().to_owned())];
        if let Some(port) = self.adb_server_port {
            env.push((ADB_SERVER_PORT_ENV.to_owned(), port.to_string().into()));
        }
        if let Some(dir) = &self.lockdown {
            env.push((
                LOCKDOWN_DIR_ENV.to_owned(),
                dir.path().as_os_str().to_owned(),
            ));
        }
        env
    }

    /// A fake adb server on 127.0.0.1 that answers `000chost:version` with `version`
    /// (`OKAY` `0004` `<4 hex digits>`); other requests get `FAIL`. Returns its port.
    pub fn adb_server(&mut self, version: u32) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        self.adb_server_version.store(version, Ordering::SeqCst);
        let answer_version = self.adb_server_version.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut len = [0u8; 4];
                if stream.read_exact(&mut len).is_err() {
                    continue;
                }
                let n = std::str::from_utf8(&len)
                    .ok()
                    .and_then(|l| usize::from_str_radix(l, 16).ok())
                    .unwrap_or(0);
                let mut request = vec![0u8; n];
                if stream.read_exact(&mut request).is_err() {
                    continue;
                }
                let answer = if request == b"host:version" {
                    let version = answer_version.load(Ordering::SeqCst);
                    format!("OKAY0004{version:04x}")
                } else {
                    "FAIL0007unknown".to_owned()
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        self.adb_server_port = Some(port);
        port
    }

    /// The fake adb server answers `version` from now on (another adb's server replaced).
    pub fn set_adb_server_version(&self, version: u32) {
        self.adb_server_version.store(version, Ordering::SeqCst);
    }

    /// A lockdown directory holding an empty `<udid>.plist` per UDID (the pair-record stat).
    pub fn lockdown(&mut self, udids: &[&str]) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        for udid in udids {
            std::fs::write(dir.path().join(format!("{udid}.plist")), b"").unwrap();
        }
        let path = dir.path().to_path_buf();
        self.lockdown = Some(dir);
        path
    }
}

impl FakeTools {
    fn check(&self, call: &FakeCall) {
        let tool = Tool::from_name(&call.tool)
            .unwrap_or_else(|| panic!("a call of an unknown tool: {call:?}"));
        let args: Vec<&OsStr> = call.args.iter().map(OsStr::new).collect();
        assert!(
            allowed(tool, &args),
            "a device call outside the allowlist: {call:?}"
        );
        if tool == Tool::Node {
            assert_eq!(
                Path::new(&call.args[0]),
                self.agent_device_script(),
                "node ran another script: {call:?}"
            );
        }
    }
}

impl Drop for FakeTools {
    fn drop(&mut self) {
        // A test already failing keeps its own message.
        if std::thread::panicking() {
            return;
        }
        for call in self.logged() {
            self.check(&call);
        }
    }
}
