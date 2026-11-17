//! `hostler run --screen` / `--account`: claims around a run (plan P3a, Task 8).
//!
//! The order: the workspace; P2's secrets; `account.check`; the claims in ascending resource id
//! order, each through `claim_wait::acquire_waiting` (or a recorded takeover with `--force`);
//! `account.materialize`; the masked command for every claim; P1's heavy slot; the command behind
//! its gate in a registered group/job; `claims.started` for every claim before the gate opens;
//! the release; one summary line per resource. Claims fail closed (78): without the service
//! nothing runs. One `Stop` (deadline and signals) lasts the whole run (L34).
//!
//! Account field values reach only the command's environment and the run's mask; the service
//! sees the command masked with the secret values (`account.materialize`) and then with all of
//! them (`claims.set_command`); until then a claim carries `<command pending>`.
//!
//! Debug-only test hook read here: `HOSTLER_TEST_KILL_AFTER_CLAIM_GRANT=1` (the wrapper dies
//! once its claims are granted, before anything else).

use std::ffi::OsString;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::time::Instant as TokioInstant;

use super::claim_wait::{self, Acquired, Deadline, Gave, Stop, WaitText};
use super::output::print_error;
use super::run::{self, EXIT_CANNOT_RUN, EXIT_QUEUE, Heavy, RunArgs};
use super::secret_run::{self, ChildIo, EXIT_REFUSED, SECRET_RUN_ENV};
use super::spawn::{self, Group};
use super::{wait, workspace};
use crate::client::Client;
use crate::contract::accounts::{AccountMaterialize, MaterializedAccount};
use std::collections::BTreeMap;

use crate::contract::claims::{
    Blocker, Claim, ClaimKind, ClaimOutcome, ClaimRelease, ClaimRequest, ClaimStarted,
    ClaimsSnapshot, HoldBinding, Purpose, ReleaseOutcome, ResourceId, ResourceKind, Selector,
    WorkspaceInfo,
};
use crate::contract::devices::{DeviceDetail, DeviceInfo, DeviceKind, Platform};
use crate::contract::heavy::{AcquireOutcome, HeavyStarted};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::mask::MaskSet;
use crate::paths::Paths;
use crate::procs::{ProcessIdentity, current_identity};
use crate::project;
use crate::secrets::sanitize_for_terminal;
use crate::secrets::value::SecretValue;
use crate::service::manager::OsManager;

/// Comma-separated live lease ids of the runs around a command (P1's heavy-token rule).
pub const CLAIM_TOKENS_ENV: &str = "HOSTLER_CLAIM_TOKENS";
/// What a claim's command is until the account's fields are known and masked (P2 L13).
pub const COMMAND_PENDING: &str = "<command pending>";
/// A claim's command, after masking (the service's bound).
const MAX_CLAIM_COMMAND: usize = crate::claims::engine::MAX_COMMAND_BYTES;
/// Reconnect-and-retry for `started`, `set_command` and the release.
const RETRY_BUDGET: Duration = Duration::from_secs(10);
const RETRY_PAUSE: Duration = Duration::from_millis(200);
const EXIT_USAGE: i32 = 2;

/// A claim this run took: its own (`joined` = linked to its workspace's claim).
#[derive(Clone, Debug)]
pub struct Held {
    pub claim: Claim,
    pub joined: bool,
    /// The kind of the claim joined, as it was at the join.
    pub parent_kind: Option<ClaimKind>,
}

/// What a resource is left in after the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    Released,
    /// A joined run whose hold still lives.
    StillHeld {
        hold_id: String,
        until: Option<i64>,
    },
    /// A joined run whose hold ended while the command ran (R2-11).
    HoldEnded {
        hold_id: String,
    },
    /// A joined run whose workspace's other run still uses the resource.
    SharedWith {
        lease_id: String,
    },
    /// A joined run under this workspace's reservation (`hostler devices reserve`).
    StillReserved,
    TakenOver {
        by: String,
    },
    /// The service did not answer; the claim ends when this process is gone.
    NotReleased,
    /// A re-entrant claim: the enclosing run holds the resource and releases it.
    Enclosing,
}

/// The run's environment for a claim (P-15, D4). Screen and account: nothing (account fields
/// come from `account.materialize`). A device: its variables from `device`, resolved right before
/// the spawn, so `ANDROID_SERIAL` is the transport at that moment.
pub fn claim_env(claim: &Claim, device: Option<&DeviceInfo>) -> Vec<(String, String)> {
    let Some(d) = device.filter(|_| claim.resource.kind == ResourceKind::Device) else {
        return Vec::new();
    };
    let platform = match d.platform {
        Platform::Android => "android",
        Platform::Ios => "ios",
    };
    let mut env = vec![
        ("HOSTLER_DEVICE_ID".to_owned(), d.hardware_id.clone()),
        (
            "HOSTLER_DEVICE_ALIAS".to_owned(),
            d.alias
                .clone()
                .unwrap_or_else(|| claim.resource.display.clone()),
        ),
        ("HOSTLER_DEVICE_PLATFORM".to_owned(), platform.to_owned()),
        ("HOSTLER_LEASE_ID".to_owned(), claim.id.clone()),
    ];
    let agent_device_id = match d.platform {
        Platform::Android => {
            let serial = d
                .transports
                .first()
                .filter(|_| d.connected)
                .map(|t| t.address.clone());
            if let Some(serial) = &serial {
                env.push(("ANDROID_SERIAL".to_owned(), serial.clone()));
            }
            serial
        }
        Platform::Ios => {
            env.push(("HOSTLER_DEVICE_UDID".to_owned(), d.hardware_id.clone()));
            Some(d.hardware_id.clone())
        }
    };
    if let Some(id) = agent_device_id {
        env.push(("HOSTLER_AGENT_DEVICE_ID".to_owned(), id));
    }
    env
}

/// `--device`: a name (alias, id, transport), or the first free device of a platform and kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceArg {
    Named(String),
    Auto {
        platform: Option<Platform>,
        kind: Option<DeviceKind>,
    },
}

const DEVICE_ARG: &str = "--device takes an alias or id, or auto, auto:<android|ios>, \
                          auto:android:<physical|emulator>, auto:ios:<physical|simulator>";

pub fn parse_device_arg(s: &str) -> Result<DeviceArg, String> {
    let Some(rest) = s.strip_prefix("auto") else {
        return if s.is_empty() || s.starts_with('-') {
            Err(DEVICE_ARG.into())
        } else {
            Ok(DeviceArg::Named(s.to_owned()))
        };
    };
    let mut parts = match rest {
        "" => Vec::new(),
        _ => match rest.strip_prefix(':') {
            Some(p) => p.split(':').collect(),
            // `autobahn`: a name.
            None => return Ok(DeviceArg::Named(s.to_owned())),
        },
    };
    if parts.len() > 2 {
        return Err(DEVICE_ARG.into());
    }
    let kind = match parts.get(1).copied() {
        None => None,
        Some("physical") => Some(DeviceKind::Physical),
        Some("emulator") => Some(DeviceKind::Emulator),
        Some("simulator") => Some(DeviceKind::Simulator),
        Some(_) => return Err(DEVICE_ARG.into()),
    };
    parts.truncate(1);
    let platform = match parts.first().copied() {
        None => None,
        Some("android") => Some(Platform::Android),
        Some("ios") => Some(Platform::Ios),
        Some(_) => return Err(DEVICE_ARG.into()),
    };
    match (platform, kind) {
        (Some(Platform::Android), Some(DeviceKind::Simulator))
        | (Some(Platform::Ios), Some(DeviceKind::Emulator)) => Err(DEVICE_ARG.into()),
        _ => Ok(DeviceArg::Auto { platform, kind }),
    }
}

/// The `Any` selector of `auto`: its platform and kind, and `ready: true` unless `--boot`.
pub fn auto_selector(platform: Option<Platform>, kind: Option<DeviceKind>, boot: bool) -> Selector {
    let mut attributes = BTreeMap::new();
    if let Some(p) = platform {
        let p = match p {
            Platform::Android => "android",
            Platform::Ios => "ios",
        };
        attributes.insert("platform".to_owned(), p.to_owned());
    }
    if let Some(k) = kind {
        let k = match k {
            DeviceKind::Physical => "physical",
            DeviceKind::Emulator => "emulator",
            DeviceKind::Simulator => "simulator",
        };
        attributes.insert("kind".to_owned(), k.to_owned());
    }
    if !boot {
        attributes.insert("ready".to_owned(), "true".to_owned());
    }
    Selector::Any {
        kind: ResourceKind::Device,
        attributes,
    }
}

/// The selector and display of `--device`. A name must be a registered device
/// (`DeviceNotRegistered` with L49's recovery); its display is its alias, the `--confirmation`
/// token. `auto`'s display is the argument.
pub async fn device_selector(
    client: &mut Client,
    arg: &DeviceArg,
    boot: bool,
) -> Result<(Selector, String), HostlerError> {
    match arg {
        DeviceArg::Named(name) => {
            let info: DeviceInfo = decode(
                client
                    .call(Request::DevicesResolve {
                        device: name.clone(),
                    })
                    .await?,
            )?;
            let alias = info
                .alias
                .clone()
                .unwrap_or_else(|| info.resource.0.clone());
            Ok((
                Selector::Exact {
                    resource: info.resource,
                },
                alias,
            ))
        }
        DeviceArg::Auto { platform, kind } => {
            let mut shown = "auto".to_owned();
            for part in [
                platform.map(|p| {
                    if p == Platform::Android {
                        "android"
                    } else {
                        "ios"
                    }
                }),
                kind.map(|k| match k {
                    DeviceKind::Physical => "physical",
                    DeviceKind::Emulator => "emulator",
                    DeviceKind::Simulator => "simulator",
                }),
            ]
            .into_iter()
            .flatten()
            {
                shown.push(':');
                shown.push_str(part);
            }
            Ok((auto_selector(*platform, *kind, boot), shown))
        }
    }
}

/// L49, P2-15: the open-session lines at a claim's end (the service writes the same ones for an
/// expired or reaped hold).
pub use crate::devices::boot::session_warnings as open_session_warnings;

/// `devices.resolve` of a held device: its state as of now.
pub(crate) async fn resolve_device(
    client: &mut Client,
    r: &ResourceId,
) -> Result<DeviceInfo, HostlerError> {
    decode(
        client
            .call(Request::DevicesResolve {
                device: r.0.clone(),
            })
            .await?,
    )
}

/// Why a device claim did not come to a ready device.
pub(crate) enum NotReady {
    /// A signal during the boot: 130/143. The service finishes the boot; the caller releases.
    Interrupted(i32),
    Refused(HostlerError),
}

/// Step 3: a granted device that is not ready is booted with `--boot` (client bound 200 s),
/// else refused with `DeviceNotReady`. Answers the device as of now.
pub(crate) async fn ready_device(
    client: &mut Client,
    claim: &Claim,
    workspace_id: &str,
    boot: bool,
    stop: &mut Stop,
) -> Result<DeviceInfo, NotReady> {
    let info = resolve_device(client, &claim.resource.id)
        .await
        .map_err(NotReady::Refused)?;
    if info.ready {
        return Ok(info);
    }
    let alias = info
        .alias
        .clone()
        .unwrap_or_else(|| claim.resource.display.clone());
    if !boot {
        return Err(NotReady::Refused(
            crate::contract::devices::errors::device_not_ready(&alias),
        ));
    }
    eprintln!(
        "[hostler] Booting {} (headless); this can take a few minutes — Ctrl+C releases it.",
        sanitize_for_terminal(&alias)
    );
    let request = Request::DevicesBoot {
        resource: claim.resource.id.clone(),
        claim_id: claim.id.clone(),
        workspace_id: workspace_id.to_owned(),
    };
    // Its own connection: the service answers one connection's requests in order, so the
    // release after a Ctrl+C must not queue behind the boot (P3bT7-2).
    // P4 R2-2: a denied state or socket directory is `sandbox_denied`.
    let paths = crate::client::resolve_paths().map_err(NotReady::Refused)?;
    let mut booting = Client::connect(&paths).await.map_err(NotReady::Refused)?;
    let booted = tokio::select! {
        biased;
        code = stop.interrupt() => {
            eprintln!(
                "[hostler] Interrupted; releasing {} (hostler finishes the boot and shuts it down \
                 when nobody claims it).",
                sanitize_for_terminal(&alias)
            );
            return Err(NotReady::Interrupted(code));
        }
        booted = booting.call(request) => booted,
    };
    booted.map_err(NotReady::Refused)?;
    resolve_device(client, &claim.resource.id)
        .await
        .map_err(NotReady::Refused)
}

/// How long a device without its transport at spawn is waited for (a USB → Wi-Fi switch).
const TRANSPORT_GRACE: Duration = Duration::from_secs(5);

/// Whether the command can be given the device now: connected and booted, and an Android
/// device with an adb transport, so `ANDROID_SERIAL` is never missing (P3bT7-3).
pub fn usable_at_spawn(d: &DeviceInfo) -> bool {
    d.connected && d.ready && (d.platform == Platform::Ios || !d.transports.is_empty())
}

/// The device right before the spawn: waited for up to 5 s, then refused (78).
pub(crate) async fn device_at_spawn(
    client: &mut Client,
    claim: &Claim,
) -> Result<DeviceInfo, HostlerError> {
    let until = Instant::now() + TRANSPORT_GRACE;
    loop {
        let info = resolve_device(client, &claim.resource.id).await?;
        if usable_at_spawn(&info) {
            return Ok(info);
        }
        if Instant::now() >= until {
            let alias = info
                .alias
                .clone()
                .unwrap_or_else(|| claim.resource.display.clone());
            let mut e = crate::contract::devices::errors::device_not_ready(&alias);
            e.message = format!("{alias} is not connected now; the command was not run");
            e.recovery = Some(format!("hostler devices list (is {alias} attached?)"));
            return Err(e);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The names a device run never inherits (P3bT7-3): another device's serial or ids from the
/// caller's shell or an outer run. The fresh values are set after.
pub fn inherited_device_names(vars: impl Iterator<Item = OsString>) -> Vec<OsString> {
    vars.filter(|name| {
        name.to_str().is_some_and(|n| {
            n == "ANDROID_SERIAL"
                || n.starts_with("HOSTLER_DEVICE_")
                || n == "HOSTLER_AGENT_DEVICE_ID"
                || n == "HOSTLER_LEASE_ID"
        })
    })
    .collect()
}

/// `hold device --json` (P2-8): the claim's resource as the catalog would list `info` now, so a
/// booted emulator's `serial` and `agent_device_id` are its `emulator-N`.
pub fn merge_device(claim: &mut Claim, info: &DeviceInfo) {
    if let Some(entry) = crate::devices::catalog::entries(std::slice::from_ref(info))
        .into_iter()
        .next()
    {
        claim.resource.display = entry.display;
        claim.resource.attributes = entry.attributes;
    }
}

/// The end of a device claim (L49, P2-15, P3-14): the open-session warnings from a fresh read;
/// when no good read came in time, the sessions-unknown line first, then the last known
/// warnings (CRB-4); plus whether hostler booted it.
pub(crate) async fn device_end(
    client: &mut Client,
    claim: &Claim,
    workspace_id: &str,
) -> (Vec<String>, bool) {
    let shown = client
        .call(Request::DevicesShow {
            device: claim.resource.id.0.clone(),
            fresh_sessions: true,
        })
        .await;
    let (unknown, detail) = match shown {
        Ok(value) => {
            let unknown = value
                .get(crate::contract::devices::SESSIONS_UNKNOWN_KEY)
                .and_then(Value::as_str)
                .map(str::to_owned);
            (unknown, decode::<DeviceDetail>(value).ok())
        }
        Err(e) => (
            Some(format!("the service did not answer: {}", e.message)),
            None,
        ),
    };
    let now = crate::heavy::engine::Clock::now_ms(&crate::heavy::engine::SystemClock);
    let mut lines: Vec<String> = unknown
        .iter()
        .map(|reason| claim_wait::sessions_unknown_line(reason))
        .collect();
    let booted = detail.as_ref().is_some_and(|d| d.info.booted_by_hostler);
    if let Some(detail) = &detail {
        lines.extend(open_session_warnings(
            &detail.info,
            workspace_id,
            (claim.granted_at_ms, now),
        ));
    }
    (lines, booted)
}

/// `HOSTLER_ACCOUNT_NAME` and `HOSTLER_ACCOUNT_<FIELD>` (fields are `^[a-z][a-z0-9_]*$`).
pub fn account_env(account: &MaterializedAccount) -> Vec<(OsString, OsString)> {
    let mut env = vec![(
        OsString::from("HOSTLER_ACCOUNT_NAME"),
        OsString::from(&account.name),
    )];
    for field in &account.fields {
        env.push((
            OsString::from(format!(
                "HOSTLER_ACCOUNT_{}",
                field.name.to_ascii_uppercase()
            )),
            OsString::from(field.value.expose()),
        ));
    }
    env
}

/// The resources a run asks for, in the order it takes them (ascending id), with their displays.
pub fn resources(
    screen: bool,
    account: Option<&str>,
    project_id: &str,
) -> Vec<(ResourceId, String)> {
    let mut out = Vec::new();
    if screen {
        out.push((ResourceId::screen(), "screen".to_owned()));
    }
    if let Some(name) = account {
        out.push((
            ResourceId::account(project_id, name),
            format!("account {name}"),
        ));
    }
    out.sort();
    out
}

/// The tokens a run's command inherits: the enclosing runs' and this run's own claims, joined
/// ones included.
pub fn tokens(inherited: &[String], held: &[Held]) -> String {
    let mut all: Vec<&str> = inherited.iter().map(String::as_str).collect();
    for h in held {
        if !all.contains(&h.claim.id.as_str()) {
            all.push(&h.claim.id);
        }
    }
    all.join(",")
}

fn inherited_tokens() -> Vec<String> {
    std::env::var(CLAIM_TOKENS_ENV)
        .map(|v| {
            v.split(',')
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn summary_line(display: &str, end: &Ended) -> String {
    match end {
        Ended::Released => format!("[hostler] {display}: released."),
        Ended::StillHeld {
            hold_id,
            until: Some(until),
        } => format!(
            "[hostler] {display}: still held by your hold {hold_id} until {}; hostler release \
             {hold_id} frees it.",
            claim_wait::hh_mm(*until)
        ),
        Ended::StillHeld {
            hold_id,
            until: None,
        } => format!(
            "[hostler] {display}: still held by your hold {hold_id}; hostler release {hold_id} \
             frees it."
        ),
        Ended::HoldEnded { hold_id } => format!(
            "[hostler] {display}: released (your hold {hold_id} ended while the command ran)."
        ),
        Ended::SharedWith { lease_id } => {
            format!("[hostler] {display}: released; this workspace's run {lease_id} still uses it.")
        }
        Ended::StillReserved => {
            format!("[hostler] {display}: released; still reserved by this workspace.")
        }
        Ended::TakenOver { by } => format!(
            "[hostler] {display}: taken over by {} while the command ran.",
            sanitize_for_terminal(by)
        ),
        Ended::NotReleased => format!(
            "[hostler] {display}: the service did not answer; the claim ends when this process \
             is gone."
        ),
        Ended::Enclosing => format!("[hostler] {display}: held by the enclosing run."),
    }
}

/// A device's line at the end of a run (D4): free, or still reserved; else P3a's lines.
pub fn device_summary_line(alias: &str, end: &Ended) -> String {
    let alias = sanitize_for_terminal(alias);
    match end {
        Ended::Released => format!("[hostler] device {alias}: released; free."),
        Ended::StillReserved => format!(
            "[hostler] device {alias}: released; still reserved by this workspace (hostler \
             devices unreserve {alias} frees it)."
        ),
        other => summary_line(&format!("device {alias}"), other),
    }
}

/// `--confirmation` names one of the run's resources: `screen`, the account's name, the
/// device's alias (its catalog display, T1-3).
pub fn confirmation_names(confirmation: &str, tokens: &[String]) -> bool {
    tokens.iter().any(|t| t == confirmation)
}

/// A refusal under a takeover (L28, L33): at `claims.acquire`, `heavy.acquire`, `heavy.started`.
fn taken_over(e: &HostlerError) -> bool {
    e.code == ErrorCode::TakenOver
}

/// `claims.started`'s refusal of a claim taken over before its command started: a live plan
/// (L27, L33) or a commit that already ended it (or a reap).
fn ended_before_start(e: &HostlerError) -> bool {
    taken_over(e) || e.code == ErrorCode::ClaimNotHeld
}

/// The claims of one run and the connection they live on.
struct Claims {
    client: Client,
    paths: Paths,
    workspace: WorkspaceInfo,
    requester: ProcessIdentity,
    /// Every resource in order: its display, and its own claim (`None`: re-entrant).
    taken: Vec<(String, Option<Held>)>,
    /// The device claim's alias, when the run holds one (its end lines differ).
    device_alias: Option<String>,
    /// The end of the run (releases) has begun: a service that stops answering then gets one
    /// shared budget, `end_by`, for every call left (T8-3).
    ending: bool,
    end_by: Option<TokioInstant>,
}

impl Claims {
    fn held(&self) -> Vec<Held> {
        self.taken.iter().filter_map(|(_, h)| h.clone()).collect()
    }

    /// `request` until `RETRY_BUDGET`, reconnecting (never starting a service) when it is gone.
    /// At the end of the run the first failure starts one budget for all the calls left, and
    /// says so.
    async fn call(&mut self, request: Request) -> Result<Value, HostlerError> {
        let mut deadline = self
            .end_by
            .unwrap_or_else(|| TokioInstant::now() + RETRY_BUDGET);
        loop {
            match tokio::time::timeout_at(deadline, self.client.call(request.clone())).await {
                // Never retried (P4 R1-3).
                Ok(Err(e)) if crate::client::is_sandbox_denied(&e) => return Err(e),
                Ok(Err(e)) if e.code == ErrorCode::ServiceUnavailable => {
                    if self.ending && self.end_by.is_none() {
                        eprintln!(
                            "[hostler] The service is not answering; retrying the release for up \
                             to {}.",
                            wait::duration_text(RETRY_BUDGET)
                        );
                        deadline = TokioInstant::now() + RETRY_BUDGET;
                        self.end_by = Some(deadline);
                    }
                    if TokioInstant::now() + RETRY_PAUSE >= deadline {
                        return Err(e);
                    }
                }
                Ok(result) => return result,
                Err(_) => {
                    if self.ending {
                        self.end_by = Some(deadline);
                    }
                    return Err(no_answer());
                }
            }
            tokio::time::sleep(RETRY_PAUSE).await;
            match tokio::time::timeout_at(deadline, Client::connect(&self.paths)).await {
                Ok(Ok(client)) => self.client = client,
                // A reconnect the sandbox denies ends the retries at once (P4 R1-3).
                Ok(Err(e)) if crate::client::is_sandbox_denied(&e) => return Err(e),
                _ => {}
            }
        }
    }

    async fn live_claim(&mut self, id: &str) -> Option<Claim> {
        let snapshot: ClaimsSnapshot =
            decode(self.call(Request::ClaimsSnapshot).await.ok()?).ok()?;
        snapshot
            .resources
            .into_iter()
            .flat_map(|r| r.claims)
            .find(|c| c.id == id)
    }

    /// Ends every own claim and says what each resource is left in (D4); returns `code`.
    /// `told` is a claim whose fate a line already gave ("taken over before the command
    /// started"): its summary line is left out.
    async fn finish(&mut self, code: i32, exit_code: Option<i32>, told: Option<&str>) -> i32 {
        self.ending = true;
        let mut lines = Vec::new();
        for (display, held) in self.taken.clone() {
            let end = match &held {
                Some(h) => self.end(h, &display, exit_code).await,
                None => Ended::Enclosing,
            };
            let device = held
                .as_ref()
                .filter(|h| h.claim.resource.kind == ResourceKind::Device);
            // A reservation is no claim a lease joins: it stays beside it (D5).
            let end = match device {
                Some(h) if end == Ended::Released && self.reserved(&h.claim.resource.id).await => {
                    Ended::StillReserved
                }
                _ => end,
            };
            if held
                .as_ref()
                .is_none_or(|h| Some(h.claim.id.as_str()) != told)
            {
                lines.push(match (device, &self.device_alias) {
                    (Some(_), Some(alias)) => device_summary_line(alias, &end),
                    _ => summary_line(&display, &end),
                });
            }
            if let Some(h) = device
                && !matches!(end, Ended::NotReleased)
            {
                let workspace_id = self.workspace.id.clone();
                let (warnings, booted) =
                    device_end(&mut self.client, &h.claim, &workspace_id).await;
                if booted && end == Ended::Released {
                    lines.push(format!(
                        "[hostler] device {}: shut down in 2 min unless claimed (hostler booted \
                         it).",
                        sanitize_for_terminal(&h.claim.resource.display)
                    ));
                }
                lines.extend(warnings);
            }
        }
        for line in lines {
            eprintln!("{line}");
        }
        code
    }

    async fn end(&mut self, h: &Held, display: &str, exit_code: Option<i32>) -> Ended {
        // A takeover in progress (its stop may just have ended our command; the other targets
        // follow, up to 15 s each) is waited for until its plan ends, committed or not, so the
        // release answers who took it (L37) instead of ending the claim under it (T8-1). The
        // plan lapses by itself 60 s after the taker's last progress. A command that never ran
        // waits for nothing: the takeover may be waiting for this wrapper to go.
        if exit_code.is_some() {
            let mut said: Option<Instant> = None;
            while self.seizing(&h.claim.resource.id).await == Some(true) {
                if said.is_none_or(|at| at.elapsed() >= wait::PRINT_EVERY) {
                    eprintln!(
                        "[hostler] {display} is being taken over; waiting for it to finish before \
                         releasing — this is waiting, not hanging."
                    );
                    said = Some(Instant::now());
                }
                tokio::time::sleep(RETRY_PAUSE).await;
            }
        }
        let release = Request::ClaimsRelease(ClaimRelease {
            claim_id: h.claim.id.clone(),
            workspace_id: self.workspace.id.clone(),
            requester: self.requester.clone(),
            exit_code,
        });
        let Ok(outcome) = self.call(release).await.and_then(decode::<ReleaseOutcome>) else {
            return Ended::NotReleased;
        };
        // L37: a claim that ended `Seized` names its taker.
        if let Some(by) = outcome.seized_by {
            return Ended::TakenOver { by };
        }
        match (h.joined, outcome.parent) {
            (false, _) => Ended::Released,
            (true, Some(parent)) if parent.kind == ClaimKind::Reservation => Ended::StillReserved,
            (true, Some(parent)) if parent.kind == ClaimKind::Hold => Ended::StillHeld {
                hold_id: parent.id,
                until: match parent.binding {
                    Some(HoldBinding::Ttl { expires_at_ms, .. }) => Some(expires_at_ms),
                    _ => None,
                },
            },
            (true, Some(parent)) => Ended::SharedWith {
                lease_id: parent.id,
            },
            (true, None) => match (&h.claim.parent, h.parent_kind) {
                (Some(hold_id), Some(ClaimKind::Hold)) => Ended::HoldEnded {
                    hold_id: hold_id.clone(),
                },
                _ => Ended::Released,
            },
        }
    }

    /// Whether this workspace still reserves `resource`.
    async fn reserved(&mut self, resource: &ResourceId) -> bool {
        let Ok(Ok(snapshot)) = self
            .call(Request::ClaimsSnapshot)
            .await
            .map(decode::<ClaimsSnapshot>)
        else {
            return false;
        };
        let ws = self.workspace.id.clone();
        snapshot
            .resources
            .iter()
            .filter(|r| r.resource.id == *resource)
            .flat_map(|r| &r.claims)
            .any(|c| c.kind == ClaimKind::Reservation && c.workspace.id == ws)
    }

    /// Whether a takeover plan freezes `resource`; `None` when the service does not answer.
    async fn seizing(&mut self, resource: &ResourceId) -> Option<bool> {
        let snapshot: ClaimsSnapshot =
            decode(self.call(Request::ClaimsSnapshot).await.ok()?).ok()?;
        Some(
            snapshot
                .resources
                .iter()
                .any(|r| r.resource.id == *resource && r.seizing),
        )
    }
}

fn no_answer() -> HostlerError {
    HostlerError {
        code: ErrorCode::ServiceUnavailable,
        message: "the hostler service did not answer in time".into(),
        details: serde_json::json!({ "reason": "timeout" }),
        recovery: Some("hostler service start".into()),
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, HostlerError> {
    serde_json::from_value(value).map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("unreadable answer from the service: {e}"),
        details: serde_json::json!({}),
        recovery: None,
    })
}

fn internal(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message,
        details: serde_json::json!({}),
        recovery: None,
    }
}

fn refuse(e: &HostlerError) -> i32 {
    print_error(e);
    EXIT_REFUSED
}

/// `hostler run` with `--screen` and/or `--account`.
pub(crate) async fn main(args: RunArgs, max_wait: Option<Duration>) -> i32 {
    if let Some(name) = &args.account
        && !crate::contract::accounts::valid_account_name(name)
    {
        eprintln!(
            "[hostler] {}",
            crate::contract::accounts::INVALID_ACCOUNT_NAME
        );
        return EXIT_USAGE;
    }
    let tokens_of = |screen: bool, account: Option<&str>| {
        let mut t = Vec::new();
        if screen {
            t.push(ResourceId::screen().token());
        }
        t.extend(account.map(str::to_owned));
        t
    };
    let device_arg = match args.device.as_deref().map(parse_device_arg).transpose() {
        Ok(arg) => arg,
        Err(e) => {
            eprintln!("[hostler] {e}");
            return EXIT_USAGE;
        }
    };
    let confirmation_refused = || {
        eprintln!(
            "[hostler] --confirmation names the resource to take over: screen, the account's \
             name, or the device's alias."
        );
        EXIT_USAGE
    };
    // A device's alias is known only from the service: checked once it is resolved.
    if device_arg.is_none()
        && let Some(confirmation) = &args.confirmation
        && !confirmation_names(
            confirmation,
            &tokens_of(args.screen, args.account.as_deref()),
        )
    {
        return confirmation_refused();
    }
    let raw = run::display_of(&args.command);
    let argv = run::command_argv(args.command.clone());
    // Before any request names this process (W-3) and before any value is held.
    spawn::let_service_observe_self();
    // L34: one listener for the whole run, from before the first request to the command's end.
    let signals = spawn::Signals::register();
    let id = uuid::Uuid::new_v4().to_string();

    // 1. The workspace (fail closed).
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return refuse(&internal(format!("cannot read the current directory: {e}"))),
    };
    let current = match project::current(&cwd) {
        Ok(current) => current,
        Err(e) => return refuse(&e),
    };
    // P4 R2-2: a denied state or socket directory is `sandbox_denied`.
    let paths = match crate::client::resolve_paths() {
        Ok(paths) => paths,
        Err(e) => return refuse(&e),
    };
    let mut client =
        match Client::connect_or_start(&paths, &OsManager, crate::cli::REPLACE_ON_START).await {
            Ok(client) => client,
            Err(e) => return refuse(&e),
        };
    let workspace = match workspace::resolve(&mut client, &current).await {
        Ok(workspace) => workspace,
        Err(e) => return refuse(&e),
    };
    let requester = match spawn::process_table().and_then(|t| current_identity(&t)) {
        Ok(requester) => requester,
        Err(e) => return refuse(&crate::client::process_error(&e)),
    };
    // 1b. The device: a name must be registered; its alias is the --confirmation token.
    let device = match &device_arg {
        None => None,
        Some(arg) => match device_selector(&mut client, arg, args.boot).await {
            Ok((selector, shown)) => {
                let alias = matches!(arg, DeviceArg::Named(_)).then(|| shown.clone());
                Some((selector, shown, alias))
            }
            Err(e) => return refuse(&e),
        },
    };
    if device.is_some()
        && let Some(confirmation) = &args.confirmation
    {
        let mut tokens = tokens_of(args.screen, args.account.as_deref());
        tokens.extend(device.as_ref().and_then(|(_, _, alias)| alias.clone()));
        if !confirmation_names(confirmation, &tokens) {
            return confirmation_refused();
        }
    }

    // 2. P2's secrets: their masked display is the display from here on.
    let secrets = !args.secret.is_empty() || !args.secret_file.is_empty();
    let (mut io, files, display, mut values) = if secrets {
        let specs = match secret_run::specs(&args.secret, &args.secret_file) {
            Ok(specs) => specs,
            Err(code) => return code,
        };
        match secret_run::setup_in(&specs, &current, &paths, &id, &raw, &mut client).await {
            Ok(p) => (p.io, p.files, p.display, p.values),
            Err(code) => return code,
        }
    } else {
        (ChildIo::default(), None, raw.clone(), Vec::new())
    };

    // 3. The account must exist.
    if let Some(name) = &args.account
        && let Err(e) = client
            .call(Request::AccountCheck {
                project: current.project.clone(),
                name: name.clone(),
            })
            .await
    {
        return refuse(&e);
    }

    // 4. The claims, in ascending resource id order, under one deadline.
    let deadline = Deadline {
        started: Instant::now(),
        max_wait,
    };
    let mut stop = Stop::new(&deadline, signals.ok());
    let mut claims = Claims {
        client,
        paths,
        workspace,
        requester,
        taken: Vec::new(),
        device_alias: None,
        ending: false,
        end_by: None,
    };
    let command = if args.account.is_some() {
        COMMAND_PENDING.to_owned()
    } else {
        run::keep_head(&display, MAX_CLAIM_COMMAND)
    };
    let inherited = inherited_tokens();
    let mut wanted: Vec<Want> = resources(
        args.screen,
        args.account.as_deref(),
        &claims.workspace.project_id,
    )
    .into_iter()
    .map(|(resource, shown)| Want {
        key: resource.0.clone(),
        token: Some(resource.token()),
        selector: Selector::Exact { resource },
        shown,
        device: false,
    })
    .collect();
    if let Some((selector, shown, alias)) = device {
        wanted.push(Want {
            // `auto` sorts where any device id would (between accounts and the screen).
            key: match &selector {
                Selector::Exact { resource } => resource.0.clone(),
                Selector::Any { .. } => "device:".to_owned(),
            },
            selector,
            shown: format!("device {shown}"),
            token: alias,
            device: true,
        });
        wanted.sort_by(|a, b| a.key.cmp(&b.key));
    }
    for want in wanted {
        let Want {
            selector,
            shown,
            token,
            device,
            ..
        } = want;
        let request = ClaimRequest {
            id: uuid::Uuid::new_v4().to_string(),
            selector: selector.clone(),
            workspace_id: claims.workspace.id.clone(),
            requester: claims.requester.clone(),
            purpose: Purpose::Lease {
                command: command.clone(),
            },
            reentrant_tokens: inherited.clone(),
            no_wait: args.no_wait,
        };
        let forced = token.is_some() && args.confirmation == token;
        let resource = match &selector {
            Selector::Exact { resource } => Some(resource.clone()),
            Selector::Any { .. } => None,
        };
        let target = Target {
            resource: resource.as_ref(),
            shown: &shown,
            token: token.as_deref().unwrap_or(""),
            device,
        };
        let got = acquire(&mut claims, &target, request, &mut stop, forced).await;
        let held = match got {
            Ok(Acquired::Granted(claim)) => Some(Held {
                claim,
                joined: false,
                parent_kind: None,
            }),
            Ok(Acquired::Joined(claim)) => {
                let parent = claim.parent.clone().unwrap_or_default();
                let parent_kind = claims.live_claim(&parent).await.map(|c| c.kind);
                Some(Held {
                    claim,
                    joined: true,
                    parent_kind,
                })
            }
            Ok(Acquired::Reentrant(_)) => None,
            Err(code) => return claims.finish(code, None, None).await,
        };
        // `auto`: the device granted is shown by its alias from here on.
        let shown = match held.as_ref().filter(|_| device) {
            Some(h) => {
                claims.device_alias = Some(h.claim.resource.display.clone());
                format!("device {}", h.claim.resource.display)
            }
            None => shown,
        };
        claims.taken.push((shown, held));
        if let Some(code) = stop.interrupted() {
            eprintln!("[hostler] Interrupted; not running.");
            return claims.finish(code, None, None).await;
        }
    }
    run::kill_self_if("HOSTLER_TEST_KILL_AFTER_CLAIM_GRANT");
    let held = claims.held();

    // 3. A device that is not ready: booted with --boot, else released and refused (78).
    let device_claim = held
        .iter()
        .find(|h| h.claim.resource.kind == ResourceKind::Device)
        .map(|h| h.claim.clone());
    if let Some(claim) = &device_claim {
        let workspace_id = claims.workspace.id.clone();
        match ready_device(
            &mut claims.client,
            claim,
            &workspace_id,
            args.boot,
            &mut stop,
        )
        .await
        {
            Ok(_) => {}
            Err(NotReady::Interrupted(code)) => return claims.finish(code, None, None).await,
            Err(NotReady::Refused(e)) => {
                let code = refuse(&e);
                return claims.finish(code, None, None).await;
            }
        }
        if let Some(reason) = claim_wait::sessions_unknown(&mut claims.client).await {
            eprintln!("{}", claim_wait::sessions_unknown_line(&reason));
        }
    }

    // 5. The account's fields, for an account claim of this run's own.
    let own_account = args.account.as_ref().and_then(|name| {
        held.iter()
            .find(|h| h.claim.resource.id.account_project().is_some())
            .map(|h| (name.clone(), h.claim.id.clone()))
    });
    if let Some((name, claim_id)) = own_account {
        let request = Request::AccountMaterialize(AccountMaterialize {
            run_id: id.clone(),
            claim_id,
            requester: claims.requester.clone(),
            project: current.project.clone(),
            name,
            command: secret_run::raw_command(&display),
            workspace: project::display_path(&current.checkout.workspace_root),
        });
        let account: MaterializedAccount = match claims.client.call_sensitive(request).await {
            Ok(account) => account,
            Err(e) => {
                let code = if taken_over(&e) {
                    eprintln!(
                        "[hostler] account {} was taken over before the command started; not \
                         running.",
                        args.account.as_deref().unwrap_or_default()
                    );
                    EXIT_REFUSED
                } else {
                    refuse(&e)
                };
                return claims.finish(code, None, None).await;
            }
        };
        values.extend(account.fields.iter().map(|f| f.value.clone()));
        if !io.env.iter().any(|(name, _)| name == SECRET_RUN_ENV) {
            // First: no injection of the same name can unset it.
            io.env
                .insert(0, (OsString::from(SECRET_RUN_ENV), OsString::from("1")));
        }
        io.env.extend(account_env(&account));
        io.masks = Some(Arc::new(merged_mask(&values)));
    }
    if let Some(code) = stop.interrupted() {
        eprintln!("[hostler] Interrupted; not running.");
        return claims.finish(code, None, None).await;
    }

    // 6. The command as others see it, masked with every value.
    let display = run::keep_head(&io.mask(&raw), MAX_CLAIM_COMMAND);
    if args.account.is_some() {
        for h in &held {
            // A claim seized meanwhile is refused at `claims.started`.
            let _ = claims
                .call(Request::ClaimsSetCommand {
                    claim_id: h.claim.id.clone(),
                    workspace_id: claims.workspace.id.clone(),
                    command: display.clone(),
                })
                .await;
        }
    }

    // 7. The heavy slot, under the same deadline; fail-open for itself only.
    let signals = stop.take_signals();
    let (mut heavy, signals) = match args.heavy.clone() {
        None => (None, signals),
        Some(kind) => {
            match heavy_slot(id.clone(), kind, &display, args.no_wait, &deadline, signals).await {
                Ok(slot) => slot,
                Err(code) => return claims.finish(code, None, None).await,
            }
        }
    };
    let signals = signals.map(Ok).unwrap_or_else(spawn::Signals::register);

    // 8–11. The command, with the device as it is right now (P-15).
    let info = match &device_claim {
        Some(claim) => match device_at_spawn(&mut claims.client, claim).await {
            Ok(info) => {
                io.remove = inherited_device_names(std::env::vars_os().map(|(k, _)| k));
                Some(info)
            }
            Err(e) => {
                let code = refuse(&e);
                if let Some(heavy) = heavy.as_mut() {
                    heavy.release(code).await;
                }
                return claims.finish(code, None, None).await;
            }
        },
        None => None,
    };
    for h in &held {
        io.env.extend(
            claim_env(&h.claim, info.as_ref())
                .into_iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
    }
    if held.is_empty() && heavy.is_none() {
        // Every claim is the enclosing run's: run in its group, so its takeover reaches this
        // command too (L18).
        let code = run::run_unqueued(argv, signals, &io, files, Group::Enclosing).await;
        return claims.finish(code, Some(code), None).await;
    }
    io.env.push((
        OsString::from(CLAIM_TOKENS_ENV),
        OsString::from(tokens(&inherited, &held)),
    ));
    run_registered(&mut claims, heavy, &id, argv, signals, io, files).await
}

/// The merged mask of the secret values and the account's fields.
fn merged_mask(values: &[SecretValue]) -> MaskSet {
    MaskSet::new(values.iter().map(|v| v.expose().as_bytes().to_vec()))
}

/// One wanted claim of the run, in the order it is taken.
struct Want {
    key: String,
    selector: Selector,
    shown: String,
    /// What `--confirmation` must equal; `None` for `auto` (nothing to take over yet).
    token: Option<String>,
    device: bool,
}

/// What `acquire` asks for: the resource (`None` for `auto`), its display and token.
struct Target<'a> {
    resource: Option<&'a ResourceId>,
    shown: &'a str,
    token: &'a str,
    device: bool,
}

/// One claim: a recorded takeover first when `forced` and another workspace holds it, else the
/// visible wait. The exit code when it did not come to a claim.
async fn acquire(
    claims: &mut Claims,
    target: &Target<'_>,
    request: ClaimRequest,
    stop: &mut Stop,
    forced: bool,
) -> Result<Acquired, i32> {
    let shown = target.shown;
    if forced && let Some(resource) = target.resource {
        let first = claim_wait::acquire_once(&mut claims.client, &request, stop)
            .await
            .map_err(|gave| gave_code(gave, shown, stop))?;
        let own = claims.workspace.path.clone();
        let takeable = match &first {
            ClaimOutcome::Queued { blockers, .. } | ClaimOutcome::Busy { blockers, .. } => {
                blockers.iter().any(|b| match b {
                    Blocker::Claim { summary } => summary.workspace_path != own,
                    Blocker::Foreign { claim } => claim.stop.is_some(),
                    // A takeover cannot end the wait for a fresh sessions read: `--force` waits
                    // on it like any run (P3bT1-8); its line names the fix (L54).
                    Blocker::Seizing { .. } | Blocker::Checking { .. } => false,
                })
            }
            _ => false,
        };
        match first {
            ClaimOutcome::Granted { claim } => return Ok(Acquired::Granted(claim)),
            ClaimOutcome::Joined { claim } => return Ok(Acquired::Joined(claim)),
            ClaimOutcome::Reentrant { claim_id } => return Ok(Acquired::Reentrant(claim_id)),
            _ if takeable => {
                let taken = claim_wait::take_over(
                    &mut claims.client,
                    resource,
                    target.token,
                    request.clone(),
                    stop,
                )
                .await;
                match taken {
                    Ok(ClaimOutcome::Granted { claim }) => return Ok(Acquired::Granted(claim)),
                    Ok(ClaimOutcome::Joined { claim }) => return Ok(Acquired::Joined(claim)),
                    Ok(ClaimOutcome::Reentrant { claim_id }) => {
                        return Ok(Acquired::Reentrant(claim_id));
                    }
                    // Anything else: the request is still ours; wait for it below.
                    Ok(_) => {}
                    Err(e) => {
                        // A grant that slipped past the failure is ours (T8-4); else the
                        // request is withdrawn. Interrupted: 130/143; refused: 78.
                        let code = claim_wait::takeover_exit_code(&e);
                        match claim_wait::settle_failed_takeover(&mut claims.client, &request, e)
                            .await
                        {
                            Ok(acquired) => return Ok(acquired),
                            Err(e) => {
                                print_error(&e);
                                return Err(code);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // No takeover line for `auto`: there is no alias to confirm yet.
    let takeover = (!forced && !target.token.is_empty()).then_some("rerun with");
    let own = claims.workspace.path.clone();
    let text = WaitText {
        display: shown,
        own_workspace: &own,
        takeover,
        token: target.token,
        device: target.device,
    };
    let no_wait = request.no_wait;
    claim_wait::acquire_waiting(&mut claims.client, request, &text, stop, no_wait)
        .await
        .map_err(|gave| gave_code(gave, shown, stop))
}

/// What a give-up prints, and its exit code.
fn gave_code(gave: Gave, shown: &str, stop: &Stop) -> i32 {
    match gave {
        Gave::TimedOut => {
            let limit = stop
                .max_wait()
                .map_or_else(String::new, wait::duration_text);
            eprintln!(
                "[hostler] Gave up after {limit} waiting for {shown}; the request was withdrawn."
            );
            EXIT_QUEUE
        }
        Gave::Busy { position, of } => {
            eprintln!(
                "[hostler] {shown} is busy (you would be {position} of {of}); not waiting \
                 (--no-wait)."
            );
            EXIT_QUEUE
        }
        Gave::Interrupted(code) => {
            eprintln!("[hostler] Interrupted; the request was withdrawn.");
            code
        }
        Gave::WithdrawnElsewhere => {
            eprintln!(
                "[hostler] The request for {shown} was withdrawn by an operator; not running."
            );
            EXIT_QUEUE
        }
        Gave::Deadlock(e) => refuse(&e),
        Gave::Failed(e) if taken_over(&e) => {
            eprintln!("{}", run::TAKEN_OVER);
            print_error(&e);
            EXIT_REFUSED
        }
        Gave::Failed(e) if e.code == ErrorCode::QueueBusy => {
            print_error(&e);
            EXIT_QUEUE
        }
        Gave::Failed(e) => refuse(&e),
    }
}

/// P1's heavy wait under the run's deadline and signals: the slot (`Some` when this run holds
/// one), the signals back, or the exit code.
async fn heavy_slot(
    id: String,
    kind: String,
    display: &str,
    no_wait: bool,
    deadline: &Deadline,
    signals: Option<spawn::Signals>,
) -> Result<(Option<Heavy>, Option<spawn::Signals>), i32> {
    let mut heavy = match Heavy::prepare(id, kind, display, no_wait) {
        Ok(heavy) => heavy,
        Err(e) => {
            run::warn_fail_open(&e);
            return Ok((None, signals));
        }
    };
    match Client::connect_or_start(&heavy.paths, &OsManager, crate::cli::REPLACE_ON_START).await {
        Ok(client) => heavy.client = Some(client),
        Err(e) => {
            run::warn_fail_open(&e);
            return Ok((None, signals));
        }
    }
    let mut signals = signals.map(Ok);
    let mut outcome = heavy
        .outcome(Request::HeavyAcquire(heavy.req.clone()))
        .await;
    let mut stop: Option<run::Stop> = None;
    let outcome = loop {
        match outcome {
            Ok(AcquireOutcome::Queued { position, of, .. }) => {
                let s = stop.get_or_insert_with(|| {
                    run::Stop::since(deadline.started, deadline.max_wait, signals.take())
                });
                outcome = match run::wait_for_slot(&mut heavy, s, position, of).await {
                    ControlFlow::Continue(next) => next,
                    ControlFlow::Break(stopped) => {
                        return Err(run::give_up(&mut heavy, stopped, deadline.max_wait).await);
                    }
                };
            }
            decided => break decided,
        }
    };
    if let Some(s) = stop.as_mut() {
        if let Some(stopped) = s.now().await {
            return Err(run::give_up(&mut heavy, stopped, deadline.max_wait).await);
        }
        signals = s.take_signals();
    }
    let signals = signals.and_then(Result::ok);
    match outcome {
        Err(e) if e.code == ErrorCode::QueueBusy => {
            eprintln!("[hostler] {}", e.message);
            Err(EXIT_QUEUE)
        }
        Err(e) if taken_over(&e) => {
            eprintln!("{}", run::TAKEN_OVER);
            Err(EXIT_REFUSED)
        }
        Err(e) => {
            run::warn_fail_open(&e);
            Ok((None, signals))
        }
        Ok(AcquireOutcome::Granted { .. }) => {
            if stop.is_some() {
                let waited = Instant::now().saturating_duration_since(deadline.started);
                eprintln!(
                    "[hostler] Slot granted after {}.",
                    wait::duration_text(waited)
                );
            }
            Ok((Some(heavy), signals))
        }
        Ok(AcquireOutcome::Reentrant) => Ok((None, signals)),
        Ok(AcquireOutcome::Busy { position, of }) => {
            eprintln!(
                "[hostler] The heavy slot is busy (you would be {position} of {of}); not \
                 waiting (--no-wait)."
            );
            Err(EXIT_QUEUE)
        }
        Ok(AcquireOutcome::Finished { .. }) => {
            eprintln!("[hostler] The request was withdrawn by an operator; not running.");
            Err(EXIT_QUEUE)
        }
        Ok(AcquireOutcome::Queued { .. }) => {
            unreachable!("the wait loop decides every queued request")
        }
    }
}

/// Steps 8–11: the command behind its gate in this run's group/job, every claim (and the heavy
/// slot) started before the gate opens, then the releases and the summary.
async fn run_registered(
    claims: &mut Claims,
    mut heavy: Option<Heavy>,
    id: &str,
    argv: Vec<OsString>,
    signals: std::io::Result<spawn::Signals>,
    io: ChildIo,
    files: Option<crate::secrets::rundir::RunDir>,
) -> i32 {
    let program = io.mask(&spawn::program(&argv));
    run::pause_before_spawn();
    let token = heavy.as_ref().map(|h| h.req.id.clone());
    let spawned = signals
        .context("cannot install signal forwarding")
        .and_then(|signals| spawn::spawn_in_group(argv, id, token.as_deref(), signals, &io));
    let mut spawned = match spawned {
        Ok(spawned) => spawned,
        Err(e) => {
            eprintln!("[hostler] cannot run {program}: {e:#}");
            if let Some(heavy) = heavy.as_mut() {
                heavy.release(EXIT_CANNOT_RUN).await;
            }
            return claims.finish(EXIT_CANNOT_RUN, None, None).await;
        }
    };
    let pumps = secret_run::start_pumps(&io, spawned.take_output());
    // Ends the gated child unopened, then everything this run holds; `$told` is a claim whose
    // fate was already printed.
    macro_rules! not_running {
        ($code:expr) => {
            not_running!($code, None)
        };
        ($code:expr, $told:expr) => {{
            let _ = run::wait_blocking(move || spawned.abandon()).await;
            secret_run::drain(pumps, Instant::now()).await;
            if let Some(heavy) = heavy.as_mut() {
                heavy.release($code).await;
            }
            drop(files);
            return claims.finish($code, None, $told).await;
        }};
    }
    if let Some(files) = &files
        && let Err(e) = files.record_child(&spawned.identity())
    {
        eprintln!("[hostler] {e:#}");
        not_running!(EXIT_REFUSED);
    }
    run::kill_self_if("HOSTLER_TEST_KILL_BEFORE_GATE");
    // 9. Every claim acknowledges the command before it may run (P1's order, L27).
    for (display, held) in claims.taken.clone() {
        let Some(h) = held else { continue };
        let started = Request::ClaimsStarted(ClaimStarted {
            claim_id: h.claim.id.clone(),
            child: spawned.identity(),
            group: spawned.group(),
        });
        match claims.call(started).await {
            Ok(_) => {}
            // L33 (a plan is live) or L27's end of it (seized or reaped meanwhile).
            Err(e) if ended_before_start(&e) => {
                eprintln!(
                    "[hostler] {display} was taken over before the command started; not \
                     running."
                );
                not_running!(EXIT_REFUSED, Some(h.claim.id.as_str()));
            }
            Err(e) => {
                print_error(&e);
                not_running!(EXIT_REFUSED);
            }
        }
    }
    if let Some(slot) = heavy.as_mut() {
        let started = Request::HeavyStarted(HeavyStarted {
            id: slot.req.id.clone(),
            child: spawned.identity(),
            group: spawned.group(),
        });
        match slot.call(started).await {
            Err(e) if taken_over(&e) => {
                eprintln!("{}", run::TAKEN_OVER);
                not_running!(EXIT_REFUSED);
            }
            Err(e) if matches!(e.code, ErrorCode::Withdrawn | ErrorCode::UnknownEntry) => {
                eprintln!("[hostler] The request was withdrawn by an operator; not running.");
                not_running!(EXIT_QUEUE);
            }
            // As in P1: an undelivered `heavy.started` only leaves the group unseen.
            _ => {}
        }
    }
    if let Err(e) = spawned.release_gate() {
        eprintln!("[hostler] {e:#}");
    }
    let code = run::wait_blocking(move || spawned.wait()).await;
    let exited = Instant::now();
    let drained = async move {
        secret_run::drain(pumps, exited).await;
        drop(files);
    };
    let release_heavy = async {
        if let Some(heavy) = heavy.as_mut() {
            heavy.release(code).await;
        }
    };
    tokio::join!(release_heavy, drained);
    claims.finish(code, Some(code), None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::claims::fixtures;
    use crate::contract::secrets::MaterializedSecret;

    fn held(id: &str, joined: bool) -> Held {
        let mut claim = fixtures::claim();
        claim.id = id.into();
        Held {
            claim,
            joined,
            parent_kind: None,
        }
    }

    #[test]
    fn claims_acquired_in_resource_id_order() {
        let got = resources(true, Some("qa-user"), "p1");
        let ids: Vec<&str> = got.iter().map(|(r, _)| r.0.as_str()).collect();
        assert_eq!(ids, ["account:p1/qa-user", "screen"]);
        assert_eq!(got[0].1, "account qa-user");
        assert_eq!(resources(true, None, "p1").len(), 1);
    }

    #[test]
    fn summary_line_shapes() {
        assert_eq!(
            summary_line("screen", &Ended::Released),
            "[hostler] screen: released."
        );
        let still = summary_line(
            "screen",
            &Ended::StillHeld {
                hold_id: "h1".into(),
                until: Some(0),
            },
        );
        assert!(
            still.starts_with("[hostler] screen: still held by your hold h1 until ")
                && still.ends_with("; hostler release h1 frees it."),
            "{still}"
        );
        assert_eq!(
            summary_line(
                "account qa-user",
                &Ended::HoldEnded {
                    hold_id: "h1".into()
                }
            ),
            "[hostler] account qa-user: released (your hold h1 ended while the command ran)."
        );
        assert_eq!(
            summary_line(
                "screen",
                &Ended::TakenOver {
                    by: "/work/b".into()
                }
            ),
            "[hostler] screen: taken over by /work/b while the command ran."
        );
        assert_eq!(
            summary_line("screen", &Ended::NotReleased),
            "[hostler] screen: the service did not answer; the claim ends when this process is \
             gone."
        );
        assert_eq!(
            summary_line("screen", &Ended::Enclosing),
            "[hostler] screen: held by the enclosing run."
        );
    }

    #[test]
    fn claim_env_is_empty_for_screen() {
        assert!(claim_env(&fixtures::claim(), None).is_empty());
        let device = crate::contract::devices::fixtures::info();
        assert!(claim_env(&fixtures::claim(), Some(&device)).is_empty());
    }

    fn device_claim(info: &DeviceInfo) -> Claim {
        let mut claim = fixtures::claim();
        claim.id = "lease-1".into();
        claim.resource = crate::devices::catalog::entries(std::slice::from_ref(info))
            .pop()
            .unwrap();
        claim
    }

    #[test]
    fn claim_env_android_and_ios_from_fresh_info() {
        use crate::contract::devices::{Transport, TransportKind};
        let mut phone = crate::contract::devices::fixtures::info();
        let claim = device_claim(&phone);
        // The transport as it is now: the phone moved to Wi-Fi after the grant.
        phone.transports.reverse();
        let env: BTreeMap<String, String> = claim_env(&claim, Some(&phone)).into_iter().collect();
        let want: BTreeMap<String, String> = [
            ("HOSTLER_DEVICE_ID", "SERIAL0001"),
            ("HOSTLER_DEVICE_ALIAS", "pixel-9"),
            ("HOSTLER_DEVICE_PLATFORM", "android"),
            ("HOSTLER_LEASE_ID", "lease-1"),
            ("ANDROID_SERIAL", "192.0.2.10:5555"),
            ("HOSTLER_AGENT_DEVICE_ID", "192.0.2.10:5555"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(env, want);

        let mut sim = crate::contract::devices::fixtures::info();
        sim.resource = ResourceId("device:ios:00000000-0000-4000-8000-000000000001".into());
        sim.hardware_id = "00000000-0000-4000-8000-000000000001".into();
        sim.platform = Platform::Ios;
        sim.kind = DeviceKind::Simulator;
        sim.alias = Some("iphone-17".into());
        sim.transports = vec![Transport {
            kind: TransportKind::Simulator,
            address: sim.hardware_id.clone(),
            state: "booted".into(),
        }];
        let env: BTreeMap<String, String> = claim_env(&device_claim(&sim), Some(&sim))
            .into_iter()
            .collect();
        assert_eq!(env["HOSTLER_DEVICE_UDID"], sim.hardware_id);
        assert_eq!(env["HOSTLER_AGENT_DEVICE_ID"], sim.hardware_id);
        assert_eq!(env["HOSTLER_DEVICE_PLATFORM"], "ios");
        assert!(!env.contains_key("ANDROID_SERIAL"));

        // A phone that is not connected right now gets no serial.
        let mut gone = crate::contract::devices::fixtures::info();
        gone.connected = false;
        let env: BTreeMap<String, String> = claim_env(&claim, Some(&gone)).into_iter().collect();
        assert!(
            !env.contains_key("ANDROID_SERIAL") && !env.contains_key("HOSTLER_AGENT_DEVICE_ID")
        );
        assert_eq!(env["HOSTLER_DEVICE_ID"], "SERIAL0001");
    }

    #[test]
    fn device_arg_parses() {
        assert_eq!(
            parse_device_arg("pixel-9"),
            Ok(DeviceArg::Named("pixel-9".into()))
        );
        assert_eq!(
            parse_device_arg("emulator-5554"),
            Ok(DeviceArg::Named("emulator-5554".into()))
        );
        assert_eq!(
            parse_device_arg("autobahn"),
            Ok(DeviceArg::Named("autobahn".into()))
        );
        assert_eq!(
            parse_device_arg("auto"),
            Ok(DeviceArg::Auto {
                platform: None,
                kind: None
            })
        );
        assert_eq!(
            parse_device_arg("auto:ios"),
            Ok(DeviceArg::Auto {
                platform: Some(Platform::Ios),
                kind: None
            })
        );
        assert_eq!(
            parse_device_arg("auto:android:emulator"),
            Ok(DeviceArg::Auto {
                platform: Some(Platform::Android),
                kind: Some(DeviceKind::Emulator)
            })
        );
        for bad in [
            "",
            "-x",
            "auto:",
            "auto:windows",
            "auto:android:simulator",
            "auto:ios:emulator",
            "auto:ios:physical:x",
            "auto:android:tablet",
        ] {
            assert!(parse_device_arg(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn auto_selector_requires_ready_unless_boot() {
        let Selector::Any { kind, attributes } =
            auto_selector(Some(Platform::Android), Some(DeviceKind::Emulator), false)
        else {
            panic!("not Any");
        };
        assert_eq!(kind, ResourceKind::Device);
        assert_eq!(attributes["platform"], "android");
        assert_eq!(attributes["kind"], "emulator");
        assert_eq!(attributes["ready"], "true");
        let Selector::Any { attributes, .. } = auto_selector(None, None, true) else {
            panic!("not Any");
        };
        assert!(attributes.is_empty(), "{attributes:?}");
    }

    #[test]
    fn device_confirmation_is_the_alias() {
        let info = crate::contract::devices::fixtures::info();
        let claim = device_claim(&info);
        // The token the engine compares (T1-3) is the alias, never the resource id.
        let token = crate::claims::engine::token_of(&claim.resource);
        assert_eq!(token, "pixel-9");
        let tokens = vec!["screen".to_owned(), token];
        assert!(confirmation_names("pixel-9", &tokens));
        assert!(!confirmation_names("device:android:SERIAL0001", &tokens));
        assert!(!confirmation_names("SERIAL0001", &tokens));
    }

    #[test]
    fn open_session_warnings_are_this_workspaces_and_the_windows() {
        use crate::contract::devices::AgentDeviceSession;
        let mut info = crate::contract::devices::fixtures::info();
        let session = |name: &str, ws: Option<&str>, seen: i64, p: Platform| AgentDeviceSession {
            session: name.into(),
            workspace_path: "/work/x".into(),
            workspace_id: ws.map(Into::into),
            seen_since_ms: seen,
            kept: false,
            platform: p,
            agent_device_id: "SERIAL0001".into(),
        };
        info.sessions = vec![
            session("own", Some("ws-1"), 10, Platform::Android),
            session("cdd", None, 1_500, Platform::Android),
            session("other", Some("ws-2"), 10, Platform::Android),
            session("late", Some("ws-2"), 5_000, Platform::Ios),
        ];
        let lines = open_session_warnings(&info, "ws-1", (1_000, 2_000));
        assert_eq!(
            lines,
            [
                "[hostler] agent-device session own is still open on pixel-9: it blocks other \
                 workspaces until closed: agent-device close --session own --platform android \
                 --serial SERIAL0001",
                "[hostler] agent-device session cdd is still open on pixel-9: it blocks other \
                 workspaces until closed: agent-device close --session cdd --platform android \
                 --serial SERIAL0001",
            ]
        );
        info.sessions = vec![session("s\x1b[2J", Some("ws-1"), 10, Platform::Ios)];
        let lines = open_session_warnings(&info, "ws-1", (0, 0));
        assert!(
            lines[0].ends_with("--platform ios --udid SERIAL0001"),
            "{lines:?}"
        );
        assert!(!lines[0].contains('\x1b'), "{lines:?}");
    }

    #[test]
    fn exit_summary_lines_for_a_device() {
        assert_eq!(
            device_summary_line("pixel_9", &Ended::Released),
            "[hostler] device pixel_9: released; free."
        );
        assert_eq!(
            device_summary_line("pixel_9", &Ended::StillReserved),
            "[hostler] device pixel_9: released; still reserved by this workspace (hostler \
             devices unreserve pixel_9 frees it)."
        );
        assert_eq!(
            device_summary_line("pixel_9", &Ended::Enclosing),
            "[hostler] device pixel_9: held by the enclosing run."
        );
    }

    #[test]
    fn hold_json_merges_the_device_as_of_now() {
        use crate::contract::devices::{Transport, TransportKind};
        // P2-8: granted while shut down (no serial), booted since as emulator-5554.
        let mut avd = crate::contract::devices::fixtures::info();
        avd.resource = ResourceId("device:android:avd:Pixel_9".into());
        avd.kind = DeviceKind::Emulator;
        avd.alias = Some("pixel_9".into());
        avd.transports.clear();
        avd.ready = false;
        let mut claim = device_claim(&avd);
        assert!(!claim.resource.attributes.contains_key("serial"));
        avd.transports = vec![Transport {
            kind: TransportKind::Emulator,
            address: "emulator-5554".into(),
            state: "device".into(),
        }];
        avd.ready = true;
        merge_device(&mut claim, &avd);
        assert_eq!(claim.resource.attributes["serial"], "emulator-5554");
        assert_eq!(
            claim.resource.attributes["agent_device_id"],
            "emulator-5554"
        );
        assert_eq!(claim.resource.attributes["ready"], "true");
        assert_eq!(claim.resource.display, "pixel_9");
    }

    #[test]
    fn account_env_names() {
        let account = MaterializedAccount {
            name: "qa-user".into(),
            fields: vec![MaterializedSecret {
                name: "session".into(),
                value: SecretValue::new("tok-1234".into()),
            }],
        };
        let env = account_env(&account);
        assert_eq!(env[0], ("HOSTLER_ACCOUNT_NAME".into(), "qa-user".into()));
        assert_eq!(
            env[1],
            ("HOSTLER_ACCOUNT_SESSION".into(), "tok-1234".into())
        );
    }

    #[test]
    fn tokens_include_joined_claims() {
        let got = tokens(
            &["outer".into()],
            &[held("own", false), held("linked", true)],
        );
        assert_eq!(got, "outer,own,linked");
        assert_eq!(tokens(&[], &[held("own", false)]), "own");
    }

    #[test]
    fn merged_mask_covers_secrets_and_fields() {
        let set = merged_mask(&[
            SecretValue::new("sec-value-1".into()),
            SecretValue::new("field-value-2".into()),
        ]);
        assert_eq!(
            set.mask_all(b"a sec-value-1 b field-value-2"),
            b"a *** b ***"
        );
    }
}
