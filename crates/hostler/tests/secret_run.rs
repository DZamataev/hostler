//! `hostler run --secret` / `--secret-file` against the real binary and a real service (plan P2,
//! Task 10). The command is the `hold` example (its print flags write a variable, a file or a
//! text). The database, the project approval and the values are set up over the socket. No
//! assertion message prints output that may hold a value: `shown` replaces every test value.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::Home;
use hostler::contract::secrets::{SecretSet, Unlock};
use hostler::contract::wire::Request;
use hostler::secrets::value::SecretValue;

const VALUE: &str = "hlxval-7f3a91c2";
const OTHER: &str = "hlxoth-0b5e64d8";
const PASSWORD: &str = "correct horse battery staple";

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

/// `hold` with these arguments.
fn hold(args: &[&str]) -> Vec<OsString> {
    let mut argv = vec![hold_exe().as_os_str().to_owned()];
    argv.extend(args.iter().map(OsString::from));
    argv
}

/// Output text with every test value replaced, fit for an assertion message.
fn shown(out: &Output) -> String {
    let text = format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    [VALUE, OTHER, "abcdef"]
        .iter()
        .fold(text, |t, v| t.replace(v, "<value>"))
}

fn holds(bytes: &[u8], what: &str) -> bool {
    bytes.windows(what.len()).any(|w| w == what.as_bytes())
}

fn assert_clean(out: &Output, values: &[&str]) {
    for v in values {
        assert!(
            !holds(&out.stdout, v) && !holds(&out.stderr, v),
            "a value reached the output:\n{}",
            shown(out)
        );
    }
}

/// A checkout in `home` whose project is approved, with `secrets` stored; the database is
/// created and unlocked first.
fn project(home: &Home, name: &str, secrets: &[(&str, &str)]) -> PathBuf {
    let repo = std::fs::canonicalize(home.make_repo(name)).unwrap();
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .expect("unlock");
    let project = hostler::project::current(&repo).unwrap().project;
    home.call(Request::ProjectApprove(project.clone()))
        .expect("approve");
    for (name, value) in secrets {
        home.call(Request::SecretSet(SecretSet {
            project: project.clone(),
            name: (*name).into(),
            value: SecretValue::new((*value).into()),
        }))
        .expect("set");
    }
    repo
}

/// `hostler run <flags> -- <argv>` in `repo`, output captured.
fn run(home: &Home, repo: &Path, flags: &[&str], argv: Vec<OsString>) -> Command {
    let mut c = home.hostler();
    c.arg("run")
        .args(flags)
        .arg("--")
        .args(argv)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn code(out: &Output) -> Option<i32> {
    out.status.code()
}

fn runs_left(home: &Home) -> usize {
    std::fs::read_dir(home.paths().runs).map_or(0, |d| d.count())
}

fn wait_until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn file_says(path: &Path, what: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(what))
}

// ---- Usage and refusals that need no materialized value ----

#[test]
fn run_needs_a_resource_flag() {
    let home = Home::new();
    let out = home.hostler().args(["run", "--", "true"]).output().unwrap();
    assert_eq!(code(&out), Some(2), "{}", shown(&out));
}

#[test]
fn invalid_secret_name_is_not_echoed() {
    let home = Home::new();
    let repo = home.make_repo("echo");
    let out = run(&home, &repo, &["--secret", VALUE], hold(&["--print", "x"]))
        .output()
        .unwrap();
    assert_eq!(code(&out), Some(2), "{}", shown(&out));
    assert_clean(&out, &[VALUE]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("NAME=ENV"));
}

#[test]
fn duplicate_env_is_a_usage_error() {
    let home = Home::new();
    let repo = home.make_repo("dup");
    let out = run(
        &home,
        &repo,
        &["--secret", "A", "--secret", "B=A"],
        hold(&["--print", "x"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(2), "{}", shown(&out));
}

#[test]
fn outside_a_checkout_refuses_with_78() {
    let home = Home::new();
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        home.dir.path(),
        &["--secret", "API_KEY"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(78), "{}", shown(&out));
    assert!(!marker.exists(), "the command ran");
}

#[test]
fn no_service_refuses_with_78_even_with_heavy() {
    let home = Home::new();
    let repo = home.make_repo("nosvc");
    // A home below a file: no state dir, so no service.
    let blocker = home.dir.path().join("file");
    std::fs::write(&blocker, "").unwrap();
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &["--heavy", "--secret", "API_KEY"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .env("HOSTLER_HOME", blocker.join("home"))
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(78), "{}", shown(&out));
    assert!(!marker.exists(), "the command ran");
}

// ---- These need the service's `secret.materialize` (Task 6) ----

#[test]
fn secret_reaches_the_command_masked() {
    let home = Home::new();
    let repo = project(&home, "basic", &[("API_KEY", VALUE)]);
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&[
            "--print",
            "key=",
            "--print-env",
            "API_KEY",
            "--print",
            " end",
        ]),
    )
    .output()
    .unwrap();
    // `--print-env` panics (exit 101) when the variable is missing.
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"key=*** end", "{}", shown(&out));
    assert_clean(&out, &[VALUE]);
}

#[test]
fn env_name_can_be_given() {
    let home = Home::new();
    let repo = project(&home, "envname", &[("API_KEY", VALUE)]);
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY=SERVICE_TOKEN"],
        hold(&["--print-env", "SERVICE_TOKEN"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"***", "{}", shown(&out));
}

#[test]
fn value_split_across_writes_is_masked() {
    let home = Home::new();
    let repo = project(&home, "slow", &[("API_KEY", VALUE)]);
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&[
            "--slowly",
            "--print",
            "a ",
            "--print-env",
            "API_KEY",
            "--print",
            " b",
        ]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"a *** b", "{}", shown(&out));
}

#[test]
fn stderr_is_masked_too() {
    let home = Home::new();
    let repo = project(&home, "stderr", &[("API_KEY", VALUE)]);
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&["--stderr", "--print-env", "API_KEY"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert!(holds(&out.stderr, "***"), "{}", shown(&out));
    assert_clean(&out, &[VALUE]);
}

#[test]
fn secret_file_has_the_value_and_is_removed_after() {
    let home = Home::new();
    let repo = project(&home, "file", &[("CERT", VALUE)]);
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[
            "--print-file-of",
            "CERT",
            "--print",
            " ",
            "--print-env",
            "CERT",
        ]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let path = stdout
        .strip_prefix("*** ")
        .expect("the masked content, then the path");
    let path = Path::new(path);
    assert!(path.starts_with(home.paths().runs), "{}", shown(&out));
    assert!(!path.exists(), "the secret file is still there");
    assert_eq!(runs_left(&home), 0);
    assert_clean(&out, &[VALUE]);
}

#[test]
fn exit_code_passes_through_with_secrets() {
    let home = Home::new();
    let repo = project(&home, "code", &[("API_KEY", VALUE)]);
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&[marker.to_str().unwrap(), "0", "7"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(7), "{}", shown(&out));
}

#[test]
fn missing_secret_refuses_with_78() {
    let home = Home::new();
    let repo = project(&home, "missing", &[("API_KEY", VALUE)]);
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY", "--secret", "NOT_SET"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(78), "{}", shown(&out));
    assert!(!marker.exists(), "the command ran");
    assert!(String::from_utf8_lossy(&out.stderr).contains("NOT_SET"));
    assert_clean(&out, &[VALUE]);
}

#[test]
fn locked_refuses_with_78() {
    let home = Home::new();
    let repo = project(&home, "locked", &[("API_KEY", VALUE)]);
    home.call(Request::SecretsLock).expect("lock");
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(78), "{}", shown(&out));
    assert!(!marker.exists(), "the command ran");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("hostler unlock"),
        "{}",
        shown(&out)
    );
}

#[test]
fn heavy_and_secret_compose() {
    use hostler::client::Client;
    let home = Home::new();
    let repo = project(&home, "heavy", &[("API_KEY", VALUE)]);
    let marker = home.dir.path().join("ran");
    // A second client that collects every queue event while the run happens.
    let paths = home.paths();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let events = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut client = Client::connect(&paths).await.unwrap();
            let mut seen = Vec::new();
            while stop_rx.try_recv().is_err() {
                if let Ok(Some(event)) =
                    tokio::time::timeout(Duration::from_millis(100), client.next_event()).await
                {
                    seen.push(serde_json::to_string(&event).unwrap());
                }
            }
            seen
        })
    });
    // The command line itself holds the value.
    let child = run(
        &home,
        &repo,
        &["--heavy", "--secret", "API_KEY"],
        hold(&["--print", VALUE, marker.to_str().unwrap(), "1500"]),
    )
    .spawn()
    .unwrap();
    wait_until("the command to start", Duration::from_secs(20), || {
        file_says(&marker, "start")
    });
    let snapshot = home.call(Request::QueueSnapshot).unwrap().to_string();
    assert!(
        snapshot.contains("***") && !snapshot.contains(VALUE),
        "the snapshot's command is not masked"
    );
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"***", "{}", shown(&out));
    stop_tx.send(()).unwrap();
    let seen = events.join().unwrap();
    assert!(!seen.is_empty(), "no queue event arrived");
    assert!(
        seen.iter().all(|e| !e.contains(VALUE)),
        "an event held the value"
    );
    let history = home
        .hostler()
        .args(["queue", "history", "--json"])
        .output()
        .unwrap();
    let history = String::from_utf8_lossy(&history.stdout).into_owned();
    assert!(
        history.contains("***") && !history.contains(VALUE),
        "history is not masked"
    );
    let db = rusqlite::Connection::open(&home.paths().db).unwrap();
    let command: String = db
        .query_row("SELECT command FROM secret_materializations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        command.contains("***") && !command.contains(VALUE),
        "the materialization row is not masked"
    );
}

#[test]
fn cannot_run_message_is_masked() {
    let home = Home::new();
    let repo = project(&home, "cannot", &[("API_KEY", VALUE)]);
    let missing = home.dir.path().join(format!("{VALUE}-missing"));
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        vec![missing.into_os_string(), "arg".into()],
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(127), "{}", shown(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot run") && stderr.contains("***"),
        "{}",
        shown(&out)
    );
    assert_clean(&out, &[VALUE]);
}

#[test]
fn heavy_display_is_masked_before_it_is_cut() {
    const CUT_VALUE: &str = "hlxprefix-7f3a-0123";
    let home = Home::new();
    let repo = project(&home, "cut", &[("API_KEY", CUT_VALUE)]);
    let marker = home.dir.path().join("ran");
    let marker = marker.to_str().unwrap();
    // display = "<hold> --print <text> <marker> 1500"; the value starts at byte 1018.
    let head = format!("{} --print ", hold_exe().display());
    let tail = format!(" {marker} 1500");
    let before = "b".repeat(1018 - head.len());
    let after = "a".repeat(3000 - 1018 - CUT_VALUE.len() - tail.len());
    let text = format!("{before}{CUT_VALUE}{after}");
    let child = run(
        &home,
        &repo,
        &["--heavy", "--secret", "API_KEY"],
        hold(&["--print", &text, marker, "1500"]),
    )
    .spawn()
    .unwrap();
    wait_until("the command to start", Duration::from_secs(20), || {
        file_says(Path::new(marker), "start")
    });
    let snapshot = home.call(Request::QueueSnapshot).unwrap().to_string();
    assert!(
        !snapshot.contains(CUT_VALUE) && !snapshot.contains(&CUT_VALUE[..6]),
        "the value or its first bytes survived the cut"
    );
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_clean(&out, &[CUT_VALUE]);
}

#[test]
fn overlong_command_sends_a_placeholder() {
    let home = Home::new();
    let repo = project(&home, "long", &[("API_KEY", VALUE)]);
    let head = format!("{} --print ", hold_exe().display());
    let text = "y".repeat(20000 - head.len());
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&["--print", &text]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    let db = rusqlite::Connection::open(&home.paths().db).unwrap();
    let command: String = db
        .query_row("SELECT command FROM secret_materializations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(command, "<command of 20000 bytes>");
}

#[test]
fn nested_run_masks_both_levels() {
    let home = Home::new();
    let repo = project(&home, "nested", &[("OUTER", VALUE), ("INNER", OTHER)]);
    let mut inner = vec![
        OsString::from(common::BIN),
        "run".into(),
        "--secret".into(),
        "INNER".into(),
        "--".into(),
    ];
    inner.extend(hold(&[
        "--print-env",
        "OUTER",
        "--print",
        " ",
        "--print-env",
        "INNER",
    ]));
    let out = run(&home, &repo, &["--secret", "OUTER"], inner)
        .output()
        .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"*** ***", "{}", shown(&out));
    assert_clean(&out, &[VALUE, OTHER]);
}

#[test]
fn stdin_still_reaches_the_command() {
    use std::io::Write;
    let home = Home::new();
    let repo = project(&home, "stdin", &[("API_KEY", VALUE)]);
    let mut child = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        hold(&["--echo-stdin"]),
    )
    .stdin(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("hello {VALUE}\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"hello ***\n", "{}", shown(&out));
}

#[test]
fn killed_wrapper_leaves_files_that_are_swept() {
    let home = Home::new();
    let repo = project(&home, "killed", &[("CERT", VALUE)]);
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "1000"]),
    )
    .env("HOSTLER_TEST_KILL_AFTER_SPAWN", "1")
    .output()
    .unwrap();
    assert_ne!(code(&out), Some(0), "{}", shown(&out));
    // The command runs on with its file.
    assert_eq!(runs_left(&home), 1);
    wait_until("the command to end", Duration::from_secs(20), || {
        file_says(&marker, "end")
    });
    let again = home.dir.path().join("again");
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[again.to_str().unwrap(), "0"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
}

#[test]
fn killed_before_gate_never_runs_and_is_swept() {
    let home = Home::new();
    let repo = project(&home, "beforegate", &[("CERT", VALUE)]);
    let marker = home.dir.path().join("ran");
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .env("HOSTLER_TEST_KILL_BEFORE_GATE", "1")
    .output()
    .unwrap();
    assert_ne!(code(&out), Some(0), "{}", shown(&out));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!marker.exists(), "the command ran");
    assert_eq!(runs_left(&home), 1);
    let again = home.dir.path().join("again");
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[again.to_str().unwrap(), "0"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
}

#[test]
fn full_downstream_pipe_does_not_block_exit_or_cleanup() {
    use std::io::Read;
    let home = Home::new();
    let repo = project(&home, "full", &[("CERT", VALUE)]);
    let mut child = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&["--print-file-of", "CERT", "--fill", "262144"]),
    )
    .env("HOSTLER_TEST_DRAIN_GRACE_MS", "300")
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    // Nobody reads stdout while the wrapper runs.
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the wrapper hangs on a full stdout"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
    assert_eq!(runs_left(&home), 0);
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    assert!(stdout.starts_with(b"***"), "the masked value is not first");
    assert!(!holds(&stdout, VALUE), "a value reached the output");
}

#[cfg(unix)]
#[test]
fn background_writer_does_not_hang_the_wrapper() {
    let home = Home::new();
    let repo = project(&home, "background", &[("API_KEY", VALUE)]);
    let marker = home.dir.path().join("bg");
    let line = format!(
        "'{}' '{}' 10000 & echo done",
        hold_exe().display(),
        marker.display()
    );
    let started = Instant::now();
    let out = run(&home, &repo, &["--secret", "API_KEY"], vec![line.into()])
        .output()
        .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"done\n", "{}", shown(&out));
    // DRAIN_GRACE (2 s) + startup, far below the background hold's 10 s.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn suffix_after_the_drain_deadline_is_discarded() {
    let home = Home::new();
    let repo = project(&home, "suffix", &[("API_KEY", "abcdef")]);
    let out = run(
        &home,
        &repo,
        &["--secret", "API_KEY"],
        vec!["(printf abc; sleep 1; printf def) & exit 0".into()],
    )
    .env("HOSTLER_TEST_DRAIN_GRACE_MS", "300")
    .env("HOSTLER_TEST_PAUSE_AFTER_SEAL_MS", "2000")
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert!(holds(&out.stdout, "***"), "{}", shown(&out));
    assert!(
        !holds(&out.stdout, "abc") && !holds(&out.stdout, "def"),
        "{}",
        shown(&out)
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_removes_secret_files() {
    use std::os::unix::process::CommandExt;
    let home = Home::new();
    let repo = project(&home, "ctrlc", &[("CERT", VALUE)]);
    let marker = home.dir.path().join("ran");
    let child = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "20000"]),
    )
    .process_group(0)
    .spawn()
    .unwrap();
    wait_until("the command to start", Duration::from_secs(20), || {
        file_says(&marker, "start")
    });
    assert_eq!(runs_left(&home), 1);
    // SAFETY: killpg has no memory preconditions; the group is the wrapper's own.
    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGINT) };
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), Some(130), "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
    assert!(
        !file_says(&marker, "end"),
        "the command was not interrupted"
    );
}

#[cfg(unix)]
#[test]
fn sigterm_while_queued_withdraws_and_removes_files() {
    let home = Home::new();
    let repo = project(&home, "queued", &[("CERT", VALUE)]);
    let busy = home.dir.path().join("busy");
    let holder = run(
        &home,
        &repo,
        &["--heavy"],
        hold(&[busy.to_str().unwrap(), "15000"]),
    )
    .spawn()
    .unwrap();
    wait_until("the slot to be taken", Duration::from_secs(20), || {
        file_says(&busy, "start")
    });
    let marker = home.dir.path().join("ran");
    let waiter = run(
        &home,
        &repo,
        &["--heavy", "--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .spawn()
    .unwrap();
    wait_until("the waiter's secret file", Duration::from_secs(20), || {
        runs_left(&home) == 1
    });
    std::thread::sleep(Duration::from_millis(500));
    // SAFETY: kill has no memory preconditions; the pid is our unreaped child.
    unsafe { libc::kill(waiter.id() as libc::pid_t, libc::SIGTERM) };
    let started = Instant::now();
    let out = waiter.wait_with_output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "SIGTERM did not stop the wait"
    );
    assert_eq!(code(&out), Some(130), "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
    assert!(!marker.exists(), "the command ran");
    // SAFETY: as above, for the slot holder.
    unsafe { libc::kill(holder.id() as libc::pid_t, libc::SIGINT) };
    let _ = holder.wait_with_output();
}

// ---- Fix round 1 (T10-1, T10-2, T10-3, T10-6) ----

#[test]
fn dash_prefixed_secret_values_are_not_echoed() {
    let home = Home::new();
    let repo = home.make_repo("dash");
    let dashed = format!("--{VALUE}");
    for flag in ["--secret", "--secret-file"] {
        let joined = format!("{flag}={dashed}");
        for flags in [vec![flag, dashed.as_str()], vec![joined.as_str()]] {
            let out = run(&home, &repo, &flags, hold(&["--print", "x"]))
                .output()
                .unwrap();
            assert_eq!(code(&out), Some(2), "{}", shown(&out));
            assert_clean(&out, &[VALUE]);
        }
    }
}

/// The pid of the wrapper that owns the only run directory.
#[cfg(unix)]
fn run_dir_wrapper(home: &Home) -> libc::pid_t {
    let dir = std::fs::read_dir(home.paths().runs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut owner = None;
    wait_until("the owner file", Duration::from_secs(10), || {
        owner = std::fs::read(dir.join("owner.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        owner.is_some()
    });
    owner.unwrap()["wrapper"]["pid"].as_i64().unwrap() as libc::pid_t
}

/// `hostler run --heavy -- hostler run --heavy <inner flags> -- <argv>`: the inner run is
/// re-entrant.
fn nested_heavy(home: &Home, repo: &Path, inner: &[&str], argv: Vec<OsString>) -> Command {
    let mut command = vec![OsString::from(common::BIN), "run".into(), "--heavy".into()];
    command.extend(inner.iter().map(OsString::from));
    command.push("--".into());
    command.extend(argv);
    run(home, repo, &["--heavy"], command)
}

#[cfg(unix)]
#[test]
fn sigint_during_materialization_withdraws_at_once() {
    let home = Home::new();
    let repo = project(&home, "early", &[("CERT", VALUE)]);
    let busy = home.dir.path().join("busy");
    let holder = run(
        &home,
        &repo,
        &["--heavy"],
        hold(&[busy.to_str().unwrap(), "20000"]),
    )
    .spawn()
    .unwrap();
    wait_until("the slot to be taken", Duration::from_secs(20), || {
        file_says(&busy, "start")
    });
    let marker = home.dir.path().join("ran");
    let waiter = run(
        &home,
        &repo,
        &["--heavy", "--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .env("HOSTLER_TEST_PAUSE_AFTER_MATERIALIZE_MS", "2000")
    .spawn()
    .unwrap();
    // The files exist and the wrapper still pauses: the SIGINT comes before the queue wait.
    wait_until("the waiter's secret file", Duration::from_secs(20), || {
        runs_left(&home) == 1
    });
    // SAFETY: kill has no memory preconditions; the pid is our unreaped child.
    unsafe { libc::kill(waiter.id() as libc::pid_t, libc::SIGINT) };
    let started = Instant::now();
    let out = waiter.wait_with_output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the early interrupt did not withdraw the request: {:?}",
        started.elapsed()
    );
    assert_eq!(code(&out), Some(130), "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
    assert!(!marker.exists(), "the command ran");
    // SAFETY: as above, for the slot holder.
    unsafe { libc::kill(holder.id() as libc::pid_t, libc::SIGINT) };
    let _ = holder.wait_with_output();
}

#[cfg(unix)]
#[test]
fn reentrant_interrupt_before_spawn_never_runs() {
    let home = Home::new();
    let repo = project(&home, "reint", &[("CERT", VALUE)]);
    let marker = home.dir.path().join("ran");
    let outer = nested_heavy(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .env("HOSTLER_TEST_PAUSE_AFTER_MATERIALIZE_MS", "2000")
    .spawn()
    .unwrap();
    wait_until("the inner secret file", Duration::from_secs(20), || {
        runs_left(&home) == 1
    });
    // SAFETY: kill has no memory preconditions; the inner wrapper is alive (it pauses).
    unsafe { libc::kill(run_dir_wrapper(&home), libc::SIGINT) };
    let out = outer.wait_with_output().unwrap();
    assert_ne!(code(&out), Some(0), "{}", shown(&out));
    assert!(!marker.exists(), "the command ran after the interrupt");
    assert_eq!(runs_left(&home), 0);
}

#[cfg(unix)]
#[test]
fn reentrant_sigterm_to_the_wrapper_alone_stops_the_command() {
    let home = Home::new();
    let repo = project(&home, "reterm", &[("CERT", VALUE)]);
    let marker = home.dir.path().join("ran");
    let outer = nested_heavy(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "20000"]),
    )
    .spawn()
    .unwrap();
    wait_until("the command to start", Duration::from_secs(20), || {
        file_says(&marker, "start")
    });
    // SAFETY: kill has no memory preconditions; the inner wrapper waits for its command.
    unsafe { libc::kill(run_dir_wrapper(&home), libc::SIGTERM) };
    let started = Instant::now();
    let out = outer.wait_with_output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "SIGTERM was not forwarded: {:?}",
        started.elapsed()
    );
    assert_eq!(code(&out), Some(128 + libc::SIGTERM), "{}", shown(&out));
    assert!(!file_says(&marker, "end"), "the command ran on");
    assert_eq!(runs_left(&home), 0);
}

#[test]
fn reentrant_kill_before_record_never_runs_and_is_swept() {
    let home = Home::new();
    let repo = project(&home, "rerecord", &[("CERT", VALUE)]);
    let marker = home.dir.path().join("ran");
    let out = nested_heavy(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .env("HOSTLER_TEST_KILL_BEFORE_RECORD", "1")
    .output()
    .unwrap();
    assert_ne!(code(&out), Some(0), "{}", shown(&out));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!marker.exists(), "the command ran before it was recorded");
    assert_eq!(runs_left(&home), 1);
    let again = home.dir.path().join("again");
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&[again.to_str().unwrap(), "0"]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
}

#[cfg(unix)]
#[test]
fn heavy_release_does_not_delay_the_drain_deadline() {
    let home = Home::new();
    let repo = project(&home, "release", &[("API_KEY", VALUE)]);
    let out = run(
        &home,
        &repo,
        &["--heavy", "--secret", "API_KEY"],
        vec!["(sleep 1; printf late) & printf early; exit 0".into()],
    )
    .env("HOSTLER_TEST_DRAIN_GRACE_MS", "300")
    .env("HOSTLER_TEST_DELAY_RELEASE_MS", "3000")
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"early", "{}", shown(&out));
}

// ---- Clean read (CR-1, CR-2, CR-3) ----

const INSIDE: &str = "<command inside a secret run>";

#[cfg(unix)]
/// Files under the home whose bytes hold `what` (the database, its WAL, the service log, …).
fn files_holding(home: &Home, what: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, what: &str, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, what, found);
            } else if std::fs::read(&path).is_ok_and(|b| holds(&b, what)) {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&home.home, what, &mut found);
    found
}

fn history_json(home: &Home) -> String {
    let out = home
        .hostler()
        .args(["queue", "history", "--json"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(unix)]
fn materialized_commands(home: &Home) -> Vec<String> {
    let db = rusqlite::Connection::open(home.paths().db).unwrap();
    let mut query = db
        .prepare("SELECT command FROM secret_materializations")
        .unwrap();
    query
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[cfg(unix)]
#[test]
fn nested_heavy_run_never_records_an_inherited_value() {
    let home = Home::new();
    let repo = project(&home, "inherit", &[("TOKEN", VALUE)]);
    // The reviewer's command: the inner run sees the expanded value in its own argv.
    let line = format!("'{}' run --heavy -- printf '%s' \"$TOKEN\"", common::BIN);
    let out = run(&home, &repo, &["--secret", "TOKEN"], vec![line.into()])
        .output()
        .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"***", "{}", shown(&out));
    let history = history_json(&home);
    assert!(history.contains(INSIDE), "the inner run is not in history");
    assert!(!history.contains(VALUE), "queue history holds the value");
    assert_eq!(files_holding(&home, VALUE), Vec::<PathBuf>::new());
}

#[cfg(unix)]
#[test]
fn nested_run_with_its_own_secret_records_no_inherited_value() {
    let home = Home::new();
    let repo = project(&home, "inherit2", &[("TOKEN", VALUE), ("OTHER", OTHER)]);
    // The outer shell expands $TOKEN into the inner command line; the inner shell, $OTHER.
    let line = format!(
        r#"'{}' run --heavy --secret OTHER -- "printf '%s %s' $TOKEN \"\$OTHER\"""#,
        common::BIN
    );
    let out = run(&home, &repo, &["--secret", "TOKEN"], vec![line.into()])
        .output()
        .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"*** ***", "{}", shown(&out));
    let commands = materialized_commands(&home);
    assert_eq!(commands.len(), 2);
    assert!(commands.iter().any(|c| c == INSIDE), "{commands:?}");
    let history = history_json(&home);
    assert!(history.contains(INSIDE) && !history.contains(VALUE) && !history.contains(OTHER));
    for value in [VALUE, OTHER] {
        assert_eq!(files_holding(&home, value), Vec::<PathBuf>::new());
    }
}

#[test]
fn plain_heavy_run_still_records_its_command() {
    let home = Home::new();
    let marker = "plainmark-3c1f";
    let out = run(
        &home,
        home.dir.path(),
        &["--heavy"],
        hold(&["--print", marker]),
    )
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    let history = history_json(&home);
    assert!(
        history.contains(marker) && !history.contains(INSIDE),
        "{history}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn gate_holding_values_is_not_dumpable() {
    use std::os::unix::fs::MetadataExt;
    let home = Home::new();
    let repo = project(&home, "dump", &[("CERT", VALUE)]);
    // The gate reports PR_GET_DUMPABLE before it waits (also as root, where /proc ownership
    // says nothing).
    let report = home.dir.path().join("dumpable");
    let out = run(
        &home,
        &repo,
        &["--secret-file", "CERT"],
        hold(&["--print", "x"]),
    )
    .env("HOSTLER_TEST_GATE_DUMPABLE_TO", &report)
    .output()
    .unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(std::fs::read_to_string(&report).unwrap(), "0");
    // SAFETY: geteuid takes no arguments and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    // Not root: while the gate waits for `heavy.started`, its /proc files are root's.
    let marker = home.dir.path().join("ran");
    let child = run(
        &home,
        &repo,
        &["--heavy", "--secret-file", "CERT"],
        hold(&[marker.to_str().unwrap(), "0"]),
    )
    .env("HOSTLER_TEST_DELAY_STARTED_MS", "3000")
    .spawn()
    .unwrap();
    let mut gate = None;
    wait_until("the recorded gate", Duration::from_secs(20), || {
        gate = std::fs::read_dir(home.paths().runs)
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| std::fs::read(e.ok()?.path().join("owner.json")).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|o| o["child"]["pid"].as_u64());
        gate.is_some()
    });
    let owner = std::fs::metadata(format!("/proc/{}/environ", gate.unwrap()))
        .unwrap()
        .uid();
    assert_eq!(owner, 0, "the waiting gate is dumpable");
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
}

#[test]
fn device_names_work_as_secret_files() {
    let home = Home::new();
    let names = ["NUL", "CON", "COM1", "AUX"];
    let secrets: Vec<(&str, &str)> = names.iter().map(|n| (*n, VALUE)).collect();
    let repo = project(&home, "devices", &secrets);
    let mut flags = Vec::new();
    let mut print = Vec::new();
    for name in names {
        flags.extend(["--secret-file", name]);
        print.extend(["--print-file-of", name, "--print", " "]);
    }
    let out = run(&home, &repo, &flags, hold(&print)).output().unwrap();
    assert_eq!(code(&out), Some(0), "{}", shown(&out));
    assert_eq!(out.stdout, b"*** *** *** *** ", "{}", shown(&out));
    assert_eq!(runs_left(&home), 0);
}
