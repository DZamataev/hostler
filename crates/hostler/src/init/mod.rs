//! `hostler init` (plan P4 Task 5): detect the toolchain, propose `hostler.toml`, place the
//! marked hostler block in the agent docs, list the steps, and install or start the service.
//! It previews by default; `--write` writes.
//!
//! Rules this module keeps:
//! - it never rewrites an existing `hostler.toml`, never writes a user-level agent config and
//!   never commits;
//! - it never reads a value out of an env file (`detect` gives names only);
//! - every service action follows the plan's table through Task 4's one mechanism
//!   (probe → `may_stop` → the atomic `service.shutdown`), and an automatic registration never
//!   takes a development build (D-Q2);
//! - a person-only command is printed as a step, never run.

pub mod detect;
pub mod docs;
pub mod propose;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::client::{self, Client, Replace};
use crate::contract::accounts::AccountInfo;
use crate::contract::devices::{DeviceInfo, Platform};
use crate::contract::doctor::Who;
use crate::contract::init::{ConfigState, Detected, InitReport, LineProblem, ServiceAction, Step};
use crate::contract::secrets::{LockState, ProjectInfo, ProjectRef, SecretList, SecretsStatus};
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::paths::{Paths, SocketName};
use crate::project::config::{self, ConfigProblem, ProjectConfig};
use crate::project::{self, config_invalid, display_path};
use crate::service::lifecycle::{
    AutoPurpose, Direction, Probe, StopRefusal, StopRule, replace_direction, this_client,
};
use crate::service::manager::{self, ChangeRefused, OsManager, Registrar, RegistrationState};

/// What `hostler init` was asked to do.
#[derive(Clone, Debug)]
pub struct InitOptions {
    /// Where init runs; the checkout is found from here (`project::find_checkout`).
    pub cwd: PathBuf,
    /// `--write`: write `hostler.toml` (only when absent) and the docs block, and act on the
    /// service. Without it nothing is written, started or stopped.
    pub write: bool,
}

/// What the service half of init runs on, so a test can inject a registrar and a binary.
/// A test seam like `Client::connect_or_start_with`, not an API a command uses.
#[doc(hidden)]
#[derive(Clone)]
pub struct InitRig {
    /// The service's paths; a `sandbox_denied` here means nothing is attempted.
    pub paths: Result<Paths, HostlerError>,
    pub registrar: Arc<dyn Registrar>,
    /// The hostler binary init runs: a dev build is never installed automatically (D-Q2).
    pub exe: PathBuf,
    /// Overrides `is_dev_build(exe)`; `None` asks the real check (tests only).
    pub dev_build: Option<bool>,
    /// The binary the OS registration runs, when a caller knows it; `None` asks the OS.
    pub registered_exe: Option<PathBuf>,
}

/// Runs init for the checkout around `opts.cwd`.
pub async fn run(opts: InitOptions) -> Result<InitReport, HostlerError> {
    let exe = std::env::current_exe().map_err(|e| HostlerError {
        code: ErrorCode::Internal,
        message: format!("cannot find the hostler executable: {e}"),
        details: json!({}),
        recovery: None,
    })?;
    run_with(
        opts,
        InitRig {
            paths: client::resolve_paths(),
            registrar: Arc::new(OsManager),
            exe,
            dev_build: None,
            registered_exe: None,
        },
    )
    .await
}

/// `run`, on an injected rig (tests).
#[doc(hidden)]
pub async fn run_with(opts: InitOptions, rig: InitRig) -> Result<InitReport, HostlerError> {
    let InitOptions { cwd, write } = opts;
    // 1. The checkout and its config, resolved exactly as every other command resolves them:
    //    `project::current` is where the checkout identity is checked to be lossless and where
    //    the reserved `[ranges]` note is printed (plan P4 review R1-2). Outside a checkout it
    //    answers P2's `NotInProject`.
    let (checkout, project_ref, state, config, proposed_toml) = match project::current(&cwd) {
        Ok(current) => {
            let root = current.checkout.workspace_root.clone();
            let (state, config, proposed) = match current.config {
                Some(config) => (ConfigState::Valid, Some(config), None),
                None => {
                    // 2. Detection, then the proposal the run would write.
                    let detected = detect::detect(&root);
                    let proposal = propose::propose(&root, &detected);
                    let rendered = config::render(&proposal);
                    (ConfigState::Absent, Some(proposal), Some(rendered))
                }
            };
            (current.checkout, current.project, state, config, proposed)
        }
        Err(e) if e.code == ErrorCode::ConfigInvalid => {
            // An invalid `hostler.toml` is a report, not a failure. `current` resolved and
            // identity-checked the checkout before it read the config.
            let checkout = project::find_checkout(&cwd)?;
            let project = ProjectRef {
                common_dir: display_path(&checkout.common_dir),
                name: project::default_name(&checkout.common_dir),
            };
            (
                checkout,
                project,
                ConfigState::Invalid {
                    problems: line_problems(&e),
                },
                None,
                None,
            )
        }
        Err(e) => return Err(e),
    };
    let root = checkout.workspace_root;
    let steps_config = config.clone().unwrap_or_default();
    let invalid = matches!(state, ConfigState::Invalid { .. });
    // 2. Detection (the config branch above may already have run it; detection is a read).
    let detected = detect::detect(&root);
    // 4. The docs plan, before anything is written or done: broken markers abort the run with
    //    nothing written.
    let docs_plan = if invalid {
        Vec::new()
    } else {
        let configured = matches!(state, ConfigState::Valid);
        docs::plan(&root, &detected, &steps_config, configured)
            .map_err(|problem| config_invalid(&[problem]))?
    };
    // 5. The service column of the plan's table.
    let service = service_phase(write, &rig).await;
    let ServiceOutcome {
        action,
        step: service_step,
        registered,
        probe: final_probe,
    } = service;
    // 6. What the service can say about the project, for the remaining steps.
    let reads = project_reads(&rig, &project_ref, &steps_config).await;
    // 7. The steps.
    let shell = Shell::current();
    let mut steps = Vec::new();
    if let Some(kind) = service_step {
        steps.push(service_step_of(
            kind,
            &rig.exe,
            registered.as_deref(),
            shell,
        ));
    }
    if matches!(state, ConfigState::Valid)
        && let Some(text) = missing_apps(&steps_config, &detected)
    {
        steps.push(Step {
            id: "init.config.apps".into(),
            who: Who::Agent,
            text,
            command: None,
            argv: None,
        });
    }
    if steps_config.agents.device_rules.is_none() {
        steps.push(Step {
            id: "init.device_rules".into(),
            who: Who::Agent,
            text: DEVICE_RULES_PROMPT.into(),
            command: None,
            argv: None,
        });
    }
    if reads.as_ref().is_some_and(|reads| !reads.approved) {
        steps.push(person_step(
            "init.approve",
            "approve this project so its secrets can be used (the person's command)",
            &["hostler", "project", "approve"],
            shell,
        ));
    }
    if locked_or_missing(final_probe.as_ref()) {
        steps.push(person_step(
            "init.unlock",
            "unlock the secrets database for a person's session (the password is theirs)",
            &["hostler", "unlock"],
            shell,
        ));
    }
    steps.extend(secret_steps(
        &root,
        &detected,
        &steps_config,
        reads.as_ref(),
        shell,
    ));
    steps.extend(account_steps(&steps_config, reads.as_ref(), shell));
    if let Some(platforms) = missing_devices(&steps_config, reads.as_ref()) {
        steps.push(Step {
            id: "init.devices".into(),
            who: Who::Person,
            text: format!(
                "register this project's {} phones with the person: hostler devices add <device>",
                platforms.join(" and ")
            ),
            command: None,
            argv: None,
        });
    }
    steps.push(sandbox_step(socket_of(&rig)));
    steps.push(Step {
        id: "init.commit".into(),
        who: Who::Agent,
        text: "review and commit hostler.toml and the docs block".into(),
        command: None,
        argv: None,
    });
    // 8. Writes (never an existing hostler.toml, never a marker-broken file).
    let mut written = Vec::new();
    if write && !invalid {
        if let Some(toml) = &proposed_toml {
            create_hostler_toml(&root, toml.as_bytes())?;
            written.push("hostler.toml".to_string());
        }
        written
            .extend(docs::apply(&root, &docs_plan).map_err(|problem| config_invalid(&[problem]))?);
    }
    Ok(InitReport {
        root: display_path(&root),
        detected,
        config: state,
        proposed_toml,
        docs: docs_plan.into_iter().map(|entry| entry.change).collect(),
        steps,
        written,
        service: action,
    })
}

/// Creates `<root>/hostler.toml`, exclusively: a file that appeared since the config was read
/// (a person or another agent writing one) is never clobbered (P4 review R2-3).
fn create_hostler_toml(root: &Path, toml: &[u8]) -> Result<(), HostlerError> {
    use std::io::Write;

    let path = root.join("hostler.toml");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(config_invalid(&[ConfigProblem {
                line: 1,
                message: "hostler.toml appeared while init was running; it was not overwritten"
                    .into(),
            }]));
        }
        Err(error) => {
            return Err(internal(format!(
                "cannot write hostler.toml: {}",
                error.kind()
            )));
        }
    };
    file.write_all(toml)
        .map_err(|error| internal(format!("cannot write hostler.toml: {}", error.kind())))
}

/// The problems of the `config_invalid` error `project::current` answered, as report lines.
fn line_problems(e: &HostlerError) -> Vec<LineProblem> {
    let problems: Vec<LineProblem> = e.details["problems"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|problem| {
                    Some(LineProblem {
                        line: usize::try_from(problem["line"].as_u64()?).ok()?,
                        message: problem["message"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if problems.is_empty() {
        // A `config_invalid` without its list (an older service, an odd error): keep the line.
        vec![LineProblem {
            line: 1,
            message: e.message.clone(),
        }]
    } else {
        problems
    }
}

// ---------------------------------------------------------------------------------------------
// The service

/// What the service half decided and did.
struct ServiceOutcome {
    action: ServiceAction,
    /// The `init.service` step, when the agent must act.
    step: Option<ServiceStep>,
    /// The binary the OS registration runs, when a step names it.
    registered: Option<PathBuf>,
    /// The service after the action: the reads and the lock step use it.
    probe: Option<Probe>,
}

/// One run of the plan's service table.
async fn service_phase(write: bool, rig: &InitRig) -> ServiceOutcome {
    let paths = match &rig.paths {
        Ok(paths) => paths.clone(),
        Err(e) => {
            return ServiceOutcome {
                action: ServiceAction::Unreachable {
                    reason: reason_of(e),
                },
                step: None,
                registered: None,
                probe: None,
            };
        }
    };
    let dev_build = rig
        .dev_build
        .unwrap_or_else(|| manager::is_dev_build(&rig.exe));
    let custom_home = paths.custom_home;
    // A custom home has no registration of its own, so the default home's is never read there.
    let registration = if custom_home {
        RegistrationState::Unknown
    } else {
        registration(rig.registrar.clone()).await
    };
    let probe = crate::service::lifecycle::probe(&paths).await;
    let decision = decide(write, custom_home, registration, &probe, dev_build);
    let (action, step) = act(decision, &paths, rig, dev_build).await;
    let registered = match step {
        Some(ServiceStep::NewerBinary | ServiceStep::ServiceBinary) => match &rig.registered_exe {
            Some(registered) => Some(registered.clone()),
            None => registered_exe().await,
        },
        _ => None,
    };
    // The reads judge the service as it is now, after any install or start.
    let probe = Some(crate::service::lifecycle::probe(&paths).await);
    ServiceOutcome {
        action,
        step,
        registered,
        probe,
    }
}

/// The registration the plan's table is read from; a slow or unreadable answer is `Unknown`,
/// which follows the `Current` rows (nothing is re-registered).
async fn registration(registrar: Arc<dyn Registrar>) -> RegistrationState {
    let read = tokio::task::spawn_blocking(move || registrar.registration());
    match tokio::time::timeout(Duration::from_secs(5), read).await {
        Ok(Ok(state)) => state,
        _ => RegistrationState::Unknown,
    }
}

/// The binary the OS registration runs (R1-10), read off this thread and bounded: `schtasks`
/// can hang on Windows.
async fn registered_exe() -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(manager::registered_exe());
    });
    tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .ok()?
        .ok()?
}

/// What the table says to do about the service (pure; unit-tested).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Decision {
    /// A final answer for this run: the preview column, or the `--write` column when it needs
    /// no action.
    Done(ServiceAction, Option<ServiceStep>),
    /// `--write`: start a `Current` registration's stopped service through the manager.
    Start,
    /// `--write`: `connect_or_start(Replace::OnMismatch)`, then the fallback install.
    Replace,
    /// `--write`: `install_service(Automatic(Maintain))`.
    Install,
}

/// The `init.service` step of one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceStep {
    /// A dev build: step 4 never installs it (D-Q2).
    DevInstall,
    /// A typed `hostler service install`.
    Install,
    /// The service is busy; the install waits for the work to finish.
    InstallWhenIdle,
    /// The database is unlocked (or the service cannot say); the install locks it.
    InstallUnlocking,
    /// This client is older: run init with the newer binary, or upgrade.
    NewerBinary,
    /// A version that does not parse: run init with the binary the service runs.
    ServiceBinary,
    /// The start failed: start it again, then doctor.
    Start,
    /// A custom `HOSTLER_HOME`: no OS registration; its clients start its service.
    StartCustomHome,
}

/// What init says about the service of a custom `HOSTLER_HOME` (P4 Task 9 F-3).
const CUSTOM_HOME_SERVICE: &str = "OS service registration only applies to the default hostler \
    home; HOSTLER_HOME is set, so this home's service is started by its clients: hostler service \
    start";

/// The plan's service table (P4 Task 5 step 4), as a pure decision.
fn decide(
    write: bool,
    custom_home: bool,
    registration: RegistrationState,
    probe: &Probe,
    dev_build: bool,
) -> Decision {
    if let Probe::Unreachable { reason } = probe {
        return Decision::Done(
            ServiceAction::Unreachable {
                reason: reason.clone(),
            },
            None,
        );
    }
    if custom_home {
        // The machine's OS registration belongs to the default home: init never installs a
        // custom home's service, and starts none for it either.
        // Its clients start it (P4 Task 9 F-3): the step names `hostler service start`, which
        // runs in the background (and replaces an older service, D-Q1), never the foreground
        // `hostler service run`.
        let refused = ServiceAction::InstallRefused {
            message: CUSTOM_HOME_SERVICE.into(),
        };
        return match probe {
            Probe::Running { contract, .. } if *contract == crate::contract::CONTRACT_VERSION => {
                Decision::Done(ServiceAction::Running, None)
            }
            Probe::Running {
                contract, version, ..
            } => {
                let step = match replace_direction(this_client(), (*contract, version)) {
                    Direction::Older => ServiceStep::NewerBinary,
                    Direction::Unknown => ServiceStep::ServiceBinary,
                    Direction::Newer | Direction::Same => ServiceStep::StartCustomHome,
                };
                Decision::Done(refused, Some(step))
            }
            _ => Decision::Done(refused, Some(ServiceStep::StartCustomHome)),
        };
    }
    match registration {
        RegistrationState::Absent | RegistrationState::Outdated => match probe {
            Probe::Stopped => install_or_preview(write, dev_build),
            Probe::Running {
                contract,
                version,
                secrets_unlocked,
                ..
            } => match replace_direction(this_client(), (*contract, version)) {
                Direction::Newer | Direction::Same if *secrets_unlocked == Some(false) => {
                    install_or_preview(write, dev_build)
                }
                Direction::Newer | Direction::Same => Decision::Done(
                    ServiceAction::InstallRefused {
                        message: "the running hostler service's secrets database is unlocked (or \
                                  it cannot say); installing locks it"
                            .into(),
                    },
                    Some(ServiceStep::InstallUnlocking),
                ),
                Direction::Older => Decision::Done(
                    ServiceAction::InstallRefused {
                        message: older_text(*contract, version),
                    },
                    Some(ServiceStep::NewerBinary),
                ),
                Direction::Unknown => Decision::Done(
                    ServiceAction::InstallRefused {
                        message: unknown_text(version),
                    },
                    Some(ServiceStep::ServiceBinary),
                ),
            },
            Probe::Unreachable { .. } => unreachable!("returned above"),
        },
        // `Current`, and `Unknown` (a registration that could not be read): nothing is
        // re-registered, so these follow the `Current` rows.
        RegistrationState::Current | RegistrationState::Unknown => match probe {
            Probe::Stopped => {
                if write {
                    Decision::Start
                } else {
                    Decision::Done(ServiceAction::WouldStart, None)
                }
            }
            // A matching contract is `running` whatever the binary version: a binary drift is
            // doctor's `service.binary`, and init stops nothing for it.
            Probe::Running { contract, .. } if *contract == crate::contract::CONTRACT_VERSION => {
                Decision::Done(ServiceAction::Running, None)
            }
            Probe::Running {
                contract,
                version,
                secrets_unlocked,
                ..
            } => match replace_direction(this_client(), (*contract, version)) {
                Direction::Newer if *secrets_unlocked == Some(false) => {
                    if write {
                        Decision::Replace
                    } else {
                        Decision::Done(ServiceAction::WouldReplace, None)
                    }
                }
                Direction::Newer => Decision::Done(
                    ServiceAction::MismatchKept {
                        direction: "newer".into(),
                        message: "the running hostler service is older; its secrets database is \
                                  unlocked (or it cannot say), so nothing stops it"
                            .into(),
                    },
                    Some(ServiceStep::InstallUnlocking),
                ),
                Direction::Older => Decision::Done(
                    ServiceAction::MismatchKept {
                        direction: "older".into(),
                        message: older_text(*contract, version),
                    },
                    Some(ServiceStep::NewerBinary),
                ),
                Direction::Unknown | Direction::Same => Decision::Done(
                    ServiceAction::MismatchKept {
                        direction: "unknown".into(),
                        message: unknown_text(version),
                    },
                    Some(ServiceStep::ServiceBinary),
                ),
            },
            Probe::Unreachable { .. } => unreachable!("returned above"),
        },
    }
}

fn install_or_preview(write: bool, dev_build: bool) -> Decision {
    if dev_build {
        // D-Q2: an automatic registration never takes a dev build.
        Decision::Done(
            ServiceAction::WouldInstall { dev_build: true },
            Some(ServiceStep::DevInstall),
        )
    } else if write {
        Decision::Install
    } else {
        Decision::Done(ServiceAction::WouldInstall { dev_build: false }, None)
    }
}

fn older_text(contract: u32, version: &str) -> String {
    let (mine, my_version) = this_client();
    format!(
        "this hostler (contract {mine}, version {my_version}) is older than the running service \
         (contract {contract}, version {version})"
    )
}

fn unknown_text(version: &str) -> String {
    format!(
        "cannot compare hostler {} with the running service's {version}",
        this_client().1
    )
}

/// Carries out a decision: the preview does nothing, `--write` acts through the one mechanism.
async fn act(
    decision: Decision,
    paths: &Paths,
    rig: &InitRig,
    dev_build: bool,
) -> (ServiceAction, Option<ServiceStep>) {
    match decision {
        Decision::Done(action, step) => (action, step),
        Decision::Start => {
            match Client::connect_or_start(paths, &rig.registrar, Replace::Never).await {
                Ok(_) => (ServiceAction::Started, None),
                Err(e) if client::is_sandbox_denied(&e) => (
                    ServiceAction::Unreachable {
                        reason: reason_of(&e),
                    },
                    None,
                ),
                Err(e) => (
                    ServiceAction::StartFailed { message: e.message },
                    Some(ServiceStep::Start),
                ),
            }
        }
        Decision::Replace => {
            match Client::connect_or_start(paths, &rig.registrar, Replace::OnMismatch).await {
                Ok(_) => (ServiceAction::Replaced, None),
                Err(e) if client::is_sandbox_denied(&e) => (
                    ServiceAction::Unreachable {
                        reason: reason_of(&e),
                    },
                    None,
                ),
                Err(e) if e.code == ErrorCode::ContractMismatch => {
                    let direction = e.details["direction"]
                        .as_str()
                        .unwrap_or("newer")
                        .to_string();
                    if !e.details["refusal"].is_null() || !e.details["busy"].is_null() {
                        // `may_stop` or the atomic stop refused: nothing was stopped. A busy
                        // service names its blockers in the message (R1-3).
                        let step = match e.details["refusal"].as_str() {
                            Some("newer_service") => ServiceStep::NewerBinary,
                            Some("unknown_version") => ServiceStep::ServiceBinary,
                            Some("unlocked_or_unknown") => ServiceStep::InstallUnlocking,
                            Some(_) => ServiceStep::Install,
                            None => ServiceStep::InstallWhenIdle,
                        };
                        let message = match busy_note(&e.details) {
                            Some(note) => format!("{}: {note}", e.message),
                            None => e.message,
                        };
                        return (
                            ServiceAction::MismatchKept { direction, message },
                            Some(step),
                        );
                    }
                    if dev_build {
                        // The replacement ran, but the registration runs an older hostler, and
                        // a dev build is never registered automatically (D-Q2).
                        return (
                            ServiceAction::MismatchKept {
                                direction,
                                message: e.message,
                            },
                            Some(ServiceStep::DevInstall),
                        );
                    }
                    // The registered binary is the old one: install this one over the fresh,
                    // idle, locked service.
                    match manager::install_service(
                        paths,
                        &rig.exe,
                        rig.registrar.clone(),
                        StopRule::Automatic(AutoPurpose::Replace),
                    )
                    .await
                    {
                        Ok(_) => (ServiceAction::Installed, None),
                        Err(refused) => refused_outcome(refused),
                    }
                }
                Err(e) => (
                    ServiceAction::StartFailed { message: e.message },
                    Some(ServiceStep::Start),
                ),
            }
        }
        Decision::Install => {
            match manager::install_service(
                paths,
                &rig.exe,
                rig.registrar.clone(),
                StopRule::Automatic(AutoPurpose::Maintain),
            )
            .await
            {
                Ok(_) => (ServiceAction::Installed, None),
                Err(refused) => refused_outcome(refused),
            }
        }
    }
}

/// A refused registration change: the action, and what the agent does about it.
fn refused_outcome(refused: ChangeRefused) -> (ServiceAction, Option<ServiceStep>) {
    match refused {
        ChangeRefused::CustomHome => (
            ServiceAction::InstallRefused {
                message: ChangeRefused::CustomHome.to_string(),
            },
            None,
        ),
        ChangeRefused::Busy(e) => (
            ServiceAction::InstallRefused { message: e.message },
            Some(ServiceStep::InstallWhenIdle),
        ),
        ChangeRefused::NotStopped { reason } => {
            let action = ServiceAction::InstallRefused {
                message: format!("{}; {}", reason.message(), reason.recovery(None)),
            };
            let step = match reason {
                StopRefusal::NewerService => ServiceStep::NewerBinary,
                StopRefusal::UnknownVersion => ServiceStep::ServiceBinary,
                StopRefusal::UnlockedOrUnknown => ServiceStep::InstallUnlocking,
                StopRefusal::NotNewer | StopRefusal::CurrentDefinition => ServiceStep::Install,
                StopRefusal::Unreachable => return (action, None),
            };
            (action, Some(step))
        }
        ChangeRefused::Failed(message) => (ServiceAction::InstallRefused { message }, None),
    }
}

/// The blockers of a `ServiceBusy` answer, as one line (`details.busy`, from the atomic stop).
fn busy_note(details: &serde_json::Value) -> Option<String> {
    let busy = &details["busy"];
    if busy.is_null() {
        return None;
    }
    let count = |name: &str| busy[name].as_array().map(Vec::len).unwrap_or(0);
    Some(format!(
        "{} running heavy task(s), {} live lease(s), {} takeover(s){}",
        count("running"),
        count("leases"),
        count("plans"),
        match busy["secrets"].as_str() {
            Some("unlocked") => ", the secrets database is unlocked",
            _ => "",
        }
    ))
}

fn reason_of(e: &HostlerError) -> String {
    e.details["reason"]
        .as_str()
        .unwrap_or("unreachable")
        .to_string()
}

/// `Some(true)` is unlocked; anything else is locked or missing.
fn locked_or_missing(probe: Option<&Probe>) -> bool {
    match probe {
        Some(Probe::Running {
            secrets_unlocked, ..
        }) => *secrets_unlocked != Some(true),
        Some(Probe::Stopped) => true,
        Some(Probe::Unreachable { .. }) | None => false,
    }
}

// ---------------------------------------------------------------------------------------------
// What the service says about the project

/// The reads the remaining steps need; `None` when no service answered.
struct ProjectReads {
    approved: bool,
    /// Declared secrets the service holds no value for.
    missing_secrets: Vec<String>,
    /// The fields of each declared account, by account name.
    accounts: Vec<AccountInfo>,
    /// Every registered device.
    devices: Vec<DeviceInfo>,
}

async fn project_reads(
    rig: &InitRig,
    project: &ProjectRef,
    config: &ProjectConfig,
) -> Option<ProjectReads> {
    let paths = rig.paths.as_ref().ok()?;
    let mut client = Client::connect(paths).await.ok()?;
    let info = call::<ProjectInfo>(&mut client, Request::ProjectResolve(project.clone())).await?;
    let status = call::<SecretsStatus>(&mut client, Request::SecretsStatus).await?;
    let unlocked = status.state == LockState::Unlocked;
    let mut reads = ProjectReads {
        approved: info.approved,
        missing_secrets: Vec::new(),
        accounts: Vec::new(),
        devices: Vec::new(),
    };
    // `secret.list` and `account.list` need an approved project and an unlocked database.
    if info.approved && unlocked {
        if !config.secrets.is_empty() {
            let list = call::<SecretList>(
                &mut client,
                Request::SecretList {
                    project: project.clone(),
                    declared: config.declared(),
                },
            )
            .await;
            if let Some(list) = list {
                reads.missing_secrets = list
                    .secrets
                    .iter()
                    .filter(|secret| {
                        secret.declared
                            && secret.state == crate::contract::secrets::SecretState::Missing
                    })
                    .map(|secret| secret.name.clone())
                    .collect();
            }
        }
        if !config.accounts.is_empty()
            && let Some(accounts) = call::<Vec<AccountInfo>>(
                &mut client,
                Request::AccountList {
                    project: project.clone(),
                },
            )
            .await
        {
            reads.accounts = accounts;
        }
    }
    if (!config.apps.android.is_empty() || !config.apps.ios.is_empty())
        && let Some(snapshot) = call::<crate::contract::devices::DevicesSnapshot>(
            &mut client,
            Request::DevicesList { all: false },
        )
        .await
    {
        reads.devices = snapshot.devices;
    }
    Some(reads)
}

async fn call<T: serde::de::DeserializeOwned>(client: &mut Client, request: Request) -> Option<T> {
    let value = client.call(request).await.ok()?;
    serde_json::from_value(value).ok()
}

// ---------------------------------------------------------------------------------------------
// The steps

/// The §7.8 prompt, verbatim (plan P4 Task 5).
const DEVICE_RULES_PROMPT: &str = "Propose device-handling instructions for this project from \
    its own context (which apps may be installed or uninstalled on which devices, what must \
    never be done to a device, when to ask the developer), show them to the developer, and only \
    after they agree write them into the project's agent docs under a heading. Then set \
    `[agents] device_rules = \"<file>#<heading>\"` in hostler.toml and run `hostler init \
    --write` again.";

fn person_step(id: &str, text: &str, argv: &[&str], shell: Shell) -> Step {
    let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
    Step {
        id: id.to_string(),
        who: Who::Person,
        text: text.to_string(),
        command: Some(join_argv(&argv, shell)),
        argv: Some(argv),
    }
}

/// The `init.service` step: its text names what it does, and its command is runnable.
fn service_step_of(kind: ServiceStep, exe: &Path, registered: Option<&Path>, shell: Shell) -> Step {
    let exe = display_path(exe);
    let (text, argv): (String, Vec<String>) = match kind {
        ServiceStep::DevInstall => (
            format!(
                "this is a dev build ({exe}); installing it registers it as the machine's service"
            ),
            vec![exe.clone(), "service".into(), "install".into()],
        ),
        ServiceStep::Install => (
            "install this hostler as the machine's service (it locks the secrets database)"
                .to_string(),
            vec!["hostler".into(), "service".into(), "install".into()],
        ),
        ServiceStep::InstallWhenIdle => (
            "when its work has finished: hostler service install (it registers this hostler and \
             restarts the service, which locks the secrets database)"
                .to_string(),
            vec!["hostler".into(), "service".into(), "install".into()],
        ),
        ServiceStep::InstallUnlocking => (
            "hostler service install (it registers this hostler and restarts the service, which \
             locks the secrets database, so only with the person's agreement)"
                .to_string(),
            vec!["hostler".into(), "service".into(), "install".into()],
        ),
        ServiceStep::NewerBinary => match registered {
            Some(registered) => {
                let path = display_path(registered);
                (
                    format!(
                        "run init with the newer hostler ({path}), or upgrade: cargo install \
                         hostler"
                    ),
                    vec![path, "init".into()],
                )
            }
            None => (
                "run init with the newer hostler binary, or upgrade: cargo install hostler".into(),
                vec!["cargo".into(), "install".into(), "hostler".into()],
            ),
        },
        ServiceStep::ServiceBinary => match registered {
            Some(registered) => {
                let path = display_path(registered);
                (
                    format!("run init with the binary the service runs ({path})"),
                    vec![path, "init".into()],
                )
            }
            None => (
                "run init with the hostler binary the service runs".to_string(),
                Vec::new(),
            ),
        },
        ServiceStep::Start => (
            "start the service, then check it: hostler service start; hostler doctor".to_string(),
            vec!["hostler".into(), "service".into(), "start".into()],
        ),
        ServiceStep::StartCustomHome => (
            "HOSTLER_HOME is set, so no OS registration applies: start this home's service in \
             the background, then check it: hostler service start; hostler doctor"
                .to_string(),
            vec!["hostler".into(), "service".into(), "start".into()],
        ),
    };
    Step {
        id: "init.service".into(),
        who: Who::Agent,
        text,
        command: (!argv.is_empty()).then(|| join_argv(&argv, shell)),
        argv: (!argv.is_empty()).then_some(argv),
    }
}

/// An env file's person import, and the agent requests for declared secrets still missing.
fn secret_steps(
    root: &Path,
    detected: &Detected,
    config: &ProjectConfig,
    reads: Option<&ProjectReads>,
    shell: Shell,
) -> Vec<Step> {
    let mut steps = Vec::new();
    for file in &detected.env_files {
        let declared = file
            .names
            .iter()
            .filter(|name| config.secrets.contains_key(*name))
            .count();
        if declared == 0 {
            continue;
        }
        let path = display_path(&root.join(&file.path));
        let argv = vec![
            "hostler".to_string(),
            "secret".to_string(),
            "import".to_string(),
            path.clone(),
        ];
        steps.push(Step {
            id: format!("init.secrets.import.{}", file.path),
            who: Who::Person,
            text: format!(
                "import {}'s {declared} declared values (only the person runs it; the file can be \
                 deleted once imported)",
                file.path
            ),
            command: Some(join_argv(&argv, shell)),
            argv: Some(argv),
        });
    }
    let missing = reads
        .map(|reads| reads.missing_secrets.clone())
        .unwrap_or_default();
    for name in missing {
        let Some(decl) = config.secrets.get(&name) else {
            continue;
        };
        let argv = vec![
            "hostler".to_string(),
            "secret".to_string(),
            "request".to_string(),
            name.clone(),
            "--reason".to_string(),
            decl.purpose.clone(),
        ];
        steps.push(Step {
            id: format!("init.secrets.{name}"),
            who: Who::Agent,
            text: format!(
                "request this project's {name} secret from the person; the reason is its purpose \
                 in hostler.toml"
            ),
            command: Some(join_argv(&argv, shell)),
            argv: Some(argv),
        });
    }
    steps
}

/// A person step per declared account whose fields the service does not hold.
fn account_steps(config: &ProjectConfig, reads: Option<&ProjectReads>, shell: Shell) -> Vec<Step> {
    let Some(reads) = reads else {
        return Vec::new();
    };
    let mut steps = Vec::new();
    for (name, decl) in config.declared_accounts() {
        let held: Vec<&str> = reads
            .accounts
            .iter()
            .find(|account| account.name == name)
            .map(|account| account.fields.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let missing: Vec<&str> = decl
            .fields
            .iter()
            .map(String::as_str)
            .filter(|field| !held.contains(field))
            .collect();
        let Some(first) = missing.first() else {
            continue;
        };
        let argv = vec![
            "hostler".to_string(),
            "account".to_string(),
            "set".to_string(),
            name.to_string(),
            "--field".to_string(),
            (*first).to_string(),
        ];
        steps.push(Step {
            id: format!("init.accounts.{name}"),
            who: Who::Person,
            text: format!(
                "set this project's {name} account (the person runs it): {}",
                missing
                    .iter()
                    .map(|field| format!("--field {field}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            command: Some(join_argv(&argv, shell)),
            argv: Some(argv),
        });
    }
    steps
}

/// The detected app ids the project's `[apps]` does not declare yet (`init.config.apps`): an
/// existing `hostler.toml` is never rewritten, so the agent adds them.
fn missing_apps(config: &ProjectConfig, detected: &Detected) -> Option<String> {
    let mut parts = Vec::new();
    for platform in [Platform::Android, Platform::Ios] {
        let declared = config.app_ids(platform);
        let mut missing: Vec<&str> = Vec::new();
        for app in &detected.apps {
            if app.platform == platform
                && !declared.contains(&app.id)
                && !missing.contains(&app.id.as_str())
            {
                missing.push(&app.id);
            }
        }
        if !missing.is_empty() {
            parts.push(format!(
                "[apps].{}: {}",
                platform_label(platform),
                missing.join(", ")
            ));
        }
    }
    (!parts.is_empty()).then(|| format!("add these app ids under [apps]: {}", parts.join("; ")))
}

/// The platforms this project has apps for and no registered device of.
fn missing_devices(config: &ProjectConfig, reads: Option<&ProjectReads>) -> Option<Vec<String>> {
    let reads = reads?;
    let registered = |platform: Platform| {
        reads
            .devices
            .iter()
            .any(|device| device.registered && device.platform == platform)
    };
    let mut missing = Vec::new();
    if !config.apps.android.is_empty() && !registered(Platform::Android) {
        missing.push(platform_label(Platform::Android));
    }
    if !config.apps.ios.is_empty() && !registered(Platform::Ios) {
        missing.push(platform_label(Platform::Ios));
    }
    (!missing.is_empty()).then_some(missing)
}

fn platform_label(platform: Platform) -> String {
    match platform {
        Platform::Android => "android",
        Platform::Ios => "ios",
    }
    .to_string()
}

/// The person's sandbox step (P4-L9): both clients' settings, and init writes neither.
fn sandbox_step(socket: Option<SocketName>) -> Step {
    let socket = match socket {
        Some(SocketName::File(path)) => display_path(&path),
        Some(SocketName::Namespaced(name)) => name,
        None => "<HOSTLER_HOME>/hostler.sock".into(),
    };
    let socket = serde_json::to_string(&socket).unwrap_or_else(|_| "\"\"".into());
    let rules = include_str!("../guide/codex_hostler.rules");
    let text = format!(
        "An agent sandbox may deny hostler its socket; these are both agent clients' settings, \
         and init writes neither of them (they are the person's).\n\
         \n\
         Codex: save this rule file as ~/.codex/rules/hostler.rules, or as \
         <checkout>/.codex/rules/hostler.rules (a project's .codex/rules is loaded only when \
         Codex trusts it):\n\
         {rules}\n\
         Codex matches every segment of a command, so an agent must call plain `hostler …`, \
         with no VAR=… prefix, no `cd … &&`, no pipe and no `;`, or the command stays sandboxed.\n\
         \n\
         Claude Code: in this project's .claude/settings.json add\n\
         \"sandbox\": {{\"network\": {{\"allowUnixSockets\": [{socket}]}}}}\n\
         and keep the sandbox on otherwise.\n\
         \n\
         More: hostler guide sandbox"
    );
    Step {
        id: "init.sandbox".into(),
        who: Who::Person,
        text,
        command: None,
        argv: None,
    }
}

fn socket_of(rig: &InitRig) -> Option<SocketName> {
    if let Ok(paths) = &rig.paths {
        return Some(paths.socket.clone());
    }
    Paths::planned().ok().map(|paths| paths.socket)
}

// ---------------------------------------------------------------------------------------------
// Printed commands: one quoting function for every path (R1-15)

/// The shell a printed command is typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Posix,
    Windows,
}

impl Shell {
    /// The shell of the machine init runs on.
    pub fn current() -> Shell {
        if cfg!(windows) {
            Shell::Windows
        } else {
            Shell::Posix
        }
    }
}

/// One argument, quoted for `shell`.
///
/// - POSIX (macOS, Linux): single quotes when it holds a character outside
///   `[A-Za-z0-9_./:@%+=,-]`, with `'` written as `'\''`.
/// - Windows: double quotes when it holds a space or one of ``&()[]{}^=;!'+,`~``. A Windows path
///   cannot contain `"`, and the command starts with `hostler`, so PowerShell and `cmd` both read
///   a quoted path as one argument.
pub fn shell_quote(argument: &str, shell: Shell) -> String {
    match shell {
        Shell::Posix => {
            let safe = argument.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'_' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b'=' | b',' | b'-'
                    )
            });
            if safe && !argument.is_empty() {
                argument.to_string()
            } else {
                format!("'{}'", argument.replace('\'', r"'\''"))
            }
        }
        Shell::Windows => {
            let needs = argument.is_empty()
                || argument
                    .chars()
                    .any(|c| c == ' ' || r#"&()[]{}^=;!'+,`~"#.contains(c));
            if needs {
                format!("\"{argument}\"")
            } else {
                argument.to_string()
            }
        }
    }
}

/// A whole command line: every argument quoted by `shell_quote`.
fn join_argv(argv: &[String], shell: Shell) -> String {
    argv.iter()
        .map(|argument| shell_quote(argument, shell))
        .collect::<Vec<_>>()
        .join(" ")
}

fn internal(message: String) -> HostlerError {
    HostlerError {
        code: ErrorCode::Internal,
        message,
        details: json!({}),
        recovery: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::contract::CONTRACT_VERSION;

    use super::*;

    fn running(contract: u32, version: &str, unlocked: Option<bool>) -> Probe {
        Probe::Running {
            contract,
            version: version.into(),
            secrets_unlocked: unlocked,
            pid: 42,
        }
    }

    #[test]
    fn d_q2_off_lists_the_step() {
        // The switch is a parameter here: a dev build with `--write` never installs (D-Q2).
        let decision = decide(
            true,
            false,
            RegistrationState::Absent,
            &Probe::Stopped,
            true,
        );
        assert_eq!(
            decision,
            Decision::Done(
                ServiceAction::WouldInstall { dev_build: true },
                Some(ServiceStep::DevInstall)
            )
        );
        // An installed binary installs, and the preview says `would_install` either way.
        assert_eq!(
            decide(
                false,
                false,
                RegistrationState::Absent,
                &Probe::Stopped,
                true
            ),
            Decision::Done(
                ServiceAction::WouldInstall { dev_build: true },
                Some(ServiceStep::DevInstall)
            )
        );
        assert_eq!(
            decide(
                false,
                false,
                RegistrationState::Absent,
                &Probe::Stopped,
                false
            ),
            Decision::Done(ServiceAction::WouldInstall { dev_build: false }, None)
        );
        assert_eq!(
            decide(
                true,
                false,
                RegistrationState::Absent,
                &Probe::Stopped,
                false
            ),
            Decision::Install
        );
        // A dev build is never installed automatically over a running, outdated service either.
        assert_eq!(
            decide(
                true,
                false,
                RegistrationState::Outdated,
                &running(CONTRACT_VERSION, "0.0.3", Some(false)),
                true
            ),
            Decision::Done(
                ServiceAction::WouldInstall { dev_build: true },
                Some(ServiceStep::DevInstall)
            )
        );
    }

    #[test]
    fn the_service_table_rows() {
        let version = env!("CARGO_PKG_VERSION");
        // Current + matching contract: running (a binary drift is doctor's).
        for unlocked in [None, Some(false), Some(true)] {
            assert_eq!(
                decide(
                    true,
                    false,
                    RegistrationState::Current,
                    &running(CONTRACT_VERSION, "0.0.1", unlocked),
                    false
                ),
                Decision::Done(ServiceAction::Running, None)
            );
        }
        // Current + stopped: start on `--write`, nothing on the preview (R1-11).
        assert_eq!(
            decide(
                false,
                false,
                RegistrationState::Current,
                &Probe::Stopped,
                false
            ),
            Decision::Done(ServiceAction::WouldStart, None)
        );
        assert_eq!(
            decide(
                true,
                false,
                RegistrationState::Current,
                &Probe::Stopped,
                false
            ),
            Decision::Start
        );
        // Current + an older, locked service: replace (only on `--write`).
        assert_eq!(
            decide(
                false,
                false,
                RegistrationState::Current,
                &running(3, "0.0.2", Some(false)),
                false
            ),
            Decision::Done(ServiceAction::WouldReplace, None)
        );
        assert_eq!(
            decide(
                true,
                false,
                RegistrationState::Current,
                &running(3, "0.0.2", Some(false)),
                false
            ),
            Decision::Replace
        );
        // Current + an older unlocked (or silent) service: kept, with the install step.
        for unlocked in [Some(true), None] {
            let decision = decide(
                true,
                false,
                RegistrationState::Current,
                &running(3, "0.0.2", unlocked),
                false,
            );
            assert!(
                matches!(
                    decision,
                    Decision::Done(
                        ServiceAction::MismatchKept {
                            ref direction,
                            ..
                        },
                        Some(ServiceStep::InstallUnlocking)
                    ) if direction == "newer"
                ),
                "{decision:?}"
            );
        }
        // Current + a newer service: kept, never stopped (R1-2).
        let decision = decide(
            true,
            false,
            RegistrationState::Current,
            &running(6, "9.9.9", Some(false)),
            false,
        );
        assert!(
            matches!(
                decision,
                Decision::Done(
                    ServiceAction::MismatchKept {
                        ref direction,
                        ..
                    },
                    Some(ServiceStep::NewerBinary)
                ) if direction == "older"
            ),
            "{decision:?}"
        );
        // Current + a version that does not parse.
        let decision = decide(
            true,
            false,
            RegistrationState::Current,
            &running(3, "dev", Some(false)),
            false,
        );
        assert!(
            matches!(
                decision,
                Decision::Done(
                    ServiceAction::MismatchKept {
                        ref direction,
                        ..
                    },
                    Some(ServiceStep::ServiceBinary)
                ) if direction == "unknown"
            ),
            "{decision:?}"
        );
        // Absent/Outdated + a running newer or same service, locked: install.
        for registration in [RegistrationState::Absent, RegistrationState::Outdated] {
            assert_eq!(
                decide(
                    true,
                    false,
                    registration,
                    &running(CONTRACT_VERSION, version, Some(false)),
                    false
                ),
                Decision::Install
            );
            // Unlocked or silent: install_refused, plus the explicit install step.
            for unlocked in [Some(true), None] {
                let decision = decide(
                    true,
                    false,
                    registration,
                    &running(CONTRACT_VERSION, version, unlocked),
                    false,
                );
                assert!(
                    matches!(
                        decision,
                        Decision::Done(
                            ServiceAction::InstallRefused { .. },
                            Some(ServiceStep::InstallUnlocking)
                        )
                    ),
                    "{decision:?}"
                );
            }
            // Older or unknown: install_refused with the direction text.
            assert!(matches!(
                decide(
                    true,
                    false,
                    registration,
                    &running(6, "9.9.9", Some(false)),
                    false
                ),
                Decision::Done(
                    ServiceAction::InstallRefused { .. },
                    Some(ServiceStep::NewerBinary)
                )
            ));
            assert!(matches!(
                decide(
                    true,
                    false,
                    registration,
                    &running(6, "dev", Some(false)),
                    false
                ),
                Decision::Done(
                    ServiceAction::InstallRefused { .. },
                    Some(ServiceStep::ServiceBinary)
                )
            ));
        }
        // Unreachable: nothing is attempted, whatever the registration.
        for registration in [
            RegistrationState::Absent,
            RegistrationState::Current,
            RegistrationState::Outdated,
            RegistrationState::Unknown,
        ] {
            assert_eq!(
                decide(
                    true,
                    false,
                    registration,
                    &Probe::Unreachable {
                        reason: "sandbox_denied".into()
                    },
                    false
                ),
                Decision::Done(
                    ServiceAction::Unreachable {
                        reason: "sandbox_denied".into()
                    },
                    None
                )
            );
        }
        // A custom home never installs and never starts; it lists the background start (F-3),
        // also for an older service, which `service start` replaces; a newer one is kept.
        for probe in [
            Probe::Stopped,
            running(CONTRACT_VERSION - 1, version, Some(false)),
        ] {
            assert_eq!(
                decide(true, true, RegistrationState::Absent, &probe, false),
                Decision::Done(
                    ServiceAction::InstallRefused {
                        message: CUSTOM_HOME_SERVICE.into()
                    },
                    Some(ServiceStep::StartCustomHome)
                ),
                "{probe:?}"
            );
        }
        assert_eq!(
            decide(
                false,
                true,
                RegistrationState::Absent,
                &running(CONTRACT_VERSION + 1, version, Some(false)),
                false
            ),
            Decision::Done(
                ServiceAction::InstallRefused {
                    message: CUSTOM_HOME_SERVICE.into()
                },
                Some(ServiceStep::NewerBinary)
            )
        );
        assert_eq!(
            decide(
                true,
                true,
                RegistrationState::Unknown,
                &running(CONTRACT_VERSION, version, None),
                false
            ),
            Decision::Done(ServiceAction::Running, None)
        );
    }

    #[test]
    fn printed_commands_quote_paths() {
        // An env file with a space (R1-15), as `secret import` prints it.
        let env_file = "/Users/a b/proj/.env.local";
        let quoted = shell_quote(env_file, Shell::Posix);
        assert_eq!(quoted, "'/Users/a b/proj/.env.local'");
        assert_eq!(split_posix(&quoted), [env_file.to_string()]);
        // A plain path stays as it is.
        assert_eq!(
            shell_quote("/Users/ab/proj/.env.local", Shell::Posix),
            "/Users/ab/proj/.env.local"
        );
        assert_eq!(shell_quote("a'b", Shell::Posix), r"'a'\''b'");

        // An exe under Program Files (R1-15), as `init.service` prints it.
        let exe = r"C:\Program Files\hostler\hostler.exe";
        let argv = vec![exe.to_string(), "service".into(), "install".into()];
        let command = join_argv(&argv, Shell::Windows);
        assert_eq!(
            command,
            r#""C:\Program Files\hostler\hostler.exe" service install"#
        );
        assert_eq!(split_windows(&command), argv);
        // Every step's argv is the same command's arguments.
        let step = service_step_of(
            ServiceStep::DevInstall,
            Path::new(exe),
            None,
            Shell::Windows,
        );
        assert_eq!(step.argv, Some(argv));
        assert_eq!(step.command.as_deref(), Some(command.as_str()));

        let posix = vec![
            "/Users/a b/proj/hostler".to_string(),
            "service".into(),
            "install".into(),
        ];
        let command = join_argv(&posix, Shell::Posix);
        assert_eq!(split_posix(&command), posix);
        let step = service_step_of(
            ServiceStep::DevInstall,
            Path::new("/Users/a b/proj/hostler"),
            None,
            Shell::Posix,
        );
        assert_eq!(step.argv, Some(posix));
        assert_eq!(step.command.as_deref(), Some(command.as_str()));

        // A purpose with spaces is one argument in both shells.
        let argv = vec![
            "hostler".to_string(),
            "secret".to_string(),
            "request".to_string(),
            "API_KEY".to_string(),
            "--reason".to_string(),
            "TODO: say what API_KEY is for".to_string(),
        ];
        assert_eq!(split_posix(&join_argv(&argv, Shell::Posix)), argv);
        assert_eq!(split_windows(&join_argv(&argv, Shell::Windows)), argv);
    }

    /// A POSIX shell word splitter for quoted words: single quotes, `'\''`, and unquoted words.
    fn split_posix(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut started = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' => {
                    started = true;
                    for c in chars.by_ref() {
                        if c == '\'' {
                            break;
                        }
                        word.push(c);
                    }
                }
                '\\' => {
                    started = true;
                    if let Some(c) = chars.next() {
                        word.push(c);
                    }
                }
                c if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                c => {
                    started = true;
                    word.push(c);
                }
            }
        }
        if started {
            words.push(word);
        }
        words
    }

    /// `CommandLineToArgvW`'s rule: whitespace splits, `"` groups, `\"` is a literal quote.
    fn split_windows(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut quoted = false;
        let mut started = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    quoted = !quoted;
                    started = true;
                }
                '\\' if chars.peek() == Some(&'"') => {
                    chars.next();
                    word.push('"');
                    started = true;
                }
                c if c.is_whitespace() && !quoted => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                c => {
                    word.push(c);
                    started = true;
                }
            }
        }
        if started {
            words.push(word);
        }
        words
    }

    #[test]
    fn the_sandbox_step_has_the_rules_and_the_socket() {
        let socket = PathBuf::from("/Users/a b/hostler home/hostler.sock");
        let step = sandbox_step(Some(SocketName::File(socket.clone())));
        assert_eq!(step.id, "init.sandbox");
        assert_eq!(step.who, Who::Person);
        assert_eq!(step.argv, None);
        assert!(
            step.text
                .contains(include_str!("../guide/codex_hostler.rules"))
        );
        assert!(step.text.contains("~/.codex/rules/hostler.rules"));
        assert!(step.text.contains("<checkout>/.codex/rules/hostler.rules"));
        let as_json = serde_json::to_string(&display_path(&socket)).unwrap();
        assert!(step.text.contains("allowUnixSockets"), "{}", step.text);
        assert!(step.text.contains(&as_json), "{}", step.text);
        assert!(step.text.contains("VAR="), "{}", step.text);
        assert!(!step.text.contains("network_proxy"), "{}", step.text);
    }

    #[test]
    fn a_refused_registration_names_its_step() {
        let (action, step) = refused_outcome(ChangeRefused::CustomHome);
        assert!(matches!(action, ServiceAction::InstallRefused { .. }));
        assert_eq!(step, None);
        let busy = HostlerError {
            code: ErrorCode::ServiceBusy,
            message: "the hostler service is busy".into(),
            details: json!({}),
            recovery: None,
        };
        let (action, step) = refused_outcome(ChangeRefused::Busy(busy));
        assert!(matches!(action, ServiceAction::InstallRefused { .. }));
        assert_eq!(step, Some(ServiceStep::InstallWhenIdle));
        let (action, step) = refused_outcome(ChangeRefused::NotStopped {
            reason: StopRefusal::NewerService,
        });
        assert_eq!(step, Some(ServiceStep::NewerBinary));
        if let ServiceAction::InstallRefused { message } = action {
            assert!(message.contains("newer"), "{message}");
        } else {
            panic!("not an install_refused");
        }
    }

    #[test]
    fn the_import_step_quotes_an_env_file_with_a_space() {
        // R1-15: the env-file path of `hostler secret import <file>` is one argument after a
        // shell re-split, and `steps[].argv` is exactly what was printed.
        use crate::contract::init::EnvFile;
        use crate::project::config::SecretDecl;

        let config = ProjectConfig {
            secrets: std::collections::BTreeMap::from([(
                "API_KEY".to_string(),
                SecretDecl {
                    purpose: "the api key".into(),
                    env: Some("API_KEY".into()),
                },
            )]),
            ..ProjectConfig::default()
        };
        let detected = Detected {
            env_files: vec![EnvFile {
                path: ".env.local".into(),
                names: vec!["API_KEY".into()],
            }],
            ..Detected::default()
        };
        let steps = secret_steps(
            Path::new("/Users/a b/proj"),
            &detected,
            &config,
            None,
            Shell::Posix,
        );
        assert_eq!(steps.len(), 1);
        let step = &steps[0];
        assert_eq!(step.id, "init.secrets.import..env.local");
        assert_eq!(step.who, Who::Person);
        let argv = step.argv.clone().unwrap();
        assert_eq!(argv[3], "/Users/a b/proj/.env.local");
        assert_eq!(split_posix(step.command.as_ref().unwrap()), argv);
        assert!(
            step.command
                .as_ref()
                .unwrap()
                .contains("'/Users/a b/proj/.env.local'")
        );
        // Nothing of the file's values is in the step: the path and the count only.
        assert!(!step.text.contains('='), "{}", step.text);
    }

    #[test]
    fn missing_app_ids_are_a_step() {
        use crate::contract::init::DetectedApp;

        let detected = Detected {
            apps: vec![
                DetectedApp {
                    platform: Platform::Android,
                    id: "com.example.a".into(),
                    evidence: "a/build.gradle".into(),
                },
                DetectedApp {
                    platform: Platform::Android,
                    id: "com.example.b".into(),
                    evidence: "b/build.gradle".into(),
                },
                DetectedApp {
                    platform: Platform::Ios,
                    id: "com.example.c".into(),
                    evidence: "c.xcodeproj".into(),
                },
            ],
            ..Detected::default()
        };
        let empty = ProjectConfig::default();
        assert_eq!(
            missing_apps(&empty, &detected).unwrap(),
            "add these app ids under [apps]: [apps].android: com.example.a, com.example.b; \
             [apps].ios: com.example.c"
        );
        // Declared ones are not repeated.
        let mut config = ProjectConfig::default();
        config.apps.android.push("com.example.a".into());
        config.apps.ios.push("com.example.c".into());
        assert_eq!(
            missing_apps(&config, &detected).unwrap(),
            "add these app ids under [apps]: [apps].android: com.example.b"
        );
        config.apps.android.push("com.example.b".into());
        assert_eq!(missing_apps(&config, &detected), None);
    }

    #[test]
    fn a_toml_that_appeared_is_not_clobbered() {
        // R2-3: the proposal is created exclusively, so a file written after the config was
        // read is never overwritten.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hostler.toml"), "mine\n").unwrap();
        let error = create_hostler_toml(dir.path(), b"proposed\n").unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid, "{error:?}");
        assert!(error.message.contains("not overwritten"), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hostler.toml")).unwrap(),
            "mine\n"
        );

        let dir = tempfile::tempdir().unwrap();
        create_hostler_toml(dir.path(), b"proposed\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hostler.toml")).unwrap(),
            "proposed\n"
        );
    }

    #[test]
    fn the_lock_step_follows_the_probe() {
        assert!(locked_or_missing(Some(&Probe::Stopped)));
        assert!(locked_or_missing(Some(&running(
            CONTRACT_VERSION,
            "0.0.3",
            Some(false)
        ))));
        assert!(locked_or_missing(Some(&running(
            CONTRACT_VERSION,
            "0.0.3",
            None
        ))));
        assert!(!locked_or_missing(Some(&running(
            CONTRACT_VERSION,
            "0.0.3",
            Some(true)
        ))));
        assert!(!locked_or_missing(None));
        assert!(!locked_or_missing(Some(&Probe::Unreachable {
            reason: "sandbox_denied".into()
        })));
    }
}
