//! `hostler secret list | set | remove | share | unshare | status` (plan P2, Task 8).

use std::ffi::OsString;
use std::io::Read;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::cli::output::report;
use crate::cli::secrets::{
    call, checked_name, current_project, internal, report_error, target_project,
    values_are_arguments,
};
use crate::cli::tty::{self, Tty};
use crate::contract::HostlerError;
use crate::contract::secrets::{
    LockState, SecretList, SecretSet, SecretSource, SecretState, SecretsStatus, SetOutcome, Share,
    errors,
};
use crate::contract::wire::Request;
use crate::secrets::value::SecretValue;
use crate::secrets::{MAX_VALUE_BYTES, sanitize_for_terminal};
use crate::service::harden::harden_value_holder;

#[derive(Debug, Deserialize, Serialize)]
struct RemoveOutcome {
    removed: bool,
}

pub fn main_status(json: bool) -> ExitCode {
    report(
        call::<SecretsStatus>(Request::SecretsStatus),
        json,
        print_status,
    )
}

pub fn main_list(json: bool) -> ExitCode {
    report(list(), json, print_list)
}

fn list() -> Result<SecretList, HostlerError> {
    let current = crate::project::current(&crate::cli::secrets::cwd()?)?;
    let declared = current
        .config
        .as_ref()
        .map(|config| config.declared())
        .unwrap_or_default();
    call(Request::SecretList {
        project: current.project,
        declared,
    })
}

pub fn main_set(name: String, extra: Vec<OsString>) -> ExitCode {
    if !extra.is_empty() {
        return values_are_arguments();
    }
    if let Err(code) = checked_name(&name) {
        return code;
    }
    match run_set(name) {
        Ok(code) => code,
        Err(e) => report_error(&e),
    }
}

fn run_set(name: String) -> Result<ExitCode, HostlerError> {
    let project = current_project()?;
    harden_value_holder();
    let value = if tty::interactive() {
        let mut tty = Tty::open("secret set")?;
        let prompt = format!(
            "Value for {name} ({}):",
            sanitize_for_terminal(&project.name)
        );
        tty.hidden(&prompt)?
    } else if tty::stdin_is_terminal() {
        return Err(errors::not_interactive("secret set"));
    } else {
        read_stdin_value()?
    };

    let outcome = call::<SetOutcome>(Request::SecretSet(SecretSet {
        project,
        name,
        value,
    }))?;
    println!("stored {}", outcome.name);
    if !outcome.answered_requests.is_empty() {
        println!(
            "answered request(s) {}",
            outcome.answered_requests.join(", ")
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// At most 64 KiB from stdin after one trailing `\n` or `\r\n` is stripped. The buffer is
/// erasing and reserved for the largest possible read, so no copy is freed in clear.
fn read_stdin_value() -> Result<SecretValue, HostlerError> {
    read_value_from(std::io::stdin())
}

/// Reads one byte more than the largest accepted input (value + `\r\n`), so an over-long input
/// stays over the limit after stripping and is refused — never cut (T8-R2-1).
fn read_value_from(reader: impl Read) -> Result<SecretValue, HostlerError> {
    const READ_LIMIT: usize = MAX_VALUE_BYTES + 3;
    let mut buf = Zeroizing::new(Vec::with_capacity(READ_LIMIT));
    reader
        .take(READ_LIMIT as u64)
        .read_to_end(&mut buf)
        .map_err(|e| internal(format!("cannot read the value from stdin: {e}")))?;
    let mut bytes = &buf[..];
    if bytes.ends_with(b"\n") {
        bytes = &bytes[..bytes.len() - 1];
        if bytes.ends_with(b"\r") {
            bytes = &bytes[..bytes.len() - 1];
        }
    }
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(errors::bad_request(
            "secret values may be at most 64 KiB".into(),
        ));
    }
    // Borrowed UTF-8 check: an error must not carry the bytes out of the erasing buffer.
    let text = std::str::from_utf8(bytes)
        .map_err(|_| errors::bad_request("the value is not valid UTF-8".into()))?;
    Ok(SecretValue::new(text.to_owned()))
}

pub fn main_remove(name: String, json: bool) -> ExitCode {
    if let Err(code) = checked_name(&name) {
        return code;
    }
    report(remove(name), json, print_removal)
}

fn remove(name: String) -> Result<RemoveOutcome, HostlerError> {
    let project = current_project()?;
    let value = call::<Value>(Request::SecretRemove { project, name })?;
    serde_json::from_value(value).map_err(|_| internal("unreadable answer from the service"))
}

pub fn main_share(name: String, to: String) -> ExitCode {
    if let Err(code) = checked_name(&name) {
        return code;
    }
    match run_share(name, to) {
        Ok(code) => code,
        Err(e) => report_error(&e),
    }
}

fn run_share(name: String, to: String) -> Result<ExitCode, HostlerError> {
    let mut tty = Tty::open("secret share")?;
    let from = current_project()?;
    let target = target_project(&to)?;
    let prompt = format!(
        "Type {} to share {name} with it:",
        sanitize_for_terminal(&target.name)
    );
    if !tty.confirm_typed(&prompt, &target.name)? {
        eprintln!("[hostler] the name did not match; nothing was shared");
        return Ok(ExitCode::FAILURE);
    }
    call::<Value>(Request::SecretShare(Share {
        from,
        name: name.clone(),
        to_project_id: target.id.clone(),
    }))?;
    println!("shared {name} with {}", sanitize_for_terminal(&target.name));
    Ok(ExitCode::SUCCESS)
}

pub fn main_unshare(name: String, to: String) -> ExitCode {
    if let Err(code) = checked_name(&name) {
        return code;
    }
    match run_unshare(name, to) {
        Ok(code) => code,
        Err(e) => report_error(&e),
    }
}

fn run_unshare(name: String, to: String) -> Result<ExitCode, HostlerError> {
    let mut tty = Tty::open("secret unshare")?;
    let from = current_project()?;
    let target = target_project(&to)?;
    let prompt = format!(
        "Stop sharing {name} with {}? [y/N]",
        sanitize_for_terminal(&target.name)
    );
    if !tty.confirm(&prompt)? {
        eprintln!("[hostler] nothing was unshared");
        return Ok(ExitCode::FAILURE);
    }
    call::<Value>(Request::SecretUnshare(Share {
        from,
        name: name.clone(),
        to_project_id: target.id.clone(),
    }))?;
    println!(
        "stopped sharing {name} with {}",
        sanitize_for_terminal(&target.name)
    );
    Ok(ExitCode::SUCCESS)
}

fn print_status(status: &SecretsStatus) {
    let state = match status.state {
        LockState::Missing => "missing",
        LockState::Locked => "locked",
        LockState::Unlocked => "unlocked",
    };
    println!("secrets: {state}");
    println!("  database: {}", sanitize_for_terminal(&status.database));
    match &status.key_file {
        Some(key_file) => println!("  key file: {}", sanitize_for_terminal(key_file)),
        None => println!("  key file: none"),
    }
    println!("  pending requests: {}", status.pending_requests);
    for file in &status.preserved_files {
        println!("  kept after a conflict: {}", sanitize_for_terminal(file));
    }
}

fn print_list(list: &SecretList) {
    println!(
        "{} ({})",
        sanitize_for_terminal(&list.project.name),
        sanitize_for_terminal(&list.project.common_dir)
    );
    let width = list
        .secrets
        .iter()
        .map(|secret| secret.name.len())
        .max()
        .unwrap_or(0)
        .max("NAME".len());
    println!(
        "{:<width$}  {:<7}  {:<12}  PURPOSE",
        "NAME", "STATE", "SOURCE"
    );
    for secret in &list.secrets {
        let state = match secret.state {
            SecretState::Set => "set",
            SecretState::Missing => "missing",
        };
        let source = match &secret.source {
            Some(SecretSource::Own) => "own".to_string(),
            Some(SecretSource::SharedFrom { project, .. }) => {
                format!("shared from {}", sanitize_for_terminal(project))
            }
            None => "-".to_string(),
        };
        let purpose = secret
            .purpose
            .as_deref()
            .map(sanitize_for_terminal)
            .unwrap_or_default();
        let name = sanitize_for_terminal(&secret.name);
        println!("{name:<width$}  {state:<7}  {source:<12}  {purpose}");
    }
}

fn print_removal(outcome: &RemoveOutcome) {
    println!(
        "{}",
        if outcome.removed {
            "removed"
        } else {
            "not set"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::read_value_from;
    use crate::secrets::MAX_VALUE_BYTES;

    fn read(input: &[u8]) -> Option<usize> {
        read_value_from(input).ok().map(|v| v.len())
    }

    #[test]
    fn crlf_after_a_full_value_and_more_lines_is_refused_not_cut() {
        let mut input = vec![b'a'; MAX_VALUE_BYTES];
        input.extend_from_slice(b"\r\nnext line\r\n");
        assert_eq!(read(&input), None);
    }

    #[test]
    fn a_full_value_with_one_line_ending_is_kept_whole() {
        let mut lf = vec![b'a'; MAX_VALUE_BYTES];
        lf.push(b'\n');
        assert_eq!(read(&lf), Some(MAX_VALUE_BYTES));
        let mut crlf = vec![b'a'; MAX_VALUE_BYTES];
        crlf.extend_from_slice(b"\r\n");
        assert_eq!(read(&crlf), Some(MAX_VALUE_BYTES));
        assert_eq!(read(&vec![b'a'; MAX_VALUE_BYTES + 1]), None);
    }
}
