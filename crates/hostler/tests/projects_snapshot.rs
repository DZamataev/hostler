//! `projects.snapshot`, `projects.changed` and the declarations `workspace.resolve` stores
//! (P5 Task 1, spec §6.3), against the real binary and CLI.

mod common;
#[path = "common/raw.rs"]
mod raw;

use std::path::Path;
use std::time::Duration;

use common::{Home, stderr};
use hostler::contract::projects::{DeclaredAccount, DeclaredApps, ProjectDecl, ProjectsSnapshot};
use hostler::contract::secrets::DeclaredSecret;
use hostler::contract::wire::{Event, Request};
use raw::{Raw, blocking, files_text, runtime};

/// A value no frame, output or log line may ever hold.
const MARKER: &str = "hostler-marker-7f3a";

const TOML: &str = "\
[apps]
android = [\"com.example.app\"]
ios = [\"com.example.App\"]

[secrets.API_KEY]
purpose = \"the indexer\"
env = \"INDEXER_KEY\"

[accounts.qa-user]
purpose = \"the main e2e test user\"
fields = [\"env\", \"session\"]
";

fn declared() -> ProjectDecl {
    ProjectDecl {
        secrets: vec![DeclaredSecret {
            name: "API_KEY".into(),
            purpose: "the indexer".into(),
            env: Some("INDEXER_KEY".into()),
        }],
        accounts: vec![DeclaredAccount {
            name: "qa-user".into(),
            purpose: "the main e2e test user".into(),
            fields: vec!["env".into(), "session".into()],
        }],
        apps: DeclaredApps {
            android: vec!["com.example.app".into()],
            ios: vec!["com.example.App".into()],
        },
    }
}

/// `hostler workspace` in `repo`: the CLI's own resolve, with the checkout's declarations.
fn resolve(home: &Home, repo: &Path) -> String {
    let out = home
        .hostler()
        .arg("workspace")
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "workspace: {}", stderr(&out));
    format!("{}{}", common::stdout(&out), stderr(&out))
}

fn snapshot(home: &Home) -> ProjectsSnapshot {
    serde_json::from_value(home.call(Request::ProjectsSnapshot).unwrap()).unwrap()
}

fn projects_events(events: &[(u64, Event)]) -> Vec<(u64, ProjectsSnapshot)> {
    events
        .iter()
        .filter_map(|(seq, e)| match e {
            Event::ProjectsChanged(s) => Some((*seq, (**s).clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn resolve_stores_declared_and_publishes() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    std::fs::write(repo.join("hostler.toml"), TOML).unwrap();
    runtime().block_on(async {
        let mut raw = Raw::connect(&home).await;
        resolve(&home, &repo);
        let events = raw.events_within(Duration::from_secs(2)).await;
        let changed = projects_events(&events);
        assert_eq!(changed.len(), 1, "{events:?}");
        let (seq, event) = &changed[0];
        assert_eq!(event.as_of_seq, *seq);
        assert_eq!(event.projects.len(), 1);
        assert_eq!(event.projects[0].declared.as_ref(), Some(&declared()));
        assert_eq!(event.projects[0].workspaces.len(), 1);
        let snap = blocking(|| snapshot(&home));
        assert!(snap.as_of_seq >= *seq);
        assert_eq!(snap.projects, event.projects);
        assert!(snap.projects[0].declared_at_ms.is_some());
        // The same declaration again: nothing changed, nothing published.
        resolve(&home, &repo);
        let events = raw.events_within(Duration::from_secs(2)).await;
        assert_eq!(projects_events(&events), []);
    });
}

#[test]
fn none_keeps_the_stored_declaration() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    std::fs::write(repo.join("hostler.toml"), TOML).unwrap();
    resolve(&home, &repo);
    let stored = snapshot(&home).projects[0].clone();
    std::fs::remove_file(repo.join("hostler.toml")).unwrap();
    runtime().block_on(async {
        let mut raw = Raw::connect(&home).await;
        resolve(&home, &repo);
        let events = raw.events_within(Duration::from_secs(2)).await;
        assert_eq!(projects_events(&events), []);
    });
    let now = snapshot(&home).projects[0].clone();
    assert_eq!(now.declared, Some(declared()));
    assert_eq!(now.declared_at_ms, stored.declared_at_ms);
    // A changed declaration replaces it.
    std::fs::write(
        repo.join("hostler.toml"),
        "[secrets.OTHER_KEY]\npurpose = \"another\"\n",
    )
    .unwrap();
    resolve(&home, &repo);
    let now = snapshot(&home).projects[0].clone();
    let decl = now.declared.unwrap();
    assert_eq!(decl.secrets.len(), 1);
    assert_eq!(decl.secrets[0].name, "OTHER_KEY");
    assert!(decl.accounts.is_empty());
}

#[test]
fn approve_publishes() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    resolve(&home, &repo);
    assert!(!snapshot(&home).projects[0].info.approved);
    runtime().block_on(async {
        let mut raw = Raw::connect(&home).await;
        blocking(|| home.approve(&repo));
        let events = raw.events_within(Duration::from_secs(2)).await;
        let changed = projects_events(&events);
        assert_eq!(changed.len(), 1, "{events:?}");
        assert!(changed[0].1.projects[0].info.approved);
    });
}

/// A checkout with `.env.local` holding the marker and a `hostler.toml` declaring its name: no
/// frame, CLI output or service-log line holds the marker.
#[test]
fn declaration_has_no_values() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    std::fs::write(repo.join(".env.local"), format!("API_KEY={MARKER}\n")).unwrap();
    std::fs::write(repo.join("hostler.toml"), TOML).unwrap();
    let lines = runtime().block_on(async {
        let mut raw = Raw::connect(&home).await;
        let output = resolve(&home, &repo);
        assert!(!output.contains(MARKER), "{output}");
        let mut lines = raw.lines_within(Duration::from_secs(2)).await;
        let answer = raw.call(2, "projects.snapshot", None).await;
        lines.push(answer.to_string());
        lines
    });
    assert!(lines.iter().any(|l| l.contains("projects.changed")));
    for line in &lines {
        assert!(!line.contains(MARKER), "{line}");
    }
    let stop = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(stop.status.success(), "{}", stderr(&stop));
    assert!(!files_text(&home.paths().log_dir).contains(MARKER));
}

/// A client-sent declaration with a bad name is refused, never echoed, and stores nothing.
#[test]
fn bad_declaration_is_refused_unechoed() {
    let home = Home::new();
    home.start_service();
    let repo = home.make_repo("app");
    let current = hostler::project::current(&repo).unwrap();
    let mut bad = declared();
    bad.secrets[0].name = MARKER.into();
    let error = home
        .call(Request::WorkspaceResolve(
            hostler::contract::claims::WorkspaceResolve {
                project: current.project,
                root: hostler::project::display_path(&current.checkout.workspace_root),
                declared: Some(bad),
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, hostler::contract::ErrorCode::BadRequest);
    assert!(!error.message.contains(MARKER), "{}", error.message);
    assert!(
        snapshot(&home)
            .projects
            .iter()
            .all(|p| p.declared.is_none())
    );
}
