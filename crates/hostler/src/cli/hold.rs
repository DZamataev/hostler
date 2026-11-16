//! `hostler hold | release | hold renew` (plan P3a, Task 7).
//!
//! A hold is a claim of this workspace on the screen or one of its project's accounts. It waits
//! visibly (Task 7's shared line in `claim_wait`), may take the resource over with
//! `--force --confirmation`, and lasts a TTL or the life of a process that runs this command.
//! `release` ends a claim of this workspace; another workspace's hold needs a takeover, and a
//! running lease is ended without stopping it only with `--force`.

use std::process::ExitCode;
use std::time::Instant;

use clap::{ArgGroup, Args, Subcommand};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::cli::claim_wait::{Acquired, Deadline, Gave};
use crate::cli::secrets::request::parse_minutes;
use crate::cli::workspace;
use crate::cli::{claim_wait, output, resource_run, spawn, wait};
use crate::client::Client;
use crate::contract::claims::{
    Blocker, Claim, ClaimKind, ClaimOutcome, ClaimRelease, ClaimRequest, ClaimsSnapshot,
    FORCE_SENTENCE, HoldBinding, Purpose, ReleaseOutcome, ResourceId, ResourceKind, SeizureRecord,
    Selector, WorkspaceInfo, errors,
};
use crate::contract::wire::Request;
use crate::contract::{Envelope, ErrorCode, HostlerError};
use crate::paths::Paths;
use crate::procs::{ProcessIdentity, ProcessTable, ancestors, current_identity, identity_of};
use crate::secrets::sanitize_for_terminal;
use crate::service::manager::OsManager;

#[derive(Debug, Args)]
pub struct HoldArgs {
    #[command(subcommand)]
    pub command: HoldCommand,
}

#[derive(Debug, Subcommand)]
pub enum HoldCommand {
    /// Hold the screen (this machine's desktop session) for this workspace.
    Screen(HoldOptions),
    /// Hold one of this project's test accounts for this workspace.
    Account {
        /// The account's name.
        name: String,
        #[command(flatten)]
        options: HoldOptions,
    },
    /// Hold a phone, emulator or simulator for this workspace (P3b).
    Device {
        /// An alias or id, or `auto`, `auto:<platform>`, `auto:<platform>:<kind>`.
        device: String,
        /// Boot an emulator or simulator that is not running yet (headless); hostler shuts it
        /// down 2 minutes after its last claim ends.
        #[arg(long)]
        boot: bool,
        #[command(flatten)]
        options: HoldOptions,
    },
    /// Extend a TTL hold to now + its TTL.
    Renew {
        /// The hold's id.
        id: String,
        /// Print the contract's JSON envelope.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("binding").args(["ttl", "while_pid", "while_parent"])))]
pub struct HoldOptions {
    /// Minutes the hold lasts unless renewed (fractions allowed; more than 0, up to 1440).
    /// Default: 30.
    #[arg(long, value_name = "MIN", value_parser = parse_ttl)]
    pub ttl: Option<f64>,
    /// Hold while process PID lives, with no TTL. It must run this command (an ancestor), and
    /// not be pid 1, a service manager or the desktop shell.
    #[arg(long, value_name = "PID")]
    pub while_pid: Option<u32>,
    /// Hold while this command's parent process lives, with no TTL.
    #[arg(long)]
    pub while_parent: bool,
    /// Take a resource this workspace does not claim yet, never joining its claims (test-runner
    /// slots).
    #[arg(long)]
    pub distinct: bool,
    /// Minutes to wait (fractions allowed; 0 = no limit). Default: 5.
    #[arg(long, value_name = "MIN", value_parser = parse_minutes)]
    pub max_wait: Option<f64>,
    /// Exit 75 at once with the queue position instead of waiting.
    #[arg(long)]
    pub no_wait: bool,
    // An explicit help: clap would drop the D9 sentence's final period from a doc comment.
    #[arg(
        long,
        requires = "confirmation",
        help = "Take the resource over from its holder, stopping every process that uses it. \
                An agent uses --force only with the operator's explicit permission."
    )]
    pub force: bool,
    /// The resource's name, typed to confirm a takeover: `screen`, the account's name, or the
    /// device's alias.
    #[arg(long, value_name = "TOKEN", requires = "force")]
    pub confirmation: Option<String>,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    /// The claim's id.
    pub id: String,
    #[arg(
        long,
        requires = "confirmation",
        help = "End another workspace's claim: its hold is taken over, stopping every run that \
                uses it and its bound process; a run's claim ends without stopping the run. \
                An agent uses --force only with the operator's explicit permission."
    )]
    pub force: bool,
    /// The resource's name, typed to confirm: `screen`, or the account's name.
    #[arg(long, value_name = "TOKEN", requires = "force")]
    pub confirmation: Option<String>,
    /// Print the contract's JSON envelope.
    #[arg(long)]
    pub json: bool,
}

/// Minutes in (0, 1440].
fn parse_ttl(s: &str) -> Result<f64, String> {
    const TTL_RANGE: &str = "a TTL is more than 0 and at most 1440 minutes";
    // P2's parser explains "0 = no limit", which does not hold for a TTL: use one text for both.
    match parse_minutes(s) {
        Ok(m) if m > 0.0 && m <= MAX_TTL_MIN => Ok(m),
        _ => Err(TTL_RANGE.into()),
    }
}

const MAX_TTL_MIN: f64 = 1440.0;
/// The plan's default hold TTL (D6).
const DEFAULT_TTL_MIN: f64 = 30.0;

/// Names a process may not be bound to: pid 1, service managers and desktop shells (D6, L14).
const REFUSED_NAMES: [&str; 10] = [
    "launchd",
    "loginwindow",
    "systemd",
    "init",
    "services.exe",
    "svchost.exe",
    "wininit.exe",
    "smss.exe",
    "csrss.exe",
    "explorer.exe",
];

/// What a process-bound hold binds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindArg {
    Parent,
    Pid(u32),
}

/// The identity a `--while-parent` / `--while-pid` hold binds to (L14). Refused with the fixed
/// text when the target is pid 1, is not an ancestor, or is a service manager or desktop shell;
/// the target must also have started before this process, so a reused pid never binds (T7-5).
pub fn binding_target(
    arg: BindArg,
    table: &dyn ProcessTable,
) -> Result<ProcessIdentity, HostlerError> {
    let me = std::process::id();
    let Some(this) = identity_of(table, me) else {
        return Err(errors::bad_binding());
    };
    // The valid prefix of the parent chain: each parent started no later than its child. A
    // reused pid (or a Windows stale `th32ParentProcessID`) breaks that order and cuts the
    // chain, so nothing beyond it is treated as an ancestor.
    let mut older: Vec<ProcessIdentity> = Vec::new();
    let mut previous = this.start_time;
    for pid in ancestors(me, table) {
        let Some(id) = identity_of(table, pid) else {
            break;
        };
        if id.start_time > previous {
            break;
        }
        previous = id.start_time;
        older.push(id);
    }
    let pid = match arg {
        BindArg::Parent => older
            .first()
            .map(|id| id.pid)
            .ok_or_else(errors::bad_binding)?,
        BindArg::Pid(pid) => pid,
    };
    if pid == 1 || pid == me {
        return Err(errors::bad_binding());
    }
    let Some(target) = older.into_iter().find(|id| id.pid == pid) else {
        return Err(errors::bad_binding());
    };
    // Fail closed: without a readable name the refused-name check cannot say it is safe (T7-6).
    match table.process_name(pid) {
        Some(name) if is_refused_name(&name) => Err(errors::bad_binding()),
        Some(_) => Ok(target),
        None => Err(unreadable_name(pid)),
    }
}

/// A bound process whose name cannot be read is refused; L14's rule cannot be checked without it.
fn unreadable_name(pid: u32) -> HostlerError {
    HostlerError {
        code: ErrorCode::BadRequest,
        message: format!("cannot read the name of process {pid}; refusing a hold bound to it"),
        details: serde_json::json!({ "pid": pid }),
        recovery: None,
    }
}

/// The executable's file name (no directory), case-insensitively, against the refused list.
fn is_refused_name(name: &str) -> bool {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    REFUSED_NAMES.contains(&base.as_str())
}

pub fn main(args: HoldArgs) -> ExitCode {
    match args.command {
        HoldCommand::Renew { id, json } => {
            let result = block_on(renew(id));
            match result {
                Ok(claim) => {
                    if json {
                        print_envelope(&claim);
                    } else {
                        print_renew(&claim);
                    }
                    ExitCode::SUCCESS
                }
                Err(failure) => failure.report(json),
            }
        }
        HoldCommand::Screen(options) => {
            let json = options.json;
            emit_hold(block_on(hold(What::Screen, options)), json)
        }
        HoldCommand::Account { name, options } => {
            let json = options.json;
            emit_hold(block_on(hold(What::Account(name), options)), json)
        }
        HoldCommand::Device {
            device,
            boot,
            options,
        } => {
            let json = options.json;
            let arg = match resource_run::parse_device_arg(&device) {
                Ok(arg) => arg,
                Err(e) => {
                    return Failure {
                        code: 2,
                        lines: vec![format!("[hostler] {e}")],
                        error: error(ErrorCode::BadRequest, e),
                    }
                    .report(json);
                }
            };
            emit_hold(block_on(hold(What::Device { arg, boot }, options)), json)
        }
    }
}

pub fn main_release(args: ReleaseArgs) -> ExitCode {
    let json = args.json;
    match block_on(release(args.id, args.force, args.confirmation)) {
        Ok(done) => {
            if json {
                match &done.outcome {
                    Released::Outcome(outcome) => print_envelope(outcome),
                    Released::Seizure(record) => print_envelope(record),
                }
            } else {
                println!("{}", done.line);
            }
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(json),
    }
}

/// A claim this command now holds.
struct Held {
    claim: Claim,
    /// The kind of claim joined, when `claim` is a linked claim.
    parent_kind: Option<ClaimKind>,
    /// True when an enclosing run already holds the resource (`HOSTLER_CLAIM_TOKENS`): no new
    /// claim exists and there is nothing for this command to release (T7-4).
    reentrant: bool,
}

fn emit_hold(result: Result<Held, Failure>, json: bool) -> ExitCode {
    match result {
        Ok(held) => {
            if json {
                print_envelope(&held.claim);
            } else {
                print_hold(&held);
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

fn print_hold(held: &Held) {
    let claim = &held.claim;
    let display = &claim.resource.display;
    if held.reentrant {
        println!(
            "[hostler] {display} is already claimed by your run {}; it is released when that run \
             ends.",
            claim.id
        );
        return;
    }
    let until = match &claim.binding {
        Some(HoldBinding::Ttl { expires_at_ms, .. }) => {
            format!("until {}", claim_wait::hh_mm(*expires_at_ms))
        }
        Some(HoldBinding::Process { holder }) => format!("while PID {} lives", holder.pid),
        None => "until released".into(),
    };
    let mut line = format!("[hostler] Holding {display} as {} {until}", claim.id);
    if let (Some(parent), Some(kind)) = (&claim.parent, held.parent_kind) {
        line.push_str(&format!(
            " (joined this workspace's {} {parent})",
            kind_word(kind)
        ));
    }
    line.push_str(&format!(". Release it with: hostler release {}", claim.id));
    println!("{line}");
}

fn print_renew(claim: &Claim) {
    let display = &claim.resource.display;
    match &claim.binding {
        Some(HoldBinding::Ttl { expires_at_ms, .. }) => println!(
            "[hostler] Hold {} on {display} lasts until {}.",
            claim.id,
            claim_wait::hh_mm(*expires_at_ms)
        ),
        Some(HoldBinding::Process { holder }) => println!(
            "[hostler] Hold {} on {display} lasts while PID {} lives.",
            claim.id, holder.pid
        ),
        None => println!("[hostler] Hold {} on {display} has no expiry.", claim.id),
    }
}

fn kind_word(kind: ClaimKind) -> &'static str {
    match kind {
        ClaimKind::Lease => "lease",
        ClaimKind::Hold => "hold",
        ClaimKind::Reservation => "reservation",
    }
}

/// What `hold` holds.
enum What {
    Screen,
    Account(String),
    Device {
        arg: resource_run::DeviceArg,
        boot: bool,
    },
}

async fn hold(what: What, options: HoldOptions) -> Result<Held, Failure> {
    let cwd = cwd()?;
    let current = crate::project::current(&cwd).map_err(refusal)?;
    let table = spawn::process_table().map_err(|e| Failure::from(internal(&format!("{e:#}"))))?;
    // Binding is refused before a service is needed (a bad pid must fail at once).
    let bound = binding(&options, &table)?;
    let paths = paths()?;
    spawn::let_service_observe_self();
    let mut client = connect(&paths).await?;
    let workspace = workspace::resolve(&mut client, &current)
        .await
        .map_err(refusal)?;
    // `token`: what --confirmation must equal (a device: its alias; `auto` has none).
    let (selector, display, token, takeover, boot) = match &what {
        What::Screen => (
            Selector::Exact {
                resource: ResourceId::screen(),
            },
            "screen".to_owned(),
            Some("screen".to_owned()),
            "hostler hold screen".to_owned(),
            None,
        ),
        What::Account(name) => {
            client
                .call(Request::AccountCheck {
                    project: current.project.clone(),
                    name: name.clone(),
                })
                .await
                .map_err(refusal)?;
            (
                Selector::Exact {
                    resource: ResourceId::account(&workspace.project_id, name),
                },
                format!("account {name}"),
                Some(name.clone()),
                format!("hostler hold account {name}"),
                None,
            )
        }
        What::Device { arg, boot } => {
            let (selector, shown) = resource_run::device_selector(&mut client, arg, *boot)
                .await
                .map_err(refusal)?;
            let named = matches!(arg, resource_run::DeviceArg::Named(_));
            (
                selector,
                format!("device {shown}"),
                named.then(|| shown.clone()),
                format!("hostler hold device {shown}"),
                Some(*boot),
            )
        }
    };
    let resource = match &selector {
        Selector::Exact { resource } => Some(resource.clone()),
        Selector::Any { .. } => None,
    };
    check_confirmation(
        options.force,
        options.confirmation.as_deref(),
        token.as_deref().unwrap_or("a named device's alias"),
    )?;
    let requester =
        current_identity(&table).map_err(|e| Failure::from(internal(&format!("{e:#}"))))?;
    let ttl_ms = bound
        .is_none()
        .then(|| (options.ttl.unwrap_or(DEFAULT_TTL_MIN) * 60_000.0).round() as u64);
    let request = ClaimRequest {
        id: uuid::Uuid::new_v4().to_string(),
        selector,
        workspace_id: workspace.id.clone(),
        requester: requester.clone(),
        purpose: Purpose::Hold {
            ttl_ms,
            bound_to: bound,
            distinct: options.distinct,
        },
        reentrant_tokens: claim_tokens(),
        no_wait: options.no_wait,
    };
    // One deadline per hold, from its first wait (D7), not from service startup.
    let deadline = Deadline {
        started: Instant::now(),
        max_wait: claim_wait::max_wait(options.max_wait),
    };
    // L34: one stop for the whole hold, its signals registered before the first acquire.
    let mut stop = claim_wait::Stop::new(&deadline, spawn::Signals::register().ok());
    let target = Target {
        resource: resource.as_ref(),
        display: &display,
        token: token.as_deref().unwrap_or(""),
        takeover: &takeover,
        device: boot.is_some(),
    };
    let acquired = acquire(
        &mut client,
        &workspace,
        &target,
        request,
        &mut stop,
        &options,
    )
    .await
    .map_err(|gave| failure_from_gave(gave, &display, deadline.max_wait))?;
    let mut held = match acquired {
        Acquired::Granted(claim) => Held {
            claim,
            parent_kind: None,
            reentrant: false,
        },
        Acquired::Joined(claim) => {
            let parent_kind = parent_kind(&mut client, &claim).await;
            Held {
                claim,
                parent_kind,
                reentrant: false,
            }
        }
        Acquired::Reentrant(claim_id) => {
            let claim = live_claim(&mut client, &claim_id)
                .await
                .ok_or_else(|| refusal(errors::claim_not_held(&claim_id)))?;
            Held {
                claim,
                parent_kind: None,
                reentrant: true,
            }
        }
    };
    if let Some(boot) = boot
        && !held.reentrant
    {
        ready_hold(
            &mut client,
            &workspace,
            &mut held,
            &requester,
            boot,
            &mut stop,
        )
        .await?;
    }
    Ok(held)
}

/// Step 3 of a device hold: boot a device that is not ready (`--boot`), else release and
/// refuse; then merge the device as of now into the claim (P2-8), so `--json` carries a booted
/// emulator's `emulator-N`.
async fn ready_hold(
    client: &mut Client,
    workspace: &WorkspaceInfo,
    held: &mut Held,
    requester: &ProcessIdentity,
    boot: bool,
    stop: &mut claim_wait::Stop,
) -> Result<(), Failure> {
    let ready = resource_run::ready_device(client, &held.claim, &workspace.id, boot, stop).await;
    let info = match ready {
        Ok(info) => info,
        Err(not_ready) => {
            // The claim was granted: it is ours to end (the service finishes a boot).
            let id = held.claim.id.clone();
            let released = release_retrying(client, &id, &workspace.id, requester).await;
            let end = if released {
                "the hold was released".to_owned()
            } else {
                format!("the hold {id} may still be held: hostler release {id}")
            };
            return Err(match not_ready {
                resource_run::NotReady::Interrupted(code) => Failure {
                    lines: vec![format!("[hostler] Interrupted; {end}.")],
                    error: error(ErrorCode::Withdrawn, format!("interrupted; {end}")),
                    code: u8::try_from(code).unwrap_or(1),
                },
                resource_run::NotReady::Refused(e) => {
                    if !released {
                        eprintln!("[hostler] {}.", sanitize_for_terminal(&end));
                    }
                    refusal(e)
                }
            });
        }
    };
    resource_run::merge_device(&mut held.claim, &info);
    if let Some(reason) = claim_wait::sessions_unknown(client).await {
        eprintln!("{}", claim_wait::sessions_unknown_line(&reason));
    }
    Ok(())
}

/// Ends this command's own claim, reconnecting while the service does not answer, within
/// `RELEASE_BUDGET`; `true` once it is ended (also when it already was).
async fn release_retrying(
    client: &mut Client,
    claim_id: &str,
    workspace_id: &str,
    requester: &ProcessIdentity,
) -> bool {
    let until = Instant::now() + RELEASE_BUDGET;
    loop {
        let release = Request::ClaimsRelease(ClaimRelease {
            claim_id: claim_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            requester: requester.clone(),
            exit_code: None,
        });
        match client.call(release).await {
            Ok(_) => return true,
            Err(e) if e.code == ErrorCode::ClaimNotHeld => return true,
            Err(e) if e.code == ErrorCode::ServiceUnavailable && Instant::now() < until => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if let Ok(paths) = Paths::resolve()
                    && let Ok(fresh) = Client::connect(&paths).await
                {
                    *client = fresh;
                }
            }
            Err(_) => return false,
        }
    }
}

/// A release after an interrupted or failed boot is retried this long (as `run`'s).
const RELEASE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

fn binding(
    options: &HoldOptions,
    table: &dyn ProcessTable,
) -> Result<Option<ProcessIdentity>, Failure> {
    if options.while_parent {
        return binding_target(BindArg::Parent, table)
            .map(Some)
            .map_err(refusal);
    }
    if let Some(pid) = options.while_pid {
        return binding_target(BindArg::Pid(pid), table)
            .map(Some)
            .map_err(refusal);
    }
    Ok(None)
}

/// The comma-separated live lease ids of ancestor runs (P1's heavy-token rule, for claims).
fn claim_tokens() -> Vec<String> {
    std::env::var("HOSTLER_CLAIM_TOKENS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter(|token| !token.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// What a hold asks for: the resource (`None` for `auto`), its display, `--confirmation` token
/// and takeover command.
struct Target<'a> {
    resource: Option<&'a ResourceId>,
    display: &'a str,
    token: &'a str,
    takeover: &'a str,
    device: bool,
}

/// The request, with `--force` giving a foreign-workspace blocker a recorded takeover first.
async fn acquire(
    client: &mut Client,
    workspace: &WorkspaceInfo,
    target: &Target<'_>,
    request: ClaimRequest,
    stop: &mut claim_wait::Stop,
    options: &HoldOptions,
) -> Result<Acquired, Gave> {
    if options.force
        && let Some(resource) = target.resource
    {
        let first = claim_wait::acquire_once(client, &request, stop).await?;
        let need_takeover = match &first {
            ClaimOutcome::Queued { blockers, .. } | ClaimOutcome::Busy { blockers, .. } => blockers
                .iter()
                .any(|blocker| other_workspace(blocker, &workspace.path)),
            _ => false,
        };
        if need_takeover {
            let confirmation = options.confirmation.as_deref().unwrap_or_default();
            let taken =
                claim_wait::take_over(client, resource, confirmation, request.clone(), stop).await;
            match taken {
                Ok(ClaimOutcome::Granted { claim }) => return Ok(Acquired::Granted(claim)),
                Ok(ClaimOutcome::Joined { claim }) => return Ok(Acquired::Joined(claim)),
                Ok(ClaimOutcome::Reentrant { claim_id }) => {
                    return Ok(Acquired::Reentrant(claim_id));
                }
                Ok(_) => {}
                Err(e) => {
                    // A grant that slipped past the failure is ours (T8-4), so no 30-min hold
                    // is left behind; else the request is withdrawn. Interrupted: 130/143;
                    // refused: 78 (`Failed` under --force).
                    let code = claim_wait::takeover_exit_code(&e);
                    return match claim_wait::settle_failed_takeover(client, &request, e).await {
                        Ok(acquired) => Ok(acquired),
                        Err(e) if code == 78 => Err(Gave::Failed(e)),
                        Err(_) => Err(Gave::Interrupted(code)),
                    };
                }
            }
        }
        match first {
            ClaimOutcome::Granted { claim } => return Ok(Acquired::Granted(claim)),
            ClaimOutcome::Joined { claim } => return Ok(Acquired::Joined(claim)),
            ClaimOutcome::Reentrant { claim_id } => {
                return Ok(Acquired::Reentrant(claim_id));
            }
            // Busy, still queued, or already decided: `acquire_waiting` re-reads it by id.
            _ => {}
        }
    }
    // L32: a hold's own takeover command, offered only behind another workspace's claim.
    let text = claim_wait::WaitText {
        display: target.display,
        own_workspace: &workspace.path,
        takeover: (!options.force && !target.token.is_empty()).then_some(target.takeover),
        token: target.token,
        device: target.device,
    };
    claim_wait::acquire_waiting(client, request, &text, stop, options.no_wait).await
}

fn other_workspace(blocker: &Blocker, own_path: &str) -> bool {
    match blocker {
        Blocker::Claim { summary } => summary.workspace_path != own_path,
        Blocker::Foreign { .. } => true,
        Blocker::Seizing { by_workspace } => by_workspace != own_path,
        // A takeover cannot end the wait for a fresh sessions read, so it starts none. What
        // `--force` answers then is Tasks 7 and 9b's (L54: a refusal with the forget-sessions
        // recovery, never a silent wait).
        Blocker::Checking { .. } => false,
    }
}

fn failure_from_gave(gave: Gave, display: &str, max_wait: Option<std::time::Duration>) -> Failure {
    match gave {
        Gave::TimedOut => {
            let limit = max_wait.map_or_else(String::new, wait::duration_text);
            let message =
                format!("gave up after {limit} waiting for {display}; the request was withdrawn");
            Failure {
                lines: vec![format!(
                    "[hostler] Gave up after {limit} waiting for {display}; the request was \
                     withdrawn."
                )],
                error: error(ErrorCode::QueueTimeout, message),
                code: 75,
            }
        }
        Gave::Busy { position, of } => Failure {
            lines: vec![format!(
                "[hostler] Not waiting (--no-wait) for {display}; you would be {position} of {of}."
            )],
            error: error(
                ErrorCode::QueueBusy,
                format!("{display} is busy; you would be {position} of {of} (--no-wait)"),
            ),
            code: 75,
        },
        Gave::Interrupted(code) => Failure {
            lines: vec!["[hostler] Interrupted; the request was withdrawn.".into()],
            error: error(
                ErrorCode::Withdrawn,
                "interrupted; the request was withdrawn".into(),
            ),
            code: u8::try_from(code).unwrap_or(1),
        },
        Gave::WithdrawnElsewhere => Failure {
            lines: vec![format!(
                "[hostler] The request was withdrawn by an operator; not holding {display}."
            )],
            error: error(
                ErrorCode::Withdrawn,
                "the request was withdrawn by an operator".into(),
            ),
            code: 75,
        },
        Gave::Deadlock(e) => refusal(e),
        Gave::Failed(e) => refusal(e),
    }
}

/// A release that happened: what to print, and the contract answer for `--json`.
struct Done {
    outcome: Released,
    line: String,
}

/// What `--json` answers: the claim release outcome, or the seizure a forced lease release
/// recorded (the record says `forced: true`).
enum Released {
    Outcome(Box<ReleaseOutcome>),
    Seizure(SeizureRecord),
}

async fn release(id: String, force: bool, confirmation: Option<String>) -> Result<Done, Failure> {
    let cwd = cwd()?;
    let current = crate::project::current(&cwd).map_err(refusal)?;
    let paths = paths()?;
    spawn::let_service_observe_self();
    let table = spawn::process_table().map_err(|e| Failure::from(internal(&format!("{e:#}"))))?;
    let mut client = connect(&paths).await?;
    let workspace = workspace::resolve(&mut client, &current)
        .await
        .map_err(refusal)?;
    let requester =
        current_identity(&table).map_err(|e| Failure::from(internal(&format!("{e:#}"))))?;
    let Some(claim) = live_claim(&mut client, &id).await else {
        let outcome: ReleaseOutcome = decode(
            client
                .call(Request::ClaimsRelease(ClaimRelease {
                    claim_id: id.clone(),
                    workspace_id: workspace.id,
                    requester,
                    exit_code: None,
                }))
                .await
                .map_err(refusal)?,
        )
        .map_err(refusal)?;
        return Ok(Done {
            outcome: Released::Outcome(Box::new(outcome)),
            line: format!("[hostler] Claim {id} was already ended."),
        });
    };
    let display = claim.resource.display.clone();
    let token = crate::claims::engine::token_of(&claim.resource);
    check_confirmation(force, confirmation.as_deref(), &token)?;
    let own = claim.workspace.id == workspace.id;

    // A lease is the run wrapper's to end: force ends the claim without stopping the run (L23).
    if claim.kind == ClaimKind::Lease {
        if !force {
            let error = if own {
                own_lease_notice(&display, &claim, &id, &token)
            } else {
                other_lease_notice(&display, &claim, &token)
            };
            return Err(refusal(error));
        }
        let record: SeizureRecord = decode(
            client
                .call(Request::ClaimsForceRelease {
                    claim_id: id.clone(),
                    confirmation: confirmation.unwrap_or_default(),
                    workspace_id: workspace.id,
                    requester,
                })
                .await
                .map_err(refusal)?,
        )
        .map_err(refusal)?;
        return Ok(Done {
            outcome: Released::Seizure(record),
            line: format!(
                "[hostler] Ended claim {id} on {display} without stopping its run; recorded as a \
                 forced release."
            ),
        });
    }

    if !own {
        if !force {
            return Err(refusal(errors::force_required(
                &display,
                &claim.workspace.path,
                &format!("hostler release {id}"),
                &token,
            )));
        }
        let request = ClaimRequest {
            id: uuid::Uuid::new_v4().to_string(),
            selector: Selector::Exact {
                resource: claim.resource.id.clone(),
            },
            workspace_id: workspace.id.clone(),
            requester: requester.clone(),
            purpose: Purpose::Hold {
                ttl_ms: Some((DEFAULT_TTL_MIN * 60_000.0) as u64),
                bound_to: None,
                distinct: false,
            },
            reentrant_tokens: Vec::new(),
            no_wait: false,
        };
        // L34: this command's one listener, handed to the takeover.
        let mut stop = claim_wait::Stop::new(
            &Deadline {
                started: Instant::now(),
                max_wait: None,
            },
            spawn::Signals::register().ok(),
        );
        let taken = claim_wait::take_over(
            &mut client,
            &claim.resource.id,
            confirmation.as_deref().unwrap_or_default(),
            request.clone(),
            &mut stop,
        )
        .await;
        let new_id = match taken {
            Ok(ClaimOutcome::Granted { claim } | ClaimOutcome::Joined { claim }) => claim.id,
            Ok(ClaimOutcome::Reentrant { claim_id }) => claim_id,
            Ok(other) => {
                return Err(Failure::from(internal(&format!(
                    "unexpected takeover answer: {other:?}"
                ))));
            }
            Err(e) => {
                // L40: a refused takeover exits 78 (an interrupted one its 130/143); a lost
                // service or an internal failure stays 1.
                let signal = claim_wait::takeover_exit_code(&e);
                // A grant that slipped past the failure (T8-4) is released below like any.
                match claim_wait::settle_failed_takeover(&mut client, &request, e).await {
                    Ok(Acquired::Granted(claim) | Acquired::Joined(claim)) => claim.id,
                    Ok(Acquired::Reentrant(claim_id)) => claim_id,
                    Err(e) => {
                        let code = if e.code == ErrorCode::Withdrawn {
                            u8::try_from(signal).unwrap_or(1)
                        } else {
                            claim_code(e.code)
                        };
                        return Err(Failure {
                            code,
                            error: e,
                            lines: Vec::new(),
                        });
                    }
                }
            }
        };
        client
            .call(Request::ClaimsRelease(ClaimRelease {
                claim_id: new_id.clone(),
                workspace_id: workspace.id,
                requester,
                exit_code: None,
            }))
            .await
            .map_err(refusal)?;
        return Ok(Done {
            outcome: Released::Outcome(Box::new(ReleaseOutcome {
                parent: None,
                seized_by: None,
            })),
            line: format!("[hostler] Released {display} ({new_id}); the resource is free."),
        });
    }

    let outcome: ReleaseOutcome = decode(
        client
            .call(Request::ClaimsRelease(ClaimRelease {
                claim_id: id.clone(),
                workspace_id: workspace.id.clone(),
                requester,
                exit_code: None,
            }))
            .await
            .map_err(refusal)?,
    )
    .map_err(refusal)?;
    // L49: a device hold's end names the sessions still open on it (stderr).
    if claim.resource.kind == ResourceKind::Device {
        let (warnings, _) = resource_run::device_end(&mut client, &claim, &workspace.id).await;
        for line in warnings {
            eprintln!("{line}");
        }
    }
    Ok(Done {
        outcome: Released::Outcome(Box::new(outcome)),
        line: format!("[hostler] Released {display} ({id})."),
    })
}

async fn renew(id: String) -> Result<Claim, Failure> {
    let cwd = cwd()?;
    let current = crate::project::current(&cwd).map_err(refusal)?;
    let paths = paths()?;
    let mut client = connect(&paths).await?;
    let workspace = workspace::resolve(&mut client, &current)
        .await
        .map_err(refusal)?;
    decode(
        client
            .call(Request::ClaimsRenew {
                claim_id: id,
                workspace_id: workspace.id,
            })
            .await
            .map_err(refusal)?,
    )
    .map_err(refusal)
}

async fn parent_kind(client: &mut Client, claim: &Claim) -> Option<ClaimKind> {
    let parent = claim.parent.as_deref()?;
    live_claim(client, parent).await.map(|c| c.kind)
}

async fn live_claim(client: &mut Client, id: &str) -> Option<Claim> {
    let snapshot: ClaimsSnapshot = decode(client.call(Request::ClaimsSnapshot).await.ok()?).ok()?;
    snapshot
        .resources
        .into_iter()
        .flat_map(|resource| resource.claims)
        .find(|claim| claim.id == id)
}

/// The resource flag of `hostler run` for `display` (`--screen`, `--account NAME`).
fn run_selector(display: &str) -> String {
    if display == "screen" {
        "--screen".into()
    } else if let Some(name) = display.strip_prefix("account ") {
        format!("--account {name}")
    } else {
        display.to_owned()
    }
}

/// `release ID` of this workspace's own run: only its wrapper ends it; a stuck one may be ended
/// without stopping anything (L23). Not a takeover command — a takeover of one's own claim is
/// refused (T7-3).
fn own_lease_notice(display: &str, claim: &Claim, id: &str, token: &str) -> HostlerError {
    let pid = claim
        .child
        .as_ref()
        .or(claim.holder.as_ref())
        .map_or_else(|| "?".to_owned(), |p| p.pid.to_string());
    HostlerError {
        code: ErrorCode::ForceRequired,
        message: format!(
            "{display} is this workspace's run (PID {pid}); it ends when that run ends. If it is \
             stuck: hostler release {id} --force --confirmation {token} ends the claim without \
             stopping the run. {FORCE_SENTENCE}"
        ),
        details: serde_json::json!({ "claim_id": id, "resource": display, "token": token }),
        recovery: None,
    }
}

/// `release ID` of another workspace's run: taking it over stops the run, so the command needs
/// the command to run after the takeover (T7-3).
fn other_lease_notice(display: &str, claim: &Claim, token: &str) -> HostlerError {
    HostlerError {
        code: ErrorCode::ForceRequired,
        message: format!(
            "{display} is held by {}. To take it over: hostler run {} --force --confirmation \
             {token} -- <command>. {FORCE_SENTENCE}",
            claim.workspace.path,
            run_selector(display)
        ),
        details: serde_json::json!({
            "resource": display,
            "holder_path": claim.workspace.path,
            "token": token,
        }),
        recovery: None,
    }
}

fn cwd() -> Result<std::path::PathBuf, Failure> {
    std::env::current_dir()
        .map_err(|e| Failure::from(internal(&format!("cannot read the current directory: {e}"))))
}

fn paths() -> Result<Paths, Failure> {
    Paths::resolve().map_err(|e| Failure::from(internal(&format!("{e:#}"))))
}

async fn connect(paths: &Paths) -> Result<Client, Failure> {
    Client::connect_or_start(paths, &OsManager)
        .await
        .map_err(refusal)
}

/// CR-B-6: `--force --confirmation` must name the resource whether or not a takeover follows,
/// as `run` checks it (exit 2).
fn check_confirmation(force: bool, confirmation: Option<&str>, token: &str) -> Result<(), Failure> {
    if !force || confirmation == Some(token) {
        return Ok(());
    }
    let message = format!(
        "--confirmation must be {token}: the resource's name, typed to confirm the takeover"
    );
    Err(Failure {
        code: 2,
        lines: vec![format!("[hostler] {}", sanitize_for_terminal(&message))],
        error: error(ErrorCode::BadRequest, message),
    })
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, HostlerError> {
    serde_json::from_value(value)
        .map_err(|e| internal(&format!("unreadable answer from the service: {e}")))
}

fn internal(message: &str) -> HostlerError {
    error(ErrorCode::Internal, message.to_owned())
}

fn error(code: ErrorCode, message: String) -> HostlerError {
    HostlerError {
        code,
        message,
        details: serde_json::json!({}),
        recovery: None,
    }
}

/// A command failure with its exit code; stop lines go to stderr, the `--json` envelope to stdout.
struct Failure {
    code: u8,
    error: HostlerError,
    lines: Vec<String>,
}

impl Failure {
    fn report(self, json: bool) -> ExitCode {
        for line in &self.lines {
            eprintln!("{line}");
        }
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
        } else if self.lines.is_empty() {
            output::print_error(&self.error);
        }
        ExitCode::from(self.code)
    }
}

impl From<HostlerError> for Failure {
    fn from(error: HostlerError) -> Failure {
        Failure {
            code: 1,
            error,
            lines: Vec::new(),
        }
    }
}

/// L40: `hold` and `release` exit like `run`. A refused claim is 78, the queue's own answers are
/// 75, a usage problem is 2, a signal is its own code, and anything else is 1.
fn claim_code(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::QueueBusy => 75,
        ErrorCode::BadRequest => 2,
        ErrorCode::UnknownResource
        | ErrorCode::ResourceInUse
        | ErrorCode::ClaimNotHeld
        | ErrorCode::SeizureFailed
        | ErrorCode::AccountMissing
        | ErrorCode::ClaimDeadlock
        | ErrorCode::TakenOver
        | ErrorCode::NotInProject
        | ErrorCode::SecretsLocked
        | ErrorCode::ProjectNotApproved
        | ErrorCode::ForceRequired
        // P3b: every device refusal (the plan's exit codes).
        | ErrorCode::DeviceNotFound
        | ErrorCode::AliasTaken
        | ErrorCode::DeviceNotRegistered
        | ErrorCode::DeviceNotReady
        | ErrorCode::BootFailed
        | ErrorCode::ToolMissing
        | ErrorCode::PermissionDenied
        // L42: without the service a claim command refuses like `run` (D8), and an older or
        // newer service too (FR-6).
        | ErrorCode::ServiceUnavailable
        | ErrorCode::ContractMismatch => 78,
        _ => 1,
    }
}

fn refusal(error: HostlerError) -> Failure {
    Failure {
        code: claim_code(error.code),
        error,
        lines: Vec::new(),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::fake::FakeTable;

    #[test]
    fn binding_refuses_pid_1_non_ancestors_and_listed_names() {
        let table = FakeTable::new("b1");
        let me = std::process::id();
        table.insert(me, Some(100), Some(1));
        table.insert(100, Some(1), Some(1));
        table.insert(1, None, Some(1));
        table.insert(200, None, Some(1));
        table.set_name(100, "zsh");

        assert_eq!(binding_target(BindArg::Pid(100), &table).unwrap().pid, 100);
        for arg in [BindArg::Pid(1), BindArg::Pid(200), BindArg::Pid(me)] {
            let e = binding_target(arg, &table).unwrap_err();
            assert_eq!(e.message, errors::bad_binding().message, "{arg:?}");
        }
        table.set_name(100, "/usr/sbin/systemd");
        assert!(binding_target(BindArg::Pid(100), &table).is_err());
        table.set_name(100, "Explorer.EXE");
        assert!(binding_target(BindArg::Pid(100), &table).is_err());
        table.set_name(100, "zsh");
        assert!(binding_target(BindArg::Pid(100), &table).is_ok());
    }

    #[test]
    fn while_parent_is_the_direct_parent() {
        let table = FakeTable::new("b1");
        let me = std::process::id();
        table.insert(me, Some(100), Some(1));
        table.insert(100, Some(1), Some(1));
        table.set_name(100, "zsh");
        assert_eq!(binding_target(BindArg::Parent, &table).unwrap().pid, 100);
        // No parent read: the fixed text, never a panic.
        let bare = FakeTable::new("b1");
        bare.insert(me, None, Some(1));
        assert!(binding_target(BindArg::Parent, &bare).is_err());
    }

    #[test]
    fn binding_refuses_a_parent_started_after_this_process() {
        let table = FakeTable::new("b1");
        let me = std::process::id();
        table.insert(me, Some(100), Some(500));
        table.insert(100, Some(200), Some(600));
        table.insert(200, None, Some(100));
        table.set_name(100, "zsh");
        table.set_name(200, "zsh");
        // A parent younger than its child is a reused pid: neither the parent nor anything
        // behind it may be bound.
        assert!(binding_target(BindArg::Parent, &table).is_err());
        assert!(binding_target(BindArg::Pid(100), &table).is_err());

        table.insert(100, Some(200), Some(400));
        table.set_name(100, "zsh");
        assert_eq!(binding_target(BindArg::Parent, &table).unwrap().pid, 100);
        assert_eq!(binding_target(BindArg::Pid(200), &table).unwrap().pid, 200);

        // A stale link behind a valid one cuts the chain (Windows' dead parent pid).
        table.insert(200, None, Some(600));
        assert_eq!(binding_target(BindArg::Parent, &table).unwrap().pid, 100);
        assert!(binding_target(BindArg::Pid(200), &table).is_err());
    }

    #[test]
    fn an_unreadable_process_name_refuses_the_binding() {
        let table = FakeTable::new("b1");
        let me = std::process::id();
        table.insert(me, Some(100), Some(1));
        table.insert(100, Some(1), Some(1));
        let e = binding_target(BindArg::Parent, &table).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(
            e.message.contains("cannot read the name of process 100"),
            "{}",
            e.message
        );
    }

    #[test]
    fn ttl_parser_and_wait_defaults() {
        assert_eq!(parse_ttl("30"), Ok(30.0));
        assert_eq!(parse_ttl("0.5"), Ok(0.5));
        assert!(parse_ttl("0").is_err());
        assert!(parse_ttl("1440.5").is_err());
        assert_eq!(
            claim_wait::max_wait(None),
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(claim_wait::max_wait(Some(0.0)), None);
    }

    fn error_of(code: ErrorCode) -> HostlerError {
        HostlerError {
            code,
            message: "test".into(),
            details: serde_json::json!({}),
            recovery: None,
        }
    }

    #[test]
    fn claim_refusals_exit_78_and_the_queue_exits_75() {
        // L40: `hold` and `release` exit like `run`.
        assert_eq!(
            failure_from_gave(Gave::Failed(error_of(ErrorCode::QueueBusy)), "screen", None).code,
            75
        );
        assert_eq!(
            failure_from_gave(
                Gave::Deadlock(errors::claim_deadlock("screen", "a cycle")),
                "screen",
                None
            )
            .code,
            78
        );
        for code in [
            ErrorCode::SecretsLocked,
            ErrorCode::AccountMissing,
            ErrorCode::ProjectNotApproved,
            ErrorCode::NotInProject,
            ErrorCode::ForceRequired,
            ErrorCode::SeizureFailed,
            ErrorCode::ClaimDeadlock,
            ErrorCode::TakenOver,
            // L42: fail closed like `run`.
            ErrorCode::ServiceUnavailable,
            // FR-6: a service of another contract version, as `run --screen`.
            ErrorCode::ContractMismatch,
            // P3b: every device refusal.
            ErrorCode::DeviceNotFound,
            ErrorCode::AliasTaken,
            ErrorCode::DeviceNotRegistered,
            ErrorCode::DeviceNotReady,
            ErrorCode::BootFailed,
            ErrorCode::ToolMissing,
            ErrorCode::PermissionDenied,
        ] {
            assert_eq!(claim_code(code), 78, "{code:?}");
        }
        // A device method still a stub answers `bad_request`: a usage-like 2, as the plan's
        // other bad requests.
        assert_eq!(
            claim_code(crate::contract::devices::errors::not_available_yet("devices.resolve").code),
            2
        );
        assert_eq!(claim_code(ErrorCode::BadRequest), 2);
        assert_eq!(claim_code(ErrorCode::Internal), 1);
    }
}
