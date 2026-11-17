//! `hostler.toml`'s version-1 schema through the CLI (plan P4 Task 2): the full config loads,
//! a bad one reports line-numbered problems that never quote a value, and `devices installs`
//! marks this project's apps.

mod common;
#[path = "common/fake_tools.rs"]
mod fake_tools;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{Home, stderr, stdout};
use fake_tools::FakeTools;
use hostler::contract::claims::ResourceId;
use hostler::contract::devices::{
    AppState, InstallChange, InstallEvent, InstallSource, InstallTime,
};
use hostler::store::Store;
use serde_json::{Value, json};

/// The plan's Task 2 example, as a project would commit it.
const FULL: &str = "\
[project]
name = \"muse\"

[apps]
android = [\"com.example.muse\"]
ios = [\"com.example.app\"]

[secrets.JACKETT_API_KEY]
purpose = \"Jackett API key for the torrent search\"
env = \"JACKETT_API_KEY\"

[accounts.qa-user]
purpose = \"the main e2e test user\"
fields = [\"env\", \"session\"]

[heavy]
commands = [\"./gradlew\", \"pod install\"]

[agents]
docs = [\"AGENTS.md\"]
device_rules = \"AGENTS.md#device-handling\"

[ranges]
";

/// A value no output may ever hold.
const MARKER: &str = "hostler-marker-7f3a";

fn repo_with(home: &Home, name: &str, toml: &str) -> PathBuf {
    let repo = home.make_repo(name);
    fs::write(repo.join("hostler.toml"), toml).unwrap();
    repo
}

fn run(home: &Home, dir: &Path, args: &[&str]) -> Output {
    home.hostler().args(args).current_dir(dir).output().unwrap()
}

fn envelope(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not an envelope: {e}\nstdout: {}\nstderr: {}",
            stdout(out),
            stderr(out)
        )
    })
}

#[test]
fn full_schema_through_the_cli() {
    let home = Home::new();
    let repo = repo_with(&home, "muse", FULL);

    let out = run(&home, &repo, &["project", "--json"]);
    assert!(out.status.success(), "project: {}", stderr(&out));
    let value = envelope(&out);
    assert_eq!(value["ok"], json!(true), "{value}");
    assert_eq!(value["data"]["name"], json!("muse"), "{value}");
    // `[ranges]` is accepted, and its note is printed once.
    let err = stderr(&out);
    assert_eq!(
        err.matches("[ranges] is reserved for a later hostler version")
            .count(),
        1,
        "{err}"
    );

    // The human output names the project too.
    let out = run(&home, &repo, &["project"]);
    assert!(out.status.success(), "project: {}", stderr(&out));
    assert!(stdout(&out).starts_with("muse\n"), "{}", stdout(&out));
}

#[test]
fn invalid_config_is_never_a_value() {
    let home = Home::new();
    let toml = format!(
        "[project]\n\
         name = \"{marker}\"\n\
         \n\
         [apps]\n\
         android = [\"{marker}\"]\n\
         windows = [\"{marker}\"]\n\
         \n\
         [secrets.{marker}]\n\
         purpose = \"{marker}\"\n\
         \n\
         [accounts.qa]\n\
         purpose = \"{marker}\"\n\
         fields = [\"{marker}\"]\n\
         \n\
         [heavy]\n\
         commands = [\"{command}\"]\n\
         \n\
         [agents]\n\
         docs = [\"/{marker}\"]\n\
         \n\
         [dev]\n\
         port = 3000\n",
        marker = MARKER,
        command = "c".repeat(201),
    );
    let repo = repo_with(&home, "bad", &toml);

    let out = run(&home, &repo, &["project", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let value = envelope(&out);
    assert_eq!(value["ok"], json!(false), "{value}");
    assert_eq!(value["error"]["code"], json!("config_invalid"), "{value}");

    let lines: Vec<i64> = value["error"]["details"]["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .map(|problem| problem["line"].as_i64().expect("a line"))
        .collect();
    // One problem per table, each on the line it is written on; the last is `[dev]`.
    assert_eq!(lines, vec![5, 6, 8, 13, 16, 19, 21], "{:?}", value["error"]);

    // Neither the JSON nor the human error may hold the marker.
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!all.contains(MARKER), "{all}");
    let out = run(&home, &repo, &["project"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!all.contains(MARKER), "{all}");
    assert!(all.contains("hostler.toml has problems"), "{all}");
}

const ANDROID_DEVICE: &str = "device:android:SERIAL0001";
const IOS_DEVICE: &str = "device:ios:00000000-0000-4000-8000-0000000000A1";

/// Installs from before the run: this project's Android app, another app, and one id installed
/// on both platforms (the checkout declares it for iOS only).
fn seed_installs(home: &Home) {
    let store = Store::open(&home.paths().db).unwrap();
    let event = |id: &str, app: &str, device: &str, alias: &str, at_ms: i64| InstallEvent {
        id: id.into(),
        device: ResourceId(device.into()),
        alias: Some(alias.into()),
        app_id: app.into(),
        change: InstallChange::Installed,
        version_name: Some("1.2.0".into()),
        version_code: Some("120".into()),
        when: InstallTime::Device { at_ms },
        source: InstallSource::NotThroughHostler,
        recorded_at_ms: at_ms,
    };
    // Recent enough that the service's startup prune keeps them.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let events = [
        event(
            "one",
            "com.example.new",
            ANDROID_DEVICE,
            "phone",
            now - 1_000,
        ),
        event(
            "two",
            "com.example.old",
            ANDROID_DEVICE,
            "phone",
            now - 2_000,
        ),
        event(
            "three",
            "com.example.shared",
            ANDROID_DEVICE,
            "phone",
            now - 3_000,
        ),
        event("four", "com.example.shared", IOS_DEVICE, "sim", now - 4_000),
    ];
    // One snapshot per device writes them all (the events are not tied to a snapshot).
    store
        .put_device_snapshot(
            &ResourceId(ANDROID_DEVICE.into()),
            &[] as &[AppState],
            now,
            None,
            &events,
        )
        .unwrap();
}

#[test]
fn installs_label_this_project() {
    // Before the home it serves (its drop check runs last).
    let tools = FakeTools::new();
    let home = Home::new();
    seed_installs(&home);
    let start = home
        .hostler()
        .envs(tools.env())
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(start.status.success(), "service start: {}", stderr(&start));

    // `com.example.shared` is declared for iOS only: an Android install of it must not be
    // marked, an iOS one must.
    let repo = repo_with(
        &home,
        "ours",
        "[apps]\nandroid = [\"com.example.new\"]\nios = [\"com.example.shared\"]\n",
    );

    // `--json`: only this project's app carries the flag.
    let out = home
        .hostler()
        .envs(tools.env())
        .args(["devices", "installs", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "devices installs: {}", stderr(&out));
    let value = envelope(&out);
    let events = value["data"].as_array().expect("events");
    assert_eq!(events.len(), 4, "{value}");
    let find = |app: &str, device: &str| {
        events
            .iter()
            .find(|event| event["app_id"] == json!(app) && event["device"] == json!(device))
            .unwrap_or_else(|| panic!("no install of {app} on {device}: {value}"))
    };
    // The Android app this project declares.
    assert_eq!(
        find("com.example.new", ANDROID_DEVICE)["this_project"],
        json!(true),
        "{value}"
    );
    // Another app.
    assert!(
        find("com.example.old", ANDROID_DEVICE)
            .get("this_project")
            .is_none(),
        "{value}"
    );
    // The same id on both platforms: only the iOS install is this project's.
    assert!(
        find("com.example.shared", ANDROID_DEVICE)
            .get("this_project")
            .is_none(),
        "{value}"
    );
    assert_eq!(
        find("com.example.shared", IOS_DEVICE)["this_project"],
        json!(true),
        "{value}"
    );

    // The human line marks it after the id, and only there.
    let out = home
        .hostler()
        .envs(tools.env())
        .args(["devices", "installs"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "devices installs: {}", stderr(&out));
    let text = stdout(&out);
    let line_of = |app: &str, device: &str| {
        text.lines()
            .find(|line| line.contains(app) && line.contains(device))
            .unwrap_or_else(|| panic!("no line for {app} on {device}: {text}"))
            .to_string()
    };
    // An unregistered device's line names its resource id (`device:<platform>:…`).
    assert!(
        line_of("com.example.new", "SERIAL0001").contains("com.example.new (this project)"),
        "{text}"
    );
    assert!(
        !line_of("com.example.old", "SERIAL0001").contains("(this project)"),
        "{text}"
    );
    // The platform decides: the same id is marked on the simulator, not on the phone.
    assert!(
        !line_of("com.example.shared", "SERIAL0001").contains("(this project)"),
        "{text}"
    );
    assert!(
        line_of("com.example.shared", "device:ios:").contains("com.example.shared (this project)"),
        "{text}"
    );

    // Outside a checkout nothing is marked and no key appears.
    let outside = home.dir.path();
    let out = home
        .hostler()
        .envs(tools.env())
        .args(["devices", "installs", "--json"])
        .current_dir(outside)
        .output()
        .unwrap();
    assert!(out.status.success(), "devices installs: {}", stderr(&out));
    let value = envelope(&out);
    assert!(
        value.to_string().find("this_project").is_none(),
        "nothing is marked outside a checkout: {value}"
    );
}
