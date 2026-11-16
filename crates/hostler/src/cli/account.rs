//! `hostler account list | set | remove` (plan P3a, Task 9): a project's test accounts in the
//! secrets database. Values come from a hidden prompt or stdin, never an argument (P2's
//! `secret set` flow).

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::output;
use crate::cli::secrets::secret::read_stdin_value;
use crate::cli::secrets::{call, current_project, internal, report_error, values_are_arguments};
use crate::cli::tty::{self, Tty};
use crate::contract::HostlerError;
use crate::contract::accounts::{
    AccountInfo, AccountRemove, AccountSet, INVALID_ACCOUNT_NAME, INVALID_FIELD_NAME,
    valid_account_name, valid_field_name,
};
use crate::contract::claims::ClaimKind;
use crate::contract::secrets::errors;
use crate::contract::wire::Request;
use crate::secrets::sanitize_for_terminal;
use crate::service::harden::harden_value_holder;

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub command: AccountCommand,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// This project's test accounts: names, field names and who holds them, never values.
    List {
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Set one field of an account, creating the account; the value comes from a prompt or
    /// stdin, never an argument.
    Set {
        /// The account's name.
        name: String,
        /// The field, e.g. `session`; it becomes HOSTLER_ACCOUNT_<FIELD> in a run.
        #[arg(long, value_name = "FIELD")]
        field: String,
        #[arg(hide = true, allow_hyphen_values = true, num_args = 0..)]
        extra: Vec<OsString>,
    },
    /// Remove an account, or one of its fields.
    ///
    /// hostler removes only its own data: the account's fields (or the one given with --field)
    /// and, once no field is left, the hostler-account tag. The KeePass entry itself is deleted
    /// only when nothing else is left in it; a password, user name, URL, notes, other custom
    /// fields or attachments a person keeps on the entry stay, and the entry stays with them.
    Remove {
        /// The account's name.
        name: String,
        /// Remove only this field.
        #[arg(long, value_name = "FIELD")]
        field: Option<String>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
}

pub fn main(args: AccountArgs) -> ExitCode {
    match args.command {
        AccountCommand::List { json } => output::report(
            current_project()
                .and_then(|project| call::<Vec<AccountInfo>>(Request::AccountList { project })),
            json,
            |accounts| print_list(accounts),
        ),
        AccountCommand::Set { name, field, extra } => {
            if !extra.is_empty() {
                return values_are_arguments();
            }
            if let Err(code) = checked(&name, Some(&field)) {
                return code;
            }
            match set(name, field) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => report_error(&e),
            }
        }
        AccountCommand::Remove { name, field, json } => {
            if let Err(code) = checked(&name, field.as_deref()) {
                return code;
            }
            output::report(remove(name, field), json, print_removal)
        }
    }
}

/// An invalid name is never echoed: it may be a pasted value.
fn checked(name: &str, field: Option<&str>) -> Result<(), ExitCode> {
    let refusal = if !valid_account_name(name) {
        INVALID_ACCOUNT_NAME
    } else if field.is_some_and(|f| !valid_field_name(f)) {
        INVALID_FIELD_NAME
    } else {
        return Ok(());
    };
    eprintln!("[hostler] {refusal}");
    Err(ExitCode::from(2))
}

fn set(name: String, field: String) -> Result<(), HostlerError> {
    let project = current_project()?;
    harden_value_holder();
    let value = if tty::interactive() {
        let mut tty = Tty::open("account set")?;
        let prompt = format!(
            "Value for {name}.{field} ({}):",
            sanitize_for_terminal(&project.name)
        );
        tty.hidden(&prompt)?
    } else if tty::stdin_is_terminal() {
        return Err(errors::not_interactive("account set"));
    } else {
        read_stdin_value()?
    };
    call::<Value>(Request::AccountSet(AccountSet {
        project,
        name: name.clone(),
        field: field.clone(),
        value,
    }))?;
    println!("stored {name}.{field}");
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct RemoveOutcome {
    removed: bool,
}

fn remove(name: String, field: Option<String>) -> Result<RemoveOutcome, HostlerError> {
    let project = current_project()?;
    let value = call::<Value>(Request::AccountRemove(AccountRemove {
        project,
        name,
        field,
    }))?;
    serde_json::from_value(value).map_err(|_| internal("unreadable answer from the service"))
}

fn print_list(accounts: &[AccountInfo]) {
    if accounts.is_empty() {
        println!("no accounts; a person adds one with `hostler account set NAME --field FIELD`");
        return;
    }
    let width = accounts
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(0)
        .max("NAME".len());
    let fields: Vec<String> = accounts.iter().map(|a| a.fields.join(", ")).collect();
    let fields_width = fields
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max("FIELDS".len());
    println!("{:<width$}  {:<fields_width$}  HOLDER", "NAME", "FIELDS");
    for (account, fields) in accounts.iter().zip(&fields) {
        let holder = account.holder.as_ref().map_or_else(
            || "-".to_string(),
            |h| {
                let kind = match h.kind {
                    ClaimKind::Lease => "run",
                    ClaimKind::Hold => "hold",
                    ClaimKind::Reservation => "reservation",
                };
                format!("{} ({kind})", sanitize_for_terminal(&h.workspace_path))
            },
        );
        println!(
            "{:<width$}  {fields:<fields_width$}  {holder}",
            account.name
        );
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
