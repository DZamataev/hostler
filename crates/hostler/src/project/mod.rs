//! Project resolution from the cwd (plan P2, Task 2).

pub mod config;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

use crate::contract::secrets::ProjectRef;
use crate::contract::{ErrorCode, HostlerError};

/// The checkout a cwd belongs to: where its `.git` is and where the repository's common dir is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
    pub workspace_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// The cwd's project: its checkout, optional `hostler.toml` and the ref sent over the contract.
#[derive(Clone, Debug)]
pub struct Current {
    pub checkout: Checkout,
    pub config: Option<config::ProjectConfig>,
    pub project: ProjectRef,
}

pub fn not_in_project(cwd: &Path) -> HostlerError {
    HostlerError {
        code: ErrorCode::NotInProject,
        message: format!(
            "{} is not inside a git checkout; hostler secrets belong to a project",
            display_path(cwd)
        ),
        details: json!({}),
        recovery: None,
    }
}

pub fn config_invalid(problems: &[config::ConfigProblem]) -> HostlerError {
    let lines: Vec<String> = problems
        .iter()
        .map(|problem| format!("line {}: {}", problem.line, problem.message))
        .collect();
    HostlerError {
        code: ErrorCode::ConfigInvalid,
        message: format!("hostler.toml has problems:\n{}", lines.join("\n")),
        details: json!({
            "problems": problems
                .iter()
                .map(|problem| json!({ "line": problem.line, "message": problem.message }))
                .collect::<Vec<_>>()
        }),
        recovery: None,
    }
}

/// A path fit for output and the contract: Windows verbatim prefixes are not useful to people.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.into_owned()
    }
}

/// Finds the checkout containing `cwd` by walking up to a `.git` directory or gitfile. No `git`
/// binary is run. Every resolved target must exist as a git directory with `HEAD`: a broken
/// checkout fails instead of becoming a new project identity.
pub fn find_checkout(cwd: &Path) -> Result<Checkout, HostlerError> {
    let start = fs::canonicalize(cwd).map_err(|_| not_in_project(cwd))?;
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let metadata = match fs::symlink_metadata(&dot_git) {
            Ok(_) => fs::metadata(&dot_git).map_err(|e| {
                unresolved(cwd, format!("cannot read {}: {e}", display_path(&dot_git)))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(unresolved(
                    cwd,
                    format!("cannot read {}: {e}", display_path(&dot_git)),
                ));
            }
        };

        if !metadata.is_dir() && !metadata.is_file() {
            return Err(unresolved(
                cwd,
                format!("{} is not a gitfile", display_path(&dot_git)),
            ));
        }
        let git_dir = if metadata.is_dir() {
            canonical_git_dir(&dot_git).map_err(|detail| unresolved(cwd, detail))?
        } else {
            let gitfile = fs::read_to_string(&dot_git).map_err(|e| {
                unresolved(
                    cwd,
                    format!("cannot read the gitfile {}: {e}", display_path(&dot_git)),
                )
            })?;
            let Some(target) = gitfile
                .lines()
                .next()
                .and_then(|line| line.trim().strip_prefix("gitdir:"))
                .map(str::trim)
                .filter(|target| !target.is_empty())
            else {
                return Err(unresolved(
                    cwd,
                    format!(
                        "{} is not a gitfile with a gitdir line",
                        display_path(&dot_git)
                    ),
                ));
            };
            canonical_git_dir(&dir.join(target)).map_err(|detail| unresolved(cwd, detail))?
        };
        let common_dir = resolve_common_dir(&git_dir, cwd)?;
        return Ok(Checkout {
            workspace_root: dir.to_path_buf(),
            git_dir,
            common_dir,
        });
    }
    Err(not_in_project(cwd))
}

/// The project's display name: the common dir's own name, or `.git`'s parent. Kept within the
/// global project-name rule (1–64 chars, no control characters).
pub fn default_name(common_dir: &Path) -> String {
    let file = common_dir.file_name().map(|name| name.to_string_lossy());
    let name = match file.as_deref() {
        Some(".git") => common_dir
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned()),
        Some(name) => Some(
            name.strip_suffix(".git")
                .filter(|stem| !stem.is_empty())
                .unwrap_or(name)
                .to_string(),
        ),
        None => None,
    };
    let name = name.unwrap_or_else(|| "project".into());
    name.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .take(64)
        .collect()
}

/// Resolves the cwd's project and reads its `hostler.toml`.
pub fn current(cwd: &Path) -> Result<Current, HostlerError> {
    let checkout = find_checkout(cwd)?;
    // The string is the project's persistent identity: a lossy conversion would merge checkouts.
    check_lossless(&checkout.common_dir)?;
    check_lossless(&checkout.workspace_root)?;
    let config = config::load(&checkout.workspace_root)?;
    warn_unknown_tables(config.as_ref());
    let name = config
        .as_ref()
        .and_then(|config| config.name.clone())
        .unwrap_or_else(|| default_name(&checkout.common_dir));
    let project = ProjectRef {
        common_dir: display_path(&checkout.common_dir),
        name,
    };
    Ok(Current {
        checkout,
        config,
        project,
    })
}

/// D5: unknown top-level tables are kept for later versions; tell a person once per command.
fn warn_unknown_tables(config: Option<&config::ProjectConfig>) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let Some(config) = config else {
        return;
    };
    if config.unknown_tables.is_empty() || WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    for table in &config.unknown_tables {
        eprintln!(
            "[hostler] hostler.toml: [{}] is not used by this hostler version",
            crate::secrets::sanitize_for_terminal(table)
        );
    }
}

fn unresolved(cwd: &Path, detail: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::NotInProject,
        message: format!(
            "cannot resolve the git checkout of {}: {detail}",
            display_path(cwd)
        ),
        details: json!({}),
        recovery: None,
    }
}

/// A git directory: it must resolve and contain `HEAD`. The error is the detail only; the
/// caller builds the message once (`unresolved`).
fn canonical_git_dir(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|e| format!("cannot resolve {}: {e}", display_path(path)))?;
    if !canonical.join("HEAD").is_file() {
        return Err(format!(
            "{} is not a git directory (it has no HEAD)",
            display_path(&canonical)
        ));
    }
    Ok(canonical)
}

/// `commondir` absent means a main checkout; present but unreadable, empty or unresolvable is an
/// error, never a fallback to the worktree's private git directory.
fn resolve_common_dir(git_dir: &Path, cwd: &Path) -> Result<PathBuf, HostlerError> {
    let commondir = git_dir.join("commondir");
    let contents = match fs::read_to_string(&commondir) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(git_dir.to_path_buf()),
        Err(e) => {
            return Err(unresolved(
                cwd,
                format!("cannot read {}: {e}", display_path(&commondir)),
            ));
        }
    };
    let target = contents.trim();
    if target.is_empty() {
        return Err(unresolved(
            cwd,
            format!("{} is empty", display_path(&commondir)),
        ));
    }
    canonical_git_dir(&git_dir.join(target)).map_err(|detail| {
        unresolved(
            cwd,
            format!(
                "the commondir {} does not resolve: {detail}",
                display_path(&commondir)
            ),
        )
    })
}

/// The project identity must exist in the contract's `String`; `display_path` only strips the
/// Windows verbatim prefix, so a UTF-8 path stays distinct from every other one.
fn check_lossless(path: &Path) -> Result<(), HostlerError> {
    if path.to_str().is_some() {
        return Ok(());
    }
    Err(HostlerError {
        code: ErrorCode::NotInProject,
        message: format!(
            "the checkout path {} is not valid UTF-8; hostler keeps checkouts apart by their \
             exact path",
            display_path(path)
        ),
        details: json!({}),
        recovery: None,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn display_path_strips_windows_verbatim_prefixes() {
        assert_eq!(display_path(Path::new(r"\\?\C:\work\app")), r"C:\work\app");
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\app")),
            r"\\server\share\app"
        );
        assert_eq!(display_path(Path::new("/work/app")), "/work/app");
    }

    #[test]
    fn default_name_uses_the_parent_of_a_dot_git_and_strips_a_git_suffix() {
        assert_eq!(default_name(Path::new("/work/my-app/.git")), "my-app");
        assert_eq!(default_name(Path::new("/work/bare/app.git")), "app");
        assert_eq!(
            default_name(Path::new(&format!("/work/{}", "n".repeat(80)))),
            "n".repeat(64)
        );
    }

    #[test]
    fn a_git_file_must_name_a_git_dir() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".git"), "not a gitfile\n").unwrap();
        let error = find_checkout(dir.path()).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotInProject);
    }

    #[test]
    fn missing_cwd_is_not_in_project() {
        let dir = tempdir().unwrap();
        let error = find_checkout(&dir.path().join("missing")).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotInProject);
    }
}
