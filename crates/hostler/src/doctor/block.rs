//! Doctor's view of the hostler block (plan P4 Task 6), through Task 5's `init::docs`: its
//! placement (`plan`), its block text and marker rules, and its one writer (`bring_current`,
//! which refuses symlinked roots, ancestors and targets, non-regular and read-only targets, and
//! validates every path by its components). Doctor writes no docs file by any other way.
//!
//! Doctor never calls init's detection, which lists env files (R1-4): the placement input is the
//! small agent-docs list built here, from the docs themselves. A configured docs entry that names
//! an env file is refused before Task 5's planner can open it. A target the planner leaves alone
//! (a symlink, or not a regular file) is read only through `files::read_text`, which keeps every
//! read inside the checkout (review t6-r1 R1-3), so doctor can say what it is.

use std::path::Path;

use crate::contract::init::{AgentDoc, AgentDocKind, Detected, DocsAction};
use crate::init::docs;
use crate::project::config::{ConfigProblem, ProjectConfig};

use super::files::{self, ReadRefused};

/// What a target holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BlockState {
    /// The file does not exist (the fix creates it with the block).
    NoFile,
    /// The file has no hostler block.
    Missing,
    /// The block is current.
    Current,
    /// The block differs.
    Differs,
    /// No block of its own: it links to or imports this file, which has it.
    Covered(String),
    /// The block's markers are broken at this 1-based line.
    Broken { line: usize },
    /// Task 5's writer refuses this file (a symlinked ancestor, a read-only file, …): why.
    Refused(String),
    /// Doctor does not read it (outside the checkout, an env file, not a regular file): why.
    Unreadable(String),
    /// A symlink inside the checkout whose block is not current: never written through.
    Symlink,
}

impl BlockState {
    /// Whether `bring_current` would write it.
    pub(crate) fn writable(&self) -> bool {
        matches!(
            self,
            BlockState::NoFile | BlockState::Missing | BlockState::Differs
        )
    }
}

/// The agent docs Task 5's placement starts from (`AGENTS.md`, `CLAUDE.md`), as detection would
/// describe them, without detection: a symlink to the other one is `SymlinkTo`, a `CLAUDE.md`
/// with the line `@AGENTS.md` is `Imports`, anything else is `File`.
fn agent_docs(root: &Path) -> Vec<AgentDoc> {
    const DOCS: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
    let mut found = Vec::new();
    for rel in DOCS {
        let Ok(metadata) = std::fs::symlink_metadata(root.join(rel)) else {
            continue;
        };
        if metadata.is_dir() {
            continue;
        }
        let kind = if metadata.file_type().is_symlink() {
            let target = std::fs::canonicalize(root.join(rel)).ok();
            DOCS.into_iter()
                .filter(|other| *other != rel)
                .find(|other| {
                    target.is_some() && std::fs::canonicalize(root.join(other)).ok() == target
                })
                .map_or(AgentDocKind::File, |other| AgentDocKind::SymlinkTo {
                    target: other.to_owned(),
                })
        } else if rel == "CLAUDE.md"
            && files::read_text(root, rel)
                .is_ok_and(|text| text.lines().any(|line| line == "@AGENTS.md"))
        {
            AgentDocKind::Imports {
                target: "AGENTS.md".into(),
            }
        } else {
            AgentDocKind::File
        };
        found.push(AgentDoc {
            path: rel.to_owned(),
            kind,
        });
    }
    found
}

fn planned_state(action: &DocsAction) -> BlockState {
    match action {
        DocsAction::Create => BlockState::NoFile,
        DocsAction::Insert => BlockState::Missing,
        DocsAction::Update => BlockState::Differs,
        DocsAction::Unchanged => BlockState::Current,
        DocsAction::CoveredBy { path } => BlockState::Covered(path.clone()),
    }
}

/// A planner problem: a broken marker keeps its line; anything else is the writer's refusal.
fn marker_or_refused(problem: &ConfigProblem) -> BlockState {
    if problem.message.contains(docs::BEGIN) || problem.message.contains(docs::END) {
        BlockState::Broken { line: problem.line }
    } else {
        BlockState::Refused(problem.message.clone())
    }
}

/// A target the planner left alone (a symlink, or not a regular file): read only inside the
/// checkout, never written.
fn unplanned_state(root: &Path, rel: &str, config: &ProjectConfig) -> BlockState {
    match files::read_text(root, rel) {
        Err(ReadRefused::NotFound) => BlockState::NoFile,
        Err(why) => BlockState::Unreadable(why.to_string()),
        Ok(text) => match docs::is_current(&text, config) {
            Ok(true) => BlockState::Current,
            Ok(false) => BlockState::Symlink,
            Err(problem) => BlockState::Broken { line: problem.line },
        },
    }
}

/// One configured `[agents] docs` file, planned on its own (the block text does not depend on
/// the docs list).
fn configured_state(root: &Path, rel: &str, config: &ProjectConfig) -> BlockState {
    // Refused before Task 5's planner, which would open the file (R1-4).
    if let Err(why) = files::project_path(root, rel) {
        return BlockState::Unreadable(why.to_string());
    }
    let mut one = config.clone();
    one.agents.docs = vec![rel.to_owned()];
    match docs::plan(root, &Detected::default(), &one, true) {
        Err(problem) => marker_or_refused(&problem),
        Ok(planned) => match planned.iter().find(|entry| entry.change.path == rel) {
            Some(entry) => planned_state(&entry.change.action),
            None => unplanned_state(root, rel, config),
        },
    }
}

/// Every target's state, by Task 5's placement: `[agents] docs` in its order, else `AGENTS.md`
/// and a `CLAUDE.md` that neither links to nor imports it.
pub(crate) fn states(root: &Path, config: &ProjectConfig) -> Vec<(String, BlockState)> {
    if !config.agents.docs.is_empty() {
        return config
            .agents
            .docs
            .iter()
            .map(|rel| (rel.clone(), configured_state(root, rel, config)))
            .collect();
    }
    let detected = Detected {
        agent_docs: agent_docs(root),
        ..Detected::default()
    };
    // The files the placement owes a block: `AGENTS.md`, and each plain doc.
    let mut expected = vec!["AGENTS.md".to_owned()];
    expected.extend(
        detected
            .agent_docs
            .iter()
            .filter(|doc| doc.kind == AgentDocKind::File && doc.path != "AGENTS.md")
            .map(|doc| doc.path.clone()),
    );
    let planned = match docs::plan(root, &detected, config, false) {
        Ok(planned) => planned,
        Err(problem) => {
            let path = problem
                .message
                .split_once(": ")
                .map_or("AGENTS.md", |(path, _)| path)
                .to_owned();
            return vec![(path, marker_or_refused(&problem))];
        }
    };
    let mut out: Vec<(String, BlockState)> = planned
        .iter()
        .map(|entry| {
            (
                entry.change.path.clone(),
                planned_state(&entry.change.action),
            )
        })
        .collect();
    for rel in expected {
        if !out.iter().any(|(path, _)| *path == rel) {
            let state = unplanned_state(root, &rel, config);
            out.push((rel, state));
        }
    }
    out
}

/// The `project.docs` fix: Task 5's `bring_current` on each target that needs the block, and
/// nothing else. The written paths.
pub(crate) fn bring_current(root: &Path, config: &ProjectConfig) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    for (rel, state) in states(root, config) {
        if !state.writable() {
            continue;
        }
        written.extend(
            docs::bring_current(root, &rel, config)
                .map_err(|problem| format!("{} (line {})", problem.message, problem.line))?,
        );
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg_attr(not(unix), allow(unused_imports))] // END is used by a Unix-only test
    use crate::init::docs::{BEGIN, END};

    #[test]
    fn placement_follows_task_5() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config = ProjectConfig::default();
        assert_eq!(
            states(root, &config),
            [("AGENTS.md".to_owned(), BlockState::NoFile)]
        );
        std::fs::write(root.join("CLAUDE.md"), "intro\n@AGENTS.md\n").unwrap();
        assert!(
            states(root, &config)
                .contains(&("CLAUDE.md".into(), BlockState::Covered("AGENTS.md".into())))
        );
        std::fs::write(root.join("CLAUDE.md"), "plain\n").unwrap();
        let found = states(root, &config);
        let mut paths: Vec<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["AGENTS.md", "CLAUDE.md"]);
        assert!(found.iter().all(|(_, s)| s.writable()), "{found:?}");
        // The fix writes both through Task 5's writer, then both are current.
        assert_eq!(bring_current(root, &config).unwrap().len(), 2);
        assert!(
            states(root, &config)
                .iter()
                .all(|(_, s)| *s == BlockState::Current)
        );
        assert!(
            std::fs::read_to_string(root.join("CLAUDE.md"))
                .unwrap()
                .starts_with("plain\n")
        );
    }

    #[test]
    fn broken_markers_keep_their_line_and_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config = ProjectConfig::default();
        let broken = format!("{BEGIN}\nhalf\n");
        std::fs::write(root.join("AGENTS.md"), &broken).unwrap();
        assert_eq!(
            states(root, &config),
            [("AGENTS.md".to_owned(), BlockState::Broken { line: 1 })]
        );
        assert!(bring_current(root, &config).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            broken
        );
    }

    /// R1-3 with Task 5's writer: a symlinked target is never written through, inside the
    /// checkout or out of it; one pointing outside is not even read.
    #[cfg(unix)]
    #[test]
    fn symlinked_targets_are_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let stale = format!("{BEGIN}\nold\n{END}\n");
        std::fs::write(outside.join("AGENTS.md"), &stale).unwrap();
        std::fs::write(root.join("real.md"), &stale).unwrap();
        std::os::unix::fs::symlink(outside.join("AGENTS.md"), root.join("AGENTS.md")).unwrap();
        std::os::unix::fs::symlink("real.md", root.join("inner.md")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("docs")).unwrap();
        let mut config = ProjectConfig::default();
        config.agents.docs = vec!["AGENTS.md".into(), "inner.md".into(), "docs/new.md".into()];
        let found = states(&root, &config);
        assert_eq!(
            found[0],
            (
                "AGENTS.md".into(),
                BlockState::Unreadable("it points outside the checkout".into())
            )
        );
        assert_eq!(found[1], ("inner.md".into(), BlockState::Symlink));
        assert!(
            matches!(&found[2], (_, BlockState::Refused(why)) if why.contains("docs/new.md")),
            "{found:?}"
        );
        assert!(bring_current(&root, &config).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(outside.join("AGENTS.md")).unwrap(),
            stale
        );
        assert_eq!(
            std::fs::read_to_string(root.join("real.md")).unwrap(),
            stale
        );
        assert!(!outside.join("new.md").exists());
    }
}
