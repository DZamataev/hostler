//! Filesystem-only tests of workspace identity (plan P3a, Task 2).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostler::contract::ErrorCode;
use hostler::contract::claims::WorkspaceOutcome;
use hostler::project::display_path;
use hostler::workspace::{
    ID_FILE, IdFile, Identification, Known, dir_identity, git_dir_for, identify, read_id_file,
    write_id_file, write_identification,
};
use tempfile::tempdir;

/// Valid UUID v4 ids: the only id form `read_id_file` accepts (T2-3).
const WT_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const MAIN_ID: &str = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";

/// A git checkout without git: `.git` with `HEAD`, enough for `find_checkout`.
fn make_checkout(root: &Path) {
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
}

/// A worktree and the git dir git would give it, with `gitdir` holding `pointer`.
fn make_worktree(base: &Path, name: &str, pointer: &str) -> (PathBuf, PathBuf) {
    let repo = base.join("repo");
    let common = repo.join(".git");
    let worktree = repo.join(name);
    let git_dir = common.join("worktrees").join(name);
    fs::create_dir_all(&git_dir).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    fs::write(git_dir.join("gitdir"), pointer).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    (worktree, git_dir)
}

#[test]
fn copied_worktree_is_refused_by_the_back_pointer() {
    let dir = tempdir().unwrap();
    let worktree = dir.path().join("repo").join("wt1");
    let pointer = format!("{}\n", worktree.join(".git").display());
    let (worktree, git_dir) = make_worktree(dir.path(), "wt1", &pointer);

    let (checkout, resolved) = git_dir_for(&worktree).unwrap();
    assert_eq!(
        checkout.workspace_root,
        fs::canonicalize(&worktree).unwrap()
    );
    assert_eq!(resolved, fs::canonicalize(&git_dir).unwrap());

    let copy = dir.path().join("repo").join("wt1-copy");
    fs::create_dir_all(&copy).unwrap();
    fs::copy(worktree.join(".git"), copy.join(".git")).unwrap();
    let before = IdFile {
        id: WT_ID.into(),
        path: display_path(&worktree),
    };
    write_id_file(&git_dir, &before).unwrap();

    let error = git_dir_for(&copy).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(
        error.message.contains("shares its git dir with"),
        "{}",
        error.message
    );
    assert_eq!(read_id_file(&git_dir), Some(before));
}

#[test]
fn relative_back_pointer_is_resolved() {
    let dir = tempdir().unwrap();
    // From repo/.git/worktrees/wt1, ../../../wt1/.git is repo/wt1/.git.
    let (worktree, git_dir) = make_worktree(dir.path(), "wt1", "../../../wt1/.git\n");
    assert_eq!(
        git_dir_for(&worktree).unwrap().1,
        fs::canonicalize(&git_dir).unwrap()
    );

    let copy = dir.path().join("repo").join("wt1-copy");
    fs::create_dir_all(&copy).unwrap();
    fs::copy(worktree.join(".git"), copy.join(".git")).unwrap();
    let error = git_dir_for(&copy).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(
        error.message.contains("shares its git dir with"),
        "{}",
        error.message
    );
}

#[test]
fn dir_identity_is_stable_and_differs_between_dirs() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();

    let first = dir_identity(&a).expect("an identity for an existing directory");
    assert_eq!(dir_identity(&a), Some(first.clone()));
    assert_ne!(dir_identity(&b), Some(first.clone()));

    let moved = dir.path().join("a-moved");
    fs::rename(&a, &moved).unwrap();
    assert_eq!(dir_identity(&moved), Some(first));
}

#[test]
fn non_canonical_root_resolves_to_the_same_checkout() {
    // The CLI sends a display path, not a canonical one: a trailing `.`/`..` or a symlink must
    // still find the same checkout and git dir (Windows: the `\\?\` prefix is stripped).
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    make_checkout(&repo);
    let (canonical, git_dir) = git_dir_for(&repo).unwrap();

    let sub = repo.join("sub");
    fs::create_dir(&sub).unwrap();
    let indirect = sub.join("..");
    let (checkout, indirect_git_dir) = git_dir_for(&indirect).unwrap();
    assert_eq!(checkout.workspace_root, canonical.workspace_root);
    assert_eq!(indirect_git_dir, git_dir);

    let recorded = IdFile {
        id: MAIN_ID.into(),
        path: display_path(&canonical.workspace_root),
    };
    write_id_file(&git_dir, &recorded).unwrap();
    assert_eq!(read_id_file(&indirect_git_dir), Some(recorded));
}

#[test]
fn root_must_be_the_checkout_root() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    make_checkout(&repo);
    assert!(git_dir_for(&repo).is_ok());

    let sub = repo.join("sub");
    fs::create_dir(&sub).unwrap();
    assert_eq!(git_dir_for(&sub).unwrap_err().code, ErrorCode::BadRequest);

    // A file — inside the git dir, or anywhere in the checkout — is not a checkout: the same
    // NOT_IN_PROJECT refusal on every system.
    for file in [repo.join(".git").join("HEAD"), repo.join("README")] {
        fs::write(repo.join("README"), "").unwrap();
        let e = git_dir_for(&file).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotInProject, "{e:?}");
        assert!(
            e.message.ends_with("is a file, not a checkout"),
            "{}",
            e.message
        );
    }
}

#[test]
fn two_checkouts_that_share_a_git_dir_are_refused() {
    // A main checkout reached through a gitfile (`git clone --separate-git-dir`) and its copy
    // share one git dir; git keeps no back-pointer for this layout (T2-1).
    let dir = tempdir().unwrap();
    let store = dir.path().join("store").join("app.git");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let app = dir.path().join("app");
    let copy = dir.path().join("app-copy");
    for root in [&app, &copy] {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join(".git"), format!("gitdir: {}\n", store.display())).unwrap();
    }

    let (first, first_git_dir) = git_dir_for(&app).unwrap();
    let (second, second_git_dir) = git_dir_for(&copy).unwrap();
    assert_eq!(first_git_dir, second_git_dir);

    let id = IdFile {
        id: WT_ID.into(),
        path: display_path(&first.workspace_root),
    };
    write_id_file(&first_git_dir, &id).unwrap();

    let known = Known {
        id: WT_ID.into(),
        project_id: "p".into(),
        path: display_path(&first.workspace_root),
        git_dir: display_path(&first_git_dir),
        project_dir_exists: true,
    };
    let file = read_id_file(&first_git_dir).unwrap();
    let Identification::Refused(error) = identify(
        Some(&file),
        &second.workspace_root,
        &second_git_dir,
        "p",
        &|_| Some(known.clone()),
        &|_| true,
    ) else {
        panic!("two checkouts with one git dir must be refused");
    };
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(error.message.contains("shares its git dir with"));
    assert!(error.message.contains("give each checkout its own git dir"));
    assert_eq!(read_id_file(&first_git_dir), Some(id));
}

#[test]
fn moved_main_checkout_keeps_its_id_beside_a_new_repository() {
    if !git_available() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (checkout, git_dir) = git_dir_for(&repo).unwrap();
    let id = IdFile {
        id: MAIN_ID.into(),
        path: display_path(&checkout.workspace_root),
    };
    write_id_file(&git_dir, &id).unwrap();

    // `mv repo repo-old && git clone other repo`: a stranger now stands at the row's path.
    let repo_old = dir.path().join("repo-old");
    fs::rename(&repo, &repo_old).unwrap();
    let other = dir.path().join("other");
    init_repo(&other);
    assert!(
        git(
            &["clone", "-q", other.to_str().unwrap(), "repo"],
            dir.path()
        )
        .status
        .success()
    );

    let (old_checkout, old_git_dir) = git_dir_for(&repo_old).unwrap();
    let file = read_id_file(&old_git_dir).unwrap();
    let known = Known {
        id: MAIN_ID.into(),
        project_id: "p".into(),
        path: display_path(&fs::canonicalize(&repo).unwrap()),
        git_dir: display_path(&fs::canonicalize(repo.join(".git")).unwrap()),
        project_dir_exists: true,
    };
    let identification = identify(
        Some(&file),
        &old_checkout.workspace_root,
        &old_git_dir,
        "p",
        &|_| Some(known.clone()),
        &|_| true,
    );
    assert_eq!(
        identification,
        Identification::Keep {
            id: MAIN_ID.into(),
            outcome: WorkspaceOutcome::Moved,
            repoint_from: None,
        }
    );
    write_identification(&old_git_dir, &old_checkout.workspace_root, &identification).unwrap();
    assert_eq!(read_id_file(&old_git_dir).unwrap().id, MAIN_ID);
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
    assert!(
        git(
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
            repo
        )
        .status
        .success()
    );
}

#[test]
fn real_worktree_move_keeps_the_git_dir() {
    if !git_available() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo);
    assert!(
        git(&["worktree", "add", "-q", "../wt1"], &repo)
            .status
            .success()
    );
    let worktree = dir.path().join("wt1");
    let (checkout, git_dir) = git_dir_for(&worktree).unwrap();
    assert!(
        git_dir.starts_with(
            fs::canonicalize(&repo)
                .unwrap()
                .join(".git")
                .join("worktrees")
        ),
        "{}",
        git_dir.display()
    );

    let file = IdFile {
        id: WT_ID.into(),
        path: display_path(&checkout.workspace_root),
    };
    write_id_file(&git_dir, &file).unwrap();

    assert!(
        git(&["worktree", "move", "../wt1", "../wt1-moved"], &repo)
            .status
            .success()
    );
    let moved = dir.path().join("wt1-moved");
    let (moved_checkout, moved_git_dir) = git_dir_for(&moved).unwrap();
    assert_eq!(moved_git_dir, git_dir);
    assert_eq!(
        moved_checkout.workspace_root,
        fs::canonicalize(&moved).unwrap()
    );
    assert_eq!(read_id_file(&moved_git_dir), Some(file));
}

#[test]
fn real_main_checkout_mv() {
    if !git_available() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (checkout, git_dir) = git_dir_for(&repo).unwrap();
    assert_eq!(git_dir, fs::canonicalize(repo.join(".git")).unwrap());
    let file = IdFile {
        id: MAIN_ID.into(),
        path: display_path(&checkout.workspace_root),
    };
    write_id_file(&git_dir, &file).unwrap();

    let moved = dir.path().join("repo-moved");
    fs::rename(&repo, &moved).unwrap();
    let (moved_checkout, moved_git_dir) = git_dir_for(&moved).unwrap();
    assert_eq!(moved_git_dir, fs::canonicalize(moved.join(".git")).unwrap());
    assert_eq!(
        moved_checkout.workspace_root,
        fs::canonicalize(&moved).unwrap()
    );
    assert_eq!(read_id_file(&moved_git_dir), Some(file));
}

#[test]
fn a_git_dir_without_head_is_not_a_checkout() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("plain");
    fs::create_dir_all(root.join(".git")).unwrap();
    let error = git_dir_for(&root).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotInProject);
    assert!(!root.join(".git").join(ID_FILE).exists());
}
