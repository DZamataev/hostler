//! The person gate and reveal against the real binary (P5 Task 4, spec §8.5): a proof grants
//! its own connection for a while, a lock or unlock voids it, every gated method refuses
//! without it, and reveal answers travel the sensitive path and never reach the log.
//!
//! Passwords and values are throwaway literals; no assertion message prints a value.

mod common;
#[path = "common/raw.rs"]
mod raw;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Home, stderr};
use hostler::client::Client;
use hostler::contract::accounts::{AccountAnswer, AccountSet, MaterializedAccount};
use hostler::contract::heavy::{AcquireOutcome, HeavyAcquire, QueueSnapshot};
use hostler::contract::secrets::{
    ImportEntry, MaterializedSecret, ProjectRef, Prove, SecretSet, SecretsStatus, Unlock,
};
use hostler::contract::wire::Request;
use hostler::contract::{ErrorCode, HostlerError};
use hostler::procs::{SysProcessTable, current_identity};
use hostler::secrets::value::SecretValue;
use raw::files_text;
use serde_json::Value;

const PASSWORD: &str = "pass-1234";
const MARKER: &str = "reveal-marker-4d8e1b";
const FIELD_MARKER: &str = "field-marker-77ac09";

/// One connection of its own, held across calls (a grant belongs to its connection).
struct Conn {
    rt: tokio::runtime::Runtime,
    client: Client,
}

impl Conn {
    fn open(home: &Home) -> Conn {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let paths = home.paths();
        let client = rt.block_on(Client::connect(&paths)).unwrap();
        Conn { rt, client }
    }

    fn call(&mut self, req: Request) -> Result<Value, HostlerError> {
        self.rt.block_on(self.client.call(req))
    }

    fn prove(&mut self, password: &str) -> Result<Value, HostlerError> {
        self.call(Request::SecretsProve(Prove {
            password: SecretValue::new(password.into()),
            key_file: None,
        }))
    }

    fn unlock(&mut self, password: &str) -> Result<Value, HostlerError> {
        self.call(Request::SecretsUnlock(Unlock {
            password: SecretValue::new(password.into()),
            key_file: None,
            forget_key_file: false,
            create: false,
        }))
    }

    fn lock(&mut self) {
        self.call(Request::SecretsLock).unwrap();
    }

    /// `secret.reveal` through the client's sensitive path (the answer line is routed raw and
    /// decoded straight into a redacted value).
    fn reveal(&mut self, p: &ProjectRef, name: &str) -> Result<String, HostlerError> {
        let secret: MaterializedSecret =
            self.rt
                .block_on(self.client.call_sensitive(Request::SecretReveal {
                    project: p.clone(),
                    name: name.into(),
                }))?;
        assert_eq!(secret.name, name);
        Ok(secret.value.expose().to_owned())
    }
}

fn code<T>(r: Result<T, HostlerError>) -> ErrorCode {
    match r {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e.code,
    }
}

/// Starts the service with extra test environment.
fn start(home: &Home, env: &[(&str, &str)]) {
    let out = home
        .hostler()
        .envs(env.iter().copied())
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service start: {}", stderr(&out));
}

fn stop(home: &Home) {
    let out = home
        .hostler()
        .args(["service", "stop", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success(), "service stop: {}", stderr(&out));
}

/// A started service with a new database, an approved project `app` and its `API_KEY`.
fn ready(env: &[(&str, &str)]) -> (Home, ProjectRef) {
    let home = Home::new();
    start(&home, env);
    home.call(Request::SecretsUnlock(Unlock {
        password: SecretValue::new(PASSWORD.into()),
        key_file: None,
        forget_key_file: false,
        create: true,
    }))
    .unwrap();
    let repo = home.make_repo("app");
    let p = hostler::project::current(&repo).unwrap().project;
    home.call(Request::ProjectApprove(p.clone())).unwrap();
    home.call(Request::SecretSet(SecretSet {
        project: p.clone(),
        name: "API_KEY".into(),
        value: SecretValue::new(MARKER.into()),
    }))
    .unwrap();
    (home, p)
}

#[test]
fn reveal_without_proof_is_refused() {
    let (home, p) = ready(&[]);
    let mut conn = Conn::open(&home);
    let e = conn.reveal(&p, "API_KEY").err().unwrap();
    assert_eq!(e.code, ErrorCode::ProofRequired);
    assert_eq!(
        e.recovery.as_deref(),
        Some("type the master password in the hostler app, or rerun the command at a terminal")
    );
}

#[test]
fn proof_then_reveal() {
    let (home, p) = ready(&[]);
    let mut conn = Conn::open(&home);
    let before = hostler_now_ms();
    let grant = conn.prove(PASSWORD).unwrap();
    let expires = grant["expires_at_ms"].as_i64().unwrap();
    assert!(expires >= before + 120_000 && expires <= hostler_now_ms() + 120_000);
    assert!(conn.reveal(&p, "API_KEY").unwrap() == MARKER);
}

fn hostler_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn unlock_grants_its_connection() {
    let (home, p) = ready(&[]);
    let mut a = Conn::open(&home);
    a.lock();
    let answer = a.unlock(PASSWORD).unwrap();
    assert!(answer["grant"]["expires_at_ms"].as_i64().is_some());
    assert!(a.reveal(&p, "API_KEY").unwrap() == MARKER);
    let mut b = Conn::open(&home);
    assert_eq!(code(b.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
}

#[test]
fn proof_is_per_connection() {
    let (home, p) = ready(&[]);
    let mut a = Conn::open(&home);
    let mut b = Conn::open(&home);
    a.prove(PASSWORD).unwrap();
    assert_eq!(code(b.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
    assert!(a.reveal(&p, "API_KEY").unwrap() == MARKER);
}

#[test]
fn grant_ends_with_its_connection() {
    let (home, p) = ready(&[]);
    let mut a = Conn::open(&home);
    a.prove(PASSWORD).unwrap();
    assert!(a.reveal(&p, "API_KEY").unwrap() == MARKER);
    drop(a);
    let mut again = Conn::open(&home);
    assert_eq!(code(again.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
}

#[test]
fn proof_expires() {
    let (home, p) = ready(&[("HOSTLER_TEST_PROVE_MS", "1500")]);
    let mut conn = Conn::open(&home);
    let proved = Instant::now();
    conn.prove(PASSWORD).unwrap();
    std::thread::sleep(Duration::from_millis(3_500).saturating_sub(proved.elapsed()));
    assert_eq!(code(conn.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
}

#[test]
fn lock_voids_the_proof() {
    let (home, p) = ready(&[]);
    let mut a = Conn::open(&home);
    a.prove(PASSWORD).unwrap();
    let mut b = Conn::open(&home);
    b.lock();
    b.unlock(PASSWORD).unwrap();
    assert_eq!(code(a.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
    // B's own unlock granted B.
    assert!(b.reveal(&p, "API_KEY").unwrap() == MARKER);
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "the pause point was never reached"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The reveal is held between the gate's pre-check and its admission under the vault mutex
/// (`HOSTLER_TEST_PAUSE_AT=reveal_admit:<file>`); a lock and an unlock run meanwhile. The
/// reveal answers `proof_required` and no value is sent.
#[test]
fn lock_between_grant_and_reveal() {
    let dir = tempfile::tempdir().unwrap();
    let pause = dir.path().join("pause");
    std::fs::write(&pause, b"").unwrap();
    let spec = format!("reveal_admit:{}", pause.display());
    let (home, p) = ready(&[("HOSTLER_TEST_PAUSE_AT", &spec)]);
    let mut a = Conn::open(&home);
    a.prove(PASSWORD).unwrap();
    let revealing = {
        let p = p.clone();
        std::thread::spawn(move || {
            let result = a.reveal(&p, "API_KEY");
            result.map(|value| value == MARKER)
        })
    };
    wait_for_file(&PathBuf::from(format!("{}.reached", pause.display())));
    let mut b = Conn::open(&home);
    b.lock();
    b.unlock(PASSWORD).unwrap();
    std::fs::remove_file(&pause).unwrap();
    let result = revealing.join().unwrap();
    assert_eq!(code(result), ErrorCode::ProofRequired);
}

fn heavy_request() -> HeavyAcquire {
    HeavyAcquire {
        id: uuid::Uuid::new_v4().to_string(),
        kind: "gradle".into(),
        command: "./gradlew assemble".into(),
        cwd: "/work/app".into(),
        workspace_root: None,
        holder: current_identity(&SysProcessTable::new().unwrap()).unwrap(),
        reentrant_token: None,
        no_wait: false,
    }
}

/// Each of the six gated methods (spec §8.5) without a grant answers `proof_required` and
/// changes nothing; with a grant each gets past the gate.
#[test]
fn every_gated_method_refuses_without_grant() {
    let (home, p) = ready(&[]);
    let prev = home
        .paths()
        .kdbx
        .with_file_name(format!("hostler.kdbx.prev-{}", uuid::Uuid::new_v4()));
    std::fs::write(&prev, b"kept").unwrap();
    let mut conn = Conn::open(&home);
    // A vault call refreshes the status's list of kept files.
    conn.call(Request::SecretSet(SecretSet {
        project: p.clone(),
        name: "OTHER_KEY".into(),
        value: SecretValue::new("other-value-1".into()),
    }))
    .unwrap();
    let acquired: AcquireOutcome =
        serde_json::from_value(conn.call(Request::HeavyAcquire(heavy_request())).unwrap()).unwrap();
    let AcquireOutcome::Granted { entry } = acquired else {
        panic!("the heavy task was not granted");
    };
    let status: SecretsStatus =
        serde_json::from_value(conn.call(Request::SecretsStatus).unwrap()).unwrap();
    assert_eq!(status.preserved_files.len(), 1);
    let gated = |p: &ProjectRef| {
        vec![
            Request::SecretsRemovePreserved {
                path: status.preserved_files[0].clone(),
            },
            Request::AccountAnswer(AccountAnswer {
                request_id: uuid::Uuid::new_v4().to_string(),
                fields: vec![ImportEntry {
                    name: "session".into(),
                    value: SecretValue::new(FIELD_MARKER.into()),
                }],
            }),
            Request::AccountDecline {
                id: uuid::Uuid::new_v4().to_string(),
            },
            Request::HeavyStop {
                id: entry.id.clone(),
                confirmation: "confirm".into(),
            },
            Request::AccountReveal {
                project: p.clone(),
                name: "qa-user".into(),
            },
        ]
    };
    assert_eq!(code(conn.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
    for req in gated(&p) {
        let method = hostler::contract::methods::method_name(&req);
        let e = match req {
            Request::AccountReveal { .. } => conn
                .rt
                .block_on(conn.client.call_sensitive::<MaterializedAccount>(req))
                .err()
                .unwrap(),
            req => conn.call(req).err().unwrap(),
        };
        assert_eq!(e.code, ErrorCode::ProofRequired, "{method}");
    }
    assert!(prev.exists(), "the preserved file is still there");
    let queue: QueueSnapshot =
        serde_json::from_value(conn.call(Request::QueueSnapshot).unwrap()).unwrap();
    assert_eq!(queue.running.len(), 1, "the heavy task still runs");
    assert_eq!(queue.running[0].id, entry.id);

    conn.prove(PASSWORD).unwrap();
    for req in gated(&p) {
        let method = hostler::contract::methods::method_name(&req);
        let result = match req {
            Request::AccountReveal { .. } => conn
                .rt
                .block_on(conn.client.call_sensitive::<MaterializedAccount>(req))
                .map(drop),
            req => conn.call(req).map(drop),
        };
        if let Err(e) = result {
            assert_ne!(e.code, ErrorCode::ProofRequired, "{method}");
        }
    }
    assert!(!prev.exists(), "removed once proven");
}

#[test]
fn prove_on_missing_database_refused() {
    let home = Home::new();
    start(&home, &[]);
    let mut conn = Conn::open(&home);
    let e = conn.prove(PASSWORD).err().unwrap();
    assert_eq!(e.code, ErrorCode::SecretsLocked);
    assert!(
        e.message.contains("no secrets database yet"),
        "{}",
        e.message
    );
}

#[test]
fn wrong_proof_keeps_the_database_unlocked() {
    let (home, p) = ready(&[]);
    let mut conn = Conn::open(&home);
    let e = conn.prove("wrong-pass").err().unwrap();
    assert_eq!(e.code, ErrorCode::UnlockFailed);
    assert_eq!(e.message, "wrong master password or key file");
    let status: SecretsStatus =
        serde_json::from_value(conn.call(Request::SecretsStatus).unwrap()).unwrap();
    assert_eq!(
        status.state,
        hostler::contract::secrets::LockState::Unlocked
    );
    assert_eq!(code(conn.reveal(&p, "API_KEY")), ErrorCode::ProofRequired);
}

/// The service log names each reveal (the secret, the account, the project, the peer pid)
/// and never a value.
#[test]
fn reveal_never_in_logs() {
    let (home, p) = ready(&[]);
    let mut conn = Conn::open(&home);
    conn.call(Request::AccountSet(AccountSet {
        project: p.clone(),
        name: "qa-user".into(),
        field: "session".into(),
        value: SecretValue::new(FIELD_MARKER.into()),
    }))
    .unwrap();
    conn.prove(PASSWORD).unwrap();
    assert!(conn.reveal(&p, "API_KEY").unwrap() == MARKER);
    let account: MaterializedAccount = conn
        .rt
        .block_on(conn.client.call_sensitive(Request::AccountReveal {
            project: p.clone(),
            name: "qa-user".into(),
        }))
        .unwrap();
    assert_eq!(account.fields.len(), 1);
    assert!(account.fields[0].value.expose() == FIELD_MARKER);
    drop(conn);
    stop(&home);
    let log = files_text(&home.paths().log_dir);
    let pid = if cfg!(any(target_os = "macos", target_os = "linux")) {
        std::process::id().to_string()
    } else {
        "unknown".to_owned()
    };
    assert!(
        log.lines().any(|l| l.contains(&format!(
            "revealed secret API_KEY of project app (peer pid {pid})"
        ))),
        "the reveal is logged by name"
    );
    assert!(
        log.lines().any(|l| l.contains(&format!(
            "revealed account qa-user of project app (peer pid {pid})"
        ))),
        "the account reveal is logged by name"
    );
    let everything = files_text(home.dir.path());
    assert!(
        !log.contains(MARKER) && !log.contains(FIELD_MARKER),
        "a value reached the log"
    );
    // Nothing under the home holds a value in clear (the database is encrypted).
    assert!(!everything.contains(MARKER) && !everything.contains(FIELD_MARKER));
}

/// A `secrets.prove` frame that does not parse gets the fixed text: serde's message would
/// quote the password.
#[test]
fn parse_error_text_is_fixed_for_prove() {
    let home = Home::new();
    start(&home, &[]);
    let runtime = raw::runtime();
    runtime.block_on(async {
        let mut conn = raw::Raw::connect(&home).await;
        for (id, params) in [
            (5, format!(r#"{{"password":"{MARKER}","key_file":7}}"#)),
            (6, format!(r#"{{"password":["{MARKER}"]}}"#)),
            (7, format!(r#"{{"pass":"{MARKER}"}}"#)),
        ] {
            let answer = conn.call(id, "secrets.prove", Some(&params)).await;
            assert_eq!(answer["error"]["code"], "bad_request");
            assert_eq!(answer["error"]["message"], "unreadable request");
            assert!(!answer.to_string().contains(MARKER));
        }
    });
}

/// No CLI command reveals a value (spec §8.2, §9.6): `src/cli/**` names neither reveal.
#[test]
fn cli_never_reveals() {
    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let cli = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("cli");
    let mut files = Vec::new();
    sources(&cli, &mut files);
    assert!(files.len() > 10, "the CLI sources were found");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for name in [
            "SecretReveal",
            "AccountReveal",
            "secret.reveal",
            "account.reveal",
        ] {
            assert!(!text.contains(name), "{} names {name}", file.display());
        }
    }
}
