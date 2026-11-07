//! Task 9: `hostler secret request | requests | decline` against the real binary and service
//! (plan P2). Timings are generous: these run beside other suites on a loaded machine.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Home, stderr, stdout};
use hostler::contract::secrets::SecretRequestInfo;
use hostler::contract::wire::Request;
use serde_json::Value;

const PASSWORD: &str = "master-pass-1";

/// An unlocked database and an approved project `app`.
fn ready() -> (Home, PathBuf) {
    let home = Home::new();
    let repo = home.make_repo("app");
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    (home, repo)
}

fn run(home: &Home, cwd: &Path, args: &[&str]) -> Output {
    home.hostler().args(args).current_dir(cwd).output().unwrap()
}

fn tty_file(home: &Home, lines: &[&str]) -> PathBuf {
    let path = home
        .dir
        .path()
        .join(format!("tty-{}", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    path
}

fn unlock_existing(home: &Home) {
    let out = home
        .hostler()
        .arg("unlock")
        .env("HOSTLER_TEST_TTY", tty_file(home, &[PASSWORD]))
        .output()
        .unwrap();
    assert!(out.status.success(), "unlock: {}", stderr(&out));
}

/// A running `hostler secret request`; its stderr lines arrive on a channel.
struct Waiter {
    child: Child,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Waiter {
    fn start(home: &Home, repo: &Path, args: &[&str]) -> Waiter {
        Waiter::start_with(home.hostler(), repo, args)
    }

    fn start_with(mut command: std::process::Command, repo: &Path, args: &[&str]) -> Waiter {
        let mut child = command
            .args(["secret", "request"])
            .args(args)
            .current_dir(repo)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = channel();
        let all = Arc::new(Mutex::new(String::new()));
        let (pipe, sink) = (child.stderr.take().unwrap(), all.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                let Ok(line) = line else { break };
                sink.lock().unwrap().push_str(&format!("{line}\n"));
                let _ = tx.send(line);
            }
        });
        Waiter {
            child,
            lines,
            stderr: all,
        }
    }

    /// Waits for a stderr line containing `needle`.
    fn line_with(&self, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(_) => panic!(
                    "no line with {needle:?} within {within:?}; stderr:\n{}",
                    self.stderr.lock().unwrap()
                ),
            }
        }
    }

    fn waiting(&self) -> String {
        self.line_with("Waiting for a person to provide", Duration::from_secs(20))
    }

    /// Exit status, stdout, all of stderr; killed and failed after `within`.
    fn finish(mut self, within: Duration) -> (ExitStatus, String, String) {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!(
                    "the waiter did not exit within {within:?}; stderr:\n{}",
                    self.stderr.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut out = String::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        // The reader thread ends at EOF; give it a moment to hand over the last line.
        std::thread::sleep(Duration::from_millis(100));
        let err = self.stderr.lock().unwrap().clone();
        (status, out, err)
    }

    /// Still waiting (not exited).
    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    #[cfg(unix)]
    fn interrupt(&self) {
        assert_eq!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGINT) },
            0
        );
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn pending(home: &Home) -> Vec<SecretRequestInfo> {
    serde_json::from_value(home.call(Request::SecretRequests).unwrap()).unwrap()
}

/// The id of the single pending request for `name`.
fn pending_id(home: &Home, name: &str) -> String {
    let ids: Vec<String> = pending(home)
        .into_iter()
        .filter(|r| r.name == name)
        .map(|r| r.id)
        .collect();
    assert_eq!(ids.len(), 1, "pending requests for {name}: {ids:?}");
    ids[0].clone()
}

fn envelope(out: &str) -> Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"))
}

#[test]
fn request_is_answered_by_set() {
    let (home, repo) = ready();
    let waiter = Waiter::start(&home, &repo, &["API_KEY", "--reason", "deploy the app"]);
    let line = waiter.waiting();
    assert!(
        line.contains("for project app (request ")
            && line.contains("A person runs: hostler secret set API_KEY"),
        "{line}"
    );
    home.set(&repo, "API_KEY", "sk-answer-value");
    let set_done = Instant::now();
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    let after_set = set_done.elapsed();
    assert_eq!(status.code(), Some(0), "{err}");
    assert_eq!(out, "stored\n");
    // Plan: within 2 s of the set (event-driven); tolerant under load.
    assert!(after_set < Duration::from_secs(4), "{after_set:?}");
    assert!(!out.contains("sk-answer") && !err.contains("sk-answer"));
}

#[test]
fn already_set_is_stored_at_once() {
    let (home, repo) = ready();
    home.set(&repo, "API_KEY", "sk-answer-value");
    let started = Instant::now();
    let out = run(
        &home,
        &repo,
        &["secret", "request", "API_KEY", "--reason", "x", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!stderr(&out).contains("Waiting"), "{}", stderr(&out));
    let env = envelope(&stdout(&out));
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["state"], "stored");
    assert_eq!(env["data"]["name"], "API_KEY");
}

#[test]
fn decline_by_short_id_and_by_name() {
    let (home, repo) = ready();
    let by_id = Waiter::start(&home, &repo, &["API_KEY", "--reason", "one"]);
    by_id.waiting();
    let id = pending_id(&home, "API_KEY");
    let out = run(&home, &repo, &["secret", "decline", &id[..8]]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("declined {} (API_KEY for project app)\n", &id[..8])
    );
    let (status, out, err) = by_id.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1), "{err}");
    assert_eq!(out, "declined\n");

    let by_name = Waiter::start(&home, &repo, &["TOKEN", "--reason", "two"]);
    by_name.waiting();
    let out = run(&home, &repo, &["secret", "decline", "TOKEN", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let env = envelope(&stdout(&out));
    assert_eq!(env["data"][0]["name"], "TOKEN");
    assert_eq!(env["data"][0]["state"], "declined");
    let (status, out, _) = by_name.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1));
    assert_eq!(out, "declined\n");

    // Nothing left to decline; garbage is refused without being echoed.
    let out = run(&home, &repo, &["secret", "decline", "TOKEN"]);
    assert_eq!(out.status.code(), Some(1));
    let out = run(&home, &repo, &["secret", "decline", "hunter2-canary"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!stderr(&out).contains("hunter2"), "{}", stderr(&out));
}

#[test]
fn max_wait_exits_75_and_withdraws() {
    let (home, repo) = ready();
    let out = run(
        &home,
        &repo,
        &[
            "secret",
            "request",
            "API_KEY",
            "--reason",
            "x",
            "--max-wait",
            "0.02",
        ],
    );
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "[hostler] Gave up after 1 s waiting for API_KEY; the request was withdrawn."
        ),
        "{}",
        stderr(&out)
    );
    assert!(pending(&home).is_empty());

    let out = run(
        &home,
        &repo,
        &[
            "secret",
            "request",
            "API_KEY",
            "--reason",
            "x",
            "--max-wait",
            "0.02",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(75));
    let env = envelope(&stdout(&out));
    assert_eq!(env["data"]["state"], "timed_out");
    // Status lines stay on stderr, the stop message too (T9-5).
    assert!(
        stderr(&out).contains("Waiting for a person"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains(
            "[hostler] Gave up after 1 s waiting for API_KEY; the request was withdrawn."
        ),
        "{}",
        stderr(&out)
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_withdraws() {
    let (home, repo) = ready();
    let waiter = Waiter::start(&home, &repo, &["API_KEY", "--reason", "x", "--json"]);
    waiter.waiting();
    waiter.interrupt();
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(130), "{err}");
    assert_eq!(envelope(&out)["data"]["state"], "withdrawn");
    assert!(
        err.contains("[hostler] Interrupted; the request was withdrawn."),
        "{err}"
    );
    assert!(pending(&home).is_empty());
}

#[test]
fn locked_request_is_refused_at_once() {
    let (home, repo) = ready();
    assert!(run(&home, &repo, &["lock"]).status.success());
    let started = Instant::now();
    let out = run(
        &home,
        &repo,
        &["secret", "request", "API_KEY", "--reason", "x"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(stderr(&out).contains("locked"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("Waiting"), "{}", stderr(&out));

    let other = home.make_repo("other");
    unlock_existing(&home);
    let out = run(
        &home,
        &other,
        &["secret", "request", "API_KEY", "--reason", "x"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not approved"), "{}", stderr(&out));
}

#[test]
fn request_survives_service_restart() {
    let (home, repo) = ready();
    let waiter = Waiter::start(&home, &repo, &["API_KEY", "--reason", "x"]);
    waiter.waiting();
    let out = run(&home, &repo, &["service", "restart"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let line = waiter.line_with("The database is locked", Duration::from_secs(30));
    assert!(
        line.contains("a person runs hostler unlock, then hostler secret set API_KEY"),
        "{line}"
    );
    unlock_existing(&home);
    home.set(&repo, "API_KEY", "sk-after-restart");
    let (status, out, err) = waiter.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(0), "{err}");
    assert_eq!(out, "stored\n");
}

#[test]
fn waiter_while_locked() {
    let (home, repo) = ready();
    let first = Waiter::start(&home, &repo, &["API_KEY", "--reason", "x"]);
    first.waiting();
    #[cfg(unix)]
    let second = Waiter::start(&home, &repo, &["TOKEN", "--reason", "y", "--json"]);
    #[cfg(unix)]
    second.waiting();
    assert!(run(&home, &repo, &["lock"]).status.success());
    first.line_with("The database is locked", Duration::from_secs(20));
    // While locked the request stays pending, and decline by id still works.
    let id = pending_id(&home, "API_KEY");
    let out = run(&home, &repo, &["secret", "decline", &id]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (status, out, err) = first.finish(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1), "{err}");
    assert_eq!(out, "declined\n");
    #[cfg(unix)]
    {
        second.line_with("The database is locked", Duration::from_secs(20));
        second.interrupt();
        let (status, out, err) = second.finish(Duration::from_secs(20));
        assert_eq!(status.code(), Some(130), "{err}");
        assert_eq!(envelope(&out)["data"]["state"], "withdrawn");
    }
}

#[test]
fn lost_event_is_recovered_by_polling() {
    let (home, repo) = ready();
    // Every pushed event is lost (the hook drops them), so only the 5 s poll can answer.
    let mut child = home.hostler();
    child.env("HOSTLER_TEST_REQUEST_IGNORE_EVENTS", "1");
    let waiter = Waiter::start_with(child, &repo, &["API_KEY", "--reason", "x"]);
    waiter.waiting();
    // A stream of queue events on every connection while the answer arrives.
    let stop = Arc::new(AtomicBool::new(false));
    let flood = {
        let (stop, paths) = (stop.clone(), home.paths());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut client = hostler::client::Client::connect(&paths).await.unwrap();
                let mut capacity = 1;
                while !stop.load(Ordering::SeqCst) {
                    capacity = 3 - capacity;
                    let _ = client.call(Request::QueueSetCapacity { capacity }).await;
                }
            });
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    home.set(&repo, "API_KEY", "sk-flooded-value");
    let set_done = Instant::now();
    let result = waiter.finish(Duration::from_secs(30));
    let after_set = set_done.elapsed();
    stop.store(true, Ordering::SeqCst);
    flood.join().unwrap();
    let (status, out, err) = result;
    assert_eq!(status.code(), Some(0), "{err}");
    assert_eq!(out, "stored\n");
    // Plan: within 6 s (one 5 s poll); tolerant under load.
    assert!(after_set < Duration::from_secs(8), "{after_set:?}");
}

#[test]
fn requests_lists_pending_with_sanitized_reason() {
    let (home, repo) = ready();
    let empty = run(&home, &repo, &["secret", "requests"]);
    assert_eq!(stdout(&empty), "no pending secret requests\n");
    let waiter = Waiter::start(
        &home,
        &repo,
        &["API_KEY", "--reason", "clear \u{1b}[2J screen"],
    );
    waiter.waiting();
    let out = run(&home, &repo, &["secret", "requests"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("  app  API_KEY  "), "{text}");
    assert!(text.contains("clear \u{FFFD}[2J screen"), "{text}");
    let id = pending_id(&home, "API_KEY");
    assert!(text.starts_with(&id[..8]), "{text}");
    let json = run(&home, &repo, &["secret", "requests", "--json"]);
    let env = envelope(&stdout(&json));
    assert_eq!(env["data"][0]["id"], id.as_str());
}

/// T9-1: a person stops the service while a request waits. The waiter reconnects for 10 s,
/// then gives up; it never starts a service of its own.
#[test]
fn stopped_service_ends_the_wait() {
    let (home, repo) = ready();
    let waiter = Waiter::start(&home, &repo, &["API_KEY", "--reason", "x"]);
    waiter.waiting();
    let out = run(&home, &repo, &["service", "stop"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stopped = Instant::now();
    let (status, out, err) = waiter.finish(Duration::from_secs(30));
    assert_eq!(status.code(), Some(1), "{err}");
    // Plan: a 10 s reconnect budget (~13 s); tolerant under load.
    assert!(
        stopped.elapsed() < Duration::from_secs(16),
        "{:?}",
        stopped.elapsed()
    );
    assert!(
        err.contains(
            "[hostler] The hostler service is unavailable; the request may still be pending \
             (hostler secret requests)."
        ),
        "{err}"
    );
    assert_eq!(out, "");
    let status = run(&home, &repo, &["service", "status"]);
    assert_eq!(status.status.code(), Some(3), "{}", stdout(&status));
}

/// T9-3: a re-check queued behind a long save is a slow answer on a live connection, not a lost
/// one: the waiter keeps waiting (each save pauses 20 s before it is placed).
#[test]
fn slow_recheck_behind_a_save_keeps_waiting() {
    let home = Home::new();
    let repo = home.make_repo("app");
    let out = home
        .hostler()
        .env("HOSTLER_TEST_VAULT_PAUSE", "before_place:20000")
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    // Creating the database places no save.
    home.unlock_new(PASSWORD);
    home.approve(&repo);
    let mut waiter = Waiter::start(&home, &repo, &["API_KEY", "--reason", "x"]);
    waiter.waiting();
    // Saving another secret holds the vault for 20 s; the 5 s poll's re-checks wait behind it.
    home.set(&repo, "OTHER", "other-value");
    assert!(
        waiter.running(),
        "the waiter gave up behind a slow save; stderr:\n{}",
        waiter.stderr.lock().unwrap()
    );
    let id = pending_id(&home, "API_KEY");
    assert!(
        run(&home, &repo, &["secret", "decline", &id])
            .status
            .success()
    );
    let (status, out, err) = waiter.finish(Duration::from_secs(30));
    assert_eq!(status.code(), Some(1), "{err}");
    assert_eq!(out, "declined\n");
    assert!(!err.contains("unavailable"), "{err}");
}

/// T9-4: when the withdraw cannot reach the service, the message does not claim it happened.
#[test]
fn failed_withdraw_is_not_called_withdrawn() {
    let (home, repo) = ready();
    let waiter = Waiter::start(
        &home,
        &repo,
        &["API_KEY", "--reason", "x", "--max-wait", "0.15", "--json"],
    );
    waiter.waiting();
    // The deadline (9 s) comes before the 10 s reconnect budget ends.
    let out = run(&home, &repo, &["service", "stop"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (status, out, err) = waiter.finish(Duration::from_secs(30));
    assert_eq!(status.code(), Some(75), "{err}");
    assert!(
        err.contains(
            "[hostler] Gave up after 9 s waiting for API_KEY; the request could not be \
             withdrawn; it ends once the service sees this process gone (hostler secret \
             requests)."
        ),
        "{err}"
    );
    assert!(!err.contains("the request was withdrawn"), "{err}");
    assert_eq!(envelope(&out)["ok"], false);
}
