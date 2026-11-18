//! The method table of contract 5 (P5, spec §6.3): every wire method once, with the type of its
//! answer, the client's bound, whether the person gate guards it (spec §8.5) and whether the
//! app's facade offers it. The CLI's `client::call_bound` reads the bounds from here, and the
//! Swift contract is generated from it (plan Task 3), so both sides share them.

use super::wire::Request;

/// The type of a method's answer (`result`). `Unit` is JSON `null`; `…List` is a JSON array of
/// the named type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answer {
    Unit,
    HelloReply,
    ServiceStatus,
    ShutdownAccepted,
    AcquireOutcome,
    QueueSnapshot,
    HeavyHistoryItemList,
    QueueStopOutcome,
    SecretsStatus,
    UnlockOutcome,
    ProofGrant,
    ProjectInfo,
    ProjectInfoList,
    SecretList,
    SetOutcome,
    Removed,
    ImportOutcome,
    SecretRequestInfo,
    SecretRequestInfoList,
    ShareOutcome,
    Unshared,
    /// Sensitive: values.
    MaterializedSecretList,
    /// Sensitive: one value.
    MaterializedSecret,
    WorkspaceInfo,
    WorkspaceInfoList,
    ClaimOutcome,
    ReleaseOutcome,
    SeizureRecord,
    Claim,
    ClaimsSnapshot,
    ClaimHistoryItemList,
    SeizePlan,
    SeizeOutcome,
    SeizeProgressReply,
    Overview,
    AccountInfo,
    AccountInfoList,
    AccountFieldSet,
    /// Sensitive: values.
    MaterializedAccount,
    AccountRequestInfo,
    AccountRequestInfoList,
    AccountAnswerOutcome,
    DevicesSnapshot,
    DeviceDetail,
    DeviceInfo,
    DeviceRemoved,
    InstallEventList,
    ToolStatus,
    ForgetRecord,
    ProjectsSnapshot,
    RequestsSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub answer: Answer,
    /// How long a client waits for the answer.
    pub timeout_ms: u64,
    /// Refused without a live proof on the connection (spec §8.5).
    pub gated: bool,
    /// Offered by the app's facade (`HostlerClient`, plan Task 3).
    pub app: bool,
}

/// The client's default bound.
pub const CALL_MS: u64 = 5_000;
/// `secrets.unlock` and `secrets.prove`: one KDF of opening (or creating) the database, maybe
/// calibrated to seconds.
pub const UNLOCK_MS: u64 = 120_000;
/// Methods that may open, re-read or save the KDBX (one KDF each).
pub const VAULT_MS: u64 = 60_000;
/// `devices.boot`: the emulator's start plus `bootstatus` (180 s), with room (P3b).
pub const DEVICE_BOOT_MS: u64 = 200_000;
/// `claims.seize_commit`: the service first reads agent-device's status once more, up to its
/// 5 s bound plus a lane wait (P-2, P2-9).
pub const SEIZE_COMMIT_MS: u64 = 15_000;
/// `devices.show`: with `fresh_sessions`, up to 6 s for a sessions read that started after the
/// call (L49, P2-15). One bound per method: a plain show gets the same.
pub const DEVICES_SHOW_MS: u64 = 10_000;
/// `heavy.stop`: SIGTERM, the grace, SIGKILL and the check that nothing is left.
pub const HEAVY_STOP_MS: u64 = 60_000;

const fn spec(name: &'static str, answer: Answer, timeout_ms: u64, app: bool) -> MethodSpec {
    MethodSpec {
        name,
        answer,
        timeout_ms,
        gated: false,
        app,
    }
}

/// Gated methods are all offered by the app (they are the person's).
const fn gated(name: &'static str, answer: Answer, timeout_ms: u64) -> MethodSpec {
    MethodSpec {
        name,
        answer,
        timeout_ms,
        gated: true,
        app: true,
    }
}

use Answer as A;

/// Every wire method once, in `Request`'s order.
pub const METHODS: &[MethodSpec] = &[
    spec("hello", A::HelloReply, CALL_MS, true),
    spec("service.status", A::ServiceStatus, CALL_MS, true),
    // P5-L3: the app never manages the service beyond the Start button.
    spec("service.shutdown", A::ShutdownAccepted, CALL_MS, false),
    spec("heavy.acquire", A::AcquireOutcome, CALL_MS, false),
    spec("heavy.attach", A::AcquireOutcome, CALL_MS, false),
    spec("heavy.started", A::Unit, CALL_MS, false),
    spec("heavy.release", A::Unit, CALL_MS, false),
    spec("heavy.withdraw", A::Unit, CALL_MS, true),
    spec("heavy.force_release", A::Unit, CALL_MS, false),
    spec("queue.snapshot", A::QueueSnapshot, CALL_MS, true),
    spec("queue.history", A::HeavyHistoryItemList, CALL_MS, true),
    spec("queue.set_capacity", A::Unit, CALL_MS, true),
    spec("secrets.status", A::SecretsStatus, CALL_MS, true),
    spec("secrets.unlock", A::UnlockOutcome, UNLOCK_MS, true),
    spec("secrets.lock", A::SecretsStatus, CALL_MS, true),
    spec("project.resolve", A::ProjectInfo, CALL_MS, true),
    spec("project.list", A::ProjectInfoList, CALL_MS, true),
    spec("project.approve", A::ProjectInfo, CALL_MS, true),
    spec("secret.list", A::SecretList, VAULT_MS, true),
    spec("secret.set", A::SetOutcome, VAULT_MS, true),
    spec("secret.remove", A::Removed, VAULT_MS, true),
    spec("secret.import", A::ImportOutcome, VAULT_MS, false),
    spec("secret.request", A::SecretRequestInfo, VAULT_MS, false),
    spec(
        "secret.request_withdraw",
        A::SecretRequestInfo,
        CALL_MS,
        false,
    ),
    spec("secret.requests", A::SecretRequestInfoList, CALL_MS, true),
    spec("secret.decline", A::SecretRequestInfoList, CALL_MS, true),
    spec("secret.share", A::ShareOutcome, VAULT_MS, true),
    spec("secret.unshare", A::Unshared, CALL_MS, true),
    spec(
        "secret.materialize",
        A::MaterializedSecretList,
        VAULT_MS,
        false,
    ),
    spec("workspace.resolve", A::WorkspaceInfo, CALL_MS, true),
    spec("workspace.list", A::WorkspaceInfoList, CALL_MS, true),
    spec("claims.acquire", A::ClaimOutcome, CALL_MS, false),
    spec("claims.attach", A::ClaimOutcome, CALL_MS, false),
    spec("claims.started", A::Unit, CALL_MS, false),
    spec("claims.release", A::ReleaseOutcome, CALL_MS, false),
    spec("claims.force_release", A::SeizureRecord, CALL_MS, false),
    spec("claims.withdraw", A::Unit, CALL_MS, true),
    spec("claims.renew", A::Claim, CALL_MS, true),
    spec("claims.set_command", A::Unit, CALL_MS, false),
    spec("claims.reserve", A::Claim, CALL_MS, true),
    spec("claims.unreserve", A::Unit, CALL_MS, true),
    spec("claims.snapshot", A::ClaimsSnapshot, CALL_MS, true),
    spec("claims.history", A::ClaimHistoryItemList, CALL_MS, true),
    spec("claims.seize_begin", A::SeizePlan, CALL_MS, false),
    spec(
        "claims.seize_commit",
        A::SeizeOutcome,
        SEIZE_COMMIT_MS,
        false,
    ),
    spec("claims.seize_abort", A::Unit, CALL_MS, false),
    spec(
        "claims.seize_progress",
        A::SeizeProgressReply,
        CALL_MS,
        false,
    ),
    spec("status.overview", A::Overview, CALL_MS, true),
    spec("account.list", A::AccountInfoList, VAULT_MS, true),
    spec("account.set", A::AccountFieldSet, VAULT_MS, true),
    spec("account.remove", A::Removed, VAULT_MS, true),
    spec("account.check", A::AccountInfo, VAULT_MS, false),
    spec(
        "account.materialize",
        A::MaterializedAccount,
        VAULT_MS,
        false,
    ),
    spec("devices.list", A::DevicesSnapshot, CALL_MS, true),
    spec("devices.show", A::DeviceDetail, DEVICES_SHOW_MS, true),
    spec("devices.resolve", A::DeviceInfo, CALL_MS, true),
    // The registry calls wait for the claims lock and a store write; as long as a vault call.
    spec("devices.add", A::DeviceInfo, VAULT_MS, true),
    spec("devices.rename", A::DeviceInfo, VAULT_MS, true),
    spec("devices.note", A::DeviceInfo, CALL_MS, true),
    spec("devices.remove", A::DeviceRemoved, VAULT_MS, true),
    spec("devices.installs", A::InstallEventList, CALL_MS, true),
    spec("devices.boot", A::DeviceInfo, DEVICE_BOOT_MS, false),
    spec("devices.tools", A::ToolStatus, CALL_MS, true),
    spec("devices.forget_sessions", A::ForgetRecord, VAULT_MS, true),
    // ---- contract 5 ----
    spec("secrets.prove", A::ProofGrant, UNLOCK_MS, true),
    gated("secret.reveal", A::MaterializedSecret, VAULT_MS),
    gated("account.reveal", A::MaterializedAccount, VAULT_MS),
    gated("secrets.remove_preserved", A::SecretsStatus, VAULT_MS),
    spec("account.request", A::AccountRequestInfo, VAULT_MS, false),
    spec(
        "account.request_withdraw",
        A::AccountRequestInfo,
        VAULT_MS,
        false,
    ),
    spec(
        "account.requests",
        A::AccountRequestInfoList,
        VAULT_MS,
        true,
    ),
    gated("account.decline", A::AccountRequestInfo, VAULT_MS),
    gated("account.answer", A::AccountAnswerOutcome, VAULT_MS),
    gated("heavy.stop", A::QueueStopOutcome, HEAVY_STOP_MS),
    spec("projects.snapshot", A::ProjectsSnapshot, CALL_MS, true),
    spec("requests.snapshot", A::RequestsSnapshot, CALL_MS, true),
];

/// The spec of a wire method name.
pub fn method_spec(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|m| m.name == name)
}

/// The spec of a request; every variant is in `METHODS` (`methods_cover_every_request`).
pub fn spec_of(request: &Request) -> &'static MethodSpec {
    let name = method_name(request);
    method_spec(name).unwrap_or_else(|| panic!("{name} is missing from METHODS"))
}

/// The wire method of a request, an exhaustive match: a new variant does not compile without
/// its name here.
pub fn method_name(request: &Request) -> &'static str {
    match request {
        Request::Hello { .. } => "hello",
        Request::ServiceStatus => "service.status",
        Request::ServiceShutdown { .. } => "service.shutdown",
        Request::HeavyAcquire(_) => "heavy.acquire",
        Request::HeavyAttach { .. } => "heavy.attach",
        Request::HeavyStarted(_) => "heavy.started",
        Request::HeavyRelease(_) => "heavy.release",
        Request::HeavyWithdraw { .. } => "heavy.withdraw",
        Request::HeavyForceRelease { .. } => "heavy.force_release",
        Request::QueueSnapshot => "queue.snapshot",
        Request::QueueHistory(_) => "queue.history",
        Request::QueueSetCapacity { .. } => "queue.set_capacity",
        Request::SecretsStatus => "secrets.status",
        Request::SecretsUnlock(_) => "secrets.unlock",
        Request::SecretsLock => "secrets.lock",
        Request::ProjectResolve(_) => "project.resolve",
        Request::ProjectList => "project.list",
        Request::ProjectApprove(_) => "project.approve",
        Request::SecretList { .. } => "secret.list",
        Request::SecretSet(_) => "secret.set",
        Request::SecretRemove { .. } => "secret.remove",
        Request::SecretImport(_) => "secret.import",
        Request::SecretRequest(_) => "secret.request",
        Request::SecretRequestWithdraw { .. } => "secret.request_withdraw",
        Request::SecretRequests => "secret.requests",
        Request::SecretDecline(_) => "secret.decline",
        Request::SecretShare(_) => "secret.share",
        Request::SecretUnshare(_) => "secret.unshare",
        Request::SecretMaterialize(_) => "secret.materialize",
        Request::WorkspaceResolve(_) => "workspace.resolve",
        Request::WorkspaceList => "workspace.list",
        Request::ClaimsAcquire(_) => "claims.acquire",
        Request::ClaimsAttach { .. } => "claims.attach",
        Request::ClaimsStarted(_) => "claims.started",
        Request::ClaimsRelease(_) => "claims.release",
        Request::ClaimsForceRelease { .. } => "claims.force_release",
        Request::ClaimsWithdraw { .. } => "claims.withdraw",
        Request::ClaimsRenew { .. } => "claims.renew",
        Request::ClaimsSetCommand { .. } => "claims.set_command",
        Request::ClaimsReserve { .. } => "claims.reserve",
        Request::ClaimsUnreserve { .. } => "claims.unreserve",
        Request::ClaimsSnapshot => "claims.snapshot",
        Request::ClaimsHistory(_) => "claims.history",
        Request::ClaimsSeizeBegin(_) => "claims.seize_begin",
        Request::ClaimsSeizeCommit(_) => "claims.seize_commit",
        Request::ClaimsSeizeAbort { .. } => "claims.seize_abort",
        Request::ClaimsSeizeProgress(_) => "claims.seize_progress",
        Request::StatusOverview => "status.overview",
        Request::AccountList { .. } => "account.list",
        Request::AccountSet(_) => "account.set",
        Request::AccountRemove(_) => "account.remove",
        Request::AccountCheck { .. } => "account.check",
        Request::AccountMaterialize(_) => "account.materialize",
        Request::DevicesList { .. } => "devices.list",
        Request::DevicesShow { .. } => "devices.show",
        Request::DevicesResolve { .. } => "devices.resolve",
        Request::DevicesAdd { .. } => "devices.add",
        Request::DevicesRename { .. } => "devices.rename",
        Request::DevicesNote { .. } => "devices.note",
        Request::DevicesRemove { .. } => "devices.remove",
        Request::DevicesInstalls { .. } => "devices.installs",
        Request::DevicesBoot { .. } => "devices.boot",
        Request::DevicesTools(_) => "devices.tools",
        Request::DevicesForgetSessions { .. } => "devices.forget_sessions",
        Request::SecretsProve(_) => "secrets.prove",
        Request::SecretReveal { .. } => "secret.reveal",
        Request::AccountReveal { .. } => "account.reveal",
        Request::SecretsRemovePreserved { .. } => "secrets.remove_preserved",
        Request::AccountRequest(_) => "account.request",
        Request::AccountRequestWithdraw { .. } => "account.request_withdraw",
        Request::AccountRequests => "account.requests",
        Request::AccountDecline { .. } => "account.decline",
        Request::AccountAnswer(_) => "account.answer",
        Request::HeavyStop { .. } => "heavy.stop",
        Request::ProjectsSnapshot => "projects.snapshot",
        Request::RequestsSnapshot => "requests.snapshot",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use super::*;
    use crate::contract::wire::{sensitive_answer, tests::all_requests};

    /// `method_name` agrees with the serde tag for every variant; every variant is in `METHODS`
    /// once and nothing else is; `gated` is exactly spec §8.5's six; `call_bound` is the table.
    #[test]
    fn methods_cover_every_request() {
        let requests = all_requests();
        let mut seen = HashSet::new();
        for request in &requests {
            let tag = serde_json::to_value(request).unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(method_name(request), tag);
            assert!(seen.insert(tag.clone()), "{tag} twice in all_requests()");
            assert_eq!(
                crate::client::call_bound(request),
                Duration::from_millis(spec_of(request).timeout_ms),
                "{tag}"
            );
        }
        let names: Vec<&str> = METHODS.iter().map(|m| m.name).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "a method is in METHODS twice");
        assert_eq!(
            unique,
            seen.iter().map(String::as_str).collect::<HashSet<_>>(),
            "METHODS and the Request variants differ"
        );
        let gated: Vec<&str> = METHODS.iter().filter(|m| m.gated).map(|m| m.name).collect();
        assert_eq!(
            gated,
            [
                "secret.reveal",
                "account.reveal",
                "secrets.remove_preserved",
                "account.decline",
                "account.answer",
                "heavy.stop",
            ]
        );
    }

    /// The plan's `app: false` list, exactly; no app method answers a materialize.
    #[test]
    fn app_methods_are_the_plans() {
        let not_app: Vec<&str> = METHODS.iter().filter(|m| !m.app).map(|m| m.name).collect();
        assert_eq!(
            not_app,
            [
                "service.shutdown",
                "heavy.acquire",
                "heavy.attach",
                "heavy.started",
                "heavy.release",
                "heavy.force_release",
                "secret.import",
                "secret.request",
                "secret.request_withdraw",
                "secret.materialize",
                "claims.acquire",
                "claims.attach",
                "claims.started",
                "claims.release",
                "claims.force_release",
                "claims.set_command",
                "claims.seize_begin",
                "claims.seize_commit",
                "claims.seize_abort",
                "claims.seize_progress",
                "account.check",
                "account.materialize",
                "devices.boot",
                "account.request",
                "account.request_withdraw",
            ]
        );
        for m in METHODS {
            if m.name.ends_with(".materialize") {
                assert!(!m.app, "{}", m.name);
            }
        }
    }

    /// The sensitive answers are exactly the value-carrying answer types.
    #[test]
    fn sensitive_answers_follow_the_table() {
        for m in METHODS {
            let values = matches!(
                m.answer,
                Answer::MaterializedSecret
                    | Answer::MaterializedSecretList
                    | Answer::MaterializedAccount
            );
            assert_eq!(sensitive_answer(m.name), values, "{}", m.name);
        }
    }
}
