//! Task 11's end-to-end leak canary (plan P2): a person stores values, an agent uses and requests
//! them, and every captured output and every file hostler writes is searched for them. Assertion
//! messages never print a canary.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::contract::secrets::Materialize;
use hostler::secrets::value::RawCommand;
use serde_json::Value;

/// The canaries are not real values; they are long enough that masking would catch them.
const CANARY: &str = "hlx-canary-7f3a9c";
const IMPORTED: &str = "hlx-imported-4d2e8b";
const PASSWORD: &str = "master-pass-canary";

fn hold_exe() -> &'static Path {
    static HOLD: OnceLock<PathBuf> = OnceLock::new();
    HOLD.get_or_init(|| {
        escargot::CargoBuild::new()
            .example("hold")
            .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .run()
            .expect("building the hold example")
            .path()
            .to_path_buf()
    })
}

fn hold(args: &[&str]) -> Vec<std::ffi::OsString> {
    let mut argv = vec![hold_exe().as_os_str().to_owned()];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    argv
}

fn canaries() -> [&'static str; 2] {
    [CANARY, IMPORTED]
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn assert_no_canary(what: &str, bytes: &[u8]) {
    for canary in canaries() {
        assert!(!holds(bytes, canary), "{what} holds a canary");
    }
}

/// A checkout whose project is approved, with one secret stored from stdin.
fn project(home: &Home, name: &str) -> PathBuf {
    let repo = std::fs::canonicalize(home.make_repo(name)).unwrap();
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    repo
}

fn run(home: &Home, repo: &Path, args: &[&str]) -> Output {
    home.hostler()
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn run_argv(home: &Home, repo: &Path, flags: &[&str], argv: Vec<std::ffi::OsString>) -> Output {
    let mut command = home.hostler();
    command.arg("run").args(flags).arg("--").args(argv);
    command
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn import(home: &Home, repo: &Path, file: &Path) -> Output {
    run(home, repo, &["secret", "import", file.to_str().unwrap()])
}

/// Every regular file under `dir`, recursively, with its bytes.
fn files_under(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path, bytes));
        }
    }
}

#[test]
fn no_value_leaks_anywhere() {
    let home = Home::new();
    let repo = project(&home, "canary");
    let mut outputs: Vec<(&str, Output)> = Vec::new();

    // A person stores one value from stdin and imports a second one from a file.
    let set = home.set(&repo, "CANARY_ONE", CANARY);
    outputs.push(("secret set from stdin", set));

    let env_file = home.dir.path().join("canaries.env");
    std::fs::write(&env_file, format!("IMPORTED_ONE={IMPORTED}\n")).unwrap();
    let imported = import(&home, &repo, &env_file);
    assert!(imported.status.success(), "{}", stderr(&imported));
    outputs.push(("secret import", imported));
    std::fs::remove_file(&env_file).unwrap();

    // Every read command an agent may use.
    let listed = run(&home, &repo, &["secret", "list", "--json"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    outputs.push(("secret list --json", listed));
    let status = run(&home, &repo, &["secret", "status", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    outputs.push(("secret status --json", status));

    // The child prints the value; only `***` may reach the wrapper's output.
    let masked = run_argv(
        &home,
        &repo,
        &["--secret", "CANARY_ONE"],
        hold(&["--print-env", "CANARY_ONE"]),
    );
    assert_eq!(masked.status.code(), Some(0), "{}", stderr(&masked));
    assert!(holds(&masked.stdout, "***"), "{}", stdout(&masked));
    outputs.push(("run --secret", masked));

    let masked_file = run_argv(
        &home,
        &repo,
        &["--heavy", "--secret-file", "IMPORTED_ONE"],
        hold(&["--print-file-of", "IMPORTED_ONE"]),
    );
    assert_eq!(
        masked_file.status.code(),
        Some(0),
        "{}",
        stderr(&masked_file)
    );
    assert!(
        holds(&masked_file.stdout, "***"),
        "{}",
        stdout(&masked_file)
    );
    outputs.push(("run --heavy --secret-file", masked_file));

    let history = run(&home, &repo, &["queue", "history", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    outputs.push(("queue history --json", history));

    // A broken .env line that holds a canary: the parser's message must not quote the line.
    let bad_env = home.dir.path().join("bad.env");
    std::fs::write(
        &bad_env,
        format!("this line holds {CANARY} without an equals sign\n"),
    )
    .unwrap();
    let bad = import(&home, &repo, &bad_env);
    assert_eq!(bad.status.code(), Some(1), "{}", stderr(&bad));
    outputs.push(("secret import bad line", bad));
    std::fs::remove_file(&bad_env).unwrap();

    // A value on the command line of a sensitive command is refused without an echo.
    let value_arg = run(&home, &repo, &["secret", "set", "CANARY_ONE", CANARY]);
    assert_eq!(value_arg.status.code(), Some(2), "{}", stderr(&value_arg));
    outputs.push(("secret set NAME VALUE", value_arg));
    let flag = format!("--{CANARY}");
    let flag_arg = run(&home, &repo, &["secret", "set", "CANARY_ONE", &flag]);
    assert_eq!(flag_arg.status.code(), Some(2), "{}", stderr(&flag_arg));
    outputs.push(("secret set NAME --VALUE", flag_arg));

    // The literal value in the command line: masked in the child's output, the queue entry and
    // the materialization history.
    let literal = run_argv(
        &home,
        &repo,
        &["--heavy", "--secret", "CANARY_ONE"],
        hold(&["--print", CANARY]),
    );
    assert_eq!(literal.status.code(), Some(0), "{}", stderr(&literal));
    assert!(holds(&literal.stdout, "***"), "{}", stdout(&literal));
    outputs.push(("run --heavy literal value", literal));

    // A hostler.toml whose purpose is a number-like bad type holding a canary: config errors
    // name the line and never the text.
    let config = repo.join("hostler.toml");
    std::fs::write(
        &config,
        format!("[secrets.CANARY_ONE]\npurpose = {CANARY}\n"),
    )
    .unwrap();
    let broken = run(&home, &repo, &["secret", "list", "--json"]);
    assert!(!broken.status.success(), "{}", stdout(&broken));
    let broken_json: Value = serde_json::from_slice(&broken.stdout).unwrap();
    assert_eq!(broken_json["error"]["code"], "config_invalid");
    outputs.push(("hostler.toml bad type", broken));
    std::fs::remove_file(&config).unwrap();

    // A materialize of a missing name, with a canary in the command it would have recorded.
    let project = hostler::project::current(&repo).unwrap().project;
    let error = home
        .materialize(Materialize {
            run_id: uuid::Uuid::new_v4().to_string(),
            project,
            names: vec!["MISSING_ONE".into()],
            command: RawCommand::new(format!("deploy --token {CANARY}")),
            workspace: hostler::project::display_path(&repo),
        })
        .unwrap_err();
    let error_text = format!("{error:?}\n{}", serde_json::to_string(&error).unwrap());
    for canary in canaries() {
        assert!(
            !error_text.contains(canary),
            "the materialize error holds a canary"
        );
    }

    // No captured stream, log, database, WAL or run directory holds a canary.
    for (what, output) in &outputs {
        assert_no_canary(&format!("{what} stdout"), &output.stdout);
        assert_no_canary(&format!("{what} stderr"), &output.stderr);
    }

    let paths = home.paths();
    let mut files = Vec::new();
    files_under(&paths.log_dir, &mut files);
    files_under(&paths.runs, &mut files);
    for suffix in ["", "-wal", "-shm"] {
        let db = PathBuf::from(format!("{}{suffix}", paths.db.display()));
        if let Ok(bytes) = std::fs::read(&db) {
            files.push((db, bytes));
        }
    }
    for (path, bytes) in &files {
        for canary in canaries() {
            assert!(!holds(bytes, canary), "{} holds a canary", path.display());
        }
    }
}

#[test]
fn secret_requests_and_request_leak_no_value() {
    let home = Home::new();
    let repo = project(&home, "canary-requests");
    home.set(&repo, "CANARY_ONE", CANARY);
    let mut outputs: Vec<(&str, Output)> = Vec::new();

    // A request for a secret that is already set answers `stored` at once, from the stored value.
    let answered = run(
        &home,
        &repo,
        &[
            "secret",
            "request",
            "CANARY_ONE",
            "--reason",
            "integration test",
            "--json",
        ],
    );
    assert_eq!(answered.status.code(), Some(0), "{}", stderr(&answered));
    let envelope: Value = serde_json::from_slice(&answered.stdout).unwrap();
    assert_eq!(envelope["data"]["state"], "stored");
    outputs.push(("secret request already set", answered));

    // A pending request answered by another process's `secret set` must not carry the value.
    let child = home
        .hostler()
        .args([
            "secret",
            "request",
            "IMPORTED_ONE",
            "--reason",
            "integration test",
            "--max-wait",
            "0.5",
            "--json",
        ])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // `secret requests --json` while the request is pending: the row must hold no value.
    let until = Instant::now() + Duration::from_secs(10);
    let pending = loop {
        let out = run(&home, &repo, &["secret", "requests", "--json"]);
        assert!(out.status.success(), "{}", stderr(&out));
        if String::from_utf8_lossy(&out.stdout).contains("IMPORTED_ONE") {
            break out;
        }
        assert!(
            Instant::now() < until,
            "the request never appeared as pending"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    outputs.push(("secret requests --json while pending", pending));

    let set = home.set(&repo, "IMPORTED_ONE", IMPORTED);
    outputs.push(("secret set answering the request", set));

    let answered = child.wait_with_output().unwrap();
    assert_eq!(answered.status.code(), Some(0), "{}", stderr(&answered));
    let envelope: Value = serde_json::from_slice(&answered.stdout).unwrap();
    assert_eq!(envelope["data"]["state"], "stored");
    outputs.push(("secret request answered by set", answered));

    let requests = run(&home, &repo, &["secret", "requests", "--json"]);
    assert!(requests.status.success(), "{}", stderr(&requests));
    let value: Value = serde_json::from_slice(&requests.stdout).unwrap();
    assert_eq!(value["ok"], true);
    outputs.push(("secret requests --json after", requests));

    for (what, output) in &outputs {
        assert_no_canary(&format!("{what} stdout"), &output.stdout);
        assert_no_canary(&format!("{what} stderr"), &output.stderr);
    }
}
