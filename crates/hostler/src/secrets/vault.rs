//! The KeePass vault: open, create, re-read, verified atomic saves (plan P2, Task 4).
//!
//! A save never replaces a file it has not verified (L5, L11): the new bytes go to a temp file
//! `T`; the current file `K` is moved aside to `P` with one plain `rename`; `P`'s SHA-256 is
//! checked against the bytes the save was built from; `T` takes the name `K` with a no-replace
//! rename; `P` is removed only when its bytes are still those. Every outside version that ever
//! stood at `K` is therefore still at `K`, in a kept `P`, or contained in hostler's save.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use keepass::config::{DatabaseVersion, KdfConfig};
use keepass::db::{History, Value, fields};
use keepass::error::{DatabaseKeyError, DatabaseOpenError};
use keepass::{Database, DatabaseKey};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::value::SecretValue;
use crate::contract::secrets::{LockState, errors};
use crate::contract::{ErrorCode, HostlerError};

/// The composite key; `keepass::DatabaseKey`'s own `Debug` prints the password.
pub struct VaultKey(DatabaseKey);

impl VaultKey {
    pub fn new(password: &SecretValue, key_file: Option<&Path>) -> Result<VaultKey, VaultError> {
        let mut key = DatabaseKey::new().with_password(password.expose());
        if let Some(path) = key_file {
            let mut file = File::open(path).map_err(|_| VaultError::KeyFileUnreadable)?;
            key = key
                .with_keyfile(&mut file)
                .map_err(|_| VaultError::KeyFileUnreadable)?;
        }
        Ok(VaultKey(key))
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultKey([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KdfChoice {
    /// Argon2id, 64 MiB, 2 lanes, iterations so that one derivation takes about `target`.
    Calibrated { target: Duration },
    /// Tests only: 2 iterations, 1 MiB, 1 lane.
    Fast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictReason {
    /// The file on disk no longer opens with the key hostler holds.
    Unreadable,
    Removed,
    /// An outside save landed during both attempts of one write.
    KeepsChanging,
    /// Windows: another program holds the file without delete sharing.
    FileInUse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultError {
    Locked,
    Missing,
    Exists,
    IncorrectKey,
    KeyFileUnreadable,
    Unsupported,
    Corrupt,
    Conflict(ConflictReason),
    /// The state directory's filesystem has neither a no-replace rename nor hard links (L10).
    NoReplaceUnsupported {
        dir: String,
    },
    Io {
        what: &'static str,
        kind: ErrorKind,
    },
    /// The new file is at `K`, but syncing the directory failed: the save may not survive a
    /// crash. Any previous version is kept as a prev file.
    NotDurable,
    /// The write closure refused; nothing was written.
    Rejected(HostlerError),
}

fn error(code: ErrorCode, message: &str, reason: &str, recovery: Option<&str>) -> HostlerError {
    HostlerError {
        code,
        message: message.into(),
        details: json!({ "reason": reason }),
        recovery: recovery.map(str::to_string),
    }
}

impl VaultError {
    /// Fixed texts only: never a `keepass` error's text.
    pub fn to_hostler(&self) -> HostlerError {
        match self {
            VaultError::Locked => errors::secrets_locked(false),
            VaultError::Missing => errors::secrets_locked(true),
            VaultError::Exists => errors::bad_request(
                "a hostler secrets database already exists; unlock it instead".into(),
            ),
            VaultError::IncorrectKey => errors::unlock_failed(),
            VaultError::KeyFileUnreadable => error(
                ErrorCode::UnlockFailed,
                "the key file cannot be read",
                "key_file_unreadable",
                Some("hostler unlock"),
            ),
            VaultError::Unsupported => error(
                ErrorCode::UnlockFailed,
                "hostler.kdbx is in a KeePass format hostler cannot open (KeePass 1 or a \
                 pre-release KDBX); save it as KDBX 4 in KeePassXC",
                "unsupported_format",
                None,
            ),
            VaultError::Corrupt => error(
                ErrorCode::UnlockFailed,
                "hostler.kdbx is not a readable KeePass database (damaged or not a KDBX file)",
                "corrupt",
                None,
            ),
            VaultError::Conflict(ConflictReason::Unreadable) => {
                errors::secrets_conflict("unreadable")
            }
            VaultError::Conflict(ConflictReason::Removed) => error(
                ErrorCode::SecretsConflict,
                "hostler.kdbx was removed outside hostler; nothing was written",
                "removed",
                Some("hostler unlock"),
            ),
            VaultError::Conflict(ConflictReason::KeepsChanging) => error(
                ErrorCode::SecretsConflict,
                "hostler.kdbx kept changing outside hostler (KeePassXC saving?) while hostler \
                 saved; the outside version is kept and nothing of this change was written; try \
                 again",
                "keeps_changing",
                None,
            ),
            VaultError::Conflict(ConflictReason::FileInUse) => error(
                ErrorCode::SecretsConflict,
                "another program holds hostler.kdbx open; nothing was written; close it and try \
                 again",
                "file_in_use",
                None,
            ),
            VaultError::NoReplaceUnsupported { dir } => HostlerError {
                code: ErrorCode::Internal,
                message: format!(
                    "the filesystem of hostler's state directory {dir} supports neither \
                     no-replace renames nor hard links, so hostler cannot save secrets there \
                     safely; nothing was written; put HOSTLER_HOME on a local disk"
                ),
                details: json!({ "reason": "no_replace_unsupported", "dir": dir }),
                recovery: None,
            },
            VaultError::NotDurable => error(
                ErrorCode::Internal,
                "hostler.kdbx was saved, but the system did not confirm that the save is on disk \
                 (syncing the state directory failed); any previous version is kept as a \
                 hostler.kdbx.prev-* file; check the disk before saving again",
                "not_durable",
                None,
            ),
            VaultError::Io { what, kind } => HostlerError {
                code: ErrorCode::Internal,
                message: format!("could not {what}: {kind}"),
                details: json!({ "reason": "io" }),
                recovery: None,
            },
            VaultError::Rejected(e) => e.clone(),
        }
    }
}

/// Named points of unlock and the write protocol for tests (env names: snake_case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookPoint {
    Unlock,
    BeforeMoveAside,
    MoveAsideRetry,
    AfterMoveAside,
    BeforePlace,
    PlaceRetry,
    BeforeRemovePrev,
    /// The directory sync after a file was placed at `K`.
    SyncDir,
}

impl HookPoint {
    #[cfg(debug_assertions)]
    fn from_name(s: &str) -> Option<HookPoint> {
        Some(match s {
            "unlock" => HookPoint::Unlock,
            "before_move_aside" => HookPoint::BeforeMoveAside,
            "move_aside_retry" => HookPoint::MoveAsideRetry,
            "after_move_aside" => HookPoint::AfterMoveAside,
            "before_place" => HookPoint::BeforePlace,
            "place_retry" => HookPoint::PlaceRetry,
            "before_remove_prev" => HookPoint::BeforeRemovePrev,
            "sync_dir" => HookPoint::SyncDir,
            _ => return None,
        })
    }
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct TestHooks {
    hook: Option<Box<dyn Fn(HookPoint) + Send>>,
    pause: Option<(HookPoint, Duration)>,
    fail: Option<HookPoint>,
    force_link: bool,
}

struct Open {
    db: Database,
    key: VaultKey,
    /// SHA-256 of the bytes `db` came from (or was saved as).
    fingerprint: [u8; 32],
}

pub struct Vault {
    path: PathBuf,
    open: Option<Open>,
    #[cfg(debug_assertions)]
    test: TestHooks,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("state", &self.state())
            .finish()
    }
}

const RETRIES: u32 = 10;
const RETRY_WAIT: Duration = Duration::from_millis(200);

enum Attempt {
    /// `T` is at `K`; the prev file still has to be checked and removed once `durable`.
    Saved { prev: PathBuf, durable: bool },
    /// An outside version is at `K` (or kept); refresh and try again.
    OutsideChanged,
}

impl Vault {
    pub fn new(path: PathBuf) -> Vault {
        Vault {
            path,
            open: None,
            #[cfg(debug_assertions)]
            test: TestHooks {
                pause: std::env::var("HOSTLER_TEST_VAULT_PAUSE")
                    .ok()
                    .and_then(|v| {
                        let (point, ms) = v.split_once(':')?;
                        Some((
                            HookPoint::from_name(point)?,
                            Duration::from_millis(ms.parse().ok()?),
                        ))
                    }),
                fail: std::env::var("HOSTLER_TEST_VAULT_FAIL")
                    .ok()
                    .and_then(|v| HookPoint::from_name(&v)),
                ..TestHooks::default()
            },
        }
    }

    pub fn state(&self) -> LockState {
        if self.open.is_some() {
            LockState::Unlocked
        } else if exists(&self.path) {
            LockState::Locked
        } else {
            LockState::Missing
        }
    }

    /// Names of the kept `hostler.kdbx.prev-*` files, sorted.
    pub fn preserved_files(&self) -> Vec<String> {
        let mut names: Vec<String> = siblings(&self.path, "prev")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    pub fn create(&mut self, key: VaultKey, kdf: KdfChoice) -> Result<(), VaultError> {
        if exists(&self.path) {
            return Err(VaultError::Exists);
        }
        let mut db = Database::new();
        db.config.kdf_config = kdf_config(kdf);
        db.config.version = DatabaseVersion::KDB4(1);
        db.meta.database_name = Some("hostler".into());
        let bytes = encrypt(&db, &key)?;
        let t = sibling(&self.path, "tmp");
        let _t_guard = RemoveOnDrop(&t);
        write_new_file(&t, &bytes).map_err(io_error("write a temporary file"))?;
        self.no_replace(&t, &self.path)
            .map_err(|e| match e.kind() {
                ErrorKind::AlreadyExists => VaultError::Exists,
                _ => self.rename_error("create the database", e),
            })?;
        let durable = self.sync_dir();
        // The loaded state is what stands at `K`, durable or not.
        self.open = Some(Open {
            db,
            key,
            fingerprint: sha256(&bytes),
        });
        if durable {
            Ok(())
        } else {
            Err(VaultError::NotDurable)
        }
    }

    /// Opens the file with `key`. A failed unlock leaves the current state as it was.
    pub fn unlock(&mut self, key: VaultKey) -> Result<(), VaultError> {
        self.at(HookPoint::Unlock)?;
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(VaultError::Missing),
            Err(e) => return Err(io_error("read the database")(e)),
        };
        let db = open_db(&bytes, &key)?;
        self.open = Some(Open {
            db,
            key,
            fingerprint: sha256(&bytes),
        });
        Ok(())
    }

    /// Drops the database and the key (both erase themselves on drop).
    pub fn lock(&mut self) {
        self.open = None;
    }

    pub fn read<T>(&mut self, f: impl FnOnce(&Database) -> T) -> Result<T, VaultError> {
        self.refresh()?;
        let open = self.open.as_ref().ok_or(VaultError::Locked)?;
        Ok(f(&open.db))
    }

    /// `f` runs on a fresh clone of the database as it is on disk, once per attempt; all
    /// preconditions belong inside `f`. `Err` aborts with `Rejected` and writes nothing.
    pub fn write<T>(
        &mut self,
        f: impl Fn(&mut Database) -> Result<T, HostlerError>,
    ) -> Result<T, VaultError> {
        for _attempt in 0..2 {
            self.refresh()?;
            let open = self.open.as_ref().ok_or(VaultError::Locked)?;
            let f0 = open.fingerprint;
            let mut db1 = open.db.clone();
            let out = f(&mut db1).map_err(VaultError::Rejected)?;
            db1.config.version = DatabaseVersion::KDB4(1);
            let bytes = encrypt(&db1, &open.key)?;
            match self.save_attempt(&bytes, &f0) {
                Ok(Attempt::Saved { prev, durable }) => {
                    if let Some(open) = self.open.as_mut() {
                        open.db = db1;
                        open.fingerprint = sha256(&bytes);
                    }
                    if !durable {
                        // `K` holds the new save: nothing moves back over it, and `P` stays.
                        return Err(VaultError::NotDurable);
                    }
                    // The save is complete: a failure from here on only keeps `P`.
                    if self.at(HookPoint::BeforeRemovePrev).is_ok() {
                        remove_if_unchanged(&prev, &f0);
                    }
                    return Ok(out);
                }
                Ok(Attempt::OutsideChanged) => continue,
                Err(e) => {
                    if e == VaultError::Conflict(ConflictReason::Removed) {
                        self.lock();
                    }
                    return Err(e);
                }
            }
        }
        Err(VaultError::Conflict(ConflictReason::KeepsChanging))
    }

    #[cfg(debug_assertions)]
    pub fn set_test_hook(&mut self, hook: Box<dyn Fn(HookPoint) + Send>) {
        self.test.hook = Some(hook);
    }

    /// Makes `no_replace_rename` take the `hard_link` fallback (tests).
    #[cfg(debug_assertions)]
    pub fn force_link_fallback(&mut self) {
        self.test.force_link = true;
    }

    /// Re-reads the file when its SHA-256 differs from the loaded one.
    fn refresh(&mut self) -> Result<(), VaultError> {
        let open = self.open.as_mut().ok_or(VaultError::Locked)?;
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                self.lock();
                return Err(VaultError::Conflict(ConflictReason::Removed));
            }
            Err(e) => return Err(io_error("read the database")(e)),
        };
        let fingerprint = sha256(&bytes);
        if fingerprint == open.fingerprint {
            return Ok(());
        }
        match open_db(&bytes, &open.key) {
            Ok(db) => {
                open.db = db;
                open.fingerprint = fingerprint;
                Ok(())
            }
            Err(_) => {
                self.lock();
                Err(VaultError::Conflict(ConflictReason::Unreadable))
            }
        }
    }

    /// Steps 3–7 of the protocol for one attempt. `T` is removed on every path (guard).
    fn save_attempt(&self, bytes: &[u8], f0: &[u8; 32]) -> Result<Attempt, VaultError> {
        let k = &self.path;
        let t = sibling(k, "tmp");
        let _t_guard = RemoveOnDrop(&t);
        // Written under a second temp name and moved to `T` with the same no-replace rename the
        // later steps use: an unsupported filesystem fails here, before `K` was moved.
        let staging = sibling(k, "tmp");
        let _staging_guard = RemoveOnDrop(&staging);
        write_new_file(&staging, bytes).map_err(io_error("write a temporary file"))?;
        self.no_replace(&staging, &t)
            .map_err(|e| self.rename_error("write a temporary file", e))?;
        self.at(HookPoint::BeforeMoveAside)?;
        let p = sibling(k, "prev");
        self.move_aside(&p)?;
        match self.after_move_aside(&t, &p, f0) {
            Ok(attempt) => Ok(attempt),
            Err(e) => {
                // Put the moved file back unless something else stands at `K` now; then `P` is
                // kept (and recovered at the next start if `K` is still missing).
                let _ = self.no_replace(&p, k);
                Err(e)
            }
        }
    }

    /// Step 4: one plain atomic rename moves whatever file stands at `K` (never link + unlink).
    fn move_aside(&self, p: &Path) -> Result<(), VaultError> {
        let mut retries = 0;
        loop {
            match fs::rename(&self.path, p) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    return Err(VaultError::Conflict(ConflictReason::Removed));
                }
                Err(e) if in_use(&e) && retries < RETRIES => {
                    retries += 1;
                    self.at(HookPoint::MoveAsideRetry)?;
                    std::thread::sleep(RETRY_WAIT);
                }
                Err(e) if in_use(&e) => {
                    return Err(VaultError::Conflict(ConflictReason::FileInUse));
                }
                Err(e) => return Err(io_error("move the database aside")(e)),
            }
        }
    }

    /// Steps 5–6. An `Err` makes the caller put `P` back.
    fn after_move_aside(&self, t: &Path, p: &Path, f0: &[u8; 32]) -> Result<Attempt, VaultError> {
        self.at(HookPoint::AfterMoveAside)?;
        let moved = file_sha256(p).map_err(io_error("check the moved-aside database"))?;
        if moved != *f0 {
            // An outside save landed before the move (or a write into `P`): put it back, or
            // keep it when a newer outside file already stands at `K`.
            match self.no_replace(p, &self.path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(self.rename_error("put the outside version back", e)),
            }
            return Ok(Attempt::OutsideChanged);
        }
        self.at(HookPoint::BeforePlace)?;
        let mut retries = 0;
        loop {
            match self.no_replace(t, &self.path) {
                Ok(()) => break,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    // An outside writer created `K` while it was aside: `K` is theirs, and `P`
                    // holds `F0`, which the refreshed database already contains.
                    remove_if_unchanged(p, f0);
                    return Ok(Attempt::OutsideChanged);
                }
                Err(e) if in_use(&e) && retries < RETRIES => {
                    retries += 1;
                    self.at(HookPoint::PlaceRetry)?;
                    std::thread::sleep(RETRY_WAIT);
                }
                Err(e) if in_use(&e) => {
                    return Err(VaultError::Conflict(ConflictReason::FileInUse));
                }
                Err(e) => return Err(self.rename_error("place the new database", e)),
            }
        }
        Ok(Attempt::Saved {
            prev: p.to_path_buf(),
            durable: self.sync_dir(),
        })
    }

    /// Makes the rename to `K` durable (Unix: fsync the directory; Windows: the rename itself
    /// is `MOVEFILE_WRITE_THROUGH`). False when that failed.
    fn sync_dir(&self) -> bool {
        if self.at(HookPoint::SyncDir).is_err() {
            return false;
        }
        #[cfg(unix)]
        if let Err(e) = File::open(dir_of(&self.path)).and_then(|d| d.sync_all()) {
            tracing::error!(error_kind = %e.kind(), "could not sync the state directory");
            return false;
        }
        true
    }

    fn no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(debug_assertions)]
        let force_link = self.test.force_link;
        #[cfg(not(debug_assertions))]
        let force_link = false;
        rename_no_replace(from, to, force_link)
    }

    fn rename_error(&self, what: &'static str, e: io::Error) -> VaultError {
        if e.kind() == ErrorKind::Unsupported {
            VaultError::NoReplaceUnsupported {
                dir: dir_of(&self.path).display().to_string(),
            }
        } else {
            io_error(what)(e)
        }
    }

    /// The test hook, then the env pause, then the env failure.
    fn at(&self, point: HookPoint) -> Result<(), VaultError> {
        #[cfg(debug_assertions)]
        {
            if let Some(hook) = &self.test.hook {
                hook(point);
            }
            if let Some((p, wait)) = self.test.pause
                && p == point
            {
                std::thread::sleep(wait);
            }
            if self.test.fail == Some(point) {
                return Err(VaultError::Io {
                    what: "pass a test failure point",
                    kind: ErrorKind::Other,
                });
            }
        }
        #[cfg(not(debug_assertions))]
        let _ = point;
        Ok(())
    }
}

/// `rename(from, to)` that fails with `AlreadyExists` instead of replacing `to`. Used only with
/// hostler's own files as the source (`T → K`, `P → K`), never with `K` as the source (L11).
pub fn no_replace_rename(from: &Path, to: &Path) -> io::Result<()> {
    rename_no_replace(from, to, false)
}

fn rename_no_replace(from: &Path, to: &Path, force_link: bool) -> io::Result<()> {
    if !force_link {
        match native_no_replace(from, to) {
            Err(e) if rename_unsupported(&e) => {}
            other => return other,
        }
    }
    link_then_unlink(from, to)
}

/// The fallback: `hard_link` refuses an existing `to`; `Unsupported` when links are unsupported.
fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => {
            // `to` already holds the file; a `from` that cannot be removed is only a second name.
            let _ = fs::remove_file(from);
            Ok(())
        }
        Err(e) if link_unsupported(&e) => Err(ErrorKind::Unsupported.into()),
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "linux")]
fn native_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_vendor = "apple")]
fn native_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let r = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn native_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both are valid NUL-terminated wide strings for the duration of the call.
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    if ok != 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(c) if c == ERROR_ALREADY_EXISTS as i32 || c == ERROR_FILE_EXISTS as i32 => {
            Err(ErrorKind::AlreadyExists.into())
        }
        _ => Err(e),
    }
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn native_no_replace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn c_path(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| ErrorKind::InvalidInput.into())
}

fn rename_unsupported(e: &io::Error) -> bool {
    if e.kind() == ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    let codes = [libc::EINVAL, libc::ENOSYS, libc::ENOTSUP, libc::EOPNOTSUPP];
    #[cfg(windows)]
    let codes = [
        windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED as i32,
        windows_sys::Win32::Foundation::ERROR_INVALID_FUNCTION as i32,
    ];
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

fn link_unsupported(e: &io::Error) -> bool {
    if e.kind() == ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    let codes = [libc::EPERM, libc::ENOTSUP, libc::EOPNOTSUPP];
    #[cfg(windows)]
    let codes = [
        windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED as i32,
        windows_sys::Win32::Foundation::ERROR_INVALID_FUNCTION as i32,
    ];
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// Windows: a sharing violation or access denied while another program holds the file.
fn in_use(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
        matches!(e.raw_os_error(), Some(c) if c == ERROR_SHARING_VIOLATION as i32
            || c == ERROR_ACCESS_DENIED as i32)
    }
    #[cfg(not(windows))]
    {
        let _ = e;
        false
    }
}

pub struct Recovery {
    pub restored: bool,
    pub removed_temps: usize,
    pub preserved: Vec<String>,
}

/// At service start: removes temp files; a missing `K` gets the newest prev file back (no
/// replace); the other prev files stay and are reported.
pub fn recover_at_start(kdbx: &Path) -> Recovery {
    let mut removed_temps = 0;
    for (_, path) in siblings(kdbx, "tmp") {
        if fs::remove_file(&path).is_ok() {
            removed_temps += 1;
        }
    }
    let mut prevs = siblings(kdbx, "prev");
    let mut restored = false;
    if !exists(kdbx) && !prevs.is_empty() {
        prevs.sort_by_key(|(_, path)| fs::metadata(path).and_then(|m| m.modified()).ok());
        if let Some((_, newest)) = prevs.last()
            && no_replace_rename(newest, kdbx).is_ok()
        {
            restored = true;
            prevs.pop();
        }
    }
    let mut preserved: Vec<String> = prevs.into_iter().map(|(name, _)| name).collect();
    preserved.sort();
    Recovery {
        restored,
        removed_temps,
        preserved,
    }
}

fn open_db(bytes: &[u8], key: &VaultKey) -> Result<Database, VaultError> {
    match DatabaseVersion::parse(bytes) {
        Ok(DatabaseVersion::KDB(_) | DatabaseVersion::KDB2(_)) => {
            return Err(VaultError::Unsupported);
        }
        Ok(_) => {}
        Err(_) => return Err(VaultError::Corrupt),
    }
    let mut db = Database::parse(bytes, key.0.clone()).map_err(|e| match e {
        DatabaseOpenError::Key(DatabaseKeyError::IncorrectKey | DatabaseKeyError::EmptyKey) => {
            VaultError::IncorrectKey
        }
        // KDBX 3.x: a wrong key shows as bad AES-CBC padding of the payload (verified with the
        // KeePassXC fixtures); 4.x checks the key with the header HMAC first.
        DatabaseOpenError::Cryptography(_) => VaultError::IncorrectKey,
        DatabaseOpenError::Key(_) => VaultError::KeyFileUnreadable,
        DatabaseOpenError::UnsupportedVersion => VaultError::Unsupported,
        _ => VaultError::Corrupt,
    })?;
    protect_passwords(&mut db);
    Ok(db)
}

/// Another client may store a `Password` unprotected: a plain `String` that is neither erased on
/// drop nor redacted. Every loaded database moves each `Password` (entries and their history)
/// into protected storage before it is kept or cloned.
fn protect_passwords(db: &mut Database) {
    fn protect(entry: &mut keepass::db::Entry) {
        if let Some(Value::Unprotected(_)) = entry.fields.get(fields::PASSWORD)
            && let Some(Value::Unprotected(v)) = entry.fields.remove(fields::PASSWORD)
        {
            entry
                .fields
                .insert(fields::PASSWORD.into(), Value::protected(v));
        }
    }
    db.foreach_entry_mut(|mut entry| {
        protect(&mut entry);
        let needs = entry.history.as_ref().is_some_and(|h| {
            h.get_entries()
                .iter()
                .any(|e| matches!(e.fields.get(fields::PASSWORD), Some(Value::Unprotected(_))))
        });
        if let Some(old) = entry.history.take_if(|_| needs) {
            // keepass gives no mutable access to history versions: rebuild the history from
            // protected copies (same order); the parsed originals are freed unerased, like
            // keepass's other parse-time buffers.
            let mut history = History::default();
            for version in old.get_entries().iter().rev() {
                let mut version = version.clone();
                protect(&mut version);
                history.add_entry(version);
            }
            entry.history = Some(history);
        }
    });
}

/// Runs the KDF.
fn encrypt(db: &Database, key: &VaultKey) -> Result<Vec<u8>, VaultError> {
    let mut out = Vec::new();
    db.save(&mut out, key.0.clone())
        .map_err(|_| VaultError::Io {
            what: "encrypt the database",
            kind: ErrorKind::Other,
        })?;
    Ok(out)
}

fn kdf_config(kdf: KdfChoice) -> KdfConfig {
    let (iterations, memory, parallelism) = match kdf {
        KdfChoice::Calibrated { target } => (calibrated_iterations(target), 64 << 20, 2),
        KdfChoice::Fast => (2, 1 << 20, 1),
    };
    KdfConfig::Argon2id {
        iterations,
        memory,
        parallelism,
        version: argon2::Version::Version13,
    }
}

/// Times one Argon2id derivation at 2 iterations, 64 MiB, 2 lanes and scales it to `target`.
fn calibrated_iterations(target: Duration) -> u64 {
    let config = argon2::Config {
        variant: argon2::Variant::Argon2id,
        version: argon2::Version::Version13,
        mem_cost: 65536,
        time_cost: 2,
        lanes: 2,
        thread_mode: argon2::ThreadMode::Parallel,
        secret: &[],
        ad: &[],
        hash_length: 32,
    };
    let start = Instant::now();
    let _ = argon2::hash_raw(b"hostler-kdf-calibration", &[0u8; 16], &config);
    let per_iteration = start.elapsed().as_secs_f64() / 2.0;
    let iterations = target.as_secs_f64() / per_iteration.max(1e-6);
    (iterations.round() as u64).clamp(10, 200)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn file_sha256(path: &Path) -> io::Result<[u8; 32]> {
    fs::read(path).map(|b| sha256(&b))
}

/// `P` is removed only when its bytes are still `F0` (contained in the save); else it is kept.
fn remove_if_unchanged(p: &Path, f0: &[u8; 32]) {
    if file_sha256(p).is_ok_and(|h| h == *f0) {
        let _ = fs::remove_file(p);
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn exists(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(e) if e.kind() == ErrorKind::NotFound)
}

fn dir_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `<dir>/<name>.<kind>-<uuid>`
fn sibling(path: &Path, kind: &str) -> PathBuf {
    path.with_file_name(format!(
        "{}.{kind}-{}",
        file_name(path),
        uuid::Uuid::new_v4()
    ))
}

/// Existing `<name>.<kind>-*` files next to `path`.
fn siblings(path: &Path, kind: &str) -> Vec<(String, PathBuf)> {
    let prefix = format!("{}.{kind}-", file_name(path));
    let Ok(entries) = fs::read_dir(dir_of(path)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix).then(|| (name, e.path()))
        })
        .collect()
}

fn io_error(what: &'static str) -> impl Fn(io::Error) -> VaultError {
    move |e| VaultError::Io {
        what,
        kind: e.kind(),
    }
}

struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use keepass::db::GroupId;

    use super::*;
    use crate::secrets::layout::{self, Lookup};

    const PW: &str = "vault-test-pass";
    const FIXTURE_PW: &str = "fixture-pass-1";

    fn key_with(pw: &str, key_file: Option<&Path>) -> VaultKey {
        VaultKey::new(&SecretValue::new(pw.into()), key_file).unwrap()
    }

    fn key() -> VaultKey {
        key_with(PW, None)
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/kdbx")
            .join(name)
    }

    fn kdbx(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("hostler.kdbx")
    }

    fn created(dir: &tempfile::TempDir) -> Vault {
        let mut v = Vault::new(kdbx(dir));
        v.create(key(), KdfChoice::Fast).unwrap();
        v
    }

    fn root(db: &Database) -> GroupId {
        db.root().id()
    }

    fn set(v: &mut Vault, name: &'static str, value: &'static str) {
        v.write(move |db| {
            let g = root(db);
            layout::put(db, g, name, &SecretValue::new(value.into()));
            Ok(())
        })
        .unwrap();
    }

    fn names(v: &mut Vault) -> Vec<String> {
        v.read(|db| layout::names(db, root(db))).unwrap()
    }

    /// Names in the database file at `path`, opened with the test key.
    fn names_in_file(path: &Path) -> Vec<String> {
        let db = Database::parse(&fs::read(path).unwrap(), key().0).unwrap();
        layout::names(&db, root(&db))
    }

    /// Every file in the directory, sorted.
    fn dir_files(dir: &tempfile::TempDir) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn only_prev(k: &Path) -> PathBuf {
        let prevs = siblings(k, "prev");
        assert_eq!(prevs.len(), 1, "expected one prev file");
        prevs[0].1.clone()
    }

    /// An outside version the way KeePassXC saves: `source` plus entry `entry`, written to a temp
    /// file and renamed over `K`.
    fn outside_save(k: &Path, source: &Path, entry: &str) {
        let bytes = outside_bytes(source, entry);
        let tmp = k.with_file_name("outside-save.tmp");
        fs::write(&tmp, &bytes).unwrap();
        fs::rename(&tmp, k).unwrap();
    }

    fn outside_bytes(source: &Path, entry: &str) -> Vec<u8> {
        let mut db = Database::parse(&fs::read(source).unwrap(), key().0).unwrap();
        let g = root(&db);
        layout::put(&mut db, g, entry, &SecretValue::new("outside-value".into()));
        db.config.version = DatabaseVersion::KDB4(1);
        let mut out = Vec::new();
        db.save(&mut out, key().0).unwrap();
        out
    }

    /// The file that holds the current version while `K` may be aside.
    fn current_source(k: &Path) -> PathBuf {
        if k.exists() {
            k.to_path_buf()
        } else {
            only_prev(k)
        }
    }

    /// A hook that runs `act` at `point` on its first `times` calls there.
    fn hook_at(
        point: HookPoint,
        times: usize,
        act: impl Fn(usize) + Send + 'static,
    ) -> Box<dyn Fn(HookPoint) + Send> {
        let calls = AtomicUsize::new(0);
        Box::new(move |p| {
            if p == point {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < times {
                    act(n);
                }
            }
        })
    }

    fn header_version(path: &Path) -> [u8; 4] {
        let b = fs::read(path).unwrap();
        [b[8], b[9], b[10], b[11]]
    }

    fn group_named(db: &Database, name: &str) -> GroupId {
        db.root().groups().find(|g| g.name == name).unwrap().id()
    }

    fn value_of(l: Lookup) -> String {
        match l {
            Lookup::Value(v) => v.expose().to_string(),
            Lookup::Missing => "<missing>".into(),
            Lookup::Duplicate => "<duplicate>".into(),
        }
    }

    // ---- create, unlock, KeePassXC files ----

    #[test]
    fn create_unlock_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::new(kdbx(&dir));
        assert_eq!(v.state(), LockState::Missing);
        assert_eq!(v.read(|_| ()).unwrap_err(), VaultError::Locked);
        v.create(key(), KdfChoice::Fast).unwrap();
        assert_eq!(v.state(), LockState::Unlocked);
        set(&mut v, "API_KEY", "v1-value");
        v.lock();
        assert_eq!(v.state(), LockState::Locked);
        assert_eq!(v.read(|_| ()).unwrap_err(), VaultError::Locked);
        v.unlock(key()).unwrap();
        let (name, value) = v
            .read(|db| {
                (
                    db.meta.database_name.clone(),
                    value_of(layout::lookup(db, root(db), "API_KEY")),
                )
            })
            .unwrap();
        assert_eq!(name.as_deref(), Some("hostler"));
        assert!(value == "v1-value");
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
        assert_eq!(header_version(&kdbx(&dir)), [1, 0, 4, 0]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(kdbx(&dir)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn create_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(kdbx(&dir), b"something").unwrap();
        let mut v = Vault::new(kdbx(&dir));
        assert_eq!(v.state(), LockState::Locked);
        assert_eq!(v.create(key(), KdfChoice::Fast), Err(VaultError::Exists));
        assert_eq!(fs::read(kdbx(&dir)).unwrap(), b"something");
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }

    #[test]
    fn unlock_of_a_missing_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::new(kdbx(&dir));
        assert_eq!(v.unlock(key()), Err(VaultError::Missing));
    }

    #[test]
    fn wrong_password_is_incorrect_key() {
        let dir = tempfile::tempdir().unwrap();
        created(&dir).lock();
        let mut v = Vault::new(kdbx(&dir));
        assert_eq!(
            v.unlock(key_with("wrong-pass", None)),
            Err(VaultError::IncorrectKey)
        );
        assert_eq!(v.state(), LockState::Locked);
        assert_eq!(
            VaultError::IncorrectKey.to_hostler().code,
            ErrorCode::UnlockFailed
        );
    }

    #[test]
    fn calibration_is_clamped() {
        assert_eq!(calibrated_iterations(Duration::from_millis(1)), 10);
        assert_eq!(calibrated_iterations(Duration::from_secs(3600)), 200);
        match kdf_config(KdfChoice::Fast) {
            KdfConfig::Argon2id {
                iterations,
                memory,
                parallelism,
                ..
            } => assert_eq!((iterations, memory, parallelism), (2, 1 << 20, 1)),
            other => panic!("{other:?}"),
        }
    }

    fn copy_fixture(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        let k = kdbx(dir);
        fs::copy(fixture(name), &k).unwrap();
        k
    }

    #[test]
    fn keepassxc_3_1_aes_fixture_opens_and_saves_as_4_1() {
        let dir = tempfile::tempdir().unwrap();
        let k = copy_fixture(&dir, "kxc-3.1-aes.kdbx");
        assert_eq!(header_version(&k), [1, 0, 3, 0]);
        let mut v = Vault::new(k.clone());
        v.unlock(key_with(FIXTURE_PW, None)).unwrap();
        let token = v
            .read(|db| value_of(layout::lookup(db, group_named(db, "fixture"), "TOKEN")))
            .unwrap();
        assert!(token == "fixture-token-value");
        v.write(|db| {
            let g = group_named(db, "fixture");
            layout::put(
                db,
                g,
                "FROM_HOSTLER",
                &SecretValue::new("hostler-value".into()),
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(header_version(&k), [1, 0, 4, 0]);
        let mut again = Vault::new(k);
        again.unlock(key_with(FIXTURE_PW, None)).unwrap();
        let both = again
            .read(|db| layout::names(db, group_named(db, "fixture")))
            .unwrap();
        assert_eq!(both, vec!["FROM_HOSTLER", "TOKEN"]);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }

    #[test]
    fn keepassxc_4_0_fixture_opens_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let k = copy_fixture(&dir, "kxc-4.0-argon2id.kdbx");
        assert_eq!(header_version(&k), [0, 0, 4, 0]);
        let mut v = Vault::new(k.clone());
        v.unlock(key_with(FIXTURE_PW, None)).unwrap();
        let (api, token) = v
            .read(|db| {
                let g = group_named(db, "proj-a");
                (
                    value_of(layout::lookup(db, g, "API_KEY")),
                    value_of(layout::lookup(db, g, "TOKEN")),
                )
            })
            .unwrap();
        assert!(api == "s3cr3t-value" && token == "fixture-token-value");
        v.write(|db| {
            let g = group_named(db, "proj-a");
            assert!(layout::remove(db, g, "API_KEY"));
            Ok(())
        })
        .unwrap();
        assert_eq!(header_version(&k)[..2], [1, 0]);
        let mut again = Vault::new(k);
        again.unlock(key_with(FIXTURE_PW, None)).unwrap();
        let left = again
            .read(|db| layout::names(db, group_named(db, "proj-a")))
            .unwrap();
        assert_eq!(left, vec!["TOKEN"]);
    }

    #[test]
    fn wrong_password_on_a_kdbx_3_1_fixture_is_incorrect_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::new(copy_fixture(&dir, "kxc-3.1-aes.kdbx"));
        assert_eq!(
            v.unlock(key_with("wrong-pass", None)),
            Err(VaultError::IncorrectKey)
        );
    }

    #[test]
    fn key_file_fixture_needs_the_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let k = copy_fixture(&dir, "kxc-keyfile.kdbx");
        let mut v = Vault::new(k);
        assert_eq!(
            v.unlock(key_with(FIXTURE_PW, None)),
            Err(VaultError::IncorrectKey)
        );
        assert_eq!(
            VaultKey::new(
                &SecretValue::new(FIXTURE_PW.into()),
                Some(&dir.path().join("no-such.keyx"))
            )
            .unwrap_err(),
            VaultError::KeyFileUnreadable
        );
        v.unlock(key_with(FIXTURE_PW, Some(&fixture("kxc-keyfile.keyx"))))
            .unwrap();
        let token = v
            .read(|db| value_of(layout::lookup(db, group_named(db, "fixture"), "TOKEN")))
            .unwrap();
        assert!(token == "fixture-token-value");
    }

    #[test]
    fn errors_never_quote_keepass() {
        let dir = tempfile::tempdir().unwrap();
        let k = kdbx(&dir);
        let bytes = fs::read(fixture("kxc-4.0-argon2id.kdbx")).unwrap();
        fs::write(&k, &bytes[..bytes.len() / 2]).unwrap();
        let mut v = Vault::new(k.clone());
        let e = v.unlock(key_with(FIXTURE_PW, None)).unwrap_err();
        assert_eq!(e, VaultError::Corrupt);
        let h = e.to_hostler();
        assert_eq!(
            h.message,
            "hostler.kdbx is not a readable KeePass database (damaged or not a KDBX file)"
        );
        // Garbage and an empty file are corrupt too; a KeePass 1 header is unsupported.
        fs::write(&k, b"not a database at all").unwrap();
        assert_eq!(v.unlock(key()), Err(VaultError::Corrupt));
        fs::write(&k, b"").unwrap();
        assert_eq!(v.unlock(key()), Err(VaultError::Corrupt));
        let mut kdb = vec![0x03, 0xd9, 0xa2, 0x9a, 0x65, 0xfb, 0x4b, 0xb5, 0, 0, 0, 0];
        kdb.extend_from_slice(&[0; 128]);
        fs::write(&k, &kdb).unwrap();
        assert_eq!(v.unlock(key()), Err(VaultError::Unsupported));
        // No message names a keepass error or holds a value or the password.
        let all = [
            VaultError::Locked,
            VaultError::Missing,
            VaultError::Exists,
            VaultError::IncorrectKey,
            VaultError::KeyFileUnreadable,
            VaultError::Unsupported,
            VaultError::Corrupt,
            VaultError::Conflict(ConflictReason::Unreadable),
            VaultError::Conflict(ConflictReason::Removed),
            VaultError::Conflict(ConflictReason::KeepsChanging),
            VaultError::Conflict(ConflictReason::FileInUse),
            VaultError::NoReplaceUnsupported {
                dir: "/state".into(),
            },
            VaultError::NotDurable,
            VaultError::Io {
                what: "read the database",
                kind: ErrorKind::PermissionDenied,
            },
        ];
        for e in all {
            let text = format!("{:?} {:?}", e, e.to_hostler());
            for bad in [
                "fixture-pass",
                "fixture-token",
                "s3cr3t",
                "Incorrect key",
                "XML",
            ] {
                assert!(!text.contains(bad), "{bad} in an error text");
            }
        }
        let codes = [
            (
                VaultError::Conflict(ConflictReason::KeepsChanging),
                "keeps_changing",
            ),
            (VaultError::Conflict(ConflictReason::Removed), "removed"),
            (
                VaultError::Conflict(ConflictReason::Unreadable),
                "unreadable",
            ),
            (
                VaultError::Conflict(ConflictReason::FileInUse),
                "file_in_use",
            ),
        ];
        for (e, reason) in codes {
            let h = e.to_hostler();
            assert_eq!(h.code, ErrorCode::SecretsConflict);
            assert_eq!(h.details["reason"], reason);
        }
        let rejected = errors::secret_exists(&["A".into()]);
        assert_eq!(
            VaultError::Rejected(rejected.clone()).to_hostler(),
            rejected
        );
    }

    #[test]
    fn vault_debug_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "API_KEY", "hunter2-canary-value");
        let k = key();
        assert_eq!(format!("{k:?}"), "VaultKey([redacted])");
        let printed = format!("{v:?}");
        assert!(printed.contains("Unlocked"), "{printed}");
        for bad in [PW, "hunter2"] {
            assert!(!printed.contains(bad));
        }
        // keepass's own Debug of the key prints the password; ours never reaches it.
        assert!(format!("{:?}", k.0).contains(PW));
    }

    // ---- refresh ----

    #[test]
    fn outside_change_is_reread() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = created(&dir);
        let mut b = Vault::new(kdbx(&dir));
        b.unlock(key()).unwrap();
        set(&mut b, "FROM_B", "value-of-b");
        assert_eq!(names(&mut a), vec!["FROM_B"]);
        // And `a` writes on top of `b`'s version.
        set(&mut a, "FROM_A", "value-of-a");
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["FROM_A", "FROM_B"]);
    }

    #[test]
    fn outside_password_change_is_conflict_and_locks() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "API_KEY", "v1-value");
        let mut db = Database::parse(&fs::read(kdbx(&dir)).unwrap(), key().0).unwrap();
        db.config.version = DatabaseVersion::KDB4(1);
        let mut out = Vec::new();
        db.save(&mut out, key_with("a-new-master", None).0).unwrap();
        fs::write(kdbx(&dir), &out).unwrap();
        assert_eq!(
            v.read(|_| ()).unwrap_err(),
            VaultError::Conflict(ConflictReason::Unreadable)
        );
        assert_eq!(v.state(), LockState::Locked);
        assert_eq!(fs::read(kdbx(&dir)).unwrap(), out);
        // A write while locked refuses and touches nothing.
        assert_eq!(v.write(|_| Ok(())).unwrap_err(), VaultError::Locked);
        v.unlock(key_with("a-new-master", None)).unwrap();
        assert_eq!(names(&mut v), vec!["API_KEY"]);
    }

    #[test]
    fn removed_file_is_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        fs::remove_file(kdbx(&dir)).unwrap();
        assert_eq!(
            v.read(|_| ()).unwrap_err(),
            VaultError::Conflict(ConflictReason::Removed)
        );
        assert_eq!(v.state(), LockState::Missing);
        let mut w = created(&dir);
        fs::remove_file(kdbx(&dir)).unwrap();
        assert_eq!(
            w.write(|_| Ok(())).unwrap_err(),
            VaultError::Conflict(ConflictReason::Removed)
        );
        assert!(dir_files(&dir).is_empty());
    }

    #[test]
    fn removed_between_refresh_and_move_aside_is_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        let k = kdbx(&dir);
        v.set_test_hook(hook_at(HookPoint::BeforeMoveAside, 1, move |_| {
            fs::remove_file(&k).unwrap();
        }));
        assert_eq!(
            v.write(|_| Ok(())).unwrap_err(),
            VaultError::Conflict(ConflictReason::Removed)
        );
        assert_eq!(v.state(), LockState::Missing);
        assert!(dir_files(&dir).is_empty());
    }

    // ---- failures and rejections ----

    #[test]
    fn failed_write_leaves_file_and_state_unchanged_and_no_temp() {
        for point in [
            HookPoint::BeforeMoveAside,
            HookPoint::AfterMoveAside,
            HookPoint::BeforePlace,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut v = created(&dir);
            set(&mut v, "API_KEY", "v1-value");
            let before = fs::read(kdbx(&dir)).unwrap();
            v.test.fail = Some(point);
            let e = v
                .write(|db| {
                    let g = root(db);
                    layout::put(db, g, "OTHER", &SecretValue::new("v2-value".into()));
                    Ok(())
                })
                .unwrap_err();
            assert!(matches!(e, VaultError::Io { .. }), "{point:?}: {e:?}");
            assert_eq!(fs::read(kdbx(&dir)).unwrap(), before, "{point:?}");
            assert_eq!(dir_files(&dir), vec!["hostler.kdbx"], "{point:?}");
            assert_eq!(v.state(), LockState::Unlocked);
            v.test.fail = None;
            assert_eq!(names(&mut v), vec!["API_KEY"], "{point:?}");
        }
    }

    #[test]
    fn failure_before_removing_prev_keeps_it_and_the_save_stands() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        v.test.fail = Some(HookPoint::BeforeRemovePrev);
        set(&mut v, "API_KEY", "v1-value");
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["API_KEY"]);
        assert_eq!(v.preserved_files().len(), 1);
    }

    #[test]
    fn rejected_closure_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "API_KEY", "v1-value");
        let before = fs::read(kdbx(&dir)).unwrap();
        let calls = AtomicUsize::new(0);
        let e = v
            .write(|db| -> Result<(), HostlerError> {
                calls.fetch_add(1, Ordering::SeqCst);
                let g = root(db);
                layout::put(db, g, "API_KEY", &SecretValue::new("v2-value".into()));
                Err(errors::secret_exists(&["API_KEY".into()]))
            })
            .unwrap_err();
        assert_eq!(
            e,
            VaultError::Rejected(errors::secret_exists(&["API_KEY".into()]))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(kdbx(&dir)).unwrap(), before);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
        // The clone was discarded: the loaded database still has the old value.
        let value = v
            .read(|db| value_of(layout::lookup(db, root(db), "API_KEY")))
            .unwrap();
        assert!(value == "v1-value");
    }

    // ---- unprotected outside passwords (T4-1) ----

    /// Every `Password` of every entry and of every history version is protected.
    fn all_passwords_protected(db: &Database) -> bool {
        let protected = |entry: &keepass::db::Entry| {
            entry
                .fields
                .get(fields::PASSWORD)
                .is_none_or(Value::is_protected)
        };
        db.iter_all_entries().all(|e| {
            protected(&e)
                && e.history
                    .as_ref()
                    .is_none_or(|h| h.get_entries().iter().all(protected))
        })
    }

    /// A database as another client may save it: entry `title` with an unprotected Password
    /// `current` and one history version with the unprotected Password `older`.
    fn plain_password_bytes(
        base: Option<&Path>,
        title: &str,
        older: &str,
        current: &str,
    ) -> Vec<u8> {
        let mut db = match base {
            Some(p) => Database::parse(&fs::read(p).unwrap(), key().0).unwrap(),
            None => Database::new(),
        };
        db.config.kdf_config = kdf_config(KdfChoice::Fast);
        db.config.version = DatabaseVersion::KDB4(1);
        let g = root(&db);
        let id = db
            .group_mut(g)
            .unwrap()
            .add_entry()
            .edit(|e| {
                e.set_unprotected(fields::TITLE, title);
                e.set_unprotected(fields::PASSWORD, older);
            })
            .id();
        db.entry_mut(id)
            .unwrap()
            .edit_tracking(|e| e.set_unprotected(fields::PASSWORD, current));
        let mut out = Vec::new();
        db.save(&mut out, key().0).unwrap();
        // The file really holds them unprotected (keepass keeps the flag on load).
        let raw = Database::parse(&out, key().0).unwrap();
        let e = raw.entry(id).unwrap();
        assert!(!e.fields[fields::PASSWORD].is_protected());
        let history = e.history.as_ref().unwrap().get_entries();
        assert_eq!(history.len(), 1);
        assert!(!history[0].fields[fields::PASSWORD].is_protected());
        out
    }

    fn password_and_history(db: &Database, title: &str) -> (String, Vec<String>) {
        let e = db
            .iter_all_entries()
            .find(|e| e.get_title() == Some(title))
            .unwrap();
        let history = e
            .history
            .as_ref()
            .map(|h| {
                h.get_entries()
                    .iter()
                    .map(|v| v.get(fields::PASSWORD).unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        (e.get(fields::PASSWORD).unwrap().to_string(), history)
    }

    #[test]
    fn unprotected_outside_passwords_are_protected_on_every_load() {
        let dir = tempfile::tempdir().unwrap();
        let k = kdbx(&dir);
        // Unlock of a file another client wrote.
        let first = plain_password_bytes(None, "PLAIN", "plain-old-value", "plain-new-value");
        fs::write(&k, first).unwrap();
        let mut v = Vault::new(k.clone());
        v.unlock(key()).unwrap();
        let (ok, (current, history)) = v
            .read(|db| {
                (
                    all_passwords_protected(db),
                    password_and_history(db, "PLAIN"),
                )
            })
            .unwrap();
        assert!(ok, "unlock kept an unprotected Password");
        assert!(current == "plain-new-value" && history == ["plain-old-value"]);
        // Refresh after an outside save that adds another unprotected one.
        let outside = plain_password_bytes(Some(&k), "PLAIN_TWO", "two-old-value", "two-new-value");
        fs::write(&k, outside).unwrap();
        let (ok, (current, history)) = v
            .read(|db| {
                (
                    all_passwords_protected(db),
                    password_and_history(db, "PLAIN_TWO"),
                )
            })
            .unwrap();
        assert!(ok, "refresh kept an unprotected Password");
        assert!(current == "two-new-value" && history == ["two-old-value"]);
        // A rejected write's clone is protected too.
        let e = v
            .write(|db| {
                assert!(all_passwords_protected(db));
                Err::<(), _>(errors::bad_request("no".into()))
            })
            .unwrap_err();
        assert!(matches!(e, VaultError::Rejected(_)));
        // A write keeps them protected on disk, values and history intact.
        write_mine(&mut v).unwrap();
        let on_disk = Database::parse(&fs::read(&k).unwrap(), key().0).unwrap();
        assert!(all_passwords_protected(&on_disk));
        let (current, history) = password_and_history(&on_disk, "PLAIN");
        assert!(current == "plain-new-value" && history == ["plain-old-value"]);
    }

    // ---- directory sync (T4-2) ----

    #[test]
    fn failed_directory_sync_keeps_prev_and_is_not_durable() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        v.test.fail = Some(HookPoint::SyncDir);
        assert_eq!(write_mine(&mut v), Err(VaultError::NotDurable));
        // The new save stands at `K`; the previous version is kept; no temp file.
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["BASE", "MINE"]);
        let kept = v.preserved_files();
        assert_eq!(kept.len(), 1);
        assert_eq!(names_in_file(&dir.path().join(&kept[0])), vec!["BASE"]);
        assert_eq!(dir_files(&dir).len(), 2);
        assert_eq!(v.state(), LockState::Unlocked);
        v.test.fail = None;
        assert_eq!(names(&mut v), vec!["BASE", "MINE"]);
        assert_eq!(
            VaultError::NotDurable.to_hostler().details["reason"],
            "not_durable"
        );
    }

    #[test]
    fn failed_directory_sync_on_create_is_not_durable() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::new(kdbx(&dir));
        v.test.fail = Some(HookPoint::SyncDir);
        assert_eq!(
            v.create(key(), KdfChoice::Fast),
            Err(VaultError::NotDurable)
        );
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
        assert_eq!(v.state(), LockState::Unlocked);
    }

    // ---- outside saves at every point ----

    fn write_mine(v: &mut Vault) -> Result<(), VaultError> {
        v.write(|db| {
            let g = root(db);
            layout::put(db, g, "MINE", &SecretValue::new("hostler-value".into()));
            Ok(())
        })
    }

    fn outside_at(point: HookPoint, times: usize) -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        v.set_test_hook(hook_at(point, times, move |n| {
            let name = if n == 0 { "OUTSIDE" } else { "OUTSIDE_TWO" };
            outside_save(&k, &current_source(&k), name);
        }));
        (dir, v)
    }

    #[test]
    fn outside_save_before_move_aside_is_restored_and_merged() {
        let (dir, mut v) = outside_at(HookPoint::BeforeMoveAside, 1);
        write_mine(&mut v).unwrap();
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["BASE", "MINE", "OUTSIDE"]);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
        assert_eq!(names(&mut v), vec!["BASE", "MINE", "OUTSIDE"]);
    }

    #[test]
    fn outside_save_after_move_aside_wins_the_name() {
        let (dir, mut v) = outside_at(HookPoint::AfterMoveAside, 1);
        write_mine(&mut v).unwrap();
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["BASE", "MINE", "OUTSIDE"]);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }

    #[test]
    fn outside_save_before_place_wins_the_name() {
        let (dir, mut v) = outside_at(HookPoint::BeforePlace, 1);
        write_mine(&mut v).unwrap();
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["BASE", "MINE", "OUTSIDE"]);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }

    #[test]
    fn in_place_write_into_moved_file_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        v.set_test_hook(hook_at(HookPoint::AfterMoveAside, 1, move |_| {
            let p = only_prev(&k);
            let bytes = outside_bytes(&p, "OUTSIDE");
            fs::write(&p, bytes).unwrap(); // same inode, rewritten in place
        }));
        write_mine(&mut v).unwrap();
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["BASE", "MINE", "OUTSIDE"]);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }

    #[test]
    fn in_place_write_after_place_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        v.set_test_hook(hook_at(HookPoint::BeforeRemovePrev, 1, move |_| {
            let p = only_prev(&k);
            let bytes = outside_bytes(&p, "OUTSIDE");
            fs::write(&p, bytes).unwrap();
        }));
        write_mine(&mut v).unwrap();
        assert_eq!(names_in_file(&kdbx(&dir)), vec!["BASE", "MINE"]);
        let kept = v.preserved_files();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].starts_with("hostler.kdbx.prev-"));
        assert_eq!(
            names_in_file(&dir.path().join(&kept[0])),
            vec!["BASE", "OUTSIDE"]
        );
        assert_eq!(dir_files(&dir).len(), 2);
    }

    #[test]
    fn keeps_changing_is_conflict_with_the_outside_version_in_place() {
        let (dir, mut v) = outside_at(HookPoint::BeforeMoveAside, 2);
        assert_eq!(
            write_mine(&mut v).unwrap_err(),
            VaultError::Conflict(ConflictReason::KeepsChanging)
        );
        assert_eq!(
            names_in_file(&kdbx(&dir)),
            vec!["BASE", "OUTSIDE", "OUTSIDE_TWO"]
        );
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
        // Still unlocked; the next read sees the outside version.
        assert_eq!(names(&mut v), vec!["BASE", "OUTSIDE", "OUTSIDE_TWO"]);
    }

    #[test]
    fn retried_closure_sees_the_outside_version() {
        let (_dir, mut v) = outside_at(HookPoint::BeforeMoveAside, 1);
        let seen = Mutex::new(Vec::new());
        let e = v
            .write(|db| {
                let names = layout::names(db, root(db));
                seen.lock().unwrap().push(names.clone());
                if names.contains(&"OUTSIDE".to_string()) {
                    return Err(errors::secret_exists(&["OUTSIDE".into()]));
                }
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(e, VaultError::Rejected(_)));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                vec!["BASE".to_string()],
                vec!["BASE".into(), "OUTSIDE".into()]
            ]
        );
    }

    #[test]
    fn move_aside_is_a_single_rename() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        // (K exists, file id, SHA-256) seen before and after the move aside
        type Seen = (bool, Option<u64>, [u8; 32]);
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (k2, seen2) = (k.clone(), seen.clone());
        let id = |p: &Path| -> Option<u64> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Some(fs::metadata(p).unwrap().ino())
            }
            #[cfg(not(unix))]
            {
                let _ = p;
                None
            }
        };
        v.set_test_hook(Box::new(move |p| match p {
            HookPoint::BeforeMoveAside => {
                seen2
                    .lock()
                    .unwrap()
                    .push((true, id(&k2), file_sha256(&k2).unwrap()));
            }
            HookPoint::AfterMoveAside => {
                let prev = only_prev(&k2);
                seen2
                    .lock()
                    .unwrap()
                    .push((k2.exists(), id(&prev), file_sha256(&prev).unwrap()));
            }
            _ => {}
        }));
        write_mine(&mut v).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(!seen[1].0, "K still exists after the move aside");
        assert_eq!(seen[0].1, seen[1].1, "P is not the inode K had");
        assert_eq!(seen[0].2, seen[1].2);
    }

    #[test]
    fn forced_link_fallback_never_loses_an_outside_save() {
        for point in [
            HookPoint::BeforeMoveAside,
            HookPoint::AfterMoveAside,
            HookPoint::BeforePlace,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut v = created(&dir);
            set(&mut v, "BASE", "base-value");
            v.force_link_fallback();
            let k = kdbx(&dir);
            let k_after_move: Arc<Mutex<Option<bool>>> = Arc::default();
            let seen = k_after_move.clone();
            let k2 = k.clone();
            v.set_test_hook(Box::new(move |p| {
                if p == HookPoint::AfterMoveAside && seen.lock().unwrap().is_none() {
                    *seen.lock().unwrap() = Some(k2.exists());
                }
            }));
            let k3 = k.clone();
            let outside = hook_at(point, 1, move |_| {
                outside_save(&k3, &current_source(&k3), "OUTSIDE");
            });
            let first = v.test.hook.take().unwrap();
            v.set_test_hook(Box::new(move |p| {
                first(p);
                outside(p);
            }));
            write_mine(&mut v).unwrap();
            assert_eq!(*k_after_move.lock().unwrap(), Some(false), "{point:?}");
            let in_k = names_in_file(&k);
            let in_prev = v
                .preserved_files()
                .iter()
                .any(|n| names_in_file(&dir.path().join(n)).contains(&"OUTSIDE".into()));
            assert!(
                in_k.contains(&"OUTSIDE".into()) || in_prev,
                "{point:?}: the outside save is lost"
            );
            assert!(in_k.contains(&"MINE".into()), "{point:?}");
            #[cfg(unix)]
            for name in dir_files(&dir) {
                use std::os::unix::fs::MetadataExt;
                let links = fs::metadata(dir.path().join(&name)).unwrap().nlink();
                assert_eq!(links, 1, "{point:?}: {name} has a second name");
            }
        }
    }

    /// A real concurrent outside writer (temp file + rename over `K`, built on what it read)
    /// while hostler saves in a loop: no outside version is ever lost. Deterministic hooks cannot
    /// reach inside the move-aside; this race does (a link + unlink move-aside fails it).
    #[test]
    fn concurrent_outside_saves_are_never_lost() {
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        let k = kdbx(&dir);
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (k, stop) = (k.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut placed = Vec::new();
                let tmp = k.with_file_name("outside-save.tmp");
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    let Ok(bytes) = fs::read(&k) else { continue };
                    let Ok(mut db) = Database::parse(&bytes, key().0) else {
                        continue;
                    };
                    let name = format!("OUT_{n}");
                    let g = root(&db);
                    layout::put(&mut db, g, &name, &SecretValue::new("outside-value".into()));
                    db.config.version = DatabaseVersion::KDB4(1);
                    let mut out = Vec::new();
                    db.save(&mut out, key().0).unwrap();
                    fs::write(&tmp, &out).unwrap();
                    if fs::rename(&tmp, &k).is_ok() {
                        placed.push(name);
                    }
                    n += 1;
                    // Paced so that hostler's saves get through too; varied to move the race.
                    std::thread::sleep(Duration::from_micros(500 * (n % 7)));
                }
                placed
            })
        };
        let start = Instant::now();
        let mut saves = 0;
        while start.elapsed() < Duration::from_millis(1500) {
            match v.write(|db| {
                let g = root(db);
                layout::put(db, g, "MINE", &SecretValue::new("hostler-value".into()));
                Ok(())
            }) {
                Ok(()) => saves += 1,
                Err(VaultError::Conflict(ConflictReason::KeepsChanging)) => {}
                Err(e) => panic!("{e:?}"),
            }
        }
        stop.store(true, Ordering::SeqCst);
        let placed = writer.join().unwrap();
        assert!(
            saves > 0 && !placed.is_empty(),
            "{saves} saves, {} outside",
            placed.len()
        );
        let mut kept = names_in_file(&k);
        for p in v.preserved_files() {
            kept.extend(names_in_file(&dir.path().join(p)));
        }
        let lost: Vec<&String> = placed.iter().filter(|n| !kept.contains(n)).collect();
        assert!(lost.is_empty(), "outside versions lost: {lost:?}");
    }

    // ---- no-replace rename and recovery ----

    #[test]
    fn no_replace_rename_refuses_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for (rename, label) in [
            (
                no_replace_rename as fn(&Path, &Path) -> io::Result<()>,
                "native",
            ),
            (link_then_unlink, "hard link"),
        ] {
            fs::write(&a, b"from").unwrap();
            fs::write(&b, b"target").unwrap();
            let e = rename(&a, &b).unwrap_err();
            assert_eq!(e.kind(), ErrorKind::AlreadyExists, "{label}");
            assert_eq!(fs::read(&b).unwrap(), b"target", "{label}");
            assert_eq!(fs::read(&a).unwrap(), b"from", "{label}");
            rename(&a, &c).unwrap();
            assert!(!a.exists(), "{label}");
            assert_eq!(fs::read(&c).unwrap(), b"from", "{label}");
            fs::remove_file(&b).unwrap();
            fs::remove_file(&c).unwrap();
        }
    }

    /// The platform call itself works here (no silent fallback): renameat2 on Linux, renamex_np
    /// on macOS, MoveFileExW on Windows.
    #[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
    #[test]
    fn native_no_replace_is_supported_here() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, b"one").unwrap();
        fs::write(&b, b"two").unwrap();
        let e = native_no_replace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::AlreadyExists, "{e:?}");
        fs::remove_file(&b).unwrap();
        native_no_replace(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read(&b).unwrap(), b"one");
    }

    #[test]
    fn recover_restores_a_moved_aside_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        let before = fs::read(&k).unwrap();
        // The state a crash after step 4 leaves: `K` missing, one `P`, one `T`.
        let p = sibling(&k, "prev");
        fs::rename(&k, &p).unwrap();
        fs::write(sibling(&k, "tmp"), b"half a save").unwrap();
        let r = recover_at_start(&k);
        assert!(r.restored);
        assert_eq!(r.removed_temps, 1);
        assert!(r.preserved.is_empty());
        assert_eq!(fs::read(&k).unwrap(), before);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
        // Nothing to do the second time.
        let r = recover_at_start(&k);
        assert!(!r.restored && r.removed_temps == 0 && r.preserved.is_empty());
    }

    #[test]
    fn recover_restores_the_newest_of_two_prev_files() {
        let dir = tempfile::tempdir().unwrap();
        let k = kdbx(&dir);
        let (older, newer) = (sibling(&k, "prev"), sibling(&k, "prev"));
        fs::write(&older, b"the older version").unwrap();
        fs::write(&newer, b"the newer version").unwrap();
        let now = std::time::SystemTime::now();
        let set_mtime = |p: &Path, t| {
            let f = File::options().write(true).open(p).unwrap();
            f.set_modified(t).unwrap();
        };
        // Created in this order, so the second UUID name is not the deciding factor.
        set_mtime(&newer, now);
        set_mtime(&older, now - Duration::from_secs(3600));
        let r = recover_at_start(&k);
        assert!(r.restored);
        assert_eq!(fs::read(&k).unwrap(), b"the newer version");
        let older_name = older.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(r.preserved, vec![older_name]);
        assert_eq!(fs::read(&older).unwrap(), b"the older version");
    }

    #[test]
    fn recover_keeps_prev_files_when_the_database_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let v = created(&dir);
        let k = kdbx(&dir);
        let p = sibling(&k, "prev");
        fs::write(&p, b"an outside version").unwrap();
        let r = recover_at_start(&k);
        assert!(!r.restored);
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(r.preserved, vec![name.clone()]);
        assert_eq!(v.preserved_files(), vec![name]);
    }

    #[test]
    fn vault_is_send() {
        fn send<T: Send>() {}
        send::<Vault>();
        send::<VaultError>();
    }

    #[test]
    fn env_hook_points_parse() {
        for (name, point) in [
            ("unlock", HookPoint::Unlock),
            ("before_move_aside", HookPoint::BeforeMoveAside),
            ("move_aside_retry", HookPoint::MoveAsideRetry),
            ("after_move_aside", HookPoint::AfterMoveAside),
            ("before_place", HookPoint::BeforePlace),
            ("place_retry", HookPoint::PlaceRetry),
            ("before_remove_prev", HookPoint::BeforeRemovePrev),
            ("sync_dir", HookPoint::SyncDir),
        ] {
            assert_eq!(HookPoint::from_name(name), Some(point));
        }
        assert_eq!(HookPoint::from_name("nowhere"), None);
    }

    // ---- Windows: a program holding the file ----

    #[cfg(windows)]
    fn hold(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_retry_then_outside_save_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        let held = Arc::new(Mutex::new(Some(hold(&k))));
        let (k2, held2) = (k.clone(), held.clone());
        let retries = Arc::new(AtomicUsize::new(0));
        let retries2 = retries.clone();
        v.set_test_hook(Box::new(move |p| {
            if p == HookPoint::MoveAsideRetry && retries2.fetch_add(1, Ordering::SeqCst) == 0 {
                drop(held2.lock().unwrap().take());
                outside_save(&k2, &k2, "OUTSIDE");
            }
        }));
        write_mine(&mut v).unwrap();
        assert!(retries.load(Ordering::SeqCst) >= 1);
        assert_eq!(names_in_file(&k), vec!["BASE", "MINE", "OUTSIDE"]);
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_place_retry_puts_the_old_file_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = created(&dir);
        set(&mut v, "BASE", "base-value");
        let k = kdbx(&dir);
        let before = fs::read(&k).unwrap();
        let held: Arc<Mutex<Option<File>>> = Arc::default();
        let (k2, held2) = (k.clone(), held.clone());
        let place_retries = Arc::new(AtomicUsize::new(0));
        let place_retries2 = place_retries.clone();
        v.set_test_hook(Box::new(move |p| match p {
            HookPoint::BeforePlace => {
                let t = siblings(&k2, "tmp");
                assert_eq!(t.len(), 1);
                *held2.lock().unwrap() = Some(hold(&t[0].1));
            }
            HookPoint::PlaceRetry => {
                place_retries2.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }));
        assert_eq!(
            write_mine(&mut v).unwrap_err(),
            VaultError::Conflict(ConflictReason::FileInUse)
        );
        assert_eq!(place_retries.load(Ordering::SeqCst), RETRIES as usize);
        assert_eq!(fs::read(&k).unwrap(), before);
        assert!(v.preserved_files().is_empty());
        drop(held.lock().unwrap().take());
        for (_, t) in siblings(&k, "tmp") {
            fs::remove_file(t).unwrap();
        }
        assert_eq!(dir_files(&dir), vec!["hostler.kdbx"]);
    }
}
