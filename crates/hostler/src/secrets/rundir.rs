//! Per-run directories for `--secret-file` and their sweep (plan P2, Task 10). A run directory
//! `runs/<run id>/` holds one file per secret and `owner.json` (the wrapper's identity, then
//! the command's); it is private to the user (Unix 0700 / 0600, Windows a protected DACL for the
//! user's SID only, inherited by the files).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::value::SecretValue;
use crate::procs::{
    Liveness, ProcessIdentity, ProcessTable, SysProcessTable, current_identity, liveness,
};

const OWNER: &str = "owner.json";
/// A directory without a readable `owner.json` is being created, unless it is older than this.
const UNOWNED_AGE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    wrapper: ProcessIdentity,
    child: Option<ProcessIdentity>,
}

/// A run directory; removed with its files on drop.
#[derive(Debug)]
pub struct RunDir {
    path: PathBuf,
    wrapper: ProcessIdentity,
}

impl RunDir {
    /// Creates `runs/<run_id>` (it must not exist) and records this process as its wrapper.
    pub fn create(runs: &Path, run_id: &str) -> anyhow::Result<RunDir> {
        let wrapper = current_identity(&SysProcessTable::new()?)?;
        RunDir::create_for(runs, run_id, wrapper)
    }

    fn create_for(runs: &Path, run_id: &str, wrapper: ProcessIdentity) -> anyhow::Result<RunDir> {
        anyhow::ensure!(
            !run_id.is_empty()
                && run_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "a run id is letters, digits and dashes"
        );
        fs::create_dir_all(runs).with_context(|| format!("cannot create {}", runs.display()))?;
        let path = runs.join(run_id);
        private_dir(&path).with_context(|| format!("cannot create {}", path.display()))?;
        let dir = RunDir { path, wrapper };
        dir.write_owner(None)?;
        Ok(dir)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `value` to the file `name` (a secret name) and returns its path.
    pub fn write(&self, name: &str, value: &SecretValue) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(super::valid_secret_name(name), "not a secret name");
        let path = self.path.join(file_name(name));
        let mut file = private_file(&path)
            .with_context(|| format!("cannot create the secret file for {name}"))?;
        file.write_all(value.expose().as_bytes())
            .with_context(|| format!("cannot write the secret file for {name}"))?;
        Ok(path)
    }

    /// Records the command's process, so a sweep keeps the files while it runs.
    pub fn record_child(&self, child: &ProcessIdentity) -> anyhow::Result<()> {
        self.write_owner(Some(child.clone()))
    }

    /// Replaces `owner.json` atomically: a sweep never reads half of it.
    fn write_owner(&self, child: Option<ProcessIdentity>) -> anyhow::Result<()> {
        let owner = Owner {
            wrapper: self.wrapper.clone(),
            child,
        };
        let tmp = self.path.join(format!("{OWNER}.tmp"));
        let _ = fs::remove_file(&tmp);
        let mut file = private_file(&tmp).context("cannot create the run's owner file")?;
        file.write_all(&serde_json::to_vec(&owner)?)
            .context("cannot write the run's owner file")?;
        drop(file);
        fs::rename(&tmp, self.path.join(OWNER)).context("cannot place the run's owner file")
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        // Windows refuses while a process still has a file open: retry briefly, then leave it
        // to a later sweep.
        let tries = if cfg!(windows) { 10 } else { 1 };
        for i in 0..tries {
            match fs::remove_dir_all(&self.path) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if i + 1 < tries => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => eprintln!(
                    "[hostler] Warning: cannot remove the secret files in {} ({e}); a later run \
                     removes them",
                    self.path.display()
                ),
            }
        }
    }
}

/// The file of secret `name`. Prefixed: valid names such as `NUL`, `CON` or `COM1` are DOS
/// devices on Windows (also with an extension), never a regular file in the run directory.
fn file_name(name: &str) -> String {
    format!("secret-{name}")
}

#[cfg(unix)]
fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use interprocess::os::windows::security_descriptor::AsSecurityDescriptor;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    // Files created inside inherit the one ACE (OICI).
    let sd =
        crate::winsec::user_only_sd_with_rights("OICI", "FA").map_err(std::io::Error::other)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_sd().cast_mut(),
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated path and attributes whose descriptor `sd` outlives the call.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// A new file only the user can read; never follows a link planted at `path`.
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Removes run directories whose wrapper and command are both gone; the number removed.
pub fn sweep(runs: &Path) -> usize {
    match SysProcessTable::new() {
        Ok(table) => sweep_with(runs, &table, SystemTime::now()),
        Err(_) => 0,
    }
}

fn sweep_with(runs: &Path, table: &dyn ProcessTable, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(runs) else {
        return 0;
    };
    let gone = |id: &ProcessIdentity| matches!(liveness(id, table), Liveness::Dead(_));
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let owner = fs::read(path.join(OWNER))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Owner>(&bytes).ok());
        let dead = match owner {
            Some(owner) => gone(&owner.wrapper) && owner.child.as_ref().is_none_or(gone),
            None => entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > UNOWNED_AGE)),
        };
        if dead && fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> ProcessIdentity {
        current_identity(&SysProcessTable::new().unwrap()).unwrap()
    }

    /// This process with a start time it never had: judged dead (pid reused).
    fn dead() -> ProcessIdentity {
        let mut id = me();
        id.start_time += 1;
        id
    }

    #[test]
    fn run_dir_files_and_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join("runs");
        let dir = RunDir::create(&runs, "run-1").unwrap();
        let path = dir
            .write("API_KEY", &SecretValue::new("k3y-value".into()))
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "k3y-value");
        assert!(
            dir.write("API_KEY", &SecretValue::new("again".into()))
                .is_err()
        );
        assert!(dir.write("../x", &SecretValue::new("abcd".into())).is_err());
        assert!(
            RunDir::create(&runs, "run-1").is_err(),
            "an existing dir is never reused"
        );
        assert!(RunDir::create(&runs, "../up").is_err());
        dir.record_child(&me()).unwrap();
        let owner: Owner =
            serde_json::from_slice(&fs::read(dir.path().join(OWNER)).unwrap()).unwrap();
        assert_eq!(owner.child, Some(me()));
        let at = dir.path().to_path_buf();
        drop(dir);
        assert!(!at.exists());
    }

    #[test]
    fn device_names_get_regular_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = RunDir::create(tmp.path(), "run-dev").unwrap();
        for name in ["NUL", "CON", "COM1", "AUX", "PRN", "LPT1"] {
            assert_eq!(file_name(name), format!("secret-{name}"));
            let path = dir.write(name, &SecretValue::new("abcd".into())).unwrap();
            assert_eq!(path, dir.path().join(format!("secret-{name}")));
            assert!(fs::metadata(&path).unwrap().is_file());
            assert_eq!(fs::read_to_string(&path).unwrap(), "abcd");
        }
        let at = dir.path().to_path_buf();
        drop(dir);
        assert!(!at.exists());
    }

    #[cfg(unix)]
    #[test]
    fn run_dir_modes_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = RunDir::create(tmp.path(), "run-2").unwrap();
        let file = dir
            .write("TOKEN", &SecretValue::new("abcd".into()))
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.path()), 0o700);
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&dir.path().join(OWNER)), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_never_follows_a_planted_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = RunDir::create(tmp.path(), "run-3").unwrap();
        let target = tmp.path().join("elsewhere");
        std::os::unix::fs::symlink(&target, dir.path().join(file_name("TOKEN"))).unwrap();
        assert!(
            dir.write("TOKEN", &SecretValue::new("abcd".into()))
                .is_err()
        );
        assert!(!target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_run_dir_dacl_is_user_only() {
        use std::os::windows::ffi::OsStrExt;

        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

        fn dacl_sddl(path: &Path) -> String {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: a NUL-terminated path and writable out-pointers; `sd` is freed below.
            let rc = unsafe {
                GetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut sd,
                )
            };
            assert_eq!(rc, 0);
            let mut text: *mut u16 = std::ptr::null_mut();
            // SAFETY: a valid descriptor; `text` is LocalAlloc'd and freed below.
            let ok = unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    sd,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0);
            // SAFETY: NUL-terminated UTF-16 from the call above.
            let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
            // SAFETY: `len` initialized u16s.
            let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
            // SAFETY: both were allocated for us by the calls above.
            unsafe {
                LocalFree(text.cast());
                LocalFree(sd);
            }
            s
        }

        let sid = crate::winsec::current_user_sid().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = RunDir::create(tmp.path(), "run-4").unwrap();
        let file = dir
            .write("TOKEN", &SecretValue::new("abcd".into()))
            .unwrap();
        let dir_sddl = dacl_sddl(dir.path());
        assert!(dir_sddl.starts_with("D:P"), "{dir_sddl}");
        assert_eq!(dir_sddl.matches("(A;").count(), 1, "{dir_sddl}");
        assert!(dir_sddl.contains(&format!(";;;{sid})")), "{dir_sddl}");
        let file_sddl = dacl_sddl(&file);
        assert_eq!(file_sddl.matches("(A;").count(), 1, "{file_sddl}");
        assert!(file_sddl.contains(&format!(";;;{sid})")), "{file_sddl}");
    }

    #[test]
    fn sweep_keeps_live_and_removes_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path();
        let table = SysProcessTable::new().unwrap();
        let now = SystemTime::now();
        let make = |id: &str, wrapper: ProcessIdentity, child: Option<ProcessIdentity>| {
            let dir = RunDir::create_for(runs, id, wrapper).unwrap();
            if let Some(child) = child {
                dir.record_child(&child).unwrap();
            }
            // Detached: the sweep, not the guard, decides.
            std::mem::forget(dir);
        };
        make("live-wrapper", me(), None);
        make("live-child", dead(), Some(me()));
        make("both-dead", dead(), Some(dead()));
        make("dead-no-child", dead(), None);
        fs::create_dir(runs.join("unowned")).unwrap();
        assert_eq!(sweep_with(runs, &table, now), 2);
        let left = |name: &str| runs.join(name).exists();
        assert!(left("live-wrapper") && left("live-child") && left("unowned"));
        assert!(!left("both-dead") && !left("dead-no-child"));
        // An owner file that never appeared: removed once the dir is old.
        assert_eq!(sweep_with(runs, &table, now + UNOWNED_AGE * 2), 1);
        assert!(!left("unowned"));
        assert_eq!(sweep(&runs.join("missing")), 0);
    }
}
