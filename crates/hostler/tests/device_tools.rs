//! The bounded device tool runner against the fake tools (P3b Task 2). No real adb, xcrun,
//! ideviceinfo or agent-device ever runs here.

#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use fake_tools::FakeTools;
use hostler::devices::tools::{
    AdbGuardPass, Call, IDENTITY, OUTPUT_CAP, Tool, ToolOutput, adb_client_version,
    ensure_adb_server, find_tools, run_tool, spawn_detached,
};
use hostler::procs::{ProcessTable, SysProcessTable};
use serde_json::json;

fn os(s: &str) -> &OsStr {
    OsStr::new(s)
}

/// The fake adb has no server to ask, so adb calls get the test pass.
fn pass(tool: Tool) -> Option<AdbGuardPass> {
    (tool == Tool::Adb).then(AdbGuardPass::for_tests)
}

fn call<'a>(tool: Tool, path: &'a Path, args: &'a [&'a OsStr]) -> Call<'a> {
    Call {
        tool,
        path,
        args,
        env: &[],
        stdin: None,
        timeout: Duration::from_secs(10),
        adb_breakaway: false,
        adb_guard: pass(tool),
    }
}

fn run(tools: &FakeTools, tool: Tool, args: &[&OsStr], timeout: Duration) -> ToolOutput {
    let path = tools.bin(tool);
    run_tool(&Call {
        timeout,
        ..call(tool, &path, args)
    })
    .unwrap()
}

const BOOT: [&str; 6] = [
    "-avd",
    "Pixel_9",
    "-no-window",
    "-no-audio",
    "-no-boot-anim",
    "-no-snapshot-save",
];

fn alive(pid: u32) -> bool {
    let table = SysProcessTable::new().unwrap();
    table.info(pid).is_some_and(|i| !i.zombie)
}

/// Waits up to `bound` for `f`.
fn eventually(bound: Duration, f: impl Fn() -> bool) -> bool {
    let until = Instant::now() + bound;
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

#[test]
fn run_tool_returns_output_and_status() {
    let tools = FakeTools::new();
    let listing = tools.fixture("adb-devices-l.txt");
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["devices", "-l"], "stdout_file": listing},
        {"tool": "adb", "args_prefix": ["-s", "emulator-5554", "shell"],
         "stdout": "emulator-5554\nsdk_gphone64_arm64\n16\n36\n\n1\n\n",
         "stderr": "bad\n", "rc": 3},
    ]));
    let out = run(
        &tools,
        Tool::Adb,
        &[os("devices"), os("-l")],
        Duration::from_secs(10),
    );
    let expected = std::fs::read(tools.dir().join(&listing)).unwrap();
    assert_eq!(out.stdout, expected);
    assert_eq!(out.status, Some(0));
    assert!(!out.timed_out && !out.truncated);
    let args = [os("-s"), os("emulator-5554"), os("shell"), os(IDENTITY)];
    let out = run(&tools, Tool::Adb, &args, Duration::from_secs(10));
    assert_eq!(
        (out.status, out.stdout, out.stderr),
        // Never trimmed: empty `getprop` values keep their lines (Task 3 reads them by position).
        (
            Some(3),
            b"emulator-5554\nsdk_gphone64_arm64\n16\n36\n\n1\n\n".to_vec(),
            b"bad\n".to_vec()
        )
    );
    let calls = tools.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].args[3], IDENTITY);
}

#[test]
fn run_tool_refuses_calls_outside_the_allowlist() {
    let tools = FakeTools::new();
    let refuse = |c: Call| {
        let err = run_tool(&c).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    };
    let adb = tools.bin(Tool::Adb);
    let unplug = [os("-s"), os("X"), os("shell"), os("dumpsys battery unplug")];
    refuse(call(Tool::Adb, &adb, &unplug));
    refuse(call(Tool::Adb, &adb, &[os("reboot")]));
    // Every adb call but `version` needs the guard's pass (P3-10).
    let devices = [os("devices"), os("-l")];
    refuse(Call {
        adb_guard: None,
        ..call(Tool::Adb, &adb, &devices)
    });
    // Only ensure_adb_server starts a server, with null stdio (P3bT2-8).
    refuse(call(Tool::Adb, &adb, &[os("start-server")]));
    let xcrun = tools.bin(Tool::Xcrun);
    refuse(call(
        Tool::Xcrun,
        &xcrun,
        &[os("simctl"), os("erase"), os("X")],
    ));
    // `simctl shutdown all` would stop every simulator, other agents' too (P3bT2-1).
    refuse(call(
        Tool::Xcrun,
        &xcrun,
        &[os("simctl"), os("shutdown"), os("all")],
    ));
    assert_eq!(tools.calls(), vec![], "nothing was spawned");
}

/// A bare name would be searched on `PATH` by the OS (P3bT2-3).
#[test]
fn relative_tool_paths_are_never_run() {
    let tools = FakeTools::new();
    let devices = [os("devices"), os("-l")];
    let err = run_tool(&call(Tool::Adb, Path::new("adb"), &devices)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(ensure_adb_server(Path::new("adb"), &[]).is_err());
    let boot: Vec<&OsStr> = BOOT.iter().map(|a| os(a)).collect();
    let log = tools.dir().join("boot.log");
    assert!(spawn_detached(Path::new("emulator"), &boot, &[], &log).is_err());
    assert_eq!(tools.calls(), vec![]);
}

/// Per-tool environment names (P3bT2-6): `NODE_OPTIONS` never reaches node, `ANDROID_SERIAL`
/// never reaches adb.
#[test]
fn run_tool_refuses_unknown_env_names() {
    let tools = FakeTools::new();
    tools.script(json!([{"tool": "node", "args_prefix": [], "stdout": "0.21.36\n"}]));
    let node = tools.bin(Tool::Node);
    let script = tools.agent_device_script();
    let version = [script.as_os_str(), os("--version")];
    let evil = [("NODE_OPTIONS", os("--require /tmp/x.js"))];
    let err = run_tool(&Call {
        env: &evil,
        ..call(Tool::Node, &node, &version)
    })
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let adb = tools.bin(Tool::Adb);
    let devices = [os("devices"), os("-l")];
    let serial = [("ANDROID_SERIAL", os("FAKE0SAMS01"))];
    assert!(
        run_tool(&Call {
            env: &serial,
            ..call(Tool::Adb, &adb, &devices)
        })
        .is_err()
    );
    assert_eq!(tools.calls(), vec![]);
    let state = [("AGENT_DEVICE_STATE_DIR", os("/tmp/ad-state"))];
    let out = run_tool(&Call {
        env: &state,
        ..call(Tool::Node, &node, &version)
    })
    .unwrap();
    assert_eq!(out.stdout, b"0.21.36\n");
    // The fake also sees what the machine itself sets (galactica has ANDROID_HOME).
    let env = &tools.calls()[0].env;
    assert!(env.iter().any(|n| n == "AGENT_DEVICE_STATE_DIR"), "{env:?}");
    assert!(!env.iter().any(|n| n == "NODE_OPTIONS"), "{env:?}");
}

#[test]
fn run_tool_writes_stdin() {
    let tools = FakeTools::new();
    let listapps = tools.fixture("simctl-listapps.json");
    tools.script(json!([
        {"tool": "plutil", "args_prefix": ["-convert", "json"], "stdout_file": listapps},
    ]));
    let plist = b"{ \"com.example.app\" = { CFBundleIdentifier = \"com.example.app\"; }; }";
    let plutil = tools.bin(Tool::Plutil);
    let args = [os("-convert"), os("json"), os("-o"), os("-"), os("-")];
    let out = run_tool(&Call {
        stdin: Some(plist),
        ..call(Tool::Plutil, &plutil, &args)
    })
    .unwrap();
    assert_eq!(out.status, Some(0));
    assert!(!out.stdout.is_empty());
    assert_eq!(tools.calls()[0].stdin, plist.len());
}

#[test]
fn run_tool_kills_at_the_bound() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "xcrun", "args_prefix": ["simctl"], "delay_ms": 30000, "stdout": "late"},
    ]));
    let started = Instant::now();
    let out = run(
        &tools,
        Tool::Xcrun,
        &[os("simctl"), os("list"), os("devices"), os("--json")],
        Duration::from_millis(300),
    );
    let took = started.elapsed();
    assert!(out.timed_out);
    assert!(out.stdout.is_empty());
    assert!(took < Duration::from_secs(3), "took {took:?}");
}

/// The tool exits at once, but a grandchild keeps its stdout open for 20 s: the call returns
/// when the tool exits, and the grandchild goes with the tool's group or job.
#[test]
fn child_that_keeps_the_pipe_open_does_not_hang() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["devices"], "stdout": "List of devices attached\n",
         "spawn_holding_child": 20000},
    ]));
    let started = Instant::now();
    let out = run(
        &tools,
        Tool::Adb,
        &[os("devices"), os("-l")],
        Duration::from_secs(15),
    );
    let took = started.elapsed();
    assert!(!out.timed_out);
    assert_eq!(out.status, Some(0));
    assert_eq!(out.stdout, b"List of devices attached\n");
    assert!(took < Duration::from_secs(3), "took {took:?}");
    let held = tools.held_pids();
    assert_eq!(held.len(), 1);
    assert!(
        eventually(Duration::from_secs(5), || !alive(held[0])),
        "the grandchild outlived the call"
    );
}

/// A tool that hangs while its grandchild holds the pipe: both end at the bound.
#[test]
fn a_hung_tool_and_its_grandchild_end_at_the_bound() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "node", "args_prefix": [], "spawn_holding_child": 20000, "delay_ms": 20000},
    ]));
    let script = tools.agent_device_script();
    let args = [script.as_os_str(), os("device"), os("status"), os("--json")];
    let started = Instant::now();
    // Long enough for the fake to start its holder on a loaded machine.
    let out = run(&tools, Tool::Node, &args, Duration::from_secs(3));
    assert!(out.timed_out);
    assert!(started.elapsed() < Duration::from_secs(6));
    let held = tools.held_pids();
    assert_eq!(held.len(), 1);
    assert!(
        eventually(Duration::from_secs(5), || !alive(held[0])),
        "the grandchild outlived the bound"
    );
}

#[test]
fn truncated_output_is_marked() {
    assert_eq!(OUTPUT_CAP, 16 << 20, "the plan's 16 MiB per stream");
    let tools = FakeTools::new();
    let big = tools.file("big.txt", vec![b'.'; OUTPUT_CAP + 4096]);
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["-s"], "stdout_file": big},
    ]));
    let args = [
        os("-s"),
        os("FAKE0SAMS01"),
        os("shell"),
        os(hostler::devices::tools::SNAPSHOT),
    ];
    let out = run(&tools, Tool::Adb, &args, Duration::from_secs(30));
    assert!(out.truncated);
    assert_eq!(out.stdout.len(), OUTPUT_CAP);
    assert_eq!(
        out.status,
        Some(0),
        "the rest was drained, so the tool finished"
    );
}

/// `start-server` runs with null stdio: a "server" it leaves behind holds no pipe of ours, the
/// call returns when the client exits, and the server survives.
#[test]
fn ensure_adb_server_uses_null_stdio() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["start-server"], "spawn_holding_child": 4000},
    ]));
    let started = Instant::now();
    ensure_adb_server(&tools.bin(Tool::Adb), &[]).unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    let calls = tools.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, ["start-server"]);
    assert_eq!(calls[0].stdout, "null");
    let held = tools.held_pids();
    assert!(alive(held[0]), "the server it started is still running");
}

#[test]
fn ensure_adb_server_fails_on_a_failing_client() {
    let tools = FakeTools::new();
    tools.script(json!([{"tool": "adb", "args_prefix": ["start-server"], "rc": 1}]));
    assert!(ensure_adb_server(&tools.bin(Tool::Adb), &[]).is_err());
}

/// The recorded `devices.env.*` reach the two long-lived spawns (P3bT2-5); the fake logs only
/// the names it sees, never values.
#[test]
fn start_server_and_boot_get_the_recorded_env() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["start-server"]},
        {"tool": "emulator", "args_prefix": ["-avd"]},
    ]));
    let port = [("ANDROID_ADB_SERVER_PORT", os("5038"))];
    ensure_adb_server(&tools.bin(Tool::Adb), &port).unwrap();
    let avds = [("ANDROID_AVD_HOME", os("/tmp/avd"))];
    let boot: Vec<&OsStr> = BOOT.iter().map(|a| os(a)).collect();
    let log = tools.dir().join("boot.log");
    spawn_detached(&tools.bin(Tool::Emulator), &boot, &avds, &log).unwrap();
    assert!(eventually(Duration::from_secs(10), || tools.calls().len() == 2));
    let calls = tools.calls();
    let start = calls.iter().find(|c| c.tool == "adb").unwrap();
    assert!(
        start.env.iter().any(|n| n == "ANDROID_ADB_SERVER_PORT"),
        "{start:?}"
    );
    let emulator = calls.iter().find(|c| c.tool == "emulator").unwrap();
    assert!(
        emulator.env.iter().any(|n| n == "ANDROID_AVD_HOME"),
        "{emulator:?}"
    );
    // A name the tool may not get is refused before anything starts.
    let serial = [("ANDROID_SERIAL", os("X"))];
    assert!(ensure_adb_server(&tools.bin(Tool::Adb), &serial).is_err());
}

/// The client version is read again when the binary at the same path changes (P3bT2-4).
#[test]
fn adb_client_version_follows_a_replaced_binary() {
    let tools = FakeTools::new();
    let adb = tools.bin(Tool::Adb);
    let answer = |v: u32| {
        tools.script(json!([{"tool": "adb", "args_prefix": ["version"],
            "stdout": format!("Android Debug Bridge version 1.0.{v}\nVersion 36.0.0\n")}]));
    };
    answer(41);
    assert_eq!(adb_client_version(&adb), Some(41));
    answer(42);
    assert_eq!(adb_client_version(&adb), Some(41), "read once per binary");
    // Replaced the way an SDK update does it: a new file at the same path. Touching the mtime
    // of the signed binary instead makes macOS refuse to run it.
    let before = std::fs::metadata(&adb).unwrap().modified().unwrap();
    let bytes = std::fs::read(fake_tools::fake_tool_exe()).unwrap();
    loop {
        std::fs::remove_file(&adb).unwrap();
        std::fs::write(&adb, &bytes).unwrap();
        if std::fs::metadata(&adb).unwrap().modified().unwrap() != before {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(adb_client_version(&adb), Some(42));
    assert_eq!(tools.calls().len(), 2);
}

/// Unix `adb_breakaway`: at the bound only the client dies; a server it started in its own
/// session lives on (P3bT2-8).
#[cfg(unix)]
#[test]
fn adb_breakaway_unix_keeps_the_server_and_kills_only_the_client_at_the_bound() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["devices"], "spawn_holding_child": 8000,
         "holder_new_session": true, "delay_ms": 30000},
    ]));
    let adb = tools.bin(Tool::Adb);
    let devices = [os("devices"), os("-l")];
    let started = Instant::now();
    let out = run_tool(&Call {
        timeout: Duration::from_secs(3),
        adb_breakaway: true,
        ..call(Tool::Adb, &adb, &devices)
    })
    .unwrap();
    assert!(out.timed_out);
    assert!(started.elapsed() < Duration::from_secs(6));
    let held = tools.held_pids();
    assert_eq!(held.len(), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(held[0]), "the server died with the client");
}

/// Windows: the client's own server breaks away from the call's kill-on-close job (P-7).
#[cfg(windows)]
#[test]
fn adb_server_started_by_a_client_survives_the_job() {
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "adb", "args_prefix": ["devices"], "stdout": "List of devices attached\n",
         "spawn_holding_child": 6000},
    ]));
    let adb = tools.bin(Tool::Adb);
    let devices = [os("devices"), os("-l")];
    let out = run_tool(&Call {
        adb_breakaway: true,
        ..call(Tool::Adb, &adb, &devices)
    })
    .unwrap();
    assert_eq!(out.status, Some(0));
    let held = tools.held_pids();
    std::thread::sleep(Duration::from_millis(500));
    assert!(alive(held[0]), "the server died with the job");
}

#[cfg(unix)]
#[test]
fn detached_spawn_outlives_its_parent() {
    let tools = FakeTools::new();
    tools.script(json!([{"tool": "emulator", "args_prefix": ["-avd"], "delay_ms": 3000}]));
    let log = tools.dir().join("boot.log");
    let args: Vec<&OsStr> = BOOT.iter().map(|a| os(a)).collect();
    let (pid, start) = spawn_detached(&tools.bin(Tool::Emulator), &args, &[], &log).unwrap();
    assert!(start > 0);
    // A new session and its own group: no group kill or hangup of the service reaches it.
    // SAFETY: plain queries of a pid.
    let (sid, pgid) = unsafe { (libc::getsid(pid as i32), libc::getpgid(pid as i32)) };
    assert_eq!((sid, pgid), (pid as i32, pid as i32));
    let table = SysProcessTable::new().unwrap();
    assert_eq!(table.info(pid).and_then(|i| i.start_time), Some(start));
    // Refused for anything but the emulator's boot vector.
    let refused = spawn_detached(&tools.bin(Tool::Emulator), &[os("-list-avds")], &[], &log);
    assert!(refused.is_err());
    assert!(eventually(Duration::from_secs(10), || !alive(pid)));
    assert_eq!(tools.calls()[0].stdout, "file");
}

/// Windows: runs one `#[ignore]`d child test alone in a fresh test process, so the jobs it joins
/// never reach the other tests.
#[cfg(windows)]
fn in_child_process(test: &str) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            test,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && text.contains("1 passed"),
        "{test}:\n{text}"
    );
}

#[cfg(windows)]
const CHILD_ENV: &str = "DEVICE_TOOLS_JOB_CHILD";

/// Puts this (child) test process into a new job with `limits`.
#[cfg(windows)]
fn join_new_job(limits: u32) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    assert!(
        std::env::var_os(CHILD_ENV).is_some(),
        "only in a child test process"
    );
    // SAFETY: an unnamed job with the given limits; this process joins it.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        assert!(!job.is_null());
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = limits;
        assert_ne!(
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ),
            0
        );
        assert_ne!(AssignProcessToJobObject(job, GetCurrentProcess()), 0);
        job
    }
}

/// Whether `pid` is in `job`.
#[cfg(windows)]
fn in_job(pid: u32, job: windows_sys::Win32::Foundation::HANDLE) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: a query handle on `pid`, closed below.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        assert!(!process.is_null(), "pid {pid} is gone");
        let mut inside = 0;
        assert_ne!(IsProcessInJob(process, job, &mut inside), 0);
        CloseHandle(process);
        inside != 0
    }
}

/// Windows: out of the caller's job (breakaway), so a kill-on-close job of the service never
/// ends an emulator hostler booted.
#[cfg(windows)]
#[test]
fn detached_spawn_outlives_its_parent() {
    in_child_process("child_detached_spawn_leaves_a_breakaway_job");
}

#[cfg(windows)]
#[test]
#[ignore = "run in its own process by detached_spawn_outlives_its_parent"]
fn child_detached_spawn_leaves_a_breakaway_job() {
    use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    let tools = FakeTools::new();
    tools.script(json!([{"tool": "emulator", "args_prefix": ["-avd"], "delay_ms": 3000}]));
    let job = join_new_job(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
    let log = tools.dir().join("boot.log");
    let args: Vec<&OsStr> = BOOT.iter().map(|a| os(a)).collect();
    let (pid, start) = spawn_detached(&tools.bin(Tool::Emulator), &args, &[], &log).unwrap();
    assert!(start > 0);
    assert!(
        !in_job(pid, job),
        "the detached process stayed in the caller's job"
    );
    assert!(eventually(Duration::from_secs(10), || !alive(pid)));
}

/// Windows, the service inside a kill-on-close job that forbids breakaway (P3bT2-2): bounds still
/// hold, a client's server still leaves the call's own job, and `spawn_detached` falls back to a
/// plain spawn that still reports pid and start time.
#[cfg(windows)]
#[test]
fn run_tool_inside_a_job_that_forbids_breakaway() {
    in_child_process("child_inside_a_job_that_forbids_breakaway");
}

#[cfg(windows)]
#[test]
#[ignore = "run in its own process by run_tool_inside_a_job_that_forbids_breakaway"]
fn child_inside_a_job_that_forbids_breakaway() {
    use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let tools = FakeTools::new();
    tools.script(json!([
        {"tool": "xcrun", "args_prefix": ["simctl"], "delay_ms": 30000},
        {"tool": "adb", "args_prefix": ["devices"], "stdout": "List of devices attached\n",
         "spawn_holding_child": 6000},
        {"tool": "emulator", "args_prefix": ["-avd"], "delay_ms": 3000},
    ]));
    let job = join_new_job(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
    let started = Instant::now();
    let list = [os("simctl"), os("list"), os("devices"), os("--json")];
    let out = run(&tools, Tool::Xcrun, &list, Duration::from_secs(2));
    assert!(out.timed_out);
    assert!(started.elapsed() < Duration::from_secs(6));
    let adb = tools.bin(Tool::Adb);
    let devices = [os("devices"), os("-l")];
    let out = run_tool(&Call {
        adb_breakaway: true,
        ..call(Tool::Adb, &adb, &devices)
    })
    .unwrap();
    assert_eq!(out.status, Some(0));
    let held = tools.held_pids();
    std::thread::sleep(Duration::from_millis(500));
    assert!(alive(held[0]), "the server died with the call's job");
    let log = tools.dir().join("boot.log");
    let args: Vec<&OsStr> = BOOT.iter().map(|a| os(a)).collect();
    let (pid, start) = spawn_detached(&tools.bin(Tool::Emulator), &args, &[], &log).unwrap();
    assert!(start > 0);
    assert!(
        in_job(pid, job),
        "breakaway is forbidden, so it stays in the job"
    );
}

#[test]
fn fake_tool_logs_and_scripts() {
    let tools = FakeTools::new();
    let status = tools.fixture("agent-device-status-0.21.36.json");
    let empty = tools.file(
        "empty-status.json",
        r#"{"success":true,"data":{"claims":[]}}"#,
    );
    tools.script(json!([
        {"tool": "node", "args_prefix": [tools.agent_device_script(), "close"],
         "stdout": "{\"success\":true}",
         "then_write": [
             {"tool": "node", "args_prefix": [tools.agent_device_script(), "device"],
              "stdout_file": empty}
         ]},
        {"tool": "node", "args_prefix": [tools.agent_device_script(), "device"],
         "stdout_file": status},
        {"tool": "node", "args_prefix": [tools.agent_device_script(), "--version"],
         "stdout": "0.21.36\n", "once": true},
    ]));
    let script = tools.agent_device_script();
    let s = script.as_os_str();
    let status_args = [s, os("device"), os("status"), os("--json")];
    let before = run(&tools, Tool::Node, &status_args, Duration::from_secs(10));
    assert!(String::from_utf8_lossy(&before.stdout).contains("p3b-envtest"));
    let version = [s, os("--version")];
    assert_eq!(
        run(&tools, Tool::Node, &version, Duration::from_secs(10)).stdout,
        b"0.21.36\n"
    );
    // `once`: the entry is gone, so the next call has no answer.
    assert_eq!(
        run(&tools, Tool::Node, &version, Duration::from_secs(10)).status,
        Some(1)
    );
    let close = [
        s,
        os("close"),
        os("--session"),
        os("p3b-envtest"),
        os("--platform"),
        os("android"),
        os("--serial"),
        os("FAKE0TECNO01"),
        os("--json"),
    ];
    run(&tools, Tool::Node, &close, Duration::from_secs(10));
    let after = run(&tools, Tool::Node, &status_args, Duration::from_secs(10));
    assert!(!String::from_utf8_lossy(&after.stdout).contains("p3b-envtest"));
    let calls = tools.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].tool, "node");
    assert_eq!(calls[0].args[1..], ["device", "status", "--json"]);
    assert_eq!(calls[0].stdout, "pipe");
}

/// With `HOSTLER_TEST_FAKE_TOOLS`, every tool resolves to the fakes (debug builds).
#[test]
fn fake_tools_env_points_every_tool_at_the_fakes() {
    let tools = FakeTools::new();
    let env = tools.env();
    let lookup = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let found = find_tools(&Default::default(), &lookup, Path::new("/nonexistent"));
    assert_eq!(found.adb, Some(tools.bin(Tool::Adb)));
    assert_eq!(found.xcrun, Some(tools.bin(Tool::Xcrun)));
    let cmd = found.agent_device.unwrap();
    assert_eq!(cmd.node, tools.bin(Tool::Node));
    assert_eq!(cmd.script, tools.agent_device_script());
}

/// The suite-wide check also refuses node running anything but the agent-device script
/// (P3bT2-11).
#[test]
fn fake_tools_check_refuses_another_node_script() {
    let tools = FakeTools::new();
    tools.script(json!([{"tool": "node", "args_prefix": [], "stdout": "x"}]));
    let node = tools.bin(Tool::Node);
    let other = tools.dir().join("elsewhere.mjs");
    let args = [other.as_os_str(), os("--version")];
    run_tool(&call(Tool::Node, &node, &args)).unwrap();
    let seen = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tools.calls()));
    assert!(seen.is_err(), "calls() let it through");
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(tools)));
    assert!(dropped.is_err(), "the drop check let it through");
}
