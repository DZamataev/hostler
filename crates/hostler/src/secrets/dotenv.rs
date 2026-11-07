//! The `.env` parser of `hostler secret import` (plan P2, Task 2).

use zeroize::Zeroizing;

use crate::secrets::valid_secret_name;
use crate::secrets::value::SecretValue;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DotenvEntry {
    pub line: usize,
    pub name: String,
    pub value: SecretValue,
}

/// A problem that can be shown to a person: it never contains anything right of a `=`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DotenvProblem {
    pub line: usize,
    pub message: String,
}

/// Parses `KEY=VALUE` lines. The import is all-or-nothing, so any problem is an `Err`.
pub fn parse(src: &str) -> Result<Vec<DotenvEntry>, Vec<DotenvProblem>> {
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for (index, raw) in src.lines().enumerate() {
        let line = index + 1;
        let mut text = raw.trim_start();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if let Some(rest) = text.strip_prefix("export")
            && rest.starts_with(char::is_whitespace)
        {
            text = rest.trim_start();
        }
        let Some(equals) = text.find('=') else {
            problems.push(problem(line, "missing `=`"));
            continue;
        };
        let name = text[..equals].trim();
        if !valid_secret_name(name) {
            problems.push(problem(line, invalid_name_message(name)));
            continue;
        }
        if !seen.insert(name.to_string()) {
            problems.push(problem(line, format!("duplicate name {name}")));
            continue;
        }
        match parse_value(&text[equals + 1..]) {
            Ok(value) => entries.push(DotenvEntry {
                line,
                name: name.to_string(),
                value: SecretValue::new(value),
            }),
            Err(message) => problems.push(problem(line, message)),
        }
    }

    if problems.is_empty() {
        Ok(entries)
    } else {
        Err(problems)
    }
}

fn parse_value(raw: &str) -> Result<String, &'static str> {
    let text = raw.trim_start();
    if let Some(rest) = text.strip_prefix('\'') {
        let Some(end) = rest.find('\'') else {
            return Err("unclosed quote");
        };
        check_trailing(&rest[end + 1..])?;
        return Ok(erasing_value(&rest[..end]));
    }
    if let Some(rest) = text.strip_prefix('"') {
        // Decoded bytes never exceed the source, so one reservation means no reallocation can
        // leave an earlier plaintext buffer unerased.
        let mut value = Zeroizing::new(String::with_capacity(rest.len()));
        let mut chars = rest.char_indices();
        while let Some((index, c)) = chars.next() {
            match c {
                '"' => {
                    check_trailing(&rest[index + 1..])?;
                    return Ok(std::mem::take(&mut *value));
                }
                '\\' => match chars.next() {
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, 'r')) => value.push('\r'),
                    Some((_, 't')) => value.push('\t'),
                    Some((_, '"')) => value.push('"'),
                    Some((_, '\\')) => value.push('\\'),
                    Some(_) => return Err("invalid escape in a quoted value"),
                    None => return Err("unclosed quote"),
                },
                c => value.push(c),
            }
        }
        return Err("unclosed quote");
    }

    let mut end = text.len();
    for (index, c) in text.char_indices() {
        if c == '#' && (index == 0 || text[..index].ends_with([' ', '\t'])) {
            end = index;
            break;
        }
    }
    Ok(text[..end].trim_end().to_string())
}

/// A single-quoted literal: built in an erasing buffer with its final capacity.
fn erasing_value(text: &str) -> String {
    let mut value = Zeroizing::new(String::with_capacity(text.len()));
    value.push_str(text);
    std::mem::take(&mut *value)
}

fn check_trailing(rest: &str) -> Result<(), &'static str> {
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err("unexpected text after the closing quote")
    }
}

fn invalid_name_message(name: &str) -> String {
    if is_safe_to_echo(name) {
        format!("invalid name {name}")
    } else {
        "an invalid name".into()
    }
}

/// The name may be echoed only from this conservative alphabet; anything else could hide a value.
fn is_safe_to_echo(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn problem(line: usize, message: impl Into<String>) -> DotenvProblem {
    DotenvProblem {
        line,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(entries: &[DotenvEntry]) -> Vec<(&str, &str)> {
        entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.value.expose()))
            .collect()
    }

    #[test]
    fn parses_quotes_export_and_comments() {
        let entries = parse(
            "# the kmp-muse pilot keys\n\
             \n\
             export JACKETT_URL=\"https://jackett.test\"\n\
             JACKETT_API_KEY='abc-def-123'\n\
             RT_INDEX_URL=https://rt.test # trailing comment\n\
             RT_INDEX_API_KEY = \"line1\\nline2\\t\\\"quoted\\\"\\\\done\"\n",
        )
        .unwrap();
        assert_eq!(
            values(&entries),
            vec![
                ("JACKETT_URL", "https://jackett.test"),
                ("JACKETT_API_KEY", "abc-def-123"),
                ("RT_INDEX_URL", "https://rt.test"),
                ("RT_INDEX_API_KEY", "line1\nline2\t\"quoted\"\\done"),
            ]
        );
        assert_eq!(
            entries.iter().map(|entry| entry.line).collect::<Vec<_>>(),
            [3, 4, 5, 6]
        );
    }

    #[test]
    fn problems_never_contain_values() {
        let problems = parse("bad-name=hunter2-x\nNO_EQUALS hunter2-y\nA=\"hunter2-z\n")
            .expect_err("expected problems");
        assert_eq!(problems.len(), 3, "{problems:?}");
        for problem in &problems {
            assert!(!problem.message.contains("hunter2"), "{problem:?}");
        }
        assert_eq!(problems[0].line, 1);
        assert_eq!(problems[0].message, "invalid name bad-name");
        assert_eq!(problems[1].message, "missing `=`");
        assert_eq!(problems[2].message, "unclosed quote");
    }

    #[test]
    fn duplicate_names_are_problems() {
        let problems = parse("A=one\nA=two\n").expect_err("expected problems");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].line, 2);
        assert_eq!(problems[0].message, "duplicate name A");
    }

    #[test]
    fn an_unsafe_name_is_not_echoed() {
        let problems = parse("A B=hunter2-x\n").expect_err("expected problems");
        assert_eq!(problems[0].message, "an invalid name");
    }

    #[test]
    fn decode_errors_after_a_long_prefix_are_problems_without_the_value() {
        let prefix = "hunter2-".repeat(20);
        let unclosed = format!("A=\"{prefix}\n");
        let bad_escape = format!("A=\"{prefix}\\q\"\n");
        let trailing = format!("A='{prefix}'tail\n");
        for source in [unclosed, bad_escape, trailing] {
            let problems = parse(&source).expect_err("expected problems");
            for problem in &problems {
                assert!(!problem.message.contains("hunter2"), "{problem:?}");
            }
        }
    }

    #[test]
    fn a_long_quoted_value_decodes_completely() {
        let prefix = "hunter2-".repeat(20);
        let source = format!("A=\"{prefix} tail\"\n");
        let entries = parse(&source).unwrap();
        assert_eq!(entries[0].value.expose(), format!("{prefix} tail"));
    }

    #[test]
    fn bad_escapes_and_trailing_text_are_problems() {
        let problems = parse("A=\"\\q\"\nB='x'y\n").expect_err("expected problems");
        assert_eq!(problems[0].message, "invalid escape in a quoted value");
        assert_eq!(
            problems[1].message,
            "unexpected text after the closing quote"
        );
    }
}
