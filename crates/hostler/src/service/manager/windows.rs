//! Windows Task Scheduler registration (plan Task 10). `schtasks` takes the XML file; the task
//! runs at logon of the current user. It starts hostler through `conhost.exe --headless`
//! (Windows 10 1809+): a console program started directly by the task gets a visible console
//! window on the user's desktop.

use std::path::Path;

use crate::paths::Paths;

use super::xml_escape;

pub const TASK_NAME: &str = "hostler";

/// Task Scheduler expands the variable when it starts the task.
pub const CONHOST: &str = r"%SystemRoot%\System32\conhost.exe";

/// What conhost runs headless. A Windows path cannot contain `"`, so plain quotes suffice.
fn conhost_arguments(exe: &Path) -> String {
    format!("--headless \"{}\" service run --managed", exe.display())
}

pub fn render_definition(exe: &Path, _paths: &Paths) -> String {
    let command = xml_escape(CONHOST);
    let arguments = xml_escape(&conhost_arguments(exe));

    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>hostler per-user service</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// Current: conhost runs `hostler service run --managed` headless. Older tasks either start a
/// service that steps aside (no `--managed`) or one with a visible console (no conhost).
/// Task Scheduler may return the quotes raw or escaped, so they are not compared.
pub fn definition_is_current(xml: &str) -> bool {
    let (Some(command), Some(arguments)) = (element(xml, "Command"), element(xml, "Arguments"))
    else {
        return false;
    };
    command
        .trim()
        .to_ascii_lowercase()
        .ends_with(r"\conhost.exe")
        && arguments.trim_start().starts_with("--headless ")
        && arguments.trim_end().ends_with(" service run --managed")
}

/// The text of the first `<tag>…</tag>` in `xml`.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    Some(&xml[start..end])
}

/// `schtasks /Query /XML` output: UTF-16 when it carries a BOM or NUL bytes, else UTF-8.
#[cfg(any(windows, test))]
fn decode_query_output(bytes: &[u8]) -> String {
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || (bytes.len() >= 2 && bytes[1] == 0);
    if utf16 {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_owned()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(windows)]
pub fn registration() -> super::RegistrationState {
    use super::RegistrationState;

    let output = match std::process::Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/XML"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return RegistrationState::Unknown,
    };
    if output.status.success() {
        return if definition_is_current(&decode_query_output(&output.stdout)) {
            RegistrationState::Current
        } else {
            RegistrationState::Outdated
        };
    }
    if task_absent(&super::CommandOutcome::from_output(output)) {
        RegistrationState::Absent
    } else {
        RegistrationState::Unknown
    }
}

/// `schtasks` says "cannot find"/"does not exist" for an absent task; anything else is a failure.
#[cfg(any(windows, test))]
fn task_absent(outcome: &super::CommandOutcome) -> bool {
    outcome.is_absent(&["cannot find", "does not exist"])
}

/// A query is only proof of absence when it positively names a missing task. Any other failure
/// (access denied, unavailable scheduler/RPC) assumes the task exists, so clients never fall back
/// to a direct spawn inside an agent's sandbox.
#[cfg(any(windows, test))]
fn installed_from_query(result: anyhow::Result<super::CommandOutcome>) -> bool {
    match result {
        Ok(outcome) if outcome.success => true,
        Ok(outcome) if task_absent(&outcome) => false,
        Ok(outcome) => {
            tracing::warn!(
                "cannot check the hostler task, assuming it is installed: {}",
                outcome.text.trim()
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                "cannot run schtasks /Query, assuming the hostler task is installed: {e:#}"
            );
            true
        }
    }
}

#[cfg(windows)]
fn write_task_xml(path: &Path, xml: &str) -> std::io::Result<()> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes)
}

#[cfg(windows)]
fn schtasks(args: &[&str]) -> anyhow::Result<super::CommandOutcome> {
    use anyhow::Context;

    let output = std::process::Command::new("schtasks")
        .args(args)
        .output()
        .context("cannot run schtasks")?;
    Ok(super::CommandOutcome::from_output(output))
}

#[cfg(windows)]
pub fn install(exe: &Path, paths: &Paths) -> anyhow::Result<()> {
    let task_file = std::env::temp_dir().join("hostler-task.xml");
    write_task_xml(&task_file, &render_definition(exe, paths))
        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", task_file.display()))?;
    let file = task_file.display().to_string();
    let result = schtasks(&["/Create", "/TN", TASK_NAME, "/XML", file.as_str(), "/F"])
        .and_then(|outcome| super::ensure_success(&outcome, "schtasks /Create"));
    let _ = std::fs::remove_file(&task_file);
    result
}

#[cfg(windows)]
pub fn uninstall() -> anyhow::Result<()> {
    let outcome = schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])?;
    if !task_absent(&outcome) {
        anyhow::bail!("cannot delete the hostler task: {}", outcome.text.trim());
    }
    Ok(())
}

#[cfg(windows)]
pub fn is_installed() -> bool {
    installed_from_query(schtasks(&["/Query", "/TN", TASK_NAME]))
}

#[cfg(windows)]
pub fn start() -> anyhow::Result<()> {
    let outcome = schtasks(&["/Run", "/TN", TASK_NAME])?;
    super::ensure_success(&outcome, "schtasks /Run")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::manager::CommandOutcome;

    #[test]
    fn delete_tolerates_only_a_confirmed_absence() {
        assert!(task_absent(&CommandOutcome::new(true, "")));
        assert!(task_absent(&CommandOutcome::new(
            false,
            "ERROR: The system cannot find the file specified."
        )));
        assert!(task_absent(&CommandOutcome::new(
            false,
            "ERROR: The specified task name \"hostler\" does not exist in the system."
        )));
        assert!(!task_absent(&CommandOutcome::new(
            false,
            "ERROR: Access is denied."
        )));
        assert!(!task_absent(&CommandOutcome::new(
            false,
            "ERROR: The RPC server is unavailable."
        )));
    }

    #[test]
    fn query_output_decodes_utf16_and_utf8() {
        let xml = "<Arguments>service run --managed</Arguments>";
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_query_output(&utf16), xml);
        assert_eq!(decode_query_output(xml.as_bytes()), xml);
    }

    #[test]
    fn query_failure_is_not_proof_of_absence() {
        assert!(installed_from_query(Ok(CommandOutcome::new(
            true,
            "TaskName          Status\nhostler           Ready"
        ))));
        assert!(!installed_from_query(Ok(CommandOutcome::new(
            false,
            "ERROR: The system cannot find the file specified."
        ))));

        let access_denied =
            installed_from_query(Ok(CommandOutcome::new(false, "ERROR: Access is denied.")));
        assert!(access_denied);

        let no_schtasks = installed_from_query(Err(anyhow::anyhow!("cannot run schtasks")));
        assert!(no_schtasks);
    }
}
