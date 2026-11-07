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
