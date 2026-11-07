//! The `.env`-style import of `hostler secret import` (plan P2, Task 8).

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use zeroize::Zeroizing;

use crate::cli::secrets::{call, current_project, internal, report_error, values_are_arguments};
use crate::cli::tty::{self, Tty};
use crate::contract::HostlerError;
use crate::contract::secrets::{ImportEntry, ImportOutcome, SecretImport};
use crate::contract::wire::Request;
use crate::secrets::dotenv;
use crate::secrets::sanitize_for_terminal;
use crate::service::harden::harden_value_holder;

/// Global Constraints "Bounds": an import file is at most 1 MiB.
const MAX_IMPORT_BYTES: u64 = 1024 * 1024;
/// Fixed text: the argument may be a pasted value, so a read failure never names the path.
const UNREADABLE_IMPORT: &str = "cannot read the file given to `hostler secret import`";

pub fn main(file: PathBuf, overwrite: bool, delete: bool, extra: Vec<OsString>) -> ExitCode {
    if !extra.is_empty() {
        return values_are_arguments();
    }
    match run(file, overwrite, delete) {
        Ok(code) => code,
        Err(e) => report_error(&e),
    }
}

fn run(file: PathBuf, overwrite: bool, delete: bool) -> Result<ExitCode, HostlerError> {
    let shown = sanitize_for_terminal(&file.display().to_string());
    // The file holds values: no core file may keep them.
    harden_value_holder();
    let source = read_import_file(&file)?;
    let text =
        std::str::from_utf8(&source).map_err(|_| internal("the import file is not valid UTF-8"))?;
    let entries = match dotenv::parse(text) {
        Ok(entries) => entries,
        Err(problems) => {
            for problem in problems {
                eprintln!(
                    "{shown}:{}: {}",
                    problem.line,
                    sanitize_for_terminal(&problem.message)
                );
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    // Every value now lives in its `SecretValue`; drop the whole-file plaintext copy.
    drop(source);

    let project = current_project()?;
    let request = SecretImport {
        project,
        entries: entries
            .into_iter()
            .map(|entry| ImportEntry {
                name: entry.name,
                value: entry.value,
            })
            .collect(),
        overwrite,
    };
    let outcome = call::<ImportOutcome>(Request::SecretImport(request))?;
    for name in &outcome.stored {
        println!("stored {name}");
    }
    if !outcome.answered_requests.is_empty() {
        println!(
            "answered request(s) {}",
            outcome.answered_requests.join(", ")
        );
    }

    if delete {
        delete_file(&file, &shown)?;
    } else if tty::interactive() {
        let mut tty = Tty::open("secret import")?;
        if tty.confirm(&format!(
            "Delete {shown} now? It still holds the values in clear. [y/N]"
        ))? {
            delete_file(&file, &shown)?;
        }
    } else {
        eprintln!(
            "[hostler] {shown} still holds the values in clear; delete it when you are done."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Opens the file, refuses one over 1 MiB, and reads it into an erasing buffer with its final
/// capacity (a FIFO or `/dev/zero` cannot grow it past the bound either).
fn read_import_file(file: &PathBuf) -> Result<Zeroizing<Vec<u8>>, HostlerError> {
    let handle =
        std::fs::File::open(file).map_err(|e| internal(format!("{UNREADABLE_IMPORT}: {e}")))?;
    let length = handle
        .metadata()
        .map_err(|e| internal(format!("{UNREADABLE_IMPORT}: {e}")))?
        .len();
    if length > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity((MAX_IMPORT_BYTES + 1) as usize));
    handle
        .take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| internal(format!("{UNREADABLE_IMPORT}: {e}")))?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

fn too_large() -> HostlerError {
    internal("the import file is larger than 1 MiB")
}

fn delete_file(file: &PathBuf, shown: &str) -> Result<(), HostlerError> {
    std::fs::remove_file(file).map_err(|e| internal(format!("cannot delete {shown}: {e}")))
}
