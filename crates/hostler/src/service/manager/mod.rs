//! OS service registration (plan Task 10): launchd on macOS, systemd on Linux, Task Scheduler
//! on Windows. Definitions are rendered by pure functions so every platform can test them.

pub mod launchd;
pub mod systemd;
pub mod windows;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::contract::HostlerError;
use crate::paths::Paths;
use crate::service::lock::{self, ServiceLock};
use crate::service::{ServiceManager, StopRefused, request_stop};

#[cfg(target_os = "macos")]
use launchd as imp;
#[cfg(target_os = "linux")]
use systemd as imp;
#[cfg(windows)]
use windows as imp;

/// Starts the service through the OS registration and asks whether one exists.
#[derive(Clone, Copy)]
pub struct OsManager;

/// What the OS registration for the default home is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationState {
    Absent,
    /// Starts `hostler service run --managed`.
    Current,
    /// Written by a hostler from before `--managed`: its service steps aside and never serves.
    Outdated,
    /// It could not be read.
    Unknown,
}

impl OsManager {
    /// Reads the installed definition (may run `schtasks` on Windows).
    pub fn registration(&self) -> RegistrationState {
        imp::registration()
    }
}

impl ServiceManager for OsManager {
    fn is_installed(&self) -> bool {
        imp::is_installed()
    }

    fn start(&self) -> anyhow::Result<()> {
        imp::start()
    }
}

/// Writes and loads the OS registration for the default home only. Low level: it replaces a
/// loaded registration (which stops its process). `install_service` is the safe entry point.
pub fn install(exe: &Path, paths: &Paths) -> anyhow::Result<()> {
    if paths.custom_home {
        anyhow::bail!(CUSTOM_HOME_REFUSAL);
    }
    imp::install(exe, paths)
}

const CUSTOM_HOME_REFUSAL: &str = "OS service registration only applies to the default hostler \
    home; HOSTLER_HOME is set, so run `hostler service run` or let clients start it";

/// Changes the OS registration; `OsManager` in production, a fake in tests.
pub trait Registrar: ServiceManager {
    fn register(&self, exe: &Path, paths: &Paths) -> anyhow::Result<()>;
    fn unregister(&self) -> anyhow::Result<()>;
}

impl Registrar for OsManager {
    fn register(&self, exe: &Path, paths: &Paths) -> anyhow::Result<()> {
        install(exe, paths)
    }

    fn unregister(&self) -> anyhow::Result<()> {
        imp::uninstall()
    }
}

/// Why a registration change did not happen.
#[derive(Debug)]
pub enum ChangeRefused {
    /// `HOSTLER_HOME` points elsewhere; nothing was touched.
    CustomHome,
    /// Heavy entries run and `force` was not given; nothing was touched.
    Busy(HostlerError),
    Failed(String),
}

impl std::fmt::Display for ChangeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeRefused::CustomHome => f.write_str(CUSTOM_HOME_REFUSAL),
            ChangeRefused::Busy(e) => f.write_str(&e.message),
            ChangeRefused::Failed(message) => f.write_str(message),
        }
    }
}

/// How long `quiesce` keeps stopping services that race it for the lock.
const QUIESCE_WAIT: Duration = Duration::from_secs(15);

/// Takes `service.lock` itself, stopping (idle only, unless `force`) whichever service holds it,
/// so no service — started by the OS manager at load, or by a client — can serve, and so grant
/// slots, while the registration changes. The lock is tried on every pass, so services racing
/// for it are stopped one by one until the installer wins.
async fn quiesce(paths: &Paths, force: bool) -> Result<ServiceLock, ChangeRefused> {
    let deadline = Instant::now() + QUIESCE_WAIT;
    loop {
        match lock::try_hold(&paths.service_lock) {
            Ok(Some(held)) => return Ok(held),
            Ok(None) => {}
            Err(e) => return Err(ChangeRefused::Failed(format!("{e:#}"))),
        }
        match request_stop(paths, force).await {
            // Not answering yet (still starting) or already stopping: try the lock again.
            Ok(_) => {}
            Err(StopRefused::Busy(e)) => return Err(ChangeRefused::Busy(e)),
            Err(StopRefused::Failed(e)) => return Err(ChangeRefused::Failed(e.message)),
        }
        if Instant::now() >= deadline {
            return Err(ChangeRefused::Failed(format!(
                "hostler services kept holding {} for {QUIESCE_WAIT:?}",
                paths.service_lock.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `hostler service install`: refuses a custom home before anything else, stops the running
/// service (refused while heavy entries run unless `force`), replaces the registration while
/// holding the service lock, then starts the service through the new registration.
pub async fn install_service(
    paths: &Paths,
    force: bool,
    exe: &Path,
    registrar: Arc<dyn Registrar>,
) -> Result<(), ChangeRefused> {
    if paths.custom_home {
        return Err(ChangeRefused::CustomHome);
    }
    let held = quiesce(paths, force).await?;
    // A service the manager starts at load finds the lock held and exits 0 (no restart).
    let registered = registrar.register(exe, paths);
    drop(held);
    registered.map_err(|e| ChangeRefused::Failed(format!("{e:#}")))?;
    Client::connect_or_start_with(paths, &registrar, exe)
        .await
        .map(drop)
        .map_err(|e| {
            ChangeRefused::Failed(format!(
                "registered, but the service did not start: {}",
                e.message
            ))
        })
}

/// `hostler service uninstall`: the same guards as `install_service`; the service is left
/// stopped (clients start it directly from now on).
pub async fn uninstall_service(
    paths: &Paths,
    force: bool,
    registrar: &dyn Registrar,
) -> Result<(), ChangeRefused> {
    if paths.custom_home {
        return Err(ChangeRefused::CustomHome);
    }
    let held = quiesce(paths, force).await?;
    let removed = registrar.unregister();
    drop(held);
    removed.map_err(|e| ChangeRefused::Failed(format!("{e:#}")))
}

/// The registration file for this platform, as it would be written.
pub fn render_definition(exe: &Path, paths: &Paths) -> String {
    imp::render_definition(exe, paths)
}

/// A finished manager command: enough evidence to tell "absent" from a real failure.
pub(crate) struct CommandOutcome {
    pub(crate) success: bool,
    pub(crate) text: String,
}

impl CommandOutcome {
    pub(crate) fn from_output(output: std::process::Output) -> Self {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&stderr);
        }
        CommandOutcome {
            success: output.status.success(),
            text,
        }
    }

    #[cfg(test)]
    pub(crate) fn new(success: bool, text: &str) -> Self {
        CommandOutcome {
            success,
            text: text.to_owned(),
        }
    }

    /// True only for success or a message that positively proves the registration is absent.
    pub(crate) fn is_absent(&self, markers: &[&str]) -> bool {
        if self.success {
            return true;
        }
        let text = self.text.to_ascii_lowercase();
        markers.iter().any(|marker| text.contains(marker))
    }
}

pub(crate) fn ensure_success(outcome: &CommandOutcome, what: &str) -> anyhow::Result<()> {
    anyhow::ensure!(outcome.success, "{what} failed: {}", outcome.text.trim());
    Ok(())
}

/// What looking up a registration file proved.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationFile {
    Present,
    Missing,
    Unknown,
}

#[cfg(unix)]
pub(crate) fn registration_file(path: &Path) -> RegistrationFile {
    match std::fs::symlink_metadata(path) {
        Ok(_) => RegistrationFile::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RegistrationFile::Missing,
        Err(_) => RegistrationFile::Unknown,
    }
}

/// The state of a registration kept in a file, judged by the platform's `is_current`.
#[cfg(unix)]
pub(crate) fn registration_of_file(
    path: Option<&Path>,
    is_current: fn(&str) -> bool,
) -> RegistrationState {
    let Some(path) = path else {
        return RegistrationState::Unknown;
    };
    match std::fs::read_to_string(path) {
        Ok(text) if is_current(&text) => RegistrationState::Current,
        Ok(_) => RegistrationState::Outdated,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RegistrationState::Absent,
        Err(_) => RegistrationState::Unknown,
    }
}

/// Only a definitively missing file proves there is no registration; an unreadable one does not.
#[cfg(unix)]
pub(crate) fn registration_installed(file: RegistrationFile) -> bool {
    file != RegistrationFile::Missing
}

pub(crate) fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_registration_file_is_confirmed_absence() {
        assert!(!registration_installed(RegistrationFile::Missing));
        assert!(registration_installed(RegistrationFile::Present));
        assert!(registration_installed(RegistrationFile::Unknown));
    }

    #[test]
    fn registration_of_file_tells_current_from_outdated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("io.github.dzamataev.hostler.plist");
        let current = launchd::definition_is_current;
        assert_eq!(
            registration_of_file(Some(&path), current),
            RegistrationState::Absent
        );
        std::fs::write(&path, "<string>service</string>\n<string>run</string>\n").unwrap();
        assert_eq!(
            registration_of_file(Some(&path), current),
            RegistrationState::Outdated
        );
        std::fs::write(&path, "<string>run</string>\n<string>--managed</string>\n").unwrap();
        assert_eq!(
            registration_of_file(Some(&path), current),
            RegistrationState::Current
        );
        assert_eq!(
            registration_of_file(None, current),
            RegistrationState::Unknown
        );
    }

    #[test]
    fn registration_file_reports_presence() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            registration_file(&dir.path().join("absent")),
            RegistrationFile::Missing
        );
        let file = dir.path().join("present");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(registration_file(&file), RegistrationFile::Present);
    }
}
