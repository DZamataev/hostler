//! The marked hostler block in a project's agent docs (plan P4 Task 5): its text, where it
//! goes, and the in-place edit. Doctor's `project.docs` compares against `block_text` and writes
//! with `bring_current`, so this module's checks are the only way a block is written.
//!
//! The block holds no secret name and no app id: `<NAME>`, `<cmd>`, `<alias>` and `<why>` are
//! placeholders, always in angle brackets. It is at most 25 lines between its markers.
//!
//! **Never outside the checkout** (plan P4 review R1-1, R2-1, R2-2): the root must be a real
//! directory, never a symlink, and its canonical form is what every later path uses; a docs path
//! holds only ordinary relative components (no root, prefix, `.` or `..`); an ancestor below the
//! root must not be a symlink, at plan time and again right before the temporary file; the
//! canonicalized parent must equal the root-joined parent. A symlinked docs *file* is never
//! written through and never replaced: the placement rules decide (a `CLAUDE.md` that links to
//! `AGENTS.md` is covered by it).
//!
//! **The write itself** (R2-3, R2-4, R2-5): the final target must be a regular file or genuinely
//! missing — a directory, FIFO, socket, device or symlink is refused; a read-only file is
//! refused; the temporary file has a unique name in the same directory, is created exclusively
//! and without following a link, never removes anything that already existed, and the file's own
//! permissions are kept across the rename. The check-then-rename window on directories is the
//! accepted residual (as P4-L16 documents for detection).

use std::path::{Path, PathBuf};

use crate::contract::claims::FORCE_SENTENCE;
use crate::contract::init::{AgentDocKind, Detected, DocsAction, DocsChange};
use crate::project::config::{ConfigProblem, ProjectConfig};

/// The block's first line.
pub const BEGIN: &str = "<!-- hostler:begin -->";
/// The block's last line.
pub const END: &str = "<!-- hostler:end -->";

/// The most lines a block may have between (and including) its markers.
pub const MAX_LINES: usize = 25;

/// The whole block, markers included, in LF line endings (what doctor compares with).
pub fn block_text(config: &ProjectConfig) -> String {
    block(config, "\n")
}

/// The block in `eol` line endings, so a CRLF file stays CRLF.
pub fn block(config: &ProjectConfig, eol: &str) -> String {
    let mut lines: Vec<String> = vec![
        "## hostler".into(),
        String::new(),
        "Work in this project through hostler, so the agents on this machine share one queue, one \
         screen and one set of devices."
            .into(),
        String::new(),
    ];
    let heavy = config.heavy_commands();
    let mut heavy_line = "- Heavy builds: `hostler run --heavy -- <cmd>`, so they queue with the \
                          other agents' work."
        .to_string();
    if !heavy.is_empty() {
        let shown: Vec<&str> = heavy.iter().take(6).map(String::as_str).collect();
        heavy_line.push_str(&format!(" This project's: {}", shown.join(", ")));
        if heavy.len() > shown.len() {
            heavy_line.push_str(&format!(", and {} more", heavy.len() - shown.len()));
        }
    }
    lines.push(heavy_line);
    lines.push(
        "- A device, for anything that installs or launches on a phone, emulator or simulator \
         (`npx react-native run-android`, `./gradlew installDebug`): `hostler run --device \
         <alias|auto:android> -- <cmd>`, or hold one first with `hostler hold device`."
            .into(),
    );
    lines.push(
        "- A secret: `hostler run --secret <NAME> -- <cmd>`; ask for a missing one with \
         `hostler secret request <NAME> --reason \"<why>\"`."
            .into(),
    );
    lines.push(
        "  The names are this project's `[secrets]` in hostler.toml; list them with: hostler \
         secret list"
            .into(),
    );
    lines.push("- The real GUI: `hostler hold screen` before driving it.".into());
    lines.push(format!("- {FORCE_SENTENCE}"));
    lines.push(
        "- Start here: `hostler guide start`; check with `hostler doctor`; in an agent sandbox: \
         `hostler guide sandbox`."
            .into(),
    );
    lines.push(String::new());
    lines.push(format!(
        "Device-handling rules for this project: {}",
        device_rules(config)
    ));
    let mut out = String::from(BEGIN);
    out.push_str(eol);
    for line in lines {
        out.push_str(&line);
        out.push_str(eol);
    }
    out.push_str(END);
    out.push_str(eol);
    out
}

fn device_rules(config: &ProjectConfig) -> String {
    match &config.agents.device_rules {
        Some(rules) => match &rules.heading {
            Some(heading) => format!("{}#{heading}", rules.path),
            None => rules.path.clone(),
        },
        None => "not written yet — see hostler init".into(),
    }
}

/// The line endings of `text`: CRLF when it holds one, LF otherwise.
pub fn line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Where the block is in `text`: the byte span from the beginning of the `begin` marker's line
/// to the end of the `end` marker's line (its newline included). `None` when there is no block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A marker problem: one line, never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerProblem {
    pub line: usize,
    pub message: String,
}

/// Finds the one block in `text`. A second `begin`, a second `end`, an `end` before the `begin`
/// or a `begin` without an `end` is a problem, with its line (plan P4 Task 5, review R1-3:
/// nothing is written then). Line endings may be CRLF.
pub fn find(text: &str) -> Result<Option<Span>, MarkerProblem> {
    let mut begin: Option<(usize, usize)> = None;
    let mut end: Option<(usize, usize)> = None;
    let mut offset = 0;
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']).trim();
        if line == BEGIN {
            if begin.is_some() {
                return Err(MarkerProblem {
                    line: index + 1,
                    message: format!("two `{BEGIN}` markers: one block per file"),
                });
            }
            begin = Some((index + 1, start));
        } else if line == END {
            if begin.is_none() {
                return Err(MarkerProblem {
                    line: index + 1,
                    message: format!("`{END}` before any `{BEGIN}`"),
                });
            }
            if end.is_some() {
                return Err(MarkerProblem {
                    line: index + 1,
                    message: format!("two `{END}` markers: one block per file"),
                });
            }
            end = Some((index + 1, offset));
        }
    }
    match (begin, end) {
        (None, None) => Ok(None),
        (Some((line, _)), None) => Err(MarkerProblem {
            line,
            message: format!("`{BEGIN}` without a `{END}`"),
        }),
        (Some((_, start)), Some((_, end))) => Ok(Some(Span { start, end })),
        (None, Some(_)) => unreachable!("an `end` without a `begin` is returned above"),
    }
}

/// Whether `text` holds exactly the current block for `config`.
pub fn is_current(text: &str, config: &ProjectConfig) -> Result<bool, MarkerProblem> {
    Ok(match find(text)? {
        None => false,
        Some(span) => {
            text[span.start..span.end].trim_end_matches(['\n', '\r'])
                == block(config, line_ending(text)).trim_end_matches(['\n', '\r'])
        }
    })
}

/// One planned change: what the report says, and the file's new bytes when it changes.
#[derive(Clone, Debug)]
pub struct Planned {
    pub change: DocsChange,
    /// The content to write; `None` when the file already holds the block.
    pub content: Option<Vec<u8>>,
}

/// What init writes into the agent docs: the block into `AGENTS.md`, into a plain `CLAUDE.md`,
/// or into the project's `[agents] docs` (in their order), and nothing into a `CLAUDE.md` that
/// links to or imports another doc. `.github/copilot-instructions.md` is never written: it is
/// listed by detection only.
///
/// `configured` says the `[agents] docs` list came from an existing `hostler.toml` (a person's
/// or an earlier init's choice). Without one, placement follows `Detected`: the proposal's own
/// `docs` list is what init is about to write, so it cannot override it.
///
/// A symlink is never written through, and a rename would replace it with a regular file, so a
/// symlinked target that no `AgentDocKind` explains is left alone (doctor reports it).
pub fn plan(
    root: &Path,
    detected: &Detected,
    config: &ProjectConfig,
    configured: bool,
) -> Result<Vec<Planned>, ConfigProblem> {
    let mut planned = Vec::new();
    if configured && !config.agents.docs.is_empty() {
        for rel in &config.agents.docs {
            if let Some(entry) = plan_file(root, rel, config)? {
                planned.push(entry);
            }
        }
        return Ok(planned);
    }
    for doc in &detected.agent_docs {
        match (&doc.path[..], &doc.kind) {
            (".github/copilot-instructions.md", _) => {}
            // A doc that links to or imports `AGENTS.md` holds no block of its own: the block is
            // already there. A link to any other file is left alone (its target may have none).
            (path, AgentDocKind::SymlinkTo { target } | AgentDocKind::Imports { target })
                if target == "AGENTS.md" =>
            {
                planned.push(Planned {
                    change: DocsChange {
                        path: path.to_string(),
                        action: DocsAction::CoveredBy {
                            path: target.clone(),
                        },
                    },
                    content: None,
                });
            }
            (_, AgentDocKind::SymlinkTo { .. } | AgentDocKind::Imports { .. }) => {}
            (path, AgentDocKind::File) => {
                if let Some(entry) = plan_file(root, path, config)? {
                    planned.push(entry);
                }
            }
        }
    }
    // `AGENTS.md` gets the block even when the checkout has no agent docs yet.
    let agents_md_planned = planned.iter().any(|entry| entry.change.path == "AGENTS.md")
        || detected
            .agent_docs
            .iter()
            .any(|doc| doc.path == "AGENTS.md");
    if !agents_md_planned && let Some(entry) = plan_file(root, "AGENTS.md", config)? {
        planned.push(entry);
    }
    Ok(planned)
}

/// The change to one file, or `None` for a symlink that `Detected` did not explain.
fn plan_file(
    root: &Path,
    rel: &str,
    config: &ProjectConfig,
) -> Result<Option<Planned>, ConfigProblem> {
    let problem = |line: usize, message: String| ConfigProblem {
        line,
        message: format!("{}: {message}", rel.replace('\n', " ")),
    };
    // The root and the path first, then the directory chain: a symlinked root or ancestor would
    // take the write outside the checkout.
    let root = canonical_root(root, rel)?;
    let rel_path = checked_relative(rel)?;
    let path = root.join(&rel_path);
    checked_parent(&root, &rel_path, rel)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let text = block(config, "\n").into_bytes();
            return Ok(Some(Planned {
                change: DocsChange {
                    path: rel.to_string(),
                    action: DocsAction::Create,
                },
                content: Some(text),
            }));
        }
        Err(error) => {
            return Err(problem(
                1,
                format!("cannot read the file: {}", error.kind()),
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        // Never write through a link, and never replace it: nothing to plan.
        return Ok(None);
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    if is_read_only(&metadata) {
        return Err(problem(
            1,
            "the file is read-only; make it writable to let init update the block".into(),
        ));
    }
    let bytes = match read_regular(&path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Err(problem(
                1,
                format!("cannot read the file: {}", error.kind()),
            ));
        }
    };
    if bytes.len() > MAX_DOCS_BYTES {
        return Err(problem(
            1,
            format!("is larger than {} MiB", MAX_DOCS_BYTES / (1024 * 1024)),
        ));
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => return Err(problem(1, "the file is not valid UTF-8".into())),
    };
    let eol = line_ending(text);
    let block = block(config, eol);
    let (content, action) = match find(text).map_err(|e| problem(e.line, e.message))? {
        Some(span) => {
            let mut next = String::with_capacity(text.len() + block.len());
            next.push_str(&text[..span.start]);
            // The span swallowed the marker lines' newline; a block at the end of the file
            // keeps it, one in the middle regains it from the rest of the file.
            let ends_with_newline = text[span.start..span.end].ends_with('\n');
            next.push_str(block.strip_suffix(eol).unwrap_or(&block));
            if ends_with_newline {
                next.push_str(eol);
            }
            next.push_str(&text[span.end..]);
            let action = if next == text {
                DocsAction::Unchanged
            } else {
                DocsAction::Update
            };
            (next, action)
        }
        None => {
            let mut next = String::with_capacity(text.len() + block.len() + 2);
            next.push_str(text);
            if !text.is_empty() {
                if !text.ends_with('\n') {
                    next.push_str(eol);
                }
                if !next.ends_with(&format!("{eol}{eol}")) {
                    next.push_str(eol);
                }
            }
            next.push_str(&block);
            (next, DocsAction::Insert)
        }
    };
    let content = (action != DocsAction::Unchanged).then(|| content.into_bytes());
    Ok(Some(Planned {
        change: DocsChange {
            path: rel.to_string(),
            action,
        },
        content,
    }))
}

/// Writes every change that has content: a temp file in the same directory, then a rename.
/// The root, the path, the directory chain and the file are checked again here, so a change
/// planned earlier cannot escape the checkout or replace something that is no longer a writable
/// regular file (P4-L21, P4 review R2-1..R2-4).
pub fn apply(root: &Path, planned: &[Planned]) -> Result<Vec<String>, ConfigProblem> {
    let mut written = Vec::new();
    for entry in planned {
        let Some(content) = &entry.content else {
            continue;
        };
        write_doc(root, &entry.change.path, content)?;
        written.push(entry.change.path.clone());
    }
    Ok(written)
}

/// Writes one docs file (`bring_current`'s and `apply`'s one implementation).
fn write_doc(root: &Path, rel: &str, content: &[u8]) -> Result<(), ConfigProblem> {
    let canonical = canonical_root(root, rel)?;
    let rel_path = checked_relative(rel)?;
    let parent = checked_parent(&canonical, &rel_path, rel)?;
    if std::fs::symlink_metadata(&parent).is_err() {
        std::fs::create_dir_all(&parent).map_err(|error| {
            path_problem(
                rel,
                &format!("cannot create its directory: {}", error.kind()),
            )
        })?;
    }
    // Again now that the directory exists: a component swapped for a symlink is caught here.
    checked_parent(&canonical, &rel_path, rel)?;
    let path = canonical.join(&rel_path);
    let mode = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(path_problem(
                rel,
                "the file is a symlink; it is never written through",
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(path_problem(
                rel,
                "the file is not a regular file; it is never replaced",
            ));
        }
        Ok(metadata) if is_read_only(&metadata) => {
            return Err(path_problem(
                rel,
                "the file is read-only; make it writable to let init update the block",
            ));
        }
        // The file's own permissions are kept across the replacement (R2-5).
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(path_problem(
                rel,
                &format!("cannot read the file: {}", error.kind()),
            ));
        }
    };
    write_atomic(&path, content, mode)
        .map_err(|error| path_problem(rel, &format!("cannot write it: {}", error.kind())))
}

/// Brings one configured docs file to the current block: doctor's `project.docs` fix. The
/// checks are `plan_file`'s and `write_doc`'s, so this is the same path `plan` + `apply` take.
/// The written paths; empty when the file already holds the block or is left alone.
pub fn bring_current(
    root: &Path,
    rel: &str,
    config: &ProjectConfig,
) -> Result<Vec<String>, ConfigProblem> {
    let Some(entry) = plan_file(root, rel, config)? else {
        return Ok(Vec::new());
    };
    if entry.content.is_none() {
        return Ok(Vec::new());
    }
    apply(root, std::slice::from_ref(&entry))
}

/// A docs path problem: one line. `rel` is the file's path inside the checkout, never a secret.
fn path_problem(rel: &str, message: &str) -> ConfigProblem {
    ConfigProblem {
        line: 1,
        message: format!("{}: {message}", rel.replace('\n', " ")),
    }
}

/// The checkout root the writer accepts: a real directory, never a symlink. The canonical form
/// is what every later path uses, so nothing is written through a link on the root's way
/// (P4 review R2-1). A root reached through a symlinked *ancestor* (macOS's `/var`) is the same
/// directory canonicalized, and that canonical path is used.
fn canonical_root(root: &Path, rel: &str) -> Result<PathBuf, ConfigProblem> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(path_problem(
                rel,
                "the checkout root is a symlink; init does not write through it",
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(path_problem(rel, "the checkout root is not a directory"));
        }
        Ok(_) => {}
        Err(error) => {
            return Err(path_problem(
                rel,
                &format!("cannot resolve the checkout: {}", error.kind()),
            ));
        }
    }
    std::fs::canonicalize(root).map_err(|error| {
        path_problem(
            rel,
            &format!("cannot resolve the checkout: {}", error.kind()),
        )
    })
}

/// A docs path the writer accepts: `/`-separated, relative, and only ordinary components (P4
/// review R2-2). A drive (`C:\x`), a UNC or verbatim path (`\\server…`, `\\?\…`), an absolute
/// path, a backslash, `.` and `..` are refused on every OS, whatever the caller validated.
fn checked_relative(rel: &str) -> Result<PathBuf, ConfigProblem> {
    let refused = || {
        path_problem(
            rel,
            "a docs path must be a relative path of ordinary components inside the checkout",
        )
    };
    if rel.is_empty()
        || rel.contains('\\')
        || rel.starts_with('/')
        || rel.as_bytes().get(1) == Some(&b':')
    {
        return Err(refused());
    }
    let mut path = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            std::path::Component::Normal(part) => path.push(part),
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir
            | std::path::Component::ParentDir => return Err(refused()),
        }
    }
    if path.as_os_str().is_empty() {
        return Err(refused());
    }
    Ok(path)
}

/// The file's directory, verified: every existing component between the canonical checkout root
/// and the file is a real directory (never a symlink), and the canonicalized parent is exactly
/// the root-joined parent — so the write stays inside the canonical checkout. A missing chain is
/// fine: `write_doc` creates it. A check-then-rename window on directories is the accepted
/// residual (P4-L16).
fn checked_parent(root: &Path, rel: &Path, text: &str) -> Result<PathBuf, ConfigProblem> {
    let mut parent = PathBuf::new();
    if let Some(rel_parent) = rel.parent() {
        for component in rel_parent.components() {
            let std::path::Component::Normal(part) = component else {
                return Err(path_problem(
                    text,
                    "a docs path must be a relative path of ordinary components inside the checkout",
                ));
            };
            parent.push(part);
            match std::fs::symlink_metadata(root.join(&parent)) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(path_problem(
                        text,
                        "a directory on the path is a symlink; keep the docs file inside the checkout",
                    ));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(path_problem(
                        text,
                        "a directory on the path is not a directory",
                    ));
                }
                Ok(_) => {}
                // Missing: created for the first time, so nothing deeper can exist to follow.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(path_problem(
                        text,
                        &format!("cannot check a directory on the path: {}", error.kind()),
                    ));
                }
            }
        }
    }
    let joined = root.join(&parent);
    match std::fs::canonicalize(&joined) {
        Ok(canonical) if canonical == joined => Ok(joined),
        Ok(_) => Err(path_problem(
            text,
            "the file's directory is not the checkout's",
        )),
        // Created by `write_doc`; the components above were checked.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(joined),
        Err(error) => Err(path_problem(
            text,
            &format!("cannot resolve the file's directory: {}", error.kind()),
        )),
    }
}

/// Whether the file refuses writes for its owner (a read-only docs file is never replaced).
#[cfg(unix)]
fn is_read_only(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o200 == 0
}

#[cfg(windows)]
fn is_read_only(metadata: &std::fs::Metadata) -> bool {
    metadata.permissions().readonly()
}

#[cfg(not(any(unix, windows)))]
fn is_read_only(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// A docs file at most this big (a file that must be edited whole).
const MAX_DOCS_BYTES: usize = 2 * 1024 * 1024;

/// Reads a docs file without following a symlink and without blocking, through the handle
/// itself (the check Task 3's `detect::open_regular` makes). `None` for anything that is not a
/// regular file; `NotFound` is `Ok(None)` too.
fn read_regular(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;

    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        // A file symlink opened as itself is a reparse point, not the file it names.
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(None);
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_DOCS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// Read-only, without following a symlink and without blocking: `O_NONBLOCK` makes a FIFO's open
/// return at once, and reading a regular file is unaffected by it.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Read-only with the reparse point itself as the object, never its target.
#[cfg(windows)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// A platform with neither Unix nor Windows file flags: the open follows what the path names, and
/// the handle check still rejects everything that is not a regular file.
#[cfg(not(any(unix, windows)))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The temp file, then the rename: a reader never sees a half-written file. The temp name is
/// unique (`.<name>.hostler-<pid>-<random>`), created exclusively and without following a link;
/// nothing that already existed is removed, and only this call's own temp is cleaned up (P4
/// review R2-3). `mode`: the target's own permissions, kept across the replacement (R2-5).
fn write_atomic(
    path: &Path,
    content: &[u8],
    mode: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hostler-docs".into());
    let (temp, mut file) = create_temp(parent, &name)?;
    if let Err(error) = write_and_sync(&mut file, content, mode) {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    // Windows: the rename needs the handle closed.
    drop(file);
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn write_and_sync(
    file: &mut std::fs::File,
    content: &[u8],
    mode: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;

    file.write_all(content)?;
    file.sync_all()?;
    if let Some(mode) = mode {
        // Best effort: a filesystem that cannot keep it does not fail the write.
        let _ = file.set_permissions(mode);
    }
    Ok(())
}

/// A free `.<name>.hostler-<pid>-<random>` in `parent`, created exclusively and without
/// following a link. A name that already exists — a file, a symlink, a directory — is skipped,
/// never removed.
fn create_temp(parent: &Path, name: &str) -> std::io::Result<(PathBuf, std::fs::File)> {
    for _ in 0..16 {
        let temp = parent.join(format!(
            ".{name}.hostler-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        match create_new(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "cannot find a free temporary name",
    ))
}

/// A new file at `path`, exclusively: an existing file, a symlink or anything else at that name
/// fails rather than being followed, replaced or removed.
fn create_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use crate::project::config::{Agents, DocRef, parse, render};

    use super::*;

    fn config() -> ProjectConfig {
        parse(
            "[project]\n\
             name = \"muse\"\n\
             \n\
             [apps]\n\
             android = [\"com.example.muse\"]\n\
             \n\
             [secrets.JACKETT_API_KEY]\n\
             purpose = \"the torrent search key\"\n\
             \n\
             [heavy]\n\
             commands = [\"./gradlew\", \"pod install\"]\n\
             \n\
             [agents]\n\
             docs = [\"AGENTS.md\"]\n\
             device_rules = \"AGENTS.md#device-handling\"\n",
        )
        .unwrap()
    }

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    fn detected(paths: &[(&str, AgentDocKind)]) -> Detected {
        Detected {
            agent_docs: paths
                .iter()
                .map(|(path, kind)| crate::contract::init::AgentDoc {
                    path: (*path).to_string(),
                    kind: kind.clone(),
                })
                .collect(),
            ..Detected::default()
        }
    }

    #[test]
    fn the_block_is_short_and_has_only_placeholders() {
        let config = config();
        let text = block_text(&config);
        let body: Vec<&str> = text.lines().collect();
        assert!(body.len() <= MAX_LINES, "{} lines:\n{text}", body.len());
        assert_eq!(body.first(), Some(&BEGIN));
        assert_eq!(body.last(), Some(&END));
        assert!(text.contains("## hostler"));

        // The plan's phrases.
        assert!(text.contains("hostler run --heavy -- <cmd>"), "{text}");
        assert!(text.contains("./gradlew"), "{text}");
        assert!(
            text.contains("hostler run --device <alias|auto:android>"),
            "{text}"
        );
        assert!(text.contains("hostler hold device"), "{text}");
        assert!(
            text.contains("hostler run --secret <NAME> -- <cmd>"),
            "{text}"
        );
        assert!(
            text.contains("hostler secret request <NAME> --reason \"<why>\""),
            "{text}"
        );
        assert!(
            text.contains("`[secrets]` in hostler.toml; list them with: hostler secret list"),
            "{text}"
        );
        assert!(text.contains("hostler hold screen"), "{text}");
        assert!(text.contains(FORCE_SENTENCE), "{text}");
        assert!(text.contains("hostler guide start"), "{text}");
        assert!(text.contains("hostler doctor"), "{text}");
        assert!(text.contains("hostler guide sandbox"), "{text}");
        assert!(
            text.contains("Device-handling rules for this project: AGENTS.md#device-handling"),
            "{text}"
        );

        // No secret name and no app id.
        assert!(!text.contains("JACKETT_API_KEY"), "{text}");
        assert!(!text.contains("com.example.muse"), "{text}");
    }

    #[test]
    fn the_block_lists_at_most_six_heavy_commands() {
        let commands: Vec<String> = (0..9).map(|n| format!("make task{n}")).collect();
        let config = ProjectConfig {
            heavy: commands.clone(),
            ..ProjectConfig::default()
        };
        let text = block_text(&config);
        assert!(text.contains("make task0, make task1"), "{text}");
        assert!(!text.contains("make task6"), "{text}");
        assert!(text.contains("and 3 more"), "{text}");
        assert!(text.lines().count() <= MAX_LINES, "{text}");
        assert!(
            text.contains("Device-handling rules for this project: not written yet"),
            "{text}"
        );
    }

    #[test]
    fn a_missing_file_is_created_with_only_the_block() {
        let dir = tree(&[]);
        let config = config();
        let planned = plan(dir.path(), &detected(&[]), &config, false).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].change.path, "AGENTS.md");
        assert_eq!(planned[0].change.action, DocsAction::Create);
        apply(dir.path(), &planned).unwrap();
        let text = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(text, block_text(&config));
    }

    #[test]
    fn a_new_block_is_appended_after_one_blank_line() {
        let dir = tree(&[("AGENTS.md", "# muse\n\nA synthetic checkout.")]);
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config(),
            false,
        )
        .unwrap();
        assert_eq!(planned[0].change.action, DocsAction::Insert);
        apply(dir.path(), &planned).unwrap();
        let text = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let expected = format!(
            "# muse\n\nA synthetic checkout.\n\n{}",
            block_text(&config())
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn a_file_ending_in_a_blank_line_gets_no_second_one() {
        let dir = tree(&[("AGENTS.md", "# muse\n\n")]);
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config(),
            false,
        )
        .unwrap();
        apply(dir.path(), &planned).unwrap();
        let text = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(text, format!("# muse\n\n{}", block_text(&config())));
    }

    #[test]
    fn the_block_is_replaced_in_place_and_crlf_survives() {
        let dir = tree(&[]);
        let config = config();
        // User text above and below, CRLF, and an older block in between.
        let before = "# muse\r\n\r\nUser notes above.\r\n\r\n<!-- hostler:begin -->\r\n## hostler\r\nold\r\n<!-- hostler:end -->\r\n\r\nUser notes below.\r\n";
        std::fs::write(dir.path().join("AGENTS.md"), before).unwrap();
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            false,
        )
        .unwrap();
        assert_eq!(planned[0].change.action, DocsAction::Update);
        apply(dir.path(), &planned).unwrap();
        let after = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let block = block(&config, "\r\n");
        assert_eq!(
            after,
            format!("# muse\r\n\r\nUser notes above.\r\n\r\n{block}\r\nUser notes below.\r\n")
        );
        assert!(!after.contains("\n\n\n"), "{after:?}");
        // Everything outside the block is byte for byte what it was.
        assert!(after.starts_with("# muse\r\n\r\nUser notes above.\r\n\r\n"));
        assert!(after.ends_with("\r\n\r\nUser notes below.\r\n"));
        // A second run changes nothing.
        let again = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            false,
        )
        .unwrap();
        assert_eq!(again[0].change.action, DocsAction::Unchanged);
        assert!(again[0].content.is_none());
    }

    #[test]
    fn a_block_at_the_end_keeps_its_newline() {
        let dir = tree(&[]);
        let config = config();
        let before = format!("# muse\n\n{}", block_text(&config));
        std::fs::write(dir.path().join("AGENTS.md"), &before).unwrap();
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            false,
        )
        .unwrap();
        assert_eq!(planned[0].change.action, DocsAction::Unchanged);
    }

    #[test]
    fn broken_markers_are_a_problem() {
        let config = config();
        for (body, line, needle) in [
            (
                "<!-- hostler:begin -->\nx\n<!-- hostler:end -->\n<!-- hostler:begin -->\ny\n",
                4,
                "two",
            ),
            // R1-3: a second end marker, with LF and with CRLF.
            (
                "<!-- hostler:begin -->\nx\n<!-- hostler:end -->\ny\n<!-- hostler:end -->\n",
                5,
                "two",
            ),
            (
                "<!-- hostler:begin -->\r\nx\r\n<!-- hostler:end -->\r\ny\r\n<!-- hostler:end -->\r\n",
                5,
                "two",
            ),
            ("<!-- hostler:begin -->\nx\n", 1, "without"),
            ("x\n<!-- hostler:end -->\n", 2, "before any"),
        ] {
            let dir = tree(&[("AGENTS.md", body)]);
            let found = plan(
                dir.path(),
                &detected(&[("AGENTS.md", AgentDocKind::File)]),
                &config,
                false,
            )
            .expect_err("broken markers");
            assert_eq!(found.line, line, "{found:?}");
            assert!(found.message.contains(needle), "{found:?}");
            // Nothing was written.
            assert_eq!(
                std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
                body
            );
        }
    }

    #[test]
    fn a_claude_md_that_links_or_imports_is_covered() {
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let config = ProjectConfig {
            agents: Agents {
                docs: Vec::new(),
                device_rules: Some(DocRef {
                    path: "AGENTS.md".into(),
                    heading: None,
                }),
            },
            ..ProjectConfig::default()
        };
        for kind in [
            AgentDocKind::SymlinkTo {
                target: "AGENTS.md".into(),
            },
            AgentDocKind::Imports {
                target: "AGENTS.md".into(),
            },
        ] {
            let detected = detected(&[("AGENTS.md", AgentDocKind::File), ("CLAUDE.md", kind)]);
            let planned = plan(dir.path(), &detected, &config, false).unwrap();
            let claude = planned
                .iter()
                .find(|entry| entry.change.path == "CLAUDE.md")
                .expect("a docs entry for CLAUDE.md");
            assert_eq!(
                claude.change.action,
                DocsAction::CoveredBy {
                    path: "AGENTS.md".into()
                }
            );
            assert!(claude.content.is_none());
        }
    }

    #[test]
    fn a_plain_claude_md_gets_a_block_and_copilot_does_not() {
        let dir = tree(&[("AGENTS.md", "# muse\n"), ("CLAUDE.md", "# claude\n")]);
        let config = ProjectConfig::default();
        let planned = plan(
            dir.path(),
            &detected(&[
                ("AGENTS.md", AgentDocKind::File),
                ("CLAUDE.md", AgentDocKind::File),
                (
                    ".github/copilot-instructions.md",
                    AgentDocKind::Imports {
                        target: "AGENTS.md".into(),
                    },
                ),
            ]),
            &config,
            false,
        )
        .unwrap();
        let paths: Vec<&str> = planned
            .iter()
            .map(|entry| entry.change.path.as_str())
            .collect();
        assert_eq!(paths, ["AGENTS.md", "CLAUDE.md"]);
        assert!(planned.iter().all(|entry| entry.content.is_some()));
    }

    #[test]
    fn the_agents_docs_list_overrides_placement_and_order() {
        let dir = tree(&[("docs/team.md", "# team\n")]);
        let config = ProjectConfig {
            agents: Agents {
                docs: vec!["docs/team.md".into()],
                device_rules: None,
            },
            ..ProjectConfig::default()
        };
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            true,
        )
        .unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].change.path, "docs/team.md");
        assert_eq!(planned[0].change.action, DocsAction::Insert);
    }

    #[test]
    fn a_symlink_is_never_written_through() {
        #[cfg(unix)]
        {
            let dir = tree(&[("real.md", "# real\n")]);
            std::os::unix::fs::symlink("real.md", dir.path().join("CLAUDE.md")).unwrap();
            let planned = plan(
                dir.path(),
                &detected(&[("CLAUDE.md", AgentDocKind::File)]),
                &ProjectConfig::default(),
                false,
            )
            .unwrap();
            assert!(
                planned.iter().all(|entry| entry.change.path != "CLAUDE.md"),
                "{planned:?}"
            );
            assert!(
                std::fs::symlink_metadata(dir.path().join("CLAUDE.md"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_parent_is_refused_at_plan_time() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("out.md"), "external\n").unwrap();
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("a/b/link")).unwrap();

        // A configured nested path, as `[agents] docs` may hold it.
        for rel in ["link/out.md", "link/new.md", "a/b/link/out.md"] {
            let config = ProjectConfig {
                agents: Agents {
                    docs: vec![rel.to_string()],
                    device_rules: None,
                },
                ..ProjectConfig::default()
            };
            let found = plan(dir.path(), &detected(&[]), &config, true)
                .expect_err("a symlinked parent must be refused");
            assert!(found.message.contains(rel), "{found:?}");
            assert!(found.message.contains("symlink"), "{found:?}");
        }
        // Nothing outside the checkout was created or changed.
        assert_eq!(
            std::fs::read_to_string(outside.path().join("out.md")).unwrap(),
            "external\n"
        );
        assert!(!outside.path().join("new.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn apply_rechecks_the_parent_before_writing() {
        // Planned while the directory was missing, then made a symlink before the write.
        let outside = tempfile::tempdir().unwrap();
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let config = ProjectConfig {
            agents: Agents {
                docs: vec!["docs/new.md".into()],
                device_rules: None,
            },
            ..ProjectConfig::default()
        };
        let planned = plan(dir.path(), &detected(&[]), &config, true).unwrap();
        assert_eq!(planned[0].change.action, DocsAction::Create);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("docs")).unwrap();

        let found = apply(dir.path(), &planned).expect_err("a symlinked parent at apply time");
        assert!(found.message.contains("docs/new.md"), "{found:?}");
        assert!(found.message.contains("symlink"), "{found:?}");
        assert!(!outside.path().join("new.md").exists());

        // A file that became a symlink between the plan and the write is refused too.
        let outside_file = outside.path().join("agents.md");
        std::fs::write(&outside_file, "outside\n").unwrap();
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let config = ProjectConfig {
            agents: Agents {
                docs: vec!["AGENTS.md".into()],
                device_rules: None,
            },
            ..ProjectConfig::default()
        };
        let planned = plan(dir.path(), &detected(&[]), &config, true).unwrap();
        assert_eq!(planned[0].change.action, DocsAction::Insert);
        std::fs::remove_file(dir.path().join("AGENTS.md")).unwrap();
        std::os::unix::fs::symlink(&outside_file, dir.path().join("AGENTS.md")).unwrap();
        let found = apply(dir.path(), &planned).expect_err("a symlinked file at apply time");
        assert!(found.message.contains("symlink"), "{found:?}");
        assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "outside\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_never_written_even_by_bring_current() {
        let dir = tree(&[("real.md", "# real\n")]);
        std::os::unix::fs::symlink("real.md", dir.path().join("AGENTS.md")).unwrap();
        let written = bring_current(dir.path(), "AGENTS.md", &config()).unwrap();
        assert!(written.is_empty(), "{written:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real.md")).unwrap(),
            "# real\n"
        );
    }

    #[test]
    fn bring_current_is_the_one_write_path() {
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let config = config();
        assert_eq!(
            bring_current(dir.path(), "AGENTS.md", &config).unwrap(),
            ["AGENTS.md"]
        );
        let text = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(is_current(&text, &config).unwrap());
        assert!(text.contains("## hostler"), "{text}");
        assert!(
            bring_current(dir.path(), "AGENTS.md", &config)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_refused() {
        // R2-1: the shared writer takes no root that is a symlink, inside or outside the tree.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("AGENTS.md"), "# outside\n").unwrap();
        let inside = tempfile::tempdir().unwrap();
        std::fs::write(inside.path().join("AGENTS.md"), "# inside\n").unwrap();
        let holder = tempfile::tempdir().unwrap();
        for (name, target, body) in [
            ("to-outside", outside.path(), "# outside\n"),
            ("to-inside", inside.path(), "# inside\n"),
        ] {
            let link = holder.path().join(name);
            std::os::unix::fs::symlink(target, &link).unwrap();
            let found = bring_current(&link, "AGENTS.md", &config())
                .expect_err("a symlinked root must be refused");
            assert!(found.message.contains("root is a symlink"), "{found:?}");
            assert_eq!(
                std::fs::read_to_string(target.join("AGENTS.md")).unwrap(),
                body,
                "{name}"
            );
            // Nothing else was created there.
            let entries = std::fs::read_dir(target).unwrap().count();
            assert_eq!(entries, 1, "{name}: {entries} entries");
        }
    }

    #[test]
    fn a_docs_path_must_be_relative() {
        // R2-2: whatever the caller validated, the writer accepts only ordinary components.
        assert_eq!(
            checked_relative("AGENTS.md").unwrap(),
            PathBuf::from("AGENTS.md")
        );
        assert_eq!(
            checked_relative("docs/team.md").unwrap(),
            PathBuf::from("docs/team.md")
        );
        let outside = tempfile::tempdir().unwrap();
        let absolute = outside.path().join("outside.md");
        let absolute = absolute.to_str().unwrap().to_string();
        let mut refused: Vec<String> = vec![
            "/AGENTS.md".into(),
            "//server/share/AGENTS.md".into(),
            r"C:\x.md".into(),
            "C:/x.md".into(),
            r"\\?\C:\x.md".into(),
            r"\\server\share\x.md".into(),
            "a/../b.md".into(),
            "../b.md".into(),
            "./AGENTS.md".into(),
            r"a\b.md".into(),
            String::new(),
            ".".into(),
            "..".into(),
        ];
        refused.push(absolute);
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        for rel in &refused {
            match bring_current(dir.path(), rel, &config()) {
                Ok(written) => panic!("{rel:?} was accepted: {written:?}"),
                Err(found) => assert!(
                    found.message.contains("relative path"),
                    "{rel:?}: {found:?}"
                ),
            }
        }
        // Nothing outside the checkout was touched.
        assert!(!outside.path().join("outside.md").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
            "# muse\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_existing_temp_name_is_never_removed() {
        // R2-3: the old fixed temp name is not ours to delete.
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let user_file = dir.path().join(".AGENTS.md.hostler-tmp");
        std::fs::write(&user_file, "user data\n").unwrap();
        assert_eq!(
            bring_current(dir.path(), "AGENTS.md", &config()).unwrap(),
            ["AGENTS.md"]
        );
        assert_eq!(
            std::fs::read_to_string(&user_file).unwrap(),
            "user data\n",
            "a user file with the old temp name was removed"
        );
        assert!(
            std::fs::read_to_string(dir.path().join("AGENTS.md"))
                .unwrap()
                .contains("## hostler")
        );
        // And no temp of ours is left behind.
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".hostler-") && !name.ends_with(".hostler-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn create_new_never_follows_a_symlink() {
        // R2-3: a pre-planted symlink at a temp-like name is never followed, and an existing
        // file is never removed.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("outside.md");
        std::fs::write(&target, "outside\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let planted = dir
            .path()
            .join(format!(".AGENTS.md.hostler-{}-planted", std::process::id()));
        std::os::unix::fs::symlink(&target, &planted).unwrap();
        let error = create_new(&planted).expect_err("a symlink at the temp name");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside\n");
        // A regular file at the name is refused too, and left as it is.
        let taken = dir.path().join("taken");
        std::fs::write(&taken, "keep me\n").unwrap();
        let error = create_new(&taken).expect_err("an existing file");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error:?}");
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "keep me\n");
        // A free name opens and is ours.
        let free = dir.path().join("free");
        let file = create_new(&free).expect("a free name");
        drop(file);
        assert!(std::fs::symlink_metadata(&free).unwrap().is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_special_file_at_apply_time_is_refused() {
        // R2-4: a plan made for a regular file cannot replace a FIFO or a directory.
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let config = config();
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            false,
        )
        .unwrap();
        assert_eq!(planned[0].change.action, DocsAction::Insert);

        // A directory in the file's place.
        std::fs::remove_file(dir.path().join("AGENTS.md")).unwrap();
        std::fs::create_dir(dir.path().join("AGENTS.md")).unwrap();
        let found = apply(dir.path(), &planned).expect_err("a directory target at apply time");
        assert!(found.message.contains("not a regular file"), "{found:?}");
        assert!(dir.path().join("AGENTS.md").is_dir());
        std::fs::remove_dir(dir.path().join("AGENTS.md")).unwrap();

        // A FIFO in the file's place.
        let fifo = dir.path().join("AGENTS.md");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.as_ref().is_ok_and(|status| status.success()) {
            eprintln!("note: mkfifo is unavailable ({made:?}); the FIFO case is skipped");
            return;
        }
        let found = apply(dir.path(), &planned).expect_err("a FIFO target at apply time");
        assert!(found.message.contains("not a regular file"), "{found:?}");
        {
            use std::os::unix::fs::FileTypeExt;

            assert!(
                std::fs::symlink_metadata(&fifo)
                    .unwrap()
                    .file_type()
                    .is_fifo(),
                "the FIFO was replaced"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_target_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        // R2-5: plan time and apply time, with the file and its mode unchanged.
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let path = dir.path().join("AGENTS.md");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let config = config();
        let found = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            false,
        )
        .expect_err("a read-only docs file");
        assert!(found.message.contains("AGENTS.md"), "{found:?}");
        assert!(found.message.contains("read-only"), "{found:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# muse\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444
        );

        // The same check at apply time, after the plan was made on a writable file.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let planned = plan(
            dir.path(),
            &detected(&[("AGENTS.md", AgentDocKind::File)]),
            &config,
            false,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let found = apply(dir.path(), &planned).expect_err("a read-only target at apply time");
        assert!(found.message.contains("read-only"), "{found:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# muse\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_files_permissions_are_kept() {
        use std::os::unix::fs::PermissionsExt;

        // R2-5: the block edit does not change metadata outside the block.
        let dir = tree(&[("AGENTS.md", "# muse\n")]);
        let path = dir.path().join("AGENTS.md");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            bring_current(dir.path(), "AGENTS.md", &config()).unwrap(),
            ["AGENTS.md"]
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_ancestor_symlink_writes_to_the_canonical_directory() {
        // R2-1: the root is reached through a symlinked *ancestor* (macOS's /var is one); it is
        // accepted, and every later path is the canonical one, so the file lands in the real
        // directory the caller named.
        let real = tempfile::tempdir().unwrap();
        std::fs::create_dir(real.path().join("checkout")).unwrap();
        let holder = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(real.path(), holder.path().join("link")).unwrap();
        let root = holder.path().join("link/checkout");
        assert_eq!(
            bring_current(&root, "AGENTS.md", &config()).unwrap(),
            ["AGENTS.md"]
        );
        let written = std::fs::canonicalize(real.path().join("checkout/AGENTS.md")).unwrap();
        assert!(written.starts_with(std::fs::canonicalize(real.path()).unwrap()));
        assert!(
            std::fs::read_to_string(real.path().join("checkout/AGENTS.md"))
                .unwrap()
                .contains("## hostler")
        );
        assert!(
            !real.path().join("AGENTS.md").exists(),
            "a sibling was written"
        );
    }

    #[test]
    fn is_current_reads_the_rendered_block() {
        let config = config();
        let text = block_text(&config);
        assert!(is_current(&text, &config).unwrap());
        assert!(!is_current("# muse\n", &config).unwrap());
        assert!(!is_current("", &config).unwrap());
        // The same block in a CRLF file is current there.
        assert!(is_current(&block(&config, "\r\n"), &config).unwrap());
    }

    #[test]
    fn a_rendered_config_round_trips_through_the_block() {
        let rendered: ProjectConfig = parse(&render(&config())).unwrap();
        assert_eq!(block_text(&rendered), block_text(&config()));
    }
}
