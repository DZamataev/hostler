//! The `hostler.toml` loader (plan P2 Task 2; version-1 schema, plan P4 Task 2).
//!
//! Every problem names its line and its key, never a value: the file is committed, but a person
//! can paste a secret into it by mistake.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::contract::HostlerError;
use crate::contract::accounts::{valid_account_name, valid_field_name};
use crate::contract::devices::Platform;
use crate::contract::secrets::DeclaredSecret;
use crate::secrets::{valid_env_name, valid_secret_name};

use super::{config_invalid, display_path};

/// Version 1 of `hostler.toml`: every table is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    pub name: Option<String>,
    pub apps: Apps,
    pub secrets: BTreeMap<String, SecretDecl>,
    pub accounts: BTreeMap<String, AccountDecl>,
    /// The commands that must run through `hostler run --heavy`.
    pub heavy: Vec<String>,
    pub agents: Agents,
    /// `[ranges]` is stage 2: accepted with any content, never read.
    pub ranges_present: bool,
}

/// App ids, to label this project's installs on a device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Apps {
    pub android: Vec<String>,
    pub ios: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretDecl {
    pub purpose: String,
    pub env: Option<String>,
}

/// An account this project uses; `fields` are the values doctor expects to be set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountDecl {
    pub purpose: String,
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agents {
    /// The agent docs holding hostler's block, relative to the checkout root.
    pub docs: Vec<String>,
    /// §7.8: where this project's device-handling instructions live.
    pub device_rules: Option<DocRef>,
}

/// `<path>` or `<path>#<heading>`, split at the first `#`: a path holds none, a heading may.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRef {
    pub path: String,
    pub heading: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigProblem {
    pub line: usize,
    pub message: String,
}

/// At most 32 ids per platform, at most 32 heavy commands.
const MAX_APPS: usize = 32;
/// `[accounts.NAME].fields` takes at most as many fields as the secrets engine stores.
use crate::secrets::engine::MAX_ACCOUNT_FIELDS;
const MAX_HEAVY_COMMANDS: usize = 32;
const MAX_AGENT_DOCS: usize = 8;

/// Parses `hostler.toml`. Problems never quote a value: the file may hold one pasted in.
pub fn parse(src: &str) -> Result<ProjectConfig, Vec<ConfigProblem>> {
    let root = match DeTable::parse(src) {
        Ok(root) => root.into_inner(),
        Err(error) => {
            return Err(vec![ConfigProblem {
                line: error.span().map_or(1, |span| line_of(src, span.start)),
                // `Display` prints the source line, which may hold a value: use `message` only.
                message: error.message().to_string(),
            }]);
        }
    };

    let mut config = ProjectConfig::default();
    let mut problems = Vec::new();
    for (key, value) in root.iter() {
        let name: &str = key.get_ref();
        let key_line = line_of(src, key.span().start);
        match name {
            "project" => match value.get_ref() {
                DeValue::Table(table) => parse_project(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`project` must be a table".into(),
                }),
            },
            "apps" => match value.get_ref() {
                DeValue::Table(table) => parse_apps(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`apps` must be a table".into(),
                }),
            },
            "secrets" => match value.get_ref() {
                DeValue::Table(table) => parse_secrets(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`secrets` must be a table".into(),
                }),
            },
            "accounts" => match value.get_ref() {
                DeValue::Table(table) => parse_accounts(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`accounts` must be a table".into(),
                }),
            },
            "heavy" => match value.get_ref() {
                DeValue::Table(table) => parse_heavy(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`heavy` must be a table".into(),
                }),
            },
            "agents" => match value.get_ref() {
                DeValue::Table(table) => parse_agents(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`agents` must be a table".into(),
                }),
            },
            // Stage 2: the table is reserved with any content and never read (`warn_reserved`
            // notes it once per process). Any other spelling of the name is an ordinary unknown
            // top-level key.
            "ranges" => match value.get_ref() {
                DeValue::Table(_) => config.ranges_present = true,
                _ => problems.push(ConfigProblem {
                    line: key_line,
                    message: "unknown key `ranges`".into(),
                }),
            },
            other => match value.get_ref() {
                DeValue::Table(_) => problems.push(ConfigProblem {
                    line: key_line,
                    message: format!("unknown table [{other}]"),
                }),
                _ => problems.push(ConfigProblem {
                    line: key_line,
                    message: format!("unknown key `{other}`"),
                }),
            },
        }
    }

    if problems.is_empty() {
        Ok(config)
    } else {
        problems.sort_by_key(|problem| problem.line);
        Err(problems)
    }
}

/// Reads `workspace_root/hostler.toml`; `None` when there is no file.
pub fn load(workspace_root: &Path) -> Result<Option<ProjectConfig>, HostlerError> {
    let path = workspace_root.join("hostler.toml");
    let src = match std::fs::read_to_string(&path) {
        Ok(src) => src,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_invalid(&[ConfigProblem {
                line: 1,
                message: format!("cannot read {}: {error}", display_path(&path)),
            }]));
        }
    };
    parse(&src)
        .map(Some)
        .map_err(|problems| config_invalid(&problems))
}

impl ProjectConfig {
    /// The declared secrets, by name.
    pub fn declared(&self) -> Vec<DeclaredSecret> {
        self.secrets
            .iter()
            .map(|(name, decl)| DeclaredSecret {
                name: name.clone(),
                purpose: decl.purpose.clone(),
                env: decl.env.clone(),
            })
            .collect()
    }

    pub fn env_for(&self, name: &str) -> Option<&str> {
        self.secrets.get(name).and_then(|decl| decl.env.as_deref())
    }

    /// The declared accounts, by name.
    pub fn declared_accounts(&self) -> Vec<(&str, &AccountDecl)> {
        self.accounts
            .iter()
            .map(|(name, decl)| (name.as_str(), decl))
            .collect()
    }

    /// This platform's declared app ids.
    pub fn app_ids(&self, platform: Platform) -> &[String] {
        match platform {
            Platform::Android => &self.apps.android,
            Platform::Ios => &self.apps.ios,
        }
    }

    /// The commands that must run through `hostler run --heavy`.
    pub fn heavy_commands(&self) -> &[String] {
        &self.heavy
    }
}

/// The canonical `hostler.toml` of a config: the tables in schema order, one comment heading
/// each. `parse(&render(c)) == c` for every valid `c`.
pub fn render(config: &ProjectConfig) -> String {
    let mut out =
        String::from("# This project's hostler settings (hostler.toml, schema version 1).\n");
    if let Some(name) = &config.name {
        out.push_str("\n# The project's name: it labels this project in hostler's output.\n");
        out.push_str("[project]\n");
        out.push_str(&format!("name = {}\n", toml_string(name)));
    }
    if !config.apps.android.is_empty() || !config.apps.ios.is_empty() {
        out.push_str("\n# App ids: they label this project's installs on a device.\n");
        out.push_str("[apps]\n");
        if !config.apps.android.is_empty() {
            out.push_str(&format!("android = {}\n", toml_array(&config.apps.android)));
        }
        if !config.apps.ios.is_empty() {
            out.push_str(&format!("ios = {}\n", toml_array(&config.apps.ios)));
        }
    }
    for (name, decl) in &config.secrets {
        out.push_str("\n# A secret this project needs: its name and purpose, never its value.\n");
        out.push_str(&format!("[secrets.{}]\n", toml_key(name)));
        out.push_str(&format!("purpose = {}\n", toml_string(&decl.purpose)));
        if let Some(env) = &decl.env {
            out.push_str(&format!("env = {}\n", toml_string(env)));
        }
    }
    for (name, decl) in &config.accounts {
        out.push_str("\n# An account this project uses; `fields` are the values it has.\n");
        out.push_str(&format!("[accounts.{}]\n", toml_key(name)));
        out.push_str(&format!("purpose = {}\n", toml_string(&decl.purpose)));
        if !decl.fields.is_empty() {
            out.push_str(&format!("fields = {}\n", toml_array(&decl.fields)));
        }
    }
    if !config.heavy.is_empty() {
        out.push_str("\n# Commands that must run through `hostler run --heavy`.\n");
        out.push_str("[heavy]\n");
        out.push_str(&format!("commands = {}\n", toml_array(&config.heavy)));
    }
    if !config.agents.docs.is_empty() || config.agents.device_rules.is_some() {
        out.push_str(
            "\n# The agent docs holding hostler's block, and this project's device-handling rules.\n",
        );
        out.push_str("[agents]\n");
        if !config.agents.docs.is_empty() {
            out.push_str(&format!("docs = {}\n", toml_array(&config.agents.docs)));
        }
        if let Some(rules) = &config.agents.device_rules {
            let value = match &rules.heading {
                Some(heading) => format!("{}#{heading}", rules.path),
                None => rules.path.clone(),
            };
            out.push_str(&format!("device_rules = {}\n", toml_string(&value)));
        }
    }
    if config.ranges_present {
        out.push_str("\n# Reserved for a later hostler version; not read.\n");
        out.push_str("[ranges]\n");
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The tables

fn parse_project(
    table: &DeTable<'_>,
    src: &str,
    config: &mut ProjectConfig,
    problems: &mut Vec<ConfigProblem>,
) {
    for (key, value) in table.iter() {
        let field: &str = key.get_ref();
        match field {
            "name" => match value.get_ref().as_str() {
                Some(name) => match check_project_name(name) {
                    Some(message) => problems.push(ConfigProblem {
                        line: line_of(src, value.span().start),
                        message: message.into(),
                    }),
                    None => config.name = Some(name.to_string()),
                },
                None => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`project.name` must be a string".into(),
                }),
            },
            other => problems.push(ConfigProblem {
                line: line_of(src, key.span().start),
                message: format!("unknown key `{other}`"),
            }),
        }
    }
}

fn parse_apps(
    table: &DeTable<'_>,
    src: &str,
    config: &mut ProjectConfig,
    problems: &mut Vec<ConfigProblem>,
) {
    for (key, value) in table.iter() {
        let field: &str = key.get_ref();
        let (valid, into): (fn(&str) -> bool, &mut Vec<String>) = match field {
            "android" => (valid_android_id, &mut config.apps.android),
            "ios" => (valid_ios_id, &mut config.apps.ios),
            other => {
                problems.push(ConfigProblem {
                    line: line_of(src, key.span().start),
                    message: format!("apps.{other}: unknown platform"),
                });
                continue;
            }
        };
        let Some(entries) = string_values(value, src, &format!("apps.{field}"), problems) else {
            continue;
        };
        if entries.len() > MAX_APPS {
            problems.push(ConfigProblem {
                line: line_of(src, value.span().start),
                message: format!("apps.{field}: at most {MAX_APPS} ids"),
            });
            continue;
        }
        let mut seen = BTreeSet::new();
        for (index, (line, id)) in entries.iter().enumerate() {
            if !seen.insert(id.clone()) {
                problems.push(ConfigProblem {
                    line: *line,
                    message: format!("apps.{field}[{index}]: listed twice"),
                });
            } else if !valid(id) {
                problems.push(ConfigProblem {
                    line: *line,
                    message: format!("apps.{field}[{index}]: not an application id"),
                });
            } else {
                into.push(id.clone());
            }
        }
    }
}

fn parse_secrets(
    table: &DeTable<'_>,
    src: &str,
    config: &mut ProjectConfig,
    problems: &mut Vec<ConfigProblem>,
) {
    let mut envs = BTreeSet::new();
    for (name_key, decl_value) in table.iter() {
        let name: &str = name_key.get_ref();
        let name_line = line_of(src, name_key.span().start);
        if !valid_secret_name(name) {
            problems.push(ConfigProblem {
                line: name_line,
                message: "invalid secret name".into(),
            });
            continue;
        }
        let DeValue::Table(decl) = decl_value.get_ref() else {
            problems.push(ConfigProblem {
                line: line_of(src, decl_value.span().start),
                message: format!("`secrets.{name}` must be a table"),
            });
            continue;
        };

        let mut purpose = None;
        let mut purpose_seen = false;
        let mut env = None;
        for (key, value) in decl.iter() {
            let field: &str = key.get_ref();
            let line = line_of(src, value.span().start);
            match field {
                "purpose" => {
                    purpose_seen = true;
                    match value.get_ref().as_str() {
                        Some(text) => match check_purpose(text) {
                            Some(message) => problems.push(ConfigProblem {
                                line,
                                message: message.into(),
                            }),
                            None => purpose = Some(text.to_string()),
                        },
                        None => problems.push(ConfigProblem {
                            line,
                            message: "`purpose` must be a string".into(),
                        }),
                    }
                }
                "env" => match value.get_ref().as_str() {
                    Some(text) if valid_env_name(text) => {
                        if !envs.insert(text.to_string()) {
                            problems.push(ConfigProblem {
                                line,
                                message: "`env` is already used by another secret".into(),
                            });
                        } else {
                            env = Some(text.to_string());
                        }
                    }
                    Some(_) => problems.push(ConfigProblem {
                        line,
                        message: "invalid env name".into(),
                    }),
                    None => problems.push(ConfigProblem {
                        line,
                        message: "`env` must be a string".into(),
                    }),
                },
                other => problems.push(ConfigProblem {
                    line: line_of(src, key.span().start),
                    message: format!("unknown key `{other}`"),
                }),
            }
        }
        if !purpose_seen {
            problems.push(ConfigProblem {
                line: line_of(src, decl_value.span().start),
                message: "`purpose` is required".into(),
            });
        } else if let Some(purpose) = purpose {
            config
                .secrets
                .insert(name.to_string(), SecretDecl { purpose, env });
        }
    }
}

fn parse_accounts(
    table: &DeTable<'_>,
    src: &str,
    config: &mut ProjectConfig,
    problems: &mut Vec<ConfigProblem>,
) {
    for (name_key, decl_value) in table.iter() {
        let name: &str = name_key.get_ref();
        let name_line = line_of(src, name_key.span().start);
        if !valid_account_name(name) {
            problems.push(ConfigProblem {
                line: name_line,
                message: "invalid account name".into(),
            });
            continue;
        }
        let DeValue::Table(decl) = decl_value.get_ref() else {
            problems.push(ConfigProblem {
                line: line_of(src, decl_value.span().start),
                // The key passed `valid_account_name`, so it is safe to name, as secrets' is.
                message: format!("`accounts.{name}` must be a table"),
            });
            continue;
        };

        let mut purpose = None;
        let mut purpose_seen = false;
        let mut fields: Vec<String> = Vec::new();
        for (key, value) in decl.iter() {
            let field: &str = key.get_ref();
            let line = line_of(src, value.span().start);
            match field {
                "purpose" => {
                    purpose_seen = true;
                    match value.get_ref().as_str() {
                        Some(text) => match check_purpose(text) {
                            Some(message) => problems.push(ConfigProblem {
                                line,
                                message: message.into(),
                            }),
                            None => purpose = Some(text.to_string()),
                        },
                        None => problems.push(ConfigProblem {
                            line,
                            message: "`purpose` must be a string".into(),
                        }),
                    }
                }
                "fields" => {
                    let Some(entries) = string_values(value, src, "`fields`", problems) else {
                        continue;
                    };
                    if entries.len() > MAX_ACCOUNT_FIELDS {
                        problems.push(ConfigProblem {
                            line,
                            message: format!("`fields`: at most {MAX_ACCOUNT_FIELDS} fields"),
                        });
                        continue;
                    }
                    let mut seen = BTreeSet::new();
                    for (index, (entry_line, field)) in entries.iter().enumerate() {
                        if !seen.insert(field.clone()) {
                            problems.push(ConfigProblem {
                                line: *entry_line,
                                message: format!("`fields`[{index}]: listed twice"),
                            });
                        } else if !valid_field_name(field) {
                            problems.push(ConfigProblem {
                                line: *entry_line,
                                message: format!("`fields`[{index}]: not a field name"),
                            });
                        } else {
                            fields.push(field.clone());
                        }
                    }
                }
                other => problems.push(ConfigProblem {
                    line: line_of(src, key.span().start),
                    message: format!("unknown key `{other}`"),
                }),
            }
        }
        if !purpose_seen {
            problems.push(ConfigProblem {
                line: line_of(src, decl_value.span().start),
                message: "`purpose` is required".into(),
            });
        } else if let Some(purpose) = purpose {
            config
                .accounts
                .insert(name.to_string(), AccountDecl { purpose, fields });
        }
    }
}

fn parse_heavy(
    table: &DeTable<'_>,
    src: &str,
    config: &mut ProjectConfig,
    problems: &mut Vec<ConfigProblem>,
) {
    for (key, value) in table.iter() {
        let field: &str = key.get_ref();
        if field != "commands" {
            problems.push(ConfigProblem {
                line: line_of(src, key.span().start),
                message: format!("unknown key `{field}`"),
            });
            continue;
        }
        let Some(entries) = string_values(value, src, "heavy.commands", problems) else {
            continue;
        };
        if entries.is_empty() || entries.len() > MAX_HEAVY_COMMANDS {
            problems.push(ConfigProblem {
                line: line_of(src, value.span().start),
                message: format!("heavy.commands: 1 to {MAX_HEAVY_COMMANDS} commands"),
            });
            continue;
        }
        for (index, (line, command)) in entries.iter().enumerate() {
            if command.is_empty() || command.chars().count() > 200 {
                problems.push(ConfigProblem {
                    line: *line,
                    message: format!(
                        "heavy.commands[{index}]: must be one line of 1 to 200 characters"
                    ),
                });
            } else if command.chars().any(char::is_control) {
                problems.push(ConfigProblem {
                    line: *line,
                    message: format!("heavy.commands[{index}]: must be one line"),
                });
            } else {
                config.heavy.push(command.clone());
            }
        }
    }
}

fn parse_agents(
    table: &DeTable<'_>,
    src: &str,
    config: &mut ProjectConfig,
    problems: &mut Vec<ConfigProblem>,
) {
    for (key, value) in table.iter() {
        let field: &str = key.get_ref();
        match field {
            "docs" => {
                let Some(entries) = string_values(value, src, "agents.docs", problems) else {
                    continue;
                };
                if entries.is_empty() || entries.len() > MAX_AGENT_DOCS {
                    problems.push(ConfigProblem {
                        line: line_of(src, value.span().start),
                        message: format!("agents.docs: 1 to {MAX_AGENT_DOCS} paths"),
                    });
                    continue;
                }
                for (index, (line, path)) in entries.iter().enumerate() {
                    if !valid_agent_path(path) {
                        problems.push(ConfigProblem {
                            line: *line,
                            message: format!(
                                "agents.docs[{index}]: not a relative path inside the project"
                            ),
                        });
                    } else {
                        config.agents.docs.push(path.clone());
                    }
                }
            }
            "device_rules" => match value.get_ref().as_str() {
                Some(text) => match parse_doc_ref(text) {
                    Ok(reference) => config.agents.device_rules = Some(reference),
                    Err(message) => problems.push(ConfigProblem {
                        line: line_of(src, value.span().start),
                        message: message.into(),
                    }),
                },
                None => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`agents.device_rules` must be a string".into(),
                }),
            },
            other => problems.push(ConfigProblem {
                line: line_of(src, key.span().start),
                message: format!("unknown key `{other}`"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rules

fn check_project_name(name: &str) -> Option<&'static str> {
    if name.chars().count() > 64 {
        Some("`project.name` must be at most 64 characters")
    } else if name.chars().any(char::is_control) {
        Some("`project.name` may not contain control characters")
    } else if name.is_empty() {
        Some("`project.name` must not be empty")
    } else {
        None
    }
}

fn check_purpose(purpose: &str) -> Option<&'static str> {
    if purpose.is_empty() {
        Some("`purpose` must not be empty")
    } else if purpose.chars().any(char::is_control) {
        Some("`purpose` must be one line")
    } else if purpose.chars().count() > 200 {
        Some("`purpose` must be at most 200 characters")
    } else {
        None
    }
}

/// `^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z][A-Za-z0-9_]*)+$`
fn valid_android_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            let b = part.as_bytes();
            !b.is_empty()
                && b[0].is_ascii_alphabetic()
                && b.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')
        })
}

/// `^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$`
fn valid_ios_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

/// A path an agent doc may hold: relative, no `..`, no backslash, no leading `/`. A `#` is
/// allowed here (`docs` has no separator); `device_rules` uses `parse_doc_ref` first.
fn valid_agent_path(path: &str) -> bool {
    !path.is_empty()
        && path.chars().count() <= 200
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains("..")
        && !path.chars().any(char::is_control)
}

fn valid_heading(heading: &str) -> bool {
    !heading.is_empty() && heading.chars().count() <= 200 && !heading.chars().any(char::is_control)
}

/// `<path>` or `<path>#<heading>`, split at the **first** `#`: the path cannot contain `#` (it is
/// the separator), while the heading may (`"AGENTS.md#C# rules"` keeps `C# rules`). `render` joins
/// them the same way, so a `DocRef` always `parse`s back to itself.
fn parse_doc_ref(text: &str) -> Result<DocRef, &'static str> {
    let (path, heading) = match text.split_once('#') {
        Some((path, heading)) => (path, Some(heading)),
        None => (text, None),
    };
    if !valid_agent_path(path) {
        return Err(
            "`agents.device_rules`: not a relative path inside the project (a path cannot contain \
             `#`)",
        );
    }
    let heading = match heading {
        Some(heading) => {
            if !valid_heading(heading) {
                return Err(
                    "`agents.device_rules`: the heading after `#` must be one line of 1 to 200 \
                     characters",
                );
            }
            Some(heading.to_string())
        }
        None => None,
    };
    Ok(DocRef {
        path: path.to_string(),
        heading,
    })
}

/// The strings of an array value, in order, each with its line. A problem (and `None`) when the
/// value is not an array of strings; the problem never quotes an entry.
fn string_values(
    value: &Spanned<DeValue<'_>>,
    src: &str,
    key: &str,
    problems: &mut Vec<ConfigProblem>,
) -> Option<Vec<(usize, String)>> {
    let Some(items) = value.get_ref().as_array() else {
        problems.push(ConfigProblem {
            line: line_of(src, value.span().start),
            message: format!("{key}: must be an array of strings"),
        });
        return None;
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item.get_ref().as_str() {
            Some(text) => out.push((line_of(src, item.span().start), text.to_string())),
            None => problems.push(ConfigProblem {
                line: line_of(src, item.span().start),
                message: format!("{key}: every entry must be a string"),
            }),
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Rendering

/// A TOML basic string: quotes and backslashes are escaped, control characters too.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|item| toml_string(item)).collect();
    format!("[{}]", inner.join(", "))
}

/// A table key: bare when TOML allows it, quoted otherwise (an account name may hold a `.`).
fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    if bare {
        key.to_string()
    } else {
        toml_string(key)
    }
}

fn line_of(src: &str, offset: usize) -> usize {
    let end = offset.min(src.len());
    src.as_bytes()[..end]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problems(src: &str) -> Vec<ConfigProblem> {
        parse(src).expect_err("expected problems")
    }

    fn messages(src: &str) -> Vec<String> {
        problems(src)
            .into_iter()
            .map(|problem| problem.message)
            .collect()
    }

    /// The example of the plan's Task 2.
    const FULL: &str = "[project]\n\
                        name = \"muse\"\n\
                        \n\
                        [apps]\n\
                        android = [\"com.example.muse\"]\n\
                        ios = [\"com.example.app\"]\n\
                        \n\
                        [secrets.JACKETT_API_KEY]\n\
                        purpose = \"Jackett API key for the torrent search\"\n\
                        env = \"JACKETT_API_KEY\"\n\
                        \n\
                        [accounts.qa-user]\n\
                        purpose = \"the main e2e test user\"\n\
                        fields = [\"env\", \"session\"]\n\
                        \n\
                        [heavy]\n\
                        commands = [\"./gradlew\", \"pod install\"]\n\
                        \n\
                        [agents]\n\
                        docs = [\"AGENTS.md\"]\n\
                        device_rules = \"AGENTS.md#device-handling\"\n\
                        \n\
                        [ranges]\n";

    #[test]
    fn full_schema_parses() {
        let config = parse(FULL).expect("the full schema");
        assert_eq!(config.name.as_deref(), Some("muse"));
        assert_eq!(config.apps.android, ["com.example.muse"]);
        assert_eq!(config.apps.ios, ["com.example.app"]);
        assert_eq!(
            config.secrets["JACKETT_API_KEY"].purpose,
            "Jackett API key for the torrent search"
        );
        assert_eq!(config.env_for("JACKETT_API_KEY"), Some("JACKETT_API_KEY"));
        assert_eq!(
            config.declared(),
            vec![DeclaredSecret {
                name: "JACKETT_API_KEY".into(),
                purpose: "Jackett API key for the torrent search".into(),
                env: Some("JACKETT_API_KEY".into()),
            }]
        );
        let accounts = config.declared_accounts();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, "qa-user");
        assert_eq!(accounts[0].1.purpose, "the main e2e test user");
        assert_eq!(accounts[0].1.fields, ["env", "session"]);
        assert_eq!(config.heavy_commands(), ["./gradlew", "pod install"]);
        assert_eq!(config.agents.docs, ["AGENTS.md"]);
        assert_eq!(
            config.agents.device_rules,
            Some(DocRef {
                path: "AGENTS.md".into(),
                heading: Some("device-handling".into()),
            })
        );
        assert!(config.ranges_present);
        assert_eq!(config.app_ids(Platform::Android), ["com.example.muse"]);
        assert_eq!(config.app_ids(Platform::Ios), ["com.example.app"]);
    }

    #[test]
    fn minimal_config_parses() {
        let config = parse(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.API_KEY]\n\
             purpose = \"the API key\"\n\
             env = \"APP_API_KEY\"\n",
        )
        .unwrap();
        assert_eq!(config.name.as_deref(), Some("app"));
        assert!(!config.ranges_present);
        let decl = &config.secrets["API_KEY"];
        assert_eq!(decl.purpose, "the API key");
        assert_eq!(decl.env.as_deref(), Some("APP_API_KEY"));
        assert_eq!(config.env_for("API_KEY"), Some("APP_API_KEY"));
        assert_eq!(config.env_for("MISSING"), None);
    }

    #[test]
    fn an_empty_file_is_an_empty_config() {
        assert_eq!(parse("").unwrap(), ProjectConfig::default());
        assert_eq!(parse("# nothing yet\n").unwrap(), ProjectConfig::default());
    }

    #[test]
    fn unknown_table_is_a_problem() {
        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [dev]\n\
             port = 3000\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 4);
        assert_eq!(found[0].message, "unknown table [dev]");
    }

    #[test]
    fn unknown_top_level_key_is_a_problem() {
        let found = problems("port = 3000\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 1);
        assert_eq!(found[0].message, "unknown key `port`");
    }

    #[test]
    fn ranges_is_reserved_as_a_table() {
        // The table is reserved with any content (`[ranges.x]` is a sub-table of it).
        for src in [
            "[ranges]\n",
            "[ranges]\nmin = 1\nmax = 10\n",
            "[ranges.build]\nfrom = \"0.1.0\"\n",
        ] {
            let config = parse(src).unwrap_or_else(|e| panic!("{src:?}: {e:?}"));
            assert!(config.ranges_present, "{src:?}");
        }

        // Only the table is reserved: any other spelling of the name is an ordinary unknown
        // top-level key, with its line.
        for src in ["ranges = 3\n", "ranges = [1, 2]\n", "ranges = \"later\"\n"] {
            let found = problems(src);
            assert_eq!(found.len(), 1, "{src:?}: {found:?}");
            assert_eq!(found[0].line, 1, "{src:?}");
            assert_eq!(found[0].message, "unknown key `ranges`", "{src:?}");
        }
        assert_eq!(messages("ranges = 3\n"), ["unknown key `ranges`"]);
    }

    #[test]
    fn apps_rules() {
        // A bad id, an unknown platform key and a duplicate: each has its line, and no message
        // holds the bad id.
        let src = "[apps]\n\
                   android = [\"com.example.ok\", \"bad\", \"com.example.ok\"]\n\
                   windows = [\"com.example.win\"]\n";
        let found = problems(src);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[0].message, "apps.android[1]: not an application id");
        assert_eq!(found[0].line, 2);
        assert_eq!(found[1].message, "apps.android[2]: listed twice");
        assert_eq!(found[1].line, 2);
        assert_eq!(found[2].message, "apps.windows: unknown platform");
        assert_eq!(found[2].line, 3);
        for problem in &found {
            assert!(!problem.message.contains("com.example"), "{problem:?}");
            assert!(!problem.message.contains("bad"), "{problem:?}");
        }

        // 33 ids are too many, and the array may not hold anything else.
        let ids: Vec<String> = (0..33).map(|n| format!("\"com.example.a{n}\"")).collect();
        let too_many = format!("[apps]\nios = [{}]\n", ids.join(", "));
        assert_eq!(
            messages(&too_many),
            ["apps.ios: at most 32 ids"],
            "{:?}",
            problems(&too_many)
        );

        // The i and Android rules differ.
        assert!(valid_android_id("com.example.App_2"));
        assert!(!valid_android_id("com.example.2app"));
        assert!(!valid_android_id("com"));
        assert!(!valid_android_id("com..example"));
        assert!(valid_ios_id("com.example.app-1"));
        assert!(!valid_ios_id("com..example"));
        assert!(!valid_ios_id("com"));

        // A platform that is not an array of strings.
        assert_eq!(
            messages("[apps]\nandroid = \"com.example.app\"\n"),
            ["apps.android: must be an array of strings"]
        );
        assert_eq!(
            messages("[apps]\nandroid = [1]\n"),
            ["apps.android: every entry must be a string"]
        );
    }

    #[test]
    fn accounts_rules() {
        let src = "[accounts.QA]\n\
                   purpose = \"one\"\n\
                   \n\
                   [accounts.qa-user]\n\
                   fields = [\"env\"]\n\
                   \n\
                   [accounts.qa-two]\n\
                   purpose = \"two\"\n\
                   fields = [\"env\", \"env\", \"NAME\", 3]\n\
                   extra = true\n";
        let found = problems(src);
        let found_messages: Vec<&str> = found.iter().map(|p| p.message.as_str()).collect();
        assert_eq!(
            found_messages,
            [
                "invalid account name",                   // line 1: `QA`
                "`purpose` is required",                  // line 4: `qa-user`
                "`fields`: every entry must be a string", // line 9: `3`
                "`fields`[1]: listed twice",              // line 9: `env`
                "`fields`[2]: not a field name",          // line 9: `NAME`
                "unknown key `extra`",                    // line 10
            ],
            "{found:?}"
        );
        assert_eq!(found[0].line, 1);
        assert_eq!(found[1].line, 4);
        assert_eq!(found[2].line, 9);
        assert_eq!(found[5].line, 10);

        // A scalar account entry names its key, like the other table problems.
        assert_eq!(
            messages("[accounts]\nqa = \"not-a-table\"\n"),
            ["`accounts.qa` must be a table"]
        );
        assert_eq!(
            messages("[accounts]\nqa = 3\n"),
            ["`accounts.qa` must be a table"]
        );

        // At most 16 fields.
        let fields: Vec<String> = (0..17).map(|n| format!("\"f{n}\"")).collect();
        let too_many = format!(
            "[accounts.qa]\npurpose = \"x\"\nfields = [{}]\n",
            fields.join(", ")
        );
        assert_eq!(messages(&too_many), ["`fields`: at most 16 fields"]);

        // A dotted account name is a table of its own when quoted.
        let config = parse("[accounts.\"a.b_c-1\"]\npurpose = \"x\"\n").unwrap();
        assert!(config.accounts.contains_key("a.b_c-1"));
    }

    #[test]
    fn heavy_rules() {
        // A table with nothing in it declares nothing.
        assert_eq!(parse("[heavy]\n").unwrap(), ProjectConfig::default());
        assert_eq!(parse("[agents]\n").unwrap(), ProjectConfig::default());
        assert_eq!(
            messages("[heavy]\ncommands = []\n"),
            ["heavy.commands: 1 to 32 commands"]
        );
        let long = format!("[heavy]\ncommands = [\"{}\"]\n", "x".repeat(201));
        assert_eq!(
            messages(&long),
            ["heavy.commands[0]: must be one line of 1 to 200 characters"]
        );
        assert_eq!(
            messages("[heavy]\ncommands = [\"\"]\n"),
            ["heavy.commands[0]: must be one line of 1 to 200 characters"]
        );
        assert_eq!(
            messages("[heavy]\ncommands = [\"one\\ntwo\"]\n"),
            ["heavy.commands[0]: must be one line"]
        );
        assert_eq!(
            messages("[heavy]\ncommands = [\"./gradlew\"]\nother = 1\n"),
            ["unknown key `other`"]
        );
        let commands: Vec<String> = (0..33).map(|n| format!("\"go{n}\"")).collect();
        let too_many = format!("[heavy]\ncommands = [{}]\n", commands.join(", "));
        assert_eq!(messages(&too_many), ["heavy.commands: 1 to 32 commands"]);
    }

    #[test]
    fn agents_rules() {
        assert_eq!(
            messages("[agents]\ndocs = [\"../AGENTS.md\"]\n"),
            ["agents.docs[0]: not a relative path inside the project"]
        );
        assert_eq!(
            messages("[agents]\ndocs = [\"/AGENTS.md\"]\n"),
            ["agents.docs[0]: not a relative path inside the project"]
        );
        assert_eq!(
            messages("[agents]\ndocs = [\"docs\\\\AGENTS.md\"]\n"),
            ["agents.docs[0]: not a relative path inside the project"]
        );
        assert_eq!(
            messages("[agents]\ndocs = [\"\"]\n"),
            ["agents.docs[0]: not a relative path inside the project"]
        );
        assert_eq!(
            messages("[agents]\ndocs = []\n"),
            ["agents.docs: 1 to 8 paths"]
        );
        let docs: Vec<String> = (0..9).map(|n| format!("\"d{n}.md\"")).collect();
        let too_many = format!("[agents]\ndocs = [{}]\n", docs.join(", "));
        assert_eq!(messages(&too_many), ["agents.docs: 1 to 8 paths"]);

        // `device_rules`: `<path>#<heading>`, split at the first `#`.
        assert_eq!(
            messages("[agents]\ndevice_rules = \"AGENTS.md#\"\n"),
            [
                "`agents.device_rules`: the heading after `#` must be one line of 1 to 200 characters"
            ]
        );
        assert_eq!(
            messages("[agents]\ndevice_rules = \"#devices\"\n"),
            [
                "`agents.device_rules`: not a relative path inside the project (a path cannot \
                 contain `#`)"
            ]
        );
        assert_eq!(
            messages("[agents]\ndevice_rules = \"../AGENTS.md#devices\"\n"),
            [
                "`agents.device_rules`: not a relative path inside the project (a path cannot \
                 contain `#`)"
            ]
        );
        assert_eq!(
            messages("[agents]\ndevice_rules = \"AGENTS.md#one\\ntwo\"\n"),
            [
                "`agents.device_rules`: the heading after `#` must be one line of 1 to 200 characters"
            ]
        );
        assert_eq!(
            messages("[agents]\ndevice_rules = 12\n"),
            ["`agents.device_rules` must be a string"]
        );
        assert_eq!(
            messages("[agents]\nother = \"x\"\n"),
            ["unknown key `other`"]
        );

        // A bare path, a heading, and a heading that itself holds a `#`.
        for (value, path, heading) in [
            ("docs/devices.md", "docs/devices.md", None),
            (
                "AGENTS.md#device-handling",
                "AGENTS.md",
                Some("device-handling"),
            ),
            ("AGENTS.md#C# rules", "AGENTS.md", Some("C# rules")),
            ("docs/a.md#one#two", "docs/a.md", Some("one#two")),
        ] {
            let config = parse(&format!(
                "[agents]\ndevice_rules = {}\n",
                toml_string(value)
            ))
            .unwrap();
            assert_eq!(
                config.agents.device_rules,
                Some(DocRef {
                    path: path.into(),
                    heading: heading.map(Into::into),
                }),
                "{value}"
            );
        }
        // A `#` in a `device_rules` value is the separator, so a literal `#` in a path is read as
        // a heading (`docs` entries may hold one; see `render_covers_the_hard_cases`).
        let split = parse("[agents]\ndevice_rules = \"docs/foo#bar.md\"\n").unwrap();
        assert_eq!(
            split.agents.device_rules,
            Some(DocRef {
                path: "docs/foo".into(),
                heading: Some("bar.md".into()),
            })
        );
        // A `docs` entry may hold a `#`: it has no separator.
        let docs = parse("[agents]\ndocs = [\"README#notes.md\"]\n").unwrap();
        assert_eq!(docs.agents.docs, ["README#notes.md"]);
    }

    #[test]
    fn bad_secret_name_reports_its_line() {
        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.api_key]\n\
             purpose = \"something\"\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 4);
        assert_eq!(found[0].message, "invalid secret name");
        assert!(!found[0].message.contains("api_key"));
    }

    #[test]
    fn unknown_key_reports_its_line() {
        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.API_KEY]\n\
             purpse = \"typo\"\n\
             purpose = \"the key\"\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 5);
        assert_eq!(found[0].message, "unknown key `purpse`");
    }

    #[test]
    fn missing_purpose_is_a_problem() {
        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.API_KEY]\n\
             env = \"APP_API_KEY\"\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].message, "`purpose` is required");
    }

    #[test]
    fn duplicate_env_is_a_problem() {
        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.ONE]\n\
             purpose = \"one\"\n\
             env = \"SHARED\"\n\
             \n\
             [secrets.TWO]\n\
             purpose = \"two\"\n\
             env = \"SHARED\"\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 10);
        assert_eq!(found[0].message, "`env` is already used by another secret");
    }

    #[test]
    fn invalid_toml_reports_a_line() {
        let found = problems("[project]\nname = \"unterminated\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 2, "{found:?}");
    }

    #[test]
    fn type_errors_and_bad_names_do_not_quote_values() {
        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.API_KEY]\n\
             purpose = 12\n\
             \n\
             [secrets.OTHER]\n\
             purpose = \"other\"\n\
             env = \"sk-hunter2-x!\"\n\
             \n\
             [secrets.hunter2-y]\n\
             purpose = \"x\"\n",
        );
        assert!(!found.is_empty());
        for problem in &found {
            assert!(!problem.message.contains("hunter2"), "{problem:?}");
        }

        let found = problems(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [secrets.API_KEY]\n\
             purpose = \"hunter2-z\n",
        );
        assert!(!found.is_empty());
        for problem in &found {
            assert!(!problem.message.contains("hunter2"), "{problem:?}");
        }
    }

    #[test]
    fn long_and_multiline_purposes_are_problems() {
        let long = format!("[secrets.API_KEY]\npurpose = \"{}\"\n", "x".repeat(201));
        assert_eq!(
            problems(&long)[0].message,
            "`purpose` must be at most 200 characters"
        );
        let multiline = "[secrets.API_KEY]\npurpose = \"one\\ntwo\"\n";
        assert_eq!(problems(multiline)[0].message, "`purpose` must be one line");
    }

    /// Values are the marker everywhere a rule can be broken; no problem may quote one.
    #[test]
    fn no_value_is_ever_quoted() {
        const MARKER: &str = "hostler-marker-7f3a";
        let src = format!(
            "[project]\n\
             name = \"{marker}\"\n\
             \n\
             [apps]\n\
             android = [\"{marker}\"]\n\
             windows = [\"{marker}\"]\n\
             \n\
             [secrets.{marker}]\n\
             purpose = \"{purpose}\"\n\
             env = \"{marker}\"\n\
             \n\
             [secrets.KEY]\n\
             purpose = \"{marker}\"\n\
             env = \"{marker}\"\n\
             \n\
             [accounts.{marker}]\n\
             purpose = \"{marker}\"\n\
             \n\
             [accounts.qa]\n\
             purpose = \"{purpose}\"\n\
             fields = [\"{marker}\"]\n\
             \n\
             [heavy]\n\
             commands = [\"{cmd}\"]\n\
             \n\
             [agents]\n\
             docs = [\"{marker}/../x\"]\n\
             device_rules = \"{marker}#{heading}\"\n",
            marker = MARKER,
            purpose = "p".repeat(201),
            cmd = "c".repeat(201),
            heading = "",
        );
        let found = problems(&src);
        assert!(found.len() >= 8, "{found:#?}");
        for problem in &found {
            assert!(!problem.message.contains(MARKER), "{problem:?}");
            assert!(!problem.message.contains("hostler-marker"), "{problem:?}");
        }

        // And the rendered config of a valid one holds its values exactly once each, escaped.
        let config = parse(FULL).unwrap();
        let rendered = render(&config);
        assert_eq!(parse(&rendered).unwrap(), config);
    }

    #[test]
    fn render_round_trips() {
        let mut rng = Lcg(0x5eed_1234);
        // The generator must really reach the schema's maxima, not only small lists.
        let mut max_android = false;
        let mut max_ios = false;
        let mut max_fields = false;
        let mut max_commands = false;
        let mut max_docs = false;
        for round in 0..300 {
            let config = random_config(&mut rng, round);
            let rendered = render(&config);
            let parsed = parse(&rendered)
                .unwrap_or_else(|e| panic!("round {round} does not re-parse: {e:#?}\n{rendered}"));
            assert_eq!(parsed, config, "round {round}:\n{rendered}");
            max_android |= config.apps.android.len() == MAX_APPS;
            max_ios |= config.apps.ios.len() == MAX_APPS;
            max_fields |= config
                .accounts
                .values()
                .any(|a| a.fields.len() == MAX_ACCOUNT_FIELDS);
            max_commands |= config.heavy.len() == MAX_HEAVY_COMMANDS;
            max_docs |= config.agents.docs.len() == MAX_AGENT_DOCS;
        }
        assert!(
            max_android && max_ios && max_fields && max_commands && max_docs,
            "the generator must reach every maximum: {max_android} {max_ios} {max_fields} \
             {max_commands} {max_docs}"
        );
    }

    /// The cases the generator is least likely to reach on its own.
    #[test]
    fn render_covers_the_hard_cases() {
        // A dotted account name is a quoted table key, not a nested table.
        let config = parse("[accounts.\"a.b_c-1\"]\npurpose = \"x\"\n").unwrap();
        let rendered = render(&config);
        assert!(rendered.contains("[accounts.\"a.b_c-1\"]"), "{rendered}");
        assert_eq!(parse(&rendered).unwrap(), config);

        // Every supported `device_rules` form renders back to itself.
        for (value, path, heading) in [
            ("AGENTS.md", "AGENTS.md", None),
            ("docs/devices.md", "docs/devices.md", None),
            (
                "AGENTS.md#device-handling",
                "AGENTS.md",
                Some("device-handling"),
            ),
            ("AGENTS.md#C# rules", "AGENTS.md", Some("C# rules")),
            ("AGENTS.md#one#two", "AGENTS.md", Some("one#two")),
            (
                "AGENTS.md#quote\"inside",
                "AGENTS.md",
                Some("quote\"inside"),
            ),
            (
                "AGENTS.md#back\\\\slash",
                "AGENTS.md",
                Some("back\\\\slash"),
            ),
        ] {
            let src = format!("[agents]\ndevice_rules = {}\n", toml_string(value));
            let config = parse(&src).unwrap_or_else(|e| panic!("{value:?}: {e:?}"));
            assert_eq!(
                config.agents.device_rules,
                Some(DocRef {
                    path: path.into(),
                    heading: heading.map(Into::into),
                }),
                "{value:?}"
            );
            let rendered = render(&config);
            assert_eq!(parse(&rendered).unwrap(), config, "{value:?}:\n{rendered}");
        }

        // The schema's maxima render and re-parse: 32 apps each, 16 fields, 32 commands, 8 docs.
        let config = ProjectConfig {
            name: Some("quote\" and back\\slash".into()),
            apps: Apps {
                android: (0..MAX_APPS)
                    .map(|n| format!("com.example.App_{n}"))
                    .collect(),
                ios: (0..MAX_APPS)
                    .map(|n| format!("com.Example-App{n}"))
                    .collect(),
            },
            secrets: BTreeMap::from([(
                "KEY".to_string(),
                SecretDecl {
                    purpose: "a purpose with quote\" and back\\slash".into(),
                    env: Some("APP_KEY".into()),
                },
            )]),
            accounts: BTreeMap::from([(
                "a.b_c-1".to_string(),
                AccountDecl {
                    purpose: "the account".into(),
                    fields: (0..MAX_ACCOUNT_FIELDS)
                        .map(|n| format!("field_{n}"))
                        .collect(),
                },
            )]),
            heavy: (0..MAX_HEAVY_COMMANDS)
                .map(|n| format!("./gradlew task{n}"))
                .collect(),
            agents: Agents {
                docs: (0..MAX_AGENT_DOCS)
                    .map(|n| format!("docs/agents{n}.md"))
                    .collect(),
                device_rules: Some(DocRef {
                    path: "AGENTS.md".into(),
                    heading: Some("C# rules".into()),
                }),
            },
            ranges_present: true,
        };
        let rendered = render(&config);
        assert_eq!(parse(&rendered).unwrap(), config, "{rendered}");
    }

    /// The rendered schema, table by table, as the plan's Task 2 shows it.
    #[test]
    fn render_matches_the_plan_example() {
        let expected = "# This project's hostler settings (hostler.toml, schema version 1).\n\
                        \n\
                        # The project's name: it labels this project in hostler's output.\n\
                        [project]\n\
                        name = \"muse\"\n\
                        \n\
                        # App ids: they label this project's installs on a device.\n\
                        [apps]\n\
                        android = [\"com.example.muse\"]\n\
                        ios = [\"com.example.app\"]\n\
                        \n\
                        # A secret this project needs: its name and purpose, never its value.\n\
                        [secrets.JACKETT_API_KEY]\n\
                        purpose = \"Jackett API key for the torrent search\"\n\
                        env = \"JACKETT_API_KEY\"\n\
                        \n\
                        # An account this project uses; `fields` are the values it has.\n\
                        [accounts.qa-user]\n\
                        purpose = \"the main e2e test user\"\n\
                        fields = [\"env\", \"session\"]\n\
                        \n\
                        # Commands that must run through `hostler run --heavy`.\n\
                        [heavy]\n\
                        commands = [\"./gradlew\", \"pod install\"]\n\
                        \n\
                        # The agent docs holding hostler's block, and this project's device-handling rules.\n\
                        [agents]\n\
                        docs = [\"AGENTS.md\"]\n\
                        device_rules = \"AGENTS.md#device-handling\"\n\
                        \n\
                        # Reserved for a later hostler version; not read.\n\
                        [ranges]\n";
        assert_eq!(render(&parse(FULL).unwrap()), expected);
    }

    #[test]
    fn render_is_canonical() {
        let config = parse(FULL).unwrap();
        let once = render(&config);
        let twice = render(&parse(&once).unwrap());
        assert_eq!(once, twice);
        // The tables come in schema order, each with one comment heading.
        let order = [
            "[project]",
            "[apps]",
            "[secrets.",
            "[accounts.",
            "[heavy]",
            "[agents]",
            "[ranges]",
        ];
        let mut at = 0;
        for part in order {
            let found = once[at..]
                .find(part)
                .unwrap_or_else(|| panic!("{part} after {at}:\n{once}"));
            at += found;
        }
        assert!(once.contains("\n# "), "{once}");
    }

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[(self.next() as usize) % items.len()]
        }

        fn chance(&mut self, one_in: u64) -> bool {
            self.next().is_multiple_of(one_in)
        }

        /// A `device_rules` heading: one line, and it may hold `#` (`"C# rules"`).
        fn heading(&mut self, round: usize) -> String {
            const HEADINGS: &[&str] = &[
                "device handling",
                "C# rules",
                "one#two",
                "a-b_c: q?",
                "quote\"inside",
                "back\\slash",
                "üñî",
            ];
            let mut out = format!("{} {round}", self.pick(HEADINGS));
            while out.chars().count() > 200 {
                out.pop();
            }
            out
        }

        fn text(&mut self, longest: usize) -> String {
            const PIECES: &[&str] = &[
                "muse",
                "a",
                "x y",
                "quote\"inside",
                "back\\slash",
                "ü",
                "AGENTS.md",
                "a.b-c_d",
                "0",
                "the main e2e test user",
            ];
            let mut out = String::from(*self.pick(PIECES));
            if self.chance(3) {
                out.push_str(self.pick(PIECES));
            }
            while out.chars().count() > longest {
                out.pop();
            }
            if out.is_empty() {
                out.push('z');
            }
            out
        }
    }

    fn random_config(rng: &mut Lcg, round: usize) -> ProjectConfig {
        let mut config = ProjectConfig::default();
        if rng.chance(4) {
            config.name = Some(rng.text(64));
        }
        // Boundary counts: the schema's maxima are generated, not only small lists.
        for n in 0..count(rng, MAX_APPS) {
            // Android ids take underscores and mixed case between segments.
            let id = format!("com.example.App_{n}_{}", rng.next() % 1000);
            config.apps.android.push(id);
        }
        for n in 0..count(rng, MAX_APPS) {
            // iOS ids take uppercase letters and hyphens, and no underscore.
            let id = format!("com.Example-App{n}-{}", rng.next() % 1000);
            config.apps.ios.push(id);
        }
        let mut envs = BTreeSet::new();
        for n in 0..count(rng, 3) {
            let name = format!("KEY_{n}");
            let env = format!("APP_ENV_{n}_{}", rng.next() % 100);
            let env = if rng.chance(2) && envs.insert(env.clone()) {
                Some(env)
            } else {
                None
            };
            config.secrets.insert(
                name,
                SecretDecl {
                    purpose: rng.text(200),
                    env,
                },
            );
        }
        for n in 0..count(rng, 3) {
            let name = match rng.next() % 4 {
                0 => format!("account{n}"),
                1 => format!("a.b_c-{n}"),
                2 => format!("qa-{n}"),
                _ => format!("team{n}.qa"),
            };
            let fields = (0..count(rng, MAX_ACCOUNT_FIELDS))
                .map(|f| format!("field_{f}"))
                .collect();
            config.accounts.insert(
                name,
                AccountDecl {
                    purpose: rng.text(200),
                    fields,
                },
            );
        }
        for n in 0..count(rng, MAX_HEAVY_COMMANDS) {
            config.heavy.push(format!("./gradlew task{n} \"x y\""));
        }
        let docs = count(rng, MAX_AGENT_DOCS);
        for n in 0..docs {
            // A `docs` entry may hold a `#`: it has no separator.
            if n == 0 && rng.chance(3) {
                config.agents.docs.push("README#notes.md".into());
            } else {
                config.agents.docs.push(format!("docs/agents{n}.md"));
            }
        }
        if rng.chance(2) {
            // A path a project may really hold: no backslash, no `#` (the separator), never empty.
            let path: String = rng
                .text(40)
                .chars()
                .filter(|c| *c != '\\' && *c != '#')
                .collect();
            let path = if path.is_empty() {
                "AGENTS.md".to_string()
            } else {
                path
            };
            config.agents.device_rules = Some(DocRef {
                path,
                heading: if rng.chance(2) {
                    Some(rng.heading(round))
                } else {
                    None
                },
            });
        }
        config.ranges_present = rng.chance(4);
        config
    }

    /// A count at a boundary the schema checks: none, one, half or the maximum.
    fn count(rng: &mut Lcg, most: usize) -> usize {
        match rng.next() % 4 {
            0 => 0,
            1 => 1,
            2 => most / 2,
            _ => most,
        }
    }
}
