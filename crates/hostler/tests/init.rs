//! `hostler init` (plan P4 Task 5): the preview and `--write`, the docs block, the steps, and
//! the service column of the plan's table.
//!
//! Every test runs in its own `HOSTLER_HOME` (a custom home: no OS registration is touched) or
//! in process with a fake registrar (no launchd, no systemd, no Task Scheduler). The trees are
//! copies of Task 3's synthetic fixtures; every env-file value is a marker.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hostler::init::{InitOptions, InitRig, run_with};
use hostler::paths::Paths;
use hostler::service::ServiceManager;
use hostler::service::manager::{Registrar, RegistrationState};
use serde_json::{Value, json};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_hostler");

/// Every fixture env-file value starts with this.
const MARKER: &str = "hunter2";

// ---------------------------------------------------------------------------------------------
// A home and a checkout

/// A `HOSTLER_HOME` of its own; the commands it builds never inherit another test's hooks.
struct TestHome {
    home: PathBuf,
    /// Held so the home outlives every service and file of the test.
    _dir: TempDir,
}

impl TestHome {
    fn new() -> TestHome {
        TestHome::named("home")
    }

    /// A home whose path holds a space, so a printed path needs quoting.
    fn spaced() -> TestHome {
        TestHome::named("hostler home")
    }

    fn named(name: &str) -> TestHome {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(name);
        TestHome { home, _dir: dir }
    }

    fn dir(&self) -> &Path {
        self._dir.path()
    }

    fn hostler(&self) -> Command {
        let mut command = Command::new(BIN);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("HOSTLER_TEST_") || name.starts_with("HOSTLER_HEAVY_") {
                command.env_remove(&*name);
            }
        }
        command
            .env("HOSTLER_HOME", &self.home)
            .env(
                "HOSTLER_LOOKY_LEDGER",
                self.home.join("no-looky-ledger.json"),
            )
            .env("HOSTLER_TEST_FAST_KDF", "1")
            // L58: a service this test started stops when the test process is gone.
            .env("HOSTLER_TEST_OWNER_PID", std::process::id().to_string())
            .stdin(Stdio::null());
        command
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.hostler().args(args).current_dir(dir).output().unwrap()
    }

    /// The same directory as the default home, so a registration applies to it.
    fn default_paths(&self) -> Paths {
        let paths = Paths::under(self.home.clone(), false);
        paths.ensure_dirs().unwrap();
        paths
    }

    fn paths(&self) -> Paths {
        let paths = Paths::under(self.home.clone(), true);
        paths.ensure_dirs().unwrap();
        paths
    }

    fn start_service(&self) {
        let out = self.hostler().args(["service", "start"]).output().unwrap();
        assert!(out.status.success(), "service start: {}", stderr(&out));
    }
}

/// A git checkout: `.git/HEAD`, copied from a fixture when one is named.
fn checkout(fixture: Option<&str>) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    if let Some(fixture) = fixture {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/init")
            .join(fixture);
        copy_tree(&from, dir.path());
    }
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

/// Every path and byte of the tree, so a write of any kind changes it.
fn digest(root: &Path) -> u64 {
    let mut files = Vec::new();
    collect(root, "", &mut files);
    files.sort();
    let mut hasher = DefaultHasher::new();
    for rel in files {
        rel.hash(&mut hasher);
        let path = root.join(&rel);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                "link".hash(&mut hasher);
                std::fs::read_link(&path).unwrap().hash(&mut hasher);
            }
            Ok(metadata) if metadata.is_dir() => "dir".hash(&mut hasher),
            Ok(_) => {
                "file".hash(&mut hasher);
                std::fs::read(&path).unwrap().hash(&mut hasher);
            }
            Err(_) => "gone".hash(&mut hasher),
        }
    }
    hasher.finish()
}

fn collect(root: &Path, prefix: &str, files: &mut Vec<String>) {
    for entry in std::fs::read_dir(root.join(prefix)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        files.push(rel.clone());
        if entry.file_type().unwrap().is_dir() {
            collect(root, &rel, files);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The binary

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `init` in `repo` under `home`.
fn init(home: &TestHome, repo: &Path, args: &[&str]) -> Output {
    let mut full = vec!["init"];
    full.extend_from_slice(args);
    home.run(repo, &full)
}

/// The `data` of an `ok:true` envelope, or the whole JSON of an `ok:false` one.
fn report(out: &Output) -> Value {
    let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): stdout {:?} stderr {:?}",
            stdout(out),
            stderr(out)
        )
    });
    value["data"].clone()
}

fn step_ids(report: &Value) -> Vec<String> {
    report["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| step["id"].as_str().unwrap().to_string())
        .collect()
}

fn step<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["id"] == id)
        .unwrap_or_else(|| panic!("no step {id}: {report}"))
}

fn service_kind(report: &Value) -> &str {
    report["service"]["kind"].as_str().unwrap()
}

fn docs_action(report: &Value, path: &str) -> String {
    let change = report["docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["path"] == path)
        .unwrap_or_else(|| panic!("no docs change for {path}: {report}"));
    change["action"]["kind"].as_str().unwrap().to_string()
}

#[test]
fn preview_writes_nothing() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    let before = digest(repo.path());

    let out = init(&home, repo.path(), &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let plain = stdout(&out);
    assert!(plain.contains("AGENTS.md"), "{plain}");
    assert!(plain.contains("for the person"), "{plain}");
    assert_eq!(digest(repo.path()), before, "the preview wrote a file");

    let json_out = init(&home, repo.path(), &["--json"]);
    assert!(json_out.status.success(), "{}", stderr(&json_out));
    let report = report(&json_out);
    assert_eq!(report["config"]["kind"], "absent");
    assert!(
        report["proposed_toml"]
            .as_str()
            .unwrap()
            .contains("com.example.muse")
    );
    assert_eq!(report["written"], json!([]));
    assert_eq!(digest(repo.path()), before, "the JSON preview wrote a file");
}

#[test]
fn write_creates_toml_and_block() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);

    let toml = read(repo.path(), "hostler.toml");
    assert!(toml.contains("[apps]"), "{toml}");
    assert!(toml.contains("\"com.example.muse\""), "{toml}");
    assert!(toml.contains("[secrets.JACKETT_API_KEY]"), "{toml}");
    assert!(
        toml.contains("TODO: say what JACKETT_API_KEY is for"),
        "{toml}"
    );
    assert!(toml.contains("[heavy]"), "{toml}");
    assert!(toml.contains("./gradlew"), "{toml}");

    let agents = read(repo.path(), "AGENTS.md");
    assert!(agents.contains("<!-- hostler:begin -->"), "{agents}");
    assert!(agents.contains("<!-- hostler:end -->"), "{agents}");
    assert!(agents.contains("## hostler"), "{agents}");
    assert!(agents.starts_with("# muse"), "{agents}");
    assert_eq!(docs_action(&report, "AGENTS.md"), "insert");
    assert_eq!(report["written"], json!(["hostler.toml", "AGENTS.md"]));
    assert_eq!(report["config"]["kind"], "absent");

    // The proposal parses: a second run reports it valid and never rewrites it.
    assert_eq!(
        hostler::project::config::parse(&toml).map(|_| ()),
        Ok(()),
        "{toml}"
    );
}

#[test]
fn second_write_changes_nothing() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    assert!(init(&home, repo.path(), &["--write"]).status.success());
    let toml = read(repo.path(), "hostler.toml");
    let agents = read(repo.path(), "AGENTS.md");
    let before = digest(repo.path());

    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(report["config"]["kind"], "valid");
    assert_eq!(report["written"], json!([]));
    assert_eq!(docs_action(&report, "AGENTS.md"), "unchanged");
    assert_eq!(read(repo.path(), "hostler.toml"), toml);
    assert_eq!(read(repo.path(), "AGENTS.md"), agents);
    assert_eq!(digest(repo.path()), before);
}

#[test]
fn existing_toml_is_never_rewritten() {
    let home = TestHome::new();
    // Invalid: exit 1, nothing written, the file untouched.
    let repo = checkout(Some("kmp"));
    write(repo.path(), "hostler.toml", "[dev]\nport = 3000\n");
    let agents = read(repo.path(), "AGENTS.md");
    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "config_invalid", "{value}");
    assert_eq!(value["error"]["details"]["problems"][0]["line"], 1);
    assert_eq!(value["data"]["config"]["kind"], "invalid");
    assert_eq!(value["data"]["written"], json!([]));
    assert_eq!(read(repo.path(), "hostler.toml"), "[dev]\nport = 3000\n");
    assert_eq!(read(repo.path(), "AGENTS.md"), agents, "a doc was written");

    // Valid: steps only, byte-identical files.
    let repo = checkout(Some("kmp"));
    let toml = "[agents]\ndocs = [\"AGENTS.md\"]\n";
    write(repo.path(), "hostler.toml", toml);
    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(report["config"]["kind"], "valid");
    assert_eq!(report["written"], json!(["AGENTS.md"]));
    assert_eq!(read(repo.path(), "hostler.toml"), toml);
    let ids = step_ids(&report);
    assert!(ids.contains(&"init.commit".to_string()));
    // The config declares no app ids, so the agent is told which ones detection found.
    assert!(ids.contains(&"init.config.apps".to_string()), "{report}");
    let apps = step(&report, "init.config.apps");
    assert_eq!(apps["who"], "agent");
    assert!(
        apps["text"].as_str().unwrap().contains("com.example.muse"),
        "{apps}"
    );
    assert!(apps["command"].is_null(), "{apps}");
}

#[test]
fn block_replaced_in_place() {
    let home = TestHome::new();
    let repo = checkout(None);
    // An existing block is replaced in place: the text above and below survives byte for byte,
    // and the CRLF line endings stay.
    let between = "<!-- hostler:begin -->\r\n## hostler\r\nstale\r\n<!-- hostler:end -->";
    let update =
        format!("# muse\r\n\r\nUser notes above.\r\n\r\n{between}\r\n\r\nUser notes below.\r\n");
    write(repo.path(), "AGENTS.md", &update);
    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(docs_action(&report, "AGENTS.md"), "update");
    let after = read(repo.path(), "AGENTS.md");
    assert!(
        after.starts_with("# muse\r\n\r\nUser notes above.\r\n\r\n"),
        "{after:?}"
    );
    assert!(
        after.ends_with("\r\n\r\nUser notes below.\r\n"),
        "{after:?}"
    );
    assert!(!after.contains("stale"), "{after:?}");
    assert!(!after.contains("\n\n\n"), "{after:?}");
    // One block, markers included, with CRLF throughout.
    assert_eq!(after.matches("<!-- hostler:begin -->").count(), 1);
    assert!(!after.contains("hostler:begin -->\n"), "{after:?}");
}

#[test]
fn broken_markers_refuse() {
    let home = TestHome::new();
    // R1-3: every marker problem, before anything is written or done.
    for broken in [
        "# muse\n\n<!-- hostler:begin -->\nx\n<!-- hostler:end -->\n<!-- hostler:begin -->\ny\n",
        "# muse\n\n<!-- hostler:begin -->\nx\n<!-- hostler:end -->\ny\n<!-- hostler:end -->\n",
        "# muse\r\n\r\n<!-- hostler:begin -->\r\nx\r\n<!-- hostler:end -->\r\ny\r\n<!-- hostler:end -->\r\n",
        "# muse\n\n<!-- hostler:end -->\n<!-- hostler:begin -->\nx\n<!-- hostler:end -->\n",
        "# muse\n\n<!-- hostler:begin -->\nx\n",
    ] {
        for args in [&["--write", "--json"][..], &["--json"]] {
            let repo = checkout(None);
            write(repo.path(), "AGENTS.md", broken);
            let out = init(&home, repo.path(), args);
            assert_eq!(
                out.status.code(),
                Some(1),
                "{broken:?} {args:?}: {}",
                stderr(&out)
            );
            let value: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(
                value["error"]["code"], "config_invalid",
                "{broken:?}: {value}"
            );
            let message = value["error"]["message"].as_str().unwrap();
            assert!(message.contains("AGENTS.md"), "{broken:?}: {value}");
            assert!(
                value["error"]["details"]["problems"][0]["line"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0,
                "{broken:?}: {value}"
            );
            assert!(
                !repo.path().join("hostler.toml").exists(),
                "{broken:?}: a toml was written"
            );
            assert_eq!(read(repo.path(), "AGENTS.md"), broken, "{broken:?}");
        }
    }
}

#[test]
#[cfg_attr(not(unix), allow(unused_variables))] // the symlink half is Unix-only
fn claude_symlink_and_import_get_no_block() {
    let home = TestHome::new();
    // A symlink to AGENTS.md: covered by it, and the link stays a link.
    let repo = checkout(Some("kmp"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("AGENTS.md", repo.path().join("CLAUDE.md")).unwrap();
        let out = init(&home, repo.path(), &["--write", "--json"]);
        assert!(out.status.success(), "{}", stderr(&out));
        let report = report(&out);
        assert_eq!(docs_action(&report, "CLAUDE.md"), "covered_by");
        assert!(
            std::fs::symlink_metadata(repo.path().join("CLAUDE.md"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink was replaced by a file"
        );
        assert_eq!(
            read(repo.path(), "CLAUDE.md"),
            read(repo.path(), "AGENTS.md")
        );
        assert_eq!(
            read(repo.path(), "AGENTS.md")
                .matches("<!-- hostler:begin -->")
                .count(),
            1
        );
    }

    // `@AGENTS.md` import: no second block, and the file is untouched.
    let home_import = TestHome::new();
    let repo = checkout(Some("rn"));
    let claude = read(repo.path(), "CLAUDE.md");
    let out = init(&home_import, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(docs_action(&report, "CLAUDE.md"), "covered_by");
    assert_eq!(read(repo.path(), "CLAUDE.md"), claude);
    assert!(read(repo.path(), "AGENTS.md").contains("<!-- hostler:begin -->"));
    let _ = home;
}

#[test]
fn plain_claude_gets_a_block() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    write(repo.path(), "CLAUDE.md", "# claude\n");
    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(docs_action(&report, "AGENTS.md"), "insert");
    assert_eq!(docs_action(&report, "CLAUDE.md"), "insert");
    assert!(read(repo.path(), "CLAUDE.md").starts_with("# claude\n"));
    assert!(read(repo.path(), "CLAUDE.md").contains("<!-- hostler:begin -->"));
}

#[test]
fn device_rules_step_until_set() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    let out = init(&home, repo.path(), &["--json"]);
    let first = report(&out);
    let prompt = step(&first, "init.device_rules");
    assert_eq!(prompt["who"], "agent");
    let text = prompt["text"].as_str().unwrap();
    assert!(
        text.contains("Propose device-handling instructions"),
        "{text}"
    );
    assert!(text.contains("[agents] device_rules"), "{text}");
    assert!(prompt["command"].is_null(), "{prompt}");

    // Set: the step is gone, and the block names the rules.
    write(
        repo.path(),
        "hostler.toml",
        "[agents]\ndocs = [\"AGENTS.md\"]\ndevice_rules = \"AGENTS.md#device-handling\"\n",
    );
    let out = init(&home, repo.path(), &["--write", "--json"]);
    let set = report(&out);
    assert!(
        !step_ids(&set).contains(&"init.device_rules".to_string()),
        "{set}"
    );
    assert!(
        read(repo.path(), "AGENTS.md")
            .contains("Device-handling rules for this project: AGENTS.md#device-handling"),
        "{}",
        read(repo.path(), "AGENTS.md")
    );
}

#[test]
fn person_steps_are_listed_not_run() {
    let home = TestHome::new();
    home.start_service();
    let repo = checkout(Some("kmp"));
    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    let ids = step_ids(&report);
    // The project is unapproved and the database is locked: both are person steps.
    assert!(ids.contains(&"init.approve".to_string()), "{report}");
    assert!(ids.contains(&"init.unlock".to_string()), "{report}");
    let import = ids
        .iter()
        .find(|id| id.starts_with("init.secrets.import."))
        .unwrap_or_else(|| panic!("no import step: {report}"))
        .clone();
    assert_eq!(step(&report, &import)["who"], "person");
    assert_eq!(step(&report, "init.approve")["who"], "person");
    assert_eq!(step(&report, "init.unlock")["who"], "person");
    assert!(
        !ids.iter()
            .any(|id| id.starts_with("init.secrets.") && id != &import),
        "a request step needs an approved, unlocked project: {report}"
    );

    // Nothing person-only ran: the project is still unapproved and the database still locked.
    let project: Value =
        serde_json::from_slice(&home.run(repo.path(), &["project", "--json"]).stdout).unwrap();
    assert_eq!(project["data"]["approved"], false, "{project}");
    let status: Value = serde_json::from_slice(
        &home
            .run(repo.path(), &["secret", "status", "--json"])
            .stdout,
    )
    .unwrap();
    assert_ne!(status["data"]["state"], "unlocked", "{status}");
}

#[test]
fn env_values_never_printed() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    for args in [&[][..], &["--json"], &["--write"], &["--write", "--json"]] {
        let out = init(&home, repo.path(), args);
        let mut text = format!("{}\n{}", stdout(&out), stderr(&out));
        for rel in ["hostler.toml", "AGENTS.md"] {
            if let Ok(body) = std::fs::read_to_string(repo.path().join(rel)) {
                text.push_str(&body);
            }
        }
        assert!(
            !text.contains(MARKER),
            "a value was printed for {args:?}: {text}"
        );
        // The names are the plan's input and may appear; the values never do.
        assert!(!text.to_lowercase().contains("hunter2"), "{text}");
    }
}

#[test]
fn custom_home_never_installs() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    for args in [&["--write", "--json"][..], &["--json"]] {
        let out = init(&home, repo.path(), args);
        assert!(out.status.success(), "{}", stderr(&out));
        let report = report(&out);
        assert_eq!(service_kind(&report), "install_refused", "{report}");
        let message = report["service"]["message"].as_str().unwrap();
        assert!(message.contains("HOSTLER_HOME"), "{message}");
        // P4 Task 9 F-3: the background start, never the foreground `service run`.
        assert!(message.contains("hostler service start"), "{message}");
        assert!(!message.contains("service run"), "{message}");
        let service = step(&report, "init.service");
        assert_eq!(service["who"], "agent", "{service}");
        assert_eq!(service["command"], "hostler service start", "{service}");
        assert!(!home.home.join("service.lock").exists());
    }
}

#[test]
fn sandbox_step_prints_both_settings() {
    let home = TestHome::spaced();
    let repo = checkout(Some("kmp"));
    for args in [&[][..], &["--write"]] {
        let out = init(&home, repo.path(), args);
        assert!(out.status.success(), "{}", stderr(&out));
        let report: Value = {
            let value: Value =
                serde_json::from_slice(&home.run(repo.path(), &["init", "--json"]).stdout).unwrap();
            value["data"].clone()
        };
        let sandbox = step(&report, "init.sandbox");
        assert_eq!(sandbox["who"], "person");
        assert!(sandbox["argv"].is_null(), "{sandbox}");
        let text = sandbox["text"].as_str().unwrap();
        let rules = include_str!("../src/guide/codex_hostler.rules");
        assert!(text.contains(rules), "{text}");
        assert!(text.contains("~/.codex/rules/hostler.rules"), "{text}");
        assert!(
            text.contains("<checkout>/.codex/rules/hostler.rules"),
            "{text}"
        );
        assert!(text.contains("allowUnixSockets"), "{text}");
        let socket = serde_json::to_string(
            &home
                .paths()
                .socket
                .clone()
                .file()
                .expect("a file socket")
                .display()
                .to_string(),
        )
        .unwrap();
        assert!(text.contains(&socket), "{text}");
        assert!(text.contains("VAR="), "{text}");
        assert!(!text.contains("network_proxy"), "{text}");

        // Neither client's files were written, in the checkout or under the test's home.
        assert!(!repo.path().join(".codex").exists());
        assert!(!repo.path().join(".claude").exists());
        assert!(!home.home.join(".codex").exists());
        assert!(!home.home.join(".claude").exists());
    }
}

/// `SocketName::File`'s path, for the sandbox step's JSON.
trait SocketFile {
    fn file(self) -> Option<PathBuf>;
}

impl SocketFile for hostler::paths::SocketName {
    fn file(self) -> Option<PathBuf> {
        match self {
            hostler::paths::SocketName::File(path) => Some(path),
            hostler::paths::SocketName::Namespaced(_) => None,
        }
    }
}

#[test]
fn block_has_only_placeholders() {
    let home = TestHome::new();
    let repo = checkout(Some("kmp"));
    write(
        repo.path(),
        "hostler.toml",
        "[agents]\ndocs = [\"AGENTS.md\"]\n\n[secrets.JACKETT_API_KEY]\npurpose = \"the torrent search key\"\n",
    );
    let out = init(&home, repo.path(), &["--write"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let block = read(repo.path(), "AGENTS.md");
    assert!(block.contains("<NAME>"), "{block}");
    assert!(!block.contains("JACKETT_API_KEY"), "{block}");
    assert!(!block.contains("com.example.muse"), "{block}");
    assert!(block.contains("-- <cmd>"), "{block}");
    assert!(block.contains("<alias"), "{block}");
    assert!(block.contains("<why>"), "{block}");
}

#[cfg(unix)]
#[test]
fn docs_path_with_a_symlinked_parent_is_refused() {
    // R1-1: a configured docs path must not leave the checkout through a symlinked directory.
    let home = TestHome::new();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "out.md", "external\n");
    let before = digest(outside.path());

    for (toml, target) in [
        ("[agents]\ndocs = [\"link/out.md\"]\n", "out.md"),
        ("[agents]\ndocs = [\"link/new.md\"]\n", "new.md"),
        ("[agents]\ndocs = [\"a/b/link/out.md\"]\n", "out.md"),
    ] {
        let repo = checkout(None);
        std::os::unix::fs::symlink(outside.path(), repo.path().join("link")).unwrap();
        std::fs::create_dir_all(repo.path().join("a/b")).unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join("a/b/link")).unwrap();
        write(repo.path(), "hostler.toml", toml);

        let out = init(&home, repo.path(), &["--write", "--json"]);
        assert_eq!(out.status.code(), Some(1), "{toml}: {}", stderr(&out));
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["error"]["code"], "config_invalid", "{toml}: {value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains(target), "{toml}: {value}");
        assert!(message.contains("symlink"), "{toml}: {value}");
        // The external tree is byte-identical: nothing was created or rewritten.
        assert_eq!(digest(outside.path()), before, "{toml}");
        assert!(!outside.path().join("new.md").exists(), "{toml}");
        assert_eq!(read(repo.path(), "hostler.toml"), toml);
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_agents_md_is_never_written() {
    // R1-1: a docs file that is a symlink is left alone; its target is never written through.
    let home = TestHome::new();
    let repo = checkout(None);
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "agents.md", "# outside\n");
    std::os::unix::fs::symlink(
        outside.path().join("agents.md"),
        repo.path().join("AGENTS.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink("AGENTS.md", repo.path().join("CLAUDE.md")).unwrap();
    let before = digest(outside.path());

    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(read(outside.path(), "agents.md"), "# outside\n");
    assert_eq!(digest(outside.path()), before);
    assert!(
        std::fs::symlink_metadata(repo.path().join("AGENTS.md"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink was replaced by a regular file"
    );
    assert!(
        !report["docs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["path"] == "AGENTS.md"),
        "a symlink was planned for a block: {report}"
    );
    // `CLAUDE.md` links to `AGENTS.md`, so it is covered by it, not written either.
    assert_eq!(docs_action(&report, "CLAUDE.md"), "covered_by");
    // The config itself is still written.
    assert!(repo.path().join("hostler.toml").exists());
}

#[cfg(unix)]
#[test]
fn non_utf8_checkout_path_is_refused() {
    // R1-2: the checkout identity is `project::current`'s (lossless UTF-8); init keeps it.
    // A worktree whose own root is not valid UTF-8, as tests/project_resolve.rs makes one.
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let home = TestHome::new();
    let parent = tempfile::tempdir().unwrap();
    let main = parent.path().join("main");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let worktree_git = main.join(".git/worktrees/wt");
    std::fs::create_dir_all(&worktree_git).unwrap();
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
    std::fs::write(worktree_git.join("commondir"), "../..\n").unwrap();
    let repo = parent.path().join(OsString::from_vec(b"wt-\xff".to_vec()));
    if std::fs::create_dir_all(&repo).is_err() {
        eprintln!("skipping: this filesystem rejects non-UTF-8 names");
        return;
    }
    std::fs::write(
        repo.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )
    .unwrap();

    let out = init(&home, &repo, &["--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["error"]["code"], "not_in_project", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not valid UTF-8"),
        "{value}"
    );
    assert!(!repo.join("hostler.toml").exists());
}

#[test]
fn ranges_is_reserved_and_warned() {
    // R1-2: the reserved-table note is `project::current`'s, printed once per process.
    let home = TestHome::new();
    let repo = checkout(None);
    write(repo.path(), "hostler.toml", "[ranges]\nmin = 1\n");
    let out = init(&home, repo.path(), &["--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("[ranges] is reserved for a later hostler version"),
        "{}",
        stderr(&out)
    );
}

#[cfg(unix)]
#[test]
fn a_pre_existing_temp_name_survives_init() {
    // R2-3: the fixed temp name of an earlier hostler is a user's file, not ours to delete.
    let home = TestHome::new();
    let repo = checkout(None);
    write(repo.path(), "AGENTS.md", "# muse\n");
    write(repo.path(), ".AGENTS.md.hostler-tmp", "user data\n");

    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(read(repo.path(), ".AGENTS.md.hostler-tmp"), "user data\n");
    assert!(read(repo.path(), "AGENTS.md").contains("<!-- hostler:begin -->"));
    // No temp of init's is left behind.
    let leftovers: Vec<String> = std::fs::read_dir(repo.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".hostler-") && name != ".AGENTS.md.hostler-tmp")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[cfg(unix)]
#[test]
fn a_read_only_docs_file_is_refused() {
    // R2-5: a read-only agent doc is not replaced, and its mode stays as the person left it.
    use std::os::unix::fs::PermissionsExt;

    let home = TestHome::new();
    let repo = checkout(None);
    let body = "# muse\n\nUser notes.\n";
    write(repo.path(), "AGENTS.md", body);
    std::fs::set_permissions(
        repo.path().join("AGENTS.md"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();

    let out = init(&home, repo.path(), &["--write", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["error"]["code"], "config_invalid", "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("AGENTS.md"), "{value}");
    assert!(message.contains("read-only"), "{value}");
    assert_eq!(read(repo.path(), "AGENTS.md"), body);
    assert_eq!(
        std::fs::metadata(repo.path().join("AGENTS.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
    // Nothing was written at all, not even the proposal.
    assert!(!repo.path().join("hostler.toml").exists());
}

#[test]
fn outside_a_checkout_it_is_not_in_project() {
    let home = TestHome::new();
    let dir = tempfile::tempdir().unwrap();
    let out = home.run(dir.path(), &["init", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["error"]["code"], "not_in_project", "{value}");
    assert_eq!(value["data"], Value::Null, "{value}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_steps_need_an_approved_unlocked_project() {
    let home = TestHome::new();
    let _service = Service::start(&home, &[]);
    let repo = checkout(None);
    write(
        repo.path(),
        "hostler.toml",
        "[agents]\ndocs = [\"AGENTS.md\"]\n\n[secrets.API_KEY]\npurpose = \"the api key\"\n",
    );

    // Unapproved and locked: both are person steps, and no request step is possible.
    let out = init(&home, repo.path(), &["--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let first = report(&out);
    let ids = step_ids(&first);
    assert!(ids.contains(&"init.approve".to_string()), "{first}");
    assert!(ids.contains(&"init.unlock".to_string()), "{first}");
    assert!(
        !ids.contains(&"init.secrets.API_KEY".to_string()),
        "{first}"
    );

    // Approved and unlocked: the declared secret is missing, so the agent asks for it.
    approve(&home, repo.path(), 4).await;
    unlock(&home, 4).await;
    let out = init(&home, repo.path(), &["--json"]);
    let discovered = report(&out);
    let ids = step_ids(&discovered);
    assert!(!ids.contains(&"init.approve".to_string()), "{discovered}");
    assert!(!ids.contains(&"init.unlock".to_string()), "{discovered}");
    let request = step(&discovered, "init.secrets.API_KEY");
    assert_eq!(request["who"], "agent");
    assert_eq!(
        request["argv"],
        json!([
            "hostler",
            "secret",
            "request",
            "API_KEY",
            "--reason",
            "the api key"
        ])
    );

    // Stored, it is no longer missing: init asks for nothing.
    let mut child = home
        .hostler()
        .args(["secret", "set", "API_KEY"])
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"a test value, not a secret")
            .unwrap();
    }
    assert!(child.wait().unwrap().success());
    let out = init(&home, repo.path(), &["--json"]);
    let stored = report(&out);
    assert!(
        !step_ids(&stored).contains(&"init.secrets.API_KEY".to_string()),
        "{stored}"
    );
}

// ---------------------------------------------------------------------------------------------
// The service table, in process with a fake registrar

/// A `hostler service run` started with test variables of its own; dropped: stopped and reaped.
struct Service {
    child: Child,
    home: PathBuf,
}

impl Service {
    fn start(home: &TestHome, vars: &[(&str, &str)]) -> Service {
        let child = home
            .hostler()
            .envs(vars.iter().copied())
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let service = Service {
            child,
            home: home.home.clone(),
        };
        let pid = u64::from(service.child.id());
        assert!(
            wait_for(Duration::from_secs(15), || status_json(home)["data"]["pid"]
                .as_u64()
                == Some(pid)),
            "the test service never answered: {}",
            status_json(home)
        );
        service
    }

    fn pid(&self) -> u64 {
        u64::from(self.child.id())
    }

    fn exits_within(&mut self, limit: Duration) -> bool {
        wait_for(limit, || self.child.try_wait().unwrap().is_some())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new(BIN)
                .env("HOSTLER_HOME", &self.home)
                .args(["service", "stop", "--force"])
                .output();
            if !self.exits_within(Duration::from_secs(10)) {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

/// `hostler service status --json`: lifecycle only, so it reads any contract.
fn status_json(home: &TestHome) -> Value {
    let out = Command::new(BIN)
        .env("HOSTLER_HOME", &home.home)
        .args(["service", "status", "--json"])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

fn pid(home: &TestHome) -> Option<u64> {
    status_json(home)["data"]["pid"].as_u64()
}

/// One heavy entry marked running in the store, held by the live test process.
fn seed_running_heavy(home: &TestHome) {
    use hostler::contract::heavy::HeavyAcquire;
    use hostler::procs::{SysProcessTable, current_identity};
    use hostler::store::Store;

    let store = Store::open(&home.paths().db).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let request = HeavyAcquire {
        id: id.clone(),
        kind: "gradle".into(),
        command: "./gradlew assemble".into(),
        cwd: "/work/app".into(),
        workspace_root: None,
        holder: current_identity(&SysProcessTable::new().unwrap()).unwrap(),
        reentrant_token: None,
        no_wait: false,
    };
    store.insert_waiting(&request, 0).unwrap();
    store.mark_running(&id, 0).unwrap();
}

/// Sends `requests` after a `hello` of `contract` (as a client of that version would); the
/// answers, without the hello's.
async fn raw(
    home: &TestHome,
    contract: u32,
    requests: Vec<hostler::contract::wire::Request>,
) -> Vec<Value> {
    use hostler::contract::wire::{ClientFrame, Request};
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let paths = home.paths();
    let name = hostler::service::server::socket_name(&paths.socket).unwrap();
    let stream = Stream::connect(name).await.unwrap();
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let hello = Request::Hello {
        contract_version: contract,
        client: "test".into(),
    };
    let mut answers = Vec::new();
    for (id, request) in std::iter::once(hello).chain(requests).enumerate() {
        let id = id as u64 + 1;
        let frame = serde_json::to_string(&ClientFrame { id, request }).unwrap();
        write.write_all(frame.as_bytes()).await.unwrap();
        write.write_all(b"\n").await.unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
                .await
                .expect("an answer")
                .unwrap()
                .expect("an open connection");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                answers.push(value);
                break;
            }
        }
    }
    answers.remove(0);
    answers
}

/// Unlocks (creates) the database of a service whose contract may not match.
async fn unlock(home: &TestHome, contract: u32) {
    use hostler::contract::secrets::Unlock;
    use hostler::contract::wire::Request;
    use hostler::secrets::value::SecretValue;

    let answer = raw(
        home,
        contract,
        vec![Request::SecretsUnlock(Unlock {
            password: SecretValue::new("a test password, not a secret".into()),
            key_file: None,
            forget_key_file: false,
            create: true,
        })],
    )
    .await;
    assert!(answer[0]["error"].is_null(), "unlock failed: {}", answer[0]);
}

/// Approves this checkout's project, as `hostler project approve` does after the person's
/// confirmation.
async fn approve(home: &TestHome, repo: &Path, contract: u32) {
    use hostler::contract::wire::Request;

    let project = hostler::project::current(repo).unwrap().project;
    let answer = raw(home, contract, vec![Request::ProjectApprove(project)]).await;
    assert!(
        answer[0]["error"].is_null(),
        "approve failed: {}",
        answer[0]
    );
}

/// Stands in for launchd/systemd/Task Scheduler: records what it was asked to do.
struct FakeRegistrar {
    home: Arc<TestHome>,
    registration: RegistrationState,
    registers: AtomicUsize,
    starts: AtomicUsize,
    children: Mutex<Vec<Child>>,
}

impl FakeRegistrar {
    fn new(home: Arc<TestHome>, registration: RegistrationState) -> Arc<FakeRegistrar> {
        Arc::new(FakeRegistrar {
            home,
            registration,
            registers: AtomicUsize::new(0),
            starts: AtomicUsize::new(0),
            children: Mutex::new(Vec::new()),
        })
    }

    fn registers(&self) -> usize {
        self.registers.load(Ordering::SeqCst)
    }

    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
}

impl ServiceManager for FakeRegistrar {
    fn is_installed(&self) -> bool {
        // What a real registrar reports after `register`.
        true
    }

    fn start(&self) -> anyhow::Result<()> {
        let child = self
            .home
            .hostler()
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.children.lock().unwrap().push(child);
        Ok(())
    }
}

impl Registrar for FakeRegistrar {
    fn register(&self, _exe: &Path, _paths: &Paths) -> anyhow::Result<()> {
        self.registers.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn unregister(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn registration(&self) -> RegistrationState {
        self.registration
    }
}

impl Drop for FakeRegistrar {
    fn drop(&mut self) {
        let _ = Command::new(BIN)
            .env("HOSTLER_HOME", &self.home.home)
            .args(["service", "stop", "--force"])
            .output();
        for mut child in self.children.lock().unwrap().drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn rig(home: &TestHome, registrar: Arc<FakeRegistrar>) -> InitRig {
    rig_with_socket(home, registrar, None)
}

/// The rig, with the client's socket at `front` when one is given (the recording proxy): the
/// state files (the lock, the store) stay in `home`, so an install still sees the service.
fn rig_with_socket(
    home: &TestHome,
    registrar: Arc<FakeRegistrar>,
    front: Option<PathBuf>,
) -> InitRig {
    let paths = home.default_paths();
    let paths = match front {
        Some(socket) => Paths {
            socket: hostler::paths::SocketName::File(socket),
            ..paths
        },
        None => paths,
    };
    InitRig {
        paths: Ok(paths),
        registrar,
        // The test binary is a dev build; every automatic install here says otherwise.
        exe: PathBuf::from(BIN),
        dev_build: Some(false),
        registered_exe: Some(PathBuf::from("/opt/hostler-test/hostler")),
    }
}

/// A socket of its own in front of the service (the proxy binds it).
fn front_socket(home: &TestHome) -> PathBuf {
    let front = Paths::under(home.dir().join("front"), false);
    front.ensure_dirs().unwrap();
    let hostler::paths::SocketName::File(socket) = front.socket else {
        panic!("a namespaced socket cannot be proxied here");
    };
    socket
}

/// Every frame a client sent through a `Proxy`, as JSON.
type Sent = Arc<Mutex<Vec<Value>>>;

/// A socket at `front` that forwards to the service at `back` and records every client frame.
/// Copied from Task 6's `tests/doctor.rs` (P4 review R1-4). It keeps listening after a
/// `service.shutdown`: the service that replaces the old one binds the same back socket.
struct Proxy {
    sent: Sent,
    front: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    fn start(front: &Path, back: &Paths) -> Proxy {
        use interprocess::local_socket::tokio::{Stream, prelude::*};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let front_socket = hostler::paths::SocketName::File(front.to_path_buf());
        let listener = hostler::service::server::bind(&front_socket).unwrap();
        let sent: Sent = Arc::default();
        let back_socket = back.socket.clone();
        let recorded = sent.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok(stream) = listener.accept().await else {
                    break;
                };
                let name = hostler::service::server::socket_name(&back_socket).unwrap();
                let Ok(upstream) = Stream::connect(name).await else {
                    continue;
                };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let (client_read, mut client_write) = tokio::io::split(stream);
                    let (mut up_read, mut up_write) = tokio::io::split(upstream);
                    let down = tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut up_read, &mut client_write).await;
                    });
                    let mut lines = BufReader::new(client_read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Ok(frame) = serde_json::from_str::<Value>(&line) {
                            recorded.lock().unwrap().push(frame);
                        }
                        let mut text = line;
                        text.push('\n');
                        if up_write.write_all(text.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    let _ = down.await;
                });
            }
        });
        Proxy {
            sent,
            front: front.to_path_buf(),
            task,
        }
    }

    fn methods(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|frame| frame["method"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    fn shutdowns(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["method"] == "service.shutdown")
            .map(|frame| frame["params"].clone())
            .collect()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.front);
    }
}

/// The automatic shutdown of a replacement: `if_idle` and `refuse_if_unlocked` both set.
const AUTOMATIC_SHUTDOWN: &str = r#"{"if_idle":true,"refuse_if_unlocked":true}"#;

async fn init_in_process(_home: &TestHome, repo: &Path, write: bool, rig: InitRig) -> Value {
    let report = run_with(
        InitOptions {
            cwd: repo.to_path_buf(),
            write,
        },
        rig,
    )
    .await
    .unwrap_or_else(|e| panic!("init failed: {e:?}"));
    serde_json::to_value(report).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_when_absent() {
    let home = Arc::new(TestHome::new());
    let repo = checkout(Some("kmp"));
    let registrar = FakeRegistrar::new(home.clone(), RegistrationState::Absent);

    // The preview installs nothing.
    let preview = init_in_process(&home, repo.path(), false, rig(&home, registrar.clone())).await;
    assert_eq!(preview["service"]["kind"], "would_install", "{preview}");
    assert_eq!(registrar.registers(), 0);
    assert!(pid(&home).is_none(), "the preview started a service");

    let report = init_in_process(&home, repo.path(), true, rig(&home, registrar.clone())).await;
    assert_eq!(report["service"]["kind"], "installed", "{report}");
    assert_eq!(registrar.registers(), 1);
    assert_eq!(registrar.starts(), 1);
    assert!(
        pid(&home).is_some(),
        "the manager did not start the service"
    );
    assert!(repo.path().join("hostler.toml").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_registration_stopped_service_is_started() {
    let home = Arc::new(TestHome::new());
    let repo = checkout(Some("kmp"));
    let registrar = FakeRegistrar::new(home.clone(), RegistrationState::Current);

    let preview = init_in_process(&home, repo.path(), false, rig(&home, registrar.clone())).await;
    assert_eq!(preview["service"]["kind"], "would_start", "{preview}");
    assert_eq!(registrar.starts(), 0);
    assert!(pid(&home).is_none(), "the preview started a service");

    let report = init_in_process(&home, repo.path(), true, rig(&home, registrar.clone())).await;
    assert_eq!(report["service"]["kind"], "started", "{report}");
    assert_eq!(registrar.starts(), 1);
    assert!(pid(&home).is_some(), "the service was not started");
    assert_eq!(
        registrar.registers(),
        0,
        "a start re-registered the service"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_installs_over_a_busy_unlocked_or_newer_service() {
    // Busy: a heavy task runs, so the automatic stop is refused.
    let home = Arc::new(TestHome::new());
    seed_running_heavy(&home);
    let busy = Service::start(&home, &[]);
    let repo = checkout(Some("kmp"));
    let registrar = FakeRegistrar::new(home.clone(), RegistrationState::Outdated);
    let socket = front_socket(&home);
    let proxy = Proxy::start(&socket, &home.paths());
    let report = init_in_process(
        &home,
        repo.path(),
        true,
        rig_with_socket(&home, registrar.clone(), Some(socket)),
    )
    .await;
    assert_eq!(report["service"]["kind"], "install_refused", "{report}");
    assert_eq!(pid(&home), Some(busy.pid()), "the service was stopped");
    assert_eq!(status_json(&home)["data"]["running_heavy"], 1, "{report}");
    assert_eq!(registrar.registers(), 0);
    assert_eq!(
        step(&report, "init.service")["who"],
        "agent",
        "the agent needs the install step"
    );
    // The atomic stop was attempted and refused: one automatic shutdown, nothing explicit.
    assert_eq!(proxy.shutdowns().len(), 1, "{:?}", proxy.methods());
    assert_eq!(
        proxy.shutdowns()[0].to_string().replace(' ', ""),
        AUTOMATIC_SHUTDOWN
    );
    drop(proxy);
    drop(busy);

    // Unlocked: the database is live work, so nothing stops it.
    let home = Arc::new(TestHome::new());
    let unlocked = Service::start(&home, &[]);
    unlock(&home, 4).await;
    assert_eq!(status_json(&home)["data"]["secrets_unlocked"], true);
    let repo = checkout(Some("kmp"));
    let registrar = FakeRegistrar::new(home.clone(), RegistrationState::Outdated);
    let socket = front_socket(&home);
    let proxy = Proxy::start(&socket, &home.paths());
    let report = init_in_process(
        &home,
        repo.path(),
        true,
        rig_with_socket(&home, registrar.clone(), Some(socket)),
    )
    .await;
    assert_eq!(report["service"]["kind"], "install_refused", "{report}");
    assert_eq!(pid(&home), Some(unlocked.pid()), "the service was stopped");
    assert_eq!(status_json(&home)["data"]["secrets_unlocked"], true);
    assert_eq!(registrar.registers(), 0);
    // An unlocked database is decided before any stop: no shutdown reaches the service.
    assert!(proxy.shutdowns().is_empty(), "{:?}", proxy.methods());
    drop(proxy);
    drop(unlocked);

    // Newer: this client never stops a newer service.
    let home = Arc::new(TestHome::new());
    let newer = Service::start(&home, &[("HOSTLER_TEST_CONTRACT_VERSION", "5")]);
    let repo = checkout(Some("kmp"));
    let registrar = FakeRegistrar::new(home.clone(), RegistrationState::Absent);
    let socket = front_socket(&home);
    let proxy = Proxy::start(&socket, &home.paths());
    let report = init_in_process(
        &home,
        repo.path(),
        true,
        rig_with_socket(&home, registrar.clone(), Some(socket)),
    )
    .await;
    assert_eq!(report["service"]["kind"], "install_refused", "{report}");
    assert!(
        report["service"]["message"]
            .as_str()
            .unwrap()
            .contains("is older than the running service"),
        "{report}"
    );
    assert_eq!(pid(&home), Some(newer.pid()), "the service was stopped");
    assert_eq!(status_json(&home)["data"]["contract_version"], 5);
    assert_eq!(registrar.registers(), 0);
    // A newer service is decided before any stop: no shutdown reaches it.
    assert!(proxy.shutdowns().is_empty(), "{:?}", proxy.methods());
}

/// One mismatch row of the plan's table: the preview column, the `--write` column, and that
/// nothing was registered.
struct Row {
    name: &'static str,
    vars: &'static [(&'static str, &'static str)],
    unlocked: bool,
    busy: bool,
    preview: &'static str,
    written: &'static str,
    direction: Option<&'static str>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_registration_mismatch_rows() {
    let rows = [
        Row {
            name: "older locked idle",
            vars: &[("HOSTLER_TEST_CONTRACT_VERSION", "3")],
            unlocked: false,
            busy: false,
            preview: "would_replace",
            written: "replaced",
            direction: None,
        },
        Row {
            name: "older locked busy",
            vars: &[("HOSTLER_TEST_CONTRACT_VERSION", "3")],
            unlocked: false,
            busy: true,
            preview: "would_replace",
            written: "mismatch_kept",
            direction: Some("newer"),
        },
        Row {
            name: "older unlocked",
            vars: &[("HOSTLER_TEST_CONTRACT_VERSION", "3")],
            unlocked: true,
            busy: false,
            preview: "mismatch_kept",
            written: "mismatch_kept",
            direction: Some("newer"),
        },
        Row {
            name: "newer contract",
            vars: &[("HOSTLER_TEST_CONTRACT_VERSION", "5")],
            unlocked: false,
            busy: false,
            preview: "mismatch_kept",
            written: "mismatch_kept",
            direction: Some("older"),
        },
        Row {
            name: "unparseable version",
            vars: &[
                ("HOSTLER_TEST_CONTRACT_VERSION", "3"),
                ("HOSTLER_TEST_SERVICE_VERSION", "dev"),
            ],
            unlocked: false,
            busy: false,
            preview: "mismatch_kept",
            written: "mismatch_kept",
            direction: Some("unknown"),
        },
    ];
    for row in rows {
        let home = Arc::new(TestHome::new());
        if row.busy {
            seed_running_heavy(&home);
        }
        let mut service = Service::start(&home, row.vars);
        let old_pid = service.pid();
        if row.unlocked {
            unlock(&home, 3).await;
        }
        let repo = checkout(Some("kmp"));
        let registrar = FakeRegistrar::new(home.clone(), RegistrationState::Current);
        // Every frame init's client sends is recorded (R1-4).
        let socket = front_socket(&home);
        let proxy = Proxy::start(&socket, &home.paths());

        // The preview column, changing nothing.
        let preview = init_in_process(
            &home,
            repo.path(),
            false,
            rig_with_socket(&home, registrar.clone(), Some(socket.clone())),
        )
        .await;
        assert_eq!(
            preview["service"]["kind"], row.preview,
            "{}: {preview}",
            row.name
        );
        assert_eq!(
            pid(&home),
            Some(old_pid),
            "{}: the preview changed it",
            row.name
        );
        assert_eq!(
            registrar.registers(),
            0,
            "{}: the preview registered",
            row.name
        );
        assert!(
            proxy.shutdowns().is_empty(),
            "{}: the preview sent a shutdown: {:?}",
            row.name,
            proxy.methods()
        );

        // The `--write` column.
        let report = init_in_process(
            &home,
            repo.path(),
            true,
            rig_with_socket(&home, registrar.clone(), Some(socket)),
        )
        .await;
        assert_eq!(
            report["service"]["kind"], row.written,
            "{}: {report}",
            row.name
        );
        let shutdowns = proxy.shutdowns();
        if row.direction.is_none() {
            // The replacement: one automatic atomic stop, never an explicit one.
            assert_eq!(shutdowns.len(), 1, "{}: {:?}", row.name, proxy.methods());
            assert_eq!(
                shutdowns[0].to_string().replace(' ', ""),
                AUTOMATIC_SHUTDOWN,
                "{}: {}",
                row.name,
                shutdowns[0]
            );
        } else if row.busy {
            // The busy row tried the atomic stop and the service refused it: idleness is only
            // known by trying, so exactly one automatic shutdown is expected — never an
            // explicit one.
            assert_eq!(shutdowns.len(), 1, "{}: {:?}", row.name, proxy.methods());
            assert_eq!(
                shutdowns[0].to_string().replace(' ', ""),
                AUTOMATIC_SHUTDOWN,
                "{}: {}",
                row.name,
                shutdowns[0]
            );
        } else {
            // The other kept rows: `may_stop` refused before the stop, so no shutdown at all.
            assert!(
                shutdowns.is_empty(),
                "{}: a shutdown was sent: {:?}",
                row.name,
                proxy.methods()
            );
        }
        if let Some(direction) = row.direction {
            assert_eq!(
                report["service"]["direction"], direction,
                "{}: {report}",
                row.name
            );
            assert_eq!(
                pid(&home),
                Some(old_pid),
                "{}: the service was stopped",
                row.name
            );
            assert_eq!(registrar.registers(), 0, "{}: it registered", row.name);
            let step = step(&report, "init.service");
            assert_eq!(step["who"], "agent", "{}: {report}", row.name);
        } else {
            // Replaced: a new pid, a matching contract, and the registered binary started.
            assert_ne!(pid(&home), Some(old_pid), "{}: no replacement", row.name);
            assert_eq!(
                status_json(&home)["data"]["contract_version"],
                4,
                "{}: the new service's contract",
                row.name
            );
            assert!(
                service.exits_within(Duration::from_secs(10)),
                "{}: the old service still runs",
                row.name
            );
            assert_eq!(registrar.registers(), 0, "{}: it registered", row.name);
        }
        if row.busy {
            assert_eq!(
                status_json(&home)["data"]["running_heavy"],
                1,
                "{}: the task stopped",
                row.name
            );
            // The mismatch names the blockers, and the step waits for them.
            let message = report["service"]["message"].as_str().unwrap();
            assert!(message.contains("1 running heavy task"), "{message}");
            let step = step(&report, "init.service");
            assert!(
                step["text"]
                    .as_str()
                    .unwrap()
                    .contains("when its work has finished"),
                "{step}"
            );
        }
        if row.unlocked {
            assert_eq!(
                status_json(&home)["data"]["secrets_unlocked"],
                true,
                "{}: the database was locked",
                row.name
            );
        }
    }
}
