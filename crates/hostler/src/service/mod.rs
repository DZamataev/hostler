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
use tokio::sync::broadcast;

use crate::bridge::looky::LookyHeavy;
use crate::client::Client;
use crate::contract::wire::Request;
use crate::contract::{ErrorCode, HostlerError};
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
    let swept = crate::secrets::rundir::sweep(&paths.runs);
    if swept > 0 {
        tracing::info!("removed {swept} leftover secret run director(ies)");
    }
    let table: Arc<dyn ProcessTable> =
        Arc::new(SysProcessTable::new().context("cannot read the boot id")?);
    let store = Store::open(&paths.db)?;
    let (events, _) = broadcast::channel(64);
    let engine = HeavyEngine::new(
        store,
        table.clone(),
        Arc::new(SystemClock),
        Box::new(LookyHeavy::new()),
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
    let listener = server::bind(&paths.socket)?;
    tracing::info!(
        "hostler service {} (pid {}) listening on {:?}",
        lifecycle::SERVICE_VERSION,
        std::process::id(),
        paths.socket
    );
    // Dropping the listener removes the socket file while the lock is still held.
    server::serve(listener, engine, secrets, events).await
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
    /// `if_idle` and heavy entries run: `details.running` lists them.
    Busy(HostlerError),
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

/// Asks the running service to stop (refused while heavy entries run unless `force`) and waits
/// until it has released its lock. `Ok(false)` when no service was running.
pub async fn stop_service(paths: &Paths, force: bool) -> Result<bool, StopRefused> {
    let Stopping::Accepted { process } = request_stop(paths, force).await? else {
        return Ok(false);
    };
    let deadline = Instant::now() + STOP_WAIT;
    // Gone means: lock released and process exited. An OS manager asked to start the service
    // right after would otherwise still see the old job running and do nothing.
    let table = SysProcessTable::new().ok();
    loop {
        let exited = match (&process, &table) {
            (Some(id), Some(table)) => procs::liveness(id, table) != procs::Liveness::Alive,
            _ => true,
        };
        match lock::is_held(&paths.service_lock) {
            Ok(false) if exited => return Ok(true),
            Ok(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Ok(_) => {
                return Err(stop_failed(format!(
                    "the service accepted the stop but still runs after {STOP_WAIT:?}"
                )));
            }
            Err(e) => return Err(stop_failed(format!("{e:#}"))),
        }
    }
}

/// What a stop request found.
pub enum Stopping {
    NotRunning,
    /// The service accepted the stop; `process` is its identity when it could be read.
    Accepted {
        process: Option<ProcessIdentity>,
    },
}

/// Sends `service.shutdown { if_idle: !force }` without waiting for the exit.
pub async fn request_stop(paths: &Paths, force: bool) -> Result<Stopping, StopRefused> {
    let mut client = match Client::connect_lifecycle(paths).await {
        Ok(client) => client,
        Err(e) if e.details["reason"] == "not_running" => return Ok(Stopping::NotRunning),
        Err(e) => return Err(StopRefused::Failed(e)),
    };
    // `service.status` is frozen, so this works against any service version; a service that
    // cannot answer it cannot accept a shutdown either.
    let status = client
        .call(Request::ServiceStatus)
        .await
        .map_err(StopRefused::Failed)?;
    let process = status["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .and_then(|pid| procs::identity_of(&SysProcessTable::new().ok()?, pid));
    match client
        .call(Request::ServiceShutdown { if_idle: !force })
        .await
    {
        Ok(_) => {}
        Err(e) if e.code == ErrorCode::ServiceBusy => return Err(StopRefused::Busy(e)),
        Err(e) => return Err(StopRefused::Failed(e)),
    }
    Ok(Stopping::Accepted { process })
}
