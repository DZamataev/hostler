//! OS service registration (plan Task 10): launchd on macOS, systemd on Linux, Task Scheduler
//! on Windows. Definitions are rendered by pure functions so every platform can test them.

pub mod launchd;
pub mod systemd;
pub mod windows;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::client::{Client, Replace};
use crate::contract::HostlerError;
use crate::paths::Paths;
use crate::service::lifecycle::{StopRefusal, StopRule};
use crate::service::lock::{self, ServiceLock};
use crate::service::{ServiceManager, StopRefused, Stopping, request_stop};

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

    /// Notes on the installed definition that change nothing about its state (P2-13): Linux
    /// `unit_lacks_killmode`; none elsewhere.
    pub fn registration_notes(&self) -> Vec<&'static str> {
        #[cfg(target_os = "linux")]
        return systemd::notes();
        #[cfg(not(target_os = "linux"))]
        Vec::new()
    }
}

/// The text `service status` prints for a registration note.
pub fn note_text(note: &str) -> Option<&'static str> {
    (note == systemd::UNIT_LACKS_KILLMODE).then_some(
        "note: the unit lacks KillMode=process: a service stop also stops the adb server and \
         hostler-booted emulators; run hostler service install (the install stops the service \
         once more under the old unit, which stops them too)",
    )
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
    /// The installed definition, read before a change (`Automatic(Maintain)` needs it). A
    /// registrar that cannot tell answers `Unknown`, which no automatic rule re-registers.
    fn registration(&self) -> RegistrationState {
        RegistrationState::Unknown
    }
}

impl Registrar for OsManager {
    fn register(&self, exe: &Path, paths: &Paths) -> anyhow::Result<()> {
        install(exe, paths)
    }

    fn unregister(&self) -> anyhow::Result<()> {
        imp::uninstall()
    }

    fn registration(&self) -> RegistrationState {
        OsManager::registration(self)
    }
}

/// Why a registration change did not happen.
#[derive(Debug)]
pub enum ChangeRefused {
    /// `HOSTLER_HOME` points elsewhere; nothing was touched.
    CustomHome,
    /// The service answered `ServiceBusy` (heavy entries, leases, a takeover, or for an
    /// automatic rule an unlocked database: `details.secrets`); nothing was touched.
    Busy(HostlerError),
    /// `may_stop` refused the rule (plan P4 R2-2); nothing was stopped or registered.
    NotStopped {
        reason: StopRefusal,
    },
    Failed(String),
}

impl std::fmt::Display for ChangeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeRefused::CustomHome => f.write_str(CUSTOM_HOME_REFUSAL),
            ChangeRefused::Busy(e) => f.write_str(&e.message),
            ChangeRefused::NotStopped { reason } => {
                write!(
                    f,
                    "{}; to do it anyway: {}",
                    reason.message(),
                    reason.recovery(None)
                )
            }
            ChangeRefused::Failed(message) => f.write_str(message),
        }
    }
}

/// What `install_service` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Installed {
    /// A running service was stopped for the change.
    pub stopped: bool,
}

/// How long `quiesce` keeps stopping services that race it for the lock.
const QUIESCE_WAIT: Duration = Duration::from_secs(15);

/// The lock `quiesce` took, and whether it stopped a service for it.
struct Held {
    _lock: ServiceLock,
    stopped: bool,
}

/// Takes `service.lock` itself, stopping whichever service holds it under `stop` (plan P4
/// R2-2: every stop is probe → `may_stop` → `service.shutdown`, on one connection), so no
/// service — started by the OS manager at load, or by a client — can serve, and so grant slots,
/// while the registration changes. The lock is tried on every pass, so services racing for it
/// are stopped one by one until the installer wins; each of them is judged by `may_stop` again.
async fn quiesce(
    paths: &Paths,
    stop: StopRule,
    registration: RegistrationState,
) -> Result<Held, ChangeRefused> {
    let deadline = Instant::now() + QUIESCE_WAIT;
    let mut stopped = false;
    loop {
        match lock::try_hold(&paths.service_lock) {
            Ok(Some(held)) => {
                return Ok(Held {
                    _lock: held,
                    stopped,
                });
            }
            Ok(None) => {}
            Err(e) => return Err(ChangeRefused::Failed(format!("{e:#}"))),
        }
        match request_stop(paths, stop, registration).await {
            Ok(Stopping::Accepted { .. }) => stopped = true,
            // Not answering yet (still starting) or already stopping: try the lock again.
            Ok(Stopping::NotRunning) => {}
            Err(StopRefused::Busy(e)) => return Err(ChangeRefused::Busy(e)),
            Err(StopRefused::NotStopped(reason)) => {
                return Err(ChangeRefused::NotStopped { reason });
            }
            // `sandbox_denied` among them: nothing was stopped, and it stays itself.
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

/// Changes the OS registration: refuses a custom home before anything else, stops the running
/// service under `stop`, replaces the registration while holding the service lock, then starts
/// the service through the new registration. Callers name their rule (plan P4 R2-2):
/// - typed `hostler service install`: `StopRule::Explicit { force }`;
/// - `init --write` and `doctor --fix` re-registering an `Absent`/`Outdated` definition:
///   `Automatic(Maintain)`;
/// - `doctor --fix` for a contract mismatch: `Automatic(Replace)`.
///
/// Automatic callers check `is_dev_build(exe)` first (D-Q2); this function never does.
pub async fn install_service(
    paths: &Paths,
    exe: &Path,
    registrar: Arc<dyn Registrar>,
    stop: StopRule,
) -> Result<Installed, ChangeRefused> {
    if paths.custom_home {
        return Err(ChangeRefused::CustomHome);
    }
    let registration = registration_for(stop, &*registrar);
    let held = quiesce(paths, stop, registration).await?;
    let stopped = held.stopped;
    // A service the manager starts at load finds the lock held and exits 0 (no restart).
    let registered = registrar.register(exe, paths);
    drop(held);
    registered.map_err(|e| ChangeRefused::Failed(format!("{e:#}")))?;
    Client::connect_or_start_with(paths, &registrar, exe, Replace::Never)
        .await
        .map(drop)
        .map_err(|e| {
            ChangeRefused::Failed(format!(
                "registered, but the service did not start: {}",
                e.message
            ))
        })?;
    Ok(Installed { stopped })
}

/// The registration `may_stop` judges: read only for `Automatic(Maintain)`, the one rule it
/// matters to (a `schtasks` query is slow).
fn registration_for(stop: StopRule, registrar: &dyn Registrar) -> RegistrationState {
    match stop {
        StopRule::Automatic(crate::service::lifecycle::AutoPurpose::Maintain) => {
            registrar.registration()
        }
        _ => RegistrationState::Unknown,
    }
}

/// `hostler service uninstall`: the same guards as `install_service`, always `Explicit`; the
/// service is left stopped (clients start it directly from now on).
pub async fn uninstall_service(
    paths: &Paths,
    force: bool,
    registrar: &dyn Registrar,
) -> Result<(), ChangeRefused> {
    if paths.custom_home {
        return Err(ChangeRefused::CustomHome);
    }
    let held = quiesce(
        paths,
        StopRule::Explicit { force },
        RegistrationState::Unknown,
    )
    .await?;
    let removed = registrar.unregister();
    drop(held);
    removed.map_err(|e| ChangeRefused::Failed(format!("{e:#}")))
}

/// The hostler binary the OS registration runs, when there is one and it can be read (R1-10):
/// launchd's `ProgramArguments[0]`, systemd's `ExecStart` first word, the Windows task's
/// `<Arguments>` (never its `<Command>`, which is conhost). May run `schtasks` on Windows.
pub fn registered_exe() -> Option<PathBuf> {
    imp::registered_exe()
}

/// D-Q2: whether `exe` is a development build: an ancestor directory of the canonicalized exe
/// holds `CACHEDIR.TAG` (cargo's target-dir marker, also under a custom `CARGO_TARGET_DIR`) or
/// `Cargo.toml` (a checkout). `cargo install` (`~/.cargo/bin`) and Homebrew paths have neither.
/// Automatic installs (init, `doctor --fix`) never register one; a typed `hostler service
/// install` registers whatever binary runs it.
pub fn is_dev_build(exe: &Path) -> bool {
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    exe.ancestors()
        .skip(1)
        .any(|dir| dir.join("CACHEDIR.TAG").exists() || dir.join("Cargo.toml").exists())
}

/// Undoes `xml_escape` (and the numeric quote Task Scheduler may return).
pub(crate) fn xml_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The registration file for this platform, as it would be written.
pub fn render_definition(exe: &Path, paths: &Paths) -> String {
    imp::render_definition(exe, paths)
}

/// A finished manager command: enough evidence to tell "absent" from a real failure.
/// The longest one `launchctl`, `systemctl` or `schtasks` call may take before it is killed (P4
/// Task 6 review t6-r2 R2-1): a registration change in flight is bounded, never abandoned, and
/// never hangs its caller. A launchd install makes at most three calls (bootout, bootstrap,
/// kickstart), so the OS part of one change takes at most 30 s.
pub(crate) const MANAGER_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// `command.output()`, killed (and reaped) after `limit`: `ErrorKind::TimedOut` then. stdin is
/// null; stdout and stderr are read on their own threads, so a full pipe never stalls it, and a
/// grandchild that keeps a pipe open costs at most a short wait.
pub(crate) fn output_within(
    command: &mut std::process::Command,
    limit: Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            let _ = tx.send(bytes);
        });
        rx
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("did not finish within {limit:?} and was stopped"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let collect =
        |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

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
        // P4: `--managed` alone is outdated; `ProcessType` `Interactive` is current.
        std::fs::write(&path, "<string>run</string>\n<string>--managed</string>\n").unwrap();
        assert_eq!(
            registration_of_file(Some(&path), current),
            RegistrationState::Outdated
        );
        std::fs::write(
            &path,
            "<key>ProcessType</key>\n<string>Interactive</string>\n<string>run</string>\n\
             <string>--managed</string>\n",
        )
        .unwrap();
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

#[cfg(test)]
mod dev_build_tests {
    use super::*;

    /// D-Q2: a cargo target dir (`CACHEDIR.TAG`) or a checkout (`Cargo.toml`) above the exe
    /// makes it a dev build; a plain directory does not.
    #[test]
    fn is_dev_build_under_cachedir_tag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CACHEDIR.TAG"), "Signature").unwrap();
        let exe = dir.path().join("debug").join("hostler");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        assert!(is_dev_build(&exe));
    }

    #[test]
    fn is_dev_build_under_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let exe = dir.path().join("bin").join("hostler");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        assert!(is_dev_build(&exe));
    }

    #[test]
    fn is_not_a_dev_build_in_a_plain_dir() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("bin").join("hostler");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        // A temp dir has no such marker above it on any OS's default.
        assert!(!is_dev_build(&exe));
    }

    #[test]
    fn xml_unescape_undoes_xml_escape() {
        let text = r#"C:\a & b\<c>\"d"\'e'"#;
        assert_eq!(xml_unescape(&xml_escape(text)), text);
        assert_eq!(xml_unescape("&amp;quot;"), "&quot;");
    }
}

#[cfg(all(test, unix))]
mod output_within_tests {
    use super::*;

    /// R2-1 (P4 Task 6 review t6-r2): a manager call that hangs is killed at its bound.
    #[test]
    fn a_hung_call_is_killed_at_the_bound() {
        let started = Instant::now();
        let error = output_within(
            std::process::Command::new("sh").args(["-c", "sleep 30"]),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let output = output_within(
            std::process::Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
    }
}
