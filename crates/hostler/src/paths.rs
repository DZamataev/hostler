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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketName {
    File(PathBuf),
    Namespaced(String),
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
        let default_home = Paths::default_home()?;
        let (home, custom_home) = match std::env::var_os("HOSTLER_HOME") {
            Some(home) => {
                let home = PathBuf::from(home);
                let custom_home = home != default_home;
                (home, custom_home)
            }
            None => (default_home, false),
        };
        let paths = Paths::under(home, custom_home);
        paths.ensure_dirs()?;
        Ok(paths)
    }

    /// Pure mapping of a home directory to all state paths.
    pub fn under(home: PathBuf, custom_home: bool) -> Paths {
        Paths {
            db: home.join("hostler.db"),
            service_lock: home.join("service.lock"),
            log_dir: home.join("logs"),
            socket: socket_name(&home),
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
