//! The secrets engine in the service (plan P2, Task 6): projects, approvals, requests, shares
//! and run materialization over the vault (KDBX) and the store (SQLite).
//!
//! Locks: the publish mutex, then the vault mutex, then the store mutex, never the other way, and
//! the store guard is never held while the vault works. Lock state and the kept prev files are cached outside the
//! vault mutex, so `secrets.status` and `secret.requests` answer while a save holds the vault.
//! Methods that need an unlocked vault check the cached flag first and answer `SecretsLocked`
//! at once. A poisoned vault mutex counts as locked, a poisoned store mutex as `Internal`.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use keepass::Database;
use keepass::db::GroupId;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use zeroize::Zeroizing;

use super::layout::{self, Lookup};
use super::value::SecretValue;
use super::vault::{KdfChoice, Vault, VaultError, VaultKey};
use super::{check_value, valid_secret_name};
use crate::contract::secrets::{
    DeclaredSecret, Decline, ImportOutcome, LockState, Materialize, MaterializedSecret,
    ProjectInfo, ProjectRef, RequestState, SecretImport, SecretInfo, SecretList, SecretRequest,
    SecretRequestInfo, SecretSet, SecretSource, SecretState, SecretsStatus, SetOutcome, Share,
    Unlock, errors,
};
use crate::contract::wire::{Event, Request};
use crate::contract::{ErrorCode, HostlerError};
use crate::heavy::engine::Clock;
use crate::mask::MaskSet;
use crate::procs::{self, Liveness, ProcessTable};
use crate::project::display_path;
use crate::store::Store;

/// The store setting that remembers the key file's path (D1).
const KEY_FILE_SETTING: &str = "secrets.key_file";
const MAX_PENDING_REQUESTS: usize = 256;
const MAX_REASON_BYTES: usize = 500;
const MAX_RUN_NAMES: usize = 64;
const MAX_IMPORT_ENTRIES: usize = 1000;
/// The raw display a run may send (L13); a longer one comes as `<command of N bytes>`.
const MAX_COMMAND_BYTES: usize = 16 * 1024;
/// The recorded command, after masking (L6, L13).
const HISTORY_COMMAND_BYTES: usize = 1024;
/// The encoded materialize answer (it travels in one frame).
const MAX_ANSWER_BYTES: usize = 4 << 20;
const PRUNE_EVERY_MS: i64 = 60 * 60 * 1000;
const HISTORY_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// A dispatch result: an ordinary JSON result, or the encoded result of `secret.materialize`,
/// which never becomes a `Value`.
pub enum Reply {
    Value(Value),
    Sensitive(Zeroizing<String>),
}

pub struct SecretsEngine {
    /// Serializes choosing a project's group, writing to it, publishing its UUID and answering
    /// pending requests — against other writes (T6-1) and a request's check-and-insert (T6-3).
    publish: Mutex<()>,
    vault: Mutex<Vault>,
    kdbx: PathBuf,
    store: Mutex<Store>,
    /// `vault.state() == Unlocked`, refreshed after every vault call.
    unlocked: AtomicBool,
    /// `vault.preserved_files()`, refreshed after every vault call.
    preserved: Mutex<Vec<String>>,
    closing: AtomicBool,
    last_prune_ms: AtomicI64,
    table: Arc<dyn ProcessTable>,
    clock: Arc<dyn Clock>,
    events: broadcast::Sender<Event>,
    kdf: KdfChoice,
    /// Unit tests pause at named points to force interleavings.
    #[cfg(test)]
    hook: Mutex<Option<TestHook>>,
}

#[cfg(test)]
type TestHook = Arc<dyn Fn(&'static str) + Send + Sync>;

fn internal(message: impl Display) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message: message.to_string(),
        details: json!({}),
        recovery: None,
    }
}

/// Store errors name tables and constraints, never bound values.
fn store_error(e: anyhow::Error) -> HostlerError {
    internal(format!("the hostler store failed: {e:#}"))
}

fn bad_request(message: impl Into<String>) -> HostlerError {
    errors::bad_request(message.into())
}

fn check_name(name: &str) -> Result<(), HostlerError> {
    if valid_secret_name(name) {
        Ok(())
    } else {
        // Never echoed: an agent may have put a value where the name goes.
        Err(bad_request(
            "invalid secret name (NAME must match ^[A-Z][A-Z0-9_]{0,63}$)",
        ))
    }
}

fn duplicate_entries(name: &str) -> HostlerError {
    let mut e = errors::secrets_conflict("duplicate_entries");
    e.message = format!(
        "hostler.kdbx holds two entries named {name} in one project group; a person keeps one \
         of them in KeePassXC"
    );
    e.details = json!({ "name": name, "reason": "duplicate_entries" });
    e.recovery = None;
    e
}

/// L19: never the value, nor anything about it but that it breaks the rules.
fn invalid_value(name: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::SecretsConflict,
        message: format!(
            "the value of {name} in hostler.kdbx breaks hostler's value rules (4 bytes to 64 KiB, \
             no control characters but tab and line breaks); it was not used"
        ),
        details: json!({ "name": name, "reason": "invalid_value" }),
        recovery: Some(format!("hostler secret set {name}")),
    }
}

/// Appends into a buffer allocated once and refuses to grow it: a reallocation would leave an
/// unerased copy of secret values behind (T6-4).
struct Bounded<'a> {
    buf: &'a mut Vec<u8>,
    limit: usize,
}

impl<'a> Bounded<'a> {
    fn new(buf: &'a mut Vec<u8>) -> Bounded<'a> {
        let limit = buf.capacity();
        Bounded { buf, limit }
    }
}

impl std::io::Write for Bounded<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if data.len() > self.limit - self.buf.len() {
            return Err(std::io::Error::other("the answer is over its bound"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `values` as JSON in a zeroizing buffer of at most `MAX_ANSWER_BYTES`, never reallocated.
fn encode_answer(values: &[MaterializedSecret]) -> Result<Zeroizing<Vec<u8>>, HostlerError> {
    // Escaping makes at most 6 bytes of one; names need none.
    let worst: usize = values
        .iter()
        .map(|v| v.value.len() * 6 + v.name.len() + 32)
        .sum::<usize>()
        + 16;
    let mut encoded = Zeroizing::new(Vec::with_capacity(worst.min(MAX_ANSWER_BYTES)));
    let mut writer = Bounded::new(&mut encoded);
    writer.limit = writer.limit.min(MAX_ANSWER_BYTES);
    serde_json::to_writer(writer, values)
        .map_err(|_| bad_request("the requested secrets are too large for one run"))?;
    Ok(encoded)
}

fn group_of(project: &ProjectInfo) -> Option<GroupId> {
    let uuid = uuid::Uuid::parse_str(project.group_uuid.as_deref()?).ok()?;
    Some(GroupId::from_uuid(uuid))
}

fn value_of<T: Serialize>(result: Result<T, HostlerError>) -> Result<Reply, HostlerError> {
    let value = result?;
    serde_json::to_value(value)
        .map(Reply::Value)
        .map_err(|_| internal("cannot encode the reply"))
}

/// The head of `s` within `max` bytes at a char boundary, `…` marking a cut.
fn keep_head(s: &str, max: usize) -> String {
    const CUT: &str = "…";
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max - CUT.len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CUT}", &s[..end])
}

/// Where a run's secret comes from.
enum Found {
    Value(SecretValue),
    Duplicate,
    /// Set in another project's own group.
    Elsewhere(String),
    Missing,
}

impl SecretsEngine {
    pub fn new(
        vault: Vault,
        kdbx: PathBuf,
        store: Store,
        table: Arc<dyn ProcessTable>,
        clock: Arc<dyn Clock>,
        events: broadcast::Sender<Event>,
        kdf: KdfChoice,
    ) -> SecretsEngine {
        let unlocked = vault.state() == LockState::Unlocked;
        let preserved = vault.preserved_files();
        SecretsEngine {
            publish: Mutex::new(()),
            vault: Mutex::new(vault),
            kdbx,
            store: Mutex::new(store),
            unlocked: AtomicBool::new(unlocked),
            preserved: Mutex::new(preserved),
            closing: AtomicBool::new(false),
            last_prune_ms: AtomicI64::new(i64::MIN),
            table,
            clock,
            events,
            kdf,
            #[cfg(test)]
            hook: Mutex::new(None),
        }
    }

    /// Every `secrets.*`, `project.*` and `secret.*` method.
    pub fn handles(req: &Request) -> bool {
        matches!(
            req,
            Request::SecretsStatus
                | Request::SecretsUnlock(_)
                | Request::SecretsLock
                | Request::ProjectResolve(_)
                | Request::ProjectList
                | Request::ProjectApprove(_)
                | Request::SecretList { .. }
                | Request::SecretSet(_)
                | Request::SecretRemove { .. }
                | Request::SecretImport(_)
                | Request::SecretRequest(_)
                | Request::SecretRequestWithdraw { .. }
                | Request::SecretRequests
                | Request::SecretDecline(_)
                | Request::SecretShare(_)
                | Request::SecretUnshare(_)
                | Request::SecretMaterialize(_)
        )
    }

    /// Blocking: the KDF runs here, so callers use the blocking pool.
    pub fn dispatch(&self, req: Request) -> Result<Reply, HostlerError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(errors::service_stopping());
        }
        match req {
            Request::SecretsStatus => value_of(self.status()),
            Request::SecretsUnlock(u) => value_of(self.unlock(u)),
            Request::SecretsLock => value_of(self.lock()),
            Request::ProjectResolve(p) => value_of(self.resolve(&p)),
            Request::ProjectList => value_of(self.store()?.projects().map_err(store_error)),
            Request::ProjectApprove(p) => value_of(self.approve(&p)),
            Request::SecretList { project, declared } => value_of(self.list(&project, &declared)),
            Request::SecretSet(s) => value_of(self.set(s)),
            Request::SecretRemove { project, name } => value_of(self.remove(&project, &name)),
            Request::SecretImport(i) => value_of(self.import(i)),
            Request::SecretRequest(r) => value_of(self.request(r)),
            Request::SecretRequestWithdraw { id, timed_out } => {
                value_of(self.withdraw(&id, timed_out))
            }
            Request::SecretRequests => value_of(self.requests()),
            Request::SecretDecline(d) => value_of(self.decline(d)),
            Request::SecretShare(s) => value_of(self.share(&s)),
            Request::SecretUnshare(s) => value_of(self.unshare(&s)),
            Request::SecretMaterialize(m) => self.materialize(m).map(Reply::Sensitive),
            _ => Err(bad_request("not a secrets method")),
        }
    }

    /// Every 2 s: pending requests whose requester is gone are withdrawn; hourly, answered
    /// requests and materializations older than 30 days are pruned.
    pub fn tick(&self) {
        let now = self.now();
        let Ok(store) = self.store() else { return };
        let pending = match store.pending_requests() {
            Ok(pending) => pending,
            Err(e) => {
                tracing::warn!("cannot read pending secret requests: {e:#}");
                return;
            }
        };
        let mut changed = Vec::new();
        for (info, requester) in pending {
            if matches!(procs::liveness(&requester, &*self.table), Liveness::Alive) {
                continue;
            }
            if store
                .finish_request(&info.id, RequestState::Withdrawn, now)
                .unwrap_or(false)
                && let Ok(Some(info)) = store.request(&info.id)
            {
                changed.push(info);
            }
        }
        if now.saturating_sub(self.last_prune_ms.load(Ordering::SeqCst)) >= PRUNE_EVERY_MS {
            self.last_prune_ms.store(now, Ordering::SeqCst);
            if let Err(e) = store.prune_secret_history(now - HISTORY_RETENTION_MS) {
                tracing::warn!("cannot prune the secrets history: {e:#}");
            }
        }
        drop(store);
        for info in changed {
            self.emit(Event::SecretRequestChanged(info));
        }
    }

    /// Later dispatches answer `service_stopping()`.
    pub fn begin_shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Drops the database and the key (erased) once the vault jobs are drained.
    pub fn lock_for_shutdown(&self) {
        if let Ok(mut vault) = self.vault.lock() {
            vault.lock();
        }
        self.unlocked.store(false, Ordering::SeqCst);
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    #[cfg(test)]
    fn at(&self, point: &'static str) {
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }

    #[cfg(not(test))]
    fn at(&self, _point: &'static str) {}

    fn publishing(&self) -> MutexGuard<'_, ()> {
        self.publish.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The project as recorded now (its group may have been published since it was resolved).
    fn current(&self, project: &ProjectInfo) -> Result<ProjectInfo, HostlerError> {
        self.store()?
            .project(&project.id)
            .map_err(store_error)?
            .ok_or_else(|| internal("the project is no longer recorded"))
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn store(&self) -> Result<MutexGuard<'_, Store>, HostlerError> {
        self.store
            .lock()
            .map_err(|_| internal("the hostler store is unavailable"))
    }

    /// Runs `f` under the vault mutex, then refreshes the cached lock state and prev files (a
    /// refresh may have locked the vault) and announces a lock change — still under the vault
    /// mutex, so the cache follows the order of vault operations (T6-2).
    fn with_vault<T>(
        &self,
        f: impl FnOnce(&mut Vault) -> Result<T, VaultError>,
    ) -> Result<T, VaultError> {
        let Ok(mut vault) = self.vault.lock() else {
            self.set_unlocked(false);
            return Err(VaultError::Locked);
        };
        let result = f(&mut vault);
        let unlocked = vault.state() == LockState::Unlocked;
        let preserved = vault.preserved_files();
        self.at("publish");
        if let Ok(mut cached) = self.preserved.lock() {
            *cached = preserved;
        }
        self.set_unlocked(unlocked);
        drop(vault);
        result
    }

    fn set_unlocked(&self, unlocked: bool) {
        if self.unlocked.swap(unlocked, Ordering::SeqCst) != unlocked
            && let Ok(status) = self.status()
        {
            self.emit(Event::SecretsLockChanged(status));
        }
    }

    fn require_unlocked(&self) -> Result<(), HostlerError> {
        if self.unlocked.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(errors::secrets_locked(!self.kdbx.exists()))
        }
    }

    fn status(&self) -> Result<SecretsStatus, HostlerError> {
        let state = if self.unlocked.load(Ordering::SeqCst) {
            LockState::Unlocked
        } else if self.kdbx.exists() {
            LockState::Locked
        } else {
            LockState::Missing
        };
        let dir = self.kdbx.parent().unwrap_or(Path::new("."));
        let preserved_files = self
            .preserved
            .lock()
            .map(|names| names.iter().map(|n| display_path(&dir.join(n))).collect())
            .unwrap_or_default();
        let store = self.store()?;
        Ok(SecretsStatus {
            state,
            database: display_path(&self.kdbx),
            key_file: store.setting(KEY_FILE_SETTING).map_err(store_error)?,
            pending_requests: store.pending_requests().map_err(store_error)?.len() as u32,
            preserved_files,
        })
    }

    fn unlock(&self, u: Unlock) -> Result<SecretsStatus, HostlerError> {
        let remembered = self
            .store()?
            .setting(KEY_FILE_SETTING)
            .map_err(store_error)?;
        let key_file = match (&u.key_file, u.forget_key_file) {
            (Some(path), _) => Some(path.clone()),
            (None, true) => None,
            (None, false) => remembered,
        };
        let key = VaultKey::new(&u.password, key_file.as_deref().map(Path::new))
            .map_err(|e| e.to_hostler())?;
        let result = self.with_vault(|v| {
            if u.create {
                v.create(key, self.kdf)
            } else {
                v.unlock(key)
            }
        });
        // A NotDurable create left a database with this key at the path.
        if matches!(result, Ok(()) | Err(VaultError::NotDurable)) {
            let store = self.store()?;
            if u.key_file.is_some() {
                store
                    .set_setting(KEY_FILE_SETTING, key_file.as_deref())
                    .map_err(store_error)?;
            } else if u.forget_key_file {
                store
                    .set_setting(KEY_FILE_SETTING, None)
                    .map_err(store_error)?;
            }
        }
        match result {
            Ok(()) => self.status(),
            Err(VaultError::Exists) => Err(bad_request(format!(
                "a database already exists at {}",
                display_path(&self.kdbx)
            ))),
            Err(e) => Err(e.to_hostler()),
        }
    }

    fn lock(&self) -> Result<SecretsStatus, HostlerError> {
        let _ = self.with_vault(|v| {
            v.lock();
            Ok(())
        });
        self.status()
    }

    /// The project of a checkout's common dir, recorded on first sight.
    fn resolve(&self, p: &ProjectRef) -> Result<ProjectInfo, HostlerError> {
        let chars = p.name.chars().count();
        if !(1..=64).contains(&chars) || p.name.chars().any(char::is_control) {
            return Err(bad_request(
                "invalid project name (1 to 64 characters, no control characters)",
            ));
        }
        let dir = std::fs::canonicalize(&p.common_dir)
            .ok()
            .filter(|dir| dir.join("HEAD").is_file())
            .ok_or_else(|| {
                bad_request(format!(
                    "{} is not a git directory (it has no HEAD)",
                    super::sanitize_for_terminal(&p.common_dir)
                ))
            })?;
        self.store()?
            .upsert_project(&display_path(&dir), &p.name, self.now())
            .map_err(store_error)
    }

    fn approved(&self, p: &ProjectRef) -> Result<ProjectInfo, HostlerError> {
        let project = self.resolve(p)?;
        if project.approved {
            Ok(project)
        } else {
            Err(errors::project_not_approved(
                &project.name,
                &project.common_dir,
            ))
        }
    }

    fn approve(&self, p: &ProjectRef) -> Result<ProjectInfo, HostlerError> {
        let project = self.resolve(p)?;
        if project.approved {
            return Ok(project);
        }
        let store = self.store()?;
        store
            .approve_project(&project.id, self.now())
            .map_err(store_error)?;
        store
            .project(&project.id)
            .map_err(store_error)?
            .ok_or_else(|| internal("the approved project is missing"))
    }

    /// `(source project, name)` of every share into `project`.
    fn shares_into(
        &self,
        project: &ProjectInfo,
    ) -> Result<Vec<(ProjectInfo, String)>, HostlerError> {
        let store = self.store()?;
        let mut shares = Vec::new();
        for (source, name) in store.shares_into(&project.id).map_err(store_error)? {
            if let Some(source) = store.project(&source).map_err(store_error)? {
                shares.push((source, name));
            }
        }
        Ok(shares)
    }

    fn list(
        &self,
        project: &ProjectRef,
        declared: &[DeclaredSecret],
    ) -> Result<SecretList, HostlerError> {
        for d in declared {
            check_name(&d.name)?;
        }
        let project = self.approved(project)?;
        self.require_unlocked()?;
        let shares = self.shares_into(&project)?;
        let own = group_of(&project);
        let secrets = self
            .with_vault(|v| {
                v.read(|db| {
                    let mut names: BTreeSet<String> =
                        declared.iter().map(|d| d.name.clone()).collect();
                    if let Some(g) = own {
                        names.extend(layout::names(db, g));
                    }
                    names.extend(shares.iter().map(|(_, name)| name.clone()));
                    names
                        .into_iter()
                        .map(|name| {
                            let decl = declared.iter().find(|d| d.name == name);
                            let set = |g: Option<GroupId>| {
                                g.is_some_and(|g| {
                                    !matches!(layout::lookup(db, g, &name), Lookup::Missing)
                                })
                            };
                            let (state, source) = if set(own) {
                                (SecretState::Set, Some(SecretSource::Own))
                            } else if let Some((from, _)) = shares.iter().find(|(_, n)| *n == name)
                            {
                                let state = if set(group_of(from)) {
                                    SecretState::Set
                                } else {
                                    SecretState::Missing
                                };
                                let source = SecretSource::SharedFrom {
                                    project_id: from.id.clone(),
                                    project: from.name.clone(),
                                };
                                (state, Some(source))
                            } else {
                                (SecretState::Missing, None)
                            };
                            SecretInfo {
                                purpose: decl.map(|d| d.purpose.clone()),
                                declared: decl.is_some(),
                                state,
                                source,
                                name,
                            }
                        })
                        .collect()
                })
            })
            .map_err(|e| e.to_hostler())?;
        Ok(SecretList { project, secrets })
    }

    fn set(&self, s: SecretSet) -> Result<SetOutcome, HostlerError> {
        check_name(&s.name)?;
        check_value(&s.value)?;
        let project = self.approved(&s.project)?;
        self.require_unlocked()?;
        let answered = self.store_values(&project, &[(s.name.clone(), s.value)], true)?;
        Ok(SetOutcome {
            name: s.name,
            answered_requests: answered,
        })
    }

    fn import(&self, i: SecretImport) -> Result<ImportOutcome, HostlerError> {
        if i.entries.len() > MAX_IMPORT_ENTRIES {
            return Err(bad_request(format!(
                "an import holds at most {MAX_IMPORT_ENTRIES} entries"
            )));
        }
        let mut seen = BTreeSet::new();
        for entry in &i.entries {
            check_name(&entry.name)?;
            check_value(&entry.value)?;
            if !seen.insert(entry.name.as_str()) {
                return Err(bad_request(format!(
                    "{} appears twice in the import",
                    entry.name
                )));
            }
        }
        let project = self.approved(&i.project)?;
        self.require_unlocked()?;
        let entries: Vec<(String, SecretValue)> =
            i.entries.into_iter().map(|e| (e.name, e.value)).collect();
        let answered = self.store_values(&project, &entries, i.overwrite)?;
        Ok(ImportOutcome {
            stored: entries.into_iter().map(|(name, _)| name).collect(),
            answered_requests: answered,
        })
    }

    /// One vault write for all entries; every precondition is checked inside the write's
    /// closure, on the database as it is on disk (it may run twice). The group id, the answered
    /// requests and their events are recorded only after the write.
    fn store_values(
        &self,
        project: &ProjectInfo,
        entries: &[(String, SecretValue)],
        overwrite: bool,
    ) -> Result<Vec<String>, HostlerError> {
        let _publishing = self.publishing();
        let project = &self.current(project)?;
        let before = group_of(project);
        let used: Mutex<Option<GroupId>> = Mutex::new(None);
        let result = self.with_vault(|v| {
            v.write(|db: &mut Database| {
                let mut existing = Vec::new();
                for (name, _) in entries {
                    match before.map(|g| layout::lookup(db, g, name)) {
                        Some(Lookup::Duplicate) => return Err(duplicate_entries(name)),
                        Some(Lookup::Value(_)) if !overwrite => existing.push(name.clone()),
                        _ => {}
                    }
                }
                if !existing.is_empty() {
                    return Err(errors::secret_exists(&existing));
                }
                let group = layout::ensure_group(db, before, &project.name);
                for (name, value) in entries {
                    layout::put(db, group, name, value);
                }
                if let Ok(mut used) = used.lock() {
                    *used = Some(group);
                }
                Ok(group)
            })
        });
        // NotDurable: `K` may hold the change, so the group it used is recorded too.
        let group = match &result {
            Ok(group) => Some(*group),
            Err(VaultError::NotDurable) => used.lock().ok().and_then(|g| *g),
            Err(e) => return Err(e.to_hostler()),
        };
        let store = self.store()?;
        if let Some(group) = group
            && Some(group) != before
        {
            store
                .set_project_group(&project.id, Some(&group.uuid().to_string()))
                .map_err(store_error)?;
        }
        if let Err(e) = result {
            return Err(e.to_hostler());
        }
        let now = self.now();
        let mut answered = Vec::new();
        let mut changed = Vec::new();
        for (name, _) in entries {
            for id in store
                .answer_pending(&project.id, name, RequestState::Stored, now)
                .map_err(store_error)?
            {
                if let Some(info) = store.request(&id).map_err(store_error)? {
                    changed.push(info);
                }
                answered.push(id);
            }
        }
        drop(store);
        for info in changed {
            self.emit(Event::SecretRequestChanged(info));
        }
        // Projects that have these names shared from this one wait for them too (T6-7).
        for (name, _) in entries {
            for target in self.share_targets(&project.id, name)? {
                match self.answer_if_set(&target, name) {
                    Ok(infos) => answered.extend(infos.into_iter().map(|i| i.id)),
                    Err(e) => tracing::warn!(
                        "cannot answer shared-in requests in project {}: {}",
                        target.id,
                        e.message
                    ),
                }
            }
        }
        Ok(answered)
    }

    /// Projects that have `name` shared from `source`.
    fn share_targets(&self, source: &str, name: &str) -> Result<Vec<ProjectInfo>, HostlerError> {
        let store = self.store()?;
        let mut targets = Vec::new();
        for project in store.projects().map_err(store_error)? {
            let shared = store
                .shares_into(&project.id)
                .map_err(store_error)?
                .into_iter()
                .any(|(from, n)| from == source && n == name);
            if shared {
                targets.push(project);
            }
        }
        Ok(targets)
    }

    /// Answers `project`'s pending requests for `name` with `Stored` when the name is set there
    /// now (own or shared). The caller holds `publish` and no store guard.
    fn answer_if_set(
        &self,
        project: &ProjectInfo,
        name: &str,
    ) -> Result<Vec<SecretRequestInfo>, HostlerError> {
        if !self.is_set(project, name)? {
            return Ok(Vec::new());
        }
        let store = self.store()?;
        let mut infos = Vec::new();
        for id in store
            .answer_pending(&project.id, name, RequestState::Stored, self.now())
            .map_err(store_error)?
        {
            if let Some(info) = store.request(&id).map_err(store_error)? {
                infos.push(info);
            }
        }
        drop(store);
        for info in &infos {
            self.emit(Event::SecretRequestChanged(info.clone()));
        }
        Ok(infos)
    }

    fn remove(&self, project: &ProjectRef, name: &str) -> Result<Value, HostlerError> {
        check_name(name)?;
        let project = self.approved(project)?;
        self.require_unlocked()?;
        let _publishing = self.publishing();
        let Some(group) = group_of(&self.current(&project)?) else {
            return Ok(json!({ "removed": false }));
        };
        let present = self
            .with_vault(|v| {
                v.read(|db| !matches!(layout::lookup(db, group, name), Lookup::Missing))
            })
            .map_err(|e| e.to_hostler())?;
        if !present {
            return Ok(json!({ "removed": false }));
        }
        let removed = self
            .with_vault(|v| v.write(|db| Ok(layout::remove(db, group, name))))
            .map_err(|e| e.to_hostler())?;
        Ok(json!({ "removed": removed }))
    }

    /// Own or shared, and set.
    fn is_set(&self, project: &ProjectInfo, name: &str) -> Result<bool, HostlerError> {
        let shares = self.shares_into(project)?;
        let own = group_of(project);
        self.with_vault(|v| {
            v.read(|db| {
                let set = |g: Option<GroupId>| {
                    g.is_some_and(|g| matches!(layout::lookup(db, g, name), Lookup::Value(_)))
                };
                set(own)
                    || shares
                        .iter()
                        .any(|(from, n)| n == name && set(group_of(from)))
            })
        })
        .map_err(|e| e.to_hostler())
    }

    fn request(&self, r: SecretRequest) -> Result<SecretRequestInfo, HostlerError> {
        // A known id answers its recorded state, whatever the lock state (a waiter re-sends).
        {
            let store = self.store()?;
            if let Some(known) = store.request(&r.id).map_err(store_error)? {
                let recorded = store.project(&known.project_id).map_err(store_error)?;
                let canonical = std::fs::canonicalize(&r.project.common_dir)
                    .map(|d| display_path(&d))
                    .ok();
                let same_project = recorded.is_some_and(|p| {
                    p.common_dir == r.project.common_dir
                        || canonical.as_deref() == Some(p.common_dir.as_str())
                });
                if !same_project || known.name != r.name {
                    return Err(bad_request(
                        "this request id belongs to another project or secret",
                    ));
                }
                drop(store);
                return Ok(self.recheck(known));
            }
        }
        if uuid::Uuid::parse_str(&r.id)
            .map(|u| u.get_version_num() != 4)
            .unwrap_or(true)
        {
            return Err(bad_request("a request id is a UUID v4"));
        }
        check_name(&r.name)?;
        if r.reason.trim().is_empty() || r.reason.len() > MAX_REASON_BYTES {
            return Err(bad_request(format!(
                "the reason must be 1 to {MAX_REASON_BYTES} bytes"
            )));
        }
        let project = self.approved(&r.project)?;
        self.require_unlocked()?;
        // No set can commit and answer requests between the check and the insert (T6-3).
        let _publishing = self.publishing();
        let project = self.current(&project)?;
        let now = self.now();
        if self.is_set(&project, &r.name)? {
            return Ok(SecretRequestInfo {
                id: r.id,
                project_id: project.id,
                project_name: project.name,
                name: r.name,
                reason: r.reason,
                state: RequestState::Stored,
                requester_pid: r.requester.pid,
                created_at_ms: now,
                answered_at_ms: Some(now),
            });
        }
        self.at("request_checked");
        let store = self.store()?;
        if store.pending_requests().map_err(store_error)?.len() >= MAX_PENDING_REQUESTS {
            return Err(HostlerError {
                code: ErrorCode::QueueBusy,
                message: format!(
                    "{MAX_PENDING_REQUESTS} secret requests are already pending on this machine"
                ),
                details: json!({ "limit": MAX_PENDING_REQUESTS }),
                recovery: Some("hostler secret requests".into()),
            });
        }
        let info = store
            .insert_request(&r, &project.id, now)
            .map_err(store_error)?;
        drop(store);
        self.emit(Event::SecretRequestChanged(info.clone()));
        Ok(info)
    }

    /// A re-sent pending request whose name has been set since — by hostler through a share,
    /// or outside hostler — is answered now (T6-7). Locked: the recorded state, without the
    /// vault. Terminal states never change; a failed check keeps the recorded state.
    fn recheck(&self, known: SecretRequestInfo) -> SecretRequestInfo {
        if known.state != RequestState::Pending || !self.unlocked.load(Ordering::SeqCst) {
            return known;
        }
        let _publishing = self.publishing();
        let answered = (|| {
            let project = self
                .store()?
                .project(&known.project_id)
                .map_err(store_error)?
                .ok_or_else(|| internal("the request's project is gone"))?;
            self.answer_if_set(&project, &known.name)
        })();
        if answered.is_err() {
            return known;
        }
        self.store()
            .ok()
            .and_then(|store| store.request(&known.id).ok().flatten())
            .unwrap_or(known)
    }

    fn withdraw(&self, id: &str, timed_out: bool) -> Result<SecretRequestInfo, HostlerError> {
        let state = if timed_out {
            RequestState::TimedOut
        } else {
            RequestState::Withdrawn
        };
        let store = self.store()?;
        let changed = store
            .finish_request(id, state, self.now())
            .map_err(store_error)?;
        let info = store
            .request(id)
            .map_err(store_error)?
            .ok_or_else(|| bad_request("unknown secret request"))?;
        drop(store);
        if changed {
            self.emit(Event::SecretRequestChanged(info.clone()));
        }
        Ok(info)
    }

    fn requests(&self) -> Result<Vec<SecretRequestInfo>, HostlerError> {
        Ok(self
            .store()?
            .pending_requests()
            .map_err(store_error)?
            .into_iter()
            .map(|(info, _)| info)
            .collect())
    }

    fn decline(&self, d: Decline) -> Result<Vec<SecretRequestInfo>, HostlerError> {
        let now = self.now();
        let ids = match (d.id, d.project, d.name) {
            (Some(id), _, _) => {
                let store = self.store()?;
                let pending = store.pending_requests().map_err(store_error)?;
                let matching: Vec<&SecretRequestInfo> = pending
                    .iter()
                    .map(|(info, _)| info)
                    .filter(|info| info.id == id || (id.len() >= 8 && info.id.starts_with(&id)))
                    .collect();
                match matching.as_slice() {
                    [one] => {
                        store
                            .finish_request(&one.id, RequestState::Declined, now)
                            .map_err(store_error)?;
                        vec![one.id.clone()]
                    }
                    [] => return Err(bad_request("no pending secret request has this id")),
                    _ => {
                        return Err(bad_request(
                            "several pending requests start with this id; give more of it",
                        ));
                    }
                }
            }
            (None, Some(project), Some(name)) => {
                check_name(&name)?;
                let project = self.resolve(&project)?;
                let ids = self
                    .store()?
                    .answer_pending(&project.id, &name, RequestState::Declined, now)
                    .map_err(store_error)?;
                if ids.is_empty() {
                    return Err(bad_request(format!(
                        "no pending request for {name} in project {}",
                        project.name
                    )));
                }
                ids
            }
            _ => {
                return Err(bad_request(
                    "decline takes a request id, or a project and a NAME",
                ));
            }
        };
        let store = self.store()?;
        let mut declined = Vec::new();
        for id in ids {
            if let Some(info) = store.request(&id).map_err(store_error)? {
                declined.push(info);
            }
        }
        drop(store);
        for info in &declined {
            self.emit(Event::SecretRequestChanged(info.clone()));
        }
        Ok(declined)
    }

    fn share(&self, s: &Share) -> Result<Value, HostlerError> {
        check_name(&s.name)?;
        let source = self.approved(&s.from)?;
        let target = self
            .store()?
            .project(&s.to_project_id)
            .map_err(store_error)?
            .ok_or_else(|| bad_request("unknown target project"))?;
        if target.id == source.id {
            return Err(bad_request(
                "a secret cannot be shared with its own project",
            ));
        }
        if !target.approved {
            return Err(errors::project_not_approved(
                &target.name,
                &target.common_dir,
            ));
        }
        self.require_unlocked()?;
        let already_shared = self
            .store()?
            .shares_into(&target.id)
            .map_err(store_error)?
            .iter()
            .any(|(_, name)| *name == s.name);
        if already_shared {
            return Err(bad_request(format!(
                "project {} already has {} shared with it",
                target.name, s.name
            )));
        }
        let (source_group, target_group) = (group_of(&source), group_of(&target));
        let (set_in_source, target_has_own) = self
            .with_vault(|v| {
                v.read(|db| {
                    let source_set = source_group.is_some_and(|g| {
                        matches!(layout::lookup(db, g, &s.name), Lookup::Value(_))
                    });
                    let target_own = target_group.is_some_and(|g| {
                        !matches!(layout::lookup(db, g, &s.name), Lookup::Missing)
                    });
                    (source_set, target_own)
                })
            })
            .map_err(|e| e.to_hostler())?;
        if !set_in_source {
            return Err(bad_request(format!(
                "{} is not set in project {}",
                s.name, source.name
            )));
        }
        if target_has_own {
            return Err(bad_request(format!(
                "project {} has its own {}",
                target.name, s.name
            )));
        }
        let added = self
            .store()?
            .add_share(&source.id, &s.name, &target.id, self.now())
            .map_err(store_error)?;
        if !added {
            return Err(bad_request(format!(
                "project {} already has {} shared with it",
                target.name, s.name
            )));
        }
        Ok(json!({ "name": s.name, "from": source.id, "to": target.id }))
    }

    fn unshare(&self, s: &Share) -> Result<Value, HostlerError> {
        check_name(&s.name)?;
        let source = self.resolve(&s.from)?;
        let removed = self
            .store()?
            .remove_share(&source.id, &s.name, &s.to_project_id)
            .map_err(store_error)?;
        if !removed {
            return Err(bad_request(format!(
                "{} is not shared from project {} with that project",
                s.name, source.name
            )));
        }
        Ok(json!({ "unshared": true }))
    }

    /// The values for a run, encoded straight into a zeroizing string. The recorded command is
    /// masked with these values first and cut afterwards (L6, L13); nothing is recorded when
    /// the run is refused.
    fn materialize(&self, m: Materialize) -> Result<Zeroizing<String>, HostlerError> {
        if m.names.is_empty() || m.names.len() > MAX_RUN_NAMES {
            return Err(bad_request(format!(
                "a run takes 1 to {MAX_RUN_NAMES} secrets"
            )));
        }
        let mut names: Vec<String> = Vec::new();
        for name in &m.names {
            check_name(name)?;
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        if m.command.len() > MAX_COMMAND_BYTES {
            return Err(bad_request(
                "the command is over 16 KiB; send `<command of N bytes>` instead",
            ));
        }
        if m.run_id.is_empty() {
            return Err(bad_request("a run needs a run id"));
        }
        let project = self.approved(&m.project)?;
        self.require_unlocked()?;
        let shares = self.shares_into(&project)?;
        let others: Vec<ProjectInfo> = self
            .store()?
            .projects()
            .map_err(store_error)?
            .into_iter()
            .filter(|p| p.id != project.id && p.group_uuid.is_some())
            .collect();
        let own = group_of(&project);
        let found: Vec<Found> = self
            .with_vault(|v| {
                v.read(|db| {
                    let look = |g: Option<GroupId>, name: &str| match g {
                        Some(g) => layout::lookup(db, g, name),
                        None => Lookup::Missing,
                    };
                    names
                        .iter()
                        .map(|name| match look(own, name) {
                            Lookup::Value(v) => Found::Value(v),
                            Lookup::Duplicate => Found::Duplicate,
                            Lookup::Missing => {
                                let shared = shares.iter().find(|(_, n)| n == name);
                                match shared.map(|(from, _)| look(group_of(from), name)) {
                                    Some(Lookup::Value(v)) => Found::Value(v),
                                    Some(Lookup::Duplicate) => Found::Duplicate,
                                    _ => others
                                        .iter()
                                        .find(|p| {
                                            !matches!(look(group_of(p), name), Lookup::Missing)
                                        })
                                        .map_or(Found::Missing, |p| {
                                            Found::Elsewhere(p.name.clone())
                                        }),
                                }
                            }
                        })
                        .collect()
                })
            })
            .map_err(|e| e.to_hostler())?;

        let mut values = Vec::with_capacity(names.len());
        let mut elsewhere = None;
        let mut missing = Vec::new();
        for (name, found) in names.iter().zip(found) {
            match found {
                // A value edited outside hostler may break the value rules (L19).
                Found::Value(value) if check_value(&value).is_err() => {
                    return Err(invalid_value(name));
                }
                Found::Value(value) => values.push(MaterializedSecret {
                    name: name.clone(),
                    value,
                }),
                Found::Duplicate => return Err(duplicate_entries(name)),
                Found::Elsewhere(owner) => {
                    elsewhere.get_or_insert((name.clone(), owner));
                }
                Found::Missing => missing.push(name.clone()),
            }
        }
        if let Some((name, owner)) = elsewhere {
            return Err(errors::secret_not_in_project(&name, &owner, &project.name));
        }
        if let Some(first) = missing.first() {
            let mut e = errors::secret_missing(first, &project.name);
            e.details["missing"] = json!(missing);
            return Err(e);
        }

        let mut encoded = encode_answer(&values)?;
        let mask = MaskSet::new(values.iter().map(|v| v.value.expose().as_bytes().to_vec()));
        let masked = mask.mask_all(m.command.expose().as_bytes());
        let recorded = keep_head(&String::from_utf8_lossy(&masked), HISTORY_COMMAND_BYTES);
        self.store()?
            .record_materialization(
                &m.run_id,
                &project.id,
                &names,
                &m.workspace,
                &recorded,
                self.now(),
            )
            .map_err(store_error)?;
        let text = String::from_utf8(std::mem::take(&mut *encoded))
            .map_err(|_| internal("cannot encode the secrets for the run"))?;
        Ok(Zeroizing::new(text))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::contract::secrets::ImportEntry;
    use crate::procs::ProcessIdentity;
    use crate::procs::fake::FakeTable;
    use crate::secrets::value::RawCommand;
    use crate::secrets::vault::HookPoint;

    const BOOT: &str = "b1";
    const PASSWORD: &str = "pass-1234";

    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct World {
        engine: Arc<SecretsEngine>,
        dir: tempfile::TempDir,
        table: Arc<FakeTable>,
        events: broadcast::Receiver<Event>,
    }

    fn world() -> World {
        world_with(|_, _| {})
    }

    /// `hooks` may install a vault test hook before the engine takes the vault.
    fn world_with(hooks: impl FnOnce(&mut Vault, &Path)) -> World {
        let dir = tempfile::tempdir().unwrap();
        let kdbx = dir.path().join("hostler.kdbx");
        let mut vault = Vault::new(kdbx.clone());
        hooks(&mut vault, &kdbx);
        let table = Arc::new(FakeTable::new(BOOT));
        let (tx, events) = broadcast::channel(256);
        let engine = SecretsEngine::new(
            vault,
            kdbx,
            Store::open(&dir.path().join("hostler.db")).unwrap(),
            table.clone(),
            Arc::new(TestClock(AtomicI64::new(1_000_000))),
            tx,
            KdfChoice::Fast,
        );
        World {
            engine: Arc::new(engine),
            dir,
            table,
            events,
        }
    }

    fn key() -> VaultKey {
        VaultKey::new(&SecretValue::new(PASSWORD.into()), None).unwrap()
    }

    impl World {
        fn call(&self, req: Request) -> Result<Value, HostlerError> {
            match self.engine.dispatch(req)? {
                Reply::Value(v) => Ok(v),
                Reply::Sensitive(_) => panic!("unexpected sensitive reply"),
            }
        }

        fn call_as<T: serde::de::DeserializeOwned>(&self, req: Request) -> Result<T, HostlerError> {
            self.call(req).map(|v| serde_json::from_value(v).unwrap())
        }

        fn repo(&self, name: &str) -> ProjectRef {
            let git = self.dir.path().join("repos").join(name).join(".git");
            std::fs::create_dir_all(&git).unwrap();
            std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            ProjectRef {
                common_dir: display_path(&std::fs::canonicalize(&git).unwrap()),
                name: name.into(),
            }
        }

        fn unlock_new(&self) {
            self.call(Request::SecretsUnlock(Unlock {
                password: SecretValue::new(PASSWORD.into()),
                key_file: None,
                forget_key_file: false,
                create: true,
            }))
            .unwrap();
        }

        fn approved_repo(&self, name: &str) -> ProjectRef {
            let p = self.repo(name);
            self.call(Request::ProjectApprove(p.clone())).unwrap();
            p
        }

        fn project(&self, p: &ProjectRef) -> ProjectInfo {
            self.call_as(Request::ProjectResolve(p.clone())).unwrap()
        }

        fn set(&self, p: &ProjectRef, name: &str, value: &str) -> Result<SetOutcome, HostlerError> {
            self.call_as(Request::SecretSet(SecretSet {
                project: p.clone(),
                name: name.into(),
                value: SecretValue::new(value.into()),
            }))
        }

        fn import(
            &self,
            p: &ProjectRef,
            entries: &[(&str, &str)],
            overwrite: bool,
        ) -> Result<ImportOutcome, HostlerError> {
            self.call_as(Request::SecretImport(SecretImport {
                project: p.clone(),
                entries: entries
                    .iter()
                    .map(|(n, v)| ImportEntry {
                        name: (*n).into(),
                        value: SecretValue::new((*v).into()),
                    })
                    .collect(),
                overwrite,
            }))
        }

        fn list(&self, p: &ProjectRef, declared: &[&str]) -> SecretList {
            self.call_as(Request::SecretList {
                project: p.clone(),
                declared: declared
                    .iter()
                    .map(|n| DeclaredSecret {
                        name: (*n).into(),
                        purpose: format!("purpose of {n}"),
                        env: None,
                    })
                    .collect(),
            })
            .unwrap()
        }

        fn materialize_with(
            &self,
            p: &ProjectRef,
            names: &[&str],
            command: &str,
        ) -> Result<Vec<MaterializedSecret>, HostlerError> {
            let reply = self
                .engine
                .dispatch(Request::SecretMaterialize(Materialize {
                    run_id: uuid::Uuid::new_v4().to_string(),
                    project: p.clone(),
                    names: names.iter().map(|n| (*n).into()).collect(),
                    command: RawCommand::new(command.into()),
                    workspace: "/work/app".into(),
                }))?;
            match reply {
                Reply::Sensitive(json) => Ok(serde_json::from_str(&json).unwrap()),
                Reply::Value(_) => panic!("materialize must answer sensitive"),
            }
        }

        fn materialize(
            &self,
            p: &ProjectRef,
            names: &[&str],
        ) -> Result<Vec<MaterializedSecret>, HostlerError> {
            self.materialize_with(p, names, "make test")
        }

        fn requester(&self, pid: u32) -> ProcessIdentity {
            self.table.insert(pid, None, Some(u64::from(pid) * 10));
            ProcessIdentity {
                pid,
                boot_id: BOOT.into(),
                start_time: u64::from(pid) * 10,
            }
        }

        fn request(&self, p: &ProjectRef, name: &str, pid: u32) -> SecretRequestInfo {
            self.request_id(&uuid::Uuid::new_v4().to_string(), p, name, pid)
                .unwrap()
        }

        fn request_id(
            &self,
            id: &str,
            p: &ProjectRef,
            name: &str,
            pid: u32,
        ) -> Result<SecretRequestInfo, HostlerError> {
            let requester = self.requester(pid);
            self.call_as(Request::SecretRequest(SecretRequest {
                id: id.into(),
                project: p.clone(),
                name: name.into(),
                reason: "run the integration tests".into(),
                requester,
            }))
        }

        /// The recorded materializations: `(names_json, workspace, command)`.
        fn history(&self) -> Vec<(String, String, String)> {
            let conn = rusqlite::Connection::open(self.dir.path().join("hostler.db")).unwrap();
            let mut stmt = conn
                .prepare("SELECT names_json, workspace, command FROM secret_materializations")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }

        fn drain_events(&mut self) -> Vec<Event> {
            let mut events = Vec::new();
            while let Ok(e) = self.events.try_recv() {
                events.push(e);
            }
            events
        }
    }

    fn value_of(secrets: &[MaterializedSecret], name: &str) -> String {
        secrets
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.value.expose().to_owned())
            .unwrap_or_else(|| panic!("{name} was not materialized"))
    }

    /// A second vault on the same file saves an outside change, the way KeePassXC would.
    fn outside_write(kdbx: &Path, f: impl Fn(&mut Database)) {
        let mut outside = Vault::new(kdbx.to_path_buf());
        outside.unlock(key()).unwrap();
        outside
            .write(|db| {
                f(db);
                Ok(())
            })
            .unwrap();
    }

    fn outside_lookup(kdbx: &Path, group: GroupId, name: &str) -> Option<String> {
        let mut outside = Vault::new(kdbx.to_path_buf());
        outside.unlock(key()).unwrap();
        outside
            .read(|db| match layout::lookup(db, group, name) {
                Lookup::Value(v) => Some(v.expose().to_owned()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn status_answers_while_a_save_holds_the_vault() {
        let hold = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicBool::new(false));
        let (h1, h2) = (hold.clone(), held.clone());
        let w = world_with(move |vault, _| {
            vault.set_test_hook(Box::new(move |point| {
                if point == HookPoint::BeforePlace && h1.load(Ordering::SeqCst) {
                    h2.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_secs(3));
                }
            }));
        });
        w.unlock_new();
        let p = w.approved_repo("app");
        hold.store(true, Ordering::SeqCst);
        let engine = w.engine.clone();
        let p2 = p.clone();
        let save = std::thread::spawn(move || {
            engine.dispatch(Request::SecretSet(SecretSet {
                project: p2,
                name: "API_KEY".into(),
                value: SecretValue::new("value-1".into()),
            }))
        });
        while !held.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        let status: SecretsStatus = w.call_as(Request::SecretsStatus).unwrap();
        assert_eq!(status.state, LockState::Unlocked);
        let _: Vec<SecretRequestInfo> = w.call_as(Request::SecretRequests).unwrap();
        // Generous for a loaded machine; the save holds the vault for 3 s.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(save.join().unwrap().is_ok());
    }

    #[test]
    fn locked_list_answers_at_once() {
        let w = world();
        let p = w.approved_repo("app");
        let started = Instant::now();
        let e = w
            .call(Request::SecretList {
                project: p.clone(),
                declared: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretsLocked);
        assert_eq!(e.message, errors::secrets_locked(true).message);
        w.unlock_new();
        w.call(Request::SecretsLock).unwrap();
        let e = w.materialize(&p, &["API_KEY"]).unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretsLocked);
        assert_eq!(e.message, errors::secrets_locked(false).message);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn unapproved_project_is_refused() {
        let w = world();
        w.unlock_new();
        let p = w.repo("app");
        let refused = |r: Result<Value, HostlerError>| {
            let e = r.unwrap_err();
            assert_eq!(e.code, ErrorCode::ProjectNotApproved, "{e:?}");
            assert_eq!(e.recovery.as_deref(), Some("hostler project approve"));
        };
        refused(w.call(Request::SecretList {
            project: p.clone(),
            declared: Vec::new(),
        }));
        refused(w.set(&p, "API_KEY", "value-1").map(|_| Value::Null));
        refused(w.materialize(&p, &["API_KEY"]).map(|_| Value::Null));
        refused(
            w.request_id(&uuid::Uuid::new_v4().to_string(), &p, "API_KEY", 42)
                .map(|_| Value::Null),
        );
        // Resolving records it, unapproved.
        assert!(!w.project(&p).approved);
    }

    #[test]
    fn set_answers_pending_requests_and_emits_events() {
        let mut w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let r = w.request(&p, "API_KEY", 42);
        assert_eq!(r.state, RequestState::Pending);
        w.drain_events();
        let out = w.set(&p, "API_KEY", "value-1").unwrap();
        assert_eq!(out.answered_requests, std::slice::from_ref(&r.id));
        let events = w.drain_events();
        assert!(events.iter().any(|e| matches!(e,
            Event::SecretRequestChanged(i) if i.id == r.id && i.state == RequestState::Stored)));
        let again: SecretRequestInfo = w.request_id(&r.id, &p, "API_KEY", 42).unwrap();
        assert_eq!(again.state, RequestState::Stored);
        assert!(value_of(&w.materialize(&p, &["API_KEY"]).unwrap(), "API_KEY") == "value-1");
    }

    #[test]
    fn import_is_all_or_nothing() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let e = w
            .import(&p, &[("GOOD", "value-1"), ("SHORT", "abc")], false)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretTooShort);
        let list = w.list(&p, &[]);
        assert!(list.secrets.is_empty(), "{:?}", list.secrets);
        let e = w
            .import(&p, &[("GOOD", "value-1"), ("GOOD", "value-2")], false)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        let e = w
            .import(&p, &[("GOOD", "value-1"), ("bad-name", "value-2")], false)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(w.list(&p, &[]).secrets.is_empty());
    }

    #[test]
    fn import_without_overwrite_refuses_existing() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "A", "value-a1").unwrap();
        let e = w
            .import(&p, &[("A", "value-a2"), ("B", "value-b1")], false)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretExists);
        assert_eq!(e.message, "already set: A; add --overwrite to replace");
        assert!(w.materialize(&p, &["B"]).is_err());
        let out = w
            .import(&p, &[("A", "value-a2"), ("B", "value-b1")], true)
            .unwrap();
        assert_eq!(out.stored, ["A", "B"]);
        let got = w.materialize(&p, &["A", "B"]).unwrap();
        assert!(value_of(&got, "A") == "value-a2" && value_of(&got, "B") == "value-b1");
    }

    #[test]
    fn outside_add_between_attempts_is_secret_exists() {
        let group: Arc<Mutex<Option<GroupId>>> = Arc::new(Mutex::new(None));
        let fired = Arc::new(AtomicUsize::new(0));
        let (g, f) = (group.clone(), fired.clone());
        let mut w = world_with(move |vault, kdbx| {
            let kdbx = kdbx.to_path_buf();
            vault.set_test_hook(Box::new(move |point| {
                let Some(group) = *g.lock().unwrap() else {
                    return;
                };
                if point == HookPoint::BeforeMoveAside && f.fetch_add(1, Ordering::SeqCst) == 0 {
                    outside_write(&kdbx, |db| {
                        layout::put(db, group, "TOKEN", &SecretValue::new("outside-1".into()))
                    });
                }
            }));
        });
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "OTHER", "value-1").unwrap();
        let before = w.project(&p).group_uuid;
        let gid = group_of(&w.project(&p)).unwrap();
        *group.lock().unwrap() = Some(gid);
        w.drain_events();
        let e = w.import(&p, &[("TOKEN", "mine-1")], false).unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretExists, "{e:?}");
        assert!(fired.load(Ordering::SeqCst) >= 1);
        let kdbx = w.dir.path().join("hostler.kdbx");
        assert_eq!(
            outside_lookup(&kdbx, gid, "TOKEN").as_deref(),
            Some("outside-1")
        );
        assert_eq!(w.project(&p).group_uuid, before);
        assert!(
            !w.drain_events()
                .iter()
                .any(|e| matches!(e, Event::SecretRequestChanged(_)))
        );
    }

    #[test]
    fn list_unions_declared_stored_and_shared() {
        let w = world();
        w.unlock_new();
        let a = w.approved_repo("app");
        let b = w.approved_repo("lib");
        w.set(&a, "OWN", "value-own").unwrap();
        w.set(&b, "SHARED", "value-shared").unwrap();
        let a_id = w.project(&a).id;
        w.call(Request::SecretShare(Share {
            from: b.clone(),
            name: "SHARED".into(),
            to_project_id: a_id,
        }))
        .unwrap();
        let list = w.list(&a, &["DECL", "OWN"]);
        let by = |n: &str| list.secrets.iter().find(|s| s.name == n).unwrap().clone();
        assert_eq!(list.secrets.len(), 3);
        let decl = by("DECL");
        assert!(decl.declared && decl.state == SecretState::Missing && decl.source.is_none());
        assert_eq!(decl.purpose.as_deref(), Some("purpose of DECL"));
        let own = by("OWN");
        assert!(own.declared && own.state == SecretState::Set);
        assert_eq!(own.source, Some(SecretSource::Own));
        let shared = by("SHARED");
        assert!(!shared.declared && shared.state == SecretState::Set);
        assert!(
            matches!(shared.source, Some(SecretSource::SharedFrom { ref project, .. }) if project == "lib")
        );
    }

    #[test]
    fn own_entry_wins_over_share() {
        let w = world();
        w.unlock_new();
        let a = w.approved_repo("app");
        let b = w.approved_repo("lib");
        w.set(&b, "X", "value-lib").unwrap();
        w.call(Request::SecretShare(Share {
            from: b.clone(),
            name: "X".into(),
            to_project_id: w.project(&a).id,
        }))
        .unwrap();
        assert!(value_of(&w.materialize(&a, &["X"]).unwrap(), "X") == "value-lib");
        w.set(&a, "X", "value-app").unwrap();
        assert!(value_of(&w.materialize(&a, &["X"]).unwrap(), "X") == "value-app");
        let list = w.list(&a, &[]);
        assert_eq!(list.secrets[0].source, Some(SecretSource::Own));
    }

    #[test]
    fn materialize_missing_lists_all_missing() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "HAVE", "value-1").unwrap();
        let e = w.materialize(&p, &["M1", "HAVE", "M2"]).unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretMissing);
        assert_eq!(e.details["missing"], json!(["M1", "M2"]));
        assert_eq!(e.message, "secret M1 is not set for project app");
        assert!(w.history().is_empty());
    }

    #[test]
    fn materialize_other_project_is_not_in_project() {
        let w = world();
        w.unlock_new();
        let a = w.approved_repo("app");
        let b = w.approved_repo("lib");
        w.set(&b, "X", "value-lib").unwrap();
        let e = w.materialize(&a, &["MISSING", "X"]).unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretNotInProject);
        assert_eq!(
            e.recovery.as_deref(),
            Some("hostler secret share X --to app")
        );
    }

    #[test]
    fn materialize_shared_value_comes_from_source() {
        let w = world();
        w.unlock_new();
        let a = w.approved_repo("app");
        let b = w.approved_repo("lib");
        w.set(&b, "X", "value-lib").unwrap();
        w.call(Request::SecretShare(Share {
            from: b.clone(),
            name: "X".into(),
            to_project_id: w.project(&a).id,
        }))
        .unwrap();
        assert!(value_of(&w.materialize(&a, &["X"]).unwrap(), "X") == "value-lib");
        w.set(&b, "X", "value-lib-2").unwrap();
        assert!(value_of(&w.materialize(&a, &["X"]).unwrap(), "X") == "value-lib-2");
    }

    #[test]
    fn materialize_masks_the_command_it_records() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "API_KEY", "hlx-canary-7f3a9c").unwrap();
        w.materialize_with(&p, &["API_KEY"], r#"curl -H "X: hlx-canary-7f3a9c""#)
            .unwrap();
        let history = w.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].2, r#"curl -H "X: ***""#);
        let (names, workspace, command) = &history[0];
        for column in [names, workspace, command] {
            assert!(!column.contains("hlx-canary"));
        }
    }

    #[test]
    fn history_command_is_masked_before_it_is_cut() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let value = "hlxprefix-7f3a-0123";
        w.set(&p, "API_KEY", value).unwrap();
        let command = format!(
            "{}{value}{}",
            "a".repeat(1018),
            "b".repeat(3000 - 1018 - value.len())
        );
        assert_eq!(command.len(), 3000);
        w.materialize_with(&p, &["API_KEY"], &command).unwrap();
        let recorded = w.history().remove(0).2;
        let mask = MaskSet::new([value.as_bytes().to_vec()]);
        let expected = keep_head(
            &String::from_utf8(mask.mask_all(command.as_bytes())).unwrap(),
            HISTORY_COMMAND_BYTES,
        );
        assert_eq!(recorded, expected);
        assert!(recorded.len() <= HISTORY_COMMAND_BYTES);
        assert!(!recorded.contains("hlxpre"));
    }

    #[test]
    fn placeholder_command_is_recorded_as_is() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "API_KEY", "value-1").unwrap();
        w.materialize_with(&p, &["API_KEY"], "<command of 20000 bytes>")
            .unwrap();
        assert_eq!(w.history()[0].2, "<command of 20000 bytes>");
        let e = w
            .materialize_with(&p, &["API_KEY"], &"x".repeat(MAX_COMMAND_BYTES + 1))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert_eq!(w.history().len(), 1);
    }

    #[test]
    fn materialize_over_4_mib_is_refused_before_history() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let quotes = "\"".repeat(super::super::MAX_VALUE_BYTES);
        let names: Vec<String> = (0..40).map(|i| format!("Q{i}")).collect();
        let entries: Vec<(&str, &str)> = names
            .iter()
            .map(|n| (n.as_str(), quotes.as_str()))
            .collect();
        w.import(&p, &entries, false).unwrap();
        let all: Vec<&str> = names.iter().map(String::as_str).collect();
        let e = w.materialize(&p, &all).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert_eq!(e.message, "the requested secrets are too large for one run");
        assert!(w.history().is_empty());
        let one = w.materialize(&p, &["Q0"]).unwrap();
        assert_eq!(one[0].value.len(), super::super::MAX_VALUE_BYTES);
        assert_eq!(w.history().len(), 1);
    }

    #[test]
    fn materialize_reply_is_sensitive() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "API_KEY", "value-1").unwrap();
        let reply = w
            .engine
            .dispatch(Request::SecretMaterialize(Materialize {
                run_id: "run-1".into(),
                project: p,
                names: vec!["API_KEY".into(), "API_KEY".into()],
                command: RawCommand::new("make".into()),
                workspace: "/w".into(),
            }))
            .unwrap();
        let Reply::Sensitive(json) = reply else {
            panic!("not sensitive");
        };
        let got: Vec<MaterializedSecret> = serde_json::from_str(&json).unwrap();
        assert_eq!(got.len(), 1, "names are deduplicated");
    }

    #[test]
    fn admission_while_locked() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let r1 = w.request(&p, "API_KEY", 42);
        let r2 = w.request(&p, "API_KEY", 43);
        let r3 = w.request(&p, "OTHER", 44);
        let b = w.approved_repo("lib");
        w.set(&b, "X", "value-x").unwrap();
        w.call(Request::SecretShare(Share {
            from: b.clone(),
            name: "X".into(),
            to_project_id: w.project(&p).id,
        }))
        .unwrap();
        w.call(Request::SecretsLock).unwrap();

        let again = w.request_id(&r1.id, &p, "API_KEY", 42).unwrap();
        assert_eq!(again.state, RequestState::Pending);
        let withdrawn: SecretRequestInfo = w
            .call_as(Request::SecretRequestWithdraw {
                id: r2.id.clone(),
                timed_out: false,
            })
            .unwrap();
        assert_eq!(withdrawn.state, RequestState::Withdrawn);
        let declined: Vec<SecretRequestInfo> = w
            .call_as(Request::SecretDecline(Decline {
                id: Some(r3.id[..8].into()),
                project: None,
                name: None,
            }))
            .unwrap();
        assert_eq!(declined[0].state, RequestState::Declined);
        let pending: Vec<SecretRequestInfo> = w.call_as(Request::SecretRequests).unwrap();
        assert_eq!(pending.len(), 1);
        w.call(Request::SecretUnshare(Share {
            from: b,
            name: "X".into(),
            to_project_id: w.project(&p).id,
        }))
        .unwrap();
        let e = w
            .request_id(&uuid::Uuid::new_v4().to_string(), &p, "NEW", 45)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretsLocked);
        // A known id under another project is refused.
        let other = w.repo("other");
        let e = w.request_id(&r1.id, &other, "API_KEY", 42).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
    }

    #[test]
    fn known_request_id_keeps_its_terminal_state() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let r = w.request(&p, "API_KEY", 42);
        w.call(Request::SecretDecline(Decline {
            id: Some(r.id.clone()),
            project: None,
            name: None,
        }))
        .unwrap();
        w.set(&p, "API_KEY", "value-1").unwrap();
        let again = w.request_id(&r.id, &p, "API_KEY", 42).unwrap();
        assert_eq!(again.state, RequestState::Declined);
        // A new request for a set name is stored at once, without a row.
        let fresh = w.request(&p, "API_KEY", 46);
        assert_eq!(fresh.state, RequestState::Stored);
        let pending: Vec<SecretRequestInfo> = w.call_as(Request::SecretRequests).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn dead_requester_is_withdrawn_by_tick() {
        let mut w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let alive = w.request(&p, "API_KEY", 42);
        let dead = w.request(&p, "OTHER", 43);
        w.table.remove(43);
        w.drain_events();
        w.engine.tick();
        assert!(w.drain_events().iter().any(|e| matches!(e,
            Event::SecretRequestChanged(i) if i.id == dead.id && i.state == RequestState::Withdrawn)));
        let state = |r: &SecretRequestInfo| {
            w.request_id(&r.id, &p, &r.name, r.requester_pid)
                .unwrap()
                .state
        };
        assert_eq!(state(&dead), RequestState::Withdrawn);
        assert_eq!(state(&alive), RequestState::Pending);
    }

    #[test]
    fn decline_by_name_declines_all_pending() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        let r1 = w.request(&p, "API_KEY", 42);
        let r2 = w.request(&p, "API_KEY", 43);
        let other = w.request(&p, "OTHER", 44);
        let declined: Vec<SecretRequestInfo> = w
            .call_as(Request::SecretDecline(Decline {
                id: None,
                project: Some(p.clone()),
                name: Some("API_KEY".into()),
            }))
            .unwrap();
        let mut ids: Vec<String> = declined.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let mut want = vec![r1.id, r2.id];
        want.sort();
        assert_eq!(ids, want);
        assert!(declined.iter().all(|i| i.state == RequestState::Declined));
        let pending: Vec<SecretRequestInfo> = w.call_as(Request::SecretRequests).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, other.id);
    }

    #[test]
    fn share_refuses_unset_or_own_target() {
        let w = world();
        w.unlock_new();
        let a = w.approved_repo("app");
        let b = w.approved_repo("lib");
        let a_id = w.project(&a).id;
        let b_id = w.project(&b).id;
        let share = |from: &ProjectRef, name: &str, to: &str| {
            w.call(Request::SecretShare(Share {
                from: from.clone(),
                name: name.into(),
                to_project_id: to.into(),
            }))
        };
        assert_eq!(
            share(&b, "X", &a_id).unwrap_err().code,
            ErrorCode::BadRequest
        );
        w.set(&b, "X", "value-lib").unwrap();
        assert_eq!(
            share(&b, "X", &b_id).unwrap_err().code,
            ErrorCode::BadRequest
        );
        w.set(&a, "X", "value-app").unwrap();
        assert_eq!(
            share(&b, "X", &a_id).unwrap_err().code,
            ErrorCode::BadRequest
        );
        let unapproved = w.repo("other");
        let u_id = w.project(&unapproved).id;
        assert_eq!(
            share(&b, "X", &u_id).unwrap_err().code,
            ErrorCode::ProjectNotApproved
        );
        let c = w.approved_repo("tool");
        let c_id = w.project(&c).id;
        share(&b, "X", &c_id).unwrap();
        assert_eq!(
            share(&b, "X", &c_id).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn group_deleted_outside_is_recreated_on_write() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "A", "value-a").unwrap();
        let first = group_of(&w.project(&p)).unwrap();
        // KeePassXC deletes a group by moving it into the Recycle Bin.
        outside_write(&w.dir.path().join("hostler.kdbx"), |db| {
            let bin = {
                let mut root = db.root_mut();
                let mut bin = root.add_group();
                bin.name = "Recycle Bin".into();
                bin.id()
            };
            db.meta.recyclebin_uuid = Some(bin.uuid());
            db.group_mut(first).unwrap().move_to(bin).unwrap();
        });
        w.set(&p, "B", "value-b").unwrap();
        let second = group_of(&w.project(&p)).unwrap();
        assert_ne!(first, second);
        let list = w.list(&p, &["A"]);
        let state = |n: &str| list.secrets.iter().find(|s| s.name == n).unwrap().state;
        assert_eq!(state("A"), SecretState::Missing);
        assert_eq!(state("B"), SecretState::Set);
    }

    #[test]
    fn closing_refuses_new_work() {
        let w = world();
        w.engine.begin_shutdown();
        for req in [Request::SecretsStatus, Request::SecretRequests] {
            let e = w.call(req).unwrap_err();
            assert_eq!(e.code, ErrorCode::ServiceUnavailable);
            assert_eq!(e.details["reason"], "stopping");
        }
    }

    #[test]
    fn unlock_remembers_and_forgets_the_key_file() {
        let w = world();
        let key_file = w.dir.path().join("key.bin");
        std::fs::write(&key_file, b"0123456789abcdef0123456789abcdef").unwrap();
        let unlock = |key_file: Option<&PathBuf>, forget: bool, create: bool| {
            w.call_as::<SecretsStatus>(Request::SecretsUnlock(Unlock {
                password: SecretValue::new(PASSWORD.into()),
                key_file: key_file.map(|k| k.display().to_string()),
                forget_key_file: forget,
                create,
            }))
        };
        let status = unlock(Some(&key_file), false, true).unwrap();
        assert_eq!(status.key_file.as_deref(), Some(key_file.to_str().unwrap()));
        w.call(Request::SecretsLock).unwrap();
        // The remembered key file is used.
        assert_eq!(
            unlock(None, false, false).unwrap().state,
            LockState::Unlocked
        );
        w.call(Request::SecretsLock).unwrap();
        // Without it the key is wrong; the remembered path is forgotten only on success.
        assert_eq!(
            unlock(None, true, false).unwrap_err().code,
            ErrorCode::UnlockFailed
        );
        let status: SecretsStatus = w.call_as(Request::SecretsStatus).unwrap();
        assert!(status.key_file.is_some());
        let e = unlock(None, false, true).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(e.message.starts_with("a database already exists at "));
    }

    #[test]
    fn lock_changes_are_events() {
        let mut w = world();
        w.unlock_new();
        w.call(Request::SecretsLock).unwrap();
        let states: Vec<LockState> = w
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                Event::SecretsLockChanged(s) => Some(s.state),
                _ => None,
            })
            .collect();
        assert_eq!(states, [LockState::Unlocked, LockState::Locked]);
    }

    /// A save that pauses at `BeforePlace` for `ms` once `armed` is set; `held` reports it.
    fn pausing_world(ms: u64) -> (World, Arc<AtomicBool>, Arc<AtomicBool>) {
        let armed = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicBool::new(false));
        let (a, h) = (armed.clone(), held.clone());
        let w = world_with(move |vault, _| {
            vault.set_test_hook(Box::new(move |point| {
                if point == HookPoint::BeforePlace && a.swap(false, Ordering::SeqCst) {
                    h.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(ms));
                }
            }));
        });
        (w, armed, held)
    }

    fn wait_for(flag: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !flag.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "the pause point was never reached"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn in_thread<T: Send + 'static>(
        w: &World,
        f: impl FnOnce(&SecretsEngine) -> T + Send + 'static,
    ) -> std::thread::JoinHandle<T> {
        let engine = w.engine.clone();
        std::thread::spawn(move || f(&engine))
    }

    fn set_request(p: &ProjectRef, name: &str, value: &str) -> Request {
        Request::SecretSet(SecretSet {
            project: p.clone(),
            name: name.into(),
            value: SecretValue::new(value.into()),
        })
    }

    /// T6-1: B resolves the project while A's first save (creating the group) is paused; both
    /// end up in the one recorded group. Again after the group was recycled outside.
    #[test]
    fn concurrent_first_writes_share_one_group() {
        let (w, armed, held) = pausing_world(1500);
        w.unlock_new();
        let p = w.approved_repo("app");
        for (first, second) in [("A", "B"), ("C", "D")] {
            armed.store(true, Ordering::SeqCst);
            held.store(false, Ordering::SeqCst);
            let (pa, pb) = (p.clone(), p.clone());
            let a = in_thread(&w, move |e| e.dispatch(set_request(&pa, first, "value-1")));
            wait_for(&held);
            let b = in_thread(&w, move |e| e.dispatch(set_request(&pb, second, "value-2")));
            assert!(a.join().unwrap().is_ok());
            assert!(b.join().unwrap().is_ok());
            let got = w.materialize(&p, &[first, second]).unwrap();
            assert!(value_of(&got, first) == "value-1" && value_of(&got, second) == "value-2");
            // Recycle the group outside before the second round.
            let group = group_of(&w.project(&p)).unwrap();
            outside_write(&w.dir.path().join("hostler.kdbx"), |db| {
                let bin = {
                    let mut root = db.root_mut();
                    let mut bin = root.add_group();
                    bin.name = "Recycle Bin".into();
                    bin.id()
                };
                db.meta.recyclebin_uuid = Some(bin.uuid());
                db.group_mut(group).unwrap().move_to(bin).unwrap();
            });
        }
    }

    /// T6-1: two no-overwrite imports of one name into a project without a group: one wins.
    #[test]
    fn concurrent_first_imports_of_one_name_refuse_the_second() {
        let (w, armed, held) = pausing_world(1500);
        w.unlock_new();
        let p = w.approved_repo("app");
        armed.store(true, Ordering::SeqCst);
        let import = |p: ProjectRef, value: &'static str| {
            move |e: &SecretsEngine| {
                e.dispatch(Request::SecretImport(SecretImport {
                    project: p,
                    entries: vec![ImportEntry {
                        name: "TOKEN".into(),
                        value: SecretValue::new(value.into()),
                    }],
                    overwrite: false,
                }))
            }
        };
        let a = in_thread(&w, import(p.clone(), "value-1"));
        wait_for(&held);
        let b = in_thread(&w, import(p.clone(), "value-2"));
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        assert!(a.is_ok());
        assert_eq!(b.err().map(|e| e.code), Some(ErrorCode::SecretExists));
        assert!(value_of(&w.materialize(&p, &["TOKEN"]).unwrap(), "TOKEN") == "value-1");
    }

    /// T6-2: an older lock is paused between its vault work and publishing the cache while an
    /// unlock runs; the cache ends with the vault's real state.
    #[test]
    fn lock_cache_follows_vault_order() {
        let mut w = world();
        w.unlock_new();
        let armed = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicBool::new(false));
        let (a, h) = (armed.clone(), held.clone());
        *w.engine.hook.lock().unwrap() = Some(Arc::new(move |point| {
            if point == "publish" && a.swap(false, Ordering::SeqCst) {
                h.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(800));
            }
        }));
        w.drain_events();
        armed.store(true, Ordering::SeqCst);
        let lock = in_thread(&w, |e| e.dispatch(Request::SecretsLock).map(|_| ()));
        wait_for(&held);
        let unlock = in_thread(&w, |e| {
            e.dispatch(Request::SecretsUnlock(Unlock {
                password: SecretValue::new(PASSWORD.into()),
                key_file: None,
                forget_key_file: false,
                create: false,
            }))
            .map(|_| ())
        });
        lock.join().unwrap().unwrap();
        unlock.join().unwrap().unwrap();
        let status: SecretsStatus = w.call_as(Request::SecretsStatus).unwrap();
        assert_eq!(status.state, LockState::Unlocked);
        let last = w.drain_events().into_iter().rev().find_map(|e| match e {
            Event::SecretsLockChanged(s) => Some(s.state),
            _ => None,
        });
        assert_eq!(last, Some(LockState::Unlocked));
    }

    /// T6-3: a set commits while a request sits between its check and its insert; the request
    /// still ends answered.
    #[test]
    fn request_racing_a_set_is_answered() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "OTHER", "value-other").unwrap();
        let armed = Arc::new(AtomicBool::new(true));
        let held = Arc::new(AtomicBool::new(false));
        let (a, h) = (armed.clone(), held.clone());
        *w.engine.hook.lock().unwrap() = Some(Arc::new(move |point| {
            if point == "request_checked" && a.swap(false, Ordering::SeqCst) {
                h.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(800));
            }
        }));
        let id = uuid::Uuid::new_v4().to_string();
        let requester = w.requester(42);
        let (rid, rp) = (id.clone(), p.clone());
        let request = in_thread(&w, move |e| {
            e.dispatch(Request::SecretRequest(SecretRequest {
                id: rid,
                project: rp,
                name: "TOKEN".into(),
                reason: "tests".into(),
                requester,
            }))
            .map(|_| ())
        });
        wait_for(&held);
        let sp = p.clone();
        let set = in_thread(&w, move |e| {
            e.dispatch(set_request(&sp, "TOKEN", "value-1"))
        });
        request.join().unwrap().unwrap();
        let Reply::Value(out) = set.join().unwrap().unwrap() else {
            panic!("set answers a value");
        };
        assert_eq!(out["answered_requests"], json!([id.clone()]));
        assert_eq!(
            w.request_id(&id, &p, "TOKEN", 42).unwrap().state,
            RequestState::Stored
        );
    }

    /// T6-4: the encoding buffer never grows: a write past its bound is refused and the
    /// allocation stays the one made up front.
    #[test]
    fn answer_encoding_never_reallocates() {
        let mut buf = Vec::with_capacity(8);
        let ptr = buf.as_ptr();
        {
            use std::io::Write;
            let mut w = Bounded::new(&mut buf);
            w.write_all(b"12345").unwrap();
            assert!(w.write_all(b"6789").is_err());
        }
        assert_eq!(buf, b"12345");
        assert_eq!((buf.as_ptr(), buf.capacity()), (ptr, 8));
        let big: Vec<MaterializedSecret> = (0..40)
            .map(|i| MaterializedSecret {
                name: format!("Q{i}"),
                value: SecretValue::new("\"".repeat(super::super::MAX_VALUE_BYTES)),
            })
            .collect();
        let e = encode_answer(&big).unwrap_err();
        assert_eq!(e.message, "the requested secrets are too large for one run");
        let small = encode_answer(&big[..1]).unwrap();
        assert!(small.len() > super::super::MAX_VALUE_BYTES);
    }

    /// L19: an outside value that breaks the value rules is not materialized.
    #[test]
    fn outside_value_breaking_the_rules_is_not_materialized() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "SHORT", "value-1").unwrap();
        w.set(&p, "CONTROL", "value-2").unwrap();
        let group = group_of(&w.project(&p)).unwrap();
        outside_write(&w.dir.path().join("hostler.kdbx"), |db| {
            layout::put(db, group, "SHORT", &SecretValue::new("ab".into()));
            layout::put(
                db,
                group,
                "CONTROL",
                &SecretValue::new("abc\u{1}defg".into()),
            );
        });
        for name in ["SHORT", "CONTROL"] {
            let e = w.materialize(&p, &[name]).unwrap_err();
            assert_eq!(e.code, ErrorCode::SecretsConflict);
            assert_eq!(
                e.details,
                json!({ "name": name, "reason": "invalid_value" })
            );
            assert_eq!(e.recovery, Some(format!("hostler secret set {name}")));
            assert!(!format!("{e:?}").contains("abc") && !e.message.contains("\"ab\""));
        }
        assert!(w.history().is_empty());
    }

    /// T6-7: S shares TOKEN into T, removes it (the link stays), T requests it, S sets it
    /// again: T's request is answered by that set, without a re-send.
    #[test]
    fn shared_in_request_is_answered_by_the_source_set() {
        let w = world();
        w.unlock_new();
        let s = w.approved_repo("source");
        let t = w.approved_repo("target");
        w.set(&s, "TOKEN", "value-1").unwrap();
        w.call(Request::SecretShare(Share {
            from: s.clone(),
            name: "TOKEN".into(),
            to_project_id: w.project(&t).id,
        }))
        .unwrap();
        w.call(Request::SecretRemove {
            project: s.clone(),
            name: "TOKEN".into(),
        })
        .unwrap();
        let r = w.request(&t, "TOKEN", 42);
        assert_eq!(r.state, RequestState::Pending);
        let out = w.set(&s, "TOKEN", "value-2").unwrap();
        assert_eq!(out.answered_requests, std::slice::from_ref(&r.id));
        let pending: Vec<SecretRequestInfo> = w.call_as(Request::SecretRequests).unwrap();
        assert!(pending.is_empty(), "{pending:?}");
        assert!(value_of(&w.materialize(&t, &["TOKEN"]).unwrap(), "TOKEN") == "value-2");
    }

    /// T6-7: a pending request whose name was set outside hostler is answered when it is
    /// re-sent — not while locked (no vault), and never once it is terminal.
    #[test]
    fn resent_request_is_answered_after_an_outside_set() {
        let w = world();
        w.unlock_new();
        let p = w.approved_repo("app");
        w.set(&p, "OTHER", "value-other").unwrap();
        let r = w.request(&p, "TOKEN", 42);
        let declined = w.request(&p, "LATER", 43);
        w.call(Request::SecretDecline(Decline {
            id: Some(declined.id.clone()),
            project: None,
            name: None,
        }))
        .unwrap();
        let group = group_of(&w.project(&p)).unwrap();
        outside_write(&w.dir.path().join("hostler.kdbx"), |db| {
            layout::put(db, group, "TOKEN", &SecretValue::new("outside-1".into()));
            layout::put(db, group, "LATER", &SecretValue::new("outside-2".into()));
        });
        w.call(Request::SecretsLock).unwrap();
        let locked = w.request_id(&r.id, &p, "TOKEN", 42).unwrap();
        assert_eq!(locked.state, RequestState::Pending);
        w.call(Request::SecretsUnlock(Unlock {
            password: SecretValue::new(PASSWORD.into()),
            key_file: None,
            forget_key_file: false,
            create: false,
        }))
        .unwrap();
        let again = w.request_id(&r.id, &p, "TOKEN", 42).unwrap();
        assert_eq!(again.state, RequestState::Stored);
        assert!(again.answered_at_ms.is_some());
        let terminal = w.request_id(&declined.id, &p, "LATER", 43).unwrap();
        assert_eq!(terminal.state, RequestState::Declined);
    }
}
