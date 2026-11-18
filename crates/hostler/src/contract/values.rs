//! The only types that hold a secret value or a command that may contain one (moved from
//! `secrets::value`, P5 Task 1, so every wire type lives in `contract/`).

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

/// On the wire a string; Task 3's Swift emitter maps it to `SecretText` by this name.
#[cfg(test)]
impl schemars::JsonSchema for SecretValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SecretValue".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "title": "SecretValue" })
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

/// On the wire a string; Task 3's Swift emitter maps it to `SecretText` by this name.
#[cfg(test)]
impl schemars::JsonSchema for RawCommand {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RawCommand".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "title": "RawCommand" })
    }
}
