// The contract fixtures and the type each one decodes into.
//
// Generated from the Rust contract by crates/hostler/src/contract/swift (plan P5 Task 3).
// Never edit by hand: run scripts/macos.sh contract --write.

import Foundation
import HostlerKit

/// Every file in `Fixtures/contract`, as Rust wrote them.
let contractFixtureNames: [String] = [
    "answer.AccountAnswerOutcome.json",
    "answer.AccountFieldSet.json",
    "answer.AccountInfo.json",
    "answer.AccountInfoList.json",
    "answer.AccountRequestInfo.json",
    "answer.AccountRequestInfoList.json",
    "answer.AcquireOutcome.json",
    "answer.Claim.json",
    "answer.ClaimHistoryItemList.json",
    "answer.ClaimOutcome.json",
    "answer.ClaimsSnapshot.json",
    "answer.DeviceDetail.json",
    "answer.DeviceInfo.json",
    "answer.DeviceRemoved.json",
    "answer.DevicesSnapshot.json",
    "answer.DoctorReport.json",
    "answer.ForgetRecord.json",
    "answer.HeavyHistoryItemList.json",
    "answer.HelloReply.json",
    "answer.ImportOutcome.json",
    "answer.InstallEventList.json",
    "answer.MaterializedAccount.json",
    "answer.MaterializedSecret.json",
    "answer.MaterializedSecretList.json",
    "answer.Overview.json",
    "answer.ProjectInfo.json",
    "answer.ProjectInfoList.json",
    "answer.ProjectsSnapshot.json",
    "answer.ProofGrant.json",
    "answer.QueueSnapshot.json",
    "answer.QueueStopOutcome.json",
    "answer.ReleaseOutcome.json",
    "answer.Removed.json",
    "answer.RequestsSnapshot.json",
    "answer.SecretList.json",
    "answer.SecretRequestInfo.json",
    "answer.SecretRequestInfoList.json",
    "answer.SecretsStatus.json",
    "answer.SeizeOutcome.json",
    "answer.SeizePlan.json",
    "answer.SeizeProgressReply.json",
    "answer.SeizureRecord.json",
    "answer.ServiceStatus.json",
    "answer.SetOutcome.json",
    "answer.ShareOutcome.json",
    "answer.ShutdownAccepted.json",
    "answer.ToolStatus.json",
    "answer.UnlockOutcome.json",
    "answer.Unshared.json",
    "answer.WorkspaceInfo.json",
    "answer.WorkspaceInfoList.json",
    "envelope.Claim.json",
    "envelope.DoctorReport.json",
    "envelope.Overview.json",
    "envelope.ReleaseOutcome.json",
    "envelope.SeizureRecord.json",
    "envelope.error.json",
    "error.account_missing.json",
    "error.alias_taken.json",
    "error.bad_request.json",
    "error.boot_failed.json",
    "error.capacity_out_of_range.json",
    "error.claim_deadlock.json",
    "error.claim_not_held.json",
    "error.config_invalid.json",
    "error.contract_mismatch.json",
    "error.device_not_found.json",
    "error.device_not_ready.json",
    "error.device_not_registered.json",
    "error.force_required.json",
    "error.internal.json",
    "error.not_in_project.json",
    "error.not_interactive.json",
    "error.permission_denied.json",
    "error.project_not_approved.json",
    "error.proof_limited.json",
    "error.proof_required.json",
    "error.queue_busy.json",
    "error.queue_timeout.json",
    "error.request_timeout.json",
    "error.resource_in_use.json",
    "error.secret_exists.json",
    "error.secret_missing.json",
    "error.secret_not_in_project.json",
    "error.secret_too_short.json",
    "error.secrets_conflict.json",
    "error.secrets_locked.json",
    "error.seizure_failed.json",
    "error.service_busy.json",
    "error.service_unavailable.json",
    "error.stop_failed.json",
    "error.taken_over.json",
    "error.tool_missing.json",
    "error.unknown_entry.json",
    "error.unknown_resource.json",
    "error.unlock_failed.json",
    "error.withdrawn.json",
    "event.accounts.request_changed.json",
    "event.claims.changed.json",
    "event.claims.seized.json",
    "event.devices.changed.json",
    "event.devices.installed.json",
    "event.projects.changed.json",
    "event.queue.changed.json",
    "event.resync.json",
    "event.secrets.lock_changed.json",
    "event.secrets.request_changed.json",
    "frame.error.json",
    "frame.event.json",
    "frame.response.json",
    "request.account.answer.json",
    "request.account.check.json",
    "request.account.decline.json",
    "request.account.list.json",
    "request.account.materialize.json",
    "request.account.remove.json",
    "request.account.request.json",
    "request.account.request_withdraw.json",
    "request.account.requests.json",
    "request.account.reveal.json",
    "request.account.set.json",
    "request.claims.acquire.json",
    "request.claims.attach.json",
    "request.claims.force_release.json",
    "request.claims.history.json",
    "request.claims.release.json",
    "request.claims.renew.json",
    "request.claims.reserve.json",
    "request.claims.seize_abort.json",
    "request.claims.seize_begin.json",
    "request.claims.seize_commit.json",
    "request.claims.seize_progress.json",
    "request.claims.set_command.json",
    "request.claims.snapshot.json",
    "request.claims.started.json",
    "request.claims.unreserve.json",
    "request.claims.withdraw.json",
    "request.devices.add.json",
    "request.devices.boot.json",
    "request.devices.forget_sessions.json",
    "request.devices.installs.json",
    "request.devices.list.json",
    "request.devices.note.json",
    "request.devices.remove.json",
    "request.devices.rename.json",
    "request.devices.resolve.json",
    "request.devices.show.json",
    "request.devices.tools.json",
    "request.heavy.acquire.json",
    "request.heavy.attach.json",
    "request.heavy.force_release.json",
    "request.heavy.release.json",
    "request.heavy.started.json",
    "request.heavy.stop.json",
    "request.heavy.withdraw.json",
    "request.hello.json",
    "request.project.approve.json",
    "request.project.list.json",
    "request.project.resolve.json",
    "request.projects.snapshot.json",
    "request.queue.history.json",
    "request.queue.set_capacity.json",
    "request.queue.snapshot.json",
    "request.requests.snapshot.json",
    "request.secret.decline.json",
    "request.secret.import.json",
    "request.secret.list.json",
    "request.secret.materialize.json",
    "request.secret.remove.json",
    "request.secret.request.json",
    "request.secret.request_withdraw.json",
    "request.secret.requests.json",
    "request.secret.reveal.json",
    "request.secret.set.json",
    "request.secret.share.json",
    "request.secret.unshare.json",
    "request.secrets.lock.json",
    "request.secrets.prove.json",
    "request.secrets.remove_preserved.json",
    "request.secrets.status.json",
    "request.secrets.unlock.json",
    "request.service.shutdown.json",
    "request.service.status.json",
    "request.status.overview.json",
    "request.workspace.list.json",
    "request.workspace.resolve.json",
    "value.AcquireOutcome.1.json",
    "value.AcquireOutcome.2.json",
    "value.AcquireOutcome.3.json",
    "value.AcquireOutcome.4.json",
    "value.AdbState.1.json",
    "value.ClaimOutcome.1.json",
    "value.ClaimOutcome.2.json",
    "value.ClaimOutcome.3.json",
    "value.ClaimOutcome.4.json",
    "value.ClaimSelector.1.json",
    "value.HoldBinding.1.json",
    "value.InstallSource.1.json",
    "value.InstallSource.2.json",
    "value.Purpose.1.json",
    "value.ReadingUInt8.1.json",
    "value.ReadingUInt8.2.json",
    "value.StopEvidence.1.json",
    "value.StopEvidence.2.json",
]

/// Decodes a value fixture as its type and encodes it again; `nil` for a request, an event or
/// a frame, which the tests take apart themselves.
func reencodeContractFixture(_ name: String, _ data: Data) throws -> Data? {
    switch name {
    case "answer.AccountAnswerOutcome.json": try reencode(AccountAnswerOutcome.self, data)
    case "answer.AccountFieldSet.json": try reencode(AccountFieldSet.self, data)
    case "answer.AccountInfo.json": try reencode(AccountInfo.self, data)
    case "answer.AccountInfoList.json": try reencode([AccountInfo].self, data)
    case "answer.AccountRequestInfo.json": try reencode(AccountRequestInfo.self, data)
    case "answer.AccountRequestInfoList.json": try reencode([AccountRequestInfo].self, data)
    case "answer.AcquireOutcome.json": try reencode(AcquireOutcome.self, data)
    case "answer.Claim.json": try reencode(Claim.self, data)
    case "answer.ClaimHistoryItemList.json": try reencode([ClaimHistoryItem].self, data)
    case "answer.ClaimOutcome.json": try reencode(ClaimOutcome.self, data)
    case "answer.ClaimsSnapshot.json": try reencode(ClaimsSnapshot.self, data)
    case "answer.DeviceDetail.json": try reencode(DeviceDetail.self, data)
    case "answer.DeviceInfo.json": try reencode(DeviceInfo.self, data)
    case "answer.DeviceRemoved.json": try reencode(DeviceRemoved.self, data)
    case "answer.DevicesSnapshot.json": try reencode(DevicesSnapshot.self, data)
    case "answer.DoctorReport.json": try reencode(DoctorReport.self, data)
    case "answer.ForgetRecord.json": try reencode(ForgetRecord.self, data)
    case "answer.HeavyHistoryItemList.json": try reencode([HeavyHistoryItem].self, data)
    case "answer.HelloReply.json": try reencode(HelloReply.self, data)
    case "answer.ImportOutcome.json": try reencode(ImportOutcome.self, data)
    case "answer.InstallEventList.json": try reencode([InstallEvent].self, data)
    case "answer.MaterializedAccount.json": try reencode(MaterializedAccount.self, data)
    case "answer.MaterializedSecret.json": try reencode(MaterializedSecret.self, data)
    case "answer.MaterializedSecretList.json": try reencode([MaterializedSecret].self, data)
    case "answer.Overview.json": try reencode(Overview.self, data)
    case "answer.ProjectInfo.json": try reencode(ProjectInfo.self, data)
    case "answer.ProjectInfoList.json": try reencode([ProjectInfo].self, data)
    case "answer.ProjectsSnapshot.json": try reencode(ProjectsSnapshot.self, data)
    case "answer.ProofGrant.json": try reencode(ProofGrant.self, data)
    case "answer.QueueSnapshot.json": try reencode(QueueSnapshot.self, data)
    case "answer.QueueStopOutcome.json": try reencode(QueueStopOutcome.self, data)
    case "answer.ReleaseOutcome.json": try reencode(ReleaseOutcome.self, data)
    case "answer.Removed.json": try reencode(Removed.self, data)
    case "answer.RequestsSnapshot.json": try reencode(RequestsSnapshot.self, data)
    case "answer.SecretList.json": try reencode(SecretList.self, data)
    case "answer.SecretRequestInfo.json": try reencode(SecretRequestInfo.self, data)
    case "answer.SecretRequestInfoList.json": try reencode([SecretRequestInfo].self, data)
    case "answer.SecretsStatus.json": try reencode(SecretsStatus.self, data)
    case "answer.SeizeOutcome.json": try reencode(SeizeOutcome.self, data)
    case "answer.SeizePlan.json": try reencode(SeizePlan.self, data)
    case "answer.SeizeProgressReply.json": try reencode(SeizeProgressReply.self, data)
    case "answer.SeizureRecord.json": try reencode(SeizureRecord.self, data)
    case "answer.ServiceStatus.json": try reencode(ServiceStatus.self, data)
    case "answer.SetOutcome.json": try reencode(SetOutcome.self, data)
    case "answer.ShareOutcome.json": try reencode(ShareOutcome.self, data)
    case "answer.ShutdownAccepted.json": try reencode(ShutdownAccepted.self, data)
    case "answer.ToolStatus.json": try reencode(ToolStatus.self, data)
    case "answer.UnlockOutcome.json": try reencode(UnlockOutcome.self, data)
    case "answer.Unshared.json": try reencode(Unshared.self, data)
    case "answer.WorkspaceInfo.json": try reencode(WorkspaceInfo.self, data)
    case "answer.WorkspaceInfoList.json": try reencode([WorkspaceInfo].self, data)
    case "envelope.Claim.json": try reencode(Envelope<Claim>.self, data)
    case "envelope.DoctorReport.json": try reencode(Envelope<DoctorReport>.self, data)
    case "envelope.Overview.json": try reencode(Envelope<Overview>.self, data)
    case "envelope.ReleaseOutcome.json": try reencode(Envelope<ReleaseOutcome>.self, data)
    case "envelope.SeizureRecord.json": try reencode(Envelope<SeizureRecord>.self, data)
    case "envelope.error.json": try reencode(Envelope<Claim>.self, data)
    case "error.account_missing.json": try reencode(HostlerError.self, data)
    case "error.alias_taken.json": try reencode(HostlerError.self, data)
    case "error.bad_request.json": try reencode(HostlerError.self, data)
    case "error.boot_failed.json": try reencode(HostlerError.self, data)
    case "error.capacity_out_of_range.json": try reencode(HostlerError.self, data)
    case "error.claim_deadlock.json": try reencode(HostlerError.self, data)
    case "error.claim_not_held.json": try reencode(HostlerError.self, data)
    case "error.config_invalid.json": try reencode(HostlerError.self, data)
    case "error.contract_mismatch.json": try reencode(HostlerError.self, data)
    case "error.device_not_found.json": try reencode(HostlerError.self, data)
    case "error.device_not_ready.json": try reencode(HostlerError.self, data)
    case "error.device_not_registered.json": try reencode(HostlerError.self, data)
    case "error.force_required.json": try reencode(HostlerError.self, data)
    case "error.internal.json": try reencode(HostlerError.self, data)
    case "error.not_in_project.json": try reencode(HostlerError.self, data)
    case "error.not_interactive.json": try reencode(HostlerError.self, data)
    case "error.permission_denied.json": try reencode(HostlerError.self, data)
    case "error.project_not_approved.json": try reencode(HostlerError.self, data)
    case "error.proof_limited.json": try reencode(HostlerError.self, data)
    case "error.proof_required.json": try reencode(HostlerError.self, data)
    case "error.queue_busy.json": try reencode(HostlerError.self, data)
    case "error.queue_timeout.json": try reencode(HostlerError.self, data)
    case "error.request_timeout.json": try reencode(HostlerError.self, data)
    case "error.resource_in_use.json": try reencode(HostlerError.self, data)
    case "error.secret_exists.json": try reencode(HostlerError.self, data)
    case "error.secret_missing.json": try reencode(HostlerError.self, data)
    case "error.secret_not_in_project.json": try reencode(HostlerError.self, data)
    case "error.secret_too_short.json": try reencode(HostlerError.self, data)
    case "error.secrets_conflict.json": try reencode(HostlerError.self, data)
    case "error.secrets_locked.json": try reencode(HostlerError.self, data)
    case "error.seizure_failed.json": try reencode(HostlerError.self, data)
    case "error.service_busy.json": try reencode(HostlerError.self, data)
    case "error.service_unavailable.json": try reencode(HostlerError.self, data)
    case "error.stop_failed.json": try reencode(HostlerError.self, data)
    case "error.taken_over.json": try reencode(HostlerError.self, data)
    case "error.tool_missing.json": try reencode(HostlerError.self, data)
    case "error.unknown_entry.json": try reencode(HostlerError.self, data)
    case "error.unknown_resource.json": try reencode(HostlerError.self, data)
    case "error.unlock_failed.json": try reencode(HostlerError.self, data)
    case "error.withdrawn.json": try reencode(HostlerError.self, data)
    case "value.AcquireOutcome.1.json": try reencode(AcquireOutcome.self, data)
    case "value.AcquireOutcome.2.json": try reencode(AcquireOutcome.self, data)
    case "value.AcquireOutcome.3.json": try reencode(AcquireOutcome.self, data)
    case "value.AcquireOutcome.4.json": try reencode(AcquireOutcome.self, data)
    case "value.AdbState.1.json": try reencode(AdbState.self, data)
    case "value.ClaimOutcome.1.json": try reencode(ClaimOutcome.self, data)
    case "value.ClaimOutcome.2.json": try reencode(ClaimOutcome.self, data)
    case "value.ClaimOutcome.3.json": try reencode(ClaimOutcome.self, data)
    case "value.ClaimOutcome.4.json": try reencode(ClaimOutcome.self, data)
    case "value.ClaimSelector.1.json": try reencode(ClaimSelector.self, data)
    case "value.HoldBinding.1.json": try reencode(HoldBinding.self, data)
    case "value.InstallSource.1.json": try reencode(InstallSource.self, data)
    case "value.InstallSource.2.json": try reencode(InstallSource.self, data)
    case "value.Purpose.1.json": try reencode(Purpose.self, data)
    case "value.ReadingUInt8.1.json": try reencode(ReadingUInt8.self, data)
    case "value.ReadingUInt8.2.json": try reencode(ReadingUInt8.self, data)
    case "value.StopEvidence.1.json": try reencode(StopEvidence.self, data)
    case "value.StopEvidence.2.json": try reencode(StopEvidence.self, data)
    default: nil
    }
}

private func reencode<T: Codable>(_: T.Type, _ data: Data) throws -> Data {
    try JSONEncoder().encode(JSONDecoder().decode(T.self, from: data))
}
