// Every generated type is Sendable (R2-8): this file does not compile otherwise.
//
// Generated from the Rust contract by crates/hostler/src/contract/swift (plan P5 Task 3).
// Never edit by hand: run scripts/macos.sh contract --write.

import AppKit
import Foundation
import HostlerKit
import OSLog
import SwiftUI
import Testing

private func assertSendable<T: Sendable>(_: T.Type) {}

@Test func everyGeneratedTypeIsSendable() {
    assertSendable(AccountAnswer.self)
    assertSendable(AccountAnswerOutcome.self)
    assertSendable(AccountFieldSet.self)
    assertSendable(AccountInfo.self)
    assertSendable(AccountMaterialize.self)
    assertSendable(AccountRemove.self)
    assertSendable(AccountRequestInfo.self)
    assertSendable(AccountRequestNew.self)
    assertSendable(AccountSet.self)
    assertSendable(AcquireOutcome.self)
    assertSendable(AdbState.self)
    assertSendable(AgentDeviceSession.self)
    assertSendable(AppState.self)
    assertSendable(Blocker.self)
    assertSendable(Check.self)
    assertSendable(CheckStatus.self)
    assertSendable(Claim.self)
    assertSendable(ClaimHistoryItem.self)
    assertSendable(ClaimKind.self)
    assertSendable(ClaimOutcome.self)
    assertSendable(ClaimRelease.self)
    assertSendable(ClaimRequest.self)
    assertSendable(ClaimSelector.self)
    assertSendable(ClaimStarted.self)
    assertSendable(ClaimSummary.self)
    assertSendable(ClaimsSnapshot.self)
    assertSendable(DeclaredAccount.self)
    assertSendable(DeclaredApps.self)
    assertSendable(DeclaredSecret.self)
    assertSendable(Decline.self)
    assertSendable(DeviceDetail.self)
    assertSendable(DeviceInfo.self)
    assertSendable(DeviceKind.self)
    assertSendable(DeviceRemoved.self)
    assertSendable(DevicesSnapshot.self)
    assertSendable(DoctorReport.self)
    assertSendable(EndReason.self)
    assertSendable(EntrySource.self)
    assertSendable(Envelope<JSONValue>.self)
    assertSendable(ErrorCode.self)
    assertSendable(Event.self)
    assertSendable(Fix.self)
    assertSendable(FixOutcome.self)
    assertSendable(FixResult.self)
    assertSendable(ForeignClaim.self)
    assertSendable(ForgetRecord.self)
    assertSendable(HeavyAcquire.self)
    assertSendable(HeavyEntry.self)
    assertSendable(HeavyHistoryItem.self)
    assertSendable(HeavyOutcome.self)
    assertSendable(HeavyRelease.self)
    assertSendable(HeavyStarted.self)
    assertSendable(HeavyState.self)
    assertSendable(HelloReply.self)
    assertSendable(HistoryQuery.self)
    assertSendable(HoldBinding.self)
    assertSendable(HostlerCallError.self)
    assertSendable(HostlerClient.self)
    assertSendable(HostlerError.self)
    assertSendable(HostlerMethod.self)
    assertSendable(ImportEntry.self)
    assertSendable(ImportOutcome.self)
    assertSendable(InstallChange.self)
    assertSendable(InstallEvent.self)
    assertSendable(InstallSource.self)
    assertSendable(InstallTime.self)
    assertSendable(JSONValue.self)
    assertSendable(LockState.self)
    assertSendable(Materialize.self)
    assertSendable(MaterializedAccount.self)
    assertSendable(MaterializedSecret.self)
    assertSendable(MethodSpec.self)
    assertSendable(Overview.self)
    assertSendable(Platform.self)
    assertSendable(ProcessIdentity.self)
    assertSendable(ProjectDecl.self)
    assertSendable(ProjectInfo.self)
    assertSendable(ProjectRef.self)
    assertSendable(ProjectView.self)
    assertSendable(ProjectsSnapshot.self)
    assertSendable(ProofGrant.self)
    assertSendable(Prove.self)
    assertSendable(Purpose.self)
    assertSendable(QueueSnapshot.self)
    assertSendable(QueueStopOutcome.self)
    assertSendable(ReadingBool.self)
    assertSendable(ReadingUInt8.self)
    assertSendable(ReleaseOutcome.self)
    assertSendable(Removed.self)
    assertSendable(Request.self)
    assertSendable(RequestState.self)
    assertSendable(RequestsSnapshot.self)
    assertSendable(ResourceId.self)
    assertSendable(ResourceInfo.self)
    assertSendable(ResourceKind.self)
    assertSendable(ResourceState.self)
    assertSendable(ResyncReason.self)
    assertSendable(RunGroup.self)
    assertSendable(SecretImport.self)
    assertSendable(SecretInfo.self)
    assertSendable(SecretList.self)
    assertSendable(SecretRequest.self)
    assertSendable(SecretRequestInfo.self)
    assertSendable(SecretSet.self)
    assertSendable(SecretSource.self)
    assertSendable(SecretState.self)
    assertSendable(SecretText.self)
    assertSendable(SecretsStatus.self)
    assertSendable(SeizeBegin.self)
    assertSendable(SeizeCommit.self)
    assertSendable(SeizeOutcome.self)
    assertSendable(SeizePlan.self)
    assertSendable(SeizeProgress.self)
    assertSendable(SeizeProgressReply.self)
    assertSendable(SeizedFrom.self)
    assertSendable(SeizureRecord.self)
    assertSendable(SensitiveData.self)
    assertSendable(ServiceStatus.self)
    assertSendable(SetOutcome.self)
    assertSendable(Share.self)
    assertSendable(ShareOutcome.self)
    assertSendable(ShutdownAccepted.self)
    assertSendable(StopEvidence.self)
    assertSendable(StopTarget.self)
    assertSendable(Telemetry.self)
    assertSendable(ToolPathsSet.self)
    assertSendable(ToolStatus.self)
    assertSendable(Transport.self)
    assertSendable(TransportKind.self)
    assertSendable(UnidentifiedTransport.self)
    assertSendable(Unlock.self)
    assertSendable(UnlockOutcome.self)
    assertSendable(Unshared.self)
    assertSendable(WaitingRequest.self)
    assertSendable(Who.self)
    assertSendable(WorkspaceInfo.self)
    assertSendable(WorkspaceOutcome.self)
    assertSendable(WorkspaceRef.self)
    assertSendable(WorkspaceResolve.self)
}

// Each type that can hold a SecretText crosses into a detached task.

private func cross(_ value: AccountAnswer) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: AccountMaterialize) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: AccountSet) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: ImportEntry) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: Materialize) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: MaterializedAccount) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: MaterializedSecret) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: Prove) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: Request) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: SecretImport) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: SecretSet) async {
    await Task.detached { _ = value }.value
}

private func cross(_ value: Unlock) async {
    await Task.detached { _ = value }.value
}
