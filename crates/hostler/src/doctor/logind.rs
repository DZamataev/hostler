//! `service.linux_session` (W-6, plan P4 Task 6, call 16): whether logind kills a user's
//! processes when the last session ends, and whether the user lingers. The parsers are pure and
//! tested on every OS; the probe runs on Linux only.
//!
//! Tools come from `/usr/bin` first, else `PATH`. Debug builds take them from
//! `HOSTLER_TEST_DOCTOR_BIN` first, and read `logind.conf` from `HOSTLER_TEST_DOCTOR_LOGIND`
//! instead of `/etc/systemd` (tests).
// The parsers serve the Linux probe; elsewhere only their tests use them.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// `busctl get-property … KillUserProcesses` prints `b true` or `b false`.
pub(crate) fn parse_busctl_bool(output: &str) -> Option<bool> {
    let mut words = output.split_whitespace();
    if words.next()? != "b" {
        return None;
    }
    let value = match words.next()? {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    words.next().is_none().then_some(value)
}

/// systemd's boolean words.
fn parse_bool(word: &str) -> Option<bool> {
    match word.trim().to_ascii_lowercase().as_str() {
        "1" | "yes" | "y" | "true" | "t" | "on" => Some(true),
        "0" | "no" | "n" | "false" | "f" | "off" => Some(false),
        _ => None,
    }
}

/// The last `KillUserProcesses=` of the `[Login]` section over `files`, in the order systemd
/// reads them (`logind.conf`, then `logind.conf.d/*.conf` sorted by name).
pub(crate) fn kill_user_processes_from_conf<'a>(
    files: impl IntoIterator<Item = &'a str>,
) -> Option<bool> {
    let mut value = None;
    for text in files {
        let mut in_login = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                in_login = line == "[Login]";
                continue;
            }
            if !in_login {
                continue;
            }
            if let Some((key, rest)) = line.split_once('=')
                && key.trim() == "KillUserProcesses"
                && let Some(parsed) = parse_bool(rest)
            {
                value = Some(parsed);
            }
        }
    }
    value
}

/// `loginctl show-user <uid> -p Linger` prints `Linger=yes|no`.
pub(crate) fn parse_linger(output: &str) -> Option<bool> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Linger="))
        .and_then(parse_bool)
}

/// What the Linux probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    /// `None`: neither busctl nor logind.conf could tell.
    pub kill_user_processes: Option<bool>,
    /// `None`: loginctl is missing or could not tell.
    pub linger: Option<bool>,
    pub user: String,
}

/// One `busctl`/`loginctl` call, at most.
#[cfg(target_os = "linux")]
const CALL_BOUND: std::time::Duration = std::time::Duration::from_secs(3);

/// Asks logind, within `deadline` (doctor's command deadline, review t6-r2 R2-5): each call gets
/// `min(3 s, time left)` and is killed after it. The `logind.conf` fallback reads small local
/// files and is not bounded.
#[cfg(target_os = "linux")]
pub(crate) async fn probe(deadline: std::time::Instant) -> Session {
    use std::path::{Path, PathBuf};

    fn tool(name: &str) -> Option<PathBuf> {
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("HOSTLER_TEST_DOCTOR_BIN") {
            let path = Path::new(&dir).join(name);
            return path.is_file().then_some(path);
        }
        let usr = Path::new("/usr/bin").join(name);
        if usr.is_file() {
            return Some(usr);
        }
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    }

    async fn output(program: &Path, args: &[&str], deadline: std::time::Instant) -> Option<String> {
        let limit = CALL_BOUND.min(deadline.saturating_duration_since(std::time::Instant::now()));
        if limit.is_zero() {
            return None;
        }
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(limit, command.output())
            .await
            .ok()?
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn conf_dir() -> PathBuf {
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("HOSTLER_TEST_DOCTOR_LOGIND") {
            return PathBuf::from(dir);
        }
        PathBuf::from("/etc/systemd")
    }

    fn from_conf() -> Option<bool> {
        let dir = conf_dir();
        let mut texts = Vec::new();
        if let Ok(text) = std::fs::read_to_string(dir.join("logind.conf")) {
            texts.push(text);
        }
        let mut drop_ins: Vec<PathBuf> = std::fs::read_dir(dir.join("logind.conf.d"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "conf"))
            .collect();
        drop_ins.sort();
        texts.extend(
            drop_ins
                .iter()
                .filter_map(|path| std::fs::read_to_string(path).ok()),
        );
        kill_user_processes_from_conf(texts.iter().map(String::as_str))
    }

    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() }.to_string();
    let user = std::env::var("USER").unwrap_or_else(|_| uid.clone());
    let mut kill_user_processes = None;
    if let Some(busctl) = tool("busctl") {
        kill_user_processes = output(
            &busctl,
            &[
                "get-property",
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                "KillUserProcesses",
            ],
            deadline,
        )
        .await
        .as_deref()
        .and_then(parse_busctl_bool);
    }
    if kill_user_processes.is_none() {
        kill_user_processes = from_conf();
    }
    let linger = match tool("loginctl") {
        Some(loginctl) => output(&loginctl, &["show-user", &uid, "-p", "Linger"], deadline)
            .await
            .as_deref()
            .and_then(parse_linger),
        None => None,
    };
    Session {
        kill_user_processes,
        linger,
        user,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busctl_output_is_read() {
        assert_eq!(parse_busctl_bool("b true\n"), Some(true));
        assert_eq!(parse_busctl_bool("b false"), Some(false));
        assert_eq!(parse_busctl_bool("s \"x\""), None);
        assert_eq!(parse_busctl_bool(""), None);
    }

    #[test]
    fn the_last_login_value_wins() {
        let main = "[Login]\n#KillUserProcesses=yes\nKillUserProcesses=no\n";
        let drop_in = "[Login]\nKillUserProcesses=yes\n";
        let other_section = "[Other]\nKillUserProcesses=no\n";
        assert_eq!(kill_user_processes_from_conf([main]), Some(false));
        assert_eq!(kill_user_processes_from_conf([main, drop_in]), Some(true));
        assert_eq!(
            kill_user_processes_from_conf([main, drop_in, other_section]),
            Some(true)
        );
        assert_eq!(kill_user_processes_from_conf(["[Login]\n"]), None);
    }

    #[test]
    fn linger_is_read() {
        assert_eq!(parse_linger("Linger=yes\n"), Some(true));
        assert_eq!(parse_linger("Linger=no"), Some(false));
        assert_eq!(parse_linger("Other=1\n"), None);
    }
}
