//! The `hostler.toml` loader (plan P2, Task 2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use toml::de::{DeTable, DeValue};

use crate::contract::HostlerError;
use crate::contract::secrets::DeclaredSecret;
use crate::secrets::{valid_env_name, valid_secret_name};

use super::{config_invalid, display_path};

/// The P2 part of `hostler.toml`; P4 extends the schema (`unknown_tables` is kept for it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    pub name: Option<String>,
    pub secrets: BTreeMap<String, SecretDecl>,
    pub unknown_tables: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretDecl {
    pub purpose: String,
    pub env: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigProblem {
    pub line: usize,
    pub message: String,
}

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
        match name {
            "project" => match value.get_ref() {
                DeValue::Table(table) => parse_project(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`project` must be a table".into(),
                }),
            },
            "secrets" => match value.get_ref() {
                DeValue::Table(table) => parse_secrets(table, src, &mut config, &mut problems),
                _ => problems.push(ConfigProblem {
                    line: line_of(src, value.span().start),
                    message: "`secrets` must be a table".into(),
                }),
            },
            other => {
                if matches!(value.get_ref(), DeValue::Table(_)) {
                    config.unknown_tables.push(other.to_string());
                }
            }
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
}

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
        assert_eq!(config.unknown_tables, Vec::<String>::new());
        let decl = &config.secrets["API_KEY"];
        assert_eq!(decl.purpose, "the API key");
        assert_eq!(decl.env.as_deref(), Some("APP_API_KEY"));
        assert_eq!(config.env_for("API_KEY"), Some("APP_API_KEY"));
        assert_eq!(config.env_for("MISSING"), None);
        assert_eq!(
            config.declared(),
            vec![DeclaredSecret {
                name: "API_KEY".into(),
                purpose: "the API key".into(),
                env: Some("APP_API_KEY".into()),
            }]
        );
    }

    #[test]
    fn unknown_table_is_kept() {
        let config = parse(
            "[project]\n\
             name = \"app\"\n\
             \n\
             [dev]\n\
             port = 3000\n",
        )
        .unwrap();
        assert_eq!(config.unknown_tables, ["dev"]);
        assert_eq!(config.name.as_deref(), Some("app"));
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
}
