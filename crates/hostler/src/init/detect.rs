//! Toolchain, app-id, env-file-name and agent-docs detection over a bounded walk (plan P4
//! Task 3). Pure: it reads files and never writes; it never runs a project's build tools; env
//! files contribute names only, and only when the whole file parses.
//!
//! Read safety (fix rounds 1 and 2):
//! - a symlinked root is not walked at all: `detect` answers nothing detected;
//! - every candidate file is opened **without following a symlink and without blocking**
//!   (`O_NOFOLLOW | O_NONBLOCK` on Unix, `FILE_FLAG_OPEN_REPARSE_POINT` on Windows) and the
//!   opened handle is checked, so a symlink, a FIFO, a socket or a device node is never read and
//!   a replacement between a path check and the open cannot be followed;
//! - enumeration is bounded: per directory at most the walk's remaining budget of names is held
//!   in a bounded heap, and no directory is enumerated past `RAW_ENTRIES`.
//!
//! **Residual, accepted (P4-L16, R2-1):** a directory entry is classified with `symlink_metadata`
//! and then recursed by path, so a directory swapped for a symlink between the check and
//! `read_dir` can send the walk outside the root. There is no `openat` walk. The exposure is
//! bounded — detection only proposes, env files give names only, and every file open is
//! no-follow — so it stays as a documented trade-off.
//!
//! The walk's cap is silent, exactly as the plan states it ("at most 5 000 entries"): `Detected`
//! carries no truncation channel and `contract/init.rs` is not Task 3's file, so a cut walk
//! reports what it saw.

use std::collections::BinaryHeap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::Path;

use crate::contract::devices::Platform;
use crate::contract::init::{
    AgentDoc, AgentDocKind, Detected, DetectedApp, EnvFile, Toolchain, ToolchainKind,
};
use crate::secrets::dotenv;

/// How far below the root the walk looks: an entry's depth is its number of path components, so
/// `a/b/c/file` is depth 4 and is never seen.
const MAX_DEPTH: usize = 3;
/// The walk visits at most this many directory entries, whatever the tree holds.
const MAX_ENTRIES: usize = 5_000;
/// No single directory is enumerated past this many entries: 20 times the walk's bound, so the
/// enumeration of one huge directory stays bounded while its lexicographically first names win.
const RAW_ENTRIES: usize = 20 * MAX_ENTRIES;
/// No file is read past this.
const MAX_READ: u64 = 2 * 1024 * 1024;
/// An env file past this gives no names at all.
const MAX_ENV: u64 = 256 * 1024;

/// Directories the walk never enters, at any depth. Dot-directories are skipped too, except
/// `.github`.
const SKIPPED_DIRS: [&str; 9] = [
    ".git",
    "node_modules",
    "build",
    ".gradle",
    "Pods",
    "DerivedData",
    "target",
    ".idea",
    ".venv",
];

/// The agent-docs files init looks at, at the root.
const AGENT_DOCS: [&str; 3] = ["AGENTS.md", "CLAUDE.md", ".github/copilot-instructions.md"];

/// The import line Claude Code understands, as a whole line.
const AGENTS_IMPORT: &str = "@AGENTS.md";

/// The lock files `Node`'s evidence names, in preference order.
const LOCK_FILES: [&str; 4] = [
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
];

/// What the checkout at `root` holds. A symlinked, missing or non-directory root holds nothing:
/// the walk never follows it.
pub fn detect(root: &Path) -> Detected {
    // `symlink_metadata` describes the link itself, so a symlinked root is not a directory here.
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return Detected::default();
    };
    if !metadata.is_dir() {
        return Detected::default();
    }
    let tree = Tree::walk(root);
    Detected {
        toolchains: toolchains(root, &tree),
        apps: apps(root, &tree),
        env_files: env_files(root, &tree),
        agent_docs: agent_docs(root),
    }
}

/// The regular files and the directories the bounded walk saw, relative to the root,
/// `/`-separated.
#[derive(Default)]
struct Tree {
    files: Vec<String>,
    dirs: Vec<String>,
}

impl Tree {
    fn walk(root: &Path) -> Tree {
        let mut tree = Tree::default();
        let mut budget = MAX_ENTRIES;
        tree.visit(root, "", 0, &mut budget);
        tree
    }

    /// Visits the first names of `dir`'s entries, which are at `depth + 1`. Only a regular file
    /// enters `files`; a symlink, a FIFO, a socket and a device node are neither entered nor
    /// read, so a symlink loop or a blocking special file cannot hold the walk.
    fn visit(&mut self, dir: &Path, prefix: &str, depth: usize, budget: &mut usize) {
        if depth >= MAX_DEPTH || *budget == 0 {
            return;
        }
        let (names, _cut) = first_names(dir, *budget, RAW_ENTRIES);
        for name in names {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let Some(name) = name.to_str().map(str::to_owned) else {
                continue;
            };
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(metadata) = fs::symlink_metadata(dir.join(&name)) else {
                continue;
            };
            let kind = metadata.file_type();
            if kind.is_dir() {
                if SKIPPED_DIRS.contains(&name.as_str())
                    || (name.starts_with('.') && name != ".github")
                {
                    continue;
                }
                self.dirs.push(rel.clone());
                let child_depth = depth + 1;
                if child_depth < MAX_DEPTH {
                    self.visit(&dir.join(&name), &rel, child_depth, budget);
                }
            } else if kind.is_file() {
                self.files.push(rel);
            }
        }
    }

    fn has_file(&self, rel: &str) -> bool {
        self.files.iter().any(|file| file == rel)
    }

    /// Whether a file of this name sits at the root (depth 1).
    fn has_root_file(&self, name: &str) -> bool {
        self.files.iter().any(|file| file == name)
    }

    fn files_named(&self, want: impl Fn(&str) -> bool) -> Vec<&str> {
        let mut found: Vec<&str> = self
            .files
            .iter()
            .filter(|file| want(file))
            .map(String::as_str)
            .collect();
        found.sort_by_key(|rel| (depth(rel), *rel));
        found
    }
}

/// The lexicographically first `keep` names among the first `raw_cap` entries of a directory, in
/// the filesystem's enumeration order; beyond that, the selection depends on that order. The flag
/// says the raw cap was reached. Holds at most `keep` names and reads at most `raw_cap` entries,
/// so memory and enumeration stay bounded whatever the directory holds (R1-3); for the same tree
/// on the same filesystem the answer is the same (P4-L16 R2-5 narrows the promise to this).
fn first_names(dir: &Path, keep: usize, raw_cap: usize) -> (Vec<OsString>, bool) {
    if keep == 0 {
        return (Vec::new(), false);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return (Vec::new(), false);
    };
    let mut kept: BinaryHeap<OsString> = BinaryHeap::new();
    let mut cut = false;
    for (index, entry) in entries.enumerate() {
        if index == raw_cap {
            cut = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        if kept.len() < keep {
            kept.push(name);
        } else if kept.peek().is_some_and(|largest| name < *largest) {
            kept.pop();
            kept.push(name);
        }
    }
    let mut names = kept.into_vec();
    names.sort_unstable();
    (names, cut)
}

/// An entry's depth: its number of path components.
fn depth(rel: &str) -> usize {
    rel.matches('/').count() + 1
}

fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// A handle to a regular file: opened **without following a symlink** and **without blocking**,
/// then checked on the opened handle (fix round 2, R2-1). A symlink, a FIFO, a socket, a device
/// node and a directory report `None` instead of reading outside the checkout or blocking on a
/// writer, and a replacement between a path check and the open cannot be followed because there
/// is no path check: the type is read from the handle.
fn open_regular(path: &Path) -> Option<fs::File> {
    let file = open_no_follow(path)?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        // A file symlink opened as itself is a reparse point, not the file it names.
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
    }
    Some(file)
}

/// Opens read-only without following a symlink and without blocking: `O_NONBLOCK` makes a FIFO's
/// open return at once, and reading a regular file is unaffected by it.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()
}

/// Opens read-only with the reparse point itself as the object, never its target.
#[cfg(windows)]
fn open_no_follow(path: &Path) -> Option<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()
}

/// A platform with neither Unix nor Windows file flags: the open follows what the path names, and
/// the handle check below still rejects everything that is not a regular file.
#[cfg(not(any(unix, windows)))]
fn open_no_follow(path: &Path) -> Option<fs::File> {
    fs::File::open(path).ok()
}

/// At most `limit` bytes of a regular file, through [`open_regular`]; `None` otherwise.
fn read_bytes(root: &Path, rel: &str, limit: u64) -> Option<Vec<u8>> {
    let file = open_regular(&root.join(rel))?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// A build file's text, at most `limit` bytes and only whole lines: when the cap cuts the file,
/// the partial final line is dropped before a parser sees it, so a literal at the boundary cannot
/// be completed by bytes that were never read (fix round 3, R3-2). A non-UTF-8 tail is decoded
/// lossily; no env file is read this way.
fn read_text(root: &Path, rel: &str, limit: u64) -> Option<String> {
    let mut bytes = read_bytes(root, rel, limit + 1)?;
    if bytes.len() as u64 > limit {
        let whole = bytes[..limit as usize]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        bytes.truncate(whole);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// An env file's text, at most `limit` bytes; `None` for an oversized file, a non-regular file, a
/// symlink or malformed UTF-8. Invalid UTF-8 is a problem, not a name source.
fn read_env(root: &Path, rel: &str, limit: u64) -> Option<String> {
    let file = open_regular(&root.join(rel))?;
    if file.metadata().ok()?.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}

fn is_build_gradle(rel: &str) -> bool {
    matches!(file_name(rel), "build.gradle" | "build.gradle.kts")
}

fn toolchains(root: &Path, tree: &Tree) -> Vec<Toolchain> {
    let mut found = Vec::new();
    let mut push = |kind: ToolchainKind, evidence: String| {
        if !found
            .iter()
            .any(|toolchain: &Toolchain| toolchain.kind == kind)
        {
            found.push(Toolchain { kind, evidence });
        }
    };

    if let Some(evidence) = ["settings.gradle.kts", "settings.gradle", "gradlew"]
        .into_iter()
        .find(|name| tree.has_root_file(name))
    {
        push(ToolchainKind::Gradle, evidence.to_owned());
    }
    if let Some(evidence) = kmp_evidence(root, tree) {
        push(ToolchainKind::KotlinMultiplatform, evidence);
    }
    let android = android_build_files(root, tree);
    if let Some((evidence, _)) = android.first() {
        push(ToolchainKind::AndroidApp, (*evidence).to_owned());
    }
    if let Some(evidence) = react_native_evidence(root, tree) {
        push(ToolchainKind::ReactNative, evidence);
    }
    let xcode = xcode_projects(tree);
    if let Some(evidence) = xcode.first() {
        push(ToolchainKind::Xcode, (*evidence).to_owned());
    }
    let podfiles = podfiles(tree);
    if let Some(evidence) = podfiles.first() {
        push(ToolchainKind::CocoaPods, (*evidence).to_owned());
    }
    if tree.has_root_file("Cargo.toml") {
        push(ToolchainKind::Cargo, "Cargo.toml".to_owned());
    }
    if tree.has_root_file("package.json") && !root_package_is_react_native(root, tree) {
        let evidence = LOCK_FILES
            .into_iter()
            .find(|name| tree.has_root_file(name))
            .unwrap_or("package.json");
        push(ToolchainKind::Node, evidence.to_owned());
    }

    found.sort_by_key(|toolchain| toolchain.kind);
    found
}

/// The evidence for the Kotlin Multiplatform plugin: a root or module `build.gradle(.kts)`, or
/// the version catalog's `kotlin-multiplatform`.
fn kmp_evidence(root: &Path, tree: &Tree) -> Option<String> {
    for rel in tree.files_named(is_build_gradle) {
        let Some(text) = read_text(root, rel, MAX_READ) else {
            continue;
        };
        if declares_kotlin_multiplatform(&text) {
            return Some(rel.to_owned());
        }
    }
    let catalog = "gradle/libs.versions.toml";
    if tree.has_file(catalog)
        && read_text(root, catalog, MAX_READ)
            .is_some_and(|text| text.contains("kotlin-multiplatform"))
    {
        return Some(catalog.to_owned());
    }
    None
}

/// Whether a Gradle script declares the Kotlin Multiplatform plugin in code (fix round 3,
/// R3-3): the `libs.plugins.kotlin.multiplatform` alias, `kotlin("multiplatform")`, or an
/// `id`/`apply plugin:` declaration naming the plugin id. A comment or an unrelated string that
/// merely mentions the plugin declares nothing, and the plugin id inside the declaration's own
/// string is only read once the declaration shape is known.
fn declares_kotlin_multiplatform(text: &str) -> bool {
    let mask = code_mask(text);
    if mask.contains("libs.plugins.kotlin.multiplatform") {
        return true;
    }
    let bytes = mask.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'"' || bytes[at] == b'\'' {
            let end = string_end(bytes, at);
            if let Some((id, _)) = string_literal(text, at) {
                let id = id.trim();
                let declared = match plugin_word(&mask, at) {
                    Some("kotlin") => id == "multiplatform",
                    Some("id" | "plugin") => {
                        id == "org.jetbrains.kotlin.multiplatform" || id == "kotlin.multiplatform"
                    }
                    _ => false,
                };
                if declared {
                    return true;
                }
            }
            at = end;
        } else {
            at += 1;
        }
    }
    false
}

/// The identifier that introduces the string at `at` — `id(`, `kotlin(`, `apply plugin:`,
/// `id 'x'` — skipping blanks, an opening parenthesis, a colon, an equals sign and a comma.
/// Comments are blanks in the mask, so a comment between them changes nothing.
fn plugin_word(mask: &str, at: usize) -> Option<&str> {
    let bytes = mask.as_bytes();
    let mut index = at;
    while index > 0 {
        match bytes[index - 1] {
            b' ' | b'\t' | b'\r' | b'\n' | b'(' | b'[' | b':' | b'=' | b',' => index -= 1,
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => break,
            _ => return None,
        }
    }
    let end = index;
    while index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_') {
        index -= 1;
    }
    (index < end).then(|| &mask[index..end])
}

/// The `build.gradle(.kts)` files that declare `applicationId` in code, shallowest first, with
/// the ids each one declares (a file whose ids are all expressions still detects the toolchain).
fn android_build_files<'a>(root: &Path, tree: &'a Tree) -> Vec<(&'a str, Vec<String>)> {
    let mut found = Vec::new();
    for rel in tree.files_named(is_build_gradle) {
        let Some(text) = read_text(root, rel, MAX_READ) else {
            continue;
        };
        let android = android_ids(&text);
        if android.declared {
            found.push((rel, android.ids));
        }
    }
    found
}

/// What a Groovy or Kotlin Gradle script declares.
struct AndroidIds {
    /// The `applicationId` key appears in code, not in a comment or a string.
    declared: bool,
    /// The values that are a lone string literal.
    ids: Vec<String>,
}

/// The text with every comment and every string literal's contents replaced by spaces, keeping
/// the byte offsets, the quotes and the line breaks: what remains is code only. The Gradle
/// matchers read keys and plugin declarations from this copy, so a mention inside a comment or a
/// string is not code (fix round 3, R3-1, R3-3). Line breaks inside comments and strings are kept
/// so a statement boundary stays where the source has one.
fn code_mask(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut mask = bytes.to_vec();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'/' && bytes.get(at + 1) == Some(&b'/') {
            let end = line_end(bytes, at);
            blank(&mut mask, at, end);
            at = end;
        } else if byte == b'/' && bytes.get(at + 1) == Some(&b'*') {
            let end = block_comment_end(bytes, at).unwrap_or(bytes.len());
            blank(&mut mask, at, end);
            at = end;
        } else if byte == b'"' || byte == b'\'' {
            let end = string_end(bytes, at);
            if end > at + 1 && bytes[end - 1] == byte {
                // A closed literal keeps its closing quote, so the mask's string is one token.
                blank(&mut mask, at + 1, end - 1);
            } else {
                // Unterminated: `string_end` stopped at a newline or the end of the file, so the
                // whole remainder is content and is blanked through `end`. Keeping the byte
                // before it could cut a multi-byte character in half and break the mask.
                blank(&mut mask, at + 1, end);
            }
            at = end;
        } else {
            at += 1;
        }
    }
    match String::from_utf8(mask) {
        Ok(mask) => mask,
        Err(error) => {
            // Not reachable: every blanked span ends on a character boundary. Should a later
            // change get that wrong, keep the byte length — the offsets must still line up — and
            // stay valid by blanking what is left of every non-ASCII byte.
            debug_assert!(false, "a mask span cut a character in half");
            let mut bytes = error.into_bytes();
            for byte in &mut bytes {
                if !byte.is_ascii() {
                    *byte = b' ';
                }
            }
            String::from_utf8(bytes).expect("ASCII is valid UTF-8")
        }
    }
}

/// Replaces `mask[from..to]` with spaces, keeping line breaks. Both ends sit on character
/// boundaries, which is what keeps the mask valid UTF-8.
fn blank(mask: &mut [u8], from: usize, to: usize) {
    let end = to.min(mask.len());
    for byte in &mut mask[from.min(end)..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Scans a Gradle script the way a reader of code would: the `applicationId` key is looked for in
/// [`code_mask`], so a comment or a string holds no key, the key must stand as a declaration
/// rather than a member access, and the value must be a lone string literal (fix rounds 1 to 3).
fn android_ids(text: &str) -> AndroidIds {
    let mask = code_mask(text);
    let bytes = mask.as_bytes();
    let mut found = AndroidIds {
        declared: false,
        ids: Vec::new(),
    };
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'"' || byte == b'\'' {
            at = string_end(bytes, at);
        } else if ident_start(byte) {
            let end = ident_end(bytes, at);
            if &mask[at..end] == "applicationId" && declaration_context(bytes, at) {
                found.declared = true;
                match application_id_value(text, end) {
                    Some((id, next)) => {
                        found.ids.push(id);
                        at = next;
                    }
                    None => at = end,
                }
            } else {
                at = end;
            }
        } else {
            at += 1;
        }
    }
    found
}

/// Whether the identifier at `at` is the Gradle `applicationId` declaration rather than a member
/// access: only blanks — whitespace and, in [`code_mask`], comments — may separate it from the
/// start of its line, and never a `.` or another identifier (fix rounds 2 and 3, R2-4, R3-1).
fn declaration_context(bytes: &[u8], at: usize) -> bool {
    let mut index = at;
    while index > 0 {
        match bytes[index - 1] {
            b' ' | b'\t' => index -= 1,
            b'.' => return false,
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => return false,
            _ => return true,
        }
    }
    true
}

/// The lone string literal after the `applicationId` key, and the index after it. `None` when
/// the value is an expression — a concatenation, a method or property call, a suffix, a
/// continuation on the next line — because such an id is not a literal init may report
/// (fix rounds 1 and 2, R1-6, R2-2).
fn application_id_value(text: &str, from: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut at = skip_blanks(bytes, from);
    if bytes.get(at) == Some(&b'=') {
        at = skip_blanks(bytes, at + 1);
    }
    let (id, end) = string_literal(text, at)?;
    if id.is_empty() || id.contains('$') || id.contains('\\') {
        return None;
    }
    let next = lone_literal_tail(bytes, end)?;
    Some((id.to_owned(), next))
}

/// Where to resume scanning when the literal at `from` is a complete statement value: spaces,
/// line breaks, a `;` and any number of complete comments may follow, and then a token that
/// starts a new statement — `}`, an identifier, `@`, or the end of the file. `None` when code
/// continues the expression (an operator, `.`, `(`, `as`, …) or a trailing comment never closes
/// (fix round 2, R2-2, R2-3).
fn lone_literal_tail(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    loop {
        at = skip_space(bytes, at);
        match bytes.get(at) {
            None => return Some(at),
            Some(b';') => return Some(at + 1),
            Some(b'}') | Some(b'@') => return Some(at),
            Some(b'/') if bytes.get(at + 1) == Some(&b'/') => at = line_end(bytes, at),
            Some(b'/') if bytes.get(at + 1) == Some(&b'*') => at = block_comment_end(bytes, at)?,
            Some(byte) if ident_start(*byte) => {
                let word = &bytes[at..ident_end(bytes, at)];
                // `as String`, `is Foo`, `in list`: the value continues on this token.
                let continues =
                    matches!(word, b"as" | b"is" | b"in" | b"instanceof" | b"and" | b"or");
                return (!continues).then_some(at);
            }
            Some(_) => return None,
        }
    }
}

/// A single- or double-quoted literal starting at `at`: its inner text and the index after the
/// closing quote. `None` for a triple-quoted string, an unterminated one or a non-literal.
fn string_literal(text: &str, at: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let quote = *bytes.get(at)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    if bytes.get(at + 1) == Some(&quote) && bytes.get(at + 2) == Some(&quote) {
        return None;
    }
    let mut index = at + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\n' => return None,
            byte if byte == quote => return Some((&text[at + 1..index], index + 1)),
            _ => index += 1,
        }
    }
    None
}

/// The index after the string literal starting at `at` (the opening quote). A triple-quoted
/// string runs to its closing triple; a simple one honours a backslash escape and gives up at
/// its newline when unterminated.
fn string_end(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    if bytes.get(at + 1) == Some(&quote) && bytes.get(at + 2) == Some(&quote) {
        let mut index = at + 3;
        while index + 2 < bytes.len() {
            if bytes[index] == quote && bytes[index + 1] == quote && bytes[index + 2] == quote {
                return index + 3;
            }
            index += 1;
        }
        return bytes.len();
    }
    let mut index = at + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\n' => return index,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// The index after the block comment at `at`, or `None` when it never closes (fix round 2,
/// R2-2). Nested comments (Kotlin's) are counted.
fn block_comment_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0;
    let mut index = at;
    while index + 1 < bytes.len() {
        if bytes[index] == b'/' && bytes[index + 1] == b'*' {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return Some(index);
            }
        } else {
            index += 1;
        }
    }
    None
}

/// The index after the newline ending this line (or the end of the text).
fn line_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| at + offset + 1)
}

/// The index after any spaces and tabs; a newline is not skipped.
fn skip_blanks(bytes: &[u8], at: usize) -> usize {
    let mut index = at;
    while bytes
        .get(index)
        .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
    {
        index += 1;
    }
    index
}

/// The index after any blanks and line breaks; comments are not skipped.
fn skip_space(bytes: &[u8], at: usize) -> usize {
    let mut index = at;
    while bytes
        .get(index)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        index += 1;
    }
    index
}

fn ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn ident_end(bytes: &[u8], at: usize) -> usize {
    let mut index = at;
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        index += 1;
    }
    index
}

fn react_native_evidence(root: &Path, tree: &Tree) -> Option<String> {
    for rel in tree.files_named(|rel| file_name(rel) == "package.json") {
        if package_is_react_native(root, rel) {
            return Some(rel.to_owned());
        }
    }
    None
}

fn root_package_is_react_native(root: &Path, tree: &Tree) -> bool {
    tree.has_root_file("package.json") && package_is_react_native(root, "package.json")
}

/// Whether `package.json` names `react-native` in its dependencies. A file that does not parse
/// is not React Native.
fn package_is_react_native(root: &Path, rel: &str) -> bool {
    let Some(text) = read_text(root, rel, MAX_READ) else {
        return false;
    };
    let Ok(package) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    ["dependencies", "devDependencies"]
        .into_iter()
        .any(|section| {
            package
                .get(section)
                .and_then(|deps| deps.get("react-native"))
                .is_some()
        })
}

/// The `*.xcodeproj` bundles within depth 2, shallowest first.
fn xcode_projects(tree: &Tree) -> Vec<&str> {
    let mut found: Vec<&str> = tree
        .dirs
        .iter()
        .map(String::as_str)
        .filter(|rel| rel.ends_with(".xcodeproj") && depth(rel) <= 2)
        .collect();
    found.sort_by_key(|rel| (depth(rel), *rel));
    found
}

/// The `Podfile`s the walk saw, shallowest first.
fn podfiles(tree: &Tree) -> Vec<&str> {
    tree.files_named(|rel| file_name(rel) == "Podfile")
}

fn apps(root: &Path, tree: &Tree) -> Vec<DetectedApp> {
    let mut apps = Vec::new();
    for (rel, ids) in android_build_files(root, tree) {
        for id in ids {
            apps.push(DetectedApp {
                platform: Platform::Android,
                id,
                evidence: rel.to_owned(),
            });
        }
    }
    for bundle in xcode_projects(tree) {
        let pbxproj = format!("{bundle}/project.pbxproj");
        let Some(text) = read_text(root, &pbxproj, MAX_READ) else {
            continue;
        };
        for id in bundle_ids(&text) {
            apps.push(DetectedApp {
                platform: Platform::Ios,
                id,
                evidence: pbxproj.clone(),
            });
        }
    }
    apps.sort_by(|a, b| {
        platform_order(a.platform)
            .cmp(&platform_order(b.platform))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.evidence.cmp(&b.evidence))
    });
    apps.dedup_by(|a, b| a.platform == b.platform && a.id == b.id);
    apps
}

fn platform_order(platform: Platform) -> u8 {
    match platform {
        Platform::Android => 0,
        Platform::Ios => 1,
    }
}

/// The `PRODUCT_BUNDLE_IDENTIFIER = x;` values of a `project.pbxproj`. A build setting, an
/// interpolated value (`$(`) and a test target's id are not app ids.
fn bundle_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("PRODUCT_BUNDLE_IDENTIFIER") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.strip_prefix('"') {
            Some(quoted) => match quoted.find('"') {
                Some(end) => &quoted[..end],
                None => continue,
            },
            None => match rest.find(';') {
                Some(end) => rest[..end].trim_end(),
                None => continue,
            },
        };
        let value = value.trim();
        if value.is_empty() || value.contains("$(") {
            continue;
        }
        let last = value.rsplit('.').next().unwrap_or(value);
        if last.ends_with("Tests")
            || last.ends_with("UITests")
            || value.to_lowercase().contains(".tests")
        {
            continue;
        }
        ids.push(value.to_owned());
    }
    ids
}

/// `.env`, `.env.local` and `.env.*.local` at the root, plus `.env.local` one level down.
/// Names only, through `dotenv::names`; an unreadable, oversized or malformed file gives none.
fn env_files(root: &Path, tree: &Tree) -> Vec<EnvFile> {
    let mut paths: Vec<&str> = tree
        .files
        .iter()
        .map(String::as_str)
        .filter(|rel| is_env_path(rel))
        .collect();
    paths.sort_unstable();
    paths
        .into_iter()
        .map(|path| EnvFile {
            path: path.to_owned(),
            names: env_names(root, path),
        })
        .collect()
}

fn is_env_path(rel: &str) -> bool {
    let name = file_name(rel);
    match depth(rel) {
        1 => {
            name == ".env"
                || name == ".env.local"
                || (name.starts_with(".env.") && name.ends_with(".local"))
        }
        2 => name == ".env.local",
        _ => false,
    }
}

fn env_names(root: &Path, rel: &str) -> Vec<String> {
    let Some(text) = read_env(root, rel, MAX_ENV) else {
        return Vec::new();
    };
    // `names` answers nothing for a file with any problem (fix round 1, R1-1).
    dotenv::names(&text)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

fn agent_docs(root: &Path) -> Vec<AgentDoc> {
    let mut docs = Vec::new();
    for rel in AGENT_DOCS {
        let Ok(metadata) = fs::symlink_metadata(root.join(rel)) else {
            continue;
        };
        let kind = if metadata.file_type().is_symlink() {
            symlink_target(root, rel).unwrap_or(AgentDocKind::File)
        } else if metadata.is_dir() {
            continue;
        } else if read_text(root, rel, MAX_READ).is_some_and(|text| imports_agents(&text)) {
            AgentDocKind::Imports {
                target: "AGENTS.md".into(),
            }
        } else {
            AgentDocKind::File
        };
        docs.push(AgentDoc {
            path: rel.to_owned(),
            kind,
        });
    }
    docs
}

/// The other agent-docs file a symlink resolves to, when it is one inside the root. Only paths
/// are compared; the target's content is never read.
fn symlink_target(root: &Path, rel: &str) -> Option<AgentDocKind> {
    let target = fs::canonicalize(root.join(rel)).ok()?;
    AGENT_DOCS
        .into_iter()
        .filter(|other| *other != rel)
        .find(|other| fs::canonicalize(root.join(other)).is_ok_and(|path| path == target))
        .map(|other| AgentDocKind::SymlinkTo {
            target: other.to_owned(),
        })
}

/// A line exactly `@AGENTS.md`, as Claude Code's import is (fix round 1, R1-7).
fn imports_agents(text: &str) -> bool {
    text.lines().any(|line| line == AGENTS_IMPORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_names_keeps_the_smallest_and_stops_at_the_raw_cap() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..50 {
            fs::write(dir.path().join(format!("f{index:02}")), "").unwrap();
        }

        let (names, cut) = first_names(dir.path(), 5, 1_000);
        let names: Vec<&str> = names.iter().map(|name| name.to_str().unwrap()).collect();
        assert_eq!(names, ["f00", "f01", "f02", "f03", "f04"]);
        assert!(!cut);

        // Ten entries of fifty: five names are held, all of them from the first ten read, and
        // the cut is reported instead of the rest being enumerated.
        let (names, cut) = first_names(dir.path(), 5, 10);
        assert_eq!(names.len(), 5);
        assert!(cut, "ten entries read of fifty must report the cut");
        let names: Vec<&str> = names.iter().map(|name| name.to_str().unwrap()).collect();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "{names:?}");

        // A tree smaller than the keep bound is held whole, sorted.
        let (names, cut) = first_names(dir.path(), 100, 1_000);
        assert_eq!(names.len(), 50);
        assert!(!cut);
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_skips_a_fifo_and_a_symlink_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("package.json");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.as_ref().is_ok_and(|status| status.success()) {
            eprintln!("note: mkfifo is unavailable ({made:?}); this case is skipped");
            return;
        }
        let outside = dir.path().join("outside.json");
        fs::write(&outside, "{\"dependencies\":{\"react-native\":\"1\"}}").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        // Called directly: no walk, no pre-filter. A plain `File::open` would block here.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(open_regular(&fifo).is_none());
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("opening a FIFO must not block"),
            "a FIFO must be skipped"
        );
        assert!(
            open_regular(&link).is_none(),
            "a symlink must not be followed"
        );
        assert!(
            open_regular(dir.path()).is_none(),
            "a directory is not a file"
        );
        let file = open_regular(&outside).expect("a regular file opens");
        assert!(file.metadata().unwrap().is_file());
    }
}
