//! P5-L16: every tooltip of the window names a command the CLI really parses.
//!
//! One fixture lists the window's `CommandHelp` texts: `macos/Tests/HostlerKitTests/Features/
//! CommandHelpTests.swift` checks the Swift table against it, and this test parses each entry's
//! argv with the real clap definitions. A template and the parser therefore cannot drift apart
//! unnoticed.

use clap::Parser;

/// The fixture, as committed: `crates/hostler/tests/fixtures/command-help.json`.
fn fixture() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/command-help.json"
    );
    let text = std::fs::read_to_string(path).expect("the command-help fixture is readable");
    serde_json::from_str(&text).expect("the command-help fixture is valid JSON")
}

fn text_of(entry: &serde_json::Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("every entry has a string {key}"))
        .to_owned()
}

#[test]
fn every_command_help_text_is_a_command_the_cli_parses() {
    let fixture = fixture();
    let entries = fixture.as_array().expect("the fixture is an array");
    assert!(!entries.is_empty(), "the fixture names at least one action");
    for entry in entries {
        let action = text_of(entry, "action");
        let command = text_of(entry, "command");
        assert!(
            command.starts_with("hostler "),
            "{action}: a tooltip names a hostler command, got {command:?}"
        );
        let argv: Vec<String> = entry
            .get("argv")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{action}: an argv list"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("{action}: argv entries are strings"))
                    .to_owned()
            })
            .collect();
        assert_eq!(
            argv.first().map(String::as_str),
            Some("hostler"),
            "{action}: argv starts with the binary"
        );
        hostler::cli::Cli::try_parse_from(&argv).unwrap_or_else(|error| {
            panic!("{action}: the CLI rejects {command:?} ({argv:?}): {error}")
        });
    }
}

#[test]
fn the_fixture_names_every_action_once() {
    let fixture = fixture();
    let entries = fixture.as_array().expect("the fixture is an array");
    let mut actions: Vec<String> = entries
        .iter()
        .map(|entry| text_of(entry, "action"))
        .collect();
    let count = actions.len();
    actions.sort();
    actions.dedup();
    assert_eq!(actions.len(), count, "an action is listed twice");
    // The two note forms are separate actions: Save and Clear are different commands.
    assert!(actions.iter().any(|action| action == "noteDevice"));
    assert!(actions.iter().any(|action| action == "clearNote"));
}
