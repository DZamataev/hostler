//! The only way doctor reads a project file (`hostler.toml`, the agent docs, the device-rules
//! file). Nothing read here is ever printed: callers report states and line numbers only.
//!
//! - **No env file (R1-4, anti-goal 1):** a path whose own name, or whose resolved target's name,
//!   looks like one is refused before it is opened, so a `[agents] docs` entry or a symlink
//!   cannot make doctor read one.
//! - **Inside the checkout (review t6-r1 R1-3):** the target is resolved against the
//!   canonical checkout root and must stay beneath it; a symlink pointing outside is refused.
//! - **One regular file:** the resolved path is opened without following a symlink and without
//!   blocking, and the handle must be a regular file (the flags of init's `open_regular`, Task
//!   3), so nothing can swap the checked file for another between the check and the read.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Doctor reads at most this much of a project file.
const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadRefused {
    NotFound,
    /// The path is not relative, or names a parent.
    NotRelative,
    /// The path, once its symlinks are resolved, leaves the checkout.
    Outside,
    /// The name (or the symlink target's name) is an env file's.
    EnvFile,
    NotAFile,
    TooLarge,
    Failed(String),
}

impl std::fmt::Display for ReadRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadRefused::NotFound => f.write_str("it does not exist"),
            ReadRefused::NotRelative => {
                f.write_str("it is not a relative path inside the checkout")
            }
            ReadRefused::Outside => f.write_str("it points outside the checkout"),
            ReadRefused::EnvFile => {
                f.write_str("it is an env file, which doctor never reads (hostler init lists them)")
            }
            ReadRefused::NotAFile => f.write_str("it is not a regular file"),
            ReadRefused::TooLarge => write!(f, "it is larger than {MAX_BYTES} bytes"),
            ReadRefused::Failed(why) => write!(f, "it cannot be read: {why}"),
        }
    }
}

/// Whether `name` is an env file's: `.env`, `.env.<anything>`, `.envrc`, or `<anything>.env`.
pub(crate) fn is_env_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix('.')
        .is_some_and(|rest| rest.starts_with("env"))
        || Path::new(&lower)
            .extension()
            .is_some_and(|extension| extension == "env")
}

/// `root/rel`, when `rel` is relative, names no parent and no env file (lexically).
pub(crate) fn project_path(root: &Path, rel: &str) -> Result<PathBuf, ReadRefused> {
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(ReadRefused::NotRelative);
    }
    if rel_path
        .components()
        .any(|c| is_env_file_name(&c.as_os_str().to_string_lossy()))
    {
        return Err(ReadRefused::EnvFile);
    }
    Ok(root.join(rel_path))
}

/// `root/rel` with its symlinks resolved, when it exists and stays beneath the canonical
/// `root`, and its resolved name is no env file's.
pub(crate) fn resolve_inside(root: &Path, rel: &str) -> Result<PathBuf, ReadRefused> {
    let path = project_path(root, rel)?;
    let canonical_root =
        std::fs::canonicalize(root).map_err(|e| ReadRefused::Failed(e.kind().to_string()))?;
    let resolved = match std::fs::canonicalize(&path) {
        Ok(resolved) => resolved,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is not "absent": it points somewhere doctor does not follow.
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(ReadRefused::Outside);
            }
            // Absent, but only "absent in the checkout" when its nearest existing directory
            // resolves beneath the root (a symlinked directory may point out of it).
            let inside = path
                .ancestors()
                .skip(1)
                .find_map(|dir| std::fs::canonicalize(dir).ok())
                .is_some_and(|dir| dir.starts_with(&canonical_root));
            return Err(if inside {
                ReadRefused::NotFound
            } else {
                ReadRefused::Outside
            });
        }
        Err(e) => return Err(ReadRefused::Failed(e.kind().to_string())),
    };
    // Containment compares canonical paths component by component, case-sensitively. On a
    // case-insensitive volume (APFS by default) a link that names the checkout in another case
    // may canonicalize to that spelling and read as outside: a refusal, never an escape (review
    // t6-r2 R2-4, an accepted limitation).
    if !resolved.starts_with(&canonical_root) {
        return Err(ReadRefused::Outside);
    }
    if resolved
        .file_name()
        .is_some_and(|name| is_env_file_name(&name.to_string_lossy()))
    {
        return Err(ReadRefused::EnvFile);
    }
    Ok(resolved)
}

/// Reads `root/rel` as text (bounded), refusing env files, targets outside the checkout and
/// anything but one regular file.
pub(crate) fn read_text(root: &Path, rel: &str) -> Result<String, ReadRefused> {
    let resolved = resolve_inside(root, rel)?;
    let file = open_regular(&resolved)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ReadRefused::Failed(e.kind().to_string()))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ReadRefused::TooLarge);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Opens `path` read-only without following a symlink and without blocking (a FIFO's open
/// returns at once), and checks on the handle that it is a regular file.
fn open_regular(path: &Path) -> Result<std::fs::File, ReadRefused> {
    let file = open_no_follow(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ReadRefused::NotFound,
        _ => ReadRefused::NotAFile,
    })?;
    let metadata = file
        .metadata()
        .map_err(|e| ReadRefused::Failed(e.kind().to_string()))?;
    if !metadata.is_file() {
        return Err(ReadRefused::NotAFile);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        // A file symlink opened as itself is a reparse point, not the file it names.
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ReadRefused::NotAFile);
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env-file names are built, not written: `doctor_source_reads_no_env_file` fails on the
    /// quoted name anywhere under `src/doctor/`.
    fn dot(rest: &str) -> String {
        format!(".{rest}")
    }

    #[test]
    fn env_file_names_are_recognised() {
        for name in [
            dot("env"),
            dot("env.local"),
            dot("ENV.production"),
            dot("envrc"),
            "prod.env".into(),
        ] {
            assert!(is_env_file_name(&name), "{name}");
        }
        for name in ["AGENTS.md", "env.md", "hostler.toml", "environment.md"] {
            assert!(!is_env_file_name(name), "{name}");
        }
    }

    #[test]
    fn reads_refuse_env_files_and_non_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("AGENTS.md"), "ok").unwrap();
        assert_eq!(read_text(root, "AGENTS.md").unwrap(), "ok");
        assert_eq!(read_text(root, "missing.md"), Err(ReadRefused::NotFound));
        assert_eq!(read_text(root, "../x.md"), Err(ReadRefused::NotRelative));
        assert_eq!(read_text(root, "/etc/hosts"), Err(ReadRefused::NotRelative));
        assert_eq!(read_text(root, "config/.env"), Err(ReadRefused::EnvFile));
        std::fs::create_dir(root.join("docs")).unwrap();
        assert_eq!(read_text(root, "docs"), Err(ReadRefused::NotAFile));
    }

    /// R1-3: a symlink inside the checkout is read only when it resolves beneath the checkout,
    /// and never when it names an env file.
    #[cfg(unix)]
    #[test]
    fn symlinks_are_read_only_inside_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("AGENTS.md"), "not ours").unwrap();
        std::fs::write(root.join("real.md"), "ours").unwrap();
        std::fs::write(root.join("secrets.env"), "X=1").unwrap();
        let link = |name: &str, target: &Path| {
            std::os::unix::fs::symlink(target, root.join(name)).unwrap();
        };
        link("AGENTS.md", &outside.join("AGENTS.md"));
        link("inner.md", Path::new("real.md"));
        link("RULES.md", Path::new("secrets.env"));
        link("dangling.md", &outside.join("gone.md"));
        std::os::unix::fs::symlink(&outside, root.join("docs")).unwrap();
        assert_eq!(read_text(&root, "AGENTS.md"), Err(ReadRefused::Outside));
        assert_eq!(
            read_text(&root, "docs/AGENTS.md"),
            Err(ReadRefused::Outside)
        );
        assert_eq!(read_text(&root, "dangling.md"), Err(ReadRefused::Outside));
        assert_eq!(read_text(&root, "RULES.md"), Err(ReadRefused::EnvFile));
        assert_eq!(read_text(&root, "inner.md").unwrap(), "ours");
    }

    /// A FIFO is refused without blocking (the no-follow, non-blocking open).
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let made = std::process::Command::new("mkfifo")
            .arg(root.join("AGENTS.md"))
            .status();
        if !made.as_ref().is_ok_and(|status| status.success()) {
            eprintln!("note: mkfifo is unavailable ({made:?}); this case is skipped");
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(read_text(&root, "AGENTS.md"));
        });
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("opening a FIFO must not block"),
            Err(ReadRefused::NotAFile)
        );
    }
}
