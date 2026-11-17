//! Toolchain detection and env-file names (plan P4 Task 3). The trees under
//! `tests/fixtures/init/` are synthetic; every env-file value is a marker.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hostler::contract::devices::Platform;
use hostler::contract::init::{
    AgentDoc, AgentDocKind, Detected, DetectedApp, EnvFile, Toolchain, ToolchainKind,
};
use hostler::init::detect::detect;
use hostler::secrets::dotenv;

/// Every fixture env-file value starts with this.
const MARKER: &str = "hunter2";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/init")
        .join(name)
}

fn toolchain(kind: ToolchainKind, evidence: &str) -> Toolchain {
    Toolchain {
        kind,
        evidence: evidence.to_owned(),
    }
}

fn app(platform: Platform, id: &str, evidence: &str) -> DetectedApp {
    DetectedApp {
        platform,
        id: id.to_owned(),
        evidence: evidence.to_owned(),
    }
}

fn doc(path: &str, kind: AgentDocKind) -> AgentDoc {
    AgentDoc {
        path: path.to_owned(),
        kind,
    }
}

fn env_file(path: &str, names: &[&str]) -> EnvFile {
    EnvFile {
        path: path.to_owned(),
        names: names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

#[cfg(unix)]
fn link_file(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn link_file(target: &str, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn link_dir(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn link_dir(target: &str, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A temporary tree of the given files, paths relative to its root.
fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }
    dir
}

#[test]
fn detects_kmp() {
    let detected = detect(&fixture("kmp"));
    assert_eq!(
        detected.toolchains,
        vec![
            toolchain(ToolchainKind::Gradle, "settings.gradle.kts"),
            toolchain(ToolchainKind::KotlinMultiplatform, "build.gradle.kts"),
            toolchain(ToolchainKind::AndroidApp, "androidApp/build.gradle.kts"),
        ]
    );
    assert_eq!(
        detected.apps,
        vec![app(
            Platform::Android,
            "com.example.muse",
            "androidApp/build.gradle.kts"
        )]
    );
    assert_eq!(
        detected.env_files,
        vec![env_file(
            ".env.local",
            &["JACKETT_API_KEY", "RT_INDEX_API_KEY"]
        )]
    );
    assert_eq!(
        detected.agent_docs,
        vec![doc("AGENTS.md", AgentDocKind::File)]
    );
}

#[test]
fn detects_rn() {
    let detected = detect(&fixture("rn"));
    assert_eq!(
        detected.toolchains,
        vec![
            toolchain(ToolchainKind::AndroidApp, "android/app/build.gradle"),
            toolchain(ToolchainKind::ReactNative, "package.json"),
            toolchain(ToolchainKind::Xcode, "ios/App.xcodeproj"),
            toolchain(ToolchainKind::CocoaPods, "Podfile"),
        ]
    );
    // `applicationIdSuffix` is not combined, the `$(…)` id and the test targets are skipped.
    assert_eq!(
        detected.apps,
        vec![
            app(
                Platform::Android,
                "com.looky.app",
                "android/app/build.gradle"
            ),
            app(
                Platform::Android,
                "com.pro.pulse",
                "android/app/build.gradle"
            ),
            app(
                Platform::Ios,
                "com.looky.app",
                "ios/App.xcodeproj/project.pbxproj"
            ),
        ]
    );
    assert_eq!(detected.env_files, Vec::new());
    assert_eq!(
        detected.agent_docs,
        vec![
            doc("AGENTS.md", AgentDocKind::File),
            doc(
                "CLAUDE.md",
                AgentDocKind::Imports {
                    target: "AGENTS.md".into()
                }
            ),
        ]
    );
}

#[test]
fn android_ids_skip_expressions() {
    let dir = tree(&[(
        "app/build.gradle",
        "android {\n    defaultConfig {\n        applicationId \"com.example.${flavor}\"\n        \
         applicationId someVariable\n    }\n}\n",
    )]);
    let detected = detect(dir.path());
    // The toolchain is the `applicationId` key; an expression is not a reported id.
    assert_eq!(
        detected.toolchains,
        vec![toolchain(ToolchainKind::AndroidApp, "app/build.gradle")]
    );
    assert!(detected.apps.is_empty(), "{:?}", detected.apps);
}

#[test]
fn android_comments_are_not_config() {
    let dir = tree(&[(
        "app/build.gradle",
        "android {\n    defaultConfig {\n        /*\n        applicationId 'com.example.block'\n        \
         */\n        // applicationId 'com.example.line'\n        \
         /* applicationId 'com.example.inline' */\n    }\n}\n",
    )]);
    // A commented-out configuration declares no Android app and yields no id.
    let detected = detect(dir.path());
    assert!(detected.toolchains.is_empty(), "{:?}", detected.toolchains);
    assert!(detected.apps.is_empty(), "{:?}", detected.apps);
}

#[test]
fn android_strings_are_not_keys() {
    let dir = tree(&[(
        "app/build.gradle",
        "android {\n    defaultConfig {\n        def label = \"applicationId = 'com.example.string'\"\n    }\n}\n",
    )]);
    let detected = detect(dir.path());
    assert!(detected.toolchains.is_empty(), "{:?}", detected.toolchains);
    assert!(detected.apps.is_empty(), "{:?}", detected.apps);
}

#[test]
fn android_ids_survive_comments() {
    let dir = tree(&[
        (
            "app/build.gradle",
            "android {\n    defaultConfig {\n        /* note */ applicationId 'com.example.groovy' // shipped\n    }\n}\n",
        ),
        (
            "kts/build.gradle.kts",
            "android {\n    defaultConfig {\n        /*\n         * a note\n         */\n        applicationId = \"com.example.kts\" /* shipped */\n    }\n}\n",
        ),
    ]);
    assert_eq!(
        detect(dir.path()).apps,
        vec![
            app(Platform::Android, "com.example.groovy", "app/build.gradle"),
            app(Platform::Android, "com.example.kts", "kts/build.gradle.kts"),
        ]
    );
}

#[test]
fn android_expressions_report_no_id() {
    let dir = tree(&[
        (
            "concat/build.gradle",
            "android {\n    defaultConfig {\n        applicationId 'com.example.' + suffix\n    }\n}\n",
        ),
        (
            "method/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.\" + providers.gradleProperty(\"suffix\").get()\n    }\n}\n",
        ),
        (
            "suffix/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.suffix\" + buildSuffix\n    }\n}\n",
        ),
        (
            "property/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = providers.gradleProperty(\"id\").get()\n    }\n}\n",
        ),
        (
            "lone/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.lone\"\n    }\n}\n",
        ),
    ]);
    let detected = detect(dir.path());
    assert_eq!(
        detected.apps,
        vec![app(
            Platform::Android,
            "com.example.lone",
            "lone/build.gradle.kts"
        )]
    );
    assert!(
        detected
            .toolchains
            .iter()
            .any(|found| found.kind == ToolchainKind::AndroidApp)
    );
}

#[test]
fn android_continuations_on_the_next_line_report_no_id() {
    let dir = tree(&[
        (
            "groovy/build.gradle",
            "android {\n    defaultConfig {\n        applicationId 'com.example.groovy'\n            + suffix\n    }\n}\n",
        ),
        (
            "kts/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.kts\"\n            .trim()\n    }\n}\n",
        ),
        (
            "cast/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.cast\"\n            as String\n    }\n}\n",
        ),
        (
            "lone/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.lone\"\n    }\n}\n",
        ),
    ]);
    // A value continued on the next line is an expression; only the complete literal is an id.
    assert_eq!(
        detect(dir.path()).apps,
        vec![app(
            Platform::Android,
            "com.example.lone",
            "lone/build.gradle.kts"
        )]
    );
}

#[test]
fn android_an_unterminated_trailing_comment_reports_no_id() {
    let dir = tree(&[
        (
            "open/build.gradle",
            "android {\n    defaultConfig {\n        applicationId 'com.example.open' /* not closed\n    }\n}\n",
        ),
        (
            "lone/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.lone\"\n    }\n}\n",
        ),
    ]);
    assert_eq!(
        detect(dir.path()).apps,
        vec![app(
            Platform::Android,
            "com.example.lone",
            "lone/build.gradle.kts"
        )]
    );
}

#[test]
fn android_valid_terminators_keep_the_id() {
    let dir = tree(&[
        (
            "semi/build.gradle",
            "android {\n    defaultConfig {\n        applicationId 'com.example.semi';\n    }\n}\n",
        ),
        (
            "comments/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.comments\" /* a */ /* b */ // c\n    }\n}\n",
        ),
        (
            "inline/build.gradle.kts",
            "android {\n    defaultConfig { applicationId = \"com.example.inline\" }\n}\n",
        ),
    ]);
    assert_eq!(
        detect(dir.path()).apps,
        vec![
            app(
                Platform::Android,
                "com.example.comments",
                "comments/build.gradle.kts"
            ),
            app(
                Platform::Android,
                "com.example.inline",
                "inline/build.gradle.kts"
            ),
            app(Platform::Android, "com.example.semi", "semi/build.gradle"),
        ]
    );
}

#[test]
fn android_member_access_is_not_the_key() {
    let dir = tree(&[
        (
            "qualified/build.gradle.kts",
            "android {\n    defaultConfig {\n        foo.applicationId = \"com.example.qualified\"\n    }\n}\n",
        ),
        (
            "infix/build.gradle",
            "android {\n    defaultConfig {\n        foo applicationId 'com.example.infix'\n    }\n}\n",
        ),
        (
            "declared/build.gradle.kts",
            "android {\n    defaultConfig { applicationId = \"com.example.declared\" }\n}\n",
        ),
        (
            "plain/build.gradle.kts",
            "android {\n    defaultConfig {\n        applicationId = \"com.example.plain\"\n    }\n}\n",
        ),
    ]);
    // A member access is not the Gradle key; the same key in a block or at a line start is.
    assert_eq!(
        detect(dir.path()).apps,
        vec![
            app(
                Platform::Android,
                "com.example.declared",
                "declared/build.gradle.kts"
            ),
            app(
                Platform::Android,
                "com.example.plain",
                "plain/build.gradle.kts"
            ),
        ]
    );
}

#[test]
fn android_a_comment_between_a_dot_and_the_key_is_member_access() {
    let dir = tree(&[
        (
            "qualified/build.gradle",
            "android {\n    defaultConfig {\n        foo. /* member */ applicationId = \"com.example.false\"\n    }\n}\n",
        ),
        (
            "declared/build.gradle.kts",
            "android {\n    defaultConfig { applicationId = \"com.example.declared\" }\n}\n",
        ),
    ]);
    // A comment is not a spot where the key becomes a declaration (fix round 3, R3-1).
    assert_eq!(
        detect(dir.path()).apps,
        vec![app(
            Platform::Android,
            "com.example.declared",
            "declared/build.gradle.kts"
        )]
    );
}

#[test]
fn kotlin_multiplatform_needs_a_declaration_in_code() {
    // A comment or an unrelated string that mentions the plugin declares nothing.
    let dir = tree(&[
        (
            "comment/build.gradle.kts",
            "plugins {\n    /*\n    kotlin(\"multiplatform\")\n    id(\"org.jetbrains.kotlin.multiplatform\")\n    */\n    // alias(libs.plugins.kotlin.multiplatform)\n}\n",
        ),
        (
            "string/build.gradle.kts",
            "val note = \"kotlin.multiplatform\"\nval other = \"id(\\\"org.jetbrains.kotlin.multiplatform\\\")\"\n",
        ),
    ]);
    assert!(
        !detect(dir.path())
            .toolchains
            .iter()
            .any(|found| found.kind == ToolchainKind::KotlinMultiplatform),
        "{:?}",
        detect(dir.path()).toolchains
    );

    // Each real declaration shape is code, including the plugin id inside its own string.
    for (name, file, body) in [
        (
            "dsl",
            "build.gradle.kts",
            "plugins {\n    kotlin(\"multiplatform\") apply false\n}\n",
        ),
        (
            "id",
            "build.gradle.kts",
            "plugins {\n    id(\"org.jetbrains.kotlin.multiplatform\") version \"2.2.20\" apply false\n}\n",
        ),
        (
            "alias",
            "build.gradle.kts",
            "plugins {\n    alias(libs.plugins.kotlin.multiplatform) apply false\n}\n",
        ),
        (
            "groovy",
            "build.gradle",
            "plugins {\n    id 'org.jetbrains.kotlin.multiplatform' version '2.2.20' apply false\n}\n",
        ),
    ] {
        let case = tempfile::tempdir().unwrap();
        fs::create_dir_all(case.path().join(name)).unwrap();
        fs::write(case.path().join(name).join(file), body).unwrap();
        assert!(
            detect(case.path())
                .toolchains
                .iter()
                .any(|found| found.kind == ToolchainKind::KotlinMultiplatform),
            "{name}: {:?}",
            detect(case.path()).toolchains
        );
    }
}

#[test]
fn a_capped_build_file_drops_its_partial_final_line() {
    // The 2 MiB read cap lands right after the literal; ` + suffix` follows it on disk only.
    const CAP: usize = 2 * 1024 * 1024;
    let literal = "applicationId = \"com.boundary\"";
    let mut text = String::new();
    while text.len() + literal.len() + 11 <= CAP {
        text.push_str("// padding\n");
    }
    text.push_str(&" ".repeat(CAP - literal.len() - text.len()));
    text.push_str(literal);
    assert_eq!(text.len(), CAP);
    text.push_str(" + suffix\n}\n");

    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("app")).unwrap();
    fs::write(dir.path().join("app/build.gradle"), &text).unwrap();

    // Only whole lines are parsed: the partial line is dropped, so the expression beyond the cap
    // never becomes a literal id (fix round 3, R3-2).
    let detected = detect(dir.path());
    assert!(detected.apps.is_empty(), "{:?}", detected.apps);
    assert!(detected.toolchains.is_empty(), "{:?}", detected.toolchains);
}

#[test]
fn unterminated_strings_and_comments_never_panic_or_report_an_id() {
    let dir = tree(&[
        // `string_end` reports the newline, so the last content byte is a character's tail; the
        // key is inside the literal and is not code.
        (
            "double/build.gradle",
            "val s = \"applicationId = com.example.double хостлер\n",
        ),
        // The end of the file inside a single-quoted literal.
        (
            "single/build.gradle",
            "val s = 'applicationId = com.example.single é",
        ),
        // An unclosed triple quote.
        (
            "triple/build.gradle.kts",
            "val s = \"\"\"applicationId = com.example.triple ü",
        ),
        // An unclosed block comment.
        (
            "comment/build.gradle",
            "/* applicationId = com.example.block ж",
        ),
    ]);
    let detected = detect(dir.path());
    assert!(detected.apps.is_empty(), "{:?}", detected.apps);
    assert!(detected.toolchains.is_empty(), "{:?}", detected.toolchains);
}

#[test]
fn detects_cargo() {
    let detected = detect(&fixture("cargo"));
    assert_eq!(
        detected.toolchains,
        vec![toolchain(ToolchainKind::Cargo, "Cargo.toml")]
    );
    assert_eq!(detected.apps, Vec::new());
    assert_eq!(detected.env_files, Vec::new());
    assert_eq!(detected.agent_docs, Vec::new());
}

#[test]
fn detects_node() {
    let detected = detect(&fixture("node"));
    // A root `package.json` that is not React Native, with its lock file as the evidence.
    assert_eq!(
        detected.toolchains,
        vec![toolchain(ToolchainKind::Node, "package-lock.json")]
    );
    assert_eq!(detected.apps, Vec::new());
}

#[test]
fn empty_detects_nothing() {
    assert_eq!(detect(&fixture("empty")), Detected::default());
}

#[test]
fn bounded_walk() {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture("deep"), dir.path());
    if let Err(error) = link_dir(".", &dir.path().join("loop")) {
        eprintln!("note: the symlink loop was not created ({error}); the other bounds still apply");
    }
    let detected = detect(dir.path());
    // Depth 3 is read, depth 4 is not: `a/b/c/build.gradle` never contributes an id.
    assert_eq!(
        detected.toolchains,
        vec![toolchain(
            ToolchainKind::KotlinMultiplatform,
            "a/b/build.gradle.kts"
        )]
    );
    assert!(
        detected.apps.is_empty(),
        "an applicationId at depth 4 was read: {:?}",
        detected.apps
    );
    // `node_modules` is skipped, so its `react-native` package.json is not seen.
    assert!(
        !detected
            .toolchains
            .iter()
            .any(|found| found.kind == ToolchainKind::ReactNative),
        "{:?}",
        detected.toolchains
    );
}

#[test]
fn walk_stops_at_five_thousand_entries() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["aaaa", "zzzz"] {
        let module = dir.path().join(name);
        fs::create_dir_all(&module).unwrap();
        fs::write(
            module.join("build.gradle"),
            format!(
                "android {{\n    defaultConfig {{\n        applicationId \"com.example.{name}\"\n    }}\n}}\n"
            ),
        )
        .unwrap();
    }
    for index in 0..5_100 {
        fs::write(dir.path().join(format!("f{index:04}")), "").unwrap();
    }
    let detected = detect(dir.path());
    // `aaaa` sorts first and is seen; the cap is spent before `zzzz`, so its id is not.
    assert_eq!(
        detected.apps,
        vec![app(
            Platform::Android,
            "com.example.aaaa",
            "aaaa/build.gradle"
        )]
    );
}

#[test]
fn walk_bound_holds_for_a_large_directory() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["aaaa", "zzzz"] {
        let module = dir.path().join(name);
        fs::create_dir_all(&module).unwrap();
        fs::write(
            module.join("build.gradle"),
            format!(
                "android {{\n    defaultConfig {{\n        applicationId \"com.example.{name}\"\n    }}\n}}\n"
            ),
        )
        .unwrap();
    }
    // Four times the walk's bound in one directory: only the lexicographically first names are
    // held and examined (fix round 1, R1-3).
    for index in 0..20_000 {
        fs::write(dir.path().join(format!("f{index:05}")), "").unwrap();
    }
    let detected = detect(dir.path());
    assert_eq!(
        detected.apps,
        vec![app(
            Platform::Android,
            "com.example.aaaa",
            "aaaa/build.gradle"
        )]
    );
}

#[test]
fn symlinked_root_is_not_walked() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("project-link");
    if let Err(error) = link_dir(fixture("kmp").to_str().unwrap(), &link) {
        eprintln!("note: a directory symlink could not be created ({error}); this case is skipped");
        return;
    }
    // The target itself is detected, so only the symlink's own type explains the empty answer.
    assert!(!detect(&fixture("kmp")).toolchains.is_empty());
    assert_eq!(detect(&link), Detected::default());
}

#[test]
fn symlinked_candidates_are_not_read() {
    let outside = tree(&[
        (
            "package.json",
            "{\"dependencies\":{\"react-native\":\"0.87.1\"}}",
        ),
        (".env.local", "OUTSIDE_KEY=hunter2-outside\n"),
        (
            "build.gradle.kts",
            "plugins {\n    kotlin(\"multiplatform\")\n}\n",
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let links = [
        ("package.json", "package.json"),
        (".env.local", ".env.local"),
        ("build.gradle.kts", "app/build.gradle.kts"),
    ];
    for (from, to) in links {
        let to = root.path().join(to);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let from = outside.path().join(from);
        if let Err(error) = link_file(from.to_str().unwrap(), &to) {
            eprintln!("note: a file symlink could not be created ({error}); this case is skipped");
            return;
        }
    }
    // Nothing behind a symlink is read: no ReactNative, no KotlinMultiplatform, no env names.
    let detected = detect(root.path());
    assert!(detected.toolchains.is_empty(), "{:?}", detected.toolchains);
    assert!(detected.apps.is_empty(), "{:?}", detected.apps);
    assert!(detected.env_files.is_empty(), "{:?}", detected.env_files);
}

#[cfg(unix)]
#[test]
fn special_files_are_never_opened() {
    let dir = tempfile::tempdir().unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(dir.path().join("package.json"))
        .arg(dir.path().join(".env.local"))
        .status();
    if !made.as_ref().is_ok_and(|status| status.success()) {
        eprintln!("note: mkfifo is unavailable ({made:?}); this case is skipped");
        return;
    }
    let root = dir.path().to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(detect(&root));
    });
    // Opening either FIFO would block for a writer that never comes.
    let detected = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("detection must not open a special file");
    assert_eq!(detected, Detected::default());
}

#[test]
fn env_names_never_values() {
    let detected = detect(&fixture("kmp"));
    assert_eq!(
        detected.env_files,
        vec![env_file(
            ".env.local",
            &["JACKETT_API_KEY", "RT_INDEX_API_KEY"]
        )]
    );
    let json = serde_json::to_string(&detected).unwrap();
    assert!(json.contains("JACKETT_API_KEY"), "{json}");
    assert!(!json.contains(MARKER), "a value reached the report: {json}");
    for file in &detected.env_files {
        for name in &file.names {
            assert!(!name.contains(MARKER), "{name}");
        }
    }
}

#[test]
fn env_file_locations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join(".env"), "A=1\n").unwrap();
    fs::write(root.join(".env.local"), "B=2\n").unwrap();
    fs::write(root.join(".env.development.local"), "C=3\n").unwrap();
    fs::write(root.join(".env.production"), "D=4\n").unwrap();
    fs::create_dir_all(root.join("android")).unwrap();
    fs::write(root.join("android/.env.local"), "E=5\n").unwrap();
    fs::write(root.join("android/.env"), "F=6\n").unwrap();
    fs::create_dir_all(root.join("ios/x")).unwrap();
    fs::write(root.join("ios/x/.env.local"), "G=7\n").unwrap();

    assert_eq!(
        detect(root).env_files,
        vec![
            env_file(".env", &["A"]),
            env_file(".env.development.local", &["C"]),
            env_file(".env.local", &["B"]),
            env_file("android/.env.local", &["E"]),
        ]
    );
}

#[test]
fn oversized_and_malformed_env_files_give_no_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut big = String::new();
    for index in 0..30_000 {
        big.push_str(&format!("KEY_{index}=marker\n"));
    }
    fs::write(dir.path().join(".env"), &big).unwrap();
    fs::write(
        dir.path().join(".env.local"),
        "BAD-NAME=marker\nNO_EQUALS\n",
    )
    .unwrap();

    assert_eq!(
        detect(dir.path()).env_files,
        vec![env_file(".env", &[]), env_file(".env.local", &[])]
    );
}

#[test]
fn env_files_with_problems_give_no_names() {
    let dir = tree(&[
        // One good line and one bad value: the whole file gives no names (fix round 1, R1-1).
        (".env", "GOOD=ok\nBAD=\"unterminated\n"),
        (".env.local", "A=one\nA=two\n"),
        (".env.development.local", "A=\"\\q\"\n"),
        (".env.staging.local", "A='x'tail\n"),
        ("android/.env.local", "BAD-NAME=marker\nNO_EQUALS\n"),
    ]);
    // Malformed UTF-8 is a problem, not a name source.
    fs::write(
        dir.path().join(".env.production.local"),
        [b'A', b'=', 0xff, b'\n'],
    )
    .unwrap();

    assert_eq!(
        detect(dir.path()).env_files,
        vec![
            env_file(".env", &[]),
            env_file(".env.development.local", &[]),
            env_file(".env.local", &[]),
            env_file(".env.production.local", &[]),
            env_file(".env.staging.local", &[]),
            env_file("android/.env.local", &[]),
        ]
    );
}

#[test]
fn dotenv_names_matches_parse_names() {
    // The P2 dotenv sources, which live inside `dotenv.rs`'s unit tests rather than in
    // fixtures, plus CRLF and spacing.
    let sources = [
        "# the kmp-muse pilot keys\n\
         \n\
         export JACKETT_URL=\"https://jackett.test\"\n\
         JACKETT_API_KEY='abc-def-123'\n\
         RT_INDEX_URL=https://rt.test # trailing comment\n\
         RT_INDEX_API_KEY = \"line1\\nline2\\t\\\"quoted\\\"\\\\done\"\n",
        "A=\"\"\nB=''\n",
        "\n\n# only a comment\n",
        "  SPACED = value  \n",
        "A=1\r\nB=2\r\n",
        "export KEPT=1\nOTHER=2\n",
    ];
    for source in sources {
        let parsed = dotenv::parse(source).unwrap();
        let expected: Vec<(usize, String)> = parsed
            .iter()
            .map(|entry| (entry.line, entry.name.clone()))
            .collect();
        assert_eq!(dotenv::names(source), expected, "{source:?}");
    }

    let fixture_source = fs::read_to_string(fixture("kmp").join(".env.local")).unwrap();
    let parsed = dotenv::parse(&fixture_source).unwrap();
    let expected: Vec<(usize, String)> = parsed
        .iter()
        .map(|entry| (entry.line, entry.name.clone()))
        .collect();
    assert_eq!(dotenv::names(&fixture_source), expected);
    assert!(!fixture_source.is_empty());
}

#[test]
fn agent_doc_kinds() {
    // `file` and `imports`, including `.github/copilot-instructions.md`.
    let dir = tree(&[
        ("AGENTS.md", "# agents\n"),
        ("CLAUDE.md", "@AGENTS.md\n"),
        (".github/copilot-instructions.md", "@AGENTS.md\n"),
    ]);
    assert_eq!(
        detect(dir.path()).agent_docs,
        vec![
            doc("AGENTS.md", AgentDocKind::File),
            doc(
                "CLAUDE.md",
                AgentDocKind::Imports {
                    target: "AGENTS.md".into()
                }
            ),
            doc(
                ".github/copilot-instructions.md",
                AgentDocKind::Imports {
                    target: "AGENTS.md".into()
                }
            ),
        ]
    );

    // `symlink_to`: the kmp fixture's `CLAUDE.md`, created here because git on Windows may not
    // keep a symlink.
    let linked = tempfile::tempdir().unwrap();
    copy_tree(&fixture("kmp"), linked.path());
    if let Err(error) = link_file("AGENTS.md", &linked.path().join("CLAUDE.md")) {
        eprintln!("note: a symlink could not be created ({error}); the symlink case is skipped");
        return;
    }
    let detected = detect(linked.path());
    assert_eq!(
        detected.agent_docs,
        vec![
            doc("AGENTS.md", AgentDocKind::File),
            doc(
                "CLAUDE.md",
                AgentDocKind::SymlinkTo {
                    target: "AGENTS.md".into()
                }
            ),
        ]
    );
    // The wire shape (T1 review R1-1): one discriminator, and the other file is `target`.
    assert_eq!(
        serde_json::to_value(&detected.agent_docs[1]).unwrap(),
        serde_json::json!({"path": "CLAUDE.md", "kind": "symlink_to", "target": "AGENTS.md"})
    );
}

#[test]
fn agent_doc_import_needs_the_exact_line() {
    // Only a line exactly `@AGENTS.md` is Claude Code's import (fix round 1, R1-7).
    let dir = tree(&[
        ("AGENTS.md", "# agents\n"),
        ("CLAUDE.md", "  @AGENTS.md  \n"),
    ]);
    assert_eq!(
        detect(dir.path()).agent_docs,
        vec![
            doc("AGENTS.md", AgentDocKind::File),
            doc("CLAUDE.md", AgentDocKind::File),
        ]
    );
}
