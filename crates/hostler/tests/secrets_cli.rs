//! Task 8's CLI commands against the real binary and a real service (plan P2):
//! `unlock`, `lock`, `secret status|list|set|remove|import|share|unshare` and
//! `project resolve|list|approve`. Every test owns a `Home` and stops its service on drop.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::contract::secrets::{
    LockState, Materialize, ProjectInfo, RequestState, SecretList, SecretSource, SecretState,
    SecretsStatus,
};
use hostler::contract::wire::Request;
use hostler::secrets::MAX_VALUE_BYTES;
use hostler::secrets::value::RawCommand;
use serde_json::Value;

const PASSWORD: &str = "master-pass-1";

fn run(home: &Home, cwd: &Path, args: &[&str]) -> Output {
    home.hostler().args(args).current_dir(cwd).output().unwrap()
}

fn tty_file(home: &Home, lines: &[&str]) -> PathBuf {
    let path = home
        .dir
        .path()
        .join(format!("tty-{}", uuid::Uuid::new_v4()));
    let mut file = fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    path
}

fn run_tty(home: &Home, cwd: &Path, args: &[&str], lines: &[&str]) -> Output {
    home.hostler()
        .args(args)
        .current_dir(cwd)
        .env("HOSTLER_TEST_TTY", tty_file(home, lines))
        .output()
        .unwrap()
}

fn run_stdin(home: &Home, cwd: &Path, args: &[&str], stdin: impl AsRef<[u8]>) -> Output {
    let mut child = home
        .hostler()
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_ref())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn status(home: &Home) -> SecretsStatus {
    let value = home.call(Request::SecretsStatus).unwrap();
    serde_json::from_value(value).unwrap()
}

fn secret_list(home: &Home, repo: &Path) -> SecretList {
    let project = hostler::project::current(repo).unwrap().project;
    let value = home
        .call(Request::SecretList {
            project,
            declared: Vec::new(),
        })
        .unwrap();
    serde_json::from_value(value).unwrap()
}

fn materialize(home: &Home, repo: &Path, name: &str) -> String {
    let project = hostler::project::current(repo).unwrap().project;
    let workspace = hostler::project::display_path(repo);
    let names = vec![name.to_string()];
    let secrets = home
        .materialize(Materialize {
            run_id: uuid::Uuid::new_v4().to_string(),
            project,
            names: names.clone(),
            command: RawCommand::new("echo test".into()),
            workspace,
        })
        .unwrap();
    secrets[0].value.expose().to_owned()
}

#[test]
fn unlock_creates_after_confirmation() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let out = home.unlock_new(PASSWORD);
    assert!(stdout(&out).contains("Unlocked."), "{}", stdout(&out));
    // W-6: a custom home is never run by the OS service manager, so the person is told.
    assert!(
        stderr(&out).contains("HOSTLER_HOME is set, so the service is not run by the OS"),
        "{}",
        stderr(&out)
    );
    assert_eq!(status(&home).state, LockState::Unlocked);
    assert!(repo.join(".git/HEAD").is_file());
}

#[test]
fn unlock_without_terminal_is_not_interactive() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let out = run(&home, &repo, &["unlock"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("needs a person at a terminal"), "{err}");
    assert!(
        err.contains("A person runs this; an agent asks the operator."),
        "{err}"
    );
    assert!(!err.contains("Master password"), "{err}");
}

#[test]
fn unlock_retries_wrong_password_three_times() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    let out = run(&home, &repo, &["lock"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let out = run_tty(
        &home,
        &repo,
        &["unlock"],
        &["wrong-pass-1", "wrong-pass-2", "wrong-pass-3"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("wrong master password or key file"),
        "{}",
        stderr(&out)
    );
    assert_eq!(status(&home).state, LockState::Locked);
}

#[test]
fn approve_needs_the_typed_name() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);

    let out = run_tty(&home, &repo, &["project", "approve"], &["wrong-name"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not approved"), "{}", stderr(&out));
    let projects: Vec<ProjectInfo> =
        serde_json::from_value(home.call(Request::ProjectList).unwrap()).unwrap();
    let app = projects
        .iter()
        .find(|project| project.name == "app")
        .unwrap();
    assert!(!app.approved);

    home.approve(&repo);
    let projects: Vec<ProjectInfo> =
        serde_json::from_value(home.call(Request::ProjectList).unwrap()).unwrap();
    let app = projects
        .iter()
        .find(|project| project.name == "app")
        .unwrap();
    assert!(app.approved);
}

#[test]
fn list_when_locked_is_secrets_locked_at_once() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    run(&home, &repo, &["lock"]);

    let started = Instant::now();
    let out = run(&home, &repo, &["secret", "list"]);
    let elapsed = started.elapsed();
    assert!(!out.status.success());
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
    assert!(stderr(&out).contains("locked"), "{}", stderr(&out));
}

#[test]
fn set_from_stdin_strips_one_newline() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);

    let out = run_stdin(&home, &repo, &["secret", "set", "API_KEY"], "value-123\n");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(materialize(&home, &repo, "API_KEY"), "value-123");
}

#[test]
fn set_refuses_3_bytes() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);

    let out = run_stdin(&home, &repo, &["secret", "set", "API_KEY"], "abc");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("at least 4 bytes"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn value_as_argument_is_refused_without_echo() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let cases: &[&[&str]] = &[
        &["secret", "set", "API_KEY", "hunter2-canary"],
        &["secret", "set", "API_KEY", "--hunter2-canary"],
        &["secret", "set", "API_KEY", "--hunter2=canary"],
        &["secret", "set", "hunter2-canary"],
        &["secret", "set", "--help=hunter2"],
        &["unlock", "hunter2-canary"],
        &["unlock", "--hunter2-canary"],
        &["unlock", "--key-file"],
        &["secret", "import", "f.env", "--hunter2-canary"],
    ];
    for args in cases {
        let out = run(&home, &repo, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let all = format!("{}{}", stdout(&out), stderr(&out));
        assert!(!all.contains("hunter2"), "{args:?}: {all}");
    }
}

#[test]
fn help_still_prints() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let out = run(&home, &repo, &["secret", "set", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("Usage"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("hunter2"));
}

#[test]
fn restart_says_the_database_locks() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);

    let out = run(&home, &repo, &["service", "restart"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("The secrets database is locked again after the restart"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn import_prints_names_only() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let file = home.dir.path().join("app.env");
    fs::write(
        &file,
        "JACKETT_URL=https://jackett.test\nJACKETT_API_KEY=jackett-value-1\n",
    )
    .unwrap();

    let out = run(&home, &repo, &["secret", "import", path(&file)]);
    assert!(out.status.success(), "{}", stderr(&out));
    let printed = stdout(&out);
    assert!(printed.contains("JACKETT_URL"), "{printed}");
    assert!(printed.contains("JACKETT_API_KEY"), "{printed}");
    let all = format!("{printed}{}", stderr(&out));
    assert!(!all.contains("jackett.test"), "{all}");
    assert!(!all.contains("jackett-value-1"), "{all}");
    assert!(file.exists());
}

#[test]
fn import_problem_stores_nothing() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let file = home.dir.path().join("bad.env");
    fs::write(&file, "bad-name=hunter2-x\nNO_EQUALS hunter2-y\n").unwrap();

    let out = run(&home, &repo, &["secret", "import", path(&file)]);
    assert_eq!(out.status.code(), Some(1));
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!all.contains("hunter2"), "{all}");
    assert!(secret_list(&home, &repo).secrets.is_empty());
}

#[test]
fn import_existing_needs_overwrite() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let file = home.dir.path().join("app.env");
    fs::write(&file, "API_KEY=first-value\n").unwrap();
    let out = run(&home, &repo, &["secret", "import", path(&file)]);
    assert!(out.status.success(), "{}", stderr(&out));

    fs::write(&file, "API_KEY=second-value\n").unwrap();
    let out = run(&home, &repo, &["secret", "import", path(&file)]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("already set"), "{}", stderr(&out));

    let out = run(
        &home,
        &repo,
        &["secret", "import", path(&file), "--overwrite"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(materialize(&home, &repo, "API_KEY"), "second-value");
}

#[test]
fn import_delete_flag_deletes() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let file = home.dir.path().join("app.env");
    fs::write(&file, "API_KEY=delete-me-1\n").unwrap();

    let out = run(&home, &repo, &["secret", "import", path(&file), "--delete"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!file.exists());
}

#[test]
fn import_without_terminal_never_deletes() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let file = home.dir.path().join("app.env");
    fs::write(&file, "API_KEY=keep-file-1\n").unwrap();

    let out = run(&home, &repo, &["secret", "import", path(&file)]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(file.exists());
    assert!(
        stderr(&out).contains("still holds the values in clear"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn share_then_list_shows_shared_from() {
    let home = Home::new();
    let lib = home.make_repo("lib");
    let app = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&lib);
    home.approve(&app);
    home.set(&lib, "SHARED", "lib-value-1");

    let out = run_tty(
        &home,
        &lib,
        &["secret", "share", "SHARED", "--to", "app"],
        &["app"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("shared SHARED with app"));

    let list = secret_list(&home, &app);
    let shared = list
        .secrets
        .iter()
        .find(|secret| secret.name == "SHARED")
        .unwrap();
    assert_eq!(shared.state, SecretState::Set);
    match &shared.source {
        Some(SecretSource::SharedFrom { project, .. }) => assert_eq!(project, "lib"),
        other => panic!("unexpected source: {other:?}"),
    }

    let out = run(&home, &app, &["secret", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("shared from lib"), "{}", stdout(&out));
}

#[test]
fn person_only_help_says_who_runs_it() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let sentence = "A person runs this; an agent asks the operator.";

    for args in [
        &["unlock", "--help"][..],
        &["project", "approve", "--help"],
        &["secret", "share", "--help"],
        &["secret", "unshare", "--help"],
    ] {
        let out = run(&home, &repo, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(
            stdout(&out).trim_end().ends_with(sentence),
            "{args:?}: {}",
            stdout(&out)
        );
    }

    for args in [
        &["project", "approve"][..],
        &["secret", "share", "API_KEY", "--to", "lib"],
        &["secret", "unshare", "API_KEY", "--to", "lib"],
    ] {
        let out = run(&home, &repo, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&out).contains(sentence),
            "{args:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn unapproved_set_is_refused_with_recovery() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);

    let out = run_stdin(&home, &repo, &["secret", "set", "API_KEY"], "value-123");
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("is not approved for secrets"), "{err}");
    assert!(err.contains("To fix: hostler project approve"), "{err}");
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn secret_status_is_json_when_asked() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);

    let out = run(&home, &repo, &["secret", "status", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["state"], "unlocked");
    assert!(
        value["data"]["database"]
            .as_str()
            .unwrap()
            .ends_with("hostler.kdbx")
    );
}

#[test]
fn project_and_project_list_resolve_the_cwd() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);

    let out = run(&home, &repo, &["project", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["data"]["name"], "app");
    assert_eq!(value["data"]["approved"], false);

    let out = run(&home, &repo, &["project", "list", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["data"][0]["name"], "app");
}

#[test]
fn remove_reports_whether_the_secret_was_there() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    home.set(&repo, "API_KEY", "value-123");

    let out = run(&home, &repo, &["secret", "remove", "API_KEY", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["data"]["removed"], true);

    let out = run(&home, &repo, &["secret", "remove", "API_KEY", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["data"]["removed"], false);
}

#[test]
fn lock_reports_json_status() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);

    let out = run(&home, &repo, &["lock", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["data"]["state"], "locked");
}

#[test]
fn requests_are_answered_by_set() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let project = hostler::project::current(&repo).unwrap().project;

    let request = hostler::contract::secrets::SecretRequest {
        id: uuid::Uuid::new_v4().to_string(),
        project: project.clone(),
        name: "WANTED".into(),
        reason: "the test wants it".into(),
        requester: hostler::procs::ProcessIdentity {
            pid: std::process::id(),
            boot_id: "test-boot".into(),
            start_time: 1,
        },
    };
    let info = home.call(Request::SecretRequest(request)).unwrap();
    assert_eq!(
        info["state"],
        serde_json::to_value(RequestState::Pending).unwrap()
    );

    let out = run_stdin(&home, &repo, &["secret", "set", "WANTED"], "wanted-value");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("answered request(s)"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn share_by_path_then_unshare_removes_it() {
    let home = Home::new();
    let lib = home.make_repo("lib");
    let app = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&lib);
    home.approve(&app);
    home.set(&lib, "SHARED", "lib-value-1");

    let out = run_tty(
        &home,
        &lib,
        &["secret", "share", "SHARED", "--to", path(&app)],
        &["app"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        secret_list(&home, &app)
            .secrets
            .iter()
            .any(|secret| secret.name == "SHARED")
    );

    let out = run_tty(
        &home,
        &lib,
        &["secret", "unshare", "SHARED", "--to", path(&app)],
        &["y"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !secret_list(&home, &app)
            .secrets
            .iter()
            .any(|secret| secret.name == "SHARED")
    );
}

#[test]
fn run_parse_errors_with_a_secret_argument_are_not_echoed() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let out = run(
        &home,
        &repo,
        &[
            "run",
            "--secret",
            "API_KEY",
            "--bogus-hunter2-canary",
            "--",
            "true",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!all.contains("hunter2"), "{all}");
}

#[test]
fn an_import_file_over_1_mib_is_refused_without_echoing_it() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);

    // Valid entries (so a build without the bound would import them), over 1 MiB in total.
    let mut data = String::new();
    for index in 0..1000 {
        let mut value = "a".repeat(1100 - 15);
        if index == 999 {
            value.push_str("-hunter2-canary");
        }
        data.push_str(&format!("K{index}={value}\n"));
    }
    assert!(data.len() as u64 > 1024 * 1024);
    let file = home.dir.path().join("big.env");
    fs::write(&file, &data).unwrap();

    let out = run(&home, &repo, &["secret", "import", path(&file)]);
    assert_eq!(out.status.code(), Some(1));
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!all.contains("hunter2"), "{all}");
}

#[test]
fn an_import_argument_that_cannot_be_read_is_not_echoed() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let out = run(
        &home,
        &repo,
        &["secret", "import", "API_KEY=sk-live-hunter2-canary"],
    );
    assert_eq!(out.status.code(), Some(1));
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(!all.contains("hunter2"), "{all}");
    assert!(!all.contains("API_KEY=sk-live"), "{all}");
}

#[test]
fn stdin_accepts_64_kib_with_its_newline_and_refuses_more() {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);

    let full = "x".repeat(MAX_VALUE_BYTES);
    let out = run_stdin(
        &home,
        &repo,
        &["secret", "set", "FULL"],
        format!("{full}\n"),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(materialize(&home, &repo, "FULL").len(), MAX_VALUE_BYTES);

    let almost = "y".repeat(MAX_VALUE_BYTES - 1);
    let out = run_stdin(
        &home,
        &repo,
        &["secret", "set", "ALMOST"],
        format!("{almost}\r\n"),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        materialize(&home, &repo, "ALMOST").len(),
        MAX_VALUE_BYTES - 1
    );

    let too_long = "z".repeat(MAX_VALUE_BYTES + 1);
    let out = run_stdin(&home, &repo, &["secret", "set", "TOO_LONG"], too_long);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("at most 64 KiB"), "{}", stderr(&out));
}

#[test]
fn stdin_rejects_invalid_utf8() {
    let home = Home::new();
    let repo = home.make_repo("app");

    let out = run_stdin(
        &home,
        &repo,
        &["secret", "set", "API_KEY"],
        [0xff_u8, 0xfe, b'\n'],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not valid UTF-8"), "{}", stderr(&out));
}

#[test]
fn an_unknown_config_table_warns_once_on_stderr() {
    let home = Home::new();
    let repo = home.make_repo("app");
    fs::write(
        repo.join("hostler.toml"),
        "[project]\nname = \"app\"\n\n[dev]\nport = 3000\n",
    )
    .unwrap();

    let message = "[hostler] hostler.toml: [dev] is not used by this hostler version";
    let out = run(&home, &repo, &["project"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out).matches(message).count(), 1, "{}", stderr(&out));
    assert!(stdout(&out).contains("app"), "{}", stdout(&out));

    // `--json` keeps stdout pure; the warning is still on stderr, once.
    let out = run(&home, &repo, &["project", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).trim_start().starts_with('{'),
        "{}",
        stdout(&out)
    );
    assert_eq!(stderr(&out).matches(message).count(), 1, "{}", stderr(&out));

    // A control character in the table name is replaced before it reaches the terminal.
    fs::write(
        repo.join("hostler.toml"),
        "[project]\nname = \"app\"\n\n[\"a\\u001bb\"]\nport = 1\n",
    )
    .unwrap();
    let out = run(&home, &repo, &["project"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("a\u{FFFD}b"), "{}", stderr(&out));
    assert!(!stderr(&out).contains('\u{1b}'), "{}", stderr(&out));
}
