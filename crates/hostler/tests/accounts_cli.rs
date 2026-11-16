//! Task 9's account commands against the real binary and a real service (plan P3a):
//! `account list | set | remove`, the server's claim checks for `account.*`, the KeePassXC
//! fixture and an account leak canary. Assertion messages never print a canary.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::client::Client;
use hostler::contract::accounts::{AccountMaterialize, MaterializedAccount};
use hostler::contract::claims::{ClaimOutcome, ClaimRequest, Purpose, ResourceId, Selector};
use hostler::contract::secrets::{ProjectInfo, ProjectRef, Unlock};
use hostler::contract::wire::{Event, Request};
use hostler::contract::{ErrorCode, HostlerError};
use hostler::procs::{ProcessIdentity, SysProcessTable, current_identity};
use hostler::secrets::value::{RawCommand, SecretValue};
use serde_json::Value;

const PASSWORD: &str = "master-pass-1";
const CANARY: &str = "hlx-acct-canary-3c9d";
const CANARY_2: &str = "hlx-acct-env-81fe";

fn me() -> ProcessIdentity {
    current_identity(&SysProcessTable::new().unwrap()).unwrap()
}

fn run(home: &Home, repo: &Path, args: &[&str]) -> Output {
    home.hostler()
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn run_stdin(home: &Home, repo: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = home
        .hostler()
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// `hostler account set NAME --field FIELD` with the value on stdin.
fn set(home: &Home, repo: &Path, name: &str, field: &str, value: &str) -> Output {
    let out = run_stdin(
        home,
        repo,
        &["account", "set", name, "--field", field],
        value,
    );
    // Never print the value, even when the command failed.
    assert!(
        out.status.success(),
        "account set {name} --field {field} failed"
    );
    out
}

/// A checkout whose project is approved, in a home with an unlocked new database.
fn approved_repo(home: &Home, name: &str) -> PathBuf {
    let repo = std::fs::canonicalize(home.make_repo(name)).unwrap();
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    repo
}

fn project_ref(repo: &Path) -> ProjectRef {
    hostler::project::current(repo).unwrap().project
}

fn project_id(home: &Home, repo: &Path) -> String {
    let info: ProjectInfo = serde_json::from_value(
        home.call(Request::ProjectResolve(project_ref(repo)))
            .unwrap(),
    )
    .unwrap();
    info.id
}

fn account_request(home: &Home, repo: &Path, name: &str, purpose: Purpose) -> ClaimRequest {
    ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector: Selector::Exact {
            resource: ResourceId::account(&project_id(home, repo), name),
        },
        workspace_id: home.workspace(repo).unwrap().id,
        requester: me(),
        purpose,
        reentrant_tokens: Vec::new(),
        no_wait: false,
    }
}

/// A granted claim on account `name` of `repo`'s project; its id.
fn claim(home: &Home, repo: &Path, name: &str, purpose: Purpose) -> String {
    let request = account_request(home, repo, name, purpose);
    let outcome: ClaimOutcome =
        serde_json::from_value(home.call(Request::ClaimsAcquire(request)).unwrap()).unwrap();
    match outcome {
        ClaimOutcome::Granted { claim } => claim.id,
        other => panic!("not granted: {other:?}"),
    }
}

fn ttl_hold() -> Purpose {
    Purpose::Hold {
        ttl_ms: Some(30 * 60 * 1000),
        bound_to: None,
        distinct: false,
    }
}

fn lease() -> Purpose {
    Purpose::Lease {
        command: "<command pending>".into(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn materialize_request(
    repo: &Path,
    claim_id: &str,
    requester: ProcessIdentity,
    command: &str,
) -> AccountMaterialize {
    AccountMaterialize {
        run_id: uuid::Uuid::new_v4().to_string(),
        claim_id: claim_id.into(),
        requester,
        project: project_ref(repo),
        name: "qa-user".into(),
        command: RawCommand::new(command.into()),
        workspace: hostler::project::display_path(repo),
    }
}

fn materialize(home: &Home, m: AccountMaterialize) -> Result<MaterializedAccount, HostlerError> {
    home.start_service();
    let paths = home.paths();
    runtime().block_on(async {
        Client::connect(&paths)
            .await?
            .call_sensitive(Request::AccountMaterialize(m))
            .await
    })
}

fn envelope(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("not JSON: {}", stdout(out)))
}

/// The `hold` example: prints an environment variable, portable to Windows.
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

fn kdbx_key() -> keepass::DatabaseKey {
    keepass::DatabaseKey::new().with_password(PASSWORD)
}

/// Runs `edit` on the entry titled `title` in the vault, then saves the file. A person editing it
/// in KeePassXC does the same; hostler re-reads the changed file.
fn edit_entry(home: &Home, title: &str, edit: impl FnOnce(&mut keepass::db::EntryMut<'_>)) {
    let path = home.paths().kdbx;
    let key = kdbx_key();
    let mut db = keepass::Database::open(&mut std::fs::File::open(&path).unwrap(), key.clone())
        .expect("the vault");
    let mut found = None;
    for group in db.root().groups() {
        for entry in group.entries() {
            if entry.get("Title") == Some(title) {
                found = Some((group.id(), entry.id()));
            }
        }
    }
    let (group_id, entry_id) = found.unwrap_or_else(|| panic!("no entry titled {title}"));
    edit(
        &mut db
            .root_mut()
            .group_mut(group_id)
            .expect("the group")
            .entry_mut(entry_id)
            .expect("the entry"),
    );
    let mut file = std::fs::File::create(&path).unwrap();
    db.save(&mut file, key).expect("saving the vault");
}

/// An entry's custom fields as `(key, value)`, and its tags.
type EntryData = (Vec<(String, String)>, Vec<String>);

fn entry_data(home: &Home, title: &str) -> Option<EntryData> {
    let db = keepass::Database::open(
        &mut std::fs::File::open(&home.paths().kdbx).unwrap(),
        kdbx_key(),
    )
    .expect("the vault");
    let mut found = None;
    for group in db.root().groups() {
        for entry in group.entries() {
            if entry.get("Title") == Some(title) {
                found = Some((
                    entry
                        .fields
                        .iter()
                        .map(|(key, value)| (key.clone(), value.as_str().to_owned()))
                        .collect(),
                    entry.tags.clone(),
                ));
            }
        }
    }
    found
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn assert_no_canary(what: &str, bytes: &[u8]) {
    for canary in [CANARY, CANARY_2] {
        assert!(!holds(bytes, canary), "{what} holds a canary");
    }
}

#[test]
fn set_from_stdin_then_list_shows_fields_and_no_values() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    let first = set(&home, &repo, "qa-user", "session", CANARY);
    assert_eq!(stdout(&first).trim(), "stored qa-user.session");
    set(&home, &repo, "qa-user", "env", &format!("{CANARY_2}\n"));

    let human = run(&home, &repo, &["account", "list"]);
    assert!(human.status.success(), "{}", stderr(&human));
    let text = stdout(&human);
    let row = text.lines().find(|l| l.starts_with("qa-user")).unwrap();
    assert!(
        row.contains("env, session") && row.trim_end().ends_with('-'),
        "{text}"
    );

    let json = run(&home, &repo, &["account", "list", "--json"]);
    assert!(json.status.success(), "{}", stderr(&json));
    let v = envelope(&json);
    assert_eq!(v["ok"], true);
    assert_eq!(
        v["data"],
        serde_json::json!([{ "name": "qa-user", "fields": ["env", "session"], "holder": null }])
    );
    for out in [&first, &human, &json] {
        assert_no_canary("account set/list", &out.stdout);
        assert_no_canary("account set/list", &out.stderr);
    }

    // The trailing newline was stripped: the value is the canary exactly.
    let id = claim(&home, &repo, "qa-user", lease());
    let got = materialize(&home, materialize_request(&repo, &id, me(), "make")).unwrap();
    let env = got.fields.iter().find(|f| f.name == "env").unwrap();
    assert!(env.value.expose() == CANARY_2);

    let removed = run(
        &home,
        &repo,
        &["account", "remove", "qa-user", "--field", "env"],
    );
    assert!(
        !removed.status.success(),
        "a claimed account is not removed"
    );
    home.call(Request::ClaimsRelease(
        hostler::contract::claims::ClaimRelease {
            claim_id: id,
            workspace_id: home.workspace(&repo).unwrap().id,
            requester: me(),
            exit_code: Some(0),
        },
    ))
    .unwrap();
    let removed = run(
        &home,
        &repo,
        &["account", "remove", "qa-user", "--field", "env"],
    );
    assert_eq!(stdout(&removed).trim(), "removed", "{}", stderr(&removed));
    let removed = run(&home, &repo, &["account", "remove", "qa-user", "--json"]);
    assert_eq!(envelope(&removed)["data"]["removed"], true);
    let removed = run(&home, &repo, &["account", "remove", "qa-user"]);
    assert_eq!(stdout(&removed).trim(), "not set");
}

#[test]
fn set_value_as_argument_is_refused_without_echo() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let flag = format!("--{CANARY}");
    for args in [
        vec!["account", "set", "qa", "--field", "s", CANARY],
        vec!["account", "set", "qa", "--field", "s", &flag],
        vec!["account", "set", "qa", CANARY, "--field", "s"],
        vec!["account", "set", "qa", &flag, "--field", "s"],
        vec!["account", "set", "qa", &format!("--field={CANARY}")],
        vec!["account", "set", "BAD NAME", "--field", "s"],
        vec!["account", "remove", "BAD NAME", "--field", "s"],
    ] {
        let out = run(&home, &repo, &args);
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert_no_canary("account set refusal", &out.stdout);
        assert_no_canary("account set refusal", &out.stderr);
        assert!(!holds(&out.stderr, "BAD NAME"), "{}", stderr(&out));
    }
    // An empty stdin is refused, never stored.
    let out = home
        .hostler()
        .args(["account", "set", "qa", "--field", "s"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
}

#[test]
fn remove_refused_while_held() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    set(&home, &repo, "qa-user", "session", CANARY);
    let id = claim(&home, &repo, "qa-user", ttl_hold());
    for args in [
        &["account", "remove", "qa-user", "--json"][..],
        &[
            "account", "remove", "qa-user", "--field", "session", "--json",
        ],
    ] {
        let out = run(&home, &repo, args);
        assert_eq!(out.status.code(), Some(1));
        let v = envelope(&out);
        assert_eq!(v["error"]["code"], "resource_in_use", "{v}");
        assert_eq!(
            v["error"]["details"]["holder_path"],
            hostler::project::display_path(&repo)
        );
    }
    let out = run(&home, &repo, &["account", "remove", "qa-user"]);
    assert!(stderr(&out).contains("is claimed by"), "{}", stderr(&out));
    // Still there.
    let got = materialize(&home, materialize_request(&repo, &id, me(), "make")).unwrap();
    assert_eq!(got.fields.len(), 1);
}

#[test]
fn materialize_requires_the_claim() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    set(&home, &repo, "qa-user", "session", CANARY);
    set(&home, &repo, "other", "session", CANARY_2);
    let id = claim(&home, &repo, "qa-user", lease());
    let other = claim(&home, &repo, "other", lease());

    let mut stranger = me();
    stranger.start_time += 1;
    let mut wrong_account = materialize_request(&repo, &other, me(), "make");
    wrong_account.name = "qa-user".into();
    for (case, request) in [
        (
            "unknown claim",
            materialize_request(&repo, "no-such-claim", me(), "make"),
        ),
        (
            "another requester",
            materialize_request(&repo, &id, stranger, "make"),
        ),
        ("a claim on another account", wrong_account),
    ] {
        let e = materialize(&home, request).unwrap_err();
        assert_eq!(e.code, ErrorCode::ClaimNotHeld, "{case}: {e:?}");
    }
    let history = rusqlite::Connection::open(home.paths().db)
        .unwrap()
        .query_row("SELECT count(*) FROM secret_materializations", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(history, 0, "a refused run records nothing");

    let got = materialize(&home, materialize_request(&repo, &id, me(), "make")).unwrap();
    assert_eq!(got.name, "qa-user");
    assert!(got.fields[0].value.expose() == CANARY);

    // `account.check` names the fields and the holder, never a value.
    let check = home
        .call(Request::AccountCheck {
            project: project_ref(&repo),
            name: "qa-user".into(),
        })
        .unwrap();
    assert_eq!(check["fields"], serde_json::json!(["session"]));
    assert_eq!(check["holder"]["id"], id);
    assert_no_canary("account.check", check.to_string().as_bytes());
    let missing = home
        .call(Request::AccountCheck {
            project: project_ref(&repo),
            name: "nobody".into(),
        })
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::AccountMissing);
}

#[test]
fn list_shows_the_holder_path() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    set(&home, &repo, "qa-user", "session", CANARY);
    set(&home, &repo, "admin", "session", CANARY_2);
    let id = claim(&home, &repo, "qa-user", ttl_hold());
    let path = hostler::project::display_path(&repo);

    let json = envelope(&run(&home, &repo, &["account", "list", "--json"]));
    let accounts = json["data"].as_array().unwrap();
    assert_eq!(accounts[0]["name"], "admin");
    assert!(accounts[0]["holder"].is_null());
    assert_eq!(accounts[1]["holder"]["id"], id);
    assert_eq!(accounts[1]["holder"]["workspace_path"], path);
    assert_eq!(accounts[1]["holder"]["kind"], "hold");

    let human = stdout(&run(&home, &repo, &["account", "list"]));
    let row = human.lines().find(|l| l.starts_with("qa-user")).unwrap();
    assert!(row.ends_with(&format!("{path} (hold)")), "{human}");
}

/// `hostler service run` as a child, with test hook environment.
fn service(home: &Home, env: &[(&str, &str)]) -> Child {
    let child = home
        .hostler()
        .args(["service", "run"])
        .envs(env.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !home
        .hostler()
        .args(["service", "status"])
        .output()
        .unwrap()
        .status
        .success()
    {
        assert!(Instant::now() < deadline, "the service never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}

/// The claims lock is released before the vault works: while an `account.remove` (which takes
/// it for `holder_of`) waits inside a paused vault save, `claims.snapshot` answers at once.
#[test]
fn claims_lock_not_held_across_the_vault() {
    let home = Home::new();
    let mut child = service(&home, &[("HOSTLER_TEST_VAULT_PAUSE", "before_place:3000")]);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let repo = std::fs::canonicalize(home.make_repo("app")).unwrap();
    home.call(Request::ProjectApprove(project_ref(&repo)))
        .unwrap();
    set(&home, &repo, "qa-user", "session", CANARY);

    let removing = {
        let mut command = home.hostler();
        let repo = repo.clone();
        std::thread::spawn(move || {
            command
                .args(["account", "remove", "qa-user"])
                .current_dir(repo)
                .output()
                .unwrap()
        })
    };
    std::thread::sleep(Duration::from_millis(1000));
    let paths = home.paths();
    let elapsed = runtime().block_on(async {
        let mut client = Client::connect(&paths).await.unwrap();
        let started = Instant::now();
        client.call(Request::ClaimsSnapshot).await.unwrap();
        started.elapsed()
    });
    assert!(elapsed < Duration::from_millis(100), "{elapsed:?}");
    let removed = removing.join().unwrap();
    assert_eq!(stdout(&removed).trim(), "removed", "{}", stderr(&removed));
    assert!(
        home.hostler()
            .args(["service", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(child.wait().unwrap().success());
}

/// The planner's file, written by keepass 0.17 and read by KeePassXC 2.7.10.
#[test]
fn fixture_with_tagged_entry_lists() {
    let home = Home::new();
    let paths = home.paths();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/kdbx/account-fields.kdbx"
        ),
        &paths.kdbx,
    )
    .unwrap();
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new("pw-acct-1".into()),
        key_file: None,
        forget_key_file: false,
        create: false,
    }))
    .unwrap();
    let repo = std::fs::canonicalize(home.make_repo("app")).unwrap();
    home.call(Request::ProjectApprove(project_ref(&repo)))
        .unwrap();

    // Map the project to the file's group `proj`, as a person's KeePassXC group would be.
    let db = keepass::Database::open(
        &mut std::fs::File::open(&paths.kdbx).unwrap(),
        keepass::DatabaseKey::new().with_password("pw-acct-1"),
    )
    .unwrap();
    let group = db
        .root()
        .groups()
        .find(|g| g.name == "proj")
        .expect("the fixture's group")
        .id()
        .uuid()
        .to_string();
    rusqlite::Connection::open(&paths.db)
        .unwrap()
        .execute(
            "UPDATE projects SET group_uuid = ?1 WHERE id = ?2",
            [&group, &project_id(&home, &repo)],
        )
        .unwrap();

    let listed = run(&home, &repo, &["account", "list", "--json"]);
    assert_eq!(
        envelope(&listed)["data"],
        serde_json::json!([{ "name": "qa-user", "fields": ["env", "session"], "holder": null }]),
        "{}",
        stderr(&listed)
    );
    let id = claim(&home, &repo, "qa-user", lease());
    let got = materialize(&home, materialize_request(&repo, &id, me(), "make")).unwrap();
    let values: Vec<(&str, &str)> = got
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.value.expose()))
        .collect();
    assert_eq!(
        values,
        vec![("env", "stage"), ("session", "sess-value-1234")]
    );
}

/// Every file under `dir`, recursively, with its bytes.
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

/// The account leak canary: a person stores two fields, an agent lists, checks, claims and
/// materializes them with the values in its command line, and every output, event, history row,
/// log and database file is searched for them.
#[test]
fn account_values_leak_nowhere() {
    let home = Home::new();
    let repo = approved_repo(&home, "canary");
    let paths = home.paths();

    // A client that sees every event of the run below.
    let (events_tx, events_rx) = std::sync::mpsc::channel::<Vec<Event>>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let watcher = {
        let paths = paths.clone();
        std::thread::spawn(move || {
            runtime().block_on(async {
                let mut client = Client::connect(&paths).await.unwrap();
                let mut seen = Vec::new();
                tokio::pin!(stop_rx);
                loop {
                    tokio::select! {
                        event = client.next_event() => match event {
                            Some(event) => seen.push(event),
                            None => break,
                        },
                        _ = &mut stop_rx => break,
                    }
                }
                events_tx.send(seen).unwrap();
            })
        })
    };

    let mut outputs: Vec<(&str, Output)> = Vec::new();
    outputs.push((
        "account set session",
        set(&home, &repo, "qa-user", "session", CANARY),
    ));
    outputs.push((
        "account set env",
        set(&home, &repo, "qa-user", "env", CANARY_2),
    ));
    for args in [
        &["account", "list"][..],
        &["account", "list", "--json"],
        &["secret", "list", "--json"],
        &["queue", "history", "--json"],
    ] {
        let out = run(&home, &repo, args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        outputs.push(("read command", out));
    }
    let flag = format!("--{CANARY}");
    for args in [
        &["account", "set", "qa-user", "--field", "session", CANARY][..],
        &["account", "set", "qa-user", "--field", "session", &flag],
    ] {
        let out = run(&home, &repo, args);
        assert_eq!(out.status.code(), Some(2));
        outputs.push(("account set with a value argument", out));
    }

    // A run's lease, its fields, and a command line that holds both values.
    let request = account_request(&home, &repo, "qa-user", lease());
    let outcome: ClaimOutcome =
        serde_json::from_value(home.call(Request::ClaimsAcquire(request)).unwrap()).unwrap();
    let ClaimOutcome::Granted { claim } = outcome else {
        panic!("not granted");
    };
    let command = format!("login --session {CANARY} --env {CANARY_2}");
    let got = materialize(&home, materialize_request(&repo, &claim.id, me(), &command)).unwrap();
    assert_eq!(got.fields.len(), 2);
    let refused = materialize(
        &home,
        materialize_request(&repo, "no-such-claim", me(), &command),
    )
    .unwrap_err();
    let mut texts = vec![format!(
        "{refused:?}{}",
        serde_json::to_string(&refused).unwrap()
    )];
    for request in [
        Request::ClaimsSnapshot,
        Request::ClaimsHistory(hostler::contract::heavy::HistoryQuery {
            since_ms: None,
            limit: 100,
        }),
        Request::AccountCheck {
            project: project_ref(&repo),
            name: "qa-user".into(),
        },
    ] {
        texts.push(home.call(request).unwrap().to_string());
    }
    home.call(Request::ClaimsRelease(
        hostler::contract::claims::ClaimRelease {
            claim_id: claim.id,
            workspace_id: claim.workspace.id,
            requester: me(),
            exit_code: Some(0),
        },
    ))
    .unwrap();
    texts.push(home.call(Request::ClaimsSnapshot).unwrap().to_string());

    // The materialization row names the fields and masks the command.
    let (names, recorded): (String, String) = rusqlite::Connection::open(&paths.db)
        .unwrap()
        .query_row(
            "SELECT names_json, command FROM secret_materializations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        names,
        r#"["account:qa-user/env","account:qa-user/session"]"#
    );
    assert_eq!(recorded, "login --session *** --env ***");

    std::thread::sleep(Duration::from_millis(300));
    let _ = stop_tx.send(());
    watcher.join().unwrap();
    let events = events_rx.recv().unwrap();
    assert!(
        events.iter().any(|e| matches!(e, Event::ClaimsChanged(_))),
        "the watcher saw the claim's events"
    );
    for event in &events {
        texts.push(serde_json::to_string(event).unwrap());
    }

    for (what, output) in &outputs {
        assert_no_canary(&format!("{what} stdout"), &output.stdout);
        assert_no_canary(&format!("{what} stderr"), &output.stderr);
    }
    for text in &texts {
        assert_no_canary("an answer or event", text.as_bytes());
    }
    let mut files = Vec::new();
    files_under(&paths.log_dir, &mut files);
    files_under(&paths.runs, &mut files);
    for suffix in ["", "-wal", "-shm"] {
        let db = PathBuf::from(format!("{}{suffix}", paths.db.display()));
        if let Ok(bytes) = std::fs::read(&db) {
            files.push((db, bytes));
        }
    }
    assert!(!files.is_empty());
    for (path, bytes) in &files {
        assert_no_canary(&path.display().to_string(), bytes);
    }
}

/// L38: a field may not be called `name` — `HOSTLER_ACCOUNT_NAME` is the account's name, and a
/// KeePass custom field `name` is a person's own data, never injected.
#[test]
fn name_field_is_reserved() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    let out = run_stdin(
        &home,
        &repo,
        &["account", "set", "qa-user", "--field", "name"],
        CANARY,
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("invalid field name"),
        "{}",
        stderr(&out)
    );
    assert_no_canary("the reserved field refusal", &out.stdout);
    assert_no_canary("the reserved field refusal", &out.stderr);
    let listed = run(&home, &repo, &["account", "list", "--json"]);
    assert_eq!(envelope(&listed)["data"], serde_json::json!([]));
}

/// L38/T10-7, end to end: a person's own KeePass custom field `name` is never listed, never
/// injected (`HOSTLER_ACCOUNT_NAME` stays the account's name) and never removed.
#[test]
fn a_persons_name_field_is_never_injected() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    set(&home, &repo, "qa-user", "session", CANARY);
    edit_entry(&home, "qa-user", |entry| {
        entry.set_protected("name", CANARY_2);
    });

    let listed = run(&home, &repo, &["account", "list", "--json"]);
    assert_eq!(
        envelope(&listed)["data"],
        serde_json::json!([{ "name": "qa-user", "fields": ["session"], "holder": null }]),
        "{}",
        stderr(&listed)
    );

    let out = home
        .hostler()
        .arg("run")
        .args(["--account", "qa-user", "--"])
        .args(hold(&["--print-env", "HOSTLER_ACCOUNT_NAME"]))
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "qa-user",
        "the name field overrode HOSTLER_ACCOUNT_NAME"
    );
    assert!(!holds(&out.stdout, CANARY_2));
    assert!(!holds(&out.stdout, CANARY));

    let removed = run(&home, &repo, &["account", "remove", "qa-user"]);
    assert_eq!(stdout(&removed).trim(), "removed", "{}", stderr(&removed));
    let (fields, tags) = entry_data(&home, "qa-user").expect("the person's entry stays");
    assert!(
        fields
            .iter()
            .any(|(key, value)| key == "name" && value == CANARY_2),
        "the person's field was removed: {fields:?}"
    );
    assert!(
        !fields.iter().any(|(key, _)| key == "session"),
        "{fields:?}"
    );
    assert!(!tags.iter().any(|tag| tag == "hostler-account"), "{tags:?}");
}

/// T10-7: a tagged entry titled `screen` (or with any invalid name) is the person's entry, not an
/// account; `account list` does not show it and hostler leaves it alone.
#[test]
fn a_tagged_entry_titled_screen_is_not_an_account() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    set(&home, &repo, "qa-user", "session", CANARY);
    edit_entry(&home, "qa-user", |entry| {
        entry.set("Title", keepass::db::Value::unprotected("screen"));
    });

    let listed = run(&home, &repo, &["account", "list", "--json"]);
    assert_eq!(
        envelope(&listed)["data"],
        serde_json::json!([]),
        "{}",
        stderr(&listed)
    );
    let (fields, tags) = entry_data(&home, "screen").expect("the person's entry stays");
    assert!(
        fields
            .iter()
            .any(|(key, value)| key == "session" && value == CANARY),
        "{fields:?}"
    );
    assert!(tags.iter().any(|tag| tag == "hostler-account"), "{tags:?}");
}

/// L39: the account name `screen` is reserved — its `--confirmation` token would collide with
/// the screen's.
#[test]
fn screen_is_a_reserved_account_name() {
    let home = Home::new();
    let repo = approved_repo(&home, "app");
    for args in [
        &["account", "set", "screen", "--field", "session"][..],
        &["account", "remove", "screen"][..],
    ] {
        let out = run_stdin(&home, &repo, args, CANARY);
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("invalid account name"),
            "{}",
            stderr(&out)
        );
        assert_no_canary("the reserved name refusal", &out.stdout);
        assert_no_canary("the reserved name refusal", &out.stderr);
    }
    let listed = run(&home, &repo, &["account", "list", "--json"]);
    assert_eq!(envelope(&listed)["data"], serde_json::json!([]));
}

/// L26 / T9-3: `--help` says what `account remove` deletes and what it keeps.
#[test]
fn remove_help_says_what_is_removed() {
    let home = Home::new();
    let out = home
        .hostler()
        .args(["account", "remove", "--help"])
        .output()
        .unwrap();
    let text = stdout(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("hostler removes only its own data")
            && text
                .contains("a password, user name, URL, notes, other custom fields or attachments"),
        "{text}"
    );
}
