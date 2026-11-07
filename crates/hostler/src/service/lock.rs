//! `service.lock`: exactly one service per home. Whoever holds it (the service, or an installer
//! changing the OS registration) keeps every other `hostler service run` from serving. The OS
//! releases it when the holder exits, however it exits.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::Context;

/// The lock file, open but not yet locked.
pub struct LockFile {
    file: File,
}

/// A held lock that borrows its `LockFile`; dropping it releases the lock.
pub struct LockGuard<'a> {
    lock: &'a mut LockFile,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        let _ = self.lock.file.unlock();
    }
}

/// A held lock that owns its file; dropping it releases the lock.
pub struct ServiceLock {
    file: File,
}

impl Drop for ServiceLock {
    fn drop(&mut self) {
        // Unlock explicitly: closing our fd is not enough while a child forked by another
        // thread still holds an inherited copy of it (until that child execs).
        let _ = self.file.unlock();
    }
}

pub fn open(path: &Path) -> anyhow::Result<LockFile> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    Ok(LockFile { file })
}

fn try_lock(file: &File) -> anyhow::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(e)) => Err(e).context("cannot lock the service lock file"),
    }
}

/// The guard, or `None` when another process holds the lock.
pub fn try_acquire(lock: &mut LockFile) -> anyhow::Result<Option<LockGuard<'_>>> {
    Ok(try_lock(&lock.file)?.then_some(LockGuard { lock }))
}

/// The lock as an owned value, or `None` when another process holds it.
pub fn try_hold(path: &Path) -> anyhow::Result<Option<ServiceLock>> {
    let LockFile { file } = open(path)?;
    Ok(try_lock(&file)?.then_some(ServiceLock { file }))
}

/// True while some process holds the lock at `path`.
pub fn is_held(path: &Path) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    Ok(try_hold(path)?.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.lock");
        let held = try_hold(&path).unwrap();
        assert!(held.is_some());
        assert!(try_hold(&path).unwrap().is_none());
        let mut other = open(&path).unwrap();
        assert!(try_acquire(&mut other).unwrap().is_none());
        assert!(is_held(&path).unwrap());
        drop(held);
        assert!(!is_held(&path).unwrap());
        let guard = try_acquire(&mut other).unwrap();
        assert!(guard.is_some());
        assert!(try_hold(&path).unwrap().is_none());
        drop(guard);
        assert!(try_hold(&path).unwrap().is_some());
    }

    #[test]
    fn released_at_once_on_drop_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.lock");
        for round in 0..200 {
            let held = try_hold(&path)
                .unwrap()
                .expect("free at the start of a round");
            assert!(is_held(&path).unwrap(), "round {round}");
            drop(held);
            assert!(
                !is_held(&path).unwrap(),
                "round {round}: still held after drop"
            );
        }
    }

    /// A child that inherited the locked open file description (as a child forked by another
    /// thread does until it execs) must not keep the lock alive after the holder drops it.
    #[cfg(unix)]
    #[test]
    fn released_on_drop_while_a_child_holds_an_inherited_fd() {
        use std::os::fd::AsRawFd;
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.lock");
        let held = try_hold(&path).unwrap().unwrap();
        // SAFETY: dup of a valid open fd; the copy has no close-on-exec flag, so the child
        // inherits it. It is closed below.
        let inherited = unsafe { libc::dup(held.file.as_raw_fd()) };
        assert!(inherited >= 0, "{}", std::io::Error::last_os_error());
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        // SAFETY: closes the copy made above, exactly once; the child keeps its own.
        unsafe { libc::close(inherited) };

        drop(held);
        let released = !is_held(&path).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(released, "the child's inherited fd kept the lock");
    }
}
