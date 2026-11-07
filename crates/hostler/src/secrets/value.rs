//! The only types that hold a secret value or a command that may contain one.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

/// A secret value: erased on drop; `Debug` and `Display` print `[redacted]`; on the wire a plain
/// JSON string.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    pub fn new(s: String) -> SecretValue {
        SecretValue(Zeroizing::new(s))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Serialize for SecretValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for SecretValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(SecretValue::new)
    }
}

/// The raw display of a command sent for materialization (L12, L13): it may hold a literal
/// value, so it is erased on drop and `Debug`/`Display` print only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct RawCommand(Zeroizing<String>);

impl RawCommand {
    pub fn new(s: String) -> RawCommand {
        RawCommand(Zeroizing::new(s))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for RawCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<command of {} bytes>", self.len())
    }
}

impl fmt::Display for RawCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl Serialize for RawCommand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for RawCommand {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(RawCommand::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::secrets::{ProjectRef, SecretSet};
    use crate::contract::wire::Request;

    #[test]
    fn secret_value_debug_and_display_are_redacted() {
        let v = SecretValue::new("hunter2-canary".into());
        assert_eq!(format!("{v:?}"), "[redacted]");
        assert_eq!(format!("{v}"), "[redacted]");
        let request = Request::SecretSet(SecretSet {
            project: ProjectRef {
                common_dir: "/work/app/.git".into(),
                name: "app".into(),
            },
            name: "API_KEY".into(),
            value: v.clone(),
        });
        let printed = format!("{request:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(v.expose() == "hunter2-canary");
        assert_eq!(v.len(), 14);
        assert!(!v.is_empty());
        assert!(SecretValue::new(String::new()).is_empty());
    }

    #[test]
    fn secret_value_serializes_as_plain_string() {
        let v = SecretValue::new("a \"quoted\" value".into());
        let encoded = serde_json::to_string(&v).unwrap();
        assert!(encoded == r#""a \"quoted\" value""#);
        let back: SecretValue = serde_json::from_str(&encoded).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn raw_command_is_redacted_and_a_plain_string_on_the_wire() {
        let c = RawCommand::new("curl hunter2-canary".into());
        assert_eq!(format!("{c:?}"), "<command of 19 bytes>");
        assert_eq!(format!("{c}"), "<command of 19 bytes>");
        assert!(c.expose() == "curl hunter2-canary");
        let encoded = serde_json::to_string(&c).unwrap();
        assert!(encoded == r#""curl hunter2-canary""#);
        assert_eq!(serde_json::from_str::<RawCommand>(&encoded).unwrap(), c);
    }
}
