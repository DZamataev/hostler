//! `hostler devices …` (plan P3b Task 6): the commands over the device service's finished methods,
//! and the device lines of `hostler status`.
//!
//! Every read has `--json` = the contract's `Envelope<…>`. A physical device's `add`, `rename` and
//! `remove` and every `forget-sessions` pass P2's person prompt first (L49, L54); the service
//! refuses them without the flag the prompt sets. Every string a device or another agent reported
//! is sanitized before it reaches a person's terminal (L40).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgGroup, Args, Subcommand};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::cli::output::{self, print_error};
use crate::cli::secrets::PERSON_ONLY;
use crate::cli::status::{held_part, taker_of};
use crate::cli::tty::Tty;
use crate::client::Client;
use crate::contract::Envelope;
use crate::contract::claims::{
    Claim, ClaimKind, ClaimsSnapshot, ResourceId, ResourceState, WorkspaceInfo,
};
use crate::contract::devices::{
    AdbState, DeviceDetail, DeviceInfo, DeviceKind, DevicesSnapshot, ForgetRecord, InstallChange,
    InstallEvent, InstallSource, InstallTime, Platform, Reading, Telemetry, ToolPathsSet,
    ToolStatus, Transport, TransportKind, UnidentifiedTransport,
};
use crate::contract::heavy::{DEFAULT_HISTORY_LIMIT, HistoryQuery};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::devices::service::RECORDED_ENV;
use crate::devices::tools::{detect_from_path, resolve_agent_device};
use crate::heavy::engine::{Clock, SystemClock};
use crate::paths::Paths;
use crate::secrets::sanitize_for_terminal;
use crate::service::manager::OsManager;

/// L49: the registry help of `add`, `rename` and `remove`. Physical devices need a person; AVDs
/// and simulators do not.
pub(crate) const DEVICE_PERSON_ONLY: &str = "For a physical device a person runs this (they \
type its model); an agent asks the operator. Agents may add, rename and remove emulators and \
simulators themselves.";

#[derive(Debug, Args)]
pub struct DevicesArgs {
    #[command(subcommand)]
    pub command: DevicesCommand,
}

#[derive(Debug, Subcommand)]
pub enum DevicesCommand {
    /// Registered devices, new physical ones, connections, telemetry and who holds each.
    List {
        /// Also unregistered emulators and simulators, and transports that cannot be identified.
        #[arg(long)]
        all: bool,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// One device: its facts, apps with their times, and agent-device sessions.
    Show {
        /// An alias, hardware id, adb serial, `ip:port`, `emulator-N`, UDID or CoreDevice id.
        device: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Register a device. A physical device is registered by a person, who types its model.
    #[command(after_help = DEVICE_PERSON_ONLY)]
    Add {
        /// The device, as `devices list` shows it.
        device: String,
        /// The alias (default: the model, AVD or simulator name, slugged).
        #[arg(long, value_name = "ALIAS")]
        alias: Option<String>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Give a registered device another alias (person-only for a physical device).
    #[command(after_help = DEVICE_PERSON_ONLY)]
    Rename {
        device: String,
        alias: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Set or clear a device's note.
    #[command(group(ArgGroup::new("what").required(true).args(["text", "clear"])))]
    Note {
        device: String,
        /// The note's text.
        text: Option<String>,
        /// Remove the note.
        #[arg(long)]
        clear: bool,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Unregister a device; nothing on the device changes (person-only for a physical device).
    #[command(after_help = DEVICE_PERSON_ONLY)]
    Remove {
        device: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Reserve a device for this workspace, across days, until `unreserve`.
    Reserve {
        device: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// End this workspace's reservation of a device.
    Unreserve {
        device: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Install history, with what each change came from.
    Installs {
        /// One device; default: every device.
        device: Option<String>,
        /// Only changes since this long ago, e.g. `90m`, `12h`, `1d`.
        #[arg(long, value_name = "AGE", value_parser = parse_since)]
        since: Option<Duration>,
        /// At most N changes.
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// Release one device from the wait for agent-device sessions while agent-device cannot be
    /// read (L54). It stops nothing.
    #[command(name = "forget-sessions", after_help = PERSON_ONLY)]
    ForgetSessions {
        /// The device's alias.
        alias: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
    /// The device tools hostler uses (adb, agent-device, Node); record them.
    Tools {
        /// Record the agent-device, Node and Android SDK settings found from this shell.
        #[arg(long)]
        detect: bool,
        /// The adb to use (the SDK's `platform-tools/adb`).
        #[arg(long, value_name = "PATH")]
        adb: Option<String>,
        /// The agent-device launcher or package script to use.
        #[arg(long, value_name = "PATH")]
        agent_device: Option<String>,
        /// The Node to run agent-device with.
        #[arg(long, value_name = "PATH")]
        node: Option<String>,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
}

pub fn main(args: DevicesArgs) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("[hostler] cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        match args.command {
            DevicesCommand::List { all, json } => list(all, json).await,
            DevicesCommand::Show { device, json } => show(device, json).await,
            DevicesCommand::Add {
                device,
                alias,
                json,
            } => add(device, alias, json).await,
            DevicesCommand::Rename {
                device,
                alias,
                json,
            } => rename(device, alias, json).await,
            DevicesCommand::Note {
                device,
                text,
                clear,
                json,
            } => note(device, text, clear, json).await,
            DevicesCommand::Remove { device, json } => remove(device, json).await,
            DevicesCommand::Reserve { device, json } => reserve(device, json).await,
            DevicesCommand::Unreserve { device, json } => unreserve(device, json).await,
            DevicesCommand::Installs {
                device,
                since,
                limit,
                json,
            } => installs(device, since, limit, json).await,
            DevicesCommand::ForgetSessions { alias, json } => forget_sessions(alias, json).await,
            DevicesCommand::Tools {
                detect,
                adb,
                agent_device,
                node,
                json,
            } => tools(detect, adb, agent_device, node, json).await,
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Output and exits

/// A failed command with its exit code: 0/1/2/78 only, as the plan says for `hostler devices`.
#[derive(Debug)]
struct Failure {
    code: u8,
    error: HostlerError,
}

impl Failure {
    fn report(self, json: bool) -> ExitCode {
        if json {
            let envelope: Envelope<()> = Envelope {
                ok: false,
                data: None,
                error: Some(self.error),
            };
            println!(
                "{}",
                serde_json::to_string(&envelope).expect("an envelope always encodes")
            );
        } else {
            print_error(&self.error);
        }
        ExitCode::from(self.code)
    }
}

impl From<HostlerError> for Failure {
    fn from(error: HostlerError) -> Failure {
        Failure {
            code: device_code(error.code),
            error,
        }
    }
}

/// A person answered the prompt with something else: nothing changed, exit 1 (P2's project
/// approve does the same). The recovery is the same command, so the operator can run it.
fn declined(message: &str, recovery: String) -> Failure {
    Failure {
        code: 1,
        error: HostlerError {
            code: ErrorCode::BadRequest,
            message: message.to_owned(),
            details: json!({}),
            recovery: Some(recovery),
        },
    }
}

/// A refusal with an explicit exit code and recovery (the service's texts).
fn refused(message: String, recovery: Option<String>) -> Failure {
    Failure {
        code: 2,
        error: HostlerError {
            code: ErrorCode::BadRequest,
            message,
            details: json!({}),
            recovery,
        },
    }
}

fn internal(message: String) -> Failure {
    Failure::from(HostlerError {
        code: ErrorCode::Internal,
        message,
        details: json!({}),
        recovery: None,
    })
}

/// L40: a device command's refusals exit 78, a usage problem 2, everything else 1.
fn device_code(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::BadRequest => 2,
        ErrorCode::DeviceNotFound
        | ErrorCode::AliasTaken
        | ErrorCode::DeviceNotRegistered
        | ErrorCode::DeviceNotReady
        | ErrorCode::BootFailed
        | ErrorCode::ToolMissing
        | ErrorCode::PermissionDenied
        | ErrorCode::NotInteractive
        | ErrorCode::ResourceInUse
        | ErrorCode::ClaimNotHeld
        | ErrorCode::UnknownResource
        | ErrorCode::NotInProject
        | ErrorCode::ServiceUnavailable
        | ErrorCode::ContractMismatch => 78,
        _ => 1,
    }
}

fn emit<T: Serialize>(result: Result<T, Failure>, json: bool, human: impl FnOnce(&T)) -> ExitCode {
    match result {
        Ok(data) => {
            if json {
                print_envelope(&data);
            } else {
                human(&data);
            }
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(json),
    }
}

fn print_envelope<T: Serialize>(data: &T) {
    let envelope = Envelope {
        ok: true,
        data: Some(data),
        error: None,
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("an envelope always encodes")
    );
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, HostlerError> {
    serde_json::from_value(value).map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("unreadable answer from the service: {e}"),
        details: json!({}),
        recovery: None,
    })
}

async fn connect() -> Result<Client, Failure> {
    let paths = Paths::resolve().map_err(|e| internal(format!("{e:#}")))?;
    Client::connect_or_start(&paths, &OsManager)
        .await
        .map_err(Failure::from)
}

// ---------------------------------------------------------------------------------------------
// The commands

async fn list(all: bool, json: bool) -> ExitCode {
    let result: Result<(DevicesSnapshot, Option<ClaimsSnapshot>), Failure> = async {
        if json {
            // The contract's snapshot for exactly the `all` asked for.
            let snapshot = output::call(Request::DevicesList { all }).await?;
            return Ok((snapshot, None));
        }
        // The human view asks for everything: it filters, and it counts the transports it hides.
        let snapshot = output::call(Request::DevicesList { all: true }).await?;
        let claims = output::call(Request::ClaimsSnapshot).await?;
        Ok((snapshot, Some(claims)))
    }
    .await;
    match result {
        Ok((snapshot, None)) => {
            print_envelope(&snapshot);
            ExitCode::SUCCESS
        }
        Ok((snapshot, Some(claims))) => {
            print_list(&snapshot, &claims, all);
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(json),
    }
}

async fn show(device: String, json: bool) -> ExitCode {
    let result: Result<DeviceDetail, Failure> = async {
        output::call(Request::DevicesShow {
            device,
            fresh_sessions: false,
        })
        .await
        .map_err(Failure::from)
    }
    .await;
    emit(result, json, print_show)
}

async fn add(device: String, alias: Option<String>, json: bool) -> ExitCode {
    let command = match &alias {
        Some(alias) => format!("devices add {device} --alias {alias}"),
        None => format!("devices add {device}"),
    };
    let result: Result<DeviceInfo, Failure> = async {
        let detail: DeviceDetail = output::call(Request::DevicesShow {
            device: device.clone(),
            fresh_sessions: false,
        })
        .await?;
        let person = match detail.info.kind {
            DeviceKind::Physical => {
                let mut tty = Tty::open(&command)?;
                print_person_device(&detail.info);
                let model = person_model(&detail.info);
                let prompt = format!("Type the model ({model}) to register it:");
                if !tty.confirm_typed(&prompt, &model)? {
                    return Err(declined(
                        "the model did not match; the device is not registered",
                        format!("hostler {command}"),
                    ));
                }
                true
            }
            _ => false,
        };
        Ok(output::call(Request::DevicesAdd {
            device,
            alias,
            person,
        })
        .await?)
    }
    .await;
    emit(result, json, |info| {
        println!(
            "[hostler] Registered {} as {}.",
            model_of(info),
            display_of(info)
        )
    })
}

async fn rename(device: String, alias: String, json: bool) -> ExitCode {
    let command = format!("devices rename {device} {alias}");
    let result: Result<DeviceInfo, Failure> = async {
        let detail: DeviceDetail = output::call(Request::DevicesShow {
            device: device.clone(),
            fresh_sessions: false,
        })
        .await?;
        let person = match detail.info.kind {
            DeviceKind::Physical => {
                let mut tty = Tty::open(&command)?;
                print_person_device(&detail.info);
                let model = person_model(&detail.info);
                let prompt = format!("Type the model ({model}) to rename it:");
                if !tty.confirm_typed(&prompt, &model)? {
                    return Err(declined(
                        "the model did not match; the device is not renamed",
                        format!("hostler {command}"),
                    ));
                }
                true
            }
            _ => false,
        };
        Ok(output::call(Request::DevicesRename {
            device,
            alias,
            person,
        })
        .await?)
    }
    .await;
    emit(result, json, |info| {
        println!("[hostler] {} is now {}.", model_of(info), display_of(info))
    })
}

async fn note(device: String, text: Option<String>, clear: bool, json: bool) -> ExitCode {
    let result: Result<DeviceInfo, Failure> = async {
        let note = if clear { None } else { text };
        Ok(output::call(Request::DevicesNote { device, note }).await?)
    }
    .await;
    emit(result, json, |info| {
        println!(
            "[hostler] Note {} on {}.",
            if info.note.is_some() {
                "set"
            } else {
                "cleared"
            },
            display_of(info)
        )
    })
}

async fn remove(device: String, json: bool) -> ExitCode {
    let shown = device.clone();
    let command = format!("devices remove {device}");
    let result: Result<(String, Value), Failure> = async {
        let detail: DeviceDetail = output::call(Request::DevicesShow {
            device: device.clone(),
            fresh_sessions: false,
        })
        .await?;
        let person = match detail.info.kind {
            DeviceKind::Physical => {
                let mut tty = Tty::open(&command)?;
                print_person_device(&detail.info);
                let model = person_model(&detail.info);
                let prompt = format!("Type the model ({model}) to remove it:");
                if !tty.confirm_typed(&prompt, &model)? {
                    return Err(declined(
                        "the model did not match; the device is not removed",
                        format!("hostler {command}"),
                    ));
                }
                true
            }
            _ => false,
        };
        let alias = detail.info.alias.clone().unwrap_or_else(|| shown.clone());
        let answer = output::call(Request::DevicesRemove { device, person }).await?;
        Ok((alias, answer))
    }
    .await;
    match result {
        Ok((alias, answer)) => {
            if json {
                print_envelope(&answer);
            } else {
                println!(
                    "[hostler] Removed {alias}. Nothing on the device changed; it stays on the \
                     machine."
                );
            }
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(json),
    }
}

async fn reserve(device: String, json: bool) -> ExitCode {
    let result: Result<Claim, Failure> = async {
        let (mut client, workspace) = workspace_and_client().await?;
        let info: DeviceInfo = decode(client.call(Request::DevicesResolve { device }).await?)?;
        let claim: Claim = decode(
            client
                .call(Request::ClaimsReserve {
                    resource: info.resource,
                    workspace_id: workspace.id,
                })
                .await?,
        )?;
        Ok(claim)
    }
    .await;
    emit(result, json, |claim| {
        println!(
            "[hostler] Reserved {} for {}.",
            sanitize(&claim.resource.display),
            sanitize(&claim.workspace.path)
        )
    })
}

async fn unreserve(device: String, json: bool) -> ExitCode {
    let result: Result<DeviceInfo, Failure> = async {
        let (mut client, workspace) = workspace_and_client().await?;
        let info: DeviceInfo = decode(client.call(Request::DevicesResolve { device }).await?)?;
        let _: () = decode(
            client
                .call(Request::ClaimsUnreserve {
                    resource: info.resource.clone(),
                    workspace_id: workspace.id,
                })
                .await?,
        )?;
        Ok(info)
    }
    .await;
    match result {
        Ok(info) => {
            if json {
                // `claims.unreserve` answers nothing; the envelope wraps that answer.
                print_envelope(&());
            } else {
                println!(
                    "[hostler] Released the reservation of {}.",
                    display_of(&info)
                );
            }
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(json),
    }
}

async fn installs(
    device: Option<String>,
    since: Option<Duration>,
    limit: Option<u32>,
    json: bool,
) -> ExitCode {
    let result: Result<Vec<InstallEvent>, Failure> = async {
        let since_ms = since_ms(SystemClock.now_ms(), since);
        let query = HistoryQuery {
            since_ms,
            limit: limit.unwrap_or(DEFAULT_HISTORY_LIMIT),
        };
        Ok(output::call(Request::DevicesInstalls { device, query }).await?)
    }
    .await;
    emit(result, json, |events| print_installs(events))
}

async fn forget_sessions(alias: String, json: bool) -> ExitCode {
    let result: Result<(ForgetRecord, String), Failure> = async {
        // Person-only: refuse before touching the device or the service (P2's project approve).
        let mut tty = Tty::open(&format!("devices forget-sessions {alias}"))?;
        let detail: DeviceDetail = output::call(Request::DevicesShow {
            device: alias.clone(),
            fresh_sessions: false,
        })
        .await?;
        let snapshot: DevicesSnapshot = output::call(Request::DevicesList { all: true }).await?;
        let tools = &snapshot.tools;
        // The service's own refusals, before a prompt nobody should answer.
        if tools.sessions_reason.is_none() {
            return Err(refused(
                "nothing to forget: agent-device status reads fine".into(),
                Some("hostler devices list".into()),
            ));
        }
        if !detail.info.checking_sessions && detail.info.sessions.is_empty() {
            return Err(refused(
                format!("nothing to forget: {alias} is not closed"),
                Some("hostler devices list".into()),
            ));
        }
        let resolved = detail.info.alias.clone().unwrap_or_else(|| alias.clone());
        print_forget_preview(&detail.info, tools);
        let prompt =
            format!("Type the alias ({resolved}) to release it from the agent-device wait:");
        if !tty.confirm_typed(&prompt, &resolved)? {
            return Err(declined(
                "the alias did not match; nothing was forgotten",
                format!("hostler devices forget-sessions {resolved}"),
            ));
        }
        let by_workspace_path = current_workspace_path();
        let record: ForgetRecord = output::call(Request::DevicesForgetSessions {
            device: alias,
            person: true,
            by_workspace_path,
            by_pid: Some(std::process::id()),
        })
        .await?;
        Ok((record, resolved))
    }
    .await;
    emit(result, json, |(record, alias)| {
        println!(
            "[hostler] Forgot {} session(s) on {alias}; stops nothing: if a session still runs on \
             {alias}, agent-device will refuse the next one.",
            record.sessions.len()
        )
    })
}

async fn tools(
    detect: bool,
    adb: Option<String>,
    agent_device: Option<String>,
    node: Option<String>,
    json: bool,
) -> ExitCode {
    let result: Result<ToolStatus, Failure> = async {
        // A read: never rewrite the settings (P3bT6-3).
        if !detect && adb.is_none() && agent_device.is_none() && node.is_none() {
            let snapshot: DevicesSnapshot =
                output::call(Request::DevicesList { all: false }).await?;
            return Ok(snapshot.tools);
        }
        let registry = if detect || agent_device.is_some() {
            let workspaces: Vec<WorkspaceInfo> = output::call(Request::WorkspaceList).await?;
            workspaces
                .iter()
                .map(|w| (w.id.clone(), w.path.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut set = ToolPathsSet::default();
        if detect {
            let path_var = std::env::var_os("PATH").unwrap_or_default();
            let env = android_env();
            let (detected, skipped) = detect_set(&path_var, &registry, env.clone());
            for dir in skipped {
                eprintln!(
                    "[hostler] skipped {dir} (project-local): prefers global installs",
                    dir = sanitize(&dir)
                );
            }
            for (name, value) in &env {
                if !value.is_empty()
                    && !detected.android_env.contains_key(name)
                    && crate::devices::service::path_valued_env(name)
                {
                    eprintln!(
                        "[hostler] ignored {name}={} (not an absolute path)",
                        sanitize(value)
                    );
                }
            }
            set = detected;
            // A launcher without a resolvable script must not leave the old script in force.
            if set.agent_device.is_some() && set.detected_script.is_none() {
                set.detected_script = Some(String::new());
            }
        }
        if let Some(path) = adb {
            set.adb = Some(absolute_flag(&path)?);
        }
        if let Some(path) = node {
            set.node = Some(absolute_flag(&path)?);
        }
        if let Some(path) = agent_device {
            let absolute = absolute_flag(&path)?;
            let Some(script) = resolve_agent_device(Path::new(&absolute)) else {
                return Err(refused(
                    format!("{absolute} does not lead to agent-device's package script"),
                    Some(AGENT_DEVICE_FIX.into()),
                ));
            };
            if path_is_project_local(Path::new(&absolute), &registry) {
                eprintln!("[hostler] warning: this path goes away with its checkout");
            }
            set.detected_script = Some(script.to_string_lossy().into_owned());
            set.agent_device = Some(absolute);
            set.explicit_agent_device = true;
        }
        if detect {
            for (label, value) in [
                ("agent-device", set.agent_device.as_deref()),
                (
                    "script",
                    set.detected_script.as_deref().filter(|s| !s.is_empty()),
                ),
                ("node", set.node.as_deref()),
            ] {
                if let Some(value) = value {
                    eprintln!("[hostler] recorded {label}: {}", sanitize(value));
                }
            }
            for (name, value) in &set.android_env {
                if !value.is_empty() {
                    eprintln!("[hostler] recorded {name}: {}", sanitize(value));
                }
            }
        }
        Ok(output::call(Request::DevicesTools(set)).await?)
    }
    .await;
    emit(result, json, print_tools)
}

/// The recovery Task 2's `ToolMissing` uses for agent-device.
const AGENT_DEVICE_FIX: &str = "install it globally (npm i -g agent-device@0.21.36), or point at \
a project's copy: hostler devices tools --agent-device \
<checkout>/node_modules/agent-device/bin/agent-device.mjs";

/// A flag path as the person's shell means it: relative to the current directory.
fn absolute_flag(value: &str) -> Result<String, Failure> {
    std::path::absolute(value)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| internal(format!("cannot make {value} absolute: {e}")))
}

/// A path a checkout owns: inside a registered workspace or under `node_modules`.
fn path_is_project_local(path: &Path, registry: &[(String, String)]) -> bool {
    let text = path.to_string_lossy();
    if text
        .split(['/', '\\'])
        .any(|component| component == "node_modules")
    {
        return true;
    }
    let canonical = std::fs::canonicalize(path)
        .map(|p| crate::project::display_path(&p))
        .unwrap_or_else(|_| text.into_owned());
    crate::claims::workspace_match::owning_workspace(registry, &canonical, cfg!(windows)).is_some()
}

/// The current checkout's workspace path, for the forget record's "who" (L54). No project, no
/// path; `forget-sessions` is not refused outside one.
fn current_workspace_path() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let current = crate::project::current(&cwd).ok()?;
    Some(crate::project::display_path(
        &current.checkout.workspace_root,
    ))
}

/// `devices tools --detect`'s work, testable without a service: PATH entries inside a registered
/// workspace are dropped (the T2 review's Task 6 duty), then Task 2 finds the first global
/// launcher and Node. `android_env` is recorded as `devices.env.*` (P2-18): empty values clear a
/// setting, and a path-valued variable that is not absolute is dropped.
pub fn detect_set(
    path_var: &OsStr,
    workspaces: &[(String, String)],
    android_env: BTreeMap<String, String>,
) -> (ToolPathsSet, Vec<String>) {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for dir in std::env::split_paths(path_var) {
        if dir.is_absolute() {
            let shown = dir.to_string_lossy().into_owned();
            if dir.ends_with(Path::new("node_modules").join(".bin")) {
                skipped.push(shown);
                continue;
            }
            let canonical = std::fs::canonicalize(&dir)
                .map(|p| crate::project::display_path(&p))
                .unwrap_or_else(|_| shown.clone());
            if crate::claims::workspace_match::owning_workspace(
                workspaces,
                &canonical,
                cfg!(windows),
            )
            .is_some()
            {
                skipped.push(shown);
                continue;
            }
        }
        kept.push(dir);
    }
    let filtered = std::env::join_paths(kept).unwrap_or_else(|_| path_var.to_os_string());
    let mut set = detect_from_path(&filtered);
    set.android_env = android_env
        .into_iter()
        .filter(|(name, value)| {
            value.is_empty()
                || !crate::devices::service::path_valued_env(name)
                || Path::new(value).is_absolute()
        })
        .collect();
    (set, skipped)
}

/// The caller's `ANDROID_*` values that `--detect` records (P2-18). Every name is sent, so an
/// unset one clears the earlier setting.
fn android_env() -> BTreeMap<String, String> {
    RECORDED_ENV
        .iter()
        .map(|name| ((*name).to_owned(), std::env::var(name).unwrap_or_default()))
        .collect()
}

async fn workspace_and_client() -> Result<(Client, WorkspaceInfo), Failure> {
    let cwd = std::env::current_dir()
        .map_err(|e| internal(format!("cannot read the current directory: {e}")))?;
    let current = crate::project::current(&cwd)?;
    let mut client = connect().await?;
    let workspace = crate::cli::workspace::resolve(&mut client, &current).await?;
    Ok((client, workspace))
}

// ---------------------------------------------------------------------------------------------
// The device lines (also `hostler status`'s, under Resources)

pub(crate) fn device_status_line(
    info: &DeviceInfo,
    state: Option<&ResourceState>,
    now: i64,
    tools: &ToolStatus,
) -> String {
    format!(
        "{}: {}; {}; {}",
        display_of(info),
        connection_text(info),
        telemetry_text_inner(info.telemetry.as_ref(), false),
        standing_text(info, state, now, tools)
    )
}

fn list_line(
    info: &DeviceInfo,
    state: Option<&ResourceState>,
    now: i64,
    tools: &ToolStatus,
) -> String {
    format!(
        "{} — {} ({}): {}; {}; {}",
        display_of(info),
        model_of(info),
        platform_kind(info),
        connection_text(info),
        telemetry_text(info.telemetry.as_ref()),
        standing_text(info, state, now, tools)
    )
}

fn new_line(info: &DeviceInfo) -> String {
    let name = match info.kind {
        DeviceKind::Physical => transport_name(info),
        _ => info.hardware_id.clone(),
    };
    let recovery = match info.kind {
        DeviceKind::Physical => format!(
            "new: a person registers it with hostler devices add {}",
            sanitize(&name)
        ),
        _ => format!("new: hostler devices add {}", sanitize(&name)),
    };
    format!(
        "{} — {} ({}): {}; {}",
        sanitize(&name),
        model_of(info),
        platform_kind(info),
        connection_text(info),
        recovery
    )
}

fn unidentified_line(transport: &UnidentifiedTransport) -> String {
    let model = transport
        .model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .map(|m| format!(" — {}", sanitize(m)))
        .unwrap_or_default();
    let hint = transport
        .hint
        .as_deref()
        .map(|h| format!("; {}", sanitize(h)))
        .unwrap_or_default();
    format!(
        "{} {} {}{model} ({}){hint}",
        platform_word(transport.platform),
        transport_kind_word(transport.transport.kind),
        sanitize(&transport.transport.address),
        sanitize(&transport.transport.state)
    )
}

fn print_list(snapshot: &DevicesSnapshot, claims: &ClaimsSnapshot, all: bool) {
    let now = SystemClock.now_ms();
    let state = |r: &ResourceId| claims.resources.iter().find(|s| s.resource.id == *r);
    let mut registered: Vec<&DeviceInfo> =
        snapshot.devices.iter().filter(|d| d.registered).collect();
    registered.sort_by_key(|a| display_of(a));
    for info in registered {
        println!(
            "{}",
            list_line(info, state(&info.resource), now, &snapshot.tools)
        );
    }
    let mut unregistered: Vec<&DeviceInfo> =
        snapshot.devices.iter().filter(|d| !d.registered).collect();
    unregistered.sort_by_key(|a| display_of(a));
    for info in unregistered {
        // A new physical device is always shown; an unregistered virtual one only with --all.
        if info.kind == DeviceKind::Physical || all {
            println!("{}", new_line(info));
        }
    }
    if all {
        for transport in &snapshot.unidentified {
            println!("{}", unidentified_line(transport));
        }
    } else if !snapshot.unidentified.is_empty() {
        let n = snapshot.unidentified.len();
        println!(
            "{n} transport{} cannot be identified: hostler devices list --all",
            if n == 1 { "" } else { "s" }
        );
    }
    print_problems(snapshot);
}

/// Tool problems and the sessions that map to no device, each once (9b copies the unmapped ones
/// into `tools.problems` too).
pub(crate) fn print_problems(snapshot: &DevicesSnapshot) {
    for line in problem_lines(snapshot) {
        println!("{line}");
    }
}

/// The problem lines, each once: 9b copies the unmapped sessions into `tools.problems` too.
fn problem_lines(snapshot: &DevicesSnapshot) -> Vec<String> {
    let mut lines: Vec<String> = snapshot
        .unmapped_sessions
        .iter()
        .map(|line| format!("problem: {}", sanitize(line)))
        .collect();
    for problem in &snapshot.tools.problems {
        if snapshot
            .unmapped_sessions
            .iter()
            .any(|line| line == problem)
        {
            continue;
        }
        lines.push(format!("problem: {}", sanitize(problem)));
    }
    lines
}

fn print_show(detail: &DeviceDetail) {
    let info = &detail.info;
    let now = SystemClock.now_ms();
    println!(
        "{} — {} ({})",
        display_of(info),
        model_of(info),
        platform_kind(info)
    );
    println!("  id: {}", sanitize(&info.resource.0));
    if let Some(os) = &info.os_version {
        println!("  os: {}", sanitize(os));
    }
    println!("  transports: {}", transports_text(info));
    println!(
        "  state: {}, {}",
        connection_text(info),
        if info.ready { "ready" } else { "not ready" }
    );
    println!("  telemetry: {}", telemetry_text(info.telemetry.as_ref()));
    println!(
        "  registered: {}",
        if !info.registered {
            "no".to_owned()
        } else if info.alias_by_person {
            "yes (by a person)".to_owned()
        } else {
            "yes (by an agent)".to_owned()
        }
    );
    println!(
        "  first seen: {}; last seen: {}",
        seen_text(now, info.first_seen_ms),
        seen_text(now, info.last_seen_ms)
    );
    if let Some(note) = &info.note {
        println!("  note: {}", sanitize(note));
    }
    if info.booted_by_hostler {
        println!("  hostler booted it; it shuts down 2 minutes after its last claim.");
    }
    if info.shutting_down {
        println!("  shutting down");
    }
    if info.removing {
        println!("  being removed");
    }
    if info.checking_sessions {
        println!("  checking for agent-device sessions");
    }
    if !info.sessions.is_empty() {
        println!("  sessions:");
        for session in &info.sessions {
            println!("    {}", session_text(session, now, None));
        }
    }
    if !detail.forgets.is_empty() {
        println!("  forget-sessions:");
        for forget in &detail.forgets {
            println!("    {}", forget_line(forget, now));
        }
    }
    match detail.snapshot_at_ms {
        Some(at) => println!("  apps (snapshot {} ago):", age_text(now, at)),
        None => println!("  apps: no snapshot yet"),
    }
    for app in &detail.apps {
        println!("    {}", app_line(app, now));
    }
}

fn print_installs(events: &[InstallEvent]) {
    if events.is_empty() {
        println!("no install changes");
        return;
    }
    let now = SystemClock.now_ms();
    for event in events {
        println!("{}", install_line(event, now));
    }
}

fn print_tools(status: &ToolStatus) {
    let now = SystemClock.now_ms();
    let adb = status
        .adb
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| "not found".to_owned());
    println!("adb: {adb}");
    if let AdbState::Foreign { version } = status.adb_state {
        println!("  another adb (version {version}) owns the adb server");
    }
    println!(
        "emulator: {}",
        status
            .emulator
            .as_deref()
            .map(sanitize)
            .unwrap_or_else(|| "not found".to_owned())
    );
    println!(
        "xcrun: {}",
        if status.xcrun { "found" } else { "not found" }
    );
    println!(
        "ideviceinfo: {}",
        status
            .ideviceinfo
            .as_deref()
            .map(sanitize)
            .unwrap_or_else(|| "not found".to_owned())
    );
    println!(
        "agent-device: {}",
        match (&status.agent_device, &status.node) {
            (Some(version), Some(node)) => {
                format!("{} (node {})", sanitize(version), sanitize(node))
            }
            (Some(version), None) => sanitize(version),
            (None, Some(node)) => {
                format!("found (version unknown) (node {})", sanitize(node))
            }
            (None, None) => "not found".to_owned(),
        }
    );
    if status.agent_device_in_use {
        println!("  agent-device is in use on this machine");
    }
    let sessions = if let Some(reason) = &status.sessions_reason {
        format!("unknown ({})", sanitize(reason))
    } else if !status.sessions_known {
        "unknown".to_owned()
    } else {
        match status.sessions_last_good_ms {
            Some(at) => format!("known (last read {} ago)", age_text(now, at)),
            None => "known".to_owned(),
        }
    };
    println!("sessions: {sessions}");
    for problem in &status.problems {
        println!("problem: {}", sanitize(problem));
    }
}

fn print_person_device(info: &DeviceInfo) {
    eprintln!("{}", model_of(info));
    eprintln!("  transport: {}", transports_text(info));
    eprintln!(
        "  os: {}",
        info.os_version
            .as_deref()
            .map(sanitize)
            .unwrap_or_else(|| "unknown".to_owned())
    );
}

fn print_forget_preview(info: &DeviceInfo, tools: &ToolStatus) {
    let now = SystemClock.now_ms();
    if info.checking_sessions {
        let reason = tools
            .sessions_reason
            .as_deref()
            .unwrap_or("its status cannot be read");
        eprintln!(
            "closed until agent-device can be read ({})",
            sanitize(reason)
        );
        match tools.sessions_last_good_ms {
            Some(at) => eprintln!("  last good read {} ago", age_text(now, at)),
            None => eprintln!("  no good read since the service started"),
        }
    }
    for session in &info.sessions {
        if session.kept {
            eprintln!("  {}", session_text(session, now, Some(tools)));
        } else {
            eprintln!(
                "  {} (seen {} ago)",
                session_text(session, now, Some(tools)),
                age_text(now, session.seen_since_ms)
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared formatting

fn display_of(info: &DeviceInfo) -> String {
    let name = info
        .alias
        .as_deref()
        .filter(|a| !a.is_empty())
        .unwrap_or(&info.hardware_id);
    sanitize(name)
}

fn model_of(info: &DeviceInfo) -> String {
    let model = info.model.trim();
    if model.is_empty() {
        display_of(info)
    } else {
        sanitize(model)
    }
}

/// The name the person types at the prompt: the sanitized model, as L49 says. Control characters
/// a device reported never reach the terminal, and the person can type what they see.
fn person_model(info: &DeviceInfo) -> String {
    let model = info.model.trim();
    if model.is_empty() {
        "device".to_owned()
    } else {
        sanitize(model)
    }
}

fn platform_word(platform: Platform) -> &'static str {
    match platform {
        Platform::Android => "android",
        Platform::Ios => "ios",
    }
}

fn platform_kind(info: &DeviceInfo) -> &'static str {
    match (info.platform, info.kind) {
        (Platform::Android, DeviceKind::Physical) => "android physical",
        (Platform::Android, DeviceKind::Emulator) => "android emulator",
        (Platform::Ios, DeviceKind::Physical) => "ios physical",
        (Platform::Ios, DeviceKind::Simulator) => "ios simulator",
        (Platform::Android, DeviceKind::Simulator) => "android simulator",
        (Platform::Ios, DeviceKind::Emulator) => "ios emulator",
    }
}

fn transport_kind_word(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Usb => "usb",
        TransportKind::Wifi => "wifi",
        TransportKind::Emulator => "emulator",
        TransportKind::Simulator => "simulator",
        TransportKind::Wired => "wired",
        TransportKind::Network => "network",
    }
}

fn transport_text(transport: &Transport) -> String {
    format!(
        "{} {} ({})",
        transport_kind_word(transport.kind),
        sanitize(&transport.address),
        sanitize(&transport.state)
    )
}

fn transports_text(info: &DeviceInfo) -> String {
    if info.transports.is_empty() {
        return "none".to_owned();
    }
    info.transports
        .iter()
        .map(transport_text)
        .collect::<Vec<_>>()
        .join("; ")
}

fn transport_name(info: &DeviceInfo) -> String {
    info.transports
        .first()
        .map(|t| t.address.clone())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| info.hardware_id.clone())
}

fn connection_text(info: &DeviceInfo) -> &'static str {
    if info.shutting_down {
        return "shutting down";
    }
    if info.removing {
        return "being removed";
    }
    if !info.connected {
        return "not connected";
    }
    match info.kind {
        DeviceKind::Physical => info
            .transports
            .first()
            .map_or("not connected", |t| transport_kind_word(t.kind)),
        DeviceKind::Simulator => {
            if info.ready {
                "booted"
            } else {
                "shut down"
            }
        }
        DeviceKind::Emulator => {
            if info.ready {
                "booted"
            } else if info.transports.is_empty() {
                "shut down"
            } else {
                "booting"
            }
        }
    }
}

fn telemetry_text(telemetry: Option<&Telemetry>) -> String {
    telemetry_text_inner(telemetry, true)
}

/// `names`: show the Wi-Fi name. `status` says only `wifi`; the name is shown in `devices list`,
/// `devices show` and `--json` (Global Constraints).
fn telemetry_text_inner(telemetry: Option<&Telemetry>, names: bool) -> String {
    let Some(t) = telemetry else {
        return "unknown".to_owned();
    };
    // D11: an emulator reports emulated readings; never show them as real.
    if matches!(t.battery_percent, Reading::Emulated)
        || matches!(t.charging, Reading::Emulated)
        || matches!(t.wifi, Reading::Emulated)
        || matches!(t.cellular_data, Reading::Emulated)
        || matches!(t.vpn, Reading::Emulated)
    {
        return "emulated".to_owned();
    }
    let mut parts = Vec::new();
    let battery = match t.battery_percent {
        Reading::Known(percent) => Some(format!("{percent}%")),
        _ => None,
    };
    let charging = matches!(t.charging, Reading::Known(true));
    match (battery, charging) {
        (Some(percent), true) => parts.push(format!("{percent} charging")),
        (Some(percent), false) => parts.push(percent),
        (None, true) => parts.push("charging".to_owned()),
        (None, false) => {}
    }
    if matches!(t.wifi, Reading::Known(true)) {
        match t.wifi_name.as_deref().filter(|n| !n.is_empty() && names) {
            Some(name) => parts.push(format!("wifi ({})", sanitize(name))),
            None => parts.push("wifi".to_owned()),
        }
    }
    if matches!(t.cellular_data, Reading::Known(true)) {
        parts.push("mobile data".to_owned());
    }
    if matches!(t.vpn, Reading::Known(true)) {
        parts.push("vpn".to_owned());
    }
    if parts.is_empty() {
        "unknown".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn standing_text(
    info: &DeviceInfo,
    state: Option<&ResourceState>,
    now: i64,
    tools: &ToolStatus,
) -> String {
    let mut parts = Vec::new();
    if let Some(state) = state {
        let joined = state
            .claims
            .iter()
            .filter(|c| c.kind != ClaimKind::Reservation)
            .count()
            .saturating_sub(1);
        if let Some(claim) = state
            .claims
            .iter()
            .find(|c| c.kind != ClaimKind::Reservation)
        {
            parts.push(held_part(claim, joined));
        } else if let Some(claim) = state.claims.first() {
            parts.push(format!("reserved by {}", sanitize(&claim.workspace.path)));
        }
        if state.seizing {
            parts.push(match taker_of(state) {
                Some(path) => format!("being taken over by {}", sanitize(path)),
                None => "being taken over".to_owned(),
            });
        }
        if !state.waiting.is_empty() {
            parts.push(format!("{} waiting", state.waiting.len()));
        }
    }
    if info.sessions.is_empty() {
        if info.checking_sessions {
            parts.push(match &tools.sessions_reason {
                Some(reason) => format!(
                    "closed until agent-device can be read ({})",
                    sanitize(reason)
                ),
                None if tools.sessions_known => "checking for agent-device sessions".to_owned(),
                None => "closed until agent-device can be read".to_owned(),
            });
        }
    } else {
        let sessions = info
            .sessions
            .iter()
            .map(|session| session_text(session, now, Some(tools)))
            .collect::<Vec<_>>()
            .join("; ");
        parts.push(format!("testing in progress: {sessions}"));
    }
    if parts.is_empty() {
        "free".to_owned()
    } else {
        parts.join("; ")
    }
}

/// A session's line. `tools` gives the last good read and the unreadable reason; `None` (as in
/// `devices show`) falls back to the session's first-seen time and a generic reason.
fn session_text(
    session: &crate::contract::devices::AgentDeviceSession,
    now: i64,
    tools: Option<&ToolStatus>,
) -> String {
    let workspace = if session.workspace_path.is_empty() {
        "an unknown workspace".to_owned()
    } else {
        sanitize(&session.workspace_path)
    };
    let mut text = format!(
        "agent-device session {} in {workspace}",
        sanitize(&session.session)
    );
    if session.kept {
        let (age, reason) = match tools {
            Some(tools) => (
                tools
                    .sessions_last_good_ms
                    .map(|at| age_text(now, at))
                    .unwrap_or_else(|| age_text(now, session.seen_since_ms)),
                tools
                    .sessions_reason
                    .as_deref()
                    .unwrap_or("its status cannot be read"),
            ),
            None => (
                age_text(now, session.seen_since_ms),
                "its status cannot be read",
            ),
        };
        text.push_str(&format!(
            " (last known {age} ago; agent-device status unreadable: {})",
            sanitize(reason)
        ));
    }
    text
}

fn forget_line(record: &ForgetRecord, now: i64) -> String {
    let mut line = format!(
        "forget-sessions {} ago: {} session(s); {}",
        age_text(now, record.at_ms),
        record.sessions.len(),
        sanitize(&record.reason)
    );
    let who = match (&record.by_workspace_path, record.by_pid) {
        (Some(path), Some(pid)) => Some(format!("{} (PID {pid})", sanitize(path))),
        (Some(path), None) => Some(sanitize(path)),
        (None, Some(pid)) => Some(format!("PID {pid}")),
        (None, None) => None,
    };
    if let Some(who) = who {
        line.push_str(&format!("; by {who}"));
    }
    line
}

fn app_line(app: &crate::contract::devices::AppState, now: i64) -> String {
    let mut text = format!("{} {}", sanitize(&app.app_id), version_text(app));
    let mut times = Vec::new();
    if let Some(installed) = &app.installed {
        times.push(time_label(installed, now, "installed"));
    }
    if let Some(updated) = &app.updated {
        times.push(time_label(updated, now, "updated"));
    }
    if !times.is_empty() {
        text.push_str(&format!(" — {}", times.join("; ")));
    }
    text
}

fn version_text(app: &crate::contract::devices::AppState) -> String {
    match (&app.version_name, &app.version_code) {
        (Some(name), Some(code)) => format!("{} ({})", sanitize(name), sanitize(code)),
        (Some(name), None) => sanitize(name),
        (None, Some(code)) => format!("({})", sanitize(code)),
        (None, None) => String::new(),
    }
}

fn install_line(event: &InstallEvent, now: i64) -> String {
    let device = event
        .alias
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| sanitize(&event.device.0));
    let version = match (&event.version_name, &event.version_code) {
        (Some(name), Some(code)) => format!(" {} ({})", sanitize(name), sanitize(code)),
        (Some(name), None) => format!(" {}", sanitize(name)),
        (None, Some(code)) => format!(" ({})", sanitize(code)),
        (None, None) => String::new(),
    };
    format!(
        "{}: {}{version} on {device} — {}, {}",
        when_text(&event.when, now),
        sanitize(&event.app_id),
        change_word(event.change),
        source_text(&event.source)
    )
}

fn change_word(change: InstallChange) -> &'static str {
    match change {
        InstallChange::Installed => "installed",
        InstallChange::Updated => "updated",
        InstallChange::Removed => "removed",
    }
}

fn source_text(source: &InstallSource) -> String {
    match source {
        InstallSource::Claim {
            workspace_path,
            branch,
            commit,
            command,
            ..
        } => {
            let mut text = format!("from {}", sanitize(workspace_path));
            let mut detail = Vec::new();
            if let Some(branch) = branch {
                detail.push(sanitize(branch));
            }
            if let Some(commit) = commit {
                detail.push(sanitize(commit));
            }
            if let Some(command) = command {
                detail.push(sanitize(command));
            }
            if !detail.is_empty() {
                text.push_str(&format!(" ({})", detail.join(" ")));
            }
            text
        }
        InstallSource::Around {
            claim_ids,
            workspace_path,
        } => format!(
            "around claim {} in {}",
            sanitize(&claim_ids.join(",")),
            sanitize(workspace_path)
        ),
        InstallSource::NotThroughHostler => "not through hostler".to_owned(),
    }
}

/// The labelled time L2 requires: `installed`/`updated` only when the device reported one; iOS
/// changes are `first seen` or `changed between`, never a guessed install time.
fn time_label(time: &InstallTime, now: i64, verb: &str) -> String {
    match time {
        InstallTime::Device { at_ms } => format!("{verb} {} ago", age_text(now, *at_ms)),
        InstallTime::FirstSeen { at_ms } => {
            format!("first seen {} ago", age_text(now, *at_ms))
        }
        InstallTime::ChangedBetween { from_ms, to_ms } => format!(
            "changed between {} and {} ago",
            age_text(now, *from_ms),
            age_text(now, *to_ms)
        ),
    }
}

fn when_text(time: &InstallTime, now: i64) -> String {
    time_label(time, now, "").trim_start().to_owned()
}

fn age_text(now: i64, at: i64) -> String {
    let secs = now.saturating_sub(at).max(0) / 1000;
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3_600 {
        format!("{} min", secs / 60)
    } else if secs < 86_400 {
        format!("{} h", secs / 3_600)
    } else {
        format!("{} d", secs / 86_400)
    }
}

/// A device the poller has not stored yet has no first/last-seen time.
fn seen_text(now: i64, at: i64) -> String {
    if at > 0 {
        format!("{} ago", age_text(now, at))
    } else {
        "just now".to_owned()
    }
}

/// `now - since`, without the overflow a huge `--since` would panic on in debug builds.
fn since_ms(now: i64, since: Option<Duration>) -> Option<i64> {
    since
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .map(|ms| now.saturating_sub(ms))
}

/// `<n>s`, `<n>m`, `<n>h` or `<n>d` (as `queue history --since`).
fn parse_since(s: &str) -> Result<Duration, String> {
    let error = || format!("{s:?} is not a duration like 30m, 3h or 1d");
    let unit = s.chars().last().ok_or_else(error)?;
    let n: u64 = s[..s.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| error())?;
    let secs = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        _ => return Err(error()),
    };
    n.checked_mul(secs)
        .map(Duration::from_secs)
        .ok_or_else(error)
}

/// One device-reported string, never terminal control (L40).
fn sanitize(text: &str) -> String {
    sanitize_for_terminal(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::devices::fixtures;

    #[test]
    fn show_labels_ios_times() {
        let now = SystemClock.now_ms();
        let app = |installed, updated| crate::contract::devices::AppState {
            app_id: "com.example.app".into(),
            version_name: Some("1.2.0".into()),
            version_code: None,
            installed,
            updated,
        };
        let detail = DeviceDetail {
            info: fixtures::info(),
            apps: vec![
                app(
                    Some(InstallTime::FirstSeen {
                        at_ms: now - 7_200_000,
                    }),
                    Some(InstallTime::ChangedBetween {
                        from_ms: now - 7_200_000,
                        to_ms: now - 3_600_000,
                    }),
                ),
                app(
                    Some(InstallTime::Device {
                        at_ms: now - 86_400_000,
                    }),
                    None,
                ),
            ],
            snapshot_at_ms: Some(now - 60_000),
            forgets: Vec::new(),
        };
        let lines: Vec<String> = detail.apps.iter().map(|a| app_line(a, now)).collect();
        assert!(lines[0].contains("first seen 2 h ago"), "{:?}", lines[0]);
        assert!(
            lines[0].contains("changed between 2 h and 1 h ago"),
            "{:?}",
            lines[0]
        );
        assert!(lines[1].contains("installed 1 d ago"), "{:?}", lines[1]);
        assert!(
            !lines[1].contains("updated"),
            "no update was reported: {:?}",
            lines[1]
        );
    }

    #[test]
    fn installs_lists_attribution() {
        let now = SystemClock.now_ms();
        let mut event = fixtures::install_event();
        event.when = InstallTime::Device {
            at_ms: now - 7_200_000,
        };
        let line = install_line(&event, now);
        assert!(line.contains("com.example.app"), "{line}");
        assert!(line.contains("1.2.0 (120)"), "{line}");
        assert!(line.contains("updated"), "{line}");
        assert!(
            line.contains("from /work/app (main abc123 make e2e)"),
            "{line}"
        );
        assert!(line.contains("2 h ago"), "{line}");

        event.source = InstallSource::NotThroughHostler;
        assert!(
            install_line(&event, now).contains("not through hostler"),
            "{}",
            install_line(&event, now)
        );
        event.source = InstallSource::Around {
            claim_ids: vec!["c1".into(), "c2".into()],
            workspace_path: "/work/app".into(),
        };
        assert!(
            install_line(&event, now).contains("around claim c1,c2 in /work/app"),
            "{}",
            install_line(&event, now)
        );
    }

    #[test]
    fn telemetry_is_short_and_never_guessed() {
        let mut telemetry = fixtures::telemetry();
        telemetry.vpn = Reading::Known(false);
        assert_eq!(
            telemetry_text(Some(&telemetry)),
            "86% charging · wifi (lab)"
        );
        telemetry.vpn = Reading::Known(true);
        telemetry.cellular_data = Reading::Known(true);
        assert_eq!(
            telemetry_text(Some(&telemetry)),
            "86% charging · wifi (lab) · mobile data · vpn"
        );
        telemetry.battery_percent = Reading::Emulated;
        assert_eq!(telemetry_text(Some(&telemetry)), "emulated");
        let unknown = Telemetry {
            battery_percent: Reading::Unknown,
            charging: Reading::Unknown,
            wifi: Reading::Unknown,
            wifi_name: None,
            cellular_data: Reading::Unknown,
            vpn: Reading::Unknown,
            at_ms: 0,
        };
        assert_eq!(telemetry_text(Some(&unknown)), "unknown");
        assert_eq!(telemetry_text(None), "unknown");
    }

    #[test]
    fn checking_and_sessions_are_standing_lines() {
        let now = SystemClock.now_ms();
        let mut info = fixtures::info();
        info.sessions.clear();
        info.checking_sessions = true;
        let known = fixtures::snapshot().tools;
        assert_eq!(
            standing_text(&info, None, now, &known),
            "checking for agent-device sessions"
        );
        let mut unknown = known.clone();
        unknown.sessions_known = false;
        assert_eq!(
            standing_text(&info, None, now, &unknown),
            "closed until agent-device can be read"
        );
        info.checking_sessions = false;
        info.sessions = fixtures::info().sessions;
        let line = standing_text(&info, None, now, &known);
        assert!(
            line.starts_with("testing in progress: agent-device session default in /work/app"),
            "{line}"
        );
    }

    #[test]
    fn device_lines_sanitize_another_agents_text() {
        let now = SystemClock.now_ms();
        let mut info = fixtures::info();
        info.model = "Pixel \u{1b}]0;PWNED\u{7}".into();
        info.note = Some("x\u{1b}[2J".into());
        info.sessions[0].workspace_path = "/work/\u{1b}]0;X\u{7}".into();
        let mut claims = ClaimsSnapshot {
            resources: Vec::new(),
        };
        let state = ResourceState {
            resource: crate::contract::claims::fixtures::snapshot().resources[0]
                .resource
                .clone(),
            claims: vec![crate::contract::claims::fixtures::claim()],
            foreign: Vec::new(),
            waiting: Vec::new(),
            seizing: false,
            seizing_by: None,
        };
        claims.resources.push(state.clone());
        let line = list_line(&info, Some(&state), now, &fixtures::snapshot().tools);
        assert!(
            !line.contains('\u{1b}') && !line.contains('\u{7}'),
            "{line:?}"
        );
        assert!(line.contains('\u{FFFD}'), "{line:?}");
    }

    #[test]
    fn person_model_is_sanitized() {
        let mut info = fixtures::info();
        info.model = "Pix\u{1b}]0;PWNED\u{7}el".into();
        let model = person_model(&info);
        assert!(
            !model.contains('\u{1b}') && !model.contains('\u{7}'),
            "{model:?}"
        );
        assert!(model.contains('\u{FFFD}'), "{model:?}");
        info.model = "  ".into();
        assert_eq!(person_model(&info), "device");
    }

    #[test]
    fn kept_session_uses_the_last_good_read_and_the_reason() {
        let now = SystemClock.now_ms();
        let mut info = fixtures::info();
        info.sessions[0].kept = true;
        info.sessions[0].seen_since_ms = now - 7_200_000;
        let mut tools = fixtures::snapshot().tools;
        tools.sessions_last_good_ms = Some(now - 60_000);
        tools.sessions_reason = Some("node not found".into());
        let line = session_text(&info.sessions[0], now, Some(&tools));
        assert!(line.contains("last known 1 min ago"), "{line}");
        assert!(
            line.contains("agent-device status unreadable: node not found"),
            "{line}"
        );
    }

    #[test]
    fn problem_lines_do_not_repeat_an_unmapped_session() {
        let mut snapshot = fixtures::snapshot();
        snapshot.unmapped_sessions = vec!["session s2 maps to no device".into()];
        snapshot.tools.problems = vec![
            "session s2 maps to no device".into(),
            "another adb owns the server".into(),
        ];
        let lines = problem_lines(&snapshot);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], "problem: session s2 maps to no device");
        assert_eq!(lines[1], "problem: another adb owns the server");
    }

    #[test]
    fn forget_line_names_who() {
        let now = SystemClock.now_ms();
        let record = ForgetRecord {
            id: "f1".into(),
            resource: fixtures::resource(),
            sessions: vec!["default".into()],
            by_workspace_path: Some("/work/app".into()),
            by_pid: Some(42),
            reason: "agent-device status unreadable: node not found".into(),
            at_ms: now - 120_000,
        };
        let line = forget_line(&record, now);
        assert!(
            line.starts_with("forget-sessions 2 min ago: 1 session(s)"),
            "{line}"
        );
        assert!(line.contains("; by /work/app (PID 42)"), "{line}");
    }

    #[test]
    fn since_overflow_is_bounded() {
        let now = 1_000;
        assert_eq!(since_ms(now, None), None);
        assert_eq!(since_ms(now, Some(Duration::from_secs(1))), Some(0));
        assert_eq!(since_ms(now, Some(Duration::from_secs(u64::MAX))), None);
    }

    #[test]
    fn relative_flag_paths_are_absolute() {
        let absolute = absolute_flag("tools/adb").unwrap();
        assert!(Path::new(&absolute).is_absolute(), "{absolute}");
    }

    #[test]
    fn unidentified_line_sanitizes_device_text() {
        let transport = UnidentifiedTransport {
            platform: Platform::Android,
            transport: Transport {
                kind: TransportKind::Usb,
                address: "SER\u{1b}]0;X\u{7}".into(),
                state: "unauthorized".into(),
            },
            model: Some("Pix\u{1b}el".into()),
            hint: Some("accept \u{1b}[2Jthe prompt".into()),
        };
        let line = unidentified_line(&transport);
        assert!(
            !line.contains('\u{1b}') && !line.contains('\u{7}'),
            "{line:?}"
        );
        assert!(line.contains('\u{FFFD}'), "{line:?}");
    }
}
