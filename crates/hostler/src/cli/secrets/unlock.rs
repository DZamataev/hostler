//! `hostler unlock` and `hostler lock` (plan P2, Task 8).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use crate::cli::output::report;
use crate::cli::secrets::{PERSON_ONLY, call, internal, report_error, values_are_arguments};
use crate::cli::tty::Tty;
use crate::contract::ErrorCode;
use crate::contract::HostlerError;
use crate::contract::secrets::errors;
use crate::contract::secrets::{LockState, SecretsStatus, Unlock};
use crate::contract::wire::Request;
use crate::secrets::sanitize_for_terminal;
use crate::secrets::value::SecretValue;
use crate::service::harden::harden_value_holder;

#[derive(Debug, Args)]
#[command(after_help = PERSON_ONLY)]
pub struct UnlockArgs {
    /// Read the master key from this key file, and remember its path for later unlocks.
    #[arg(long, value_name = "PATH", conflicts_with = "no_key_file")]
    key_file: Option<PathBuf>,
    /// Forget the remembered key file.
    #[arg(long, conflicts_with = "key_file")]
    no_key_file: bool,
    #[arg(hide = true, allow_hyphen_values = true, num_args = 0..)]
    extra: Vec<OsString>,
}

pub fn main(args: UnlockArgs) -> ExitCode {
    if !args.extra.is_empty() {
        return values_are_arguments();
    }
    match run(args) {
        Ok(code) => code,
        Err(e) => report_error(&e),
    }
}

/// Prompts a person, so nothing in here runs inside a tokio runtime.
fn run(args: UnlockArgs) -> Result<ExitCode, HostlerError> {
    harden_value_holder();
    let mut tty = Tty::open("unlock")?;

    let key_file = match args.key_file {
        Some(path) => {
            let canonical = std::fs::canonicalize(&path).map_err(|e| {
                internal(format!(
                    "cannot read key file {}: {e}",
                    crate::project::display_path(&path)
                ))
            })?;
            Some(crate::project::display_path(&canonical))
        }
        None => None,
    };
    let forget_key_file = args.no_key_file;

    let status = call::<SecretsStatus>(Request::SecretsStatus)?;
    let database = sanitize_for_terminal(&status.database);
    match status.state {
        LockState::Unlocked => {
            println!("already unlocked");
            return Ok(ExitCode::SUCCESS);
        }
        LockState::Missing => {
            if !tty.confirm(&format!(
                "No secrets database at {database}. Create a new one? [y/N]"
            ))? {
                eprintln!("[hostler] no secrets database was created");
                return Ok(ExitCode::FAILURE);
            }
            let password = new_password(&mut tty)?;
            call::<SecretsStatus>(Request::SecretsUnlock(Unlock {
                password,
                key_file,
                forget_key_file,
                create: true,
            }))?;
        }
        LockState::Locked => {
            let mut last = None;
            let mut unlocked = false;
            for _ in 0..3 {
                let password = tty.hidden(&format!("Master password for {database}:"))?;
                match call::<SecretsStatus>(Request::SecretsUnlock(Unlock {
                    password,
                    key_file: key_file.clone(),
                    forget_key_file,
                    create: false,
                })) {
                    Ok(_) => {
                        unlocked = true;
                        break;
                    }
                    Err(e) if e.code == ErrorCode::UnlockFailed => last = Some(e),
                    Err(e) => return Err(e),
                }
            }
            if !unlocked {
                return Err(last.unwrap_or_else(errors::unlock_failed));
            }
        }
    }

    println!("Unlocked. It stays unlocked until `hostler lock` or the service stops.");
    if let Some(warning) = unregistered_service_warning() {
        eprintln!("{warning}");
    }
    Ok(ExitCode::SUCCESS)
}

/// W-6: a service the OS manager does not run lives in this login session (Linux logind with
/// `KillUserProcesses=yes` ends it with an SSH session), and the unlock goes with it.
fn unregistered_service_warning() -> Option<String> {
    use crate::service::manager::{OsManager, RegistrationState};
    let custom_home = crate::paths::Paths::resolve()
        .map(|paths| paths.custom_home)
        .unwrap_or(false);
    if custom_home {
        return Some(
            "[hostler] note: HOSTLER_HOME is set, so the service is not run by the OS service \
             manager; it can end with this login session, and the database is locked again then."
                .into(),
        );
    }
    match OsManager.registration() {
        RegistrationState::Current => None,
        _ => Some(
            "[hostler] note: the hostler service is not registered with the OS, so it can end \
             with this login session and lock the database again; `hostler service install` \
             keeps it running."
                .into(),
        ),
    }
}

/// A new database's password, typed twice; three mismatches give up.
fn new_password(tty: &mut Tty) -> Result<SecretValue, HostlerError> {
    for attempt in 0..3 {
        let first = tty.hidden("New master password:")?;
        let repeat = tty.hidden("Repeat it:")?;
        if first == repeat {
            return Ok(first);
        }
        if attempt < 2 {
            eprintln!("[hostler] the passwords do not match; try again");
        }
    }
    Err(errors::bad_request("the passwords did not match".into()))
}

pub fn main_lock(json: bool) -> ExitCode {
    report(call::<SecretsStatus>(Request::SecretsLock), json, |_| {
        println!("locked")
    })
}
