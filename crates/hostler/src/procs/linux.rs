//! Linux: `/proc`. The parsers are plain functions over file contents so they are tested on every
//! OS; the readers exist only on Linux.

/// The fields hostler needs from `/proc/<pid>/stat`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Stat {
    pub state: char,
    pub ppid: u32,
    pub pgrp: u32,
    /// Field 22: clock ticks since boot.
    pub starttime: u64,
}

impl Stat {
    /// `Z` is a zombie; `X` (dead) can be seen for an instant while the kernel removes it.
    pub fn zombie(&self) -> bool {
        matches!(self.state, 'Z' | 'X')
    }
}

/// Parses `/proc/<pid>/stat`: `pid (comm) state ppid pgrp … starttime …`. `comm` is raw bytes
/// (spaces, parentheses, non-UTF-8 are all legal), so only the ASCII part after the last `)` is
/// decoded.
pub(super) fn parse_stat(contents: &[u8]) -> Option<Stat> {
    let close = contents.iter().rposition(|&b| b == b')')?;
    let rest = std::str::from_utf8(&contents[close + 1..]).ok()?;
    // Field 3 (state) is index 0 after the comm.
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    let mut state = fields.first()?.chars();
    let (Some(state), None) = (state.next(), state.next()) else {
        return None;
    };
    Some(Stat {
        state,
        ppid: fields.get(1)?.parse().ok()?,
        pgrp: fields.get(2)?.parse().ok()?,
        starttime: fields.get(19)?.parse().ok()?,
    })
}

/// Parses `/proc/sys/kernel/random/boot_id`.
pub(super) fn parse_boot_id(contents: &str) -> Option<String> {
    let id = contents.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(target_os = "linux")]
pub(super) fn boot_id() -> anyhow::Result<String> {
    use anyhow::Context;
    const PATH: &str = "/proc/sys/kernel/random/boot_id";
    let contents = std::fs::read_to_string(PATH).with_context(|| format!("cannot read {PATH}"))?;
    parse_boot_id(&contents).ok_or_else(|| anyhow::anyhow!("{PATH} is empty"))
}

#[cfg(target_os = "linux")]
fn read_stat(pid: &str) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/stat")).ok()
}

#[cfg(target_os = "linux")]
pub(super) fn info(pid: u32) -> Option<super::ProcInfo> {
    let contents = read_stat(&pid.to_string())?;
    // The process exists; a stat we cannot parse leaves its start time unreadable (= dead).
    let stat = parse_stat(&contents);
    Some(super::ProcInfo {
        pid,
        parent: stat.as_ref().map(|s| s.ppid).filter(|&p| p != 0),
        start_time: stat.as_ref().map(|s| s.starttime),
        zombie: stat.as_ref().is_some_and(Stat::zombie),
    })
}

#[cfg(target_os = "linux")]
pub(super) fn group_has_live_member(pgid: u32) -> bool {
    // An unreadable /proc counts as no member: a false "alive" would hold a slot forever.
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            return false;
        };
        read_stat(pid)
            .and_then(|c| parse_stat(&c))
            .is_some_and(|s| s.pgrp == pgid && !s.zombie())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Linux stat format; the comm holds a space and parentheses.
    const STAT: &str = "4242 (my (odd) cmd) S 4200 4242 4200 34816 4242 4194560 1185 0 0 0 \
        2 1 0 0 20 0 1 0 987654 9502720 1024 18446744073709551615 1 1 0 0 0 0 0 4 65538 0 0 0 \
        17 3 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

    #[test]
    fn parse_stat_reads_state_ppid_pgrp_starttime() {
        assert_eq!(
            parse_stat(STAT.as_bytes()),
            Some(Stat {
                state: 'S',
                ppid: 4200,
                pgrp: 4242,
                starttime: 987654,
            })
        );
    }

    #[test]
    fn parse_stat_marks_zombies() {
        let z = parse_stat(b"7 (sh) Z 1 7 7 0 -1 4228108 0 0 0 0 0 0 0 0 20 0 1 0 55 0 0 0")
            .expect("zombie stat parses");
        assert!(z.zombie());
        assert_eq!((z.ppid, z.pgrp, z.starttime), (1, 7, 55));
        assert!(!parse_stat(STAT.as_bytes()).unwrap().zombie());
    }

    #[test]
    fn parse_stat_accepts_non_utf8_comm() {
        let stat =
            parse_stat(b"9 (build-\xff\xfe)) R 8 9 8 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 0 0")
                .expect("non-UTF-8 comm parses");
        assert_eq!(
            (stat.state, stat.ppid, stat.pgrp, stat.starttime),
            ('R', 8, 9, 77)
        );
    }

    #[test]
    fn parse_stat_rejects_truncated_or_garbled() {
        assert_eq!(parse_stat(b""), None);
        assert_eq!(parse_stat(b"1 (init) S 0 1 1"), None);
        assert_eq!(
            parse_stat(b"1 init S 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9"),
            None
        );
        assert_eq!(
            parse_stat(b"1 (init) S x 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9"),
            None
        );
    }

    #[test]
    fn parse_boot_id_trims_and_rejects_empty() {
        assert_eq!(
            parse_boot_id("5f1c0e9a-2b7d-4d36-9d6e-1a0c3f2b4e8d\n").as_deref(),
            Some("5f1c0e9a-2b7d-4d36-9d6e-1a0c3f2b4e8d")
        );
        assert_eq!(parse_boot_id(" \n"), None);
    }
}
