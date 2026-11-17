use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};

/// Where this hostler installation keeps its state.
#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    /// True when `HOSTLER_HOME` points somewhere other than the OS default.
    pub custom_home: bool,
    pub db: PathBuf,
    pub service_lock: PathBuf,
    pub log_dir: PathBuf,
    pub socket: SocketName,
    /// The secrets database (P2).
    pub kdbx: PathBuf,
    /// Per-run directories of `hostler run --secret-file` (P2).
    pub runs: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketName {
    File(PathBuf),
    Namespaced(String),
}

/// The home directory and whether it is a custom one. L45 (W-13): `HOSTLER_HOME` must be
/// absolute — the socket name is derived from its spelling, so a relative or drive-relative one
/// would start a service that the same home written absolutely can never reach.
fn chosen_home(
    var: Option<std::ffi::OsString>,
    default_home: PathBuf,
) -> anyhow::Result<(PathBuf, bool)> {
    let Some(home) = var else {
        return Ok((default_home, false));
    };
    let home = PathBuf::from(home);
    anyhow::ensure!(
        home.is_absolute(),
        "HOSTLER_HOME must be an absolute path, not {:?}; set it to the full path of the \
         directory",
        home.display().to_string()
    );
    let custom_home = home != default_home;
    Ok((home, custom_home))
}

impl Paths {
    /// The per-user data directory, ignoring `HOSTLER_HOME`.
    pub fn default_home() -> anyhow::Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("io.github", "dzamataev", "hostler")
            .context("no data directory for the current user")?;
        Ok(dirs.data_dir().to_path_buf())
    }

    /// `HOSTLER_HOME` or the OS default, then `ensure_dirs`.
    pub fn resolve() -> anyhow::Result<Paths> {
        let paths = Paths::planned()?;
        paths.ensure_dirs()?;
        Ok(paths)
    }

    /// The paths `resolve` would give, without creating or checking any directory (to name the
    /// socket in an error from `ensure_dirs`, plan P4 R1-2).
    pub fn planned() -> anyhow::Result<Paths> {
        let (home, custom_home) =
            chosen_home(std::env::var_os("HOSTLER_HOME"), Paths::default_home()?)?;
        Ok(Paths::under(home, custom_home))
    }

    /// Pure mapping of a home directory to all state paths.
    pub fn under(home: PathBuf, custom_home: bool) -> Paths {
        Paths {
            db: home.join("hostler.db"),
            service_lock: home.join("service.lock"),
            log_dir: home.join("logs"),
            socket: socket_name(&home),
            kdbx: home.join("hostler.kdbx"),
            runs: home.join("runs"),
            custom_home,
            home,
        }
    }

    /// Creates the state directories and rejects existing ones other users can reach.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            ensure_private_dir(&self.home)?;
            ensure_private_dir(&self.log_dir)?;
            if let SocketName::File(socket) = &self.socket
                && let Some(dir) = socket.parent()
                && dir != self.home.as_path()
            {
                ensure_private_dir(dir)?;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&self.home)
                .with_context(|| format!("cannot create {}", self.home.display()))?;
            std::fs::create_dir_all(&self.log_dir)
                .with_context(|| format!("cannot create {}", self.log_dir.display()))
        }
    }
}

#[cfg(unix)]
const MAX_SOCKET_FILE_LEN: usize = 100;

#[cfg(unix)]
fn socket_name(home: &Path) -> SocketName {
    // Unix socket paths are limited to ~104 bytes, and $TMPDIR is too long on
    // macOS. The fallback lives under the literal /tmp in a private per-user
    // directory so nobody else can race the socket file.
    let preferred = home.join("hostler.sock");
    if preferred.as_os_str().as_encoded_bytes().len() <= MAX_SOCKET_FILE_LEN {
        SocketName::File(preferred)
    } else {
        SocketName::File(
            Path::new("/tmp")
                .join(format!("hostler-{}-{}", current_uid(), home_hash(home)))
                .join("hostler.sock"),
        )
    }
}

// Linux abstract sockets ignore file permissions, so they are never used.
#[cfg(windows)]
fn socket_name(home: &Path) -> SocketName {
    SocketName::Namespaced(format!("hostler-{}-{}", username(), home_hash(home)))
}

fn home_hash(home: &Path) -> String {
    let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

#[cfg(windows)]
fn username() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_owned())
}

#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    match std::fs::symlink_metadata(dir) {
        Ok(_) => check_private_dir(dir),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            // The directory may have appeared between the check and the create.
            check_private_dir(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("cannot restrict {}", dir.display()))
        }
        Err(err) => Err(err).with_context(|| format!("cannot inspect {}", dir.display())),
    }
}

#[cfg(unix)]
fn check_private_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("cannot inspect {}", dir.display()))?;
    if !meta.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }
    if meta.uid() != current_uid() {
        anyhow::bail!(
            "{} is not owned by the current user; choose another HOSTLER_HOME",
            dir.display()
        );
    }
    if meta.permissions().mode() & 0o077 != 0 {
        anyhow::bail!(
            "{} is accessible by other users; make it private with `chmod 700 {}`",
            dir.display(),
            dir.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod home_tests {
    use super::*;

    /// L45 (W-13): only an absolute `HOSTLER_HOME`.
    #[test]
    fn hostler_home_must_be_absolute() {
        let default = std::env::temp_dir().join("default-home");
        assert_eq!(
            chosen_home(None, default.clone()).unwrap(),
            (default.clone(), false)
        );
        let custom = std::env::temp_dir().join("custom-home");
        assert_eq!(
            chosen_home(Some(custom.clone().into()), default.clone()).unwrap(),
            (custom, true)
        );
        for relative in ["relative/home", "", "."] {
            let e = chosen_home(Some(relative.into()), default.clone()).unwrap_err();
            assert!(
                e.to_string()
                    .starts_with("HOSTLER_HOME must be an absolute path"),
                "{e}"
            );
        }
        #[cfg(windows)]
        assert!(chosen_home(Some(r"C:Users\\me".into()), default).is_err());
    }
}
