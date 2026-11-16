//! `hostler guide` (plan Task 12): the topics an agent can read from hostler itself.

use serde::Serialize;

/// Every topic and its one-line summary, in the order `hostler guide` lists them.
pub const TOPICS: &[(&str, &str)] = &[
    (
        "heavy",
        "the machine-wide heavy queue: when and how to wrap build commands",
    ),
    (
        "secrets",
        "project secrets: requesting, injecting and masking values, and the person-only commands",
    ),
    (
        "holds",
        "holds, leases, waiting, takeover and workspaces: who may use the screen, a device or one account",
    ),
    (
        "devices",
        "phones, emulators and simulators: onboarding, leases, holds, agent-device sessions and telemetry",
    ),
    (
        "screen",
        "this machine's desktop session: hold it before driving the real GUI",
    ),
    (
        "accounts",
        "a project's test accounts: protected fields, environment variables and masking",
    ),
];

/// One entry of `hostler guide --json`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TopicSummary {
    pub name: &'static str,
    pub summary: &'static str,
}

/// `hostler guide <topic> --json`.
#[derive(Clone, Debug, Serialize)]
pub struct TopicText {
    pub name: String,
    pub text: &'static str,
}

/// The full text of a topic, or `None` for an unknown topic.
pub fn topic(name: &str) -> Option<&'static str> {
    match name {
        "heavy" => Some(include_str!("heavy.md")),
        "secrets" => Some(include_str!("secrets.md")),
        "holds" => Some(include_str!("holds.md")),
        "devices" => Some(include_str!("devices.md")),
        "screen" => Some(include_str!("screen.md")),
        "accounts" => Some(include_str!("accounts.md")),
        _ => None,
    }
}

/// The topic names, for error messages and `details`.
pub fn topic_names() -> Vec<&'static str> {
    TOPICS.iter().map(|(name, _)| *name).collect()
}
