//! What every command that talks to the service shares: one call, printing errors (P1 style),
//! the `--json` envelope.

use std::process::ExitCode;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::client::Client;
use crate::contract::wire::Request;
use crate::contract::{Envelope, ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::service::manager::OsManager;

pub(crate) async fn call<T: DeserializeOwned>(request: Request) -> Result<T, HostlerError> {
    let paths = Paths::resolve().map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("{e:#}"),
        details: json!({}),
        recovery: None,
    })?;
    let mut client = Client::connect_or_start(&paths, &OsManager).await?;
    let value = client.call(request).await?;
    serde_json::from_value(value).map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("unreadable answer from the service: {e}"),
        details: json!({}),
        recovery: None,
    })
}

/// Error texts can embed agent-chosen text (a checkout path, a project name), so control
/// characters become U+FFFD before they reach a person's terminal; line breaks a message uses
/// on purpose (e.g. `hostler.toml has problems:`) stay.
pub(crate) fn print_error(e: &HostlerError) {
    eprintln!("[hostler] {}", terminal_text(&e.message));
    if let Some(recovery) = &e.recovery {
        eprintln!("[hostler] To fix: {}", terminal_text(recovery));
    }
}

fn terminal_text(s: &str) -> String {
    s.split('\n')
        .map(crate::secrets::sanitize_for_terminal)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn report<T: Serialize>(
    result: Result<T, HostlerError>,
    json: bool,
    human: fn(&T),
) -> ExitCode {
    let code = if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    };
    if json {
        let envelope = match result {
            Ok(data) => Envelope {
                ok: true,
                data: Some(data),
                error: None,
            },
            Err(e) => Envelope {
                ok: false,
                data: None,
                error: Some(e),
            },
        };
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("an envelope always encodes")
        );
    } else {
        match &result {
            Ok(data) => human(data),
            Err(e) => print_error(e),
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::terminal_text;

    #[test]
    fn terminal_text_neutralizes_escapes_but_keeps_line_breaks() {
        let s = terminal_text("project app (/w/\u{1b}[2Jx) is not approved\nline 2:\tbad");
        assert!(!s.contains('\u{1b}'));
        assert!(!s.contains('\t'));
        assert_eq!(s.lines().count(), 2);
        assert!(s.starts_with("project app (/w/\u{FFFD}[2Jx)"));
    }
}
