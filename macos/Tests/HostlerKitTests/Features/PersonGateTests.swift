import Foundation
import Testing
@testable import HostlerKit

/// The person gate (spec §8.5, plan Task 9): a live grant runs the action, otherwise the sheet
/// asks for the master password, proves it, then runs the action; `proof_required` gives one
/// more sheet; `proof_limited` shows the wait and sends nothing before it.
@Suite(.timeLimit(.minutes(1))) @MainActor struct PersonGateTests {
    typealias Outcome = GateResult<AccountRequestInfo>

    static func decline(_ client: HostlerClient) async throws(HostlerCallError) -> AccountRequestInfo {
        try await client.accountDecline(id: "a-1")
    }

    @Test func aLiveGrantFromAnUnlockRunsTheActionWithNoProve() async throws {
        let f = GateFixture()
        // An unlock on this connection granted it 120 s (spec §8.5).
        _ = try await f.client.secretsUnlock(Unlock(password: SecretText("correct horse"),
                                                    forgetKeyFile: false, create: false))
        let outcome: Outcome = await f.gate.perform("Decline login", Self.decline)
        guard case .done(let info) = outcome else { Issue.record("\(outcome)"); return }
        #expect(info.state == .declined)
        #expect(await f.transport.methods == [.secretsUnlock, .accountDecline])
        #expect(f.gate.phase == .idle)
    }

    @Test func withoutAGrantItAsksProvesThenRuns() async throws {
        let f = GateFixture()
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        guard case .done = await outcome else { Issue.record("not done"); return }
        #expect(await f.transport.methods == [.secretsProve, .accountDecline])
        #expect(f.gate.password.isEmpty)
    }

    @Test func anExpiredGrantProvesFirst() async throws {
        let f = GateFixture()
        async let first: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        _ = await first
        // Within the grant (minus the 2 s margin) no second prove.
        f.clock.advance(by: 117)
        let second: Outcome = await f.gate.perform("Decline login", Self.decline)
        guard case .done = second else { Issue.record("\(second)"); return }
        #expect(await f.transport.count(.secretsProve) == 1)
        // Inside the margin: it proves again rather than racing the expiry.
        f.clock.advance(by: 1.5)
        async let third: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        guard case .done = await third else { Issue.record("not done"); return }
        #expect(await f.transport.count(.secretsProve) == 2)
    }

    @Test func proofRequiredGivesOneMoreSheetThenTheError() async throws {
        let f = GateFixture()
        // A grant the service no longer honours (a lock or a reconnect voided it).
        f.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 60_000)
        await f.transport.queue(.service(Errors.proofRequired), for: .accountDecline)
        await f.transport.queue(.service(Errors.proofRequired), for: .accountDecline)
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        let result = await outcome
        guard case .failed(let error) = result else { Issue.record("\(result)"); return }
        #expect(error == .service(message: Errors.proofRequired.message,
                                  recovery: Errors.proofRequired.recovery))
        #expect(await f.transport.methods == [.accountDecline, .secretsProve, .accountDecline])
        #expect(f.gate.phase == .idle)
    }

    @Test func proofRequiredOnceIsAnsweredByTheSecondProve() async throws {
        let f = GateFixture()
        await f.transport.queue(.service(Errors.proofRequired), for: .accountDecline)
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        try await f.answer("correct horse")
        guard case .done = await outcome else { Issue.record("not done"); return }
        #expect(await f.transport.methods
            == [.secretsProve, .accountDecline, .secretsProve, .accountDecline])
    }

    @Test func aWrongPasswordStaysOnTheSheetWithTheServicesMessageAndAClearedField() async throws {
        let f = GateFixture()
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("wrong")
        try await storeSoon("the failure is shown") { f.gate.message != nil && f.gate.phase == .asking }
        // R1-4: the gate's own text, with no CLI recovery line ("hostler unlock").
        #expect(f.gate.message == "Wrong password — try again.")
        #expect(f.gate.message == PersonGate.wrongPassword)
        #expect(f.gate.recovery == nil)
        #expect(f.gate.password.isEmpty)
        #expect(await f.transport.count(.accountDecline) == 0)
        f.gate.cancel()
        guard case .cancelled = await outcome else { Issue.record("not cancelled"); return }
        #expect(f.gate.password.isEmpty)
        #expect(f.gate.message == nil)
    }

    @Test func proofLimitedShowsTheCountdownAndSendsNothingBeforeIt() async throws {
        let f = GateFixture()
        await f.transport.set { $0.limitedMs = 4_000 }
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        try await storeSoon("the wait is shown") { f.limit.isWaiting && f.gate.phase == .asking }
        #expect(f.gate.password.isEmpty)
        #expect(f.limit.waitText == "Too many wrong passwords; next try in 4 s. Another process of "
            + "your user may be guessing.")
        f.clock.advance(by: 1.2)
        #expect(f.limit.waitText == "Too many wrong passwords; next try in 3 s. Another process of "
            + "your user may be guessing.")
        // A submit during the wait sends nothing and keeps nothing typed.
        await f.transport.set { $0.limitedMs = nil }
        f.gate.password.text = "correct horse"
        f.gate.submit()
        #expect(f.gate.password.isEmpty)
        #expect(f.gate.phase == .asking)
        #expect(await f.transport.count(.secretsProve) == 1)
        // After the wait, a submit proves.
        f.clock.advance(by: 3)
        #expect(!f.limit.isWaiting)
        #expect(f.limit.waitText == nil)
        f.gate.password.text = "correct horse"
        f.gate.submit()
        guard case .done = await outcome else { Issue.record("not done"); return }
        #expect(await f.transport.count(.secretsProve) == 2)
    }

    @Test func anotherProveFailureKeepsTheServicesMessageAndRecovery() async throws {
        let f = GateFixture()
        let missing = HostlerError(code: .secretsLocked, message: "no secrets database yet",
                                   details: .object([:]), recovery: "create one in the hostler app")
        await f.transport.queue(.service(missing), for: .secretsProve)
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("correct horse")
        try await storeSoon("the failure is shown") { f.gate.message != nil && f.gate.phase == .asking }
        // In the app's words (Task 13 F-5): the `secrets_locked` here has no `state`, so it
        // reads as locked.
        #expect(f.gate.message == "The secrets database is locked. Unlock it in hostler's menu bar panel.")
        #expect(f.gate.recovery == nil)
        f.gate.cancel()
        _ = await outcome
    }

    @Test func cancellingClearsTheFieldAndRunsNothing() async throws {
        let f = GateFixture()
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await storeSoon("the gate asks") { f.gate.phase == .asking }
        #expect(f.gate.purpose == "Decline login")
        f.gate.password.text = "half typed"
        f.gate.cancel()
        guard case .cancelled = await outcome else { Issue.record("not cancelled"); return }
        #expect(f.gate.password.isEmpty)
        #expect(f.gate.purpose == nil)
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func cancelClearsTheFieldEvenWithNothingWaiting() {
        let f = GateFixture()
        f.gate.password.text = "stray"
        f.gate.cancel()
        #expect(f.gate.password.isEmpty)
        f.gate.password.text = "stray"
        f.gate.dismissed(.popover)
        #expect(f.gate.password.text == "stray")
        f.gate.submit()
        #expect(f.gate.password.isEmpty)
    }

    @Test func aSecondActionWhileOneAsksIsRefused() async throws {
        let f = GateFixture()
        async let first: Outcome = f.gate.perform("Decline login", Self.decline)
        try await storeSoon("the gate asks") { f.gate.phase == .asking }
        let second: Outcome = await f.gate.perform("Decline other", Self.decline)
        guard case .failed(.gateBusy) = second else { Issue.record("\(second)"); return }
        f.gate.cancel()
        _ = await first
    }

    @Test func onlyThePresenterThatAskedCancelsOnClose() async throws {
        let f = GateFixture()
        async let outcome: Outcome = f.gate.perform("Decline login", from: .window, Self.decline)
        try await storeSoon("the gate asks") { f.gate.phase == .asking }
        f.gate.dismissed(.popover)
        #expect(f.gate.phase == .asking)
        f.gate.dismissed(.window)
        guard case .cancelled = await outcome else { Issue.record("not cancelled"); return }
    }

    @Test func otherActionErrorsAreShownAsTheServiceSaysThem() async throws {
        let f = GateFixture()
        f.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 60_000)
        let missing = HostlerError(code: .unknownEntry, message: "no such request",
                                   details: .object([:]), recovery: "hostler account requests")
        await f.transport.queue(.service(missing), for: .accountDecline)
        let outcome: Outcome = await f.gate.perform("Decline login", Self.decline)
        guard case .failed(let error) = outcome else { Issue.record("\(outcome)"); return }
        #expect(error == .service(message: "no such request", recovery: "hostler account requests"))
        #expect(await f.transport.methods == [.accountDecline])
    }

    @Test func theTextsClaimNoMoreThanTheSpecAllows() {
        #expect(PersonGate.explanation
            == "This confirms it is you. It does not stop other programs of your user from using "
            + "hostler's socket.")
    }

    @Test func nothingTypedReachesTheLog() async throws {
        let f = GateFixture()
        async let outcome: Outcome = f.gate.perform("Decline login", Self.decline)
        try await f.answer("wrong-marker-7731")
        try await storeSoon("the failure is shown") { f.gate.message != nil }
        try await f.answer("correct horse")
        _ = await outcome
        #expect(!f.log.lines.isEmpty)
        for line in f.log.lines {
            #expect(!line.contains("marker-7731"))
            #expect(!line.contains("correct horse"))
        }
    }
}
