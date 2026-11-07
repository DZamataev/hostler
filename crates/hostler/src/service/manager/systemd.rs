//! Linux systemd user unit (plan Task 10).

use std::path::Path;

use crate::paths::Paths;

pub const UNIT_NAME: &str = "hostler.service";

pub fn render_definition(exe: &Path, _paths: &Paths) -> String {
    format!(
        "[Unit]\n\
         Description=hostler per-user service\n\
         \n\
         [Service]\n\
         ExecStart={} service run --managed\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        quote_exec(exe)
    )
}

/// Quotes for systemd's command line and escapes `%`, which systemd expands even inside quotes.
fn quote_exec(exe: &Path) -> String {
    let path = exe.display().to_string().replace('%', "%%");
    if path
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        path
    }
}

/// A unit from before `--managed` starts a service that steps aside.
pub fn definition_is_current(unit: &str) -> bool {
    unit.lines().any(|line| {
        line.trim_start().starts_with("ExecStart=") && line.trim_end().ends_with(" --managed")
    })
}

#[cfg(target_os = "linux")]
pub fn registration() -> super::RegistrationState {
    super::registration_of_file(unit_path().as_deref(), definition_is_current)
}

/// Only a diagnostic naming hostler.service itself as missing proves absence. Generic text such
/// as "No such file or directory" also appears in bus failures ("Failed to connect to bus: No
/// such file or directory") and proves nothing about the unit.
#[cfg(any(target_os = "linux", test))]
fn disable_absent(outcome: &super::CommandOutcome) -> bool {
    outcome.is_absent(&[
        "unit file hostler.service does not exist",
        "unit hostler.service does not exist",
        "unit hostler.service not loaded",
    ])
}

/// `systemctl --user disable --now`, then remove the unit file and reload. The file is kept
/// whenever the disable failed for any reason other than a confirmed missing unit.
#[cfg(any(target_os = "linux", test))]
fn uninstall_with(
    systemctl: &dyn Fn(&[&str]) -> anyhow::Result<super::CommandOutcome>,
    unit: Option<&Path>,
) -> anyhow::Result<()> {
    let outcome = systemctl(&["disable", "--now", UNIT_NAME])?;
    if !disable_absent(&outcome) {
        anyhow::bail!("cannot disable {UNIT_NAME}: {}", outcome.text.trim());
    }
    if let Some(path) = unit
        && path.exists()
    {
        std::fs::remove_file(path)
            .map_err(|e| anyhow::anyhow!("cannot remove {}: {e}", path.display()))?;
    }
    let outcome = systemctl(&["daemon-reload"])?;
    super::ensure_success(&outcome, "systemctl --user daemon-reload")
}

#[cfg(target_os = "linux")]
fn unit_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config/systemd/user")
            .join(UNIT_NAME),
    )
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> anyhow::Result<super::CommandOutcome> {
    use anyhow::Context;

    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("cannot run systemctl --user")?;
    Ok(super::CommandOutcome::from_output(output))
}

#[cfg(target_os = "linux")]
fn require_user_systemd() -> anyhow::Result<()> {
    use anyhow::Context;

    let output = std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .context(
            "systemctl --user is unavailable; hostler will keep starting the service directly",
        )?;
    anyhow::ensure!(
        output.status.success(),
        "systemctl --user is unavailable (no user systemd instance); \
         hostler will keep starting the service directly"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn install(exe: &Path, paths: &Paths) -> anyhow::Result<()> {
    let path =
        unit_path().ok_or_else(|| anyhow::anyhow!("cannot find HOME for the systemd user unit"))?;
    require_user_systemd()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, render_definition(exe, paths))
        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
    let outcome = systemctl(&["daemon-reload"])?;
    super::ensure_success(&outcome, "systemctl --user daemon-reload")?;
    let outcome = systemctl(&["enable", "--now", UNIT_NAME])?;
    super::ensure_success(&outcome, "systemctl --user enable --now")
}

#[cfg(target_os = "linux")]
pub fn uninstall() -> anyhow::Result<()> {
    uninstall_with(&systemctl, unit_path().as_deref())
}

#[cfg(target_os = "linux")]
pub fn is_installed() -> bool {
    let Some(path) = unit_path() else {
        tracing::warn!("cannot find HOME, so the systemd user unit cannot be checked");
        return true;
    };
    let file = super::registration_file(&path);
    if file == super::RegistrationFile::Unknown {
        tracing::warn!(
            "cannot check {}, assuming the systemd user unit is installed",
            path.display()
        );
    }
    super::registration_installed(file)
}

#[cfg(target_os = "linux")]
pub fn start() -> anyhow::Result<()> {
    let outcome = systemctl(&["start", UNIT_NAME])?;
    super::ensure_success(&outcome, "systemctl --user start")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::manager::CommandOutcome;

    #[test]
    fn disable_tolerates_only_a_confirmed_absence() {
        assert!(disable_absent(&CommandOutcome::new(true, "")));
        assert!(disable_absent(&CommandOutcome::new(
            false,
            "Failed to disable unit: Unit file hostler.service does not exist."
        )));
        assert!(disable_absent(&CommandOutcome::new(
            false,
            "Failed to disable unit: Unit hostler.service not loaded."
        )));
        assert!(!disable_absent(&CommandOutcome::new(
            false,
            "Failed to disable unit: Access denied"
        )));
        assert!(!disable_absent(&CommandOutcome::new(
            false,
            "Failed to connect to bus: No medium found"
        )));
        assert!(!disable_absent(&CommandOutcome::new(
            false,
            "Failed to connect to bus: No such file or directory"
        )));
    }

    #[test]
    fn bus_failure_keeps_the_unit_file_and_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join(UNIT_NAME);
        std::fs::write(&unit, "[Unit]\n").unwrap();
        let calls = std::cell::RefCell::new(Vec::new());
        let bus_down = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            Ok(CommandOutcome::new(
                false,
                "Failed to connect to bus: No such file or directory",
            ))
        };
        let err = uninstall_with(&bus_down, Some(&unit)).unwrap_err();
        assert!(
            format!("{err:#}").contains("Failed to connect to bus"),
            "{err:#}"
        );
        assert!(
            unit.exists(),
            "the unit file was removed after a failed disable"
        );
        assert_eq!(*calls.borrow(), ["disable --now hostler.service"]);
    }

    #[test]
    fn confirmed_missing_unit_removes_the_file_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join(UNIT_NAME);
        std::fs::write(&unit, "[Unit]\n").unwrap();
        let calls = std::cell::RefCell::new(Vec::new());
        let missing = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            Ok(match args[0] {
                "disable" => CommandOutcome::new(
                    false,
                    "Failed to disable unit: Unit file hostler.service does not exist.",
                ),
                _ => CommandOutcome::new(true, ""),
            })
        };
        uninstall_with(&missing, Some(&unit)).unwrap();
        assert!(!unit.exists());
        assert_eq!(
            *calls.borrow(),
            ["disable --now hostler.service", "daemon-reload"]
        );
    }

    #[test]
    fn percent_in_the_executable_is_a_specifier_and_is_escaped() {
        let unit = render_definition(Path::new("/home/alice/build%h/hostler"), &sample_paths());
        assert!(unit.contains("ExecStart=/home/alice/build%%h/hostler service run --managed"));
    }

    fn sample_paths() -> Paths {
        Paths::under(
            std::path::PathBuf::from("/home/alice/.local/share/hostler"),
            false,
        )
    }
}
