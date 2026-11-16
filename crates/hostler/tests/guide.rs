use assert_cmd::Command;
use hostler::contract::wire::Request;
use predicates::prelude::*;
use serde_json::Value;

mod common;

use common::Home;

fn run(args: &[&str]) -> std::process::Output {
    Command::cargo_bin("hostler")
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

const THREAT_MODEL: &str = "hostler protects against accidental exposure and against agents that follow this guide: values never appear in any hostler output, log, error, event, history row, Debug print or panic message, and no command prints a value. It does not protect against a malicious process of your own OS user: such a process can talk to the socket, run `hostler run --secret X -- sh -c 'echo $X | base64'`, or read hostler's memory.";

const FORCE_SENTENCE: &str = "An agent uses --force only with the operator's explicit permission.";

/// The full text of one guide topic.
fn topic_text(name: &str) -> String {
    let output = run(&["guide", name]);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Prose units (paragraphs, list items, table rows and numbered items; fenced code blocks and
/// headings excluded) that mention `--force` without the D9 sentence (T10-8, L40).
fn units_missing_the_sentence(text: &str) -> Vec<String> {
    let mut bad = Vec::new();
    let mut unit = String::new();
    let mut in_code = false;
    let flush = |unit: &mut String, bad: &mut Vec<String>| {
        if unit.contains("--force") && !unit.contains(FORCE_SENTENCE) {
            bad.push(std::mem::take(unit));
        }
        unit.clear();
    };
    let starts_unit = |line: &str| {
        line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with('|')
            || line.split_once('.').is_some_and(|(number, rest)| {
                !number.is_empty()
                    && number.chars().all(|c| c.is_ascii_digit())
                    && rest.starts_with(' ')
            })
    };
    for line in text.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.starts_with("```") {
            flush(&mut unit, &mut bad);
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            flush(&mut unit, &mut bad);
            continue;
        }
        if starts_unit(line) {
            flush(&mut unit, &mut bad);
        }
        if !unit.is_empty() {
            unit.push(' ');
        }
        unit.push_str(line);
    }
    bad
}

#[test]
fn the_sentence_check_is_per_item_and_row() {
    let one_bad = format!("- a --force thing. {FORCE_SENTENCE}\n- another --force thing.");
    assert_eq!(units_missing_the_sentence(&one_bad).len(), 1);
    let rows = format!("| `--force` | x |\n| `--force` | {FORCE_SENTENCE} |");
    assert_eq!(units_missing_the_sentence(&rows).len(), 1);
    let one_good = format!("1. a --force thing. {FORCE_SENTENCE}\n2. plain");
    assert!(units_missing_the_sentence(&one_good).is_empty());
}

#[test]
fn guide_lists_topics() {
    Command::cargo_bin("hostler")
        .unwrap()
        .arg("guide")
        .assert()
        .success()
        .stdout(predicate::str::contains("heavy"));
}

#[test]
fn guide_heavy_mentions_exit_75_and_force_rule() {
    Command::cargo_bin("hostler")
        .unwrap()
        .args(["guide", "heavy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("75"))
        .stdout(predicate::str::contains("explicit permission"))
        .stdout(predicate::str::contains("--max-wait"))
        .stdout(predicate::str::contains("service restart"))
        .stdout(predicate::str::contains(
            "released even if it left background processes behind",
        ))
        .stdout(predicate::str::contains("Agent sandboxes"))
        .stdout(predicate::str::contains("hostler.sock"));
}

#[test]
fn guide_json_lists_topics() {
    let output = run(&["guide", "--json"]);
    assert!(output.status.success(), "{output:?}");

    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert!(value["error"].is_null());

    let topics = value["data"].as_array().unwrap();
    assert!(topics.iter().any(|topic| {
        topic["name"] == "heavy"
            && topic["summary"]
                .as_str()
                .is_some_and(|summary| !summary.is_empty())
    }));
}

#[test]
fn guide_json_prints_a_topic() {
    let output = run(&["guide", "heavy", "--json"]);
    assert!(output.status.success(), "{output:?}");

    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["name"], "heavy");
    let text = value["data"]["text"].as_str().unwrap();
    assert!(text.starts_with("# The heavy queue"));
    assert!(text.contains("75"));
}

#[test]
fn guide_json_unknown_topic_is_a_bad_request() {
    let output = run(&["guide", "missing", "--json"]);
    assert!(!output.status.success());

    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert!(value["data"].is_null());
    assert_eq!(value["error"]["code"], "bad_request");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("heavy")
    );
}

#[test]
fn guide_lists_secrets() {
    let output = run(&["guide"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("secrets"));

    let output = run(&["guide", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let topics = value["data"].as_array().unwrap();
    assert!(topics.iter().any(|topic| {
        topic["name"] == "secrets"
            && topic["summary"]
                .as_str()
                .is_some_and(|summary| !summary.is_empty())
    }));

    let output = run(&["guide", "secrets"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("# Secrets"));
}

#[test]
fn guide_secrets_has_threat_model_and_person_rule() {
    let output = run(&["guide", "secrets", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["name"], "secrets");
    let text = value["data"]["text"].as_str().unwrap();

    for needed in [
        THREAT_MODEL,
        "A person runs this; an agent asks the operator.",
        "secret request",
        "secret requests",
        "secret decline",
        "--secret-file",
        "exit 78",
        "secrets_locked",
        "secret_missing",
        "hostler service restart",
        "KeePassXC",
        "hostler unlock",
        "1 MiB",
        "is not used by this hostler version",
        "every 5 seconds",
        "Stdin is for a person's own pipelines",
        "an agent never pipes a value — it asks with `hostler secret request`",
        "| Code | When | What to do |",
        "HOSTLER_SECRET_RUN=1",
        "<command inside a secret run>",
        "The outer secret run still masks the nested command's stdout and stderr",
        "private file named `secret-NAME`",
        "login session and lock the database again",
    ] {
        assert!(text.contains(needed), "the secrets guide lacks {needed:?}");
    }
    assert!(
        !text.contains("an agent can pipe a value"),
        "the guide still invites an agent to pipe a value"
    );
}

#[test]
fn guide_lists_devices() {
    let output = run(&["guide"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("devices"),
        "the topic list lacks devices"
    );

    let output = run(&["guide", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let topics = value["data"].as_array().unwrap();
    assert!(
        topics.iter().any(|topic| {
            topic["name"] == "devices"
                && topic["summary"]
                    .as_str()
                    .is_some_and(|summary| !summary.is_empty())
        }),
        "the JSON topic list lacks devices"
    );

    let output = run(&["guide", "devices", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["name"], "devices");
    assert!(
        value["data"]["text"]
            .as_str()
            .unwrap()
            .starts_with("# Devices")
    );
}

#[test]
fn guide_devices_has_the_flags_rule_both_shapes_and_force_sentence() {
    let raw = topic_text("devices");
    let text = normalized(&raw);
    for needed in [
        "hostler run --device",
        "hostler hold device",
        "auto:android",
        "auto:ios",
        "auto:android:emulator",
        "--boot",
        "ANDROID_SERIAL",
        "HOSTLER_DEVICE_UDID",
        "--serial \"$ANDROID_SERIAL\"",
        "--udid \"$HOSTLER_DEVICE_UDID\"",
        "agent-device close --session",
        "one session hold a device",
        "--distinct",
        "--confirmation",
        "the device's alias",
        "exits 2",
        FORCE_SENTENCE,
    ] {
        assert!(text.contains(needed), "the devices guide lacks {needed:?}");
    }
    let bad = units_missing_the_sentence(&raw);
    assert!(
        bad.is_empty(),
        "the devices guide mentions --force without the D9 sentence: {bad:?}"
    );
}

#[test]
fn guide_devices_says_a_person_registers_phones() {
    let text = normalized(&topic_text("devices"));
    for needed in [
        "A person",
        "types the model",
        "Agents may add",
        "AVD",
        "simulator",
        "unauthorized",
        "hostler devices add",
        "exit 78",
        "A person runs this; an agent asks the operator.",
    ] {
        assert!(text.contains(needed), "the devices guide lacks {needed:?}");
    }
}

#[test]
fn guide_devices_has_the_forget_sessions_escape() {
    let text = normalized(&topic_text("devices"));
    for needed in [
        "hostler devices forget-sessions",
        "stops nothing",
        "person-only",
        "does not override",
        "nothing to forget: agent-device status reads fine",
        "nothing to forget:",
        "is not closed",
        "devices show",
        "device-claims",
    ] {
        assert!(text.contains(needed), "the devices guide lacks {needed:?}");
    }
}

#[test]
fn readme_has_devices() {
    let readme = normalized(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap(),
    );
    for needed in [
        "## Devices",
        "hostler devices tools --detect",
        "hostler devices list",
        "hostler run --device",
        "hostler hold device",
        "--boot",
        "forget-sessions",
        "ANDROID_SERIAL",
        "HOSTLER_DEVICE_UDID",
    ] {
        assert!(readme.contains(needed), "the README lacks {needed:?}");
    }
}

#[test]
fn guide_lists_holds_screen_accounts() {
    let output = run(&["guide"]);
    assert!(output.status.success(), "{output:?}");
    let listed = String::from_utf8(output.stdout).unwrap();
    for topic in ["holds", "screen", "accounts"] {
        assert!(listed.contains(topic), "the topic list lacks {topic}");
    }

    let output = run(&["guide", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let topics = value["data"].as_array().unwrap();
    for (name, heading) in [
        ("holds", "# Holds and takeover"),
        ("screen", "# The screen"),
        ("accounts", "# Test accounts"),
    ] {
        assert!(
            topics.iter().any(|topic| topic["name"] == name
                && topic["summary"]
                    .as_str()
                    .is_some_and(|summary| !summary.is_empty())),
            "the JSON topic list lacks {name}"
        );
        let output = run(&["guide", name, "--json"]);
        assert!(output.status.success(), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["data"]["name"], name);
        assert!(
            value["data"]["text"].as_str().unwrap().starts_with(heading),
            "the {name} guide does not start with {heading:?}"
        );
    }
}

#[test]
fn guides_contain_the_force_sentence() {
    for topic in ["holds", "screen", "accounts", "heavy", "devices"] {
        let text = topic_text(topic);
        let bad = units_missing_the_sentence(&text);
        assert!(
            bad.is_empty(),
            "the {topic} guide mentions --force without the D9 sentence: {bad:?}"
        );
    }
    // The requirement is not vacuous: the guides document takeover and the queue's force release.
    for topic in ["holds", "screen", "heavy", "devices"] {
        assert!(
            topic_text(topic).contains("--force"),
            "the {topic} guide no longer documents --force"
        );
    }
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let bad = units_missing_the_sentence(&readme);
    assert!(
        bad.is_empty(),
        "the README mentions --force without the D9 sentence: {bad:?}"
    );
    assert!(
        readme.contains("--force"),
        "the README no longer documents --force"
    );
}

/// Phrases that wrap in the source are matched after collapsing whitespace.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The rules the guides state for the final rulings (clean-read fixes).
#[test]
fn guides_state_the_clean_read_rules() {
    let holds = normalized(&topic_text("holds"));
    for needed in [
        "`bad_request`, exit 2",
        "no service at all (fail closed, as for `run`)",
        "a disconnected service is 78",
        "only after the path has been gone for two minutes in a row",
        "is never stranded by its path",
    ] {
        assert!(holds.contains(needed), "the holds guide lacks {needed:?}");
    }
    for gone in ["takes no claim and no heavy slot", "--secret`-only"] {
        assert!(!holds.contains(gone), "the holds guide still says {gone:?}");
    }

    let accounts = normalized(&topic_text("accounts"));
    for needed in [
        "Envelope<{removed}>",
        "Refreshing a session",
        "writes it back through hostler",
        "is not in that run's mask",
    ] {
        assert!(
            accounts.contains(needed),
            "the accounts guide lacks {needed:?}"
        );
    }

    let heavy = normalized(&topic_text("heavy"));
    for needed in [
        "refused while heavy tasks or claim runs",
        "a takeover is in progress",
        "gets a clean environment",
        "HOSTLER_HOME` must be an absolute path",
    ] {
        assert!(heavy.contains(needed), "the heavy guide lacks {needed:?}");
    }
    let readme = normalized(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap(),
    );
    for needed in [
        "heavy tasks or claim runs run",
        "a takeover is in progress",
        "gets a clean environment",
        "must be an absolute path",
    ] {
        assert!(readme.contains(needed), "the README lacks {needed:?}");
    }
}

#[test]
fn status_json_is_the_overview() {
    let home = Home::new();
    let repo = std::fs::canonicalize(home.make_repo("app")).unwrap();
    let output = home
        .hostler()
        .args(["status", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", common::stderr(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true, "{value}");
    assert!(value["error"].is_null(), "{value}");
    let data = &value["data"];
    assert!(
        data["service_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "{value}"
    );
    assert!(data["heavy"]["capacity"].as_u64().is_some(), "{value}");
    assert!(data["heavy"]["running"].is_array(), "{value}");
    assert!(data["heavy"]["waiting"].is_array(), "{value}");
    let resources = data["claims"]["resources"].as_array().unwrap();
    assert!(
        resources
            .iter()
            .any(|state| state["resource"]["display"] == "screen"),
        "{value}"
    );
    assert!(data["secrets"]["state"].is_string(), "{value}");
}

#[test]
fn status_names_holder_paths() {
    let home = Home::new();
    let repo = std::fs::canonicalize(home.make_repo("app")).unwrap();
    let held = home.hold(&repo, &["screen"]);
    assert!(held.status.success(), "{}", common::stderr(&held));
    let workspace = home.workspace(&repo).unwrap();

    let output = home
        .hostler()
        .arg("status")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", common::stderr(&output));
    let text = common::stdout(&output);
    let path = hostler::project::display_path(&repo);
    for needed in [
        "Heavy queue:",
        "Secrets:",
        "This workspace:",
        "screen:",
        "held by",
    ] {
        assert!(text.contains(needed), "status lacks {needed:?}: {text}");
    }
    assert!(text.contains(&path), "status lacks the holder path: {text}");
    assert!(text.contains("hold "), "status lacks the hold id: {text}");
    assert!(
        text.contains(&format!("This workspace: {path} ({})", workspace.id)),
        "status lacks the workspace id: {text}"
    );
}

/// L40/T10-5: `status` is a read. It never registers or re-identifies a checkout, and says so.
#[test]
fn status_never_registers_a_checkout() {
    let home = Home::new();
    let repo = std::fs::canonicalize(home.make_repo("fresh")).unwrap();
    let id_file = repo.join(".git").join("hostler-workspace");
    let workspaces = |home: &Home| -> Vec<Value> {
        serde_json::from_value(home.call(Request::WorkspaceList).unwrap()).unwrap()
    };

    let output = home
        .hostler()
        .args(["status", "--json"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", common::stderr(&output));
    assert!(!id_file.exists(), "status --json wrote the id file");
    assert!(workspaces(&home).is_empty(), "status --json registered");

    let output = home
        .hostler()
        .arg("status")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", common::stderr(&output));
    let text = common::stdout(&output);
    assert!(text.contains("not registered yet"), "{text}");
    assert!(!id_file.exists(), "status wrote the id file");
    assert!(workspaces(&home).is_empty(), "status registered");
}
