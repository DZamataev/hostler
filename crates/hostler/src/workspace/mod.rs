//! Workspace identity: the id file in a checkout's git dir (plan P3a, Task 2).
//!
//! Only the service reads and writes the file. The CLI sends the checkout's root; the git dir is
//! derived here with P2's [`find_checkout`], and a worktree must point back at its own root.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::contract::claims::WorkspaceOutcome;
use crate::contract::secrets::errors::bad_request;
use crate::contract::{ErrorCode, HostlerError};
use crate::project::{Checkout, display_path, find_checkout};

pub const ID_FILE: &str = "hostler-workspace";

/// The largest id file this hostler writes; a bigger one is not a workspace id (T2-3).
const MAX_ID_FILE_BYTES: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdFile {
    pub id: String,
    /// The checkout root, for people; identity decisions use the store row's path (L19).
    pub path: String,
}

/// A workspace the store already knows, as much as [`identify`] needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub id: String,
    pub project_id: String,
    /// The store row's path, not the id file's (L19).
    pub path: String,
    /// The store row's git dir: its checkout's id file lives there. Two checkouts with one git
    /// dir would rewrite each other's id (T2-1).
    pub git_dir: String,
    /// Whether the project this workspace was recorded under still has its common dir.
    pub project_dir_exists: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identification {
    Keep {
        id: String,
        outcome: WorkspaceOutcome,
        /// The old project that follows the move to `root`, when the identity changed with it.
        repoint_from: Option<String>,
    },
    New {
        id: String,
        outcome: WorkspaceOutcome,
    },
    /// T2-1: another live checkout uses the same git dir (a main checkout reached through a
    /// gitfile, and its copy). Nothing is written; Task 5 returns this error.
    Refused(HostlerError),
}

/// The checkout's git dir, which is where its workspace id lives.
///
/// A main checkout uses `<repo>/.git`; a worktree uses `<common>/worktrees/<name>`, whose
/// `gitdir` back-pointer must name this root's `.git` (a copied worktree shares the original's
/// git dir and would otherwise write its id into the original).
pub fn git_dir_for(root: &Path) -> Result<(Checkout, PathBuf), HostlerError> {
    // A file is never a checkout. Said here, the same on every system: the walk below would
    // otherwise reach the enclosing checkout on Windows (`<file>\.git` is "not found" there)
    // and call the file a subdirectory, but stop on Unix (`ENOTDIR`).
    if fs::metadata(root).is_ok_and(|m| !m.is_dir()) {
        return Err(not_in_project(format!(
            "{} is a file, not a checkout",
            display_path(root)
        )));
    }
    let checkout = find_checkout(root)?;
    let canonical_root =
        fs::canonicalize(root).map_err(|_| crate::project::not_in_project(root))?;
    if checkout.workspace_root != canonical_root {
        return Err(bad_request(format!(
            "{} is not the root of its checkout; the checkout root is {}",
            display_path(&canonical_root),
            display_path(&checkout.workspace_root)
        )));
    }

    let git_dir = checkout.git_dir.clone();
    let worktrees = checkout.common_dir.join("worktrees");
    let is_common = git_dir == checkout.common_dir;
    let is_worktree = git_dir.parent() == Some(worktrees.as_path());
    if !is_common && !is_worktree {
        return Err(bad_request(format!(
            "{} is not this checkout's git dir (expected {} or {}/worktrees/<name>)",
            display_path(&git_dir),
            display_path(&checkout.common_dir),
            display_path(&checkout.common_dir)
        )));
    }
    if !git_dir.join("HEAD").is_file() {
        return Err(bad_request(format!(
            "{} is not a git directory (it has no HEAD)",
            display_path(&git_dir)
        )));
    }
    if is_worktree {
        check_back_pointer(&git_dir, &canonical_root)?;
    }
    Ok((checkout, git_dir))
}

/// `gitdir` names `<root>/.git`, relative to the git dir when git wrote it that way
/// (`worktree.useRelativePaths`, git ≥ 2.48).
fn check_back_pointer(git_dir: &Path, root: &Path) -> Result<(), HostlerError> {
    let pointer = git_dir.join("gitdir");
    let text = fs::read_to_string(&pointer).map_err(|e| {
        not_in_project(format!(
            "{} is a worktree git dir without a readable gitdir back-pointer: {e}",
            display_path(git_dir)
        ))
    })?;
    let target = text.trim();
    if target.is_empty() {
        return Err(not_in_project(format!(
            "{} has an empty gitdir back-pointer",
            display_path(&pointer)
        )));
    }
    let named = Path::new(target);
    let named = if named.is_absolute() {
        named.to_path_buf()
    } else {
        git_dir.join(named)
    };
    let named = fs::canonicalize(&named).unwrap_or(named);
    let dot_git = root.join(".git");
    let dot_git = fs::canonicalize(&dot_git).unwrap_or(dot_git);
    if named != dot_git {
        return Err(not_in_project(format!(
            "{} shares its git dir with {} (a copied worktree?); run `git worktree repair` in the \
             checkout you keep",
            display_path(root),
            display_path(&named)
        )));
    }
    Ok(())
}

fn not_in_project(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::NotInProject,
        message,
        details: serde_json::json!({}),
        recovery: None,
    }
}

/// L21's filesystem identity of a directory: `unix:<dev>:<ino>:<birth time ns or ->` or
/// `win:<volume serial>:<128-bit file id>`. `None` when it cannot be read: the identity rules are
/// then skipped and the path decides, as in P2.
#[cfg(unix)]
pub fn dir_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    use std::time::UNIX_EPOCH;

    let meta = fs::metadata(path).ok()?;
    // A filesystem without birth times (some Linux ones) is a documented inode-reuse residual.
    let birth_time = meta
        .created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|age| age.as_nanos().to_string())
        .unwrap_or_else(|| "-".into());
    Some(format!("unix:{}:{}:{}", meta.dev(), meta.ino(), birth_time))
}

#[cfg(windows)]
pub fn dir_identity(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo,
        GetFileInformationByHandleEx, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: a NUL-terminated path; the handle is closed below. A directory needs
    // FILE_FLAG_BACKUP_SEMANTICS. std's volume_serial_number/file_index are still unstable.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info = FILE_ID_INFO::default();
    // SAFETY: `info` is a FILE_ID_INFO and its size is passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    // SAFETY: the handle came from CreateFileW and is closed once.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    let file_id = u128::from_be_bytes(info.FileId.Identifier);
    Some(format!(
        "win:{:016x}:{:032x}",
        info.VolumeSerialNumber, file_id
    ))
}

/// `None` when the path cannot be opened, is not a regular file (a symlink or FIFO would block
/// or exhaust the service, T2-3), is larger than [`MAX_ID_FILE_BYTES`], or does not hold a
/// UUID v4.
pub fn read_id_file(git_dir: &Path) -> Option<IdFile> {
    let file = open_id_file(&git_dir.join(ID_FILE))?;
    // Check the opened file, not the path: a check-then-open would race with a replacement.
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_ID_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_ID_FILE_BYTES {
        return None;
    }
    let file: IdFile = serde_json::from_slice(&bytes).ok()?;
    let id = uuid::Uuid::parse_str(&file.id).ok()?;
    (id.get_version_num() == 4).then_some(file)
}

/// A FIFO stays a FIFO when opened non-blocking; a symlink is never followed (Unix) or is
/// recognised from the reparse point (Windows).
#[cfg(unix)]
fn open_id_file(path: &Path) -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()
}

#[cfg(windows)]
fn open_id_file(path: &Path) -> Option<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()
}

/// The file is written next to its final name, synced, and renamed, so a reader never sees half
/// of it and a crash cannot leave an empty id (T2-4).
pub fn write_id_file(git_dir: &Path, file: &IdFile) -> io::Result<()> {
    use std::io::Write;

    let json = serde_json::to_vec(file).map_err(io::Error::other)?;
    let temp = git_dir.join(format!(".{ID_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let target = git_dir.join(ID_FILE);
    let written = (|| -> io::Result<()> {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        out.write_all(&json)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&temp, &target)?;
        // The content is already synced; syncing the directory only makes the rename durable,
        // and some filesystems refuse it. After a completed rename this is never a failed write.
        #[cfg(unix)]
        if let Err(e) = fs::File::open(git_dir).and_then(|dir| dir.sync_all())
            && !dir_sync_is_ignorable(&e)
        {
            tracing::warn!(error_kind = %e.kind(), "could not sync the workspace directory");
        }
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// `EINVAL`/`ENOTSUP`: this filesystem does not sync directories (some FUSE/SMB/NFS mounts).
#[cfg(unix)]
fn dir_sync_is_ignorable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL) | Some(libc::ENOTSUP)
    )
}

/// Records an identification in the git dir: the chosen id and the current root. Every `Keep`
/// rewrites the file, so its path follows a moved checkout (L19).
pub fn write_identification(
    git_dir: &Path,
    root: &Path,
    identification: &Identification,
) -> io::Result<()> {
    let id = match identification {
        Identification::Keep { id, .. } | Identification::New { id, .. } => id,
        // A refused checkout must never reach here; Task 5 returns the refusal first.
        Identification::Refused(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a checkout that shares its git dir gets no id file",
            ));
        }
    };
    write_id_file(
        git_dir,
        &IdFile {
            id: id.clone(),
            path: display_path(root),
        },
    )
}

/// The L19 identity decision, against the store row's path rather than the id file's hint.
///
/// `root` is the checkout's canonical root and `git_dir` its git dir (what `git_dir_for`
/// returned); `known` answers by id from the store; `path_exists` answers whether the store
/// row's path still exists.
pub fn identify(
    file: Option<&IdFile>,
    root: &Path,
    git_dir: &Path,
    project_id: &str,
    known: &dyn Fn(&str) -> Option<Known>,
    path_exists: &dyn Fn(&Path) -> bool,
) -> Identification {
    let Some(file) = file else {
        return new(WorkspaceOutcome::Registered);
    };
    let Some(row) = known(&file.id) else {
        // A lost hostler.db: the id file is still this checkout's identity.
        return keep(file.id.clone(), WorkspaceOutcome::Registered, None);
    };

    let old_project_follows =
        (row.project_id != project_id && !row.project_dir_exists).then(|| row.project_id.clone());

    if same_path(&row.path, root) {
        return match (row.project_id == project_id, old_project_follows) {
            (true, _) => keep(row.id, WorkspaceOutcome::Known, None),
            (false, Some(old)) => keep(row.id, WorkspaceOutcome::Known, Some(old)),
            // The path now holds another repository whose old project is still alive.
            (false, None) => new(WorkspaceOutcome::Reidentified),
        };
    }

    if path_exists(Path::new(&row.path)) {
        // A second checkout at the row's path whose `.git` file points at the same git dir
        // rewrites one shared id file back and forth (T2-1). Git keeps no back-pointer for it.
        if same_git_dir(&row.git_dir, git_dir) {
            return Identification::Refused(not_in_project(format!(
                "{} shares its git dir with {}; give each checkout its own git dir",
                display_path(root),
                display_path(Path::new(&row.path))
            )));
        }
        // Rule 4 holds only while the checkout at the row's path still reads this id from its
        // own id file. `mv repo repo-old && git clone other repo` leaves a stranger there, so
        // the moved original keeps its id (T2-2).
        if row_still_reads_this_id(&row) {
            return new(WorkspaceOutcome::Reidentified);
        }
    }

    // The store row's path is gone, or no longer reads this id: this is that checkout, moved. A
    // second copy resolving later meets the branch above.
    keep(row.id, WorkspaceOutcome::Moved, old_project_follows)
}

fn row_still_reads_this_id(row: &Known) -> bool {
    read_id_file(Path::new(&row.git_dir)).is_some_and(|file| file.id == row.id)
}

/// Store paths come from the contract and may lack the Windows verbatim prefix that
/// canonicalization adds; people compare them as displayed.
fn same_path(recorded: &str, root: &Path) -> bool {
    display_path(Path::new(recorded)) == display_path(root)
}

fn same_git_dir(recorded: &str, git_dir: &Path) -> bool {
    display_path(Path::new(recorded)) == display_path(git_dir)
}

fn keep(id: String, outcome: WorkspaceOutcome, repoint_from: Option<String>) -> Identification {
    Identification::Keep {
        id,
        outcome,
        repoint_from,
    }
}

fn new(outcome: WorkspaceOutcome) -> Identification {
    Identification::New {
        id: uuid::Uuid::new_v4().to_string(),
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::{TempDir, tempdir};

    use super::*;

    /// A valid UUID v4: the only id form `read_id_file` accepts (T2-3).
    const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    fn id_file(id: &str) -> IdFile {
        IdFile {
            id: id.into(),
            path: "/somewhere".into(),
        }
    }

    fn missing_path(_: &Path) -> bool {
        false
    }

    fn present_path(_: &Path) -> bool {
        true
    }

    fn row(id: &str, project: &str, path: &str, git_dir: &str) -> Known {
        Known {
            id: id.into(),
            project_id: project.into(),
            path: path.into(),
            git_dir: git_dir.into(),
            project_dir_exists: true,
        }
    }

    /// A store row whose checkout still reads `id` from its own id file at `git_dir`.
    fn row_with_id_file(id: &str, project: &str, path: &str) -> (TempDir, Known) {
        let dir = tempdir().unwrap();
        write_id_file(dir.path(), &id_file(id)).unwrap();
        let row = row(id, project, path, &dir.path().to_string_lossy());
        (dir, row)
    }

    #[test]
    fn no_file_is_new() {
        let got = identify(
            None,
            Path::new("/w"),
            Path::new("/w/.git"),
            "p",
            &|_| panic!("no id to look up"),
            &missing_path,
        );
        assert!(matches!(
            got,
            Identification::New {
                outcome: WorkspaceOutcome::Registered,
                ..
            }
        ));
    }

    #[test]
    fn same_path_is_known() {
        let file = id_file(ID);
        let known = row(ID, "p", "/w", "/w/.git");
        assert_eq!(
            identify(
                Some(&file),
                Path::new("/w"),
                Path::new("/w/.git"),
                "p",
                &|_| Some(known.clone()),
                &present_path
            ),
            keep(ID.into(), WorkspaceOutcome::Known, None)
        );
    }

    #[test]
    fn copy_with_original_present_is_reidentified() {
        let file = id_file(ID);
        let (_dir, known) = row_with_id_file(ID, "p", "/original");
        let got = identify(
            Some(&file),
            Path::new("/copy"),
            Path::new("/copy/.git"),
            "p",
            &|_| Some(known.clone()),
            &present_path,
        );
        assert!(matches!(
            got,
            Identification::New {
                outcome: WorkspaceOutcome::Reidentified,
                ..
            }
        ));
    }

    #[test]
    fn moved_main_checkout_keeps_the_id_and_repoints_the_project() {
        let file = id_file(ID);
        let mut known = row(ID, "old-project", "/old", "/old/.git");
        // The old project's common dir went with the move (L15).
        known.project_dir_exists = false;
        assert_eq!(
            identify(
                Some(&file),
                Path::new("/new"),
                Path::new("/new/.git"),
                "new-project",
                &|_| Some(known.clone()),
                &missing_path
            ),
            keep(
                ID.into(),
                WorkspaceOutcome::Moved,
                Some("old-project".into())
            )
        );
    }

    #[test]
    fn moved_worktree_keeps_the_id_without_repoint() {
        let file = id_file(ID);
        let known = row(ID, "p", "/old", "/old/.git");
        assert_eq!(
            identify(
                Some(&file),
                Path::new("/new"),
                Path::new("/new/.git"),
                "p",
                &|_| Some(known.clone()),
                &missing_path
            ),
            keep(ID.into(), WorkspaceOutcome::Moved, None)
        );
    }

    #[test]
    fn copied_main_checkout_is_reidentified() {
        let file = id_file(ID);
        let (_dir, known) = row_with_id_file(ID, "old-project", "/original");
        let got = identify(
            Some(&file),
            Path::new("/copy"),
            Path::new("/copy/.git"),
            "new-project",
            &|_| Some(known.clone()),
            &present_path,
        );
        assert!(matches!(
            got,
            Identification::New {
                outcome: WorkspaceOutcome::Reidentified,
                ..
            }
        ));
    }

    #[test]
    fn unknown_id_is_registered_as_is() {
        let file = id_file(ID);
        assert_eq!(
            identify(
                Some(&file),
                Path::new("/w"),
                Path::new("/w/.git"),
                "p",
                &|_| None,
                &present_path
            ),
            keep(ID.into(), WorkspaceOutcome::Registered, None)
        );
    }

    #[test]
    fn shared_git_dir_is_refused() {
        // Two main checkouts whose `.git` files point at one git dir (T2-1).
        let file = id_file(ID);
        let known = row(ID, "p", "/other", "/shared.git");
        let got = identify(
            Some(&file),
            Path::new("/copy"),
            Path::new("/shared.git"),
            "p",
            &|_| Some(known.clone()),
            &present_path,
        );
        let Identification::Refused(error) = got else {
            panic!("a shared git dir must be refused");
        };
        assert_eq!(error.code, ErrorCode::NotInProject);
        assert!(
            error
                .message
                .contains("/copy shares its git dir with /other")
        );
        assert!(error.message.contains("give each checkout its own git dir"));
    }

    #[test]
    fn moved_checkout_survives_a_new_repository_at_its_old_path() {
        // `mv repo repo-old && git clone other repo`: the row path exists but its checkout no
        // longer reads this id, so the moved original keeps it (T2-2).
        let file = id_file(ID);
        let known = row(ID, "p", "/repo", "/repo/.git");
        assert_eq!(
            identify(
                Some(&file),
                Path::new("/repo-old"),
                Path::new("/repo-old/.git"),
                "p",
                &|_| Some(known.clone()),
                &present_path
            ),
            keep(ID.into(), WorkspaceOutcome::Moved, None)
        );
    }

    #[test]
    fn two_copies_after_the_original_is_gone_get_distinct_ids() {
        // `cp -R repo c1; cp -R repo c2; rm -rf repo`: both carry the same id file.
        let file = id_file(ID);
        let mut rows: HashMap<String, Known> = HashMap::new();
        rows.insert(ID.into(), row(ID, "p", "/repo", "/repo/.git"));

        let first = identify(
            Some(&file),
            Path::new("/c1"),
            Path::new("/c1/.git"),
            "p",
            &|id| rows.get(id).cloned(),
            &missing_path,
        );
        let Identification::Keep { id, outcome, .. } = first else {
            panic!("the first copy keeps the id");
        };
        assert_eq!(outcome, WorkspaceOutcome::Moved);
        // The service moves the store row to the first copy to resolve, which still reads the id.
        let (_dir, moved_row) = row_with_id_file(&id, "p", "/c1");
        rows.insert(id.clone(), moved_row);

        let second = identify(
            Some(&file),
            Path::new("/c2"),
            Path::new("/c2/.git"),
            "p",
            &|id| rows.get(id).cloned(),
            &present_path,
        );
        let Identification::New { id: fresh, .. } = second else {
            panic!("the second copy is reidentified");
        };
        assert_ne!(fresh, id);
    }

    #[test]
    fn the_copy_that_resolves_first_keeps_the_id_when_the_original_moved() {
        // `cp -R repo c1 && mv repo c2`: the store row is still at /repo (gone).
        let file = id_file(ID);
        let mut rows: HashMap<String, Known> = HashMap::new();
        rows.insert(ID.into(), row(ID, "p", "/repo", "/repo/.git"));

        let moved = identify(
            Some(&file),
            Path::new("/c2"),
            Path::new("/c2/.git"),
            "p",
            &|id| rows.get(id).cloned(),
            &missing_path,
        );
        let Identification::Keep { id, .. } = moved else {
            panic!("the moved original keeps the id");
        };
        let (_dir, moved_row) = row_with_id_file(&id, "p", "/c2");
        rows.insert(id.clone(), moved_row);

        let copy = identify(
            Some(&file),
            Path::new("/c1"),
            Path::new("/c1/.git"),
            "p",
            &|row_id| rows.get(row_id).cloned(),
            &present_path,
        );
        assert!(matches!(
            copy,
            Identification::New {
                outcome: WorkspaceOutcome::Reidentified,
                ..
            }
        ));
    }

    #[test]
    fn keep_rewrites_the_id_file() {
        let dir = tempdir().unwrap();
        let file = IdFile {
            id: ID.into(),
            path: "/old".into(),
        };
        write_id_file(dir.path(), &file).unwrap();

        let identification = keep(ID.into(), WorkspaceOutcome::Moved, None);
        write_identification(dir.path(), Path::new("/new"), &identification).unwrap();

        assert_eq!(
            read_id_file(dir.path()),
            Some(IdFile {
                id: ID.into(),
                path: "/new".into()
            })
        );
    }

    #[test]
    fn garbled_file_reads_as_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(ID_FILE), b"not json").unwrap();
        assert_eq!(read_id_file(dir.path()), None);
    }

    #[test]
    fn read_id_file_rejects_a_malformed_or_non_v4_id() {
        let dir = tempdir().unwrap();
        for id in ["", "not-a-uuid", "6ba7b810-9dad-11d1-80b4-00c04fd430c8"] {
            let text = format!(r#"{{"id":"{id}","path":"/w"}}"#);
            fs::write(dir.path().join(ID_FILE), text).unwrap();
            assert_eq!(read_id_file(dir.path()), None, "{id}");
        }
    }

    #[test]
    fn read_id_file_rejects_oversized_files() {
        let dir = tempdir().unwrap();
        let mut bytes = serde_json::to_vec(&id_file(ID)).unwrap();
        bytes.resize(MAX_ID_FILE_BYTES as usize + 1, b' ');
        fs::write(dir.path().join(ID_FILE), bytes).unwrap();
        assert_eq!(read_id_file(dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn read_id_file_rejects_symlinks() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, serde_json::to_vec(&id_file(ID)).unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(ID_FILE)).unwrap();
        assert_eq!(read_id_file(dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn read_id_file_rejects_fifos() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join(ID_FILE);
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path in an existing directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // Opened with O_NONBLOCK and rejected from the opened file's metadata: a plain open
        // would block here forever (T2-3 residual).
        assert_eq!(read_id_file(dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_directory_syncs_are_ignored() {
        for code in [libc::EINVAL, libc::ENOTSUP] {
            assert!(dir_sync_is_ignorable(&io::Error::from_raw_os_error(code)));
        }
        assert!(!dir_sync_is_ignorable(&io::Error::from_raw_os_error(
            libc::EACCES
        )));
        assert!(!dir_sync_is_ignorable(&io::Error::other("no OS error")));
    }

    #[test]
    fn write_is_atomic_and_readable() {
        let dir = tempdir().unwrap();
        let file = id_file(ID);
        write_id_file(dir.path(), &file).unwrap();
        assert_eq!(read_id_file(dir.path()), Some(file));

        // Nothing but the final file is left behind.
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(ID_FILE)]);
    }

    #[test]
    fn refused_identification_writes_nothing() {
        let dir = tempdir().unwrap();
        let refusal = Identification::Refused(not_in_project("shared".into()));
        assert!(write_identification(dir.path(), Path::new("/w"), &refusal).is_err());
        assert_eq!(read_id_file(dir.path()), None);
    }
}
