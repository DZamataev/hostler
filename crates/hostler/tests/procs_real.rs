//! The real per-OS process table (plan Task 3).

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hostler::procs::{
    Liveness, ProcessTable, SysProcessTable, current_identity, identity_of, liveness,
};

fn table() -> SysProcessTable {
    SysProcessTable::new().expect("boot id readable")
}

/// A child that blocks reading stdin until `release`, so its identity is read while it certainly
/// runs (`spawn` returns only after exec).
fn gated_command() -> Command {
    let mut c = Command::new(if cfg!(windows) { "sort" } else { "cat" });
    c.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// Closes the gate: the child reads EOF and exits.
fn release(child: &mut Child) {
    drop(child.stdin.take());
}

#[test]
fn real_current_process_is_alive() {
    let t = table();
    let me = current_identity(&t).unwrap();
    assert_eq!(me.pid, std::process::id());
    assert_eq!(me.boot_id, t.boot_id());
    assert_eq!(liveness(&me, &t), Liveness::Alive);
    assert_eq!(identity_of(&t, me.pid), Some(me));
}

#[test]
fn real_exited_child_is_dead() {
    let t = table();
    let mut child = gated_command().spawn().unwrap();
    let id = identity_of(&t, child.id()).expect("gated child identity");
    assert_eq!(liveness(&id, &t), Liveness::Alive);
    release(&mut child);
    child.wait().unwrap();
    drop(child);
    assert!(matches!(liveness(&id, &t), Liveness::Dead(_)));
}

/// Exit code 259 equals `STILL_ACTIVE`; with our handle still open the process object remains,
/// and only its signaled state says it has exited.
#[cfg(windows)]
#[test]
fn windows_exit_259_with_open_handle_is_dead() {
    use hostler::procs::DeadReason;
    let t = table();
    let mut child = Command::new("cmd")
        .args(["/C", "exit 259"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Readable running or exited: `child` holds the process handle.
    let id = identity_of(&t, child.id()).expect("child identity");
    assert_eq!(child.wait().unwrap().code(), Some(259));
    assert_eq!(liveness(&id, &t), Liveness::Dead(DeadReason::Zombie));
    drop(child);
}

#[test]
fn identity_is_stable_over_time() {
    let t = table();
    let first = current_identity(&t).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(current_identity(&t).unwrap(), first);
    assert_eq!(SysProcessTable::new().unwrap().boot_id(), first.boot_id);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_start_time_is_proc_starttime() {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let after_comm = &stat[stat.rfind(')').unwrap() + 1..];
    // Field 22; fields after the comm start at field 3.
    let field22: u64 = after_comm
        .split_whitespace()
        .nth(22 - 3)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(current_identity(&table()).unwrap().start_time, field22);
}

#[cfg(unix)]
mod unix {
    use std::os::unix::process::CommandExt;
    use std::time::Instant;

    use hostler::procs::{DeadReason, RunGroup, group_liveness, unix_group_has_live_member};

    use super::*;

    /// Polls `cond` every 20 ms for up to 10 s.
    fn eventually(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cond()
    }

    fn in_group(mut c: Command, pgid: i32) -> Child {
        c.process_group(pgid).spawn().unwrap()
    }

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    }

    /// An exited, unreaped child. macOS refuses `proc_pidinfo` for zombies, so there a zombie
    /// reads as no process; both are dead.
    fn reads_as_zombie(l: Liveness) -> bool {
        if cfg!(target_os = "macos") {
            matches!(
                l,
                Liveness::Dead(DeadReason::Zombie | DeadReason::NoProcess)
            )
        } else {
            l == Liveness::Dead(DeadReason::Zombie)
        }
    }

    fn group(t: &SysProcessTable, pgid: u32) -> RunGroup {
        RunGroup::UnixGroup {
            pgid,
            boot_id: t.boot_id(),
        }
    }

    #[test]
    fn group_alive_while_grandchild_survives() {
        let t = table();
        let mut leader = in_group(sh("sleep 3 & exit 0"), 0);
        let pgid = leader.id();
        leader.wait().unwrap();
        assert!(unix_group_has_live_member(pgid));
        assert!(group_liveness(&group(&t, pgid), &t));
        assert!(eventually(|| !group_liveness(&group(&t, pgid), &t)));
        assert!(!unix_group_has_live_member(pgid));
    }

    #[test]
    fn zombie_only_group_is_dead() {
        let t = table();
        let mut child = in_group(gated_command(), 0);
        let pgid = child.id();
        let id = identity_of(&t, pgid).expect("gated child identity");
        assert!(group_liveness(&group(&t, pgid), &t));
        // Not reaped: once it has exited it is a zombie, and the group has no live member.
        release(&mut child);
        assert!(eventually(|| reads_as_zombie(liveness(&id, &t))));
        assert!(!unix_group_has_live_member(pgid));
        assert!(!group_liveness(&group(&t, pgid), &t));
        child.wait().unwrap();
    }

    #[test]
    fn zombie_plus_live_member_group_is_alive() {
        let t = table();
        let mut live = in_group(sh("exec sleep 30"), 0);
        let pgid = live.id();
        let mut zombie = in_group(gated_command(), pgid as i32);
        let zid = identity_of(&t, zombie.id()).expect("gated child identity");
        release(&mut zombie);
        assert!(eventually(|| reads_as_zombie(liveness(&zid, &t))));
        assert!(unix_group_has_live_member(pgid));
        assert!(group_liveness(&group(&t, pgid), &t));
        // With the live member gone, only the unreaped zombie remains: dead.
        live.kill().unwrap();
        live.wait().unwrap();
        assert!(!unix_group_has_live_member(pgid));
        zombie.wait().unwrap();
    }

    /// The comm in `/proc/<pid>/stat` is raw bytes; a non-UTF-8 one must not hide a live process.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_non_utf8_comm_is_alive() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        // A symlink, not a copy: exec of a freshly written file can fail with ETXTBSY. The kernel
        // takes comm from the exec'd path's base name.
        let exe = dir.path().join(std::ffi::OsStr::from_bytes(b"gate-\xff"));
        std::os::unix::fs::symlink("/bin/cat", &exe).unwrap();
        let mut child = Command::new(&exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        let comm_path = format!("/proc/{pid}/comm");
        assert!(
            eventually(|| std::fs::read(&comm_path).is_ok_and(|c| c.starts_with(b"gate-\xff"))),
            "comm never became non-UTF-8"
        );
        let t = table();
        let id = identity_of(&t, pid).expect("identity despite non-UTF-8 comm");
        assert_eq!(liveness(&id, &t), Liveness::Alive);
        assert!(unix_group_has_live_member(pid));
        release(&mut child);
        child.wait().unwrap();
    }
}
