import AppKit
import Foundation
import Testing
@testable import HostlerKit

/// Reveal (spec §8.5, §9.6; plan Task 9): behind the person gate; shown for at most 30 s;
/// hidden at once on a lock; Copy goes through the concealed clipboard.
@Suite(.timeLimit(.minutes(1))) @MainActor struct RevealControllerTests {
    @MainActor struct Fixture {
        let gate = GateFixture()
        let sleeper = FakeSleeper()
        let board = RecordingPasteboard()
        let reveal: RevealController

        init() {
            let clipboard = ConcealedClipboard(pasteboard: board, sleeper: FakeSleeper())
            reveal = RevealController(gate: gate.gate, clipboard: clipboard, sleeper: sleeper,
                                      log: gate.log)
        }

        var transport: FakeValuesTransport { gate.transport }
    }

    /// A pasteboard that records what was written (the real one is covered by PasteboardTests).
    @MainActor final class RecordingPasteboard: PasteboardWriting {
        private(set) var written: [String] = []
        var changeCount = 0
        func writeConcealed(_ value: SecretText) -> Int {
            written.append(value.reveal())
            changeCount += 1
            return changeCount
        }
        func clear() { changeCount += 1 }
    }

    static let project = ProjectRef(commonDir: "/work/app/.git", name: "app")
    static let apiKey = RevealSubject.secret(project: project, name: "API_KEY")

    @Test func theFirstRevealProvesThenShowsTheValue() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "sk-live-123"] }
        async let done: Void = f.reveal.reveal(Self.apiKey)
        try await storeSoon("revealing") { f.reveal.revealing == Self.apiKey }
        try await f.gate.answer("correct horse")
        await done
        let shown = try #require(f.reveal.shown)
        #expect(shown.subject == Self.apiKey)
        #expect(shown.fields.map(\.name) == ["API_KEY"])
        #expect(shown.fields.first?.value == SecretText("sk-live-123"))
        #expect(f.reveal.revealing == nil)
        #expect(await f.transport.methods == [.secretsProve, .secretReveal])
        guard case .secretReveal(let project, let name) = await f.transport.calls.last else {
            Issue.record("no reveal"); return
        }
        #expect(project == Self.project)
        #expect(name == "API_KEY")
    }

    @Test func noSecondProveWithinTheGrantAndAnotherAfterIt() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1", "OTHER": "v2"] }
        async let first: Void = f.reveal.reveal(Self.apiKey)
        try await f.gate.answer("correct horse")
        await first
        f.gate.clock.advance(by: 100)
        await f.reveal.reveal(.secret(project: Self.project, name: "OTHER"))
        #expect(f.reveal.shown?.fields.first?.value == SecretText("v2"))
        #expect(await f.transport.count(.secretsProve) == 1)
        f.gate.clock.advance(by: 30)
        async let third: Void = f.reveal.reveal(Self.apiKey)
        try await f.gate.answer("correct horse")
        await third
        #expect(await f.transport.count(.secretsProve) == 2)
        #expect(await f.transport.count(.secretReveal) == 3)
    }

    @Test func proofRequiredGivesOneReprove() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1"] }
        await f.transport.queue(.service(Errors.proofRequired), for: .secretReveal)
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        async let done: Void = f.reveal.reveal(Self.apiKey)
        try await f.gate.answer("correct horse")
        await done
        #expect(f.reveal.shown?.fields.first?.value == SecretText("v1"))
        #expect(await f.transport.methods == [.secretReveal, .secretsProve, .secretReveal])
    }

    @Test func proofLimitedIsShownWithItsWaitAndACancelRevealsNothing() async throws {
        let f = Fixture()
        await f.transport.set {
            $0.secretValues = ["API_KEY": "v1"]
            $0.limitedMs = 30_000
        }
        async let done: Void = f.reveal.reveal(Self.apiKey)
        try await f.gate.answer("correct horse")
        try await storeSoon("the wait is shown") { f.gate.limit.isWaiting }
        #expect(f.gate.limit.waitText == ProofLimit.waitText(seconds: 30))
        f.gate.gate.cancel()
        await done
        #expect(f.reveal.shown == nil)
        #expect(f.reveal.error == nil)
        #expect(await f.transport.count(.secretReveal) == 0)
    }

    @Test func aFailedRevealShowsTheError() async throws {
        let f = Fixture()
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        #expect(f.reveal.shown == nil)
        #expect(f.reveal.error == RevealController.Failure(
            subject: Self.apiKey, error: .service(message: "no such secret", recovery: nil)))
    }

    @Test func theValueHidesAfterThirtySeconds() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        #expect(f.reveal.shown != nil)
        try await storeSoon("the hide is scheduled") { f.sleeper.waitingCount == 1 }
        #expect(f.sleeper.durations == [.seconds(30)])
        f.sleeper.releaseWaiting()
        try await storeSoon("hidden") { f.reveal.shown == nil }
    }

    @Test func anOlderTimerNeverHidesANewerReveal() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1", "OTHER": "v2"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        try await storeSoon("the first hide is scheduled") { f.sleeper.waitingCount == 1 }
        await f.reveal.reveal(.secret(project: Self.project, name: "OTHER"))
        try await storeSoon("the second hide is scheduled") { f.sleeper.durations.count == 2 }
        #expect(f.reveal.shown?.subject == .secret(project: Self.project, name: "OTHER"))
        // The first timer was cancelled with its value; only the second one remains.
        #expect(f.sleeper.waitingCount == 1)
    }

    @Test func aLockHidesAtOnce() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        f.reveal.lockStateChanged(.unlocked)
        #expect(f.reveal.shown != nil)
        f.reveal.lockStateChanged(.locked)
        #expect(f.reveal.shown == nil)
    }

    @Test func anAccountRevealShowsEveryField() async throws {
        let f = Fixture()
        await f.transport.set { $0.accountFields = ["login": [("user", "qa"), ("password", "pa55")]] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(.account(project: Self.project, name: "login"))
        let shown = try #require(f.reveal.shown)
        #expect(shown.fields.map(\.name) == ["user", "password"])
        #expect(shown.fields.map(\.value) == [SecretText("qa"), SecretText("pa55")])
        #expect(await f.transport.methods == [.accountReveal])
    }

    @Test func copyWritesTheShownValueToTheClipboard() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        let field = try #require(f.reveal.shown?.fields.first)
        f.reveal.copy(field)
        #expect(f.board.written == ["v1"])
        #expect(f.reveal.copied == "API_KEY")
    }

    @Test func theShownValueNeverPrints() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "value-marker-3141"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        let shown = try #require(f.reveal.shown)
        for text in [String(describing: shown), String(reflecting: shown),
                     String(describing: shown.fields), "\(shown.fields[0])"] {
            #expect(!text.contains("marker-3141"))
        }
        #expect(f.gate.log.lines.allSatisfy { !$0.contains("marker-3141") })
    }

    /// R1-2: a field kept from an earlier reveal (another subject or an older generation with
    /// the same field name) copies nothing; a current field copies the value on screen, never
    /// the value the caller's field carries.
    @Test func copyTakesTheCurrentRevealsValueByIdentityNeverTheCallersValue() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "first"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        let other = RevealSubject.secret(project: ProjectRef(commonDir: "/work/other/.git", name: "other"),
                                         name: "API_KEY")
        await f.reveal.reveal(Self.apiKey)
        let stale = try #require(f.reveal.shown?.fields.first)

        // Another subject with the same field name: the stale field copies nothing.
        await f.transport.set { $0.secretValues = ["API_KEY": "second"] }
        await f.reveal.reveal(other)
        #expect(f.reveal.shown?.subject == other)
        f.reveal.copy(stale)
        #expect(f.board.written.isEmpty)
        #expect(f.reveal.copied == nil)

        // The same subject revealed again (a new generation): the old field still copies nothing.
        await f.transport.set { $0.secretValues = ["API_KEY": "third"] }
        await f.reveal.reveal(Self.apiKey)
        let current = try #require(f.reveal.shown?.fields.first)
        #expect(current.generation != stale.generation)
        f.reveal.copy(stale)
        #expect(f.board.written.isEmpty)

        // A current identity with a forged value copies the value on screen.
        var forged = current
        forged.value = SecretText("forged")
        f.reveal.copy(forged)
        #expect(f.board.written == ["third"])

        // After a hide nothing is copied.
        f.reveal.hide()
        f.reveal.copy(current)
        #expect(f.board.written == ["third"])
    }

    // MARK: Clean read C-B2: an in-flight reveal never installs values after its surface went

    /// Starts a reveal of `subject` whose answer the fake holds; returns once it waits.
    private func heldReveal(_ f: Fixture, _ subject: RevealSubject = Self.apiKey) async throws -> Task<Void, Never> {
        await f.transport.set {
            $0.secretValues = ["API_KEY": "late-value"]
            $0.holdReveal = true
        }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        let reveal = f.reveal
        let task = Task { await reveal.reveal(subject) }
        try await storeSoon("the reveal waits for its answer") { await f.transport.revealWaiting == 1 }
        #expect(f.reveal.revealing == subject)
        return task
    }

    @Test func aRevealAnsweredAfterItsSurfaceDisappearedShowsNothing() async throws {
        let f = Fixture()
        let task = try await heldReveal(f)
        // The row or window goes away while the answer is on its way (nothing shown yet).
        f.reveal.dismissed(Self.apiKey)
        await f.transport.releaseReveal()
        await task.value
        #expect(f.reveal.shown == nil)
        #expect(f.reveal.revealing == nil)
        #expect(f.reveal.error == nil)
        #expect(f.reveal.lateAnswersDropped == 1)
        #expect(f.sleeper.waitingCount == 0)
        // Nothing was kept that a copy could reach.
        f.reveal.copy(RevealedField(name: "API_KEY", value: SecretText("x"), generation: 1, index: 0))
        #expect(f.board.written.isEmpty)
    }

    @Test func hideOrALockDuringARevealDropsItsAnswer() async throws {
        let f = Fixture()
        var task = try await heldReveal(f)
        f.reveal.hide()
        await f.transport.releaseReveal()
        await task.value
        #expect(f.reveal.shown == nil)

        task = try await heldReveal(f)
        f.reveal.lockStateChanged(.locked)
        await f.transport.releaseReveal()
        await task.value
        #expect(f.reveal.shown == nil)
        #expect(f.reveal.lateAnswersDropped == 2)
    }

    @Test func anotherSubjectsDismissalLeavesTheRevealAlone() async throws {
        let f = Fixture()
        let task = try await heldReveal(f)
        f.reveal.dismissed(.secret(project: Self.project, name: "OTHER"))
        await f.transport.releaseReveal()
        await task.value
        #expect(f.reveal.shown?.subject == Self.apiKey)
        #expect(f.reveal.lateAnswersDropped == 0)
    }

    @Test func dismissingTheShownSubjectHidesIt() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey)
        #expect(f.reveal.shown != nil)
        f.reveal.dismissed(Self.apiKey)
        #expect(f.reveal.shown == nil)
    }

    /// Task 13 F-1: closing a surface hides what that surface revealed, and only that.
    @Test func aPresentersCloseHidesOnlyItsOwnReveal() async throws {
        let f = Fixture()
        await f.transport.set { $0.secretValues = ["API_KEY": "v1"] }
        f.gate.grants.grant = ProofGrant(expiresAtMs: 1_800_000_000_000 + 100_000)
        await f.reveal.reveal(Self.apiKey, from: .window)
        f.reveal.dismissed(presenter: .popover)
        #expect(f.reveal.shown != nil)
        f.reveal.dismissed(presenter: .window)
        #expect(f.reveal.shown == nil)

        let task = try await heldReveal(f)  // from the window by default
        await f.reveal.reveal(Self.apiKey, from: .popover)  // refused: one at a time
        f.reveal.dismissed(presenter: .window)
        await f.transport.releaseReveal()
        await task.value
        #expect(f.reveal.shown == nil)
    }
}
