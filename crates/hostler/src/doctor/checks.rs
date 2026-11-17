//! Doctor's checks, one function per check id, in the fixed order (plan P4 Task 6).
//!
//! One pass (`gather`) probes the service once (a lifecycle connection and one
//! `service.status`; never a start), connects with `hello` only when the contracts match, and
//! reads the project from the checkout. Checks that need what an earlier one could not get are
//! `skipped` with the reason. The pass is bounded: every service call shares one 20 s deadline.
//! Reads only: no `workspace.resolve` or `project.resolve` (doctor never registers a checkout),
//! and no env file (R1-4).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::client::{Client, SANDBOX_DENIED, is_sandbox_denied};
use crate::contract::accounts::AccountInfo;
use crate::contract::claims::{ClaimKind, ClaimsSnapshot, HoldBinding, WorkspaceInfo};
use crate::contract::devices::{AdbState, DevicesSnapshot, ToolStatus};
use crate::contract::doctor::{Check, CheckStatus, Fix, Who};
use crate::contract::heavy::ServiceStatus;
use crate::contract::secrets::{LockState, ProjectInfo, ProjectRef, SecretList, SecretState};
use crate::contract::wire::Request;
use crate::contract::{CONTRACT_VERSION, ErrorCode, HostlerError};
use crate::project::config::{self, ConfigProblem, ProjectConfig};
use crate::project::{self, Checkout, display_path};
use crate::service::lifecycle::{
    self, AutoPurpose, Direction, Probe, StopRefusal, StopRule, this_client,
};
use crate::service::manager::RegistrationState;

use super::block::{self, BlockState};
use super::files::{self, ReadRefused};
use super::logind;
use super::{DoctorEnv, FixAction};

/// The whole command, `--fix` included (plan: "the whole run within 20 s"; review t6-r1 R1-1).
pub(crate) const CHECKS_BOUND: Duration = Duration::from_secs(20);

/// One registration lookup (`registration()`, `registered_exe()`).
pub(crate) const LOOKUP_BOUND: Duration = Duration::from_secs(5);

/// A TTL hold older than this is reported by `claims.workspace`.
const STALE_HOLD: Duration = Duration::from_secs(2 * 60 * 60);

/// One check and, when its fix is safe, what `--fix` runs for it.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub check: Check,
    /// `Some` exactly when `check.fix` is `Some` with `safe: true`.
    pub action: Option<FixAction>,
}

impl Found {
    fn new(id: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Found {
        Found {
            check: Check {
                id: id.into(),
                status,
                message: message.into(),
                fix: None,
                details: json!({}),
            },
            action: None,
        }
    }

    fn ok(id: impl Into<String>, message: impl Into<String>) -> Found {
        Found::new(id, CheckStatus::Ok, message)
    }

    fn skipped(id: impl Into<String>, message: impl Into<String>) -> Found {
        Found::new(id, CheckStatus::Skipped, message)
    }

    fn details(mut self, details: Value) -> Found {
        self.check.details = details;
        self
    }

    fn fix(mut self, fix: Fix) -> Found {
        self.check.fix = Some(fix);
        self
    }

    /// A fix `--fix` runs: an agent's command, marked safe.
    fn safe_fix(mut self, command: &str, text: impl Into<String>, action: FixAction) -> Found {
        self.check.fix = Some(Fix {
            command: Some(command.into()),
            text: text.into(),
            who: Who::Agent,
            safe: true,
        });
        self.action = Some(action);
        self
    }
}

fn agent(command: Option<&str>, text: impl Into<String>) -> Fix {
    Fix {
        command: command.map(str::to_owned),
        text: text.into(),
        who: Who::Agent,
        safe: false,
    }
}

fn person(command: Option<&str>, text: impl Into<String>) -> Fix {
    Fix {
        command: command.map(str::to_owned),
        text: text.into(),
        who: Who::Person,
        safe: false,
    }
}

/// Why an automatic stop is not safe now, for a fix's text (`verb`: "installing",
/// "restarting").
fn why_not(refusal: StopRefusal, verb: &str) -> String {
    match refusal {
        StopRefusal::NewerService => "the running service is newer".into(),
        StopRefusal::UnknownVersion => {
            "the running service's version cannot be compared with this hostler's".into()
        }
        StopRefusal::NotNewer => "the running service is not older than this hostler".into(),
        StopRefusal::UnlockedOrUnknown => format!(
            "its secrets database is unlocked (or it cannot say); {verb} locks it, so only with \
             the person's agreement"
        ),
        StopRefusal::CurrentDefinition => "the OS registration is current".into(),
        StopRefusal::Unreachable => "hostler cannot reach its service from here".into(),
    }
}

/// What the lifecycle probe found.
#[derive(Debug)]
enum Svc {
    Stopped,
    Unreachable(HostlerError),
    Running(ServiceStatus),
}

/// `hostler.toml` as doctor read it.
#[derive(Debug)]
enum ConfigState {
    Absent,
    Valid(ProjectConfig),
    Invalid(Vec<ConfigProblem>),
    Unreadable(String),
}

struct ProjectFacts {
    checkout: Checkout,
    config: ConfigState,
    project: ProjectRef,
}

impl ProjectFacts {
    fn root(&self) -> &Path {
        &self.checkout.workspace_root
    }

    fn valid(&self) -> Option<&ProjectConfig> {
        match &self.config {
            ConfigState::Valid(config) => Some(config),
            _ => None,
        }
    }
}

/// What one pass learned, shared by the checks in order.
pub(crate) struct Ctx<'a> {
    env: &'a DoctorEnv,
    deadline: Instant,
    /// `None` under a custom `HOSTLER_HOME`.
    registration: Option<RegistrationState>,
    /// Why the registration reads `Unknown`, for the message.
    registration_why: String,
    /// The binary the OS registration runs (bounded lookup; `None` under a custom home).
    registered_exe: Option<PathBuf>,
    service: Svc,
    /// Said `hello`; only when the contracts match.
    client: Option<Client>,
    lock: Option<LockState>,
    project: Option<ProjectFacts>,
    /// This checkout's project in `project.list`, when listed.
    project_info: Option<ProjectInfo>,
}

impl<'a> Ctx<'a> {
    async fn new(env: &'a DoctorEnv, cwd: &Path, deadline: Instant) -> Ctx<'a> {
        // R1-4 (review t6-r1): both lookups may run `schtasks` on Windows (or a slow
        // `systemctl`): each on its own worker, bounded by `LOOKUP_BOUND` and the deadline.
        let limit = LOOKUP_BOUND.min(deadline.saturating_duration_since(Instant::now()));
        let mut registration_why = "the OS service manager could not tell".to_owned();
        let (registration, registered_exe) = if env.paths.custom_home {
            (None, None)
        } else {
            let registrar = env.registrar.clone();
            let lookup = env.registered_exe.clone();
            let (state, exe) = tokio::join!(
                super::bounded_blocking(limit, move || registrar.registration()),
                super::bounded_blocking(limit, move || lookup()),
            );
            if state.is_none() {
                registration_why =
                    format!("the OS service manager did not answer within {limit:?}");
            }
            (
                Some(state.unwrap_or(RegistrationState::Unknown)),
                exe.flatten(),
            )
        };
        let service = match &env.paths_error {
            Some(e) => Svc::Unreachable(e.clone()),
            None => match tokio::time::timeout_at(
                deadline.into(),
                lifecycle::probe_connection(&env.paths),
            )
            .await
            {
                Ok(Ok(Some((_, status)))) => Svc::Running(status),
                Ok(Ok(None)) => Svc::Stopped,
                Ok(Err(e)) => Svc::Unreachable(e),
                Err(_) => Svc::Unreachable(timed_out()),
            },
        };
        let client = match &service {
            Svc::Running(status) if status.contract_version == CONTRACT_VERSION => {
                tokio::time::timeout_at(deadline.into(), Client::connect(&env.paths))
                    .await
                    .ok()
                    .and_then(Result::ok)
            }
            _ => None,
        };
        let project = project::find_checkout(cwd).ok().map(|checkout| {
            let config = match files::read_text(&checkout.workspace_root, "hostler.toml") {
                Err(ReadRefused::NotFound) => ConfigState::Absent,
                Err(other) => ConfigState::Unreadable(other.to_string()),
                Ok(src) => match config::parse(&src) {
                    Ok(config) => ConfigState::Valid(config),
                    Err(problems) => ConfigState::Invalid(problems),
                },
            };
            let name = match &config {
                ConfigState::Valid(config) => config.name.clone(),
                _ => None,
            }
            .unwrap_or_else(|| project::default_name(&checkout.common_dir));
            let project = ProjectRef {
                common_dir: display_path(&checkout.common_dir),
                name,
            };
            ProjectFacts {
                checkout,
                config,
                project,
            }
        });
        Ctx {
            env,
            deadline,
            registration,
            registration_why,
            registered_exe,
            service,
            client,
            lock: None,
            project,
            project_info: None,
        }
    }

    fn probe(&self) -> Probe {
        match &self.service {
            Svc::Stopped => Probe::Stopped,
            Svc::Unreachable(e) => Probe::Unreachable {
                reason: e.details["reason"]
                    .as_str()
                    .unwrap_or("unreachable")
                    .to_owned(),
            },
            Svc::Running(status) => Probe::Running {
                contract: status.contract_version,
                version: status.service_version.clone(),
                secrets_unlocked: status.secrets_unlocked,
                pid: status.pid,
            },
        }
    }

    /// Why a check that needs the service is skipped.
    fn no_service(&self) -> String {
        match &self.service {
            Svc::Stopped => "the service is not running (see service.reachable)".into(),
            Svc::Unreachable(_) => {
                "hostler cannot reach its service from here (see service.reachable)".into()
            }
            Svc::Running(status) if status.contract_version != CONTRACT_VERSION => {
                "the service speaks another contract (see service.contract)".into()
            }
            Svc::Running(_) => "the service did not answer in time".into(),
        }
    }

    /// One call on the hello'd connection, within the pass's deadline.
    async fn call<T: DeserializeOwned>(&mut self, request: Request) -> Result<T, String> {
        let reason = self.no_service();
        let deadline = self.deadline;
        let Some(client) = self.client.as_mut() else {
            return Err(reason);
        };
        match tokio::time::timeout_at(deadline.into(), client.call(request)).await {
            Err(_) => {
                self.client = None;
                Err(timed_out().message)
            }
            Ok(Err(e)) => Err(e.message),
            Ok(Ok(value)) => {
                serde_json::from_value(value).map_err(|e| format!("unreadable answer: {e}"))
            }
        }
    }
}

fn timed_out() -> HostlerError {
    HostlerError {
        code: ErrorCode::ServiceUnavailable,
        message: format!("the hostler service did not answer within doctor's {CHECKS_BOUND:?}"),
        details: json!({ "reason": "timeout" }),
        recovery: None,
    }
}

/// One pass of every check, in the fixed order, within `deadline` (the command's).
pub(crate) async fn gather(env: &DoctorEnv, cwd: &Path, deadline: Instant) -> Vec<Found> {
    let mut ctx = Ctx::new(env, cwd, deadline).await;
    let mut out = vec![
        service_registered(&ctx),
        service_reachable(&ctx),
        service_contract(&ctx),
        service_binary(&ctx),
        linux_session(&ctx).await,
        secrets_database(&mut ctx).await,
    ];
    out.extend(project_config(&ctx));
    out.push(project_approved(&mut ctx).await);
    out.push(project_docs(&ctx));
    out.push(project_device_rules(&ctx));
    out.extend(secrets_required(&mut ctx).await);
    out.extend(accounts_required(&mut ctx).await);
    out.extend(tools(&mut ctx).await);
    out.extend(workspace_checks(&mut ctx).await);
    if env.dev_build {
        out = out
            .into_iter()
            .map(|found| no_dev_install(found, &env.exe))
            .collect();
    }
    out
}

/// D-Q2, C-3 (P4 clean read): `--fix` never registers a dev build, so an install fix of a dev
/// build is not offered as safe: it is the typed step, with the dev-build note (as init's).
fn no_dev_install(mut found: Found, exe: &Path) -> Found {
    if matches!(found.action, Some(FixAction::Install(_))) {
        found.action = None;
        found.check.fix = Some(agent(
            Some("hostler service install"),
            super::fix::dev_build_note(exe),
        ));
    }
    found
}

// ---------------------------------------------------------------------------------------------
// The service

/// `service install` through `Automatic(Maintain)`: safe when nothing runs, or when `may_stop`
/// allows the registration-maintenance stop (direction `Newer` or `Same`, locked).
fn maintain_fix(probe: &Probe, found: Found, registration: RegistrationState) -> Found {
    let rule = StopRule::Automatic(AutoPurpose::Maintain);
    match lifecycle::may_stop(rule, this_client(), probe, registration) {
        Ok(()) => found.safe_fix(
            "hostler service install",
            "registers this hostler with the OS; a running service is stopped only when it is \
             idle and its secrets database is locked",
            FixAction::Install(AutoPurpose::Maintain),
        ),
        Err(refusal) => found.fix(agent(
            Some("hostler service install"),
            format!("not safe now: {}", why_not(refusal, "installing")),
        )),
    }
}

fn service_registered(ctx: &Ctx) -> Found {
    const ID: &str = "service.registered";
    let Some(registration) = ctx.registration else {
        return Found::skipped(
            ID,
            "HOSTLER_HOME is set: OS registration applies to the default hostler home only",
        );
    };
    let details = json!({ "registration": registration_name(registration) });
    match registration {
        RegistrationState::Current => Found::ok(ID, "registered with the OS").details(details),
        RegistrationState::Unknown => Found::new(
            ID,
            CheckStatus::Warn,
            format!("cannot read the registration: {}", ctx.registration_why),
        )
        .details(details),
        RegistrationState::Absent => maintain_fix(
            &ctx.probe(),
            Found::new(
                ID,
                CheckStatus::Problem,
                "hostler is not registered with the OS: a service an agent starts runs inside \
                 that agent's sandbox and ends with it",
            )
            .details(details),
            registration,
        ),
        RegistrationState::Outdated => maintain_fix(
            &ctx.probe(),
            Found::new(
                ID,
                CheckStatus::Problem,
                "the OS registration is outdated: it lacks ProcessType Interactive (macOS) or \
                 --managed",
            )
            .details(details),
            registration,
        ),
    }
}

fn registration_name(state: RegistrationState) -> &'static str {
    match state {
        RegistrationState::Absent => "absent",
        RegistrationState::Current => "current",
        RegistrationState::Outdated => "outdated",
        RegistrationState::Unknown => "unknown",
    }
}

fn service_reachable(ctx: &Ctx) -> Found {
    const ID: &str = "service.reachable";
    match &ctx.service {
        Svc::Running(status) => Found::ok(
            ID,
            format!(
                "the service answers (hostler {}, pid {})",
                status.service_version, status.pid
            ),
        )
        .details(json!({ "pid": status.pid, "service_version": status.service_version })),
        Svc::Stopped => Found::new(
            ID,
            CheckStatus::Problem,
            "the hostler service is not running",
        )
        .details(json!({ "reason": "not_running" }))
        .safe_fix(
            "hostler service start",
            "starts it (through the OS registration when there is one)",
            FixAction::Start,
        ),
        Svc::Unreachable(e) if is_sandbox_denied(e) => {
            // R1-13, P4-L5/L7/L18: a sandbox setting, or approving a run outside the sandbox,
            // is the person's; nothing here can be run by doctor.
            let text = e.recovery.clone().unwrap_or_else(|| {
                lifecycle::sandbox_recovery(e.details["dir"].as_str().map(Path::new))
            });
            Found::new(ID, CheckStatus::Problem, e.message.clone())
                .details(json!({
                    "reason": SANDBOX_DENIED,
                    "socket": e.details["socket"],
                    "dir": e.details["dir"],
                }))
                .fix(person(None, text))
        }
        Svc::Unreachable(e) => Found::new(ID, CheckStatus::Problem, e.message.clone())
            .details(json!({ "reason": e.details["reason"], "socket": e.details["socket"] }))
            .fix(agent(
                None,
                "the service does not answer; see: hostler guide recovery",
            )),
    }
}

fn version_details(status: &ServiceStatus, direction: Direction) -> Value {
    json!({
        "client_contract": CONTRACT_VERSION,
        "service_contract": status.contract_version,
        "client_version": lifecycle::SERVICE_VERSION,
        "service_version": status.service_version,
        "direction": direction_name(direction),
        "secrets_unlocked": status.secrets_unlocked,
    })
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Newer => "newer",
        Direction::Same => "same",
        Direction::Older => "older",
        Direction::Unknown => "unknown",
    }
}

fn service_contract(ctx: &Ctx) -> Found {
    const ID: &str = "service.contract";
    let Svc::Running(status) = &ctx.service else {
        return Found::skipped(ID, ctx.no_service());
    };
    let direction = lifecycle::replace_direction(
        this_client(),
        (status.contract_version, &status.service_version),
    );
    let details = version_details(status, direction);
    if status.contract_version == CONTRACT_VERSION {
        return Found::ok(
            ID,
            format!("the service speaks contract {CONTRACT_VERSION}"),
        )
        .details(details);
    }
    let registered = ctx.registered_exe.as_deref();
    let (v, w) = (lifecycle::SERVICE_VERSION, &status.service_version);
    let (n, m) = (CONTRACT_VERSION, status.contract_version);
    match direction {
        Direction::Newer | Direction::Same => {
            let found = Found::new(
                ID,
                CheckStatus::Problem,
                format!(
                    "the running service (contract {m}, hostler {w}) is older than this hostler \
                     (contract {n}, version {v})"
                ),
            )
            .details(details);
            // `install`, never `restart`: a restart starts the same old registered binary
            // (R1-3). Under a custom home there is no registration: `service start` replaces.
            let (command, action) = if ctx.env.paths.custom_home {
                ("hostler service start", FixAction::Start)
            } else {
                (
                    "hostler service install",
                    FixAction::Install(AutoPurpose::Replace),
                )
            };
            // R2-2 (review t6-r2): an automatic install needs a registration doctor could read
            // (`Current`, `Absent` or `Outdated`); over an unreadable one it is the person's.
            if ctx.registration == Some(RegistrationState::Unknown) {
                return found.fix(person(
                    Some(command),
                    format!(
                        "cannot read the registration: {}; doctor registers nothing over it, so \
                         a person who means it types the command",
                        ctx.registration_why
                    ),
                ));
            }
            let rule = StopRule::Automatic(AutoPurpose::Replace);
            match lifecycle::may_stop(
                rule,
                this_client(),
                &ctx.probe(),
                RegistrationState::Unknown,
            ) {
                Ok(()) => found.safe_fix(
                    command,
                    "replaces the older service with this hostler when it is idle and its \
                     secrets database is locked",
                    action,
                ),
                Err(refusal) => found.fix(agent(
                    Some(command),
                    format!("not safe now: {}", why_not(refusal, "installing")),
                )),
            }
        }
        Direction::Older => Found::new(
            ID,
            CheckStatus::Problem,
            format!(
                "this hostler (contract {n}, version {v}) is older than the running service \
                 (contract {m}, version {w})"
            ),
        )
        .details(details)
        .fix(agent(None, lifecycle::older_client_recovery(registered))),
        Direction::Unknown => Found::new(
            ID,
            CheckStatus::Problem,
            format!("cannot compare hostler {v} with the running service's {w}"),
        )
        .details(details)
        // The plan's Older/Unknown recovery (review t6-r1 R1-5): a version that does not
        // parse is not a binary to keep using.
        .fix(agent(None, lifecycle::older_client_recovery(registered))),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The binary the OS registration runs, as far as `service.binary` can tell (C-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegisteredBin<'a> {
    /// No registration (a custom home, or none registered): a restart starts this hostler.
    NoRegistration,
    /// The registration runs this hostler.
    This,
    /// The registration runs another binary.
    Other(&'a Path),
    /// A registration whose binary could not be read (the lookup failed or timed out).
    Unknown,
}

impl RegisteredBin<'_> {
    fn name(self) -> &'static str {
        match self {
            RegisteredBin::NoRegistration => "none",
            RegisteredBin::This => "this",
            RegisteredBin::Other(_) => "other",
            RegisteredBin::Unknown => "unknown",
        }
    }
}

fn service_binary(ctx: &Ctx) -> Found {
    const ID: &str = "service.binary";
    let status = match &ctx.service {
        Svc::Running(status) if status.contract_version == CONTRACT_VERSION => status,
        _ => return Found::skipped(ID, ctx.no_service()),
    };
    let direction = lifecycle::replace_direction(
        this_client(),
        (status.contract_version, &status.service_version),
    );
    // C-2 (P4 clean read): which binary a restart would start. "No registration" (a custom
    // home, or none registered) starts this hostler; a registration whose binary could not be
    // read may start an older one again, so it is never restarted automatically.
    let registered_bin =
        if ctx.env.paths.custom_home || ctx.registration == Some(RegistrationState::Absent) {
            RegisteredBin::NoRegistration
        } else {
            match ctx.registered_exe.as_deref() {
                Some(exe) if same_file(exe, &ctx.env.exe) => RegisteredBin::This,
                Some(exe) => RegisteredBin::Other(exe),
                None => RegisteredBin::Unknown,
            }
        };
    let registered = match registered_bin {
        RegisteredBin::This => ctx.registered_exe.as_deref(),
        RegisteredBin::Other(exe) => Some(exe),
        RegisteredBin::NoRegistration | RegisteredBin::Unknown => None,
    };
    let differs = match registered_bin {
        RegisteredBin::Other(exe) => Some(exe),
        _ => None,
    };
    let mut details = version_details(status, direction);
    details["exe"] = json!(ctx.env.exe.display().to_string());
    details["registered_exe"] = json!(registered.map(|p| p.display().to_string()));
    details["registered_binary"] = json!(registered_bin.name());
    let (v, w) = (lifecycle::SERVICE_VERSION, &status.service_version);
    match (direction, differs) {
        (Direction::Older, _) => Found::new(
            ID,
            CheckStatus::Warn,
            format!("this hostler ({v}) is older than the running service ({w})"),
        )
        .details(details)
        .fix(agent(
            None,
            format!(
                "this hostler is older than the running service; {}",
                lifecycle::older_client_recovery(registered)
            ),
        )),
        (Direction::Unknown, _) => Found::new(
            ID,
            CheckStatus::Warn,
            format!("cannot compare hostler {v} with the running service's {w}"),
        )
        .details(details)
        .fix(agent(
            None,
            StopRefusal::UnknownVersion.recovery(registered),
        )),
        // A restart would start the registered binary again: only a new registration helps.
        (Direction::Newer | Direction::Same, Some(registered)) => Found::new(
            ID,
            CheckStatus::Warn,
            format!(
                "the OS registration runs {}, not this hostler ({})",
                registered.display(),
                ctx.env.exe.display()
            ),
        )
        .details(details)
        .fix(person(
            Some("hostler service install"),
            "registers the hostler that runs it, which changes which binary the service runs; \
             type it with the binary you mean to keep",
        )),
        (Direction::Same, None) => {
            Found::ok(ID, format!("the service runs this hostler's version ({v})")).details(details)
        }
        (Direction::Newer, None) if registered_bin == RegisteredBin::Unknown => Found::new(
            ID,
            CheckStatus::Warn,
            format!(
                "the running service is hostler {w}; this hostler is {v} (newer), and the binary \
                 the OS registration runs could not be read"
            ),
        )
        .details(details)
        .fix(person(
            Some("hostler service install"),
            "a restart could start the registered (older) binary again, so doctor restarts \
             nothing; a person installs the hostler meant to keep (it locks the secrets database)",
        )),
        (Direction::Newer, None) => {
            let found = Found::new(
                ID,
                CheckStatus::Warn,
                format!("the running service is hostler {w}; this hostler is {v} (newer)"),
            )
            .details(details);
            let rule = StopRule::Automatic(AutoPurpose::Replace);
            match lifecycle::may_stop(
                rule,
                this_client(),
                &ctx.probe(),
                RegistrationState::Unknown,
            ) {
                Ok(()) => found.safe_fix(
                    "hostler service restart",
                    "restarts the service when it is idle and its secrets database is locked",
                    FixAction::Restart,
                ),
                Err(refusal) => found.fix(agent(
                    Some("hostler service restart"),
                    format!("not safe now: {}", why_not(refusal, "restarting")),
                )),
            }
        }
    }
}

/// W-6 (call 16). The decision is pure (tested on every OS); the probe runs on Linux only.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
fn linux_session_found(
    registration: Option<RegistrationState>,
    unknown_why: &str,
    probe: &Probe,
    session: &logind::Session,
) -> Found {
    const ID: &str = "service.linux_session";
    let Some(registration) = registration else {
        return Found::skipped(
            ID,
            "HOSTLER_HOME is set: OS registration applies to the default hostler home only",
        );
    };
    let details = json!({
        "kill_user_processes": session.kill_user_processes,
        "linger": session.linger,
        "registration": registration_name(registration),
    });
    // R1-2 (review t6-r1): an unreadable registration is never taken for an absent one, so
    // nothing is registered over a definition doctor could not read.
    if registration == RegistrationState::Unknown {
        return Found::new(
            ID,
            CheckStatus::Warn,
            format!("cannot read the registration: {unknown_why}"),
        )
        .details(details);
    }
    let registered = matches!(
        registration,
        RegistrationState::Current | RegistrationState::Outdated
    );
    if !registered {
        return match session.kill_user_processes {
            Some(true) => maintain_fix(
                probe,
                Found::new(
                    ID,
                    CheckStatus::Problem,
                    "logind ends your processes with your last session (KillUserProcesses), and \
                     hostler is not registered with systemd, so its service ends with it",
                )
                .details(details),
                registration,
            ),
            Some(false) => {
                Found::ok(ID, "logind keeps processes after the last session ends").details(details)
            }
            None => Found::skipped(
                ID,
                "cannot read logind's KillUserProcesses (no busctl, no logind.conf value)",
            )
            .details(details),
        };
    }
    match session.linger {
        Some(true) => {
            Found::ok(ID, "the user lingers: the service outlives your sessions").details(details)
        }
        Some(false) => Found::new(
            ID,
            CheckStatus::Warn,
            "the service stops when your last session ends (no linger)",
        )
        .details(details)
        .fix(person(
            Some(&format!("loginctl enable-linger {}", session.user)),
            "lets the user's services run without a session",
        )),
        None => Found::skipped(ID, "loginctl is missing or could not tell").details(details),
    }
}

#[cfg(target_os = "linux")]
async fn linux_session(ctx: &Ctx<'_>) -> Found {
    // R2-5 (review t6-r2): logind is asked within the command's deadline.
    if ctx.registration.is_some() && Instant::now() >= ctx.deadline {
        return Found::skipped(
            "service.linux_session",
            "doctor ran out of time before asking logind",
        );
    }
    let session = match ctx.registration {
        Some(_) => logind::probe(ctx.deadline).await,
        None => logind::Session {
            kill_user_processes: None,
            linger: None,
            user: String::new(),
        },
    };
    linux_session_found(
        ctx.registration,
        &ctx.registration_why,
        &ctx.probe(),
        &session,
    )
}

#[cfg(not(target_os = "linux"))]
async fn linux_session(_ctx: &Ctx<'_>) -> Found {
    Found::skipped("service.linux_session", "not Linux")
}

// ---------------------------------------------------------------------------------------------
// Secrets and the project

async fn secrets_database(ctx: &mut Ctx<'_>) -> Found {
    const ID: &str = "secrets.database";
    let status = match ctx
        .call::<crate::contract::secrets::SecretsStatus>(Request::SecretsStatus)
        .await
    {
        Ok(status) => status,
        Err(why) => return Found::skipped(ID, why),
    };
    ctx.lock = Some(status.state);
    let unlock = person(
        Some("hostler unlock"),
        "a person unlocks it (or creates it) in a terminal",
    );
    match status.state {
        LockState::Missing => Found::new(ID, CheckStatus::Warn, "no secrets database yet")
            .details(json!({ "state": "missing" }))
            .fix(unlock),
        LockState::Locked => Found::new(ID, CheckStatus::Warn, "the secrets database is locked")
            .details(json!({ "state": "locked" }))
            .fix(unlock),
        LockState::Unlocked => Found::ok(ID, "the secrets database is unlocked")
            .details(json!({ "state": "unlocked" })),
    }
}

const NOT_IN_CHECKOUT: &str = "not in a git checkout";

fn project_config(ctx: &Ctx) -> Vec<Found> {
    const ID: &str = "project.config";
    let Some(project) = &ctx.project else {
        return vec![Found::skipped(ID, NOT_IN_CHECKOUT)];
    };
    let init = |text: &str| agent(Some("hostler init"), text);
    match &project.config {
        ConfigState::Absent => vec![
            Found::new(ID, CheckStatus::Warn, "no hostler.toml in this checkout").fix(init(
                "hostler init proposes one from the project; review it, then: hostler init --write",
            )),
        ],
        ConfigState::Unreadable(why) => vec![
            Found::new(
                ID,
                CheckStatus::Problem,
                format!("hostler.toml cannot be read: {why}"),
            )
            .fix(init("fix the file by hand; hostler init never rewrites it")),
        ],
        ConfigState::Invalid(problems) => {
            let mut seen = std::collections::BTreeMap::<usize, usize>::new();
            problems
                .iter()
                .map(|problem| {
                    let count = seen.entry(problem.line).or_default();
                    *count += 1;
                    let id = if *count == 1 {
                        format!("{ID}.{}", problem.line)
                    } else {
                        format!("{ID}.{}-{count}", problem.line)
                    };
                    Found::new(
                        id,
                        CheckStatus::Problem,
                        format!("hostler.toml line {}: {}", problem.line, problem.message),
                    )
                    .details(json!({ "line": problem.line }))
                    .fix(init(
                        "fix this line of hostler.toml by hand (hostler init never rewrites it); \
                         see: hostler guide init",
                    ))
                })
                .collect()
        }
        ConfigState::Valid(config) => {
            let todo: Vec<&str> = config
                .secrets
                .iter()
                .map(|(name, decl)| (name, &decl.purpose))
                .chain(
                    config
                        .accounts
                        .iter()
                        .map(|(name, decl)| (name, &decl.purpose)),
                )
                .filter(|(_, purpose)| purpose.trim_start().starts_with("TODO"))
                .map(|(name, _)| name.as_str())
                .collect();
            if todo.is_empty() {
                vec![Found::ok(ID, "hostler.toml is valid")]
            } else {
                vec![
                    Found::new(
                        ID,
                        CheckStatus::Warn,
                        format!(
                            "these declarations still have a TODO purpose: {}",
                            todo.join(", ")
                        ),
                    )
                    .details(json!({ "todo": todo }))
                    .fix(agent(None, "say in hostler.toml what each one is for")),
                ]
            }
        }
    }
}

async fn project_approved(ctx: &mut Ctx<'_>) -> Found {
    const ID: &str = "project.approved";
    let Some(common_dir) = ctx.project.as_ref().map(|p| p.checkout.common_dir.clone()) else {
        return Found::skipped(ID, NOT_IN_CHECKOUT);
    };
    // `project.list`, never `project.resolve`: a read registers nothing.
    let projects = match ctx.call::<Vec<ProjectInfo>>(Request::ProjectList).await {
        Ok(projects) => projects,
        Err(why) => return Found::skipped(ID, why),
    };
    let found = projects
        .into_iter()
        .find(|info| same_file(Path::new(&info.common_dir), &common_dir));
    ctx.project_info = found.clone();
    match found {
        Some(info) if info.approved => Found::ok(
            ID,
            format!("this project ({}) is approved for secrets", info.name),
        )
        .details(json!({ "project": info.name })),
        _ => Found::new(
            ID,
            CheckStatus::Problem,
            "this project is not approved for secrets",
        )
        .fix(person(
            Some("hostler project approve"),
            "a person approves it in a terminal, typing the project's name",
        )),
    }
}

fn project_docs(ctx: &Ctx) -> Found {
    const ID: &str = "project.docs";
    let Some(project) = &ctx.project else {
        return Found::skipped(ID, NOT_IN_CHECKOUT);
    };
    let Some(config) = project.valid() else {
        return Found::skipped(
            ID,
            "hostler.toml is missing or invalid (see project.config)",
        );
    };
    // Task 5's placement and block (`init::docs`), through doctor's adapter.
    let states = block::states(project.root(), config);
    let details = json!({
        "files": states
            .iter()
            .map(|(path, state)| json!({ "path": path, "state": block_state_name(state) }))
            .collect::<Vec<_>>(),
    });
    let broken: Vec<String> = states
        .iter()
        .filter_map(|(path, state)| match state {
            BlockState::Broken { line } => Some(format!(
                "{path}: the hostler markers are broken at line {line}"
            )),
            BlockState::Unreadable(why) => Some(format!("{path}: {why}")),
            BlockState::Refused(why) => Some(why.clone()),
            BlockState::Symlink => Some(format!(
                "{path} is a symlink and its block is not current; doctor never writes through \
                 one"
            )),
            _ => None,
        })
        .collect();
    if !broken.is_empty() {
        return Found::new(ID, CheckStatus::Problem, broken.join("; "))
            .details(details)
            .fix(agent(
                None,
                "fix it by hand (one <!-- hostler:begin --> … <!-- hostler:end --> pair per \
                 file; a read-only file made writable); doctor never writes through a symlink or \
                 outside the checkout",
            ));
    }
    let stale: Vec<String> = states
        .iter()
        .filter_map(|(path, state)| match state {
            BlockState::NoFile | BlockState::Missing => Some(format!("missing from {path}")),
            BlockState::Differs => Some(format!("out of date in {path}")),
            _ => None,
        })
        .collect();
    if stale.is_empty() {
        let files: Vec<&str> = states.iter().map(|(path, _)| path.as_str()).collect();
        return Found::ok(
            ID,
            format!("the hostler block is current in {}", files.join(", ")),
        )
        .details(details);
    }
    Found::new(
        ID,
        CheckStatus::Warn,
        format!("the hostler block is {}", stale.join(", ")),
    )
    .details(details)
    .safe_fix(
        "hostler init --write",
        "rewrites the hostler block between its markers (doctor --fix writes only the block, \
         never hostler.toml)",
        FixAction::DocsBlock,
    )
}

fn block_state_name(state: &BlockState) -> &'static str {
    match state {
        BlockState::NoFile => "no_file",
        BlockState::Missing => "missing",
        BlockState::Current => "current",
        BlockState::Differs => "differs",
        BlockState::Broken { .. } => "broken",
        BlockState::Unreadable(_) => "unreadable",
        BlockState::Refused(_) => "refused",
        BlockState::Covered(_) => "covered",
        BlockState::Symlink => "symlink",
    }
}

fn project_device_rules(ctx: &Ctx) -> Found {
    const ID: &str = "project.device_rules";
    let Some(project) = &ctx.project else {
        return Found::skipped(ID, NOT_IN_CHECKOUT);
    };
    let Some(config) = project.valid() else {
        return Found::skipped(
            ID,
            "hostler.toml is missing or invalid (see project.config)",
        );
    };
    let Some(rules) = &config.agents.device_rules else {
        return Found::new(
            ID,
            CheckStatus::Warn,
            "this project has no device-handling instructions yet",
        )
        .fix(agent(
            None,
            "propose them from the project's own context, show them to the developer, and only \
             after they agree write them under a heading in the agent docs; then set [agents] \
             device_rules = \"<file>#<heading>\" (the init.device_rules step of: hostler init)",
        ));
    };
    let mut details = json!({ "path": rules.path, "heading": rules.heading });
    let fix = || {
        agent(
            None,
            "write the rules under that heading, or correct [agents] device_rules in hostler.toml",
        )
    };
    let text = match files::read_text(project.root(), &rules.path) {
        Ok(text) => text,
        Err(why) => {
            details["reason"] = json!("file");
            return Found::new(
                ID,
                CheckStatus::Problem,
                format!("device_rules: {}: {why}", rules.path),
            )
            .details(details)
            .fix(fix());
        }
    };
    let Some(heading) = &rules.heading else {
        return Found::ok(ID, format!("the device rules are in {}", rules.path)).details(details);
    };
    let found = text.lines().any(|line| {
        line.trim_start().starts_with('#')
            && line.trim_start().trim_start_matches('#').trim() == heading.trim()
    });
    if found {
        Found::ok(
            ID,
            format!("the device rules are in {}#{heading}", rules.path),
        )
        .details(details)
    } else {
        details["reason"] = json!("heading");
        Found::new(
            ID,
            CheckStatus::Problem,
            format!("device_rules: {} has no heading \"{heading}\"", rules.path),
        )
        .details(details)
        .fix(fix())
    }
}

/// Why the per-name secret and account checks cannot ask the service, if they cannot.
fn vault_gate(ctx: &Ctx) -> Option<String> {
    if ctx.client.is_none() {
        return Some(ctx.no_service());
    }
    if !ctx.project_info.as_ref().is_some_and(|info| info.approved) {
        return Some("the project is not approved (see project.approved)".into());
    }
    if ctx.lock != Some(LockState::Unlocked) {
        return Some("the secrets database is not unlocked (see secrets.database)".into());
    }
    None
}

/// A purpose as one shell word for a printed command.
fn shell_word(text: &str) -> String {
    let text = crate::secrets::sanitize_for_terminal(text);
    if cfg!(windows) {
        format!("\"{}\"", text.replace('"', "'"))
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

async fn secrets_required(ctx: &mut Ctx<'_>) -> Vec<Found> {
    let Some((declared, project)) = ctx.project.as_ref().and_then(|p| {
        p.valid()
            .map(|config| (config.declared(), p.project.clone()))
    }) else {
        return Vec::new();
    };
    if declared.is_empty() {
        return Vec::new();
    }
    let id = |name: &str| format!("secrets.required.{name}");
    if let Some(why) = vault_gate(ctx) {
        return declared
            .iter()
            .map(|d| Found::skipped(id(&d.name), why.clone()))
            .collect();
    }
    let request = Request::SecretList {
        project,
        declared: declared.clone(),
    };
    let list = match ctx.call::<SecretList>(request).await {
        Ok(list) => list,
        Err(why) => {
            return declared
                .iter()
                .map(|d| Found::skipped(id(&d.name), why.clone()))
                .collect();
        }
    };
    declared
        .iter()
        .map(|decl| {
            let state = list
                .secrets
                .iter()
                .find(|info| info.name == decl.name)
                .map(|info| info.state);
            match state {
                Some(SecretState::Set) => {
                    Found::ok(id(&decl.name), format!("{} is set", decl.name))
                }
                _ => Found::new(
                    id(&decl.name),
                    CheckStatus::Problem,
                    format!("{} is declared but has no value", decl.name),
                )
                // R1-4: no env file is read or named; the person's route is in the details.
                .details(json!({
                    "person_fix": {
                        "command": format!("hostler secret set {}", decl.name),
                        "text": "or: hostler secret import <file>; if you keep it in an env \
                                 file, import that file",
                        "who": "person",
                    },
                }))
                .fix(agent(
                    Some(&format!(
                        "hostler secret request {} --reason {}",
                        decl.name,
                        shell_word(&decl.purpose)
                    )),
                    format!(
                        "asks a person for it; or a person runs: hostler secret set {} (or \
                         hostler secret import <file>; if you keep it in an env file, import \
                         that file)",
                        decl.name
                    ),
                )),
            }
        })
        .collect()
}

async fn accounts_required(ctx: &mut Ctx<'_>) -> Vec<Found> {
    let Some((accounts, project)) = ctx.project.as_ref().and_then(|p| {
        p.valid().map(|config| {
            let accounts: Vec<(String, Vec<String>)> = config
                .accounts
                .iter()
                .map(|(name, decl)| (name.clone(), decl.fields.clone()))
                .collect();
            (accounts, p.project.clone())
        })
    }) else {
        return Vec::new();
    };
    if accounts.is_empty() {
        return Vec::new();
    }
    let id = |name: &str| format!("accounts.required.{name}");
    if let Some(why) = vault_gate(ctx) {
        return accounts
            .iter()
            .map(|(name, _)| Found::skipped(id(name), why.clone()))
            .collect();
    }
    let listed = match ctx
        .call::<Vec<AccountInfo>>(Request::AccountList { project })
        .await
    {
        Ok(listed) => listed,
        Err(why) => {
            return accounts
                .iter()
                .map(|(name, _)| Found::skipped(id(name), why.clone()))
                .collect();
        }
    };
    accounts
        .iter()
        .map(|(name, fields)| {
            let set_fix = |field: &str| {
                person(
                    Some(&format!("hostler account set {name} --field {field}")),
                    "a person sets each field in a terminal",
                )
            };
            match listed.iter().find(|info| &info.name == name) {
                None => Found::new(
                    id(name),
                    CheckStatus::Problem,
                    format!("account {name} is declared but not set"),
                )
                .details(json!({ "fields": fields }))
                .fix(set_fix(fields.first().map_or("<field>", String::as_str))),
                Some(info) => {
                    let missing: Vec<&String> =
                        fields.iter().filter(|f| !info.fields.contains(f)).collect();
                    match missing.first() {
                        None => Found::ok(id(name), format!("account {name} is set")),
                        Some(first) => Found::new(
                            id(name),
                            CheckStatus::Warn,
                            format!(
                                "account {name} lacks the declared field(s): {}",
                                missing
                                    .iter()
                                    .map(|f| f.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                        .details(json!({ "missing": missing }))
                        .fix(set_fix(first)),
                    }
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Tools, claims, the workspace

const TOOL_IDS: [&str; 5] = [
    "tools.adb",
    "tools.emulator",
    "tools.xcrun",
    "tools.ideviceinfo",
    "tools.agent_device",
];

async fn tools(ctx: &mut Ctx<'_>) -> Vec<Found> {
    let snapshot = match ctx
        .call::<DevicesSnapshot>(Request::DevicesList { all: false })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(why) => {
            return TOOL_IDS
                .iter()
                .map(|id| Found::skipped(*id, why.clone()))
                .collect();
        }
    };
    let config = ctx.project.as_ref().and_then(ProjectFacts::valid);
    let android = config.is_some_and(|c| !c.apps.android.is_empty());
    let ios = config.is_some_and(|c| !c.apps.ios.is_empty());
    tool_checks(&snapshot.tools, android, ios, cfg!(target_os = "macos"))
}

fn detect() -> Fix {
    agent(
        Some("hostler devices tools --detect"),
        "records where the tools are, from this shell's PATH (install them first)",
    )
}

/// `tools.*` from `devices.list`'s `ToolStatus` (pure).
fn tool_checks(tools: &ToolStatus, android: bool, ios: bool, macos: bool) -> Vec<Found> {
    let needed = |wanted: bool| {
        if wanted {
            CheckStatus::Problem
        } else {
            CheckStatus::Warn
        }
    };
    let mut out = Vec::new();
    out.push(match (&tools.adb, &tools.adb_state) {
        (Some(path), AdbState::Ours) => Found::ok("tools.adb", format!("adb: {path}")),
        (_, AdbState::Foreign { version }) => Found::new(
            "tools.adb",
            needed(android),
            format!(
                "another adb server (protocol {version}) runs; hostler does not manage it, so \
                 Android devices are not seen"
            ),
        )
        .details(json!({ "adb_state": "foreign", "version": version }))
        .fix(agent(
            Some("hostler devices tools --detect"),
            "stop that adb server with the adb that started it (adb kill-server), or point \
             hostler at the same adb",
        )),
        _ => Found::new("tools.adb", needed(android), "adb was not found")
            .details(json!({ "adb_state": "missing" }))
            .fix(detect()),
    });
    out.push(match &tools.emulator {
        Some(path) => Found::ok("tools.emulator", format!("emulator: {path}")),
        None if android => Found::new(
            "tools.emulator",
            CheckStatus::Warn,
            "the Android emulator was not found",
        )
        .fix(detect()),
        None => Found::skipped("tools.emulator", "no Android apps declared"),
    });
    out.push(if !macos {
        Found::skipped("tools.xcrun", "not macOS")
    } else if tools.xcrun {
        Found::ok("tools.xcrun", "xcrun (Xcode) is installed")
    } else if ios {
        Found::new(
            "tools.xcrun",
            CheckStatus::Problem,
            "xcrun (Xcode) was not found: iOS simulators are not seen",
        )
        .fix(person(
            Some("xcode-select --install"),
            "installs Xcode's command-line tools",
        ))
    } else {
        Found::skipped("tools.xcrun", "no iOS apps declared")
    });
    out.push(match &tools.ideviceinfo {
        Some(path) => Found::ok("tools.ideviceinfo", format!("ideviceinfo: {path}")),
        None if ios => Found::new(
            "tools.ideviceinfo",
            CheckStatus::Warn,
            "ideviceinfo (libimobiledevice) was not found: physical iPhones are not seen",
        )
        .fix(detect()),
        None => Found::skipped("tools.ideviceinfo", "no iOS apps declared"),
    });
    out.push(match &tools.agent_device {
        None => Found::skipped(
            "tools.agent_device",
            "agent-device is not installed (optional)",
        ),
        Some(version) => {
            let target = crate::devices::agent_device::TARGET_VERSION;
            let parsed = parse_triple(version);
            if parsed.is_some_and(|v| v < target) {
                Found::new(
                    "tools.agent_device",
                    CheckStatus::Warn,
                    format!(
                        "agent-device {version} is older than {}.{}.{}, which hostler targets",
                        target.0, target.1, target.2
                    ),
                )
                .fix(agent(
                    None,
                    format!(
                        "upgrade agent-device to {}.{}.{} or later",
                        target.0, target.1, target.2
                    ),
                ))
            } else {
                Found::ok("tools.agent_device", format!("agent-device {version}"))
            }
        }
    });
    for (index, problem) in tools.problems.iter().enumerate() {
        out.push(
            Found::new(
                format!("tools.problem.{}", index + 1),
                CheckStatus::Warn,
                problem.clone(),
            )
            .fix(agent(
                None,
                "do what the message says, then: hostler devices tools",
            )),
        );
    }
    out
}

fn parse_triple(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim().split(['.', '-', '+']);
    let mut next = || parts.next()?.parse::<u32>().ok();
    Some((next()?, next()?, next()?))
}

fn stale_after() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("HOSTLER_TEST_DOCTOR_STALE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    STALE_HOLD
}

/// A claim `claims.workspace` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StaleClaim {
    pub id: String,
    pub resource: String,
    pub kind: &'static str,
    pub age_ms: i64,
}

/// This workspace's TTL holds older than `limit`, and reservations held by this project's
/// workspaces whose path is gone (pure; `exists` stands in for the file system).
pub(crate) fn stale_claims(
    snapshot: &ClaimsSnapshot,
    this_path: &str,
    project_workspaces: &[String],
    now_ms: i64,
    limit: Duration,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<StaleClaim> {
    let limit_ms = i64::try_from(limit.as_millis()).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    for state in &snapshot.resources {
        for claim in &state.claims {
            let age_ms = now_ms.saturating_sub(claim.granted_at_ms);
            let stale_hold = claim.kind == ClaimKind::Hold
                && matches!(claim.binding, Some(HoldBinding::Ttl { .. }))
                && same_path_str(&claim.workspace.path, this_path)
                && age_ms > limit_ms;
            let orphan_reservation = claim.kind == ClaimKind::Reservation
                && project_workspaces.contains(&claim.workspace.id)
                && !exists(&claim.workspace.path);
            if stale_hold || orphan_reservation {
                out.push(StaleClaim {
                    id: claim.id.clone(),
                    resource: claim.resource.display.clone(),
                    kind: if stale_hold { "hold" } else { "reservation" },
                    age_ms,
                });
            }
        }
    }
    out
}

fn same_path_str(a: &str, b: &str) -> bool {
    a == b || same_file(Path::new(a), Path::new(b))
}

fn age_text(ms: i64) -> String {
    let minutes = ms / 60_000;
    if minutes >= 120 {
        format!("{} h", minutes / 60)
    } else {
        format!("{minutes} min")
    }
}

async fn workspace_checks(ctx: &mut Ctx<'_>) -> Vec<Found> {
    const CLAIMS: &str = "claims.workspace";
    const REGISTERED: &str = "workspace.registered";
    let Some(root) = ctx
        .project
        .as_ref()
        .map(|p| p.checkout.workspace_root.clone())
    else {
        return vec![
            Found::skipped(CLAIMS, NOT_IN_CHECKOUT),
            Found::skipped(REGISTERED, NOT_IN_CHECKOUT),
        ];
    };
    let this_path = display_path(&root);
    let workspaces = match ctx.call::<Vec<WorkspaceInfo>>(Request::WorkspaceList).await {
        Ok(workspaces) => workspaces,
        Err(why) => {
            return vec![
                Found::skipped(CLAIMS, why.clone()),
                Found::skipped(REGISTERED, why),
            ];
        }
    };
    let this = workspaces
        .iter()
        .find(|ws| same_path_str(&ws.path, &this_path));
    let project_id = ctx
        .project_info
        .as_ref()
        .map(|info| info.id.clone())
        .or_else(|| this.map(|ws| ws.project_id.clone()));
    let project_workspaces: Vec<String> = workspaces
        .iter()
        .filter(|ws| Some(&ws.project_id) == project_id.as_ref())
        .map(|ws| ws.id.clone())
        .collect();
    let registered = match this {
        Some(ws) => Found::ok(REGISTERED, "this checkout is a registered workspace")
            .details(json!({ "workspace_id": ws.id })),
        None => Found::new(
            REGISTERED,
            CheckStatus::Warn,
            "this checkout is not registered yet (it is registered on its first claim)",
        ),
    };
    let claims = match ctx.call::<ClaimsSnapshot>(Request::ClaimsSnapshot).await {
        Err(why) => Found::skipped(CLAIMS, why),
        Ok(snapshot) => {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            let stale = stale_claims(
                &snapshot,
                &this_path,
                &project_workspaces,
                now_ms,
                stale_after(),
                &|path| Path::new(path).exists(),
            );
            if stale.is_empty() {
                Found::ok(CLAIMS, "no stale claims for this workspace")
            } else {
                let listed: Vec<String> = stale
                    .iter()
                    .map(|c| {
                        format!(
                            "{} ({} {}, {})",
                            c.id,
                            c.kind,
                            c.resource,
                            age_text(c.age_ms)
                        )
                    })
                    .collect();
                let command = match stale.as_slice() {
                    [one] => format!("hostler release {}", one.id),
                    _ => "hostler release <id>".into(),
                };
                Found::new(
                    CLAIMS,
                    CheckStatus::Warn,
                    format!("stale claims: {}", listed.join("; ")),
                )
                .details(json!({
                    "claims": stale
                        .iter()
                        .map(|c| json!({
                            "id": c.id,
                            "resource": c.resource,
                            "kind": c.kind,
                            "age_ms": c.age_ms,
                        }))
                        .collect::<Vec<_>>(),
                }))
                .fix(agent(
                    Some(&command),
                    "release each one only when no live agent of this workspace still uses it",
                ))
            }
        }
    };
    vec![claims, registered]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::contract::claims::WorkspaceRef;
    use crate::contract::claims::{Claim, ResourceId, ResourceInfo, ResourceKind, ResourceState};

    fn claim(id: &str, kind: ClaimKind, ws: (&str, &str), granted: i64, ttl: bool) -> Claim {
        Claim {
            id: id.into(),
            resource: ResourceInfo {
                id: ResourceId("screen".into()),
                kind: ResourceKind::Screen,
                display: "screen".into(),
                attributes: BTreeMap::new(),
                available: true,
            },
            kind,
            parent: None,
            workspace: WorkspaceRef {
                id: ws.0.into(),
                path: ws.1.into(),
            },
            holder: None,
            child: None,
            group: None,
            binding: ttl.then_some(HoldBinding::Ttl {
                ttl_ms: 1,
                expires_at_ms: 0,
            }),
            command: None,
            granted_at_ms: granted,
        }
    }

    #[test]
    fn stale_claims_are_old_ttl_holds_here_and_orphaned_reservations() {
        let hour = 3_600_000;
        let now = 10 * hour;
        let snapshot = ClaimsSnapshot {
            resources: vec![ResourceState {
                resource: claim("x", ClaimKind::Hold, ("w", "/a"), 0, true).resource,
                claims: vec![
                    claim(
                        "old-hold",
                        ClaimKind::Hold,
                        ("w1", "/here"),
                        now - 3 * hour,
                        true,
                    ),
                    claim(
                        "new-hold",
                        ClaimKind::Hold,
                        ("w1", "/here"),
                        now - hour,
                        true,
                    ),
                    claim("bound-hold", ClaimKind::Hold, ("w1", "/here"), 0, false),
                    claim("elsewhere", ClaimKind::Hold, ("w9", "/other"), 0, true),
                    claim(
                        "gone-res",
                        ClaimKind::Reservation,
                        ("w2", "/gone"),
                        0,
                        false,
                    ),
                    claim(
                        "live-res",
                        ClaimKind::Reservation,
                        ("w1", "/here"),
                        0,
                        false,
                    ),
                    claim(
                        "foreign-res",
                        ClaimKind::Reservation,
                        ("w9", "/gone"),
                        0,
                        false,
                    ),
                ],
                foreign: vec![],
                waiting: vec![],
                seizing: false,
                seizing_by: None,
            }],
        };
        let ids: Vec<String> = stale_claims(
            &snapshot,
            "/here",
            &["w1".into(), "w2".into()],
            now,
            STALE_HOLD,
            &|path| path != "/gone",
        )
        .into_iter()
        .map(|c| c.id)
        .collect();
        assert_eq!(ids, ["old-hold", "gone-res"]);
    }

    #[test]
    fn tool_checks_follow_the_declared_platforms() {
        let tools = ToolStatus {
            adb: None,
            adb_state: AdbState::Missing,
            emulator: None,
            xcrun: false,
            ideviceinfo: None,
            agent_device: Some("0.20.1".into()),
            node: None,
            agent_device_in_use: false,
            sessions_known: false,
            sessions_last_good_ms: None,
            sessions_reason: None,
            problems: vec!["something to look at".into()],
        };
        let statuses = |android, ios, macos| -> Vec<(String, CheckStatus)> {
            tool_checks(&tools, android, ios, macos)
                .into_iter()
                .map(|f| (f.check.id, f.check.status))
                .collect()
        };
        use CheckStatus::*;
        assert_eq!(
            statuses(true, true, true),
            [
                ("tools.adb".to_string(), Problem),
                ("tools.emulator".into(), Warn),
                ("tools.xcrun".into(), Problem),
                ("tools.ideviceinfo".into(), Warn),
                ("tools.agent_device".into(), Warn),
                ("tools.problem.1".into(), Warn),
            ]
        );
        assert_eq!(
            statuses(false, false, false),
            [
                ("tools.adb".to_string(), Warn),
                ("tools.emulator".into(), Skipped),
                ("tools.xcrun".into(), Skipped),
                ("tools.ideviceinfo".into(), Skipped),
                ("tools.agent_device".into(), Warn),
                ("tools.problem.1".into(), Warn),
            ]
        );
        let foreign = ToolStatus {
            adb: Some("/sdk/adb".into()),
            adb_state: AdbState::Foreign { version: 40 },
            ..tools.clone()
        };
        let adb = &tool_checks(&foreign, true, false, false)[0];
        assert_eq!(adb.check.status, Problem);
        assert!(adb.check.message.contains("another adb server"));
        assert!(
            tool_checks(&foreign, true, false, false)
                .iter()
                .all(|f| f.action.is_none())
        );
    }

    #[test]
    fn shell_words_keep_a_purpose_in_one_word() {
        if cfg!(windows) {
            assert_eq!(shell_word("it's \"x\""), "\"it's 'x'\"");
        } else {
            assert_eq!(shell_word("it's x"), r"'it'\''s x'");
        }
    }

    /// W-6: the decision behind `service.linux_session`, on every OS (the probe is Linux's).
    #[test]
    fn linux_session_rules() {
        let session = |kill: Option<bool>, linger: Option<bool>| logind::Session {
            kill_user_processes: kill,
            linger,
            user: "alice".into(),
        };
        let locked_idle = Probe::Running {
            contract: CONTRACT_VERSION,
            version: lifecycle::SERVICE_VERSION.into(),
            secrets_unlocked: Some(false),
            pid: 1,
        };
        let found =
            |reg, probe: &Probe, s: &logind::Session| linux_session_found(reg, "x", probe, s);
        use CheckStatus::*;

        // Killed with the session and not registered: a problem, fixed by the install.
        let f = found(
            Some(RegistrationState::Absent),
            &Probe::Stopped,
            &session(Some(true), None),
        );
        assert_eq!(f.check.status, Problem);
        assert_eq!(f.action, Some(FixAction::Install(AutoPurpose::Maintain)));
        let f = found(
            Some(RegistrationState::Absent),
            &locked_idle,
            &session(Some(true), None),
        );
        assert_eq!(f.action, Some(FixAction::Install(AutoPurpose::Maintain)));
        // Unlocked: not safe.
        let unlocked = Probe::Running {
            contract: CONTRACT_VERSION,
            version: lifecycle::SERVICE_VERSION.into(),
            secrets_unlocked: Some(true),
            pid: 1,
        };
        let f = found(
            Some(RegistrationState::Absent),
            &unlocked,
            &session(Some(true), None),
        );
        assert_eq!(f.check.status, Problem);
        assert_eq!(f.action, None);
        assert!(!f.check.fix.as_ref().unwrap().safe);
        // Registered without linger: a warning with the person's command.
        let f = found(
            Some(RegistrationState::Current),
            &Probe::Stopped,
            &session(Some(true), Some(false)),
        );
        assert_eq!(f.check.status, Warn);
        let fix = f.check.fix.unwrap();
        assert_eq!(fix.who, Who::Person);
        assert_eq!(fix.command.as_deref(), Some("loginctl enable-linger alice"));
        assert_eq!(
            found(
                Some(RegistrationState::Current),
                &Probe::Stopped,
                &session(None, Some(true))
            )
            .check
            .status,
            Ok
        );
        // A tool missing: skipped.
        assert_eq!(
            found(
                Some(RegistrationState::Current),
                &Probe::Stopped,
                &session(Some(true), None)
            )
            .check
            .status,
            Skipped
        );
        assert_eq!(
            found(
                Some(RegistrationState::Absent),
                &Probe::Stopped,
                &session(None, None)
            )
            .check
            .status,
            Skipped
        );
        assert_eq!(
            found(None, &Probe::Stopped, &session(Some(true), Some(false)))
                .check
                .status,
            Skipped
        );
        // R1-2: an unreadable registration is never "absent": a warning, nothing to run.
        for kill in [Some(true), Some(false), None] {
            let f = linux_session_found(
                Some(RegistrationState::Unknown),
                "the OS service manager did not answer",
                &Probe::Stopped,
                &session(kill, Some(false)),
            );
            assert_eq!(f.check.status, Warn, "{kill:?}");
            assert_eq!(
                f.check.message,
                "cannot read the registration: the OS service manager did not answer"
            );
            assert!(f.check.fix.is_none());
            assert_eq!(f.action, None);
        }
    }

    #[test]
    fn versions_parse_as_triples() {
        assert_eq!(parse_triple("0.21.36"), Some((0, 21, 36)));
        assert_eq!(parse_triple("1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(parse_triple("dev"), None);
    }
}
