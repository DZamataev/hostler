//! The frozen lifecycle protocol: `hello`, `service.status`, `service.shutdown` (plan Task 4/6).
//! These answers must stay readable by every past and future client.

use serde_json::json;

use crate::client::Client;
use crate::contract::claims::ClaimSummary;
use crate::contract::heavy::{HeavyEntry, ServiceStatus};
use crate::contract::wire::Request;
use crate::contract::{CONTRACT_VERSION, ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::service::manager::RegistrationState;

pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RESTART_RECOVERY: &str = "hostler service restart";

pub use crate::contract::lifecycle::HelloReply;

/// The version this service reports in `hello` and `service.status`. Debug builds honour
/// `HOSTLER_TEST_SERVICE_VERSION` so tests can run an "older" or "newer" service (P4 Task 4).
pub fn service_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("HOSTLER_TEST_SERVICE_VERSION")
        && !v.is_empty()
    {
        return v;
    }
    SERVICE_VERSION.into()
}

/// The contract version this service speaks. Debug builds honour `HOSTLER_TEST_CONTRACT_VERSION`
/// so tests can run a "different" service.
pub fn service_contract_version() -> u32 {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var("HOSTLER_TEST_CONTRACT_VERSION")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return v;
    }
    CONTRACT_VERSION
}

/// `hello`: the service's versions, or `ContractMismatch` when the client speaks another version.
pub fn hello(client_contract: u32, service_contract: u32) -> Result<HelloReply, HostlerError> {
    let service_version = service_version();
    if client_contract == service_contract {
        return Ok(HelloReply {
            contract_version: service_contract,
            service_version,
            boot_id: crate::procs::local_boot_id(),
            instance: None,
        });
    }
    Err(HostlerError {
        code: ErrorCode::ContractMismatch,
        message: format!(
            "the running hostler service speaks contract {service_contract} \
             (hostler {service_version}); this client speaks contract {client_contract}"
        ),
        details: json!({
            "client_contract": client_contract,
            "service_contract": service_contract,
            "service_version": service_version,
        }),
        recovery: Some(RESTART_RECOVERY.into()),
    })
}

/// A versioned method before a matching `hello` on this connection.
pub fn hello_required(service_contract: u32) -> HostlerError {
    HostlerError {
        code: ErrorCode::ContractMismatch,
        message: format!(
            "this connection has no matching hello; the service speaks contract {service_contract}"
        ),
        details: json!({ "service_contract": service_contract, "service_version": SERVICE_VERSION }),
        recovery: Some(RESTART_RECOVERY.into()),
    }
}

/// `service.shutdown { if_idle: true }` while heavy entries or leases (linked ones included)
/// run, or a takeover is in progress (T6-7: its taker would lose its commit after stopping the
/// holder); holds and reservations never block a stop.
pub fn busy(
    running: &[HeavyEntry],
    leases: &[ClaimSummary],
    plans: &[serde_json::Value],
) -> HostlerError {
    let mut parts = Vec::new();
    if !running.is_empty() {
        parts.push(format!("{} heavy task(s) are running", running.len()));
    }
    if !leases.is_empty() {
        parts.push(format!(
            "{} run(s) holding claims are running",
            leases.len()
        ));
    }
    if !plans.is_empty() {
        parts.push(format!("{} takeover(s) are in progress", plans.len()));
    }
    let what = parts.join(" and ");
    HostlerError {
        code: ErrorCode::ServiceBusy,
        message: format!(
            "{what}; stopping the service now would not stop them, but their slots and claims \
             would be unguarded until it restarts"
        ),
        details: json!({ "running": running, "leases": leases, "plans": plans }),
        recovery: Some("hostler service stop --force".into()),
    }
}

// The one mechanism by which any path stops a running service (plan P4 Global Constraints
// "Replacement and restart", R2-2): `probe` → `may_stop` → the atomic `service.shutdown`.

/// This client against the running service, by `(contract_version, service_version)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// This client is newer than the service.
    Newer,
    Same,
    /// This client is older than the service.
    Older,
    /// A version does not parse as `major.minor.patch`.
    Unknown,
}

/// `major.minor.patch`, with a pre-release (`-…`) or build (`+…`) suffix dropped.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

/// Compares `(contract_version, service_version)` pairs lexicographically, this client's
/// first. A version that does not parse is `Unknown`, whatever the contracts say.
pub fn replace_direction(client: (u32, &str), service: (u32, &str)) -> Direction {
    let (Some(mine), Some(theirs)) = (parse_version(client.1), parse_version(service.1)) else {
        return Direction::Unknown;
    };
    match (client.0, mine).cmp(&(service.0, theirs)) {
        std::cmp::Ordering::Greater => Direction::Newer,
        std::cmp::Ordering::Equal => Direction::Same,
        std::cmp::Ordering::Less => Direction::Older,
    }
}

/// This hostler's `(CONTRACT_VERSION, version)`, what every caller passes to `may_stop`.
pub fn this_client() -> (u32, &'static str) {
    (CONTRACT_VERSION, SERVICE_VERSION)
}

/// What a lifecycle connection and one `service.status` found (no hello).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// No service runs: installing or starting needs no stop.
    Stopped,
    /// A service may run but cannot be asked, e.g. `reason: "sandbox_denied"`.
    Unreachable { reason: String },
    Running {
        contract: u32,
        version: String,
        /// `None` from a 0.0.3 service, which cannot say ("live work").
        secrets_unlocked: Option<bool>,
        pid: u32,
    },
}

impl Probe {
    fn running(status: &ServiceStatus) -> Probe {
        Probe::Running {
            contract: status.contract_version,
            version: status.service_version.clone(),
            secrets_unlocked: status.secrets_unlocked,
            pid: status.pid,
        }
    }
}

/// Probes the service of `paths`: a lifecycle connection, no hello, one `service.status`.
/// Never starts anything.
pub async fn probe(paths: &Paths) -> Probe {
    match probe_connection(paths).await {
        Ok(Some((_, status))) => Probe::running(&status),
        Ok(None) => Probe::Stopped,
        Err(e) => Probe::Unreachable {
            reason: e.details["reason"]
                .as_str()
                .unwrap_or("unreachable")
                .to_owned(),
        },
    }
}

/// `probe`, keeping the connection: a stop sent on it reaches the very service probed.
/// `Ok(None)`: no service runs.
pub(crate) async fn probe_connection(
    paths: &Paths,
) -> Result<Option<(Client, ServiceStatus)>, HostlerError> {
    let mut client = match Client::connect_lifecycle(paths).await {
        Ok(client) => client,
        Err(e) if e.details["reason"] == "not_running" => return Ok(None),
        Err(e) => return Err(e),
    };
    let value = client.call(Request::ServiceStatus).await?;
    let status = serde_json::from_value::<ServiceStatus>(value).map_err(|e| HostlerError {
        code: ErrorCode::ServiceUnavailable,
        message: format!("unreadable service.status answer: {e}"),
        details: json!({ "reason": "protocol" }),
        recovery: None,
    })?;
    if let Some(boot_id) = &status.boot_id {
        crate::procs::learn_boot_id(boot_id);
    }
    Ok(Some((client, status)))
}

/// What a caller that may stop a running service passes; never a bare flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StopRule {
    /// A typed `hostler service stop|restart|install`: today's `if_idle` rule, or `force`.
    Explicit { force: bool },
    /// A path the person did not type: needs the right direction and a locked database, then
    /// stops only through `service.shutdown { if_idle: true, refuse_if_unlocked: true }`.
    Automatic(AutoPurpose),
}

impl StopRule {
    /// The `service.shutdown` fields this rule sends: `(if_idle, refuse_if_unlocked)`.
    pub fn shutdown_fields(self) -> (bool, bool) {
        match self {
            StopRule::Explicit { force } => (!force, false),
            StopRule::Automatic(_) => (true, true),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutoPurpose {
    /// A contract mismatch: direction `Newer` only.
    Replace,
    /// Re-registering an `Absent` or `Outdated` definition: `Newer` or `Same`.
    Maintain,
}

/// Why `may_stop` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StopRefusal {
    /// The running service is newer than this client.
    NewerService,
    /// A version that does not parse: nothing can be compared.
    UnknownVersion,
    /// `Automatic(Replace)` against a service of the same version (P4-L8, R1-2): a replace
    /// stops only an older service. Only `Automatic(Maintain)` stops a same-version service,
    /// for an outdated definition.
    NotNewer,
    /// The secrets database is unlocked, or the service cannot say (0.0.3).
    UnlockedOrUnknown,
    /// `Automatic(Maintain)` against a definition that is `Current` (or cannot be read):
    /// there is nothing to re-register, so nothing is stopped for it.
    CurrentDefinition,
    /// The service cannot be asked (e.g. `sandbox_denied`), so nothing can be stopped.
    Unreachable,
}

impl StopRefusal {
    /// The stable id of the refusal (`details.refusal`).
    pub fn id(self) -> &'static str {
        match self {
            StopRefusal::NewerService => "newer_service",
            StopRefusal::UnknownVersion => "unknown_version",
            StopRefusal::NotNewer => "not_newer",
            StopRefusal::UnlockedOrUnknown => "unlocked_or_unknown",
            StopRefusal::CurrentDefinition => "current_definition",
            StopRefusal::Unreachable => "unreachable",
        }
    }

    /// Why nothing was stopped, one line.
    pub fn message(self) -> &'static str {
        match self {
            StopRefusal::NewerService => {
                "the running hostler service is newer than this hostler; an older hostler never \
                 stops it"
            }
            StopRefusal::UnknownVersion => {
                "this hostler cannot compare its version with the running service's"
            }
            StopRefusal::NotNewer => {
                "the running hostler service is not older than this hostler; nothing replaces it"
            }
            StopRefusal::UnlockedOrUnknown => {
                "the running hostler service's secrets database is unlocked (or it cannot say); \
                 stopping it would lock it"
            }
            StopRefusal::CurrentDefinition => {
                "the OS registration is current; nothing needs re-registering"
            }
            StopRefusal::Unreachable => "hostler cannot reach its service from here",
        }
    }

    /// The explicit command (or step) that does it anyway. `registered_exe`: the binary the
    /// OS registration runs, when known.
    pub fn recovery(self, registered_exe: Option<&std::path::Path>) -> String {
        match self {
            StopRefusal::NewerService => older_client_recovery(registered_exe),
            StopRefusal::UnknownVersion => match registered_exe {
                Some(exe) => format!("use the binary the service runs ({})", exe.display()),
                None => "use the hostler binary the service runs".into(),
            },
            StopRefusal::NotNewer => RESTART_RECOVERY.into(),
            StopRefusal::UnlockedOrUnknown | StopRefusal::CurrentDefinition => {
                INSTALL_RECOVERY.into()
            }
            StopRefusal::Unreachable => sandbox_recovery(None),
        }
    }
}

/// The recovery of a busy older service (R1-3): `service restart` would start the same old
/// registered binary again.
pub const INSTALL_RECOVERY: &str = "hostler service install";

/// What an agent does when its sandbox denies the socket (P4-L5: no experimental agent
/// features; P4-L6: no MCP). Without a sandbox, an inaccessible directory is a plain permissions
/// problem (P4-L18): `dir` is the directory that may not be searched, when known.
pub fn sandbox_recovery(dir: Option<&std::path::Path>) -> String {
    let dir = match dir {
        Some(dir) => dir.display().to_string(),
        None => "hostler's socket directory".into(),
    };
    format!(
        "allow hostler's socket in the agent's sandbox, or run hostler outside the sandbox; if no \
         sandbox is involved, check the permissions of {dir}; see: hostler guide sandbox"
    )
}

/// The recovery when this client is older than the running service (R1-2).
pub fn older_client_recovery(registered_exe: Option<&std::path::Path>) -> String {
    match registered_exe {
        Some(exe) => format!(
            "use the newer hostler binary (the service runs {}), or upgrade: cargo install \
             hostler",
            exe.display()
        ),
        None => "use the newer hostler binary, or upgrade: cargo install hostler".into(),
    }
}

/// Whether `rule` may stop the service `probe` found (pure; the Global Constraints table).
/// `registration` is read before any change and matters only to `Automatic(Maintain)`.
pub fn may_stop(
    rule: StopRule,
    client: (u32, &str),
    probe: &Probe,
    registration: RegistrationState,
) -> Result<(), StopRefusal> {
    let (contract, version, secrets_unlocked) = match probe {
        // Nothing runs: no live work to protect.
        Probe::Stopped => return Ok(()),
        Probe::Unreachable { .. } => return Err(StopRefusal::Unreachable),
        Probe::Running {
            contract,
            version,
            secrets_unlocked,
            ..
        } => (*contract, version.as_str(), *secrets_unlocked),
    };
    let StopRule::Automatic(purpose) = rule else {
        // Typed by a person: the service's own `if_idle` check (or `force`) decides.
        return Ok(());
    };
    match replace_direction(client, (contract, version)) {
        Direction::Older => return Err(StopRefusal::NewerService),
        Direction::Unknown => return Err(StopRefusal::UnknownVersion),
        Direction::Same if purpose == AutoPurpose::Replace => return Err(StopRefusal::NotNewer),
        Direction::Newer | Direction::Same => {}
    }
    if purpose == AutoPurpose::Maintain
        && !matches!(
            registration,
            RegistrationState::Absent | RegistrationState::Outdated
        )
    {
        return Err(StopRefusal::CurrentDefinition);
    }
    // An unlocked database is live work (R1-6); `None` is every 0.0.3 service.
    if secrets_unlocked != Some(false) {
        return Err(StopRefusal::UnlockedOrUnknown);
    }
    Ok(())
}

/// The `ServiceBusy` of `service.shutdown { refuse_if_unlocked: true }` on an unlocked
/// database (R1-6): an automatic stop never locks a person's unlock.
pub fn secrets_unlocked_busy() -> HostlerError {
    HostlerError {
        code: ErrorCode::ServiceBusy,
        message: "the secrets database is unlocked; a restart locks it".into(),
        details: json!({ "secrets": "unlocked" }),
        recovery: Some(RESTART_RECOVERY.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_matches_or_reports_both_versions() {
        let ok = hello(1, 1).unwrap();
        assert_eq!(ok.contract_version, 1);
        let err = hello(1, 999).unwrap_err();
        assert_eq!(err.code, ErrorCode::ContractMismatch);
        assert_eq!(err.details["client_contract"], 1);
        assert_eq!(err.details["service_contract"], 999);
        assert_eq!(err.details["service_version"], SERVICE_VERSION);
        assert_eq!(err.recovery.as_deref(), Some("hostler service restart"));
    }

    #[test]
    fn replace_direction_compares_contract_then_version() {
        assert_eq!(
            replace_direction((4, "0.0.4"), (4, "0.0.3")),
            Direction::Newer
        );
        assert_eq!(
            replace_direction((4, "0.0.3"), (4, "0.0.3")),
            Direction::Same
        );
        assert_eq!(
            replace_direction((4, "0.0.3"), (5, "0.0.3")),
            Direction::Older
        );
        assert_eq!(
            replace_direction((4, "9.9.9"), (5, "0.0.1")),
            Direction::Older
        );
        assert_eq!(
            replace_direction((5, "0.0.1"), (4, "9.9.9")),
            Direction::Newer
        );
        assert_eq!(
            replace_direction((4, "0.1.0"), (4, "0.0.10")),
            Direction::Newer
        );
        assert_eq!(
            replace_direction((4, "dev"), (4, "0.0.3")),
            Direction::Unknown
        );
        assert_eq!(
            replace_direction((4, "0.0.3"), (3, "dev")),
            Direction::Unknown
        );
        assert_eq!(replace_direction((4, "0.0.3"), (4, "")), Direction::Unknown);
        assert_eq!(
            replace_direction((4, "0.0"), (4, "0.0.3")),
            Direction::Unknown
        );
        assert_eq!(
            replace_direction((4, "0.0.3.1"), (4, "0.0.3")),
            Direction::Unknown
        );
        // Pre-release and build suffixes are dropped.
        assert_eq!(
            replace_direction((4, "0.0.4-rc.1"), (4, "0.0.4+build.7")),
            Direction::Same
        );
        assert_eq!(
            replace_direction((4, "0.0.4-dev"), (4, "0.0.3")),
            Direction::Newer
        );
    }

    const CLIENT: (u32, &str) = (4, "0.0.4");

    fn running(direction: Direction, secrets_unlocked: Option<bool>) -> Probe {
        let (contract, version) = match direction {
            Direction::Newer => (4, "0.0.3"),
            Direction::Same => (4, "0.0.4"),
            Direction::Older => (5, "0.0.4"),
            Direction::Unknown => (4, "dev"),
        };
        assert_eq!(replace_direction(CLIENT, (contract, version)), direction);
        Probe::Running {
            contract,
            version: version.into(),
            secrets_unlocked,
            pid: 42,
        }
    }

    const RULES: [StopRule; 4] = [
        StopRule::Explicit { force: false },
        StopRule::Explicit { force: true },
        StopRule::Automatic(AutoPurpose::Replace),
        StopRule::Automatic(AutoPurpose::Maintain),
    ];
    const DIRECTIONS: [Direction; 4] = [
        Direction::Newer,
        Direction::Same,
        Direction::Older,
        Direction::Unknown,
    ];
    const LOCKS: [Option<bool>; 3] = [Some(false), Some(true), None];
    const REGISTRATIONS: [RegistrationState; 4] = [
        RegistrationState::Absent,
        RegistrationState::Outdated,
        RegistrationState::Current,
        RegistrationState::Unknown,
    ];

    /// The Global Constraints table, row by row (R2-2; P4-L8 R1-2): every rule × direction ×
    /// `secrets_unlocked` × registration.
    #[test]
    fn may_stop_table() {
        use AutoPurpose::{Maintain, Replace};
        use StopRefusal::*;
        let mut allowed = Vec::new();
        for rule in RULES {
            for direction in DIRECTIONS {
                for lock in LOCKS {
                    for registration in REGISTRATIONS {
                        let got = may_stop(rule, CLIENT, &running(direction, lock), registration);
                        let expected = match (rule, direction) {
                            // Typed by a person: the service's own if_idle (or force) decides.
                            (StopRule::Explicit { .. }, _) => Ok(()),
                            (StopRule::Automatic(_), Direction::Older) => Err(NewerService),
                            (StopRule::Automatic(_), Direction::Unknown) => Err(UnknownVersion),
                            (StopRule::Automatic(Replace), Direction::Same) => Err(NotNewer),
                            (StopRule::Automatic(Maintain), _)
                                if !matches!(
                                    registration,
                                    RegistrationState::Absent | RegistrationState::Outdated
                                ) =>
                            {
                                Err(CurrentDefinition)
                            }
                            (StopRule::Automatic(_), _) if lock != Some(false) => {
                                Err(UnlockedOrUnknown)
                            }
                            (StopRule::Automatic(_), _) => Ok(()),
                        };
                        assert_eq!(
                            got, expected,
                            "{rule:?} {direction:?} {lock:?} {registration:?}"
                        );
                        if got.is_ok() && matches!(rule, StopRule::Automatic(_)) {
                            allowed.push((rule, direction, registration));
                        }
                    }
                }
            }
        }
        // The automatic rules stop exactly these: a locked database, and Newer (Replace), or
        // Newer/Same with an Absent/Outdated definition (Maintain).
        let mut expected = Vec::new();
        for registration in REGISTRATIONS {
            expected.push((StopRule::Automatic(Replace), Direction::Newer, registration));
        }
        for direction in [Direction::Newer, Direction::Same] {
            for registration in [RegistrationState::Absent, RegistrationState::Outdated] {
                expected.push((StopRule::Automatic(Maintain), direction, registration));
            }
        }
        allowed.sort_by_key(|row| format!("{row:?}"));
        expected.sort_by_key(|row| format!("{row:?}"));
        assert_eq!(allowed, expected);

        // The named rows.
        let outdated = RegistrationState::Outdated;
        let current = RegistrationState::Current;
        let maintain = StopRule::Automatic(Maintain);
        let replace = StopRule::Automatic(Replace);
        let same_locked = running(Direction::Same, Some(false));
        assert_eq!(may_stop(maintain, CLIENT, &same_locked, outdated), Ok(()));
        assert_eq!(
            may_stop(maintain, CLIENT, &same_locked, current),
            Err(CurrentDefinition)
        );
        assert_eq!(
            may_stop(replace, CLIENT, &same_locked, outdated),
            Err(NotNewer)
        );
    }

    /// No service: nothing to protect. Unreachable: nothing can be asked, so nothing stops.
    #[test]
    fn may_stop_without_a_running_service() {
        let unreachable = Probe::Unreachable {
            reason: "sandbox_denied".into(),
        };
        for rule in RULES {
            for registration in REGISTRATIONS {
                assert_eq!(
                    may_stop(rule, CLIENT, &Probe::Stopped, registration),
                    Ok(())
                );
                assert_eq!(
                    may_stop(rule, CLIENT, &unreachable, registration),
                    Err(StopRefusal::Unreachable)
                );
            }
        }
    }

    #[test]
    fn refusals_name_their_explicit_command() {
        let exe = std::path::Path::new("/opt/hostler/bin/hostler");
        let newer = StopRefusal::NewerService.recovery(Some(exe));
        assert!(newer.contains("/opt/hostler/bin/hostler"), "{newer}");
        assert!(newer.contains("cargo install hostler"), "{newer}");
        assert_eq!(
            StopRefusal::UnlockedOrUnknown.recovery(None),
            "hostler service install"
        );
        assert!(
            StopRefusal::Unreachable
                .recovery(None)
                .contains("hostler guide sandbox")
        );
        let ids: std::collections::HashSet<_> = [
            StopRefusal::NewerService,
            StopRefusal::UnknownVersion,
            StopRefusal::NotNewer,
            StopRefusal::UnlockedOrUnknown,
            StopRefusal::CurrentDefinition,
            StopRefusal::Unreachable,
        ]
        .iter()
        .map(|r| r.id())
        .collect();
        assert_eq!(ids.len(), 6);
    }

    #[test]
    fn automatic_rules_send_the_atomic_shutdown() {
        assert_eq!(
            StopRule::Explicit { force: false }.shutdown_fields(),
            (true, false)
        );
        assert_eq!(
            StopRule::Explicit { force: true }.shutdown_fields(),
            (false, false)
        );
        for purpose in [AutoPurpose::Replace, AutoPurpose::Maintain] {
            assert_eq!(StopRule::Automatic(purpose).shutdown_fields(), (true, true));
        }
    }
}
