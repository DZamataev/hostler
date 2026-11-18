//! The value types live in `contract::values` (P5 Task 1: every wire type in `contract/`);
//! re-exported here, where the secrets code uses them.

pub use crate::contract::values::{RawCommand, SecretValue};

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
