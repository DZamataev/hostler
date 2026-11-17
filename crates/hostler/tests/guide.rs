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

/// The listing order (plain and `--json`), and the count, follow the one topic table.
#[test]
fn guide_lists_the_table_in_order() {
    let expected = [
        "start", "heavy", "secrets", "holds", "devices", "screen", "accounts", "init", "doctor",
        "sandbox", "recovery",
    ];
    let table: Vec<&str> = hostler::guide::TOPICS.iter().map(|t| t.name).collect();
    assert_eq!(table, expected);
    assert_eq!(hostler::guide::topic_names(), expected);

    let output = run(&["guide", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let listed: Vec<&str> = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(listed, expected);
    for (topic, entry) in value["data"]
        .as_array()
        .unwrap()
        .iter()
        .zip(hostler::guide::TOPICS)
    {
        assert_eq!(topic["summary"], entry.summary);
        assert!(!entry.summary.is_empty(), "{}", entry.name);
    }

    let output = run(&["guide"]);
    assert!(output.status.success(), "{output:?}");
    let plain = String::from_utf8(output.stdout).unwrap();
    let listed: Vec<&str> = plain
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(listed, expected, "{plain}");
}

/// The table is the only list (P4 Task 1): every `src/guide/*.md` file is in it with exactly
/// that file's text, every entry is served by `hostler guide <name>`, and every text starts
/// with its `# ` heading.
#[test]
fn topic_table_is_the_only_list() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/guide");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    files.sort();
    let mut table: Vec<String> = hostler::guide::TOPICS
        .iter()
        .map(|t| t.name.to_owned())
        .collect();
    table.sort();
    assert_eq!(files, table, "the .md files and the topic table differ");

    for entry in hostler::guide::TOPICS {
        let file = std::fs::read_to_string(dir.join(format!("{}.md", entry.name))).unwrap();
        assert_eq!(
            entry.text, file,
            "{}: the table's text is not its file",
            entry.name
        );
        assert!(
            entry.text.starts_with("# ") && entry.text.lines().next().is_some_and(|h| h.len() > 2),
            "{} does not start with a `# ` heading",
            entry.name
        );
        assert_eq!(hostler::guide::topic(entry.name), Some(entry.text));
        assert_eq!(topic_text(entry.name), entry.text, "{}", entry.name);
    }
    assert_eq!(hostler::guide::topic("nonexistent"), None);
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
    for entry in hostler::guide::TOPICS {
        let text = topic_text(entry.name);
        let bad = units_missing_the_sentence(&text);
        assert!(
            bad.is_empty(),
            "the {} guide mentions --force without the D9 sentence: {bad:?}",
            entry.name
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

// ---------------------------------------------------------------------------------------------
// P4 Task 8: the topics agents get, and the Codex rule they ship with.

/// The rule file `sandbox.md` embeds verbatim.
const RULE: &str = include_str!("../src/guide/codex_hostler.rules");

/// One item of a `prefix_rule` pattern: a command word, or a list of alternatives.
#[derive(Debug, PartialEq, Eq)]
enum Item {
    Word(String),
    List(Vec<String>),
}

/// Every `prefix_rule` of a rule file, as its pattern.
fn rules(text: &str) -> Vec<Vec<Item>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("prefix_rule(") {
        let after = &rest[at + "prefix_rule(".len()..];
        let end = after.find("\n)").map_or(after.len(), |i| i + 2);
        let block = &after[..end];
        assert!(
            block.contains("decision = \"allow\""),
            "every shipped rule must be an allow: {block}"
        );
        let start = block.find("pattern = [").expect("a pattern") + "pattern = [".len();
        out.push(parse_pattern(&block[start..]));
        rest = &after[end..];
    }
    assert!(!out.is_empty(), "no rule found");
    out
}

/// The index of the `]` closing a list whose opening `[` is just before `text`.
fn bracket_end(text: &str) -> usize {
    let mut depth = 1usize;
    let mut in_string = false;
    for (index, ch) in text.char_indices() {
        if ch == '"' {
            in_string = !in_string;
        } else if !in_string {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return index;
                    }
                }
                _ => {}
            }
        }
    }
    panic!("unbalanced pattern: {text}");
}

fn parse_pattern(text: &str) -> Vec<Item> {
    let body = &text[..bracket_end(text)];
    let mut items = Vec::new();
    let mut index = 0;
    while index < body.len() {
        let ch = body[index..].chars().next().unwrap();
        match ch {
            '"' => {
                let rest = &body[index + 1..];
                let close = rest.find('"').expect("a closed string");
                items.push(Item::Word(rest[..close].to_string()));
                index += close + 2;
            }
            '[' => {
                let inner = &body[index + 1..];
                let close = bracket_end(inner);
                items.push(Item::List(quoted_words(&inner[..close])));
                index += close + 2;
            }
            ',' | ' ' | '\n' => index += 1,
            other => panic!("unexpected {other:?} in {body}"),
        }
    }
    items
}

fn quoted_words(text: &str) -> Vec<String> {
    text.split('"')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, word)| word.to_string())
        .collect()
}

/// Every subcommand word of a rule is one hostler has, so a renamed command fails the test.
#[test]
fn codex_rule_names_real_subcommands() {
    use clap::CommandFactory;
    let root = hostler::cli::Cli::command();
    let find = |parent: &clap::Command, name: &str| {
        parent
            .get_subcommands()
            .find(|sub| sub.get_name() == name)
            .cloned()
    };
    for pattern in rules(RULE) {
        assert!(
            matches!(&pattern[0], Item::Word(word) if word == "hostler"),
            "{pattern:?}"
        );
        match &pattern[1] {
            Item::List(words) => {
                for word in words {
                    assert!(
                        find(&root, word).is_some(),
                        "{word} is not a hostler command"
                    );
                }
            }
            Item::Word(command) => {
                let sub = find(&root, command)
                    .unwrap_or_else(|| panic!("{command} is not a hostler command"));
                let check = |word: &str| {
                    if let Some(flag) = word.strip_prefix("--") {
                        assert!(
                            sub.get_arguments().any(|arg| arg.get_long() == Some(flag)),
                            "{command} has no --{flag}"
                        );
                    } else {
                        assert!(
                            find(&sub, word).is_some(),
                            "{command} {word} is not a hostler command"
                        );
                    }
                };
                match &pattern[2] {
                    Item::List(words) => words.iter().for_each(|word| check(word)),
                    Item::Word(word) => check(word),
                }
            }
        }
    }
    // `renew` is a hold subcommand, never a top-level one.
    assert!(
        root.get_subcommands().all(|sub| sub.get_name() != "renew"),
        "renew is a top-level command"
    );
    let hold = find(&root, "hold").expect("hold is a command");
    assert!(
        find(&hold, "renew").is_some(),
        "hold renew is not a command"
    );
}

/// P4-L9/P4-L10: the shipped rule is narrow, and `sandbox.md` carries it verbatim.
#[test]
fn sandbox_ships_the_narrow_codex_rule() {
    let text = topic_text("sandbox");
    assert!(
        text.contains(RULE),
        "sandbox.md does not embed src/guide/codex_hostler.rules verbatim"
    );

    // Commands the rule must never allow: person-only, destructive, or `run` (its child too).
    const NEVER: [&str; 23] = [
        "run",
        // C-1 (P4-L28): it re-registers and restarts, and `--force` overrides live work.
        "install",
        "unlock",
        "lock",
        "__gate",
        "add",
        "rename",
        "remove",
        "note",
        "forget-sessions",
        "reserve",
        "unreserve",
        "uninstall",
        "stop",
        "restart",
        "capacity",
        "withdraw",
        "set",
        "import",
        "share",
        "unshare",
        "decline",
        "approve",
    ];
    /// A second token that must always come with a subcommand list (P4-L10).
    const REGISTRY: [&str; 7] = [
        "devices", "service", "queue", "secret", "account", "project", "hold",
    ];

    let patterns = rules(RULE);
    let mut allowed = std::collections::BTreeSet::new();
    for pattern in &patterns {
        assert!(
            matches!(&pattern[0], Item::Word(word) if word == "hostler"),
            "{pattern:?}"
        );
        assert!(
            pattern.len() > 1,
            "a bare [\"hostler\"] rule allows every command: {pattern:?}"
        );
        let words: Vec<&str> = pattern
            .iter()
            .flat_map(|item| match item {
                Item::Word(word) => vec![word.as_str()],
                Item::List(words) => words.iter().map(String::as_str).collect(),
            })
            .collect();
        for word in &words {
            assert!(
                !NEVER.contains(word),
                "the shipped rule allows {word:?}: {pattern:?}"
            );
        }
        match (&pattern[1], pattern.get(2)) {
            (Item::List(commands), None) => {
                for command in commands {
                    allowed.insert(command.clone());
                }
            }
            (Item::Word(command), Some(Item::List(subcommands))) => {
                assert!(
                    !REGISTRY.contains(&command.as_str()) || !subcommands.is_empty(),
                    "a bare {command} prefix: {pattern:?}"
                );
                for subcommand in subcommands {
                    allowed.insert(format!("{command} {subcommand}"));
                }
            }
            (Item::List(commands), Some(Item::Word(subcommand))) => {
                assert!(!REGISTRY.contains(&subcommand.as_str()), "{pattern:?}");
                for command in commands {
                    allowed.insert(format!("{command} {subcommand}"));
                }
            }
            (Item::Word(command), None) => panic!("a bare {command} prefix: {pattern:?}"),
            other => panic!("unexpected pattern shape {other:?}"),
        }
    }

    // The exact set Task 5 shipped (and the plan lists): a widening edit fails here.
    let expected: std::collections::BTreeSet<String> = [
        "status",
        "release",
        "workspace",
        "guide",
        "init",
        "doctor",
        "hold screen",
        "hold account",
        "hold device",
        "hold renew",
        "devices list",
        "devices show",
        "devices installs",
        "devices tools",
        "service status",
        "service start",
        "queue --json",
        "queue history",
        "secret status",
        "secret list",
        "secret request",
        "secret requests",
        "account list",
        "project list",
    ]
    .iter()
    .map(|entry| (*entry).to_string())
    .collect();
    assert_eq!(allowed, expected, "the shipped rule's allow list changed");
}

/// P4-L5: the sandbox topic names no experimental agent setting and no full-network one.
#[test]
fn sandbox_names_no_experimental_setting() {
    let text = topic_text("sandbox");
    assert!(
        text.contains("allowUnixSockets"),
        "the sandbox guide lacks Claude Code's documented setting"
    );
    for forbidden in [
        "network_proxy",
        "features.",
        "unix_sockets",
        "dangerously_allow_all_unix_sockets",
        "network_access",
        "network.enabled",
    ] {
        assert!(
            !text.contains(forbidden),
            "the sandbox guide names the forbidden setting {forbidden:?}"
        );
    }
}

#[test]
fn sandbox_has_the_phrases_an_agent_needs() {
    let text = normalized(&topic_text("sandbox"));
    for needed in [
        "sandbox_denied",
        "outside the sandbox",
        "fails open",
        "plain `hostler`",
        "trusted project",
        "hostler project list",
        "hostler run --heavy",
        "exit 78",
        "hostler guide recovery",
    ] {
        assert!(text.contains(needed), "the sandbox guide lacks {needed:?}");
    }
}

/// Every error code the contract can carry, by wire name, is in the recovery topic.
/// C-4 (P4 clean read): `hostler service status`'s own exit code 3 (not running) is documented
/// where the exit codes are, and in the heavy guide's service list.
#[test]
fn service_status_exit_code_3_is_documented() {
    let recovery = normalized(&topic_text("recovery"));
    assert!(
        recovery.contains("`hostler service status` exits **3** when the service is not running"),
        "recovery.md lacks service status's exit code 3"
    );
    assert!(topic_text("heavy").contains("exit 0 running, 3 not running"));
}

#[test]
fn recovery_names_every_error_code() {
    use hostler::contract::ErrorCode;
    let codes = [
        ErrorCode::ServiceUnavailable,
        ErrorCode::ContractMismatch,
        ErrorCode::ServiceBusy,
        ErrorCode::BadRequest,
        ErrorCode::UnknownEntry,
        ErrorCode::QueueTimeout,
        ErrorCode::QueueBusy,
        ErrorCode::Withdrawn,
        ErrorCode::CapacityOutOfRange,
        ErrorCode::ForceRequired,
        ErrorCode::Internal,
        ErrorCode::NotInteractive,
        ErrorCode::NotInProject,
        ErrorCode::ConfigInvalid,
        ErrorCode::ProjectNotApproved,
        ErrorCode::SecretsLocked,
        ErrorCode::UnlockFailed,
        ErrorCode::SecretsConflict,
        ErrorCode::SecretMissing,
        ErrorCode::SecretNotInProject,
        ErrorCode::SecretExists,
        ErrorCode::SecretTooShort,
        ErrorCode::RequestTimeout,
        ErrorCode::UnknownResource,
        ErrorCode::ResourceInUse,
        ErrorCode::ClaimNotHeld,
        ErrorCode::SeizureFailed,
        ErrorCode::AccountMissing,
        ErrorCode::ClaimDeadlock,
        ErrorCode::TakenOver,
        ErrorCode::DeviceNotFound,
        ErrorCode::AliasTaken,
        ErrorCode::DeviceNotRegistered,
        ErrorCode::DeviceNotReady,
        ErrorCode::BootFailed,
        ErrorCode::ToolMissing,
        ErrorCode::PermissionDenied,
    ];
    assert_eq!(codes.len(), 37, "the contract has 37 error codes");
    let text = normalized(&topic_text("recovery"));
    for code in codes {
        let value = serde_json::to_value(code).unwrap();
        let name = value.as_str().unwrap_or_else(|| panic!("{value}"));
        assert!(
            text.contains(&format!("`{name}`")),
            "the recovery guide lacks {name:?}"
        );
    }
    // Its `service_unavailable` reasons and both mismatch directions are documented too.
    for needed in [
        "not_running",
        "sandbox_denied",
        "timeout",
        "newer",
        "older",
        "unknown",
        "hostler service start",
    ] {
        assert!(text.contains(needed), "the recovery guide lacks {needed:?}");
    }
}

/// Every topic is reachable from the overview, so an agent that reads one page finds the rest.
#[test]
fn start_mentions_every_topic() {
    let text = normalized(&topic_text("start"));
    for entry in hostler::guide::TOPICS {
        assert!(
            text.contains(entry.name),
            "the start guide does not mention the {} topic",
            entry.name
        );
    }
    for needed in [
        "hostler status",
        "hostler doctor",
        "hostler init",
        FORCE_SENTENCE,
    ] {
        assert!(text.contains(needed), "the start guide lacks {needed:?}");
    }
}

/// Every check id doctor reports is documented, so an agent can look one up.
#[test]
fn doctor_lists_every_check_id() {
    let text = normalized(&topic_text("doctor"));
    assert_eq!(hostler::doctor::CHECK_IDS.len(), 21);
    for id in hostler::doctor::CHECK_IDS {
        assert!(
            text.contains(&format!("`{id}`")),
            "the doctor guide lacks the check id {id:?}"
        );
    }
    for needed in [
        "Exit codes",
        "0",
        "1",
        "78",
        "hostler service install",
        "hostler service start",
        "hostler service restart",
        "20 second",
        "refused",
    ] {
        assert!(text.contains(needed), "the doctor guide lacks {needed:?}");
    }
}

/// The docs block carries the force rule, and `init.md` quotes it.
#[test]
fn init_block_has_force_sentence() {
    let text = normalized(&topic_text("init"));
    for needed in [
        FORCE_SENTENCE,
        "<!-- hostler:begin -->",
        "<!-- hostler:end -->",
        "init.device_rules",
        "init.sandbox",
        "init.service",
        "--write",
        "never rewritten",
        "for the person",
    ] {
        assert!(text.contains(needed), "the init guide lacks {needed:?}");
    }
    // The block itself, as the project receives it, carries the same sentence.
    let block = hostler::init::docs::block_text(
        &hostler::project::config::parse("[project]\nname = \"x\"\n").expect("a valid config"),
    );
    assert!(block.contains(FORCE_SENTENCE), "{block}");
}

// ---------------------------------------------------------------------------------------------
// P4 Task 8 fix round 1: the service_unavailable matrix, the agent-facing text rules, and the
// rule's matching.

/// One `service_unavailable` reason: where the crate emits it, and a phrase of the recovery the
/// emitted error carries (`recovery` is empty only where the error carries none).
struct UnavailableReason {
    reason: &'static str,
    /// The emission site in the crate, so a new or changed reason is easy to add here.
    site: &'static str,
    recovery: &'static str,
}

/// Every `details.reason` a `service_unavailable` error carries, from the crate's own sites.
/// `client::unavailable` (client.rs) attaches `hostler service start` unless a site overrides it:
/// - `not_running`: `connect_failed` and `check_socket_owner`;
/// - `sandbox_denied`: `sandbox_denied_at` (overrides with `sandbox_recovery`);
/// - `timeout`: the connect and call timers, plus `cli/run.rs`, `cli/resource_run.rs` and
///   `cli/secrets/request.rs`;
/// - `stopping`: `contract/secrets.rs::service_stopping`;
/// - `io`, `closed`, `protocol`, `bad_socket_name`, `foreign_owner`, `no_executable`,
///   `spawn_failed`, `manager_failed`: `client.rs`;
/// - `start_timeout`: `client.rs`, overridden to `hostler service install` after a manager start;
/// - `manager_timeout`, `manager_start_failed`: `client.rs`, overridden to `hostler service status`;
/// - `protocol` (a status answer): `service/lifecycle.rs::probe_connection`, no recovery;
/// - `registration_outdated`: `cli/service.rs::outdated_registration`.
const UNAVAILABLE_REASONS: &[UnavailableReason] = &[
    UnavailableReason {
        reason: "not_running",
        site: "client.rs connect_failed / check_socket_owner, cli/service.rs compose_status",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "sandbox_denied",
        site: "client.rs sandbox_denied_at (sandbox_recovery)",
        recovery: "check the permissions of",
    },
    UnavailableReason {
        reason: "sandbox_denied",
        site: "client.rs sandbox_denied_at (sandbox_recovery)",
        recovery: "hostler guide sandbox",
    },
    UnavailableReason {
        reason: "timeout",
        site: "client.rs connect/call timers, cli/run.rs, cli/resource_run.rs, cli/secrets/request.rs",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "timeout",
        site: "doctor/checks.rs timed_out (a check message)",
        recovery: "no recovery",
    },
    UnavailableReason {
        reason: "stopping",
        site: "contract/secrets.rs service_stopping",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "io",
        site: "client.rs read_frames and exchange",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "closed",
        site: "client.rs read_frames and read_frame",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "protocol",
        site: "client.rs read_frames (an unreadable frame)",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "protocol",
        site: "service/lifecycle.rs probe_connection (an unreadable status answer)",
        recovery: "no recovery",
    },
    UnavailableReason {
        reason: "bad_socket_name",
        site: "client.rs connect",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "foreign_owner",
        site: "client.rs check_socket_owner",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "no_executable",
        site: "client.rs connect_or_start",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "spawn_failed",
        site: "client.rs connect_or_start_with",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "start_timeout",
        site: "client.rs connect_or_start_with (a spawned start)",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "start_timeout",
        site: "client.rs connect_or_start_with (a manager start)",
        recovery: "hostler service install",
    },
    UnavailableReason {
        reason: "manager_failed",
        site: "client.rs off_thread",
        recovery: "hostler service start",
    },
    UnavailableReason {
        reason: "manager_timeout",
        site: "client.rs off_thread",
        recovery: "hostler service status",
    },
    UnavailableReason {
        reason: "manager_start_failed",
        site: "client.rs manager_start_failed",
        recovery: "hostler service status",
    },
    UnavailableReason {
        reason: "registration_outdated",
        site: "cli/service.rs outdated_registration",
        recovery: "hostler service install",
    },
];

/// The documented matrix is exactly the emitted reason set, and each row carries that reason's
/// recovery as the code emits it.
#[test]
fn recovery_documents_every_unavailable_reason_and_its_recovery() {
    let text = topic_text("recovery");
    let section = text
        .split("## `service_unavailable` and its reasons")
        .nth(1)
        .expect("the service_unavailable section")
        .split("\n## ")
        .next()
        .unwrap();
    let rows: Vec<&str> = section
        .lines()
        .filter(|line| line.starts_with("| `") && !line.starts_with("| `details.reason`"))
        .collect();
    let documented: std::collections::BTreeSet<String> = rows
        .iter()
        .map(|row| {
            row.split('|')
                .nth(1)
                .and_then(|cell| cell.split('`').nth(1))
                .expect("a backticked reason")
                .to_string()
        })
        .collect();

    let mut expected = std::collections::BTreeSet::new();
    for entry in UNAVAILABLE_REASONS {
        expected.insert(entry.reason.to_string());
        let row = rows
            .iter()
            .find(|row| row.starts_with(&format!("| `{}` |", entry.reason)))
            .unwrap_or_else(|| panic!("recovery.md has no row for {}", entry.reason));
        assert!(
            row.contains(entry.recovery),
            "the {} row ({}:{}) lacks its recovery {:?}: {row}",
            entry.reason,
            entry.site.split(' ').next().unwrap(),
            entry.site,
            entry.recovery
        );
    }
    assert_eq!(expected.len(), 16, "the emitted reason set changed");
    assert_eq!(
        documented, expected,
        "recovery.md's reasons and the crate's differ"
    );
}

/// The agent-facing text carries no internal phase label, no MCP promise, and no `run` JSON.
#[test]
fn guide_and_readme_have_no_internal_labels() {
    let mut texts: Vec<(String, String)> = hostler::guide::TOPICS
        .iter()
        .map(|entry| (entry.name.to_string(), topic_text(entry.name)))
        .collect();
    texts.push((
        "README".into(),
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap(),
    ));
    for (name, text) in texts {
        assert!(
            !text.to_ascii_uppercase().contains("MCP"),
            "the {name} text still promises MCP"
        );
        for (index, line) in text.lines().enumerate() {
            // A phase label: `P` + a digit at the start of a word (`P2`, `P3b-1`, `P4`, `P4-L9`).
            for (at, _) in line.match_indices('P') {
                let starts_word = at == 0 || !line.as_bytes()[at - 1].is_ascii_alphanumeric();
                let followed_by_digit = line.as_bytes().get(at + 1).is_some_and(u8::is_ascii_digit);
                assert!(
                    !(starts_word && followed_by_digit),
                    "{name}:{}: an internal phase label: {line:?}",
                    index + 1
                );
            }
            // `hostler run` has no `--json` envelope; no line may claim one.
            if line.contains("--json") {
                assert!(
                    !line.contains("run --"),
                    "{name}:{}: a `run` JSON claim: {line:?}",
                    index + 1
                );
            }
        }
    }
    // The sentence the sandbox topic must keep: run has no envelope, and the denial is reported by
    // the commands that do take `--json`.
    let sandbox = normalized(&topic_text("sandbox"));
    assert!(sandbox.contains("no `--json` envelope"), "{sandbox}");
    assert!(
        sandbox.contains("hostler status --json"),
        "the sandbox guide does not name a command with `--json`"
    );
}

/// Splits a shell command the way Codex's rule matching does: every segment at `|`, `||`, `&&`
/// and `;` must match, and an env prefix, a redirection, a substitution or a glob is not
/// evaluated at all. A model of the documented policy: `codex execpolicy check` against the real
/// binary stays Task 9's real-session check.
fn codex_command_allowed(command: &str) -> bool {
    if command.contains(['>', '<', '$', '*', '?']) {
        return false;
    }
    let patterns = rules(RULE);
    command
        .split("||")
        .flat_map(|part| part.split("&&"))
        .flat_map(|part| part.split(';'))
        .flat_map(|part| part.split('|'))
        .all(|segment| {
            let words: Vec<&str> = segment.split_whitespace().collect();
            if words.is_empty() || words[0].contains('=') {
                return false;
            }
            patterns.iter().any(|pattern| {
                pattern.iter().enumerate().all(|(at, item)| {
                    let Some(word) = words.get(at) else {
                        return false;
                    };
                    match item {
                        Item::Word(one) => one == word,
                        Item::List(alternatives) => alternatives.iter().any(|alt| alt == word),
                    }
                })
            })
        })
}

/// Representative matching for the shipped rule, at the level this suite can exercise.
#[test]
fn codex_rule_matches_like_the_documented_policy() {
    for allowed in [
        "hostler status --json",
        "hostler hold screen --json",
        "hostler release abc-1",
        "hostler devices list",
        "hostler devices show pixel-9",
        "hostler service status --json",
        "hostler queue --json",
        "hostler queue history",
        "hostler secret list --json",
        "hostler account list",
        "hostler project list",
        "hostler service start",
        // A prefix rule cannot exclude a flag (P4-L11): the force rule is the agent's to keep.
        "hostler hold screen --force --confirmation screen",
    ] {
        assert!(
            codex_command_allowed(allowed),
            "{allowed:?} should run outside the sandbox"
        );
    }
    for blocked in [
        "hostler",
        "hostler run --heavy -- true",
        "hostler run --device pixel-9 -- ./gradlew",
        "hostler unlock",
        "hostler lock",
        "hostler project approve",
        "hostler secret import .env.local",
        "hostler secret set API_KEY",
        "hostler service stop",
        "hostler service restart",
        // C-1 (P4-L28): never outside the sandbox, with or without `--force`.
        "hostler service install",
        "hostler service install --force",
        "hostler queue capacity set 2",
        "hostler queue withdraw --id x",
        "hostler devices add emulator-5554",
        "hostler devices remove pixel-9",
        "cd /tmp && hostler status",
        "hostler status | head",
        "HOSTLER_HOME=/x hostler status",
        "hostler status > out.txt",
        "hostler status $(true)",
    ] {
        assert!(
            !codex_command_allowed(blocked),
            "{blocked:?} must stay sandboxed"
        );
    }
}
