//! The proposed `hostler.toml` from what init detected (plan P4 Task 5): app ids, one secret
//! per env-file name (names only, never a value), the heavy commands of the toolchains found,
//! and the agent docs the block goes into. `config::render` turns it into the canonical file, so
//! a proposal always parses.
//!
//! The package manager is read from the checkout root's lock file (its existence only), so the
//! signature takes the root beside `Detected`, which carries no lock-file slot of its own.

use std::collections::BTreeMap;
use std::path::Path;

use crate::contract::devices::Platform;
use crate::contract::init::{AgentDocKind, Detected, ToolchainKind};
use crate::project::config::{Agents, Apps, ProjectConfig, SecretDecl};

/// A `hostler.toml` for the checkout at `root`. `name` stays unset (the default name applies)
/// and `device_rules` too: the agent fills that one in (`init.device_rules`).
pub fn propose(root: &Path, detected: &Detected) -> ProjectConfig {
    ProjectConfig {
        name: None,
        apps: apps(detected),
        secrets: secrets(detected),
        accounts: std::collections::BTreeMap::new(),
        heavy: heavy_commands(root, detected),
        agents: Agents {
            docs: docs(detected),
            device_rules: None,
        },
        ranges_present: false,
    }
}

fn kinds(detected: &Detected) -> Vec<ToolchainKind> {
    detected
        .toolchains
        .iter()
        .map(|toolchain| toolchain.kind)
        .collect()
}

fn apps(detected: &Detected) -> Apps {
    let mut apps = Apps::default();
    for app in &detected.apps {
        let ids = match app.platform {
            Platform::Android => &mut apps.android,
            Platform::Ios => &mut apps.ios,
        };
        if !ids.contains(&app.id) {
            ids.push(app.id.clone());
        }
    }
    apps
}

/// One secret per env-file name, in document order (the files, then their names). The name was
/// read by `dotenv::names`, which accepts only `[A-Z][A-Z0-9_]*`, so it is a valid secret name;
/// `env` names the variable the file defines. The purpose is a TODO that passes validation and
/// that doctor warns about.
fn secrets(detected: &Detected) -> BTreeMap<String, SecretDecl> {
    let mut secrets: BTreeMap<String, SecretDecl> = BTreeMap::new();
    for file in &detected.env_files {
        for name in &file.names {
            secrets.entry(name.clone()).or_insert_with(|| SecretDecl {
                purpose: format!("TODO: say what {name} is for"),
                env: Some(name.clone()),
            });
        }
    }
    secrets
}

/// The heavy commands of the toolchains found, in a fixed order, without duplicates.
fn heavy_commands(root: &Path, detected: &Detected) -> Vec<String> {
    let kinds = kinds(detected);
    let has = |kind: ToolchainKind| kinds.contains(&kind);
    let mut commands: Vec<String> = Vec::new();
    let mut add = |command: &str| {
        if !commands.iter().any(|have| have == command) {
            commands.push(command.to_string());
        }
    };
    if has(ToolchainKind::Gradle) {
        add("./gradlew");
    }
    if has(ToolchainKind::CocoaPods) {
        add("pod install");
    }
    if has(ToolchainKind::Xcode) {
        add("xcodebuild");
    }
    let node = has(ToolchainKind::Node);
    let react_native = has(ToolchainKind::ReactNative);
    if react_native || node {
        add(node_install(root));
    }
    // Not `npx react-native run-`: `run-android`/`run-ios` install on a device, so they are
    // device commands (`hostler run --device`), not heavy-only ones (C-6). Not `cargo test`:
    // heavy is builds and installs, never tests or linters (C-5, the start and heavy guides).
    if has(ToolchainKind::Cargo) {
        add("cargo build");
    }
    commands
}

/// The root's package-manager install, from its lock file; `npm install` when none is there and
/// `npm ci` for `package-lock.json`.
fn node_install(root: &Path) -> &'static str {
    const LOCKED: [(&str, &str); 4] = [
        ("yarn.lock", "yarn install"),
        ("pnpm-lock.yaml", "pnpm install"),
        ("bun.lockb", "bun install"),
        ("bun.lock", "bun install"),
    ];
    for (lock, install) in LOCKED {
        if is_file(root, lock) {
            return install;
        }
    }
    if is_file(root, "package-lock.json") {
        "npm ci"
    } else {
        "npm install"
    }
}

fn is_file(root: &Path, rel: &str) -> bool {
    std::fs::symlink_metadata(root.join(rel)).is_ok_and(|metadata| metadata.is_file())
}

/// The agent docs the block goes into: the ones detection found holding no link to another one.
fn docs(detected: &Detected) -> Vec<String> {
    detected
        .agent_docs
        .iter()
        .filter(|doc| matches!(doc.kind, AgentDocKind::File))
        .map(|doc| doc.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::contract::init::{AgentDoc, DetectedApp, EnvFile, Toolchain};
    use crate::project::config::{self, render};

    use super::*;

    /// C-5, C-6 (P4 clean read): with every toolchain at once, no proposed heavy command is a
    /// test, a linter or a device launch (those are quick, or `hostler run --device`); and the
    /// block says what a device command is.
    #[test]
    fn heavy_is_builds_and_installs_only() {
        let root = root_with(&["yarn.lock", "Cargo.toml"]);
        let all = [
            ToolchainKind::Gradle,
            ToolchainKind::KotlinMultiplatform,
            ToolchainKind::AndroidApp,
            ToolchainKind::ReactNative,
            ToolchainKind::Xcode,
            ToolchainKind::CocoaPods,
            ToolchainKind::Cargo,
            ToolchainKind::Node,
        ];
        let kinds: Vec<(ToolchainKind, &str)> = all.iter().map(|kind| (*kind, "x")).collect();
        let config = propose(root.path(), &detected(&kinds));
        assert!(!config.heavy.is_empty());
        for command in &config.heavy {
            for word in ["test", "lint", "clippy", "fmt", "run-"] {
                assert!(
                    !command.contains(word),
                    "{command:?} is not a heavy-only command"
                );
            }
        }
        let block = crate::init::docs::block_text(&config);
        assert!(
            block.contains(
                "(`npx react-native run-android`, `./gradlew installDebug`): `hostler \
                            run --device"
            ),
            "{block}"
        );
    }

    fn detected(kinds: &[(ToolchainKind, &str)]) -> Detected {
        Detected {
            toolchains: kinds
                .iter()
                .map(|(kind, evidence)| Toolchain {
                    kind: *kind,
                    evidence: (*evidence).into(),
                })
                .collect(),
            ..Detected::default()
        }
    }

    fn root_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in files {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        dir
    }

    #[test]
    fn kotlin_multiplatform_proposes_gradle_heavy_and_apps() {
        let root = root_with(&["settings.gradle.kts", "build.gradle.kts"]);
        let mut detected = detected(&[
            (ToolchainKind::Gradle, "settings.gradle.kts"),
            (ToolchainKind::KotlinMultiplatform, "build.gradle.kts"),
            (ToolchainKind::AndroidApp, "androidApp/build.gradle.kts"),
        ]);
        detected.apps = vec![DetectedApp {
            platform: Platform::Android,
            id: "com.example.muse".into(),
            evidence: "androidApp/build.gradle.kts".into(),
        }];
        detected.env_files = vec![EnvFile {
            path: ".env.local".into(),
            names: vec!["JACKETT_API_KEY".into(), "RT_INDEX_API_KEY".into()],
        }];
        detected.agent_docs = vec![
            AgentDoc {
                path: "AGENTS.md".into(),
                kind: AgentDocKind::File,
            },
            AgentDoc {
                path: "CLAUDE.md".into(),
                kind: AgentDocKind::SymlinkTo {
                    target: "AGENTS.md".into(),
                },
            },
        ];

        let config = propose(root.path(), &detected);
        assert_eq!(config.name, None);
        assert_eq!(config.apps.android, ["com.example.muse"]);
        assert!(config.apps.ios.is_empty());
        assert_eq!(config.heavy, ["./gradlew"]);
        assert_eq!(config.agents.docs, ["AGENTS.md"]);
        assert_eq!(config.agents.device_rules, None);
        assert_eq!(
            config.secrets["JACKETT_API_KEY"].purpose,
            "TODO: say what JACKETT_API_KEY is for"
        );
        assert_eq!(config.env_for("JACKETT_API_KEY"), Some("JACKETT_API_KEY"));
        assert_eq!(config.secrets.len(), 2);

        // The proposal is the canonical file and parses back to itself.
        let rendered = render(&config);
        assert!(rendered.contains("[apps]"), "{rendered}");
        assert_eq!(config::parse(&rendered).unwrap(), config);
    }

    #[test]
    fn heavy_commands_follow_the_toolchains() {
        let plain = root_with(&[]);
        let rn = propose(
            plain.path(),
            &detected(&[
                (ToolchainKind::ReactNative, "package.json"),
                (ToolchainKind::Xcode, "ios/App.xcodeproj"),
                (ToolchainKind::CocoaPods, "Podfile"),
            ]),
        );
        assert_eq!(rn.heavy, ["pod install", "xcodebuild", "npm install",]);

        let yarn = root_with(&["yarn.lock"]);
        assert_eq!(
            propose(
                yarn.path(),
                &detected(&[(ToolchainKind::ReactNative, "package.json")])
            )
            .heavy,
            ["yarn install"]
        );

        let pnpm = root_with(&["pnpm-lock.yaml"]);
        assert_eq!(
            propose(
                pnpm.path(),
                &detected(&[(ToolchainKind::Node, "pnpm-lock.yaml")])
            )
            .heavy,
            ["pnpm install"]
        );

        let npm = root_with(&["package-lock.json"]);
        assert_eq!(
            propose(
                npm.path(),
                &detected(&[(ToolchainKind::Node, "package-lock.json")])
            )
            .heavy,
            ["npm ci"]
        );

        let cargo = root_with(&["Cargo.toml"]);
        assert_eq!(
            propose(
                cargo.path(),
                &detected(&[(ToolchainKind::Cargo, "Cargo.toml")])
            )
            .heavy,
            ["cargo build"]
        );

        let bare_node = root_with(&["package.json"]);
        assert_eq!(
            propose(
                bare_node.path(),
                &detected(&[(ToolchainKind::Node, "package.json")])
            )
            .heavy,
            ["npm install"]
        );

        assert!(propose(plain.path(), &Detected::default()).heavy.is_empty());
    }

    #[test]
    fn an_empty_detection_proposes_an_empty_file() {
        let plain = root_with(&[]);
        let config = propose(plain.path(), &Detected::default());
        assert_eq!(config, ProjectConfig::default());
        assert_eq!(
            render(&config),
            "# This project's hostler settings (hostler.toml, schema version 1).\n"
        );
        assert_eq!(config::parse(&render(&config)).unwrap(), config);
    }

    #[test]
    fn the_same_env_name_in_two_files_is_one_secret() {
        let plain = root_with(&[]);
        let mut detected = detected(&[]);
        detected.env_files = vec![
            EnvFile {
                path: ".env".into(),
                names: vec!["API_KEY".into()],
            },
            EnvFile {
                path: ".env.local".into(),
                names: vec!["API_KEY".into()],
            },
        ];
        let config = propose(plain.path(), &detected);
        assert_eq!(config.secrets.len(), 1);
    }

    #[test]
    fn a_covered_claude_md_is_not_a_target() {
        let plain = root_with(&[]);
        let mut detected = detected(&[]);
        detected.agent_docs = vec![
            AgentDoc {
                path: "AGENTS.md".into(),
                kind: AgentDocKind::File,
            },
            AgentDoc {
                path: "CLAUDE.md".into(),
                kind: AgentDocKind::Imports {
                    target: "AGENTS.md".into(),
                },
            },
        ];
        assert_eq!(propose(plain.path(), &detected).agents.docs, ["AGENTS.md"]);
    }
}
