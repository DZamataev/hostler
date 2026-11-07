use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn run(args: &[&str]) -> std::process::Output {
    Command::cargo_bin("hostler")
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

const THREAT_MODEL: &str = "hostler protects against accidental exposure and against agents that follow this guide: values never appear in any hostler output, log, error, event, history row, Debug print or panic message, and no command prints a value. It does not protect against a malicious process of your own OS user: such a process can talk to the socket, run `hostler run --secret X -- sh -c 'echo $X | base64'`, or read hostler's memory.";

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
