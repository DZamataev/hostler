//! Tool paths, the bounded tool runner and the exact argument allowlist (P3b Task 2).
//!
//! Anti-goal 2: hostler only reads from devices. Every call is matched against `ALLOWED`, exact
//! argument vectors, before anything is spawned; `adb shell` runs only the script constants below,
//! each checked by a test to hold read-only commands. Every call is bounded and killed at its bound
//! with its whole tree: its process group (Unix) or a kill-on-close job (Windows). The adb server
//! and hostler-booted emulators are never inside that tree (P-7): `ensure_adb_server` and
//! `spawn_detached` start them detached, and an adb call's job lets a server it starts break away.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::contract::devices::ToolPathsSet;
use crate::procs::{ProcessTable, SysProcessTable};

/// Debug builds: every tool resolves to the fake tool in `<dir>/bin` (tests).
pub const FAKE_TOOLS_ENV: &str = "HOSTLER_TEST_FAKE_TOOLS";
/// Each output stream is cut here; a cut output is `truncated` and parsers treat it as unreadable.
pub const OUTPUT_CAP: usize = 16 << 20;
/// How long a finished call waits for its pipes to close: a process that left the tree (a
/// `setsid` grandchild, an adb server that broke away) may hold them open for good.
const DRAIN_GRACE: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(10);
const START_SERVER_BOUND: Duration = Duration::from_secs(10);
const VERSION_BOUND: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Adb,
    Emulator,
    Xcrun,
    Plutil,
    Ideviceinfo,
    Node,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Adb,
        Tool::Emulator,
        Tool::Xcrun,
        Tool::Plutil,
        Tool::Ideviceinfo,
        Tool::Node,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Adb => "adb",
            Tool::Emulator => "emulator",
            Tool::Xcrun => "xcrun",
            Tool::Plutil => "plutil",
            Tool::Ideviceinfo => "ideviceinfo",
            Tool::Node => "node",
        }
    }

    pub fn from_name(name: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The executable's file name on this OS: `adb`, or `adb.exe` on Windows.
    pub fn file_name(self) -> String {
        exe(self.name())
    }
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

// The `adb shell` scripts: the only shell input hostler ever sends (P-12). Task 3 parses their
// output; `scripts_are_read_only` checks every command in them.

/// Seven `getprop` lines, in this order: serial, model, release, SDK, AVD name, two qemu flags.
pub const IDENTITY: &str = "getprop ro.serialno; getprop ro.product.model; \
getprop ro.build.version.release; getprop ro.build.version.sdk; getprop ro.boot.qemu.avd_name; \
getprop ro.kernel.qemu; getprop ro.boot.qemu";
/// Battery, the connected-network agents, and the first `mWifiInfo SSID:` line (`.` stands for
/// the space, so the pattern needs no quoting).
pub const TELEMETRY: &str = "dumpsys battery; dumpsys connectivity | grep NetworkAgentInfo{; \
dumpsys wifi | grep -m1 mWifiInfo.SSID:";
/// The device clock (`date +%s`, `date +%z`), its time zone id (L69: a change re-seeds), the
/// user packages, `===`, the package dump. Chained with `&&` (P3bT3-1): a failed `pm list` never
/// prints `===`, so the parser reads the snapshot as unreadable, never as "no apps".
pub const SNAPSHOT: &str = "date +%s; date +%z; getprop persist.sys.timezone; \
pm list packages -3 --show-versioncode && echo === && dumpsys package packages";
pub const BOOT_COMPLETED: &str = "getprop sys.boot_completed";
const SCRIPTS: &[&str] = &[IDENTITY, TELEMETRY, SNAPSHOT, BOOT_COMPLETED];

/// One argument of an allowed vector. Every value kind has a fixed shape (P3bT2-1): never a flag,
/// never empty, never a word a tool reads as "every device" (`simctl shutdown all|booted`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    Lit(&'static str),
    /// An adb transport serial (`FAKE0SAMS01`, `10.0.0.7:5555`, `adb-<id>._adb-tls-connect._tcp`).
    Serial,
    /// `emulator-<n>`: the only target of `emu` commands.
    EmulatorSerial,
    /// A UDID: 20–64 hex digits and dashes (simulator UUIDs, `00008101-…`, 40-hex phones).
    Udid,
    /// An AVD name.
    Avd,
    /// An absolute path.
    Path,
    /// An agent-device session name, as agent-device printed it (P2-19).
    Session,
    /// `android` or `ios`.
    Platform,
    /// One of the listed script constants, byte for byte.
    Script(&'static [&'static str]),
}

impl Arg {
    fn matches(&self, arg: &OsStr) -> bool {
        // A value that is not UTF-8 is never one hostler made.
        let Some(s) = arg.to_str() else { return false };
        match self {
            Arg::Lit(lit) => s == *lit,
            Arg::Serial => word(s, "._:-"),
            Arg::EmulatorSerial => s
                .strip_prefix("emulator-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
            Arg::Udid => {
                (20..=64).contains(&s.len())
                    && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
            }
            Arg::Avd => word(s, "._-"),
            Arg::Session => word(s, "._:@/+=-"),
            Arg::Path => {
                !s.starts_with('-')
                    && !s.chars().any(char::is_control)
                    && Path::new(s).is_absolute()
            }
            Arg::Platform => s == "android" || s == "ios",
            Arg::Script(scripts) => scripts.contains(&s),
        }
    }
}

/// ASCII letters and digits plus `extra`; never empty, never a flag (a leading `-`).
fn word(s: &str, extra: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

use Arg::{Avd, EmulatorSerial, Lit, Path as P, Platform as Plat, Script, Serial, Session, Udid};

/// Every call hostler may make (P-12), as exact argument vectors. Nothing else is spawned.
pub const ALLOWED: &[(Tool, &[Arg])] = &[
    (Tool::Adb, &[Lit("start-server")]),
    (Tool::Adb, &[Lit("devices"), Lit("-l")]),
    (Tool::Adb, &[Lit("version")]),
    (
        Tool::Adb,
        &[Lit("-s"), Serial, Lit("shell"), Script(SCRIPTS)],
    ),
    (
        Tool::Adb,
        &[
            Lit("-s"),
            EmulatorSerial,
            Lit("emu"),
            Lit("avd"),
            Lit("name"),
        ],
    ),
    // The shutdown of an emulator hostler booted, after its identity check (P-6).
    (
        Tool::Adb,
        &[Lit("-s"), EmulatorSerial, Lit("emu"), Lit("kill")],
    ),
    (Tool::Emulator, &[Lit("-list-avds")]),
    (
        Tool::Emulator,
        &[
            Lit("-avd"),
            Avd,
            Lit("-no-window"),
            Lit("-no-audio"),
            Lit("-no-boot-anim"),
            Lit("-no-snapshot-save"),
        ],
    ),
    (
        Tool::Xcrun,
        &[
            Lit("devicectl"),
            Lit("list"),
            Lit("devices"),
            Lit("--json-output"),
            P,
        ],
    ),
    (
        Tool::Xcrun,
        &[
            Lit("devicectl"),
            Lit("device"),
            Lit("info"),
            Lit("apps"),
            Lit("--device"),
            Udid,
            Lit("--json-output"),
            P,
        ],
    ),
    (
        Tool::Xcrun,
        &[Lit("simctl"), Lit("list"), Lit("devices"), Lit("--json")],
    ),
    (Tool::Xcrun, &[Lit("simctl"), Lit("listapps"), Udid]),
    (Tool::Xcrun, &[Lit("simctl"), Lit("boot"), Udid]),
    (
        Tool::Xcrun,
        &[Lit("simctl"), Lit("bootstatus"), Udid, Lit("-b")],
    ),
    (Tool::Xcrun, &[Lit("simctl"), Lit("shutdown"), Udid]),
    (
        Tool::Plutil,
        &[Lit("-convert"), Lit("json"), Lit("-o"), Lit("-"), Lit("-")],
    ),
    (
        Tool::Plutil,
        &[
            Lit("-extract"),
            Lit("CFBundleShortVersionString"),
            Lit("raw"),
            P,
        ],
    ),
    (
        Tool::Ideviceinfo,
        &[Lit("-u"), Udid, Lit("-q"), Lit("com.apple.mobile.battery")],
    ),
    // agent-device runs as `<node> <script> …`.
    (
        Tool::Node,
        &[P, Lit("device"), Lit("status"), Lit("--json")],
    ),
    (Tool::Node, &[P, Lit("--version")]),
    (
        Tool::Node,
        &[
            P,
            Lit("close"),
            Lit("--session"),
            Session,
            Lit("--platform"),
            Plat,
            Lit("--serial"),
            Serial,
            Lit("--json"),
        ],
    ),
    (
        Tool::Node,
        &[
            P,
            Lit("close"),
            Lit("--session"),
            Session,
            Lit("--platform"),
            Plat,
            Lit("--udid"),
            Udid,
            Lit("--json"),
        ],
    ),
];

/// Whether `tool args` is exactly one of `ALLOWED`.
pub fn allowed(tool: Tool, args: &[&OsStr]) -> bool {
    ALLOWED.iter().any(|(t, pattern)| {
        *t == tool
            && pattern.len() == args.len()
            && pattern.iter().zip(args).all(|(p, a)| p.matches(a))
    })
}

const SCRIPT_NAMES: [(&str, &str); 4] = [
    (IDENTITY, "IDENTITY"),
    (TELEMETRY, "TELEMETRY"),
    (SNAPSHOT, "SNAPSHOT"),
    (BOOT_COMPLETED, "BOOT_COMPLETED"),
];

/// The args of an allowed call as they are logged: a script by its constant's name.
fn describe(args: &[&OsStr]) -> String {
    args.iter()
        .map(|a| {
            let a = a.to_string_lossy();
            match SCRIPT_NAMES.iter().find(|(script, _)| *script == a) {
                Some((_, name)) => (*name).to_owned(),
                None => a.into_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------------------------
// Tool paths (D2, P-4)

/// The `devices.*` path settings and `devices.env.*` (P2-18), as the service read them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolSettings {
    pub adb_path: Option<PathBuf>,
    pub agent_device_path: Option<PathBuf>,
    pub agent_device_script: Option<PathBuf>,
    pub node_path: Option<PathBuf>,
    /// The caller's Android variables that `devices tools --detect` recorded; they win over the
    /// service's own environment.
    pub env: BTreeMap<String, String>,
}

/// agent-device runs as `<node> <script>`, both absolute, so the service's `PATH` never matters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDeviceCmd {
    pub node: PathBuf,
    pub script: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPaths {
    pub adb: Option<PathBuf>,
    pub emulator: Option<PathBuf>,
    pub xcrun: Option<PathBuf>,
    pub plutil: Option<PathBuf>,
    pub ideviceinfo: Option<PathBuf>,
    pub agent_device: Option<AgentDeviceCmd>,
    pub problems: Vec<String>,
}

/// An absolute path to a file. A relative one is never used: `Command::new` would search `PATH`
/// for a bare name (P3bT2-3).
fn is_file(p: &Path) -> bool {
    p.is_absolute() && std::fs::metadata(p).is_ok_and(|m| m.is_file())
}

fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| is_file(p))
}

/// Finds every tool in the Global Constraints order. adb on `PATH` is never used, and nothing
/// here reads `PATH`. `env` is the service's environment; `home` the user's home directory. A
/// recorded path that no longer exists falls through to the search.
pub fn find_tools(
    settings: &ToolSettings,
    env: &dyn Fn(&str) -> Option<OsString>,
    home: &Path,
) -> ToolPaths {
    if cfg!(debug_assertions)
        && let Some(dir) = env(FAKE_TOOLS_ENV)
    {
        return fake_paths(Path::new(&dir));
    }
    let var = |name: &str| {
        settings
            .env
            .get(name)
            .map(OsString::from)
            .or_else(|| env(name))
            .map(PathBuf::from)
            .filter(|v| v.is_absolute())
    };
    let mut problems = Vec::new();

    let mut sdks: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .into_iter()
        .filter_map(var)
        .collect();
    let configured = settings.adb_path.is_some() || !sdks.is_empty();
    sdks.push(default_sdk(&var, home));
    let adb = settings
        .adb_path
        .clone()
        .filter(|p| is_file(p))
        .or_else(|| {
            first_file(
                sdks.iter()
                    .map(|s| s.join("platform-tools").join(exe("adb"))),
            )
        });
    if adb.is_none() && configured {
        problems.push(
            "adb not found in the configured Android SDK; fix: hostler devices tools --detect, \
             or set ANDROID_HOME"
                .to_owned(),
        );
    }
    // The emulator of adb's own SDK first.
    let adb_sdk = adb
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let emulator = first_file(
        adb_sdk
            .into_iter()
            .chain(sdks)
            .map(|s| s.join("emulator").join(exe("emulator"))),
    );

    let macos = cfg!(target_os = "macos");
    let xcrun = macos
        .then(|| first_file([PathBuf::from("/usr/bin/xcrun")]))
        .flatten();
    let plutil = macos
        .then(|| first_file([PathBuf::from("/usr/bin/plutil")]))
        .flatten();
    let ideviceinfo = macos
        .then(|| {
            first_file([
                PathBuf::from("/opt/homebrew/bin/ideviceinfo"),
                PathBuf::from("/usr/local/bin/ideviceinfo"),
            ])
        })
        .flatten();

    let agent_device = find_agent_device(settings, &var, home, &mut problems);
    ToolPaths {
        adb,
        emulator,
        xcrun,
        plutil,
        ideviceinfo,
        agent_device,
        problems,
    }
}

fn fake_paths(dir: &Path) -> ToolPaths {
    let bin = dir.join("bin");
    let at = |t: Tool| Some(bin.join(t.file_name()));
    ToolPaths {
        adb: at(Tool::Adb),
        emulator: at(Tool::Emulator),
        xcrun: at(Tool::Xcrun),
        plutil: at(Tool::Plutil),
        ideviceinfo: at(Tool::Ideviceinfo),
        agent_device: Some(AgentDeviceCmd {
            node: bin.join(Tool::Node.file_name()),
            script: bin.join("agent-device.mjs"),
        }),
        problems: Vec::new(),
    }
}

fn default_sdk(var: &dyn Fn(&str) -> Option<PathBuf>, home: &Path) -> PathBuf {
    if cfg!(windows) {
        var("LOCALAPPDATA")
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("Android")
            .join("Sdk")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Android/sdk")
    } else {
        home.join("Android/Sdk")
    }
}

/// volta, asdf, fnm's and nvm's default aliases, each only when it exists (L54, P3-5).
fn node_manager_places(var: &dyn Fn(&str) -> Option<PathBuf>, home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut places = vec![
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".local/share/fnm/aliases/default/bin"),
    ];
    if cfg!(target_os = "macos") {
        places.push(home.join("Library/Application Support/fnm/aliases/default/bin"));
    }
    let nvm = var("NVM_DIR").unwrap_or_else(|| home.join(".nvm"));
    if let Some(bin) = nvm_default_bin(&nvm) {
        places.push(bin);
    }
    places.retain(|p| p.is_dir());
    places
}

/// nvm's default Node: `<nvm>/alias/default` holds a version (`22`, `v22.1.0`), `node` or
/// `stable` (the newest installed), or another alias (`lts/*` → `lts/jod` → `v22.11.0`),
/// followed for a few hops.
fn nvm_default_bin(nvm: &Path) -> Option<PathBuf> {
    let versions = nvm.join("versions/node");
    let mut alias = "default".to_owned();
    for _ in 0..5 {
        if alias.contains("..") {
            return None;
        }
        let value = std::fs::read_to_string(nvm.join("alias").join(&alias)).ok()?;
        let value = value.trim();
        let number = value.trim_start_matches('v');
        if value == "node" || value == "stable" {
            return newest_node(&versions, None);
        }
        if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return newest_node(&versions, Some(number));
        }
        alias = value.to_owned();
    }
    None
}

/// `versions/node/v<wanted>/bin`, or the newest installed version `wanted` is a prefix of (any,
/// without `wanted`).
fn newest_node(versions: &Path, wanted: Option<&str>) -> Option<PathBuf> {
    if let Some(w) = wanted {
        let exact = versions.join(format!("v{w}/bin"));
        if exact.is_dir() {
            return Some(exact);
        }
    }
    let prefix = wanted
        .map(|w| format!("v{w}."))
        .unwrap_or_else(|| "v".to_owned());
    let mut matching: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(versions)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let key: Option<Vec<u64>> = name
                .strip_prefix('v')?
                .split('.')
                .map(|n| n.parse().ok())
                .collect();
            name.starts_with(&prefix)
                .then_some(())
                .and(key)
                .map(|k| (k, e.path().join("bin")))
        })
        .collect();
    matching.sort();
    matching.pop().map(|(_, bin)| bin).filter(|b| b.is_dir())
}

fn launcher_places(var: &dyn Fn(&str) -> Option<PathBuf>, home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        let mut places = Vec::new();
        if let Some(appdata) = var("APPDATA") {
            places.push(appdata.join("npm").join("agent-device.cmd"));
        }
        if let Some(local) = var("LOCALAPPDATA") {
            places.push(local.join("Volta").join("bin").join("agent-device.cmd"));
        }
        return places;
    }
    let mut dirs = vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".npm-global/bin"),
    ];
    dirs.extend(node_manager_places(var, home));
    dirs.into_iter().map(|d| d.join("agent-device")).collect()
}

fn node_places(
    var: &dyn Fn(&str) -> Option<PathBuf>,
    home: &Path,
    launcher_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = launcher_dir.map(Path::to_path_buf).into_iter().collect();
    if cfg!(windows) {
        if let Some(pf) = var("ProgramFiles") {
            dirs.push(pf.join("nodejs"));
        }
    } else {
        dirs.extend(node_manager_places(var, home));
        dirs.push(home.join(".local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs.into_iter().map(|d| d.join(exe("node"))).collect()
}

const AGENT_DEVICE_FIX: &str = "install it globally (npm i -g agent-device@0.21.36), or point at \
a project's copy: hostler devices tools --agent-device \
<checkout>/node_modules/agent-device/bin/agent-device.mjs";

fn find_agent_device(
    settings: &ToolSettings,
    var: &dyn Fn(&str) -> Option<PathBuf>,
    home: &Path,
    problems: &mut Vec<String>,
) -> Option<AgentDeviceCmd> {
    // The setting first, then the recorded script, then the launcher places. A launcher that
    // leads to no script (a Volta shim, a stale wrapper) never hides a later one (P3bT2-7).
    let mut unresolved = None;
    let found = match first_resolved(settings.agent_device_path.clone(), &mut unresolved) {
        Some((l, s)) => Some((Some(l), s)),
        None => match settings
            .agent_device_script
            .clone()
            .filter(|p| is_file(p) && is_script(p))
        {
            Some(s) => Some((None, s)),
            None => first_resolved(launcher_places(var, home), &mut unresolved)
                .map(|(l, s)| (Some(l), s)),
        },
    };
    let Some((launcher, script)) = found else {
        if let Some(l) = unresolved {
            problems.push(format!(
                "agent-device at {} does not lead to its package script; fix: {AGENT_DEVICE_FIX}",
                l.display()
            ));
        }
        return None;
    };
    let launcher_dir = launcher.as_deref().and_then(Path::parent);
    let node = settings
        .node_path
        .clone()
        .filter(|p| is_file(p))
        .or_else(|| first_file(node_places(var, home, launcher_dir)));
    match node {
        Some(node) => Some(AgentDeviceCmd { node, script }),
        None => {
            problems.push(
                "agent-device's Node not found; fix: hostler devices tools --detect, or \
                 hostler devices tools --node <path>"
                    .to_owned(),
            );
            None
        }
    }
}

/// The first launcher file that resolves to its script; the first one that does not is noted.
fn first_resolved(
    launchers: impl IntoIterator<Item = PathBuf>,
    unresolved: &mut Option<PathBuf>,
) -> Option<(PathBuf, PathBuf)> {
    for launcher in launchers.into_iter().filter(|l| is_file(l)) {
        match resolve_agent_device(&launcher) {
            Some(script) => return Some((launcher, script)),
            None => {
                unresolved.get_or_insert(launcher);
            }
        }
    }
    None
}

fn is_script(p: &Path) -> bool {
    matches!(
        p.extension().and_then(OsStr::to_str),
        Some("mjs" | "js" | "cjs")
    )
}

/// The agent-device package's `bin` script behind a launcher: the launcher itself when it is the
/// script, a symlink's target, an npm `.cmd` shim's target, or the npm global layout.
pub fn resolve_agent_device(launcher: &Path) -> Option<PathBuf> {
    if is_script(launcher) {
        return is_file(launcher).then(|| launcher.to_path_buf());
    }
    if let Ok(target) = std::fs::canonicalize(launcher)
        && is_script(&target)
        && is_file(&target)
    {
        return Some(target);
    }
    let dir = launcher.parent()?;
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(launcher) {
        let _ = file.take(64 << 10).read_to_end(&mut head);
    }
    if let Some(rel) = cmd_shim_target(&String::from_utf8_lossy(&head)) {
        let script = dir.join(rel);
        if is_file(&script) {
            return Some(script);
        }
    }
    // The npm global layout (`<prefix>/bin` beside `<prefix>/lib/node_modules`, or the Windows
    // prefix itself), then Volta's package image beside `~/.volta/bin`.
    let root = dir.parent();
    let volta = root.map(|r| {
        r.join("tools")
            .join("image")
            .join("packages")
            .join("agent-device")
    });
    root.map(|r| r.join("lib").join("node_modules"))
        .into_iter()
        .chain([dir.join("node_modules")])
        .chain(
            volta
                .iter()
                .flat_map(|v| [v.join("lib").join("node_modules"), v.join("node_modules")]),
        )
        .find_map(|modules| package_bin(&modules.join("agent-device")))
}

/// The script an npm cmd shim runs: `"%dp0%\node_modules\…\x.mjs"`, relative to the shim.
fn cmd_shim_target(text: &str) -> Option<PathBuf> {
    ["%dp0%\\", "%~dp0\\"].iter().find_map(|marker| {
        text.match_indices(marker).find_map(|(at, _)| {
            let rest = &text[at + marker.len()..];
            let rel = &rest[..rest.find('"')?];
            let path: PathBuf = rel.split(['\\', '/']).filter(|c| !c.is_empty()).collect();
            is_script(&path).then_some(path)
        })
    })
}

/// `<pkg>/<bin>` from the package's `package.json` (`"bin": "x"` or `{"agent-device": "x"}`).
fn package_bin(pkg: &Path) -> Option<PathBuf> {
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(pkg.join("package.json")).ok()?).ok()?;
    let bin = match &json["bin"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => map.get("agent-device")?.as_str()?.to_owned(),
        _ => return None,
    };
    let script = pkg.join(bin);
    is_file(&script).then_some(script)
}

/// `devices tools --detect`, run in the person's shell: the agent-device launcher, its script and
/// Node from `path_var`, never adb. Relative entries and `node_modules/.bin` entries are skipped,
/// so a project-local copy is never picked up. The caller fills `android_env` and drops entries
/// inside registered workspaces.
pub fn detect_from_path(path_var: &OsStr) -> ToolPathsSet {
    let launcher_names: &[&str] = if cfg!(windows) {
        &["agent-device.cmd", "agent-device.exe"]
    } else {
        &["agent-device"]
    };
    let mut set = ToolPathsSet::default();
    for dir in std::env::split_paths(path_var) {
        if !dir.is_absolute() || dir.ends_with(Path::new("node_modules").join(".bin")) {
            continue;
        }
        if set.agent_device.is_none()
            && let Some(launcher) = first_file(launcher_names.iter().map(|n| dir.join(n)))
        {
            set.detected_script =
                resolve_agent_device(&launcher).map(|s| s.to_string_lossy().into_owned());
            set.agent_device = Some(launcher.to_string_lossy().into_owned());
        }
        if set.node.is_none() && is_file(&dir.join(exe("node"))) {
            set.node = Some(dir.join(exe("node")).to_string_lossy().into_owned());
        }
    }
    set
}

// ---------------------------------------------------------------------------------------------
// The bounded runner

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolOutput {
    /// `None` when killed by a signal, or not reaped within the grace.
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    /// A stream passed `OUTPUT_CAP`; the rest was read and dropped.
    pub truncated: bool,
}

/// One tool call. The lanes run it on the blocking pool.
pub struct Call<'a> {
    pub tool: Tool,
    pub path: &'a Path,
    pub args: &'a [&'a OsStr],
    pub env: &'a [(&'a str, &'a OsStr)],
    pub stdin: Option<&'a [u8]>,
    pub timeout: Duration,
    /// An adb call that may start a server: the server escapes the call's job, and on timeout
    /// only the client is killed (P-7).
    pub adb_breakaway: bool,
    /// Required for every `Tool::Adb` call but `adb version` (P3-10, P3bT2-8): the pass
    /// `probes::adb_guard` gives right before the call. Without one nothing is spawned.
    pub adb_guard: Option<AdbGuardPass>,
}

/// Proof that `probes::adb_guard` cleared one adb call: the server answering on the adb port is
/// ours, or none answers (P2-2, P3-10). Only `devices::probes` makes one
/// (`only_probes_make_guard_passes`); it is not `Clone`, so each pass covers one call.
#[derive(Debug)]
pub struct AdbGuardPass(());

impl AdbGuardPass {
    /// For `probes::adb_guard` only.
    pub(in crate::devices) fn cleared() -> AdbGuardPass {
        AdbGuardPass(())
    }

    /// Tests against the fake adb, which has no server to ask. Debug builds only.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn for_tests() -> AdbGuardPass {
        AdbGuardPass(())
    }
}

/// The Android variables `devices.env.*` may carry (P2-18): the only names adb and the emulator
/// get from a call.
pub const ANDROID_ENV: &[&str] = &[
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "ANDROID_SDK_HOME",
    "ANDROID_USER_HOME",
    "ANDROID_AVD_HOME",
    "ANDROID_EMULATOR_HOME",
    "ANDROID_ADB_SERVER_PORT",
    "ADB_SERVER_SOCKET",
];

/// The environment names a call may set, per tool (P3bT2-6): `NODE_OPTIONS`,
/// `DYLD_INSERT_LIBRARIES` and the like never widen the argument allowlist sideways.
pub fn env_allowed(tool: Tool, name: &str) -> bool {
    match tool {
        Tool::Adb | Tool::Emulator => ANDROID_ENV.contains(&name),
        Tool::Node => name == "AGENT_DEVICE_STATE_DIR",
        Tool::Xcrun | Tool::Plutil | Tool::Ideviceinfo => false,
    }
}

fn refuse(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_owned())
}

/// Checks shared by every spawn: an absolute path, an allowed vector, allowed env names. A refused
/// call is logged by tool, argument count, first literal and script name only: its values may come
/// from a device (P3bT2-12).
fn check(tool: Tool, path: &Path, args: &[&OsStr], env: &[(&str, &OsStr)]) -> io::Result<()> {
    let why = if !path.is_absolute() {
        "a tool path that is not absolute"
    } else if !allowed(tool, args) {
        "not an allowed call"
    } else if env.iter().any(|(name, _)| !env_allowed(tool, name)) {
        "an environment name this tool may not get"
    } else {
        return Ok(());
    };
    // Only a literal hostler itself would send, never the value given.
    let first = args.first().and_then(|a| a.to_str()).and_then(|a| {
        ALLOWED
            .iter()
            .filter(|(t, _)| *t == tool)
            .find_map(|(_, p)| match p.first() {
                Some(Arg::Lit(lit)) if *lit == a => Some(*lit),
                _ => None,
            })
    });
    let script = args
        .iter()
        .find_map(|a| SCRIPT_NAMES.iter().find(|(s, _)| OsStr::new(s) == *a))
        .map(|(_, name)| *name);
    tracing::warn!(
        tool = tool.name(),
        args = args.len(),
        first = first.unwrap_or("-"),
        script = script.unwrap_or("-"),
        "refused device call: {why}"
    );
    Err(refuse(why))
}

/// Runs one allowed call within its bound; see the module doc. Refused calls spawn nothing.
pub fn run_tool(call: &Call) -> io::Result<ToolOutput> {
    check(call.tool, call.path, call.args, call.env)?;
    if call.tool == Tool::Adb {
        // Only `ensure_adb_server` starts a server: with null stdio, detached (P3bT2-8).
        if call.args == [OsStr::new("start-server")] {
            return Err(refuse(
                "adb start-server runs only through ensure_adb_server",
            ));
        }
        // `adb version` prints the client's own version and never contacts a server.
        if call.adb_guard.is_none() && call.args != [OsStr::new("version")] {
            return Err(refuse("an adb call without the adb guard's pass"));
        }
    }
    let what = describe(call.args);
    let started = Instant::now();
    let result = run_bounded(call);
    let ms = started.elapsed().as_millis();
    match &result {
        Ok(out) => tracing::debug!(
            tool = call.tool.name(),
            args = %what,
            status = ?out.status,
            timed_out = out.timed_out,
            ms,
            "device tool"
        ),
        Err(e) => {
            tracing::debug!(tool = call.tool.name(), args = %what, ms, error = %e, "device tool")
        }
    }
    result
}

fn run_bounded(call: &Call) -> io::Result<ToolOutput> {
    let mut command = Command::new(call.path);
    command
        .args(call.args)
        .stdin(if call.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in call.env {
        command.env(name, value);
    }
    let deadline = Instant::now() + call.timeout;
    let (mut child, tree) = os::Tree::spawn(&mut command, call.adb_breakaway)?;

    if let (Some(bytes), Some(mut pipe)) = (call.stdin, child.stdin.take()) {
        let bytes = bytes.to_vec();
        // A tool that exits without reading gives a broken pipe: not an error of the call.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let (tx, rx) = mpsc::channel();
    let readers: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    let mut open = [false; 2];
    for (i, reader) in readers.into_iter().enumerate() {
        if let Some(mut reader) = reader {
            open[i] = true;
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0; 64 << 10];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send((i, Some(buf[..n].to_vec()))).is_err() {
                                return;
                            }
                        }
                    }
                }
                let _ = tx.send((i, None));
            });
        }
    }
    drop(tx);

    let mut out = ToolOutput::default();
    let mut ended: Option<Instant> = None;
    let mut status: Option<ExitStatus> = None;
    loop {
        let now = Instant::now();
        if ended.is_none() {
            if tree.exited(&mut child) {
                // Whatever the tool left in its tree goes with it (not a breakaway server).
                tree.end_after_exit();
                ended = Some(now);
            } else if now >= deadline {
                tree.kill(&mut child);
                out.timed_out = true;
                ended = Some(now);
            }
        }
        if let Some(at) = ended {
            if status.is_none() {
                status = child.try_wait().ok().flatten();
            }
            if (status.is_some() && open == [false; 2]) || now >= at + DRAIN_GRACE {
                break;
            }
        }
        let wait = match ended {
            Some(at) => (at + DRAIN_GRACE).saturating_duration_since(now).min(POLL),
            None => deadline.saturating_duration_since(now).min(POLL),
        };
        if open == [false; 2] {
            std::thread::sleep(wait);
            continue;
        }
        match rx.recv_timeout(wait) {
            Ok((i, Some(chunk))) => {
                let buf = if i == 0 {
                    &mut out.stdout
                } else {
                    &mut out.stderr
                };
                let room = OUTPUT_CAP.saturating_sub(buf.len());
                if chunk.len() > room {
                    out.truncated = true;
                }
                buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Ok((i, None)) => open[i] = false,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => open = [false; 2],
        }
    }
    match status {
        Some(s) => out.status = s.code(),
        // Killed but not reaped within the grace (a process stuck in the kernel): reap later.
        None => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    Ok(out)
}

/// Starts `emulator -avd <name> …` detached (P-6): it outlives the call and the service. Unix:
/// a new session, null stdin, output to `log`; Windows: out of the service's job when the job
/// allows it. `env` is the merged `devices.env.*` (P3bT2-5). Returns the pid and its start time
/// (`procs` units). Only the emulator's boot vector is accepted.
pub fn spawn_detached(
    path: &Path,
    args: &[&OsStr],
    env: &[(&str, &OsStr)],
    log: &Path,
) -> io::Result<(u32, u64)> {
    check(Tool::Emulator, path, args, env)?;
    if args.first() != Some(&OsStr::new("-avd")) {
        return Err(refuse("only the emulator boot runs detached"));
    }
    let file = std::fs::File::create(log)?;
    let mut command = Command::new(path);
    command
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file);
    let mut child = os::spawn_detached(&mut command)?;
    let pid = child.id();
    let table = SysProcessTable::new().map_err(io::Error::other)?;
    // The child is not reaped yet, so its start time is still readable.
    let start = table.info(pid).and_then(|i| i.start_time);
    tracing::info!(args = %describe(args), pid, "started detached");
    // Reaped when it ends, so no zombie stays behind while the service runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let start = start.ok_or_else(|| {
        io::Error::other(format!(
            "cannot read the start time of the started process {pid}"
        ))
    })?;
    Ok((pid, start))
}

/// `adb start-server` with null stdio, detached, bounded to 10 s (P-7). Call it only when no
/// server answers `host:version` (P2-2). A client stuck past the bound is killed; the server it
/// started is not in its group or job, so it stays. `env` is the merged `devices.env.*`, so the
/// server listens where the person's adb expects it (P3bT2-5).
pub fn ensure_adb_server(adb: &Path, env: &[(&str, &OsStr)]) -> io::Result<()> {
    check(Tool::Adb, adb, &[OsStr::new("start-server")], env)?;
    let started = Instant::now();
    let mut command = Command::new(adb);
    command
        .arg("start-server")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = os::spawn_detached(&mut command)?;
    let deadline = started + START_SERVER_BOUND;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            tracing::warn!("adb start-server timed out");
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "adb start-server did not finish within 10 s",
            ));
        }
        std::thread::sleep(POLL);
    };
    tracing::debug!(tool = "adb", args = "start-server", status = ?status.code(), ms = started.elapsed().as_millis(), "device tool");
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "adb start-server exited with {:?}",
            status.code()
        )))
    }
}

/// The adb client's version: `adb version` → `1.0.N` → N. Read once per binary: the cache is keyed
/// by path, size and modification time, so an SDK update at the same path is read again (P3bT2-4).
pub fn adb_client_version(adb: &Path) -> Option<u32> {
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static SEEN: OnceLock<Mutex<HashMap<Key, u32>>> = OnceLock::new();
    let meta = std::fs::metadata(adb).ok()?;
    let key = (adb.to_path_buf(), meta.len(), meta.modified().ok());
    let seen = SEEN.get_or_init(Default::default);
    if let Some(v) = seen.lock().ok()?.get(&key) {
        return Some(*v);
    }
    let out = run_tool(&Call {
        tool: Tool::Adb,
        path: adb,
        args: &[OsStr::new("version")],
        env: &[],
        stdin: None,
        timeout: VERSION_BOUND,
        adb_breakaway: false,
        adb_guard: None,
    })
    .ok()?;
    let version = parse_adb_version(&String::from_utf8_lossy(&out.stdout))?;
    seen.lock().ok()?.insert(key, version);
    Some(version)
}

fn parse_adb_version(out: &str) -> Option<u32> {
    let rest = out.split("Android Debug Bridge version 1.0.").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The last guard (P2-2): an adb client that met a server of another version says so on stderr
/// (and kills it). A fixed text with the two versions; nothing else of the output is echoed.
pub fn adb_mismatch(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .find(|l| l.contains("doesn't match this client"))?;
    let numbers: Vec<u32> = line
        .split(['(', ')'])
        .skip(1)
        .step_by(2)
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    Some(match numbers[..] {
        [server, client] => {
            format!("adb server version ({server}) doesn't match this client ({client})")
        }
        _ => "the adb server's version doesn't match this client".to_owned(),
    })
}

#[cfg(unix)]
mod os {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    /// The call's process group: the tool leads it (`process_group(0)`).
    pub struct Tree {
        pid: libc::pid_t,
        breakaway: bool,
    }

    impl Tree {
        pub fn spawn(command: &mut Command, breakaway: bool) -> io::Result<(Child, Tree)> {
            command.process_group(0);
            let child = command.spawn()?;
            let pid = child.id() as libc::pid_t;
            Ok((child, Tree { pid, breakaway }))
        }

        /// Exited, without reaping: the zombie keeps the pid and its group id from reuse until
        /// the group kill below has run.
        pub fn exited(&self, _child: &mut Child) -> bool {
            // SAFETY: a zeroed siginfo_t is valid; waitid only writes into it.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: plain syscall on our own child with a valid out pointer.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc != 0 {
                // ECHILD: already reaped; anything else (EINTR): look again next round.
                return io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
            }
            info.si_signo != 0
        }

        pub fn end_after_exit(&self) {
            if !self.breakaway {
                // SAFETY: no memory preconditions; the group id is still reserved by the zombie.
                unsafe { libc::killpg(self.pid, libc::SIGKILL) };
            }
        }

        pub fn kill(&self, _child: &mut Child) {
            // SAFETY: no memory preconditions; the leader is alive (not reaped).
            unsafe {
                if self.breakaway {
                    libc::kill(self.pid, libc::SIGKILL);
                } else {
                    libc::killpg(self.pid, libc::SIGKILL);
                }
            }
        }
    }

    /// A new session (`setsid`): no terminal, group or session ties to the service.
    pub fn spawn_detached(command: &mut Command) -> io::Result<Child> {
        // SAFETY: setsid is async-signal-safe and touches no memory.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn()
    }
}

#[cfg(windows)]
mod os {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
        OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    /// The call's kill-on-close job. The tool starts suspended and runs only once it is inside,
    /// so nothing it starts escapes unless `breakaway` lets it (P-7).
    pub struct Tree {
        job: HANDLE,
        breakaway: bool,
    }

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Tree {}

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: opened by CreateJobObjectW, closed once; kill-on-close ends what is left.
            unsafe { CloseHandle(self.job) };
        }
    }

    impl Tree {
        pub fn spawn(command: &mut Command, breakaway: bool) -> io::Result<(Child, Tree)> {
            // SAFETY: an unnamed job with default security.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let tree = Tree { job, breakaway };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | if breakaway {
                    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK
                } else {
                    0
                };
            // SAFETY: a valid job handle and a correctly sized limits struct.
            let set = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(io::Error::last_os_error());
            }
            command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
            let mut child = command.spawn()?;
            // SAFETY: both handles are open; the child is still suspended.
            let assigned =
                unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
            if assigned == 0 || !resume(child.id()) {
                let error = io::Error::last_os_error();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok((child, tree))
        }

        pub fn exited(&self, child: &mut Child) -> bool {
            child.try_wait().map(|s| s.is_some()).unwrap_or(true)
        }

        pub fn end_after_exit(&self) {
            if !self.breakaway {
                // SAFETY: a valid job handle.
                unsafe { TerminateJobObject(self.job, 1) };
            }
        }

        pub fn kill(&self, child: &mut Child) {
            if self.breakaway {
                let _ = child.kill();
            } else {
                // SAFETY: a valid job handle.
                unsafe { TerminateJobObject(self.job, 1) };
            }
        }
    }

    /// Resumes the threads of a process created suspended; false when none was resumed.
    fn resume(pid: u32) -> bool {
        // SAFETY: a thread snapshot, walked with a correctly sized entry, closed once.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut resumed = false;
            let mut more = Thread32First(snapshot, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        resumed |= ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            resumed
        }
    }

    /// Out of the service's job (Task Scheduler's), in its own process group; a job that forbids
    /// breakaway answers ACCESS_DENIED: start inside it then, as `client.rs` does.
    pub fn spawn_detached(command: &mut Command) -> io::Result<Child> {
        let base = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
        command.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        match command.spawn() {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                command.creation_flags(base);
                command.spawn()
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    fn env_of(vars: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_os_str().to_owned()))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    /// The machine's own `/opt/homebrew/bin` and `/usr/local/bin` are searched too, so tests
    /// count only adb's problems.
    fn adb_problems(found: &ToolPaths) -> usize {
        found
            .problems
            .iter()
            .filter(|p| p.starts_with("adb"))
            .count()
    }

    fn adb_in(sdk: &Path) -> PathBuf {
        sdk.join("platform-tools").join(exe("adb"))
    }

    #[cfg(not(windows))]
    fn default_sdk_of(home: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            home.join("Library/Android/sdk")
        } else {
            home.join("Android/Sdk")
        }
    }

    #[cfg(windows)]
    fn default_sdk_of(home: &Path) -> PathBuf {
        home.join("AppData")
            .join("Local")
            .join("Android")
            .join("Sdk")
    }

    #[test]
    fn adb_is_found_in_setting_then_android_home_then_sdk_root_then_default() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let (set, ah, root) = (
            t.path().join("set"),
            t.path().join("ah"),
            t.path().join("root"),
        );
        let def = default_sdk_of(&home);
        for sdk in [&set, &ah, &root, &def] {
            touch(&adb_in(sdk));
        }
        let env = env_of(&[("ANDROID_HOME", &ah), ("ANDROID_SDK_ROOT", &root)]);
        let mut settings = ToolSettings {
            adb_path: Some(adb_in(&set)),
            ..Default::default()
        };
        assert_eq!(find_tools(&settings, &env, &home).adb, Some(adb_in(&set)));
        settings.adb_path = Some(t.path().join("gone/adb"));
        assert_eq!(find_tools(&settings, &env, &home).adb, Some(adb_in(&ah)));
        std::fs::remove_file(adb_in(&ah)).unwrap();
        assert_eq!(find_tools(&settings, &env, &home).adb, Some(adb_in(&root)));
        std::fs::remove_file(adb_in(&root)).unwrap();
        let found = find_tools(&settings, &env, &home);
        assert_eq!(found.adb, Some(adb_in(&def)));
        assert_eq!(adb_problems(&found), 0, "{:?}", found.problems);
        // `devices.env.ANDROID_HOME` wins over the service's own variable (P2-18).
        touch(&adb_in(&root));
        let recorded = ToolSettings {
            env: [(
                "ANDROID_HOME".to_owned(),
                root.to_string_lossy().into_owned(),
            )]
            .into(),
            ..Default::default()
        };
        let other = env_of(&[("ANDROID_HOME", &ah)]);
        assert_eq!(
            find_tools(&recorded, &other, &home).adb,
            Some(adb_in(&root))
        );
    }

    #[test]
    fn emulator_comes_from_the_sdk() {
        let t = tempfile::tempdir().unwrap();
        let sdk = t.path().join("sdk");
        touch(&adb_in(&sdk));
        let emulator = sdk.join("emulator").join(exe("emulator"));
        touch(&emulator);
        let env = env_of(&[("ANDROID_HOME", &sdk)]);
        let found = find_tools(&ToolSettings::default(), &env, &t.path().join("home"));
        assert_eq!(found.emulator, Some(emulator));
    }

    #[test]
    fn adb_on_path_is_never_used() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        touch(&bin.join(exe("adb")));
        let env = env_of(&[("PATH", &bin)]);
        let found = find_tools(&ToolSettings::default(), &env, &t.path().join("home"));
        assert_eq!(found.adb, None);
        // No SDK is configured: no adb is not a problem (a machine without Android).
        assert_eq!(adb_problems(&found), 0, "{:?}", found.problems);
        let detected = detect_from_path(bin.as_os_str());
        assert_eq!(detected.adb, None);
    }

    /// The npm global layout: `<prefix>/bin/agent-device` → `../lib/node_modules/agent-device/…`.
    #[cfg(unix)]
    fn npm_global(prefix: &Path) -> (PathBuf, PathBuf) {
        let pkg = prefix.join("lib/node_modules/agent-device");
        let script = pkg.join("bin/agent-device.mjs");
        touch(&script);
        std::fs::write(
            pkg.join("package.json"),
            br#"{"name":"agent-device","bin":{"agent-device":"bin/agent-device.mjs"}}"#,
        )
        .unwrap();
        let launcher = prefix.join("bin/agent-device");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(
            "../lib/node_modules/agent-device/bin/agent-device.mjs",
            &launcher,
        )
        .unwrap();
        (launcher, script.canonicalize().unwrap())
    }

    #[test]
    fn agent_device_resolves_launcher_to_node_and_script() {
        let t = tempfile::tempdir().unwrap();
        // A Windows npm cmd shim (the fixture), resolved on every OS.
        let npm = t.path().join("npm");
        let shim = npm.join("agent-device.cmd");
        std::fs::create_dir_all(&npm).unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/devices/agent-device.cmd"
            ),
            &shim,
        )
        .unwrap();
        let script = npm.join("node_modules/agent-device/bin/agent-device.mjs");
        assert_eq!(
            resolve_agent_device(&shim),
            None,
            "the script does not exist yet"
        );
        touch(&script);
        assert_eq!(resolve_agent_device(&shim), Some(script.clone()));
        // A path that is the script itself.
        assert_eq!(resolve_agent_device(&script), Some(script));
        #[cfg(unix)]
        {
            let (launcher, script) = npm_global(&t.path().join("prefix"));
            assert_eq!(resolve_agent_device(&launcher), Some(script.clone()));
            // A copied (not linked) launcher still finds the package by the npm layout.
            let copy = t.path().join("prefix/bin/agent-device-copy");
            std::fs::write(&copy, b"#!/usr/bin/env node\n").unwrap();
            assert_eq!(
                resolve_agent_device(&copy),
                Some(
                    t.path()
                        .join("prefix/lib/node_modules/agent-device/bin/agent-device.mjs")
                )
            );
            let node = t.path().join("node-elsewhere").join(exe("node"));
            touch(&node);
            let settings = ToolSettings {
                agent_device_path: Some(launcher.clone()),
                node_path: Some(node.clone()),
                ..Default::default()
            };
            let found = find_tools(&settings, &env_of(&[]), &t.path().join("home"));
            assert_eq!(found.agent_device, Some(AgentDeviceCmd { node, script }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn node_is_found_next_to_the_launcher() {
        let t = tempfile::tempdir().unwrap();
        let (launcher, script) = npm_global(&t.path().join("prefix"));
        let node = launcher.parent().unwrap().join("node");
        touch(&node);
        let settings = ToolSettings {
            agent_device_path: Some(launcher),
            ..Default::default()
        };
        let found = find_tools(&settings, &env_of(&[]), &t.path().join("home"));
        assert_eq!(found.agent_device, Some(AgentDeviceCmd { node, script }));
        assert!(found.problems.is_empty(), "{:?}", found.problems);
    }

    /// launchd/systemd give the service a minimal `PATH`: nothing here reads it (P-4).
    #[cfg(unix)]
    #[test]
    fn managed_service_finds_agent_device_without_path() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        // `~/.local/bin/agent-device` → `~/.local/lib/node_modules/…`, Node beside it.
        let (_, script) = npm_global(&home.join(".local"));
        let node = home.join(".local/bin/node");
        touch(&node);
        let found = find_tools(&ToolSettings::default(), &|_: &str| None, &home);
        assert_eq!(found.agent_device, Some(AgentDeviceCmd { node, script }));
        // An nvm default alias also finds Node, with nothing on PATH.
        let t2 = tempfile::tempdir().unwrap();
        let home2 = t2.path().join("home");
        let (launcher, script2) = npm_global(&t2.path().join("prefix"));
        let nvm_node = home2.join(".nvm/versions/node/v22.4.1/bin/node");
        touch(&nvm_node);
        std::fs::create_dir_all(home2.join(".nvm/alias")).unwrap();
        std::fs::write(home2.join(".nvm/alias/default"), "22\n").unwrap();
        let settings = ToolSettings {
            agent_device_path: Some(launcher.clone()),
            ..Default::default()
        };
        // The launcher's own directory has no node, so the nvm place is next.
        let found = find_tools(&settings, &|_: &str| None, &home2);
        assert_eq!(
            found.agent_device,
            Some(AgentDeviceCmd {
                node: nvm_node,
                script: script2
            })
        );
    }

    #[test]
    fn missing_tools_report_problems() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let sdk = t.path().join("sdk-without-adb");
        std::fs::create_dir_all(&sdk).unwrap();
        let found = find_tools(
            &ToolSettings::default(),
            &env_of(&[("ANDROID_HOME", &sdk)]),
            &home,
        );
        assert_eq!(found.adb, None);
        assert_eq!(adb_problems(&found), 1, "{:?}", found.problems);
        assert!(found.problems[0].contains("hostler devices tools --detect"));
        // A found script without Node is a problem too.
        let script = t.path().join("pkg/bin/agent-device.mjs");
        touch(&script);
        let settings = ToolSettings {
            agent_device_script: Some(script),
            ..Default::default()
        };
        let found = find_tools(&settings, &env_of(&[]), &home);
        if found.agent_device.is_none() {
            assert!(
                found.problems.iter().any(|p| p.contains("Node not found")),
                "{:?}",
                found.problems
            );
        }
        // Nothing configured and nothing installed: no problems, no tools.
        let empty = find_tools(&ToolSettings::default(), &env_of(&[]), &home);
        assert_eq!(empty.adb, None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_exe() {
        let t = tempfile::tempdir().unwrap();
        let sdk = t.path().join("sdk");
        let adb = sdk.join("platform-tools").join("adb.exe");
        touch(&adb);
        // Without the suffix, nothing is found.
        touch(&sdk.join("emulator").join("emulator"));
        let found = find_tools(
            &ToolSettings::default(),
            &env_of(&[("ANDROID_HOME", &sdk)]),
            &t.path().join("home"),
        );
        assert_eq!(found.adb, Some(adb));
        assert_eq!(found.emulator, None);
        assert_eq!(Tool::Node.file_name(), "node.exe");
    }

    /// One command of a script: what `scripts_are_read_only` accepts.
    fn read_only_command(cmd: &str) -> bool {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let fixed = |w: &str| {
            !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "{}._:-".contains(c))
        };
        match words[..] {
            ["getprop", name] => name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_'),
            ["dumpsys", "battery"]
            | ["dumpsys", "connectivity"]
            | ["dumpsys", "wifi"]
            | ["dumpsys", "package", "packages"]
            | ["pm", "list", "packages", "-3", "--show-versioncode"]
            | ["date", "+%s"]
            | ["date", "+%z"] => true,
            ["grep", w] | ["grep", "-m1", w] => fixed(w) && !w.starts_with('-'),
            ["echo", marker] => marker
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "=_-".contains(c)),
            _ => false,
        }
    }

    fn read_only_script(script: &str) -> bool {
        let forbidden = [">", "<", "$(", "`", "||"];
        if forbidden.iter().any(|f| script.contains(f)) {
            return false;
        }
        // A lone `&` (background) is refused; `&&` is a separator.
        if script.replace("&&", "").contains('&') {
            return false;
        }
        script
            .split("&&")
            .flat_map(|s| s.split([';', '|', '\n']))
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .all(read_only_command)
    }

    /// Task 3's parsers read exactly these shapes (P3bT3-1).
    #[test]
    fn scripts_give_the_parsers_their_shapes() {
        assert_eq!(
            IDENTITY.split("; ").collect::<Vec<_>>(),
            [
                "getprop ro.serialno",
                "getprop ro.product.model",
                "getprop ro.build.version.release",
                "getprop ro.build.version.sdk",
                "getprop ro.boot.qemu.avd_name",
                "getprop ro.kernel.qemu",
                "getprop ro.boot.qemu",
            ]
        );
        assert!(SNAPSHOT.starts_with("date +%s; date +%z; getprop persist.sys.timezone; "));
        assert!(SNAPSHOT.ends_with(
            "pm list packages -3 --show-versioncode && echo === && dumpsys package packages"
        ));
        assert!(TELEMETRY.starts_with("dumpsys battery; "));
        assert!(TELEMETRY.contains("dumpsys connectivity | grep NetworkAgentInfo{"));
        assert!(TELEMETRY.ends_with("dumpsys wifi | grep -m1 mWifiInfo.SSID:"));
    }

    #[test]
    fn scripts_are_read_only() {
        for script in SCRIPTS {
            assert!(read_only_script(script), "not read-only: {script}");
        }
        // Each script is reachable only as one exact `adb shell` argument.
        for script in SCRIPTS {
            assert!(allowed(
                Tool::Adb,
                &[os("-s"), os("emulator-5554"), os("shell"), os(script)]
            ));
        }
    }

    #[test]
    fn scripts_with_redirects_or_substitutions_are_refused() {
        for bad in [
            "dumpsys battery > /sdcard/x",
            "getprop ro.serialno < /sdcard/x",
            "getprop $(reboot)",
            "getprop `reboot`",
            "getprop ro.serialno || reboot",
            "getprop ro.serialno & reboot",
            "dumpsys battery unplug",
            "dumpsys battery reset",
            "getprop ro.serialno; reboot",
            "pm uninstall com.example",
            "getprop ro.serialno && setprop x 1",
            "grep -r x /sdcard",
        ] {
            assert!(!read_only_script(bad), "accepted: {bad}");
        }
        assert!(read_only_script(
            "getprop ro.serialno && getprop ro.product.model"
        ));
    }

    #[test]
    fn allowlist_rejects_dumpsys_battery_unplug_and_unknown_scripts() {
        fn shell(cmd: &str) -> Vec<&OsStr> {
            vec![os("-s"), os("FAKE0SAMS01"), os("shell"), os(cmd)]
        }
        assert!(allowed(Tool::Adb, &shell(TELEMETRY)));
        assert!(!allowed(Tool::Adb, &shell("dumpsys battery unplug")));
        assert!(!allowed(Tool::Adb, &shell("dumpsys battery")));
        assert!(!allowed(Tool::Adb, &shell("getprop ro.serialno")));
        let padded = format!("{TELEMETRY} ");
        assert!(!allowed(Tool::Adb, &shell(&padded)));
        // Split into several arguments, a script is not the constant either.
        let split = [
            os("-s"),
            os("X"),
            os("shell"),
            os("dumpsys"),
            os("battery"),
            os("unplug"),
        ];
        assert!(!allowed(Tool::Adb, &split));
        // State-changing adb commands are not there at all.
        for words in [
            &["tcpip", "5555"][..],
            &["usb"],
            &["root"],
            &["remount"],
            &["reboot"],
            &["kill-server"],
            &["connect", "10.0.0.7:5555"],
            &["pair", "10.0.0.7:5555"],
            &["-s", "X", "install", "/tmp/a.apk"],
            &["-s", "X", "uninstall", "com.example"],
            &["-s", "X", "reboot"],
        ] {
            let args: Vec<&OsStr> = words.iter().map(|w| os(w)).collect();
            assert!(!allowed(Tool::Adb, &args), "{words:?}");
        }
        // A vector of one tool is not allowed for another.
        assert!(!allowed(Tool::Node, &[os("devices"), os("-l")]));
        assert!(allowed(Tool::Adb, &[os("devices"), os("-l")]));
        // Simulator state changes other than boot/shutdown are not there.
        for words in [
            &["simctl", "erase", "X"][..],
            &["simctl", "delete", "X"],
            &["simctl", "uninstall", "X", "com.example"],
        ] {
            let args: Vec<&OsStr> = words.iter().map(|w| os(w)).collect();
            assert!(!allowed(Tool::Xcrun, &args), "{words:?}");
        }
    }

    #[test]
    fn session_args_starting_with_dash_are_refused() {
        let script = if cfg!(windows) {
            r"C:\ad\agent-device.mjs"
        } else {
            "/ad/agent-device.mjs"
        };
        fn close<'a>(script: &'a str, session: &'a str, id: &'a str) -> Vec<&'a OsStr> {
            [
                script,
                "close",
                "--session",
                session,
                "--platform",
                "android",
                "--serial",
                id,
                "--json",
            ]
            .into_iter()
            .map(OsStr::new)
            .collect()
        }
        assert!(allowed(
            Tool::Node,
            &close(script, "p3b-envtest", "emulator-5554")
        ));
        assert!(allowed(
            Tool::Node,
            &close(script, "cwd:0000000000000000:default", "FAKE0TECNO01")
        ));
        for session in ["--all", "-x", "", "a b", "a\nb", "a\u{7}b", "\tx"] {
            assert!(
                !allowed(Tool::Node, &close(script, session, "emulator-5554")),
                "{session:?}"
            );
        }
        assert!(!allowed(Tool::Node, &close(script, "s", "--all")));
        assert!(!allowed(Tool::Xcrun, &[os("simctl"), os("boot"), os("-h")]));
        assert!(!allowed(
            Tool::Emulator,
            &[
                os("-avd"),
                os("-wipe-data"),
                os("-no-window"),
                os("-no-audio"),
                os("-no-boot-anim"),
                os("-no-snapshot-save")
            ]
        ));
        assert!(!allowed(
            Tool::Adb,
            &[os("-s"), os("-d"), os("shell"), os(IDENTITY)]
        ));
        // The script path must be absolute.
        assert!(!allowed(
            Tool::Node,
            &[os("agent-device.mjs"), os("--version")]
        ));
    }

    /// `simctl shutdown all` would stop every simulator, other agents' too (P3bT2-1).
    #[test]
    fn udid_values_must_be_udids() {
        let simctl = |verb: &'static str, udid: &'static str| -> Vec<&'static OsStr> {
            if verb == "bootstatus" {
                vec![os("simctl"), os(verb), os(udid), os("-b")]
            } else {
                vec![os("simctl"), os(verb), os(udid)]
            }
        };
        for verb in ["shutdown", "boot", "bootstatus", "listapps"] {
            for bad in ["all", "booted", "", "-h", "x;y", "$(reboot)", "0000-0000"] {
                assert!(!allowed(Tool::Xcrun, &simctl(verb, bad)), "{verb} {bad:?}");
            }
            for good in [
                "00000000-0000-4000-8000-00000000000A",
                "00008101-000A00000000001E",
                "0123456789abcdef0123456789abcdef01234567",
            ] {
                assert!(allowed(Tool::Xcrun, &simctl(verb, good)), "{verb} {good}");
            }
        }
        let battery =
            |udid: &'static str| vec![os("-u"), os(udid), os("-q"), os("com.apple.mobile.battery")];
        assert!(!allowed(Tool::Ideviceinfo, &battery("x")));
        assert!(allowed(
            Tool::Ideviceinfo,
            &battery("00008101-000A00000000001E")
        ));
        // `emu` commands reach only an emulator's serial.
        let emu = |serial: &'static str| vec![os("-s"), os(serial), os("emu"), os("kill")];
        assert!(allowed(Tool::Adb, &emu("emulator-5554")));
        for bad in [
            "FAKE0SAMS01",
            "10.0.0.7:5555",
            "emulator-",
            "emulator-55x4",
            "all",
        ] {
            assert!(!allowed(Tool::Adb, &emu(bad)), "{bad}");
        }
    }

    #[test]
    fn serial_avd_and_session_values_have_fixed_shapes() {
        let shell = |serial: &'static str| vec![os("-s"), os(serial), os("shell"), os(IDENTITY)];
        for good in [
            "FAKE0SAMS01",
            "10.0.0.7:5555",
            "emulator-5554",
            "adb-FAKE0SAMS01-Ab3dEf._adb-tls-connect._tcp",
        ] {
            assert!(allowed(Tool::Adb, &shell(good)), "{good}");
        }
        let boot = |avd: &'static str| {
            vec![
                os("-avd"),
                os(avd),
                os("-no-window"),
                os("-no-audio"),
                os("-no-boot-anim"),
                os("-no-snapshot-save"),
            ]
        };
        assert!(allowed(Tool::Emulator, &boot("Pixel_9")));
        assert!(allowed(Tool::Emulator, &boot("Pixel_9.Pro-2")));
        let script = if cfg!(windows) {
            r"C:\ad\agent-device.mjs"
        } else {
            "/ad/agent-device.mjs"
        };
        let close = |session: &'static str| -> Vec<&'static OsStr> {
            vec![
                os(script),
                os("close"),
                os("--session"),
                os(session),
                os("--platform"),
                os("ios"),
                os("--udid"),
                os("00000000-0000-4000-8000-00000000000A"),
                os("--json"),
            ]
        };
        assert!(allowed(Tool::Node, &close("cwd:0000000000000000:default")));
        for bad in [
            "", "-x", "a;b", "a|b", "$(x)", "`x`", "a b", "a'b", "a\"b", "a&b", "a>b", "*",
        ] {
            assert!(!allowed(Tool::Adb, &shell(bad)), "serial {bad:?}");
            assert!(!allowed(Tool::Emulator, &boot(bad)), "avd {bad:?}");
            assert!(!allowed(Tool::Node, &close(bad)), "session {bad:?}");
        }
    }

    /// A relative setting or variable is never used: `Command::new` would search `PATH` for a
    /// bare name (P3bT2-3).
    #[test]
    fn relative_tool_paths_are_never_used() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let def = default_sdk_of(&home);
        touch(&adb_in(&def));
        let settings = ToolSettings {
            adb_path: Some(PathBuf::from("adb")),
            node_path: Some(PathBuf::from("node")),
            ..Default::default()
        };
        let relative = PathBuf::from("sdk");
        let env = env_of(&[("ANDROID_HOME", &relative)]);
        let found = find_tools(&settings, &env, &home);
        assert_eq!(
            found.adb,
            Some(adb_in(&def)),
            "fell through to the default SDK"
        );
        assert!(
            found
                .agent_device
                .as_ref()
                .is_none_or(|c| c.node.is_absolute())
        );
        assert!(
            !is_file(Path::new("Cargo.toml")),
            "a relative file is never a tool"
        );
        let refused = run_tool(&Call {
            tool: Tool::Plutil,
            path: Path::new("plutil"),
            args: &[os("-convert"), os("json"), os("-o"), os("-"), os("-")],
            env: &[],
            stdin: None,
            timeout: Duration::from_secs(1),
            adb_breakaway: false,
            adb_guard: None,
        });
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    /// A launcher that leads to no script never hides a later one (P3bT2-7).
    #[cfg(unix)]
    #[test]
    fn an_unresolvable_launcher_falls_through_to_the_next_place() {
        let t = tempfile::tempdir().unwrap();
        let shim = t.path().join("volta/bin/agent-device");
        touch(&shim);
        let (launcher, script) = npm_global(&t.path().join("prefix"));
        let mut unresolved = None;
        let found = first_resolved(
            [t.path().join("missing"), shim.clone(), launcher.clone()],
            &mut unresolved,
        );
        assert_eq!(found, Some((launcher, script)));
        assert_eq!(unresolved, Some(shim));
        // Volta's own package image resolves.
        let volta = t.path().join(".volta");
        let pkg = volta.join("tools/image/packages/agent-device/lib/node_modules/agent-device");
        touch(&pkg.join("bin/agent-device.mjs"));
        std::fs::write(
            pkg.join("package.json"),
            br#"{"bin":"bin/agent-device.mjs"}"#,
        )
        .unwrap();
        let volta_shim = volta.join("bin/agent-device");
        touch(&volta_shim);
        assert_eq!(
            resolve_agent_device(&volta_shim),
            Some(pkg.join("bin/agent-device.mjs"))
        );
    }

    /// nvm is Unix-only here (`node_manager_places`), and `lts/*` is no Windows file name.
    #[cfg(unix)]
    #[test]
    fn nvm_default_alias_node_and_lts() {
        let t = tempfile::tempdir().unwrap();
        let nvm = t.path().join(".nvm");
        for v in ["v18.20.4", "v20.18.0", "v22.11.0", "v22.9.0"] {
            std::fs::create_dir_all(nvm.join("versions/node").join(v).join("bin")).unwrap();
        }
        let bin = |v: &str| nvm.join("versions/node").join(v).join("bin");
        let alias = |name: &str, value: &str| {
            let p = nvm.join("alias").join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, format!("{value}\n")).unwrap();
        };
        alias("default", "20");
        assert_eq!(nvm_default_bin(&nvm), Some(bin("v20.18.0")));
        alias("default", "22");
        assert_eq!(
            nvm_default_bin(&nvm),
            Some(bin("v22.11.0")),
            "numeric, not lexical"
        );
        alias("default", "v18.20.4");
        assert_eq!(nvm_default_bin(&nvm), Some(bin("v18.20.4")));
        for newest in ["node", "stable"] {
            alias("default", newest);
            assert_eq!(nvm_default_bin(&nvm), Some(bin("v22.11.0")), "{newest}");
        }
        alias("default", "lts/*");
        alias("lts/*", "lts/jod");
        alias("lts/jod", "v22.11.0");
        assert_eq!(nvm_default_bin(&nvm), Some(bin("v22.11.0")));
        alias("default", "lts/iron");
        alias("lts/iron", "v20.18.0");
        assert_eq!(nvm_default_bin(&nvm), Some(bin("v20.18.0")));
        // A loop of aliases ends.
        alias("default", "a");
        alias("a", "default");
        assert_eq!(nvm_default_bin(&nvm), None);
        alias("default", "24");
        assert_eq!(nvm_default_bin(&nvm), None, "not installed");
    }

    /// A recorded agent-device path that stops existing (an `nvm uninstall`) searches again
    /// (L54).
    #[cfg(unix)]
    #[test]
    fn a_vanished_agent_device_setting_searches_again() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let (_, script) = npm_global(&home.join(".local"));
        let node = home.join(".local/bin/node");
        touch(&node);
        let settings = ToolSettings {
            agent_device_path: Some(t.path().join("gone/bin/agent-device")),
            agent_device_script: Some(t.path().join("gone/lib/agent-device.mjs")),
            node_path: Some(t.path().join("gone/bin/node")),
            ..Default::default()
        };
        let found = find_tools(&settings, &|_: &str| None, &home);
        assert_eq!(found.agent_device, Some(AgentDeviceCmd { node, script }));
    }

    /// P3-10: only `devices::probes` makes a guard pass; the test constructor is never used by
    /// the crate itself.
    #[test]
    fn only_probes_make_guard_passes() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension() != Some(OsStr::new("rs")) {
                    continue;
                }
                let mut text = std::fs::read_to_string(&path).unwrap();
                let rel = path.strip_prefix(&src).unwrap();
                let here = rel == Path::new("devices").join("tools.rs");
                if here {
                    // Not this test's own strings.
                    // CRLF checkouts too (galactica).
                    text.truncate(text.find("\nmod tests {").unwrap());
                }
                let probes = rel == Path::new("devices").join("probes.rs");
                let made = text.matches("AdbGuardPass::cleared(").count()
                    + text.matches("AdbGuardPass(())").count();
                if here {
                    assert_eq!(made, 3, "the definition and the two constructors only");
                } else if !probes {
                    assert_eq!(made, 0, "{} makes a guard pass", rel.display());
                }
                if !here {
                    assert!(
                        !text.contains("for_tests()"),
                        "{} uses the test pass",
                        rel.display()
                    );
                }
            }
        }
    }

    #[test]
    fn adb_mismatch_is_detected() {
        let fixture = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/devices/adb-mismatch.txt"
        ))
        .unwrap();
        assert_eq!(
            adb_mismatch(&fixture).as_deref(),
            Some("adb server version (41) doesn't match this client (39)")
        );
        assert_eq!(adb_mismatch(b"* daemon started successfully\n"), None);
        assert_eq!(adb_mismatch(b""), None);
    }

    #[test]
    fn adb_client_version_parses() {
        let out =
            "Android Debug Bridge version 1.0.41\nVersion 36.0.0-13206524\nInstalled as /x/adb\n";
        assert_eq!(parse_adb_version(out), Some(41));
        assert_eq!(
            parse_adb_version("Android Debug Bridge version 1.0.39\r\n"),
            Some(39)
        );
        assert_eq!(parse_adb_version("something else"), None);
        assert_eq!(
            parse_adb_version("Android Debug Bridge version 1.0.\n"),
            None
        );
    }

    #[test]
    fn calls_are_logged_with_scripts_by_name() {
        let args = [os("-s"), os("emulator-5554"), os("shell"), os(SNAPSHOT)];
        assert_eq!(describe(&args), "-s emulator-5554 shell SNAPSHOT");
    }

    #[test]
    fn detect_skips_project_local_installs() {
        let t = tempfile::tempdir().unwrap();
        let local = t.path().join("proj/node_modules/.bin");
        touch(&local.join(exe("node")));
        let global = t.path().join("global");
        touch(&global.join(exe("node")));
        let path = std::env::join_paths([local, global.clone()]).unwrap();
        let set = detect_from_path(&path);
        assert_eq!(
            set.node,
            Some(global.join(exe("node")).to_string_lossy().into_owned())
        );
        assert_eq!(set.adb, None);
    }
}
