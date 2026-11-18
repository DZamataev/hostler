//! The values the fixtures are written from (plan P5 Task 3): one per answer type in the method
//! table, one `HostlerError` per `ErrorCode`, and extra values for the variants of tagged enums
//! that the requests, events and answers do not reach (`fixtures_reach_every_enum_variant`).
//! The values are made up; no fixture holds a real secret.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;
use serde_json::{Value, json};

use crate::contract::accounts::*;
use crate::contract::claims::{self, *};
use crate::contract::devices::{self, *};
use crate::contract::doctor::*;
use crate::contract::heavy::*;
use crate::contract::lifecycle::HelloReply;
use crate::contract::methods::Answer;
use crate::contract::process::{ProcessIdentity, RunGroup};
use crate::contract::projects;
use crate::contract::secrets::*;
use crate::contract::values::SecretValue;
use crate::contract::wire::RequestsSnapshot;
use crate::contract::{CONTRACT_VERSION, Envelope, ErrorCode, HostlerError};

/// A sample of one named type: registers the type in a generator (answering its `$ref`) and
/// holds its JSON.
pub struct Sample {
    pub schema: fn(&mut SchemaGenerator) -> Schema,
    pub value: Value,
}

fn sample<T: JsonSchema + Serialize>(value: T) -> Sample {
    Sample {
        schema: |g| g.subschema_for::<T>(),
        value: serde_json::to_value(value).unwrap(),
    }
}

/// The answer of a method: `Unit` (JSON `null`), one value or a list of the type.
pub enum AnswerSample {
    Unit,
    One(Sample),
    List(Sample),
}

fn one<T: JsonSchema + Serialize>(value: T) -> AnswerSample {
    AnswerSample::One(sample(value))
}

/// A list's sample is one element; the fixture is an array of it.
fn list<T: JsonSchema + Serialize>(value: T) -> AnswerSample {
    AnswerSample::List(sample(value))
}

fn identity(pid: u32) -> ProcessIdentity {
    claims::fixtures::identity(pid)
}

fn project_info() -> ProjectInfo {
    ProjectInfo {
        id: "p1".into(),
        name: "app".into(),
        common_dir: "/work/app/.git".into(),
        approved: true,
        group_uuid: Some("8a7c2f4e-1b0d-4c55-9e1a-2f3b4c5d6e7f".into()),
        created_at_ms: 1_000,
        approved_at_ms: Some(2_000),
    }
}

fn heavy_entry(state: HeavyState) -> HeavyEntry {
    HeavyEntry {
        id: "h1".into(),
        ticket: 3,
        kind: "gradle".into(),
        command: "./gradlew build".into(),
        cwd: "/work/app".into(),
        workspace_root: Some("/work/app".into()),
        holder: identity(42),
        child: Some(identity(43)),
        group: Some(RunGroup::WindowsJob {
            name: "hostler-h1".into(),
            boot_id: "boot-1".into(),
            session: Some(2),
        }),
        state,
        enqueued_at_ms: 1_000,
        granted_at_ms: Some(1_100),
        source: EntrySource::Hostler,
    }
}

fn heavy_history_item() -> HeavyHistoryItem {
    HeavyHistoryItem {
        id: "h0".into(),
        ticket: 2,
        kind: "xcodebuild".into(),
        command: "xcodebuild test".into(),
        cwd: "/work/app/ios".into(),
        workspace_root: None,
        enqueued_at_ms: 500,
        granted_at_ms: Some(600),
        finished_at_ms: 900,
        exit_code: Some(65),
        outcome: HeavyOutcome::Failed,
    }
}

fn queue_snapshot() -> QueueSnapshot {
    QueueSnapshot {
        capacity: 2,
        running: vec![heavy_entry(HeavyState::Running)],
        waiting: vec![HeavyEntry {
            id: "h2".into(),
            ticket: 4,
            child: None,
            group: None,
            granted_at_ms: None,
            state: HeavyState::Waiting,
            ..heavy_entry(HeavyState::Waiting)
        }],
        foreign_running: vec![HeavyEntry {
            source: EntrySource::RnLookyLedger,
            ..heavy_entry(HeavyState::Running)
        }],
        as_of_seq: 12,
    }
}

fn secrets_status() -> SecretsStatus {
    SecretsStatus {
        state: LockState::Unlocked,
        database: "/state/hostler.kdbx".into(),
        key_file: Some("/keys/hostler.keyx".into()),
        pending_requests: 1,
        preserved_files: vec!["/state/hostler.kdbx.prev-1".into()],
        as_of_seq: 14,
    }
}

fn secret_request_info() -> SecretRequestInfo {
    SecretRequestInfo {
        id: "6f1e6a9a".into(),
        project_id: "p1".into(),
        project_name: "app".into(),
        name: "API_KEY".into(),
        reason: "the e2e tests".into(),
        state: RequestState::Pending,
        requester_pid: 42,
        created_at_ms: 1_000,
        answered_at_ms: None,
    }
}

fn account_request_info() -> AccountRequestInfo {
    AccountRequestInfo {
        id: "6f1e6a9b".into(),
        project_id: "p1".into(),
        project_name: "app".into(),
        name: "qa-user".into(),
        fields: vec!["session".into(), "env".into()],
        reason: "the e2e tests".into(),
        state: RequestState::Pending,
        requester_pid: 42,
        created_at_ms: 1_000,
        answered_at_ms: None,
    }
}

fn workspace_info() -> WorkspaceInfo {
    WorkspaceInfo {
        id: "ws-1".into(),
        project_id: "p1".into(),
        path: "/work/app".into(),
        outcome: WorkspaceOutcome::Moved,
        project_repointed: false,
        note: Some("moved from /work/old".into()),
        last_seen_ms: 3_000,
    }
}

fn blockers() -> Vec<Blocker> {
    vec![
        Blocker::Claim {
            summary: claims::fixtures::summary(),
        },
        Blocker::Foreign {
            claim: claims::fixtures::foreign(),
        },
        Blocker::Seizing {
            by_workspace: "/work/taker".into(),
        },
        Blocker::Checking {
            resource: "device:android:SERIAL0001".into(),
            text: "checking agent-device sessions".into(),
        },
    ]
}

fn materialized(name: &str, value: &str) -> MaterializedSecret {
    MaterializedSecret {
        name: name.into(),
        value: SecretValue::new(value.into()),
    }
}

fn device_detail() -> DeviceDetail {
    DeviceDetail {
        info: devices::fixtures::info(),
        apps: vec![
            AppState {
                app_id: "com.example.app".into(),
                version_name: Some("1.2.0".into()),
                version_code: Some("120".into()),
                installed: Some(InstallTime::FirstSeen { at_ms: 1_000 }),
                updated: Some(InstallTime::ChangedBetween {
                    from_ms: 3_000,
                    to_ms: 4_000,
                }),
            },
            AppState {
                app_id: "com.example.other".into(),
                version_name: None,
                version_code: None,
                installed: None,
                updated: None,
            },
        ],
        snapshot_at_ms: Some(5_000),
        forgets: vec![forget_record()],
        sessions_unknown: Some("agent-device did not answer".into()),
    }
}

fn forget_record() -> ForgetRecord {
    ForgetRecord {
        id: "f1".into(),
        resource: devices::fixtures::resource(),
        sessions: vec!["default".into()],
        by_workspace_path: Some("/work/app".into()),
        by_pid: Some(42),
        reason: "the session was left over".into(),
        at_ms: 4_800,
    }
}

fn tool_status() -> ToolStatus {
    ToolStatus {
        adb: None,
        adb_state: AdbState::Missing,
        emulator: Some("/sdk/emulator/emulator".into()),
        xcrun: true,
        ideviceinfo: Some("/opt/homebrew/bin/ideviceinfo".into()),
        agent_device: None,
        node: None,
        agent_device_in_use: false,
        sessions_known: false,
        sessions_last_good_ms: None,
        sessions_reason: Some("agent-device is not installed".into()),
        problems: Vec::new(),
    }
}

fn doctor_report() -> DoctorReport {
    DoctorReport {
        checks: vec![
            Check {
                id: "service.registered".into(),
                status: CheckStatus::Ok,
                message: "the service is registered".into(),
                fix: None,
                details: json!({}),
            },
            Check {
                id: "secrets.required.API_KEY".into(),
                status: CheckStatus::Problem,
                message: "API_KEY is not set".into(),
                fix: Some(Fix {
                    command: Some("hostler secret set API_KEY".into()),
                    text: "a person sets it".into(),
                    who: Who::Person,
                    safe: false,
                }),
                details: json!({ "name": "API_KEY" }),
            },
        ],
        fixed: vec![FixOutcome {
            check: "service.registered".into(),
            command: "hostler service install".into(),
            result: FixResult::Fixed,
            message: "registered".into(),
        }],
    }
}

/// The sample of every answer type, an exhaustive match: a new answer does not compile without
/// one.
pub fn answer(answer: Answer) -> AnswerSample {
    use Answer as A;
    match answer {
        A::Unit => AnswerSample::Unit,
        A::HelloReply => one(HelloReply {
            contract_version: CONTRACT_VERSION,
            service_version: "0.0.4".into(),
            boot_id: Some("boot-1".into()),
            instance: Some("1b4e28ba-2fa1-41d2-883f-0016d3cca427".into()),
        }),
        A::ServiceStatus => one(ServiceStatus {
            service_version: "0.0.4".into(),
            contract_version: CONTRACT_VERSION,
            pid: 4242,
            running_heavy: 1,
            waiting_heavy: 0,
            secrets_unlocked: Some(true),
            boot_id: Some("boot-1".into()),
            instance: Some("1b4e28ba-2fa1-41d2-883f-0016d3cca427".into()),
            exe: Some("/Users/someone/.cargo/bin/hostler".into()),
        }),
        A::ShutdownAccepted => one(ShutdownAccepted { stopping: true }),
        A::AcquireOutcome => one(AcquireOutcome::Queued {
            entry: heavy_entry(HeavyState::Waiting),
            position: 2,
            of: 3,
        }),
        A::QueueSnapshot => one(queue_snapshot()),
        A::HeavyHistoryItemList => list(heavy_history_item()),
        A::QueueStopOutcome => one(QueueStopOutcome {
            id: "h1".into(),
            stopped: true,
            pids: vec![42, 43],
        }),
        A::SecretsStatus => one(secrets_status()),
        A::UnlockOutcome => one(UnlockOutcome {
            status: secrets_status(),
            grant: ProofGrant {
                expires_at_ms: 121_000,
            },
        }),
        A::ProofGrant => one(ProofGrant {
            expires_at_ms: 121_000,
        }),
        A::ProjectInfo => one(project_info()),
        A::ProjectInfoList => list(project_info()),
        A::SecretList => one(SecretList {
            project: project_info(),
            secrets: vec![
                SecretInfo {
                    name: "API_KEY".into(),
                    purpose: Some("the indexer".into()),
                    declared: true,
                    state: SecretState::Set,
                    source: Some(SecretSource::Own),
                },
                SecretInfo {
                    name: "SHARED_TOKEN".into(),
                    purpose: None,
                    declared: false,
                    state: SecretState::Set,
                    source: Some(SecretSource::SharedFrom {
                        project_id: "p2".into(),
                        project: "lib".into(),
                    }),
                },
                SecretInfo {
                    name: "MISSING_ONE".into(),
                    purpose: Some("declared, not set".into()),
                    declared: true,
                    state: SecretState::Missing,
                    source: None,
                },
            ],
        }),
        A::SetOutcome => one(SetOutcome {
            name: "API_KEY".into(),
            answered_requests: vec!["6f1e6a9a".into()],
        }),
        A::Removed => one(Removed { removed: true }),
        A::ImportOutcome => one(ImportOutcome {
            stored: vec!["API_KEY".into(), "OTHER_KEY".into()],
            answered_requests: Vec::new(),
        }),
        A::SecretRequestInfo => one(secret_request_info()),
        A::SecretRequestInfoList => list(SecretRequestInfo {
            state: RequestState::Declined,
            answered_at_ms: Some(2_000),
            ..secret_request_info()
        }),
        A::ShareOutcome => one(ShareOutcome {
            name: "API_KEY".into(),
            from: "p1".into(),
            to: "p2".into(),
        }),
        A::Unshared => one(Unshared { unshared: true }),
        A::MaterializedSecretList => list(materialized("API_KEY", "fixture-value-1")),
        A::MaterializedSecret => one(materialized("API_KEY", "fixture-value-2")),
        A::WorkspaceInfo => one(workspace_info()),
        A::WorkspaceInfoList => list(workspace_info()),
        A::ClaimOutcome => one(ClaimOutcome::Granted {
            claim: claims::fixtures::claim(),
        }),
        A::ReleaseOutcome => one(ReleaseOutcome {
            parent: Some(claims::fixtures::claim()),
            seized_by: Some("/work/taker".into()),
        }),
        A::SeizureRecord => one(claims::fixtures::seizure()),
        A::Claim => one(claims::fixtures::claim()),
        A::ClaimsSnapshot => one(claims::fixtures::snapshot()),
        A::ClaimHistoryItemList => list(claims::fixtures::history_item()),
        A::SeizePlan => one(SeizePlan {
            plan_id: "plan-1".into(),
            claims: vec![claims::fixtures::claim()],
            foreign: vec![claims::fixtures::foreign()],
            stops: vec![
                StopTarget::RunGroup {
                    claim_id: "c1".into(),
                    holder: identity(42),
                    child: Some(identity(43)),
                    group: Some(claims::fixtures::group()),
                },
                StopTarget::Process {
                    claim_id: "c2".into(),
                    process: identity(44),
                },
                StopTarget::Foreign {
                    source: "agent-device".into(),
                    resource: devices::fixtures::resource(),
                    data: json!({ "session": "s1" }),
                },
            ],
            expires_at_ms: 9_000,
        }),
        A::SeizeOutcome => one(SeizeOutcome {
            outcome: ClaimOutcome::Busy {
                position: 1,
                of: 2,
                blockers: blockers(),
            },
            attested: true,
        }),
        A::SeizeProgressReply => one(SeizeProgressReply {
            expires_at_ms: 9_500,
        }),
        A::Overview => one(Overview {
            service_version: "0.0.4".into(),
            heavy: queue_snapshot(),
            claims: claims::fixtures::snapshot(),
            secrets: secrets_status(),
        }),
        A::AccountInfo => one(AccountInfo {
            name: "qa-user".into(),
            fields: vec!["session".into(), "env".into()],
            holder: Some(claims::fixtures::summary()),
        }),
        A::AccountInfoList => list(AccountInfo {
            name: "qa-admin".into(),
            fields: vec!["token".into()],
            holder: None,
        }),
        A::AccountFieldSet => one(AccountFieldSet {
            name: "qa-user".into(),
            field: "session".into(),
        }),
        A::MaterializedAccount => one(MaterializedAccount {
            name: "qa-user".into(),
            fields: vec![
                materialized("session", "fixture-value-3"),
                materialized("env", "fixture-value-4"),
            ],
        }),
        A::AccountRequestInfo => one(account_request_info()),
        A::AccountRequestInfoList => list(account_request_info()),
        A::AccountAnswerOutcome => one(AccountAnswerOutcome {
            name: "qa-user".into(),
            stored_fields: vec!["session".into(), "env".into()],
            answered_requests: vec!["6f1e6a9b".into()],
        }),
        A::DevicesSnapshot => one(devices::fixtures::snapshot()),
        A::DeviceDetail => one(device_detail()),
        A::DeviceInfo => one(devices::fixtures::info()),
        A::DeviceRemoved => one(DeviceRemoved {
            removed: devices::fixtures::resource(),
        }),
        A::InstallEventList => list(devices::fixtures::install_event()),
        A::ToolStatus => one(tool_status()),
        A::ForgetRecord => one(forget_record()),
        A::ProjectsSnapshot => one(projects::fixtures::snapshot()),
        A::RequestsSnapshot => one(RequestsSnapshot {
            secrets: vec![secret_request_info()],
            accounts: vec![account_request_info()],
            as_of_seq: 15,
        }),
    }
}

/// Answers outside the method table that the app decodes: `hostler doctor --json`'s report.
pub fn other_answers() -> Vec<(&'static str, Sample)> {
    vec![("DoctorReport", sample(doctor_report()))]
}

/// One `HostlerError` per code, an exhaustive match; the service's own constructor where there
/// is one, so the details have their real shape.
pub fn error(code: ErrorCode) -> HostlerError {
    use ErrorCode as E;
    let plain = |message: &str, recovery: Option<&str>| HostlerError {
        code,
        message: message.into(),
        details: json!({}),
        recovery: recovery.map(str::to_owned),
    };
    match code {
        E::ServiceUnavailable => crate::contract::secrets::errors::service_stopping(),
        E::ContractMismatch => HostlerError {
            code,
            message: "this hostler speaks contract 5, the service contract 4".into(),
            details: json!({
                "client_contract": 5,
                "service_contract": 4,
                "service_version": "0.0.4",
            }),
            recovery: Some("hostler service start --replace".into()),
        },
        E::ServiceBusy => HostlerError {
            code,
            message: "the service has live work".into(),
            details: json!({ "secrets": "unlocked" }),
            recovery: None,
        },
        E::BadRequest => crate::contract::devices::errors::not_available_yet("heavy.stop"),
        E::UnknownEntry => plain("no heavy entry h9", None),
        E::QueueTimeout => plain(
            "gave up after 60 min in the heavy queue",
            Some("hostler queue"),
        ),
        E::QueueBusy => plain("the heavy queue is full", Some("hostler queue")),
        E::Withdrawn => plain("the request was withdrawn", None),
        E::CapacityOutOfRange => plain("capacity must be 1 to 16", None),
        E::ForceRequired => {
            claims::errors::force_required("screen", "/work/app", "hostler hold screen", "screen")
        }
        E::Internal => plain("an internal error", None),
        E::NotInteractive => crate::contract::secrets::errors::not_interactive("unlock"),
        E::NotInProject => plain("not inside a project", None),
        E::ConfigInvalid => plain("hostler.toml line 3: unknown key", Some("hostler doctor")),
        E::ProjectNotApproved => {
            crate::contract::secrets::errors::project_not_approved("app", "/work/app/.git")
        }
        E::SecretsLocked => crate::contract::secrets::errors::secrets_locked(false),
        E::UnlockFailed => crate::contract::secrets::errors::unlock_failed(),
        E::SecretsConflict => crate::contract::secrets::errors::secrets_conflict("changed key"),
        E::SecretMissing => crate::contract::secrets::errors::secret_missing("API_KEY", "app"),
        E::SecretNotInProject => {
            crate::contract::secrets::errors::secret_not_in_project("API_KEY", "lib", "app")
        }
        E::SecretExists => crate::contract::secrets::errors::secret_exists(&["API_KEY".to_owned()]),
        E::SecretTooShort => crate::contract::secrets::errors::secret_too_short(),
        E::RequestTimeout => plain("nobody answered within 5 min", None),
        E::UnknownResource => claims::errors::unknown_resource("device:x:1"),
        E::ResourceInUse => claims::errors::resource_in_use("screen", "/work/app"),
        E::ClaimNotHeld => claims::errors::claim_not_held("c1"),
        E::SeizureFailed => claims::errors::seizure_failed("screen", "the holder did not stop"),
        E::AccountMissing => claims::errors::account_missing("qa-user", "app"),
        E::ClaimDeadlock => claims::errors::claim_deadlock("screen", "a waits for b"),
        E::TakenOver => plain("the run is being taken over", None),
        E::DeviceNotFound => crate::contract::devices::errors::device_not_found("pixel-9"),
        E::AliasTaken => crate::contract::devices::errors::alias_taken("pixel-9", "pixel-8"),
        E::DeviceNotRegistered => {
            crate::contract::devices::errors::device_not_registered("SERIAL0001", true)
        }
        E::DeviceNotReady => crate::contract::devices::errors::device_not_ready("emu-1"),
        E::BootFailed => crate::contract::devices::errors::boot_failed(
            "emu-1",
            "the emulator exited",
            "/state/logs/emu-1.log",
        ),
        E::ToolMissing => crate::contract::devices::errors::tool_missing(
            crate::contract::devices::errors::MissingTool::Adb,
        ),
        E::PermissionDenied => crate::contract::devices::errors::person_required("add"),
        E::ProofRequired => crate::contract::secrets::errors::proof_required(),
        E::ProofLimited => crate::contract::secrets::errors::proof_limited(4_000),
        E::StopFailed => crate::contract::heavy::errors::stop_failed("h1", &[42, 43]),
    }
}

/// Values of the tagged-enum variants nothing else reaches. Only wire types: a type no request,
/// event or answer reaches (`ForeignView`, `Freshness`) is not generated.
pub fn extra_values() -> Vec<Sample> {
    let entry = heavy_entry(HeavyState::Stopping);
    let mut attributes = BTreeMap::new();
    attributes.insert("platform".to_owned(), "android".to_owned());
    vec![
        sample(AcquireOutcome::Granted {
            entry: entry.clone(),
        }),
        sample(AcquireOutcome::Reentrant),
        sample(AcquireOutcome::Busy { position: 1, of: 1 }),
        sample(AcquireOutcome::Finished {
            item: heavy_history_item(),
        }),
        sample(ClaimOutcome::Joined {
            claim: claims::fixtures::claim(),
        }),
        sample(ClaimOutcome::Reentrant {
            claim_id: "c1".into(),
        }),
        sample(ClaimOutcome::Queued {
            request_id: "r1".into(),
            position: 2,
            of: 2,
            blockers: blockers(),
        }),
        sample(ClaimOutcome::Finished {
            item: claims::fixtures::history_item(),
        }),
        sample(Selector::Any {
            kind: ResourceKind::Device,
            attributes,
        }),
        sample(HoldBinding::Process {
            holder: identity(42),
        }),
        sample(Purpose::Hold {
            ttl_ms: Some(1_800_000),
            bound_to: Some(identity(42)),
            distinct: true,
        }),
        sample(InstallSource::Around {
            claim_ids: vec!["c1".into()],
            workspace_path: "/work/app".into(),
        }),
        sample(InstallSource::NotThroughHostler),
        sample(AdbState::Ours),
        sample(Reading::<u8>::Emulated),
        sample(Reading::<u8>::Unknown),
        sample(StopEvidence::Gone { target: 0 }),
        sample(StopEvidence::JobGone {
            target: 1,
            session: 2,
        }),
    ]
}

/// The type parameter of a generic contract type (`Envelope<T>`): its schema is a marker the
/// emitter reads as Swift's `T`.
pub struct TypeParam;

impl JsonSchema for TypeParam {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "T".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // Inside `allOf`, so `Option<T>` wraps it as `anyOf [it, null]` (a schema without
        // `type` or a subschema would be left as it is, and `null` lost).
        schemars::json_schema!({ "allOf": [{ "x-hostler-type-param": "T" }] })
    }
}

/// A CLI `--json` answer (spec §6.3): the envelope, and its `data`'s sample.
pub struct EnvelopeSample {
    /// The fixture's name part: the data's Swift type, or `error`.
    pub name: &'static str,
    /// The type of `data`.
    pub data: Sample,
    pub envelope: Value,
}

fn ok_envelope<T: JsonSchema + Serialize + Clone>(name: &'static str, data: T) -> EnvelopeSample {
    EnvelopeSample {
        name,
        envelope: serde_json::to_value(Envelope {
            ok: true,
            data: Some(data.clone()),
            error: None,
        })
        .unwrap(),
        data: sample(data),
    }
}

/// The envelopes of the commands the app runs as subprocesses (`doctor`, the takeovers'
/// `hold`/`release`, `status`), and one refusal.
pub fn envelopes() -> Vec<EnvelopeSample> {
    vec![
        ok_envelope("DoctorReport", doctor_report()),
        ok_envelope("Claim", claims::fixtures::claim()),
        ok_envelope(
            "ReleaseOutcome",
            ReleaseOutcome {
                parent: None,
                seized_by: None,
            },
        ),
        ok_envelope("SeizureRecord", claims::fixtures::seizure()),
        ok_envelope(
            "Overview",
            Overview {
                service_version: "0.0.4".into(),
                heavy: queue_snapshot(),
                claims: claims::fixtures::snapshot(),
                secrets: secrets_status(),
            },
        ),
        EnvelopeSample {
            name: "error",
            envelope: serde_json::to_value(Envelope::<Claim> {
                ok: false,
                data: None,
                error: Some(error(ErrorCode::ForceRequired)),
            })
            .unwrap(),
            data: sample(claims::fixtures::claim()),
        },
    ]
}
