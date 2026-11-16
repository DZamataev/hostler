//! Probes that are not tool calls (P3b Task 5): the adb server's `host:version` socket read
//! (P2-2) and the lockdown pair-record stat (P2-3, P3-8). Neither runs a program, so neither is
//! in the tool allowlist; both are bounded.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::service::DevicesService;
use super::tools;
use crate::contract::devices::AdbState;

/// The adb server's default port.
pub const ADB_PORT: u16 = 5037;
/// Debug builds: the port of the tests' fake adb server.
pub const TEST_ADB_SERVER_PORT_ENV: &str = "HOSTLER_TEST_ADB_SERVER_PORT";
/// Debug builds: the directory standing in for `/var/db/lockdown`.
pub const TEST_LOCKDOWN_DIR_ENV: &str = "HOSTLER_TEST_LOCKDOWN_DIR";
/// One `host:version` read.
pub const PROBE_BOUND: Duration = Duration::from_secs(1);
/// A guard answer is reused for this long (P3-10).
pub const GUARD_FRESH: Duration = Duration::from_secs(1);
/// While another adb owns the server, it is asked again this often (P2-2, P22-1: one socket
/// read, never a kill), so hostler takes adb back within seconds once the other server is gone.
pub const FOREIGN_REPROBE: Duration = Duration::from_secs(10);

/// Where the adb server listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbServerAddr {
    pub host: String,
    pub port: u16,
}

/// What hostler can ask about the adb server (P3bT5-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdbTarget {
    Tcp(AdbServerAddr),
    /// `ADB_SERVER_SOCKET` in a form hostler cannot read (`localfilesystem:…`): the server is
    /// never known, so no adb call runs.
    Unreadable,
    /// Debug builds with the fake tools and no test port: no server, so a test never touches a
    /// real one.
    TestNoServer,
}

impl AdbServerAddr {
    /// The server the adb client would talk to: `ADB_SERVER_SOCKET` (`tcp:<host>:<port>`,
    /// `tcp:[<ipv6>]:<port>` or `tcp:<port>`), else `ANDROID_ADB_SERVER_PORT`, else
    /// `127.0.0.1:5037`. `var` reads the recorded `devices.env.*` first, then the service's
    /// environment (P2-18). Debug builds: `HOSTLER_TEST_ADB_SERVER_PORT` wins.
    pub fn resolve(var: &dyn Fn(&str) -> Option<String>) -> AdbTarget {
        let local = |port| {
            AdbTarget::Tcp(AdbServerAddr {
                host: "127.0.0.1".into(),
                port,
            })
        };
        if cfg!(debug_assertions) {
            if let Some(port) = var(TEST_ADB_SERVER_PORT_ENV).and_then(|p| p.parse().ok()) {
                return local(port);
            }
            if var(tools::FAKE_TOOLS_ENV).is_some() {
                return AdbTarget::TestNoServer;
            }
        }
        if let Some(socket) = var("ADB_SERVER_SOCKET").filter(|s| !s.is_empty()) {
            let tcp = || -> Option<AdbTarget> {
                let rest = socket.strip_prefix("tcp:")?;
                Some(match rest.rsplit_once(':') {
                    Some((host, port)) => AdbTarget::Tcp(AdbServerAddr {
                        host: host
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .to_owned(),
                        port: port.parse().ok()?,
                    }),
                    None => local(rest.parse().ok()?),
                })
            };
            return tcp().unwrap_or(AdbTarget::Unreadable);
        }
        let port = var("ANDROID_ADB_SERVER_PORT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(ADB_PORT);
        local(port)
    }
}

/// One `host:version` read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The port is closed (connection refused): an adb client may start a server.
    NoServer,
    Version(u32),
    /// Something listens but gave no version in time (busy, `FAIL`, garbage, a short read), or
    /// the host does not resolve: never "no server" (P3bT5-1).
    Unknown,
}

/// The adb server's version from `000chost:version`, within `bound`. The answer is `OKAY`, a
/// 4-hex length, then that many hex digits (`OKAY` `0004` `0029` = 41). Only a socket read: no
/// adb client runs, so nothing can kill the server.
pub fn adb_server_version(addr: &AdbServerAddr, bound: Duration) -> Probe {
    let deadline = Instant::now() + bound;
    let Some(target) = (addr.host.as_str(), addr.port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    else {
        return Probe::Unknown;
    };
    let mut stream = match TcpStream::connect_timeout(&target, bound) {
        Ok(stream) => stream,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return Probe::NoServer,
        Err(_) => return Probe::Unknown,
    };
    let mut version = || -> Option<u32> {
        let left = || {
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1))
        };
        stream.set_write_timeout(Some(left())).ok()?;
        stream.write_all(b"000chost:version").ok()?;
        let mut read = |n: usize| -> Option<Vec<u8>> {
            stream.set_read_timeout(Some(left())).ok()?;
            let mut buf = vec![0u8; n];
            stream.read_exact(&mut buf).ok()?;
            Some(buf)
        };
        if read(4)? != b"OKAY" {
            return None;
        }
        let len = usize::from_str_radix(std::str::from_utf8(&read(4)?).ok()?, 16).ok()?;
        if len == 0 || len > 8 {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(&read(len)?).ok()?, 16).ok()
    };
    version().map_or(Probe::Unknown, Probe::Version)
}

/// `/var/db/lockdown` (macOS); debug builds honour `HOSTLER_TEST_LOCKDOWN_DIR`.
pub fn lockdown_dir() -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(dir) = std::env::var_os(TEST_LOCKDOWN_DIR_ENV)
    {
        return Some(PathBuf::from(dir));
    }
    cfg!(target_os = "macos").then(|| PathBuf::from("/var/db/lockdown"))
}

/// Whether `<lockdown_dir>/<udid>.plist` exists: a stat by name, never a read of the record
/// (P2-3, P3-8). `None` when it cannot be told: a UDID the `ideviceinfo` call's allowlist would
/// refuse (one shared shape, P3bT5-13) is never statted, nor is a stat error an answer.
pub fn has_pair_record(lockdown_dir: &Path, udid: &str) -> Option<bool> {
    let args = ["-u", udid, "-q", "com.apple.mobile.battery"].map(std::ffi::OsStr::new);
    if !tools::allowed(tools::Tool::Ideviceinfo, &args) {
        return None;
    }
    match std::fs::metadata(lockdown_dir.join(format!("{udid}.plist"))) {
        Ok(meta) => Some(meta.is_file()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// What the last probe found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerSeen {
    /// No server: an adb call may start one.
    None,
    Version(u32),
    /// Another adb's client said the server is another version, without saying which.
    OtherVersion,
    /// Not known (P3bT5-1): no adb call runs.
    Unknown,
}

/// Why the guard refuses an adb call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// hostler's own adb version cannot be read, so nothing can be compared (fail closed).
    NoClientVersion,
    /// Another adb's server; `None`: its version is not known.
    Foreign(Option<u32>),
    /// The server does not answer `host:version`, or cannot be asked.
    ServerUnknown,
}

impl Refused {
    /// As the tool status shows it (the contract has no "unknown" state: `Missing`, and the
    /// problem text says why).
    pub fn state(self) -> AdbState {
        match self {
            Refused::Foreign(v) => AdbState::Foreign {
                version: v.unwrap_or(0),
            },
            Refused::NoClientVersion | Refused::ServerUnknown => AdbState::Missing,
        }
    }
}

/// The adb guard's state (P2-2, P3-10): where the server is, the client's version, and the last
/// answer with its time.
pub struct AdbGuard {
    target: AdbTarget,
    adb: Option<PathBuf>,
    last: Mutex<Option<(Instant, ServerSeen)>>,
}

impl AdbGuard {
    pub fn new(target: AdbTarget, adb: Option<PathBuf>) -> AdbGuard {
        AdbGuard {
            target,
            adb,
            last: Mutex::new(None),
        }
    }

    /// The server now: a socket read, or the last answer when fresh. A foreign server is asked
    /// again every 10 s.
    pub fn server(&self) -> ServerSeen {
        self.server_within(self.client(), FOREIGN_REPROBE)
    }

    /// `server`, against `client`'s version: a foreign answer reused for at most `foreign`.
    fn server_within(&self, client: Option<u32>, foreign: Duration) -> ServerSeen {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, seen)) = *last {
            let fresh = match (seen, client) {
                (ServerSeen::Version(v), Some(c)) if v != c => foreign,
                (ServerSeen::OtherVersion, _) => foreign,
                _ => GUARD_FRESH,
            };
            if at.elapsed() < fresh {
                return seen;
            }
        }
        let seen = match &self.target {
            AdbTarget::Tcp(addr) => match adb_server_version(addr, PROBE_BOUND) {
                Probe::NoServer => ServerSeen::None,
                Probe::Version(v) => ServerSeen::Version(v),
                Probe::Unknown => ServerSeen::Unknown,
            },
            AdbTarget::TestNoServer => ServerSeen::None,
            AdbTarget::Unreadable => ServerSeen::Unknown,
        };
        *last = Some((Instant::now(), seen));
        seen
    }

    /// The last guard (P2-2): an adb client's stderr said the server is another version.
    pub fn saw_mismatch(&self, server_version: Option<u32>) {
        let seen = server_version.map_or(ServerSeen::OtherVersion, ServerSeen::Version);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), seen));
    }

    /// hostler's adb client version (`adb version`, once per path).
    pub fn client(&self) -> Option<u32> {
        tools::adb_client_version(self.adb.as_deref()?)
    }

    /// `Ok` when an adb call may run: no server listens (a client then starts its own), or the
    /// server is hostler's adb's version. The client's version is read first.
    pub fn check(&self) -> Result<(), Refused> {
        self.check_with(self.client())
    }

    /// `check`, a foreign answer asked again at once (older than 1 s): a `devices tools` call
    /// or a devices read sees another adb's server gone without waiting (P22-1).
    pub fn recheck(&self) -> Result<(), Refused> {
        self.check_within(self.client(), GUARD_FRESH)
    }

    /// `check` with the client's version given.
    fn check_with(&self, client: Option<u32>) -> Result<(), Refused> {
        self.check_within(client, FOREIGN_REPROBE)
    }

    fn check_within(&self, client: Option<u32>, foreign: Duration) -> Result<(), Refused> {
        let Some(client) = client else {
            return Err(Refused::NoClientVersion);
        };
        match self.server_within(Some(client), foreign) {
            ServerSeen::None => Ok(()),
            ServerSeen::Version(v) if v == client => Ok(()),
            ServerSeen::Version(v) => Err(Refused::Foreign(Some(v))),
            ServerSeen::OtherVersion => Err(Refused::Foreign(None)),
            ServerSeen::Unknown => Err(Refused::ServerUnknown),
        }
    }
}

/// Every adb call passes this first (P3-10): a socket probe, or one cached for less than 1 s.
pub fn adb_guard(svc: &DevicesService) -> Result<super::tools::AdbGuardPass, Refused> {
    svc.adb_guard()
        .check()
        .map(|()| super::tools::AdbGuardPass::cleared())
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// A one-shot server answering `answer` to the first request.
    fn server(answer: &'static [u8]) -> AdbServerAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut req = [0u8; 16];
                if s.read_exact(&mut req).is_ok() && &req == b"000chost:version" {
                    let _ = s.write_all(answer);
                }
            }
        });
        AdbServerAddr {
            host: "127.0.0.1".into(),
            port,
        }
    }

    #[test]
    fn host_version_answer_is_parsed_length_prefixed() {
        assert_eq!(
            adb_server_version(&server(b"OKAY00040029"), PROBE_BOUND),
            Probe::Version(41)
        );
        assert_eq!(
            adb_server_version(&server(b"OKAY0004002b"), PROBE_BOUND),
            Probe::Version(43)
        );
        // The length prefix decides how many digits follow.
        assert_eq!(
            adb_server_version(&server(b"OKAY000229"), PROBE_BOUND),
            Probe::Version(41)
        );
        assert_eq!(
            adb_server_version(&server(b"OKAY000800000029"), PROBE_BOUND),
            Probe::Version(41)
        );
        for bad in [&b"FAIL0007unknown"[..], b"OKAY", b"OKAY0004zz", b"OKAY000"] {
            let leaked: &'static [u8] = Box::leak(bad.to_vec().into_boxed_slice());
            // A server that answers without a version is not "no server" (P3bT5-1).
            assert_eq!(
                adb_server_version(&server(leaked), PROBE_BOUND),
                Probe::Unknown,
                "{bad:?}"
            );
        }
        // Nothing listening: the only "no server".
        let closed = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let addr = AdbServerAddr {
            host: "127.0.0.1".into(),
            port: closed,
        };
        assert_eq!(adb_server_version(&addr, PROBE_BOUND), Probe::NoServer);
        // A host that does not resolve.
        let nowhere = AdbServerAddr {
            host: "no-such-host.invalid".into(),
            port: 5037,
        };
        assert_eq!(adb_server_version(&nowhere, PROBE_BOUND), Probe::Unknown);
    }

    #[test]
    fn a_server_that_never_answers_is_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _held: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(5));
        });
        let start = Instant::now();
        let addr = AdbServerAddr {
            host: "127.0.0.1".into(),
            port,
        };
        assert_eq!(
            adb_server_version(&addr, Duration::from_millis(300)),
            Probe::Unknown,
            "silent is not absent"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_server_address_follows_the_adb_variables() {
        let with = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let local = |port| {
            AdbTarget::Tcp(AdbServerAddr {
                host: "127.0.0.1".into(),
                port,
            })
        };
        assert_eq!(AdbServerAddr::resolve(&with(&[])), local(5037));
        assert_eq!(
            AdbServerAddr::resolve(&with(&[("ANDROID_ADB_SERVER_PORT", "5038")])),
            local(5038)
        );
        assert_eq!(
            AdbServerAddr::resolve(&with(&[
                ("ANDROID_ADB_SERVER_PORT", "5038"),
                ("ADB_SERVER_SOCKET", "tcp:localhost:5040"),
            ])),
            AdbTarget::Tcp(AdbServerAddr {
                host: "localhost".into(),
                port: 5040
            })
        );
        assert_eq!(
            AdbServerAddr::resolve(&with(&[("ADB_SERVER_SOCKET", "tcp:[::1]:5042")])),
            AdbTarget::Tcp(AdbServerAddr {
                host: "::1".into(),
                port: 5042
            })
        );
        assert_eq!(
            AdbServerAddr::resolve(&with(&[("ADB_SERVER_SOCKET", "tcp:5041")])),
            local(5041)
        );
        // A socket form hostler cannot read: never "no server" (P3bT5-1).
        for pairs in [
            &[("ADB_SERVER_SOCKET", "localfilesystem:/x")][..],
            &[("ADB_SERVER_SOCKET", "tcp:host:port")][..],
            &[("ADB_SERVER_SOCKET", "tcp:")][..],
        ] {
            assert_eq!(
                AdbServerAddr::resolve(&with(pairs)),
                AdbTarget::Unreadable,
                "{pairs:?}"
            );
        }
        if cfg!(debug_assertions) {
            assert_eq!(
                AdbServerAddr::resolve(&with(&[(TEST_ADB_SERVER_PORT_ENV, "6000")])),
                local(6000)
            );
            assert_eq!(
                AdbServerAddr::resolve(&with(&[(tools::FAKE_TOOLS_ENV, "/fake")])),
                AdbTarget::TestNoServer,
                "tests never reach a real server"
            );
        }
    }

    /// P22-1: a foreign server (40, hostler's adb is 41) replaced by hostler's version: the
    /// guard asks again within 10 s, and `recheck` asks at once once its answer is 1 s old.
    #[test]
    fn foreign_server_is_asked_again_within_seconds() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        assert!(FOREIGN_REPROBE <= Duration::from_secs(10));
        let version = Arc::new(AtomicU32::new(40));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = AdbServerAddr {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
        };
        let answer = version.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut req = [0u8; 16];
                if s.read_exact(&mut req).is_ok() {
                    let v = answer.load(Ordering::SeqCst);
                    let _ = s.write_all(format!("OKAY0004{v:04x}").as_bytes());
                }
            }
        });
        let guard = AdbGuard::new(AdbTarget::Tcp(addr), None);
        let aged = |by: Duration| {
            let mut last = guard.last.lock().unwrap();
            let (at, seen) = last.unwrap();
            *last = Some((at.checked_sub(by).unwrap(), seen));
        };
        assert_eq!(guard.check_with(Some(41)), Err(Refused::Foreign(Some(40))));
        version.store(41, Ordering::SeqCst);
        // Within the bound: the foreign answer stands (no read).
        aged(GUARD_FRESH);
        assert_eq!(guard.check_with(Some(41)), Err(Refused::Foreign(Some(40))));
        // Past it: asked again, hostler's version now.
        aged(FOREIGN_REPROBE);
        assert_eq!(guard.check_with(Some(41)), Ok(()));
        // `recheck`: a foreign answer older than 1 s is asked again at once.
        version.store(40, Ordering::SeqCst);
        guard.saw_mismatch(Some(40));
        assert_eq!(
            guard.check_within(Some(41), GUARD_FRESH),
            Err(Refused::Foreign(Some(40)))
        );
        version.store(41, Ordering::SeqCst);
        aged(GUARD_FRESH);
        assert_eq!(guard.check_with(Some(41)), Err(Refused::Foreign(Some(40))));
        assert_eq!(guard.check_within(Some(41), GUARD_FRESH), Ok(()));
    }

    /// P3-10: a server of another version appearing between two cycles is seen by the next
    /// guard (a fresh socket read after 1 s), so no adb call meets it.
    #[test]
    fn foreign_server_appearing_between_cycles_is_refused_by_the_guard() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let version = Arc::new(AtomicU32::new(41));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let answer = version.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut req = [0u8; 16];
                if s.read_exact(&mut req).is_ok() {
                    let v = answer.load(Ordering::SeqCst);
                    let _ = s.write_all(format!("OKAY0004{v:04x}").as_bytes());
                }
            }
        });
        let addr = AdbServerAddr {
            host: "127.0.0.1".into(),
            port,
        };
        // The guard compares with a known client version (no `adb version` call here).
        struct Fixed(AdbGuard, u32);
        impl Fixed {
            fn check(&self) -> Result<(), Refused> {
                match self.0.server() {
                    ServerSeen::Version(v) if v != self.1 => Err(Refused::Foreign(Some(v))),
                    _ => Ok(()),
                }
            }
        }
        let guard = Fixed(AdbGuard::new(AdbTarget::Tcp(addr), None), 41);
        assert_eq!(guard.check(), Ok(()));
        version.store(40, Ordering::SeqCst);
        // Cached for less than 1 s: the same answer.
        assert_eq!(guard.check(), Ok(()));
        std::thread::sleep(GUARD_FRESH + Duration::from_millis(100));
        assert_eq!(guard.check(), Err(Refused::Foreign(Some(40))));
        // Without a readable client version the real guard fails closed.
        assert_eq!(
            AdbGuard::new(AdbTarget::TestNoServer, None).check(),
            Err(Refused::NoClientVersion)
        );
        // The last guard: a client's mismatch text marks the server foreign at once, with or
        // without its version.
        let real = AdbGuard::new(AdbTarget::TestNoServer, None);
        real.saw_mismatch(Some(39));
        assert_eq!(real.server(), ServerSeen::Version(39));
        real.saw_mismatch(None);
        assert_eq!(real.server(), ServerSeen::OtherVersion);
        assert_eq!(
            Refused::Foreign(None).state(),
            AdbState::Foreign { version: 0 }
        );
    }

    /// P3bT5-1: an `ADB_SERVER_SOCKET` hostler cannot read is never "no server": the guard
    /// refuses every adb call, whatever the client's version.
    #[test]
    fn unreadable_adb_server_socket_gets_no_adb_call() {
        let guard = AdbGuard::new(AdbTarget::Unreadable, None);
        assert_eq!(guard.server(), ServerSeen::Unknown);
        assert_eq!(guard.check_with(Some(41)), Err(Refused::ServerUnknown));
        assert_eq!(Refused::ServerUnknown.state(), AdbState::Missing);
    }

    /// P3bT5-1: a server that is silent past the bound, or answers `FAIL`, refuses every adb
    /// call; only a closed port lets a client start one.
    #[test]
    fn silent_or_failing_server_gets_no_adb_call() {
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let silent_addr = AdbServerAddr {
            host: "127.0.0.1".into(),
            port: silent.local_addr().unwrap().port(),
        };
        std::thread::spawn(move || {
            let _held: Vec<_> = silent.incoming().take(4).collect();
            std::thread::sleep(Duration::from_secs(10));
        });
        for addr in [silent_addr, server(b"FAIL0007unknown")] {
            let guard = AdbGuard::new(AdbTarget::Tcp(addr.clone()), None);
            assert_eq!(
                guard.check_with(Some(41)),
                Err(Refused::ServerUnknown),
                "{addr:?}"
            );
        }
        let closed = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let guard = AdbGuard::new(
            AdbTarget::Tcp(AdbServerAddr {
                host: "127.0.0.1".into(),
                port: closed,
            }),
            None,
        );
        assert_eq!(guard.check_with(Some(41)), Ok(()));
    }

    #[test]
    fn pair_records_are_a_stat_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("00008110-000A1B2C3D4E5F60.plist"), b"").unwrap();
        assert_eq!(
            has_pair_record(dir.path(), "00008110-000A1B2C3D4E5F60"),
            Some(true)
        );
        assert_eq!(
            has_pair_record(dir.path(), "00008110-FFFFFFFFFFFFFFFF"),
            Some(false)
        );
        std::fs::create_dir(dir.path().join("00008110-000000000000000D.plist")).unwrap();
        assert_eq!(
            has_pair_record(dir.path(), "00008110-000000000000000D"),
            Some(false),
            "a directory is no record"
        );
        // A UDID that is not one is never put into a path: the same shape as the call's
        // allowlist (20–64 hex digits and dashes).
        for bad in [
            "",
            "../x",
            "a/b",
            "a.b",
            "x\0",
            &"A".repeat(65),
            "DIR-1",
            "UDID-W",
            "SIM0000000000000000ZZ",
        ] {
            assert_eq!(has_pair_record(dir.path(), bad), None, "{bad:?}");
        }
    }
}
