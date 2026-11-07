//! Project resolution against real on-disk checkouts (plan P2, Task 2).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hostler::contract::ErrorCode;
use hostler::project;
use tempfile::tempdir;

fn make_git_dir(repo: &Path) {
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("cannot run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn main_checkout_resolves_to_its_git_dir() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("app");
    make_git_dir(&repo);

    let checkout = project::find_checkout(&repo).unwrap();
    assert_eq!(checkout.workspace_root, canonical(&repo));
    assert_eq!(checkout.git_dir, canonical(&repo.join(".git")));
    assert_eq!(checkout.common_dir, canonical(&repo.join(".git")));
}

#[test]
fn worktree_shares_the_common_dir() {
    let dir = tempdir().unwrap();
    let main = dir.path().join("main");
    make_git_dir(&main);
    let worktree_git = main.join(".git/worktrees/wt");
    fs::create_dir_all(&worktree_git).unwrap();
    fs::write(worktree_git.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
    fs::write(worktree_git.join("commondir"), "../..\n").unwrap();

    let worktree = dir.path().join("wt");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();

    let from_main = project::find_checkout(&main).unwrap();
    let from_worktree = project::find_checkout(&worktree).unwrap();
    assert_eq!(from_main.common_dir, canonical(&main.join(".git")));
    assert_eq!(from_worktree.common_dir, from_main.common_dir);
    assert_eq!(from_worktree.workspace_root, canonical(&worktree));
    assert_eq!(from_worktree.git_dir, canonical(&worktree_git));
}

#[test]
fn nested_cwd_finds_the_checkout() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("app");
    make_git_dir(&repo);
    let nested = repo.join("src/deep");
    fs::create_dir_all(&nested).unwrap();

    let checkout = project::find_checkout(&nested).unwrap();
    assert_eq!(checkout.workspace_root, canonical(&repo));
    assert_eq!(checkout.git_dir, canonical(&repo.join(".git")));
}

#[test]
fn gitfile_to_a_missing_target_is_refused() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("app");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join(".git"), "gitdir: ../missing\n").unwrap();

    let error = project::find_checkout(&repo).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(
        error.message.contains("cannot resolve"),
        "{}",
        error.message
    );
}

#[test]
fn gitfile_to_a_file_is_refused() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("app");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("plain.txt"), "not a git dir\n").unwrap();
    fs::write(repo.join(".git"), "gitdir: plain.txt\n").unwrap();

    let error = project::find_checkout(&repo).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("no HEAD"), "{}", error.message);
}

fn worktree_with_commondir(dir: &Path, contents: &[u8]) -> PathBuf {
    let main = dir.join("main");
    make_git_dir(&main);
    let worktree_git = main.join(".git/worktrees/wt");
    fs::create_dir_all(&worktree_git).unwrap();
    fs::write(worktree_git.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
    fs::write(worktree_git.join("commondir"), contents).unwrap();

    let worktree = dir.join("wt");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
    worktree
}

#[test]
fn commondir_with_a_missing_target_is_refused() {
    let dir = tempdir().unwrap();
    let worktree = worktree_with_commondir(dir.path(), b"../missing\n");

    let error = project::find_checkout(&worktree).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("commondir"), "{}", error.message);
}

#[test]
fn empty_commondir_is_refused() {
    let dir = tempdir().unwrap();
    let worktree = worktree_with_commondir(dir.path(), b"  \n");

    let error = project::find_checkout(&worktree).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("is empty"), "{}", error.message);
}

#[test]
fn commondir_with_invalid_utf8_is_refused() {
    let dir = tempdir().unwrap();
    let worktree = worktree_with_commondir(dir.path(), &[0xff, 0xfe, b'\n']);

    let error = project::find_checkout(&worktree).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("commondir"), "{}", error.message);
}

#[test]
fn commondir_target_without_head_is_refused() {
    let dir = tempdir().unwrap();
    let worktree = worktree_with_commondir(dir.path(), b"../no-head\n");
    let no_head = dir.path().join("main/.git/worktrees/no-head");
    fs::create_dir_all(&no_head).unwrap();

    let error = project::find_checkout(&worktree).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("commondir"), "{}", error.message);
    assert!(error.message.contains("no HEAD"), "{}", error.message);
}

#[cfg(unix)]
#[test]
fn a_non_utf8_checkout_is_refused_instead_of_reusing_a_lossy_twin() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use hostler::store::Store;

    let dir = tempdir().unwrap();
    let valid = dir.path().join("app-\u{FFFD}");
    make_git_dir(&valid);
    let invalid = dir.path().join(OsString::from_vec(b"app-\xff".to_vec()));
    if fs::create_dir_all(&invalid).is_err() {
        eprintln!("skipping: this filesystem rejects non-UTF-8 names");
        return;
    }
    make_git_dir(&invalid);

    let valid_ref = project::current(&valid).unwrap().project;
    assert_eq!(
        valid_ref.common_dir,
        project::display_path(&canonical(&valid.join(".git")))
    );
    // Without the check these two canonical paths display as the same string.
    let invalid_checkout = project::find_checkout(&invalid).unwrap();
    assert_eq!(
        project::display_path(&invalid_checkout.common_dir),
        valid_ref.common_dir
    );

    // An approved record for the valid checkout must not become reachable through the other.
    let store = Store::open_in_memory().unwrap();
    let info = store
        .upsert_project(&valid_ref.common_dir, "app", 1)
        .unwrap();
    store.approve_project(&info.id, 2).unwrap();
    assert!(
        store
            .project_by_dir(&valid_ref.common_dir)
            .unwrap()
            .unwrap()
            .approved
    );

    let error = project::current(&invalid).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(
        error.message.contains("not valid UTF-8"),
        "{}",
        error.message
    );
}

#[cfg(unix)]
#[test]
fn a_worktree_with_a_non_utf8_root_is_refused() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let dir = tempdir().unwrap();
    let main = dir.path().join("main");
    make_git_dir(&main);
    let worktree_git = main.join(".git/worktrees/wt");
    fs::create_dir_all(&worktree_git).unwrap();
    fs::write(worktree_git.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
    fs::write(worktree_git.join("commondir"), "../..\n").unwrap();

    let worktree = dir.path().join(OsString::from_vec(b"wt-\xff".to_vec()));
    if fs::create_dir_all(&worktree).is_err() {
        eprintln!("skipping: this filesystem rejects non-UTF-8 names");
        return;
    }
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )
    .unwrap();

    // The common dir is valid; only the checkout's own root is not.
    let checkout = project::find_checkout(&worktree).unwrap();
    assert_eq!(checkout.common_dir, canonical(&main.join(".git")));
    let error = project::current(&worktree).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(
        error.message.contains("not valid UTF-8"),
        "{}",
        error.message
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_git_symlink_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let repo = dir.path().join("app");
    fs::create_dir_all(&repo).unwrap();
    symlink(repo.join("missing"), repo.join(".git")).unwrap();

    let error = project::find_checkout(&repo).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("cannot read"), "{}", error.message);
}

#[test]
fn outside_git_is_not_in_project() {
    let dir = tempdir().unwrap();
    let error = project::find_checkout(dir.path()).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(
        error.message.contains("not inside a git checkout"),
        "{}",
        error.message
    );
}

#[test]
fn name_from_config_else_directory() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("my-app");
    make_git_dir(&repo);

    let current = project::current(&repo).unwrap();
    assert_eq!(current.project.name, "my-app");
    assert_eq!(
        current.project.common_dir,
        project::display_path(&canonical(&repo.join(".git")))
    );
    assert!(current.config.is_none());

    fs::write(
        repo.join("hostler.toml"),
        "[project]\nname = \"the custom name\"\n",
    )
    .unwrap();
    let current = project::current(&repo).unwrap();
    assert_eq!(current.project.name, "the custom name");
    assert_eq!(
        current.config.as_ref().unwrap().name.as_deref(),
        Some("the custom name")
    );
}

#[test]
fn broken_config_is_config_invalid_with_a_line() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("app");
    make_git_dir(&repo);
    fs::write(
        repo.join("hostler.toml"),
        "[secrets.API_KEY]\npurpose = 12\n",
    )
    .unwrap();

    let error = project::current(&repo).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(
        error.message.starts_with("hostler.toml has problems:"),
        "{}",
        error.message
    );
    assert_eq!(error.details["problems"][0]["line"], 2);
}

#[test]
fn real_git_worktree_matches() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping real_git_worktree_matches: git is not installed");
        return;
    }

    let dir = tempdir().unwrap();
    let main = dir.path().join("main");
    fs::create_dir_all(&main).unwrap();
    run_git(&main, &["init", "-q"]);
    run_git(
        &main,
        &[
            "-c",
            "user.name=hostler",
            "-c",
            "user.email=hostler@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    let worktree = dir.path().join("wt");
    run_git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "wt",
        ],
    );

    let from_main = project::find_checkout(&main).unwrap();
    let from_worktree = project::find_checkout(&worktree).unwrap();
    assert_eq!(from_worktree.common_dir, from_main.common_dir);
    assert_eq!(from_main.common_dir, canonical(&main.join(".git")));
}
