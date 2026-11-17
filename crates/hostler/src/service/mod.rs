//! The per-user hostler service (plan Task 6): one process per home, found through a local
//! socket, owning the store, the heavy-queue engine and the secrets engine (P2 Task 6).

pub mod harden;
pub mod lifecycle;
pub mod lock;
pub mod manager;
pub mod server;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::json;
use tokio::sync::{broadcast, watch};

use crate::bridge::looky::{LookyHeavy, RefreshedLooky};
use crate::claims::engine::ClaimsEngine;
use crate::claims::{ForeignClaimSource, ResourceCatalog, ScreenCatalog};
use crate::contract::heavy::ServiceStatus;
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
use crate::devices::agent_device::AgentDeviceSource;
use crate::devices::catalog::DeviceCatalog;
use crate::devices::service::DeviceCache;
use crate::heavy::engine::{HeavyEngine, SystemClock};
use crate::paths::Paths;
use crate::procs::{self, ProcessIdentity, ProcessTable, SysProcessTable};
use crate::secrets::engine::SecretsEngine;
use crate::secrets::vault::{self, KdfChoice, Vault};
use crate::store::Store;

/// Registers and starts the service with the OS (Task 10).
pub trait ServiceManager: Send + Sync {
    fn is_installed(&self) -> bool;
    fn start(&self) -> anyhow::Result<()>;
}

impl<T: ServiceManager + ?Sized> ServiceManager for Arc<T> {
    fn is_installed(&self) -> bool {
        (**self).is_installed()
    }

    fn start(&self) -> anyhow::Result<()> {
        (**self).start()
    }
}

/// No OS registration: clients spawn the service themselves.
#[derive(Clone, Copy)]
pub struct NoManager;

impl ServiceManager for NoManager {
    fn is_installed(&self) -> bool {
        false
    }

    fn start(&self) -> anyhow::Result<()> {
        anyhow::bail!("the hostler service is not registered with the OS")
    }
}

/// How `hostler service run` ended without an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// Served until stopped.
    Stopped,
    /// Another service, or an installer changing the registration, holds the lock.
    LockHeld,
    /// Started directly (not by the OS manager) while an OS registration exists for this home:
    /// the registered service is the one that serves (spec §6.1).
    Registered,
}

/// `hostler service run [--managed]`; `managed` is passed only by the OS registration.
pub async fn run_service(paths: Paths, managed: bool) -> anyhow::Result<RunEnd> {
    run_service_with(paths, managed, &manager::OsManager).await
}

/// `run_service` with an injected manager (tests).
pub async fn run_service_with(
    paths: Paths,
    managed: bool,
    manager: &dyn ServiceManager,
) -> anyhow::Result<RunEnd> {
    #[cfg(unix)]
    // SAFETY: umask has no preconditions; it only affects files this process creates later.
    unsafe {
        libc::umask(0o077);
    }
    let Some(held) = lock::try_hold(&paths.service_lock)? else {
        return Ok(RunEnd::LockHeld);
    };
    let _log = init_logging(&paths);
    // Checked while holding the lock: an installer writes the registration before it releases
    // the lock, so a directly started service can never serve once a registration exists.
    if !managed && !paths.custom_home && manager.is_installed() {
        tracing::info!(
            "not serving: started directly, but an OS registration exists (if it is from an \
             older hostler without --managed, refresh it with `hostler service install`)"
        );
        drop(held);
        return Ok(RunEnd::Registered);
    }
    let result = serve_locked(&paths).await;
    if let Err(e) = &result {
        tracing::error!("the service stopped with an error: {e:#}");
    }
    drop(held);
    result.map(|()| RunEnd::Stopped)
}

async fn serve_locked(paths: &Paths) -> anyhow::Result<()> {
    harden::harden_service_process();
    let recovery = vault::recover_at_start(&paths.kdbx);
    if recovery.restored || recovery.removed_temps > 0 || !recovery.preserved.is_empty() {
        tracing::info!(
            "secrets database repair: restored a moved-aside file: {}, removed {} temp file(s),              kept: {:?}",
            recovery.restored,
            recovery.removed_temps,
            recovery.preserved
        );
    }
    let sys_table = SysProcessTable::new().context("cannot read the boot id")?;
    // The service runs outside any agent sandbox and must know its boot: an identity a
    // sandboxed client sends without one is completed with it (P4 Task 9 F-1).
    anyhow::ensure!(
        sys_table.boot_id_known(),
        "the hostler service may not read this machine's boot id; it cannot run here (inside \
         an agent sandbox?)"
    );
    crate::procs::set_local_boot_id(&sys_table.boot_id());
    // Review f1-r1: the run-directory sweep judges stored owners with this table's real boot id,
    // after the local boot id is known.
    let swept = crate::secrets::rundir::sweep_with_table(&paths.runs, &sys_table);
    if swept > 0 {
        tracing::info!("removed {swept} leftover secret run director(ies)");
    }
    let table: Arc<dyn ProcessTable> = Arc::new(sys_table);
    // L58 (debug builds): a service a test started stops once its test process is gone, even
    // when a killed run never reached the test's own stop guard. The initial receiver stays
    // alive: a stop sent before `serve` subscribes must still be a stored value.
    let (shutdown, _open) = watch::channel(false);
    #[cfg(debug_assertions)]
    spawn_owner_watchdog(table.clone(), shutdown.clone());
    let store = Store::open(&paths.db)?;
    let (events, _) = broadcast::channel(64);
    // P3b: the device service, on its own (4th) connection. Its catalog and sessions source
    // answer the claims engine from its cache, without I/O (T5-4).
    let devices_store = Store::open(&paths.db)?;
    let device_tools = crate::devices::service::find_service_tools(
        &crate::devices::service::read_settings(&devices_store),
    );
    let (devices, device_cache) =
        crate::devices::service::DevicesService::new(devices_store, device_tools, events.clone());
    let engine = HeavyEngine::new(
        store,
        table.clone(),
        Arc::new(SystemClock),
        Box::new(RefreshedLooky::start(LookyHeavy::new())),
        events.clone(),
    );
    let claims = ClaimsEngine::new(
        Store::open(&paths.db)?,
        table.clone(),
        Arc::new(SystemClock),
        catalogs(&device_cache),
        foreign_sources(&device_cache),
        events.clone(),
    );
    let secrets = SecretsEngine::new(
        Vault::new(paths.kdbx.clone()),
        paths.kdbx.clone(),
        Store::open(&paths.db)?,
        table,
        Arc::new(SystemClock),
        events.clone(),
        kdf_choice(),
    );
    // Workspace paths for the stranding check, probed off the claims lock (T5-1); the task
    // ends with the runtime.
    tokio::spawn(claims.path_probe().run());
    let listener = server::bind(&paths.socket)?;
    tracing::info!(
        "hostler service {} (pid {}) listening on {:?}",
        lifecycle::SERVICE_VERSION,
        std::process::id(),
        paths.socket
    );
    // Dropping the listener removes the socket file while the lock is still held.
    server::serve(listener, engine, claims, secrets, devices, events, shutdown).await
}

/// L58, debug builds only: `HOSTLER_TEST_OWNER_PID` names the test process a service started by
/// a test belongs to. Once that identity is gone (dead, or the pid reused), the service takes
/// its normal shutdown path, so no test daemon outlives a killed run. Its own task: it never
/// holds the heavy, claims or secrets locks.
#[cfg(debug_assertions)]
fn spawn_owner_watchdog(table: Arc<dyn ProcessTable>, shutdown: watch::Sender<bool>) {
    let Some(pid) = owner_pid(std::env::var_os("HOSTLER_TEST_OWNER_PID").as_deref()) else {
        return;
    };
    tokio::spawn(async move {
        // Read once: a pid that is gone, or whose start time cannot be read, never comes back
        // as the same process.
        let owner = procs::identity_of(&*table, pid);
        let mut every = tokio::time::interval(Duration::from_secs(1));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate: an unreadable owner stops the service at once.
        loop {
            every.tick().await;
            let alive = owner
                .as_ref()
                .is_some_and(|owner| procs::liveness(owner, &*table) == procs::Liveness::Alive);
            if !alive {
                tracing::info!(
                    "stopping: the test process {pid} that started this service is gone"
                );
                let _ = shutdown.send(true);
                return;
            }
        }
    });
}

/// `HOSTLER_TEST_OWNER_PID` as a decimal pid; anything else is logged and the hook ignored.
#[cfg(debug_assertions)]
fn owner_pid(raw: Option<&std::ffi::OsStr>) -> Option<u32> {
    let raw = raw?;
    let pid = raw.to_str().and_then(|v| v.trim().parse::<u32>().ok());
    if pid.is_none() {
        tracing::warn!("ignoring HOSTLER_TEST_OWNER_PID={raw:?}: not a process id");
    }
    pid
}

/// The screen and the registered devices (P3b). Debug builds add `HOSTLER_TEST_FAKE_CATALOG=<n>`
/// fake devices.
fn catalogs(devices: &Arc<DeviceCache>) -> Vec<Box<dyn ResourceCatalog>> {
    #[allow(unused_mut)]
    let mut catalogs: Vec<Box<dyn ResourceCatalog>> = vec![
        Box::new(ScreenCatalog),
        Box::new(DeviceCatalog(devices.clone())),
    ];
    #[cfg(debug_assertions)]
    if let Some(n) = std::env::var("HOSTLER_TEST_FAKE_CATALOG")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        catalogs.push(Box::new(test_hooks::FakeCatalog(n)));
    }
    catalogs
}

/// The agent-device sessions from the device cache (P3b, L50–L54); debug builds also read
/// `HOSTLER_TEST_FAKE_FOREIGN=<path>` on each tick.
fn foreign_sources(devices: &Arc<DeviceCache>) -> Vec<Box<dyn ForeignClaimSource>> {
    #[allow(unused_mut)]
    let mut sources: Vec<Box<dyn ForeignClaimSource>> =
        vec![Box::new(AgentDeviceSource(devices.clone()))];
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("HOSTLER_TEST_FAKE_FOREIGN") {
        sources.push(Box::new(test_hooks::FakeForeign(path.into())));
    }
    sources
}

/// The only way to exercise `Any` selectors, device sharing and foreign claims end to end
/// before P3b.
#[cfg(debug_assertions)]
mod test_hooks {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::claims::{ForeignClaimSource, ResourceCatalog};
    use crate::contract::claims::{ForeignClaim, ResourceId, ResourceInfo, ResourceKind};

    /// `device:test:<i>`, kind `Device`, `attributes {test: "1"}`, display `fake-<i>`.
    pub struct FakeCatalog(pub u32);

    impl ResourceCatalog for FakeCatalog {
        fn resources(&self) -> Vec<ResourceInfo> {
            (0..self.0)
                .map(|i| ResourceInfo {
                    id: ResourceId(format!("device:test:{i}")),
                    kind: ResourceKind::Device,
                    display: format!("fake-{i}"),
                    attributes: BTreeMap::from([("test".to_owned(), "1".to_owned())]),
                    available: true,
                })
                .collect()
        }
    }

    /// Claims read from a JSON file (`[ForeignClaim]`); missing or unreadable means none.
    pub struct FakeForeign(pub PathBuf);

    impl ForeignClaimSource for FakeForeign {
        fn foreign(&self) -> Vec<ForeignClaim> {
            std::fs::read(&self.0)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default()
        }
    }
}

/// New databases: Argon2id calibrated to about 1 s; debug builds honour
/// `HOSTLER_TEST_FAST_KDF=1` (tests).
fn kdf_choice() -> KdfChoice {
    #[cfg(debug_assertions)]
    if std::env::var_os("HOSTLER_TEST_FAST_KDF").is_some_and(|v| v == "1") {
        return KdfChoice::Fast;
    }
    KdfChoice::Calibrated {
        target: Duration::from_secs(1),
    }
}

/// Daily-rotated `log_dir/service.log`; the guard flushes on drop.
fn init_logging(paths: &Paths) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let appender = tracing_appender::rolling::daily(&paths.log_dir, "service.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}

/// How long a stop waits for the service to release its lock: longer than the service's own
/// 30 s drain of vault work (P2-5).
pub const STOP_WAIT: Duration = Duration::from_secs(40);

/// Why a running service was not stopped.
pub enum StopRefused {
    /// The service answered `ServiceBusy`: heavy entries, leases or a takeover (`details.running`,
    /// `leases`, `plans`), or an unlocked database for an automatic rule (`details.secrets`).
    Busy(HostlerError),
    /// `may_stop` refused before anything was sent (an automatic rule only).
    NotStopped(lifecycle::StopRefusal),
    Failed(HostlerError),
}

fn stop_failed(message: String) -> StopRefused {
    StopRefused::Failed(HostlerError {
        code: ErrorCode::Internal,
        message,
        details: json!({}),
        recovery: None,
    })
}

/// Stops the running service under `rule` (probe → `may_stop` → `service.shutdown`) and waits
/// until it has released its lock and exited, up to `STOP_WAIT`. `Ok(false)` when no service
/// was running. `registration` matters only to `Automatic(Maintain)`.
pub async fn stop_service(
    paths: &Paths,
    rule: lifecycle::StopRule,
    registration: manager::RegistrationState,
) -> Result<bool, StopRefused> {
    let Stopping::Accepted { process, .. } = request_stop(paths, rule, registration).await? else {
        return Ok(false);
    };
    wait_gone(paths, process.as_ref(), STOP_WAIT).await?;
    Ok(true)
}

/// Waits until the service that accepted a stop is gone: lock released and process exited.
/// An OS manager asked to start the service right after would otherwise still see the old job
/// running and do nothing.
pub async fn wait_gone(
    paths: &Paths,
    process: Option<&ProcessIdentity>,
    limit: Duration,
) -> Result<(), StopRefused> {
    let deadline = Instant::now() + limit;
    let table = SysProcessTable::new().ok();
    loop {
        let exited = match (process, &table) {
            (Some(id), Some(table)) => procs::liveness(id, table) != procs::Liveness::Alive,
            _ => true,
        };
        match lock::is_held(&paths.service_lock) {
            Ok(false) if exited => return Ok(()),
            Ok(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Ok(_) => {
                return Err(stop_failed(format!(
                    "the service accepted the stop but still runs after {limit:?}"
                )));
            }
            Err(e) => return Err(stop_failed(format!("{e:#}"))),
        }
    }
}

/// What a stop request found.
pub enum Stopping {
    NotRunning,
    /// The service accepted the stop; `process` is its identity when it could be read, and
    /// `status` what it reported just before.
    Accepted {
        process: Option<ProcessIdentity>,
        status: Box<ServiceStatus>,
    },
}

/// The one way any path stops a service (plan P4 R2-2), without waiting for the exit: on one
/// lifecycle connection, `service.status` (the probe), then `lifecycle::may_stop(rule, …)`, then
/// `service.shutdown` with the rule's fields: `Explicit { force }` sends `if_idle: !force,
/// refuse_if_unlocked: false`; every `Automatic` rule sends `if_idle: true, refuse_if_unlocked:
/// true`, which the service re-checks atomically under its engine locks. The stop reaches the
/// very service that was probed. A `ServiceBusy` is a refusal, never retried with less.
pub async fn request_stop(
    paths: &Paths,
    rule: lifecycle::StopRule,
    registration: manager::RegistrationState,
) -> Result<Stopping, StopRefused> {
    // `service.status` is frozen, so this works against any service version; a service that
    // cannot answer it cannot accept a shutdown either.
    let (mut client, status) = match lifecycle::probe_connection(paths).await {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(Stopping::NotRunning),
        Err(e) => return Err(StopRefused::Failed(e)),
    };
    let probe = lifecycle::Probe::Running {
        contract: status.contract_version,
        version: status.service_version.clone(),
        secrets_unlocked: status.secrets_unlocked,
        pid: status.pid,
    };
    lifecycle::may_stop(rule, lifecycle::this_client(), &probe, registration)
        .map_err(StopRefused::NotStopped)?;
    let process = SysProcessTable::new()
        .ok()
        .and_then(|table| procs::identity_of(&table, status.pid));
    let (if_idle, refuse_if_unlocked) = rule.shutdown_fields();
    match client
        .call(Request::ServiceShutdown {
            if_idle,
            refuse_if_unlocked,
        })
        .await
    {
        Ok(_) => {}
        Err(e) if e.code == ErrorCode::ServiceBusy => return Err(StopRefused::Busy(e)),
        Err(e) => return Err(StopRefused::Failed(e)),
    }
    Ok(Stopping::Accepted {
        process,
        status: Box::new(status),
    })
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use std::ffi::OsStr;

    use super::owner_pid;

    /// L58: the hook takes a decimal pid and ignores anything else.
    #[test]
    fn the_owner_pid_hook_reads_a_decimal_pid_only() {
        assert_eq!(owner_pid(Some(OsStr::new("4242"))), Some(4242));
        assert_eq!(owner_pid(Some(OsStr::new(" 4242\n"))), Some(4242));
        assert_eq!(owner_pid(Some(OsStr::new("sleep"))), None);
        assert_eq!(owner_pid(Some(OsStr::new("-1"))), None);
        assert_eq!(owner_pid(Some(OsStr::new(""))), None);
        assert_eq!(owner_pid(None), None);
    }
}
