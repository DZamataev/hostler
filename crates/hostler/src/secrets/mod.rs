//! Secrets (plan P2): names, value rules and the parts of the secrets engine.

pub mod dotenv;
pub mod engine;
pub mod layout;
pub mod rundir;
pub mod value;
pub mod vault;

use crate::contract::HostlerError;
use crate::contract::secrets::errors;
use value::SecretValue;

pub const MIN_VALUE_BYTES: usize = 4;
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// `^[A-Z][A-Z0-9_]{0,63}$`
pub fn valid_secret_name(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

/// `^[A-Za-z_][A-Za-z0-9_]{0,127}$`
pub fn valid_env_name(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=128).contains(&b.len())
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')
}

/// At least 4 bytes, at most 64 KiB, no control characters but tab and line breaks (KDBX is
/// XML). The errors never quote the value.
pub fn check_value(v: &SecretValue) -> Result<(), HostlerError> {
    if v.len() < MIN_VALUE_BYTES {
        return Err(errors::secret_too_short());
    }
    if v.len() > MAX_VALUE_BYTES {
        return Err(errors::bad_request(
            "secret values may be at most 64 KiB".into(),
        ));
    }
    if v.expose()
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(errors::bad_request(
            "secret values may not contain control characters other than tab and line breaks"
                .into(),
        ));
    }
    Ok(())
}

/// Agent-supplied text for a person's terminal: control characters become U+FFFD.
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ErrorCode;

    #[test]
    fn names_are_validated() {
        for ok in ["A", "API_KEY_2", &format!("A{}", "B".repeat(63))] {
            assert!(valid_secret_name(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "_A",
            "A-B",
            "1A",
            "Aé",
            &format!("A{}", "B".repeat(64)),
        ] {
            assert!(!valid_secret_name(bad), "{bad}");
        }
        for ok in ["_x", "Path", "A1_b", &format!("x{}", "y".repeat(127))] {
            assert!(valid_env_name(ok), "{ok}");
        }
        for bad in ["", "1x", "A-B", "A=B", &format!("x{}", "y".repeat(128))] {
            assert!(!valid_env_name(bad), "{bad}");
        }
    }

    fn check(s: &str) -> Result<(), HostlerError> {
        check_value(&SecretValue::new(s.into()))
    }

    #[test]
    fn check_value_rules() {
        assert_eq!(check("abc").unwrap_err().code, ErrorCode::SecretTooShort);
        assert_eq!(check("").unwrap_err().code, ErrorCode::SecretTooShort);
        assert_eq!(check("\u{0}abcd").unwrap_err().code, ErrorCode::BadRequest);
        assert_eq!(
            check("abcd\u{1b}[2J").unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(check("abcd\u{7f}").unwrap_err().code, ErrorCode::BadRequest);
        check("line1\nline2").unwrap();
        check("tab\there\r\n").unwrap();
        check("abcd").unwrap();
        check("ключ").unwrap();
        check(&"x".repeat(MAX_VALUE_BYTES)).unwrap();
        let too_long = check(&"x".repeat(MAX_VALUE_BYTES + 1)).unwrap_err();
        assert_eq!(too_long.code, ErrorCode::BadRequest);
        // The errors never quote the value.
        let e = check("hunter2\u{0}").unwrap_err();
        assert!(!format!("{e:?}").contains("hunter2"));
    }

    #[test]
    fn sanitize_replaces_control_characters() {
        assert_eq!(sanitize_for_terminal("ok text"), "ok text");
        assert_eq!(
            sanitize_for_terminal("a\u{1b}[2Jb\nc\u{7}"),
            "a\u{FFFD}[2Jb\u{FFFD}c\u{FFFD}"
        );
        assert_eq!(sanitize_for_terminal("é\u{9b}"), "é\u{FFFD}");
    }
}
