//! `hostler guide` (plan Task 12; one table since P4 Task 1): the topics an agent can read from
//! hostler itself.

use serde::Serialize;

/// One guide topic: its name, its one-line summary and its full text.
#[derive(Clone, Copy, Debug)]
pub struct Topic {
    pub name: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

/// Every topic, in the order `hostler guide` lists them. This is the only list: `topic()`,
/// `topic_names()` and `hostler guide` read it.
pub const TOPICS: &[Topic] = &[
    Topic {
        name: "start",
        summary: "start here: what hostler is for, the first commands, and every topic",
        text: include_str!("start.md"),
    },
    Topic {
        name: "heavy",
        summary: "the machine-wide heavy queue: when and how to wrap build commands",
        text: include_str!("heavy.md"),
    },
    Topic {
        name: "secrets",
        summary: "project secrets: requesting, injecting and masking values, and the person-only \
                  commands",
        text: include_str!("secrets.md"),
    },
    Topic {
        name: "holds",
        summary: "holds, leases, waiting, takeover and workspaces: who may use the screen, a \
                  device or one account",
        text: include_str!("holds.md"),
    },
    Topic {
        name: "devices",
        summary: "phones, emulators and simulators: onboarding, leases, holds, agent-device \
                  sessions and telemetry",
        text: include_str!("devices.md"),
    },
    Topic {
        name: "screen",
        summary: "this machine's desktop session: hold it before driving the real GUI",
        text: include_str!("screen.md"),
    },
    Topic {
        name: "accounts",
        summary: "a project's test accounts: protected fields, environment variables and masking",
        text: include_str!("accounts.md"),
    },
    Topic {
        name: "init",
        summary: "set a project up: hostler.toml (never rewritten), the agent-docs block, and the \
                  steps a person takes",
        text: include_str!("init.md"),
    },
    Topic {
        name: "doctor",
        summary: "every check and id, the safe fixes --fix runs, the ones it never runs, and the \
                  exit codes",
        text: include_str!("doctor.md"),
    },
    Topic {
        name: "sandbox",
        summary: "agent sandboxes: the sandbox_denied report, Claude Code's setting and Codex's \
                  command rule",
        text: include_str!("sandbox.md"),
    },
    Topic {
        name: "recovery",
        summary: "every error code, with the reasons a service is unavailable and what resolves \
                  each one",
        text: include_str!("recovery.md"),
    },
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
    TOPICS.iter().find(|t| t.name == name).map(|t| t.text)
}

/// The topic names, for error messages and `details`.
pub fn topic_names() -> Vec<&'static str> {
    TOPICS.iter().map(|t| t.name).collect()
}
