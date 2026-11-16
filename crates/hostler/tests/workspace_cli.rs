//! Workspace identity through the real service (plan P3a Task 5): the id file, copies, moves,
//! and P2 projects following a moved repository (L15, L19, L21).

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use common::{Home, stderr};
use hostler::contract::ErrorCode;
use hostler::contract::claims::{WorkspaceInfo, WorkspaceOutcome, WorkspaceResolve};
use hostler::contract::secrets::ProjectRef;
use hostler::contract::wire::Request;
use serde_json::Value;

fn workspace_json(home: &Home, dir: &Path) -> (Output, Value) {
    let out = home
        .hostler()
        .args(["workspace", "--json"])
        .current_dir(dir)
        .output()
        .unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out, json)
}

fn info(home: &Home, dir: &Path) -> WorkspaceInfo {
    let (out, json) = workspace_json(home, dir);
    assert!(out.status.success(), "{}", stderr(&out));
    serde_json::from_value(json["data"].clone()).unwrap()
}

fn id_file(git_dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(git_dir.join("hostler-workspace")).unwrap()).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn git(args: &[&str], cwd: &Path) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs")
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    assert!(git(&["init", "-q"], repo).status.success());
    let commit = git(
        &[
            "-c",
            "user.name=hostler test",
            "-c",
            "user.email=hostler@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
        repo,
    );
    assert!(commit.status.success());
}

/// The canonical path as the service displays it (`project::display_path`).
fn shown(p: &Path) -> String {
    hostler::project::display_path(&fs::canonicalize(p).unwrap())
}

#[test]
fn workspace_resolves_and_writes_the_id_file() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let info = info(&home, &repo);
    assert_eq!(info.outcome, WorkspaceOutcome::Registered);
    let file = id_file(&repo.join(".git"));
    assert_eq!(file["id"], info.id);
    // Paths as the service shows them (`display_path`: no `\\?\` prefix on Windows).
    assert_eq!(file["path"], shown(&repo));
}

#[test]
fn second_call_is_known() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let first = info(&home, &repo);
    let second = info(&home, &repo);
    assert_eq!(second.id, first.id);
    assert_eq!(second.outcome, WorkspaceOutcome::Known);
    // From a subdirectory the checkout is the same workspace.
    let sub = repo.join("src");
    fs::create_dir_all(&sub).unwrap();
    assert_eq!(info(&home, &sub).id, first.id);
}

#[test]
fn copied_checkout_is_reidentified_and_rewritten() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let original = info(&home, &repo);
    let copy = repo.with_file_name("app-copy");
    copy_dir(&repo, &copy);
    let copied = info(&home, &copy);
    assert_eq!(copied.outcome, WorkspaceOutcome::Reidentified);
    assert_ne!(copied.id, original.id);
    assert_eq!(id_file(&copy.join(".git"))["id"], copied.id);
    assert_eq!(id_file(&repo.join(".git"))["id"], original.id);
    assert_eq!(info(&home, &repo).outcome, WorkspaceOutcome::Known);
}

#[test]
fn moved_worktree_keeps_its_id() {
    if !git_available() {
        eprintln!("skipped: no git");
        return;
    }
    let home = Home::new();
    let main = home.dir.path().join("repos").join("main");
    init_repo(&main);
    let wt = main.with_file_name("wt");
    assert!(
        git(&["worktree", "add", "-q", wt.to_str().unwrap()], &main)
            .status
            .success()
    );
    let before = info(&home, &wt);
    let moved = main.with_file_name("wt-moved");
    let out = git(
        &[
            "worktree",
            "move",
            wt.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
        &main,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = info(&home, &moved);
    assert_eq!(after.id, before.id);
    assert_eq!(after.outcome, WorkspaceOutcome::Moved);
    assert_eq!(after.project_id, before.project_id);
    // The main checkout is a workspace of its own in the same project.
    let main_info = info(&home, &main);
    assert_ne!(main_info.id, before.id);
    assert_eq!(main_info.project_id, before.project_id);
}

#[test]
fn copied_worktree_is_refused() {
    if !git_available() {
        eprintln!("skipped: no git");
        return;
    }
    let home = Home::new();
    let main = home.dir.path().join("repos").join("main");
    init_repo(&main);
    let wt = main.with_file_name("wt");
    assert!(
        git(&["worktree", "add", "-q", wt.to_str().unwrap()], &main)
            .status
            .success()
    );
    let original = info(&home, &wt);
    let copy = main.with_file_name("wt-copy");
    copy_dir(&wt, &copy);
    let (out, json) = workspace_json(&home, &copy);
    assert!(!out.status.success());
    assert_eq!(json["error"]["code"], "not_in_project", "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("git worktree repair")
    );
    // The original keeps its id; its git dir's file is untouched.
    assert_eq!(info(&home, &wt).id, original.id);
}

/// L15/L21: a moved main checkout keeps its workspace id, and its project (approval and
/// secrets) follows — whether `hostler workspace` or a P2 command sees the move first.
#[test]
fn moved_main_checkout_keeps_id_approval_and_secrets() {
    for workspace_first in [true, false] {
        let home = Home::new();
        home.unlock_new("pass-1234");
        let repo = home.make_repo("app");
        home.approve(&repo);
        home.set(&repo, "API_KEY", "value-1234");
        let before = info(&home, &repo);
        let moved = repo.with_file_name("app-moved");
        fs::rename(&repo, &moved).unwrap();

        if workspace_first {
            let after = info(&home, &moved);
            assert_eq!(after.id, before.id);
            assert_eq!(after.outcome, WorkspaceOutcome::Moved);
            assert_eq!(after.project_id, before.project_id);
            assert!(after.project_repointed, "{after:?}");
        }
        let list = home
            .hostler()
            .args(["secret", "list", "--json"])
            .current_dir(&moved)
            .output()
            .unwrap();
        assert!(list.status.success(), "{}", stderr(&list));
        let json: Value = serde_json::from_slice(&list.stdout).unwrap();
        assert_eq!(json["data"]["project"]["approved"], true, "{json}");
        assert_eq!(json["data"]["project"]["id"], before.project_id.as_str());
        assert_eq!(json["data"]["secrets"][0]["name"], "API_KEY");
        assert_eq!(json["data"]["secrets"][0]["state"], "set");
        if !workspace_first {
            // `secret list` recorded the move already (T5-6): the workspace is known there.
            let after = info(&home, &moved);
            assert_eq!(after.id, before.id);
            assert_eq!(after.project_id, before.project_id);
            assert_eq!(after.outcome, WorkspaceOutcome::Known);
            assert_eq!(after.path, shown(&moved), "{after:?}");
        }
    }
}

#[test]
fn two_copies_after_the_original_is_gone_get_distinct_ids() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let original = info(&home, &repo);
    let c1 = repo.with_file_name("c1");
    let c2 = repo.with_file_name("c2");
    copy_dir(&repo, &c1);
    copy_dir(&repo, &c2);
    fs::remove_dir_all(&repo).unwrap();
    let first = info(&home, &c1);
    assert_eq!(first.id, original.id);
    assert_eq!(first.outcome, WorkspaceOutcome::Moved);
    let second = info(&home, &c2);
    assert_ne!(second.id, original.id);
    assert_eq!(second.outcome, WorkspaceOutcome::Reidentified);
}

/// L21: `mv x y; <a new repository at x>` — x is a new, unapproved project and y keeps the old
/// one's approval and secrets, whichever resolves first.
#[test]
fn reused_path_with_another_repository_is_a_new_unapproved_project() {
    for y_first in [false, true] {
        let home = Home::new();
        home.unlock_new("pass-1234");
        let x = home.make_repo("x");
        home.approve(&x);
        home.set(&x, "API_KEY", "value-1234");
        info(&home, &x);
        let y = x.with_file_name("y");
        fs::rename(&x, &y).unwrap();
        let x = home.make_repo("x");

        let list_in = |dir: &Path| {
            home.hostler()
                .args(["secret", "list", "--json"])
                .current_dir(dir)
                .output()
                .unwrap()
        };
        let (in_x, in_y) = if y_first {
            let y_out = list_in(&y);
            (list_in(&x), y_out)
        } else {
            let x_out = list_in(&x);
            (x_out, list_in(&y))
        };
        let x_json: Value = serde_json::from_slice(&in_x.stdout).unwrap();
        assert_eq!(
            x_json["error"]["code"],
            "project_not_approved",
            "y_first={y_first}: {x_json} {}",
            stderr(&in_x)
        );
        assert!(
            in_y.status.success(),
            "y_first={y_first}: {}",
            stderr(&in_y)
        );
        let y_json: Value = serde_json::from_slice(&in_y.stdout).unwrap();
        assert_eq!(y_json["data"]["project"]["approved"], true);
        assert_eq!(y_json["data"]["secrets"][0]["name"], "API_KEY");
    }
}

#[test]
fn crafted_root_is_refused() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let sub = repo.join("src");
    fs::create_dir_all(&sub).unwrap();
    let current = hostler::project::current(&repo).unwrap();
    let resolve = |root: &Path, project: ProjectRef| {
        home.call(Request::WorkspaceResolve(WorkspaceResolve {
            project,
            root: hostler::project::display_path(root),
        }))
    };
    // A subdirectory is not a checkout root.
    let e = resolve(&sub, current.project.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest, "{e:?}");
    // A directory outside any checkout.
    let outside = home.dir.path().join("plain");
    fs::create_dir_all(&outside).unwrap();
    let e = resolve(&outside, current.project.clone()).unwrap_err();
    assert!(
        matches!(e.code, ErrorCode::NotInProject | ErrorCode::BadRequest),
        "{e:?}"
    );
    // Another checkout's root under this project's name.
    let other = home.make_repo("other");
    let e = resolve(&other, current.project).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest, "{e:?}");
    // Nothing was written outside the real checkouts.
    assert!(!sub.join("hostler-workspace").exists());
    assert!(!outside.join(".git").exists());
}

/// T5-R1: two copies of a checkout whose original is gone, resolved at the same time, never both
/// keep its id, and the old project follows at most one of them.
#[test]
fn concurrent_copies_after_the_original_is_gone_get_distinct_ids() {
    let home = Home::new();
    let start = home
        .hostler()
        .env("HOSTLER_TEST_RESOLVE_STEP_PAUSE_MS", "1500")
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(start.status.success(), "{}", stderr(&start));
    let repo = home.make_repo("main");
    let original = info(&home, &repo);
    let (a, b) = (repo.with_file_name("a"), repo.with_file_name("b"));
    copy_dir(&repo, &a);
    copy_dir(&repo, &b);
    fs::remove_dir_all(&repo).unwrap();
    let (ia, ib) = std::thread::scope(|scope| {
        let ra = scope.spawn(|| info(&home, &a));
        let rb = scope.spawn(|| info(&home, &b));
        (ra.join().unwrap(), rb.join().unwrap())
    });
    assert_ne!(ia.id, ib.id, "{ia:?} {ib:?}");
    assert!(
        ia.id == original.id || ib.id == original.id,
        "{ia:?} {ib:?}"
    );
    let followed = [&ia, &ib]
        .iter()
        .filter(|i| i.project_id == original.project_id)
        .count();
    assert!(followed <= 1, "{ia:?} {ib:?}");
    assert_eq!(id_file(&a.join(".git"))["id"], ia.id);
    assert_eq!(id_file(&b.join(".git"))["id"], ib.id);
}

/// T6-R2-2: parallel commands in one checkout all resolve it, to one id.
#[test]
fn eight_concurrent_resolves_of_one_checkout_share_one_id() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    let infos: Vec<WorkspaceInfo> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8).map(|_| scope.spawn(|| info(&home, &repo))).collect();
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    assert!(infos.iter().all(|i| i.id == infos[0].id), "{infos:?}");
    assert_eq!(id_file(&repo.join(".git"))["id"], infos[0].id);
    let listed: Vec<WorkspaceInfo> =
        serde_json::from_value(home.call(Request::WorkspaceList).unwrap()).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
}

/// T6-R2-2: when the id file cannot be written, a new id is not left stored without it.
#[cfg(unix)]
#[test]
fn unwritable_git_dir_leaves_no_workspace_row() {
    use std::os::unix::fs::PermissionsExt;

    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    let git = repo.join(".git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(git.join("probe"), "").is_ok() {
        // Root ignores the mode (the Linux container): nothing to check here.
        eprintln!("skipped: the git dir stays writable for this user");
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let (out, json) = workspace_json(&home, &repo);
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success());
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot record the workspace id"),
        "{json}"
    );
    let listed: Vec<WorkspaceInfo> =
        serde_json::from_value(home.call(Request::WorkspaceList).unwrap()).unwrap();
    assert!(listed.is_empty(), "{listed:?}");
    // And it resolves once it can write.
    assert_eq!(info(&home, &repo).outcome, WorkspaceOutcome::Registered);
}
