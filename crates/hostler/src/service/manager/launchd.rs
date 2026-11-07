//! macOS launchd agent (plan Task 10).

use std::path::Path;

use crate::paths::Paths;

use super::xml_escape;

pub const LABEL: &str = "io.github.dzamataev.hostler";

pub fn render_definition(exe: &Path, paths: &Paths) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let stdout = xml_escape(&paths.log_dir.join("service.out").display().to_string());
    let stderr = xml_escape(&paths.log_dir.join("service.err").display().to_string());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>service</string>
    <string>run</string>
    <string>--managed</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>1</integer>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#
    )
}

/// A plist from before `--managed` starts a service that steps aside.
pub fn definition_is_current(plist: &str) -> bool {
    plist.contains("<string>--managed</string>")
}

#[cfg(target_os = "macos")]
pub fn registration() -> super::RegistrationState {
    super::registration_of_file(plist_path().as_deref(), definition_is_current)
}

/// `launchctl bootout` answers "No such process"/"Could not find service" when nothing is loaded;
/// every other failure is a real error.
#[cfg(any(target_os = "macos", test))]
fn unload_absent(outcome: &super::CommandOutcome) -> bool {
    outcome.is_absent(&["no such process", "could not find service"])
}

#[cfg(target_os = "macos")]
fn plist_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
    )
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> anyhow::Result<super::CommandOutcome> {
    use anyhow::Context;

    let output = std::process::Command::new("launchctl")
        .args(args)
        .output()
        .context("cannot run launchctl")?;
    Ok(super::CommandOutcome::from_output(output))
}

#[cfg(target_os = "macos")]
pub fn install(exe: &Path, paths: &Paths) -> anyhow::Result<()> {
    let path = plist_path()
        .ok_or_else(|| anyhow::anyhow!("cannot find HOME for the launch agent path"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, render_definition(exe, paths))
        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;

    let target = format!("gui/{}", current_uid());
    // Replacing a loaded agent requires bootout first; an absent agent is fine.
    let label = format!("{target}/{LABEL}");
    let outcome = launchctl(&["bootout", &label])?;
    if !unload_absent(&outcome) {
        anyhow::bail!(
            "cannot replace the loaded launch agent {LABEL}: {}",
            outcome.text.trim()
        );
    }
    let plist = path.display().to_string();
    let outcome = launchctl(&["bootstrap", &target, &plist])?;
    super::ensure_success(&outcome, "launchctl bootstrap")
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> anyhow::Result<()> {
    let target = format!("gui/{}", current_uid());
    let label = format!("{target}/{LABEL}");
    let outcome = launchctl(&["bootout", &label])?;
    if !unload_absent(&outcome) {
        anyhow::bail!(
            "cannot unload the launch agent {LABEL}: {}",
            outcome.text.trim()
        );
    }
    if let Some(path) = plist_path()
        && path.exists()
    {
        std::fs::remove_file(&path)
            .map_err(|e| anyhow::anyhow!("cannot remove {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn is_installed() -> bool {
    let Some(path) = plist_path() else {
        tracing::warn!("cannot find HOME, so the launch agent registration cannot be checked");
        return true;
    };
    let file = super::registration_file(&path);
    if file == super::RegistrationFile::Unknown {
        tracing::warn!(
            "cannot check {}, assuming the launch agent is installed",
            path.display()
        );
    }
    super::registration_installed(file)
}

#[cfg(target_os = "macos")]
pub fn start() -> anyhow::Result<()> {
    let target = format!("gui/{}/{LABEL}", current_uid());
    let outcome = launchctl(&["kickstart", &target])?;
    super::ensure_success(&outcome, "launchctl kickstart")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::manager::CommandOutcome;

    #[test]
    fn unload_tolerates_only_a_confirmed_absence() {
        assert!(unload_absent(&CommandOutcome::new(true, "")));
        assert!(unload_absent(&CommandOutcome::new(
            false,
            "Boot-out failed: 3: No such process"
        )));
        assert!(unload_absent(&CommandOutcome::new(
            false,
            "Could not find service \"io.github.dzamataev.hostler\" in domain for user gui: 501"
        )));
        assert!(!unload_absent(&CommandOutcome::new(
            false,
            "Boot-out failed: 1: Operation not permitted"
        )));
        assert!(!unload_absent(&CommandOutcome::new(
            false,
            "Bootstrap failed: 5: Input/output error"
        )));
    }
}
