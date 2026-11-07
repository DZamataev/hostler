//! Shared helpers of the secrets integration tests (plan P2). Every `Home` has its own
//! `HOSTLER_HOME` and stops its service on drop.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use hostler::client::Client;
use hostler::contract::HostlerError;
use hostler::contract::secrets::{Materialize, MaterializedSecret};
use hostler::contract::wire::Request;
use hostler::paths::Paths;
use serde_json::Value;
use tempfile::TempDir;

pub const BIN: &str = env!("CARGO_BIN_EXE_hostler");

pub struct Home {
    pub home: PathBuf,
    pub dir: TempDir,
}

impl Home {
    pub fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        Home {
            home: dir.path().join("home"),
            dir,
        }
    }

    pub fn paths(&self) -> Paths {
        let paths = Paths::under(self.home.clone(), true);
        paths.ensure_dirs().unwrap();
        paths
    }

    /// `hostler` with this home, a fast KDF and no other test hook: the service a command
    /// auto-starts inherits this environment.
    pub fn hostler(&self) -> Command {
        let mut c = Command::new(BIN);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("HOSTLER_TEST_") || name.starts_with("HOSTLER_HEAVY_") {
                c.env_remove(&*name);
            }
        }
        // A ledger that does not exist: this machine's real rn-looky work must not block tests.
        c.env("HOSTLER_HOME", &self.home)
            .env(
                "HOSTLER_LOOKY_LEDGER",
                self.home.join("no-looky-ledger.json"),
            )
            .env("HOSTLER_TEST_FAST_KDF", "1")
            .stdin(Stdio::null());
        c
    }

    /// Starts the service, when it is not running, through the sanitized `hostler()` builder:
    /// it inherits that environment, not the test process's.
    pub fn start_service(&self) {
        let out = self.hostler().args(["service", "start"]).output().unwrap();
        assert!(out.status.success(), "service start: {}", stderr(&out));
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// One call over the socket; `secret.materialize` is refused (use `materialize`).
    pub fn call(&self, request: Request) -> Result<Value, HostlerError> {
        self.start_service();
        let paths = self.paths();
        Home::runtime().block_on(async { Client::connect(&paths).await?.call(request).await })
    }

    /// `secret.materialize`, decoded straight into redacted values.
    pub fn materialize(&self, m: Materialize) -> Result<Vec<MaterializedSecret>, HostlerError> {
        self.start_service();
        let paths = self.paths();
        Home::runtime().block_on(async {
            Client::connect(&paths)
                .await?
                .call_sensitive(Request::SecretMaterialize(m))
                .await
        })
    }

    /// Answers for `HOSTLER_TEST_TTY`, one prompt per line.
    fn tty_file(&self, lines: &[&str]) -> PathBuf {
        let path = self
            .dir
            .path()
            .join(format!("tty-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    /// `hostler unlock` creating a new database with `password`.
    pub fn unlock_new(&self, password: &str) -> Output {
        let tty = self.tty_file(&["y", password, password]);
        let out = self
            .hostler()
            .arg("unlock")
            .env("HOSTLER_TEST_TTY", tty)
            .output()
            .unwrap();
        assert!(out.status.success(), "unlock failed: {}", stderr(&out));
        out
    }

    /// `hostler project approve` in `repo`, typing the project name (its directory name).
    pub fn approve(&self, repo: &Path) -> Output {
        let name = repo.file_name().unwrap().to_str().unwrap();
        let tty = self.tty_file(&[name]);
        let out = self
            .hostler()
            .args(["project", "approve"])
            .current_dir(repo)
            .env("HOSTLER_TEST_TTY", tty)
            .output()
            .unwrap();
        assert!(out.status.success(), "approve failed: {}", stderr(&out));
        out
    }

    /// `hostler secret set NAME` in `repo` with the value on stdin.
    pub fn set(&self, repo: &Path, name: &str, value: &str) -> Output {
        let mut child = self
            .hostler()
            .args(["secret", "set", name])
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(value.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        // Never print the value, even when the command failed.
        assert!(out.status.success(), "secret set {name} failed");
        out
    }

    /// A directory that counts as a git checkout: `.git/HEAD`.
    pub fn make_repo(&self, name: &str) -> PathBuf {
        let repo = self.dir.path().join("repos").join(name);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        repo
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.hostler().args(["service", "stop", "--force"]).output();
    }
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
