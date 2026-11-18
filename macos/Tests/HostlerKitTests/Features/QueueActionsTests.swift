import Foundation
import Testing
@testable import HostlerKit

/// The queue's actions (spec §9.3, §11.1; plan Task 11): withdraw, stop behind the person gate,
/// and the capacity's bounds. A wrong typed id sends nothing.
@Suite(.timeLimit(.minutes(1))) @MainActor struct QueueActionsTests {
    private func row(_ id: String = "h-1", phase: QueueRow.Phase = .running,
                     source: EntrySource = .hostler, foreign: Bool = false) -> QueueRow {
        var row = queueRows(queue: QueueSnapshot(
            capacity: 2,
            running: [Mk.queueEntry(id, ticket: 1, state: .running, grantedAt: Mk.now)],
            waiting: [], foreignRunning: [], asOfSeq: 1),
            history: [], nowMs: Mk.now)[0]
        row.phase = phase
        row.source = source
        row.isForeign = foreign
        return row
    }

    @Test func withdrawSendsHeavyWithdrawForTheTask() async throws {
        let f = WindowFixture()
        #expect(await f.queue.withdraw(row()) == nil)
        #expect(await f.transport.methods == [.heavyWithdraw])
        guard case .heavyWithdraw(let id, let timedOut)? = await f.transport.request(.heavyWithdraw)
        else { Issue.record("no heavy.withdraw"); return }
        #expect(id == "h-1")
        #expect(!timedOut)
    }

    @Test func aServiceFailureIsTheAppsError() async {
        let f = WindowFixture()
        await f.transport.fail(.heavyWithdraw,
                               with: .service(Errors.badRequest))
        let failure = await f.queue.withdraw(row())
        #expect(failure == .app(.service(message: Errors.badRequest.message,
                                         recovery: Errors.badRequest.recovery)))
    }

    @Test func stopRefusesATypedIdThatIsNotTheTasksAndSendsNothing() async {
        let f = WindowFixture()
        f.grantLive()
        let outcome = await f.queue.stop(row("h-7"), typed: "h-8")
        guard case .failed(let failure) = outcome else { Issue.record("\(outcome)"); return }
        #expect(failure == .confirmationMismatch(expected: "h-7"))
        #expect(await f.transport.calls.isEmpty)
        #expect(f.gate.purpose == nil)
    }

    @Test func stopTrimsTheTypedIdLikeTheSheetDoes() async {
        let f = WindowFixture()
        f.grantLive()
        let outcome = await f.queue.stop(row("h-7"), typed: " h-7\n")
        guard case .done(let answer) = outcome else { Issue.record("\(outcome)"); return }
        #expect(answer.stopped)
        guard case .heavyStop(let id, let confirmation)? = await f.transport.request(.heavyStop)
        else { Issue.record("no heavy.stop"); return }
        #expect(id == "h-7")
        #expect(confirmation == "h-7")
    }

    @Test func stopWithALiveGrantRunsWithoutAsking() async throws {
        let f = WindowFixture()
        f.grantLive()
        let outcome = await f.queue.stop(row(), typed: "h-1")
        guard case .done = outcome else { Issue.record("\(outcome)"); return }
        #expect(await f.transport.methods == [.heavyStop])
        #expect(f.gate.phase == .idle)
    }

    @Test func stopWithoutAGrantAsksForThePasswordThenStops() async throws {
        let f = WindowFixture()
        async let stopping = f.queue.stop(row(), typed: "h-1")
        try await f.answer("correct horse")
        guard case .done = await stopping else { Issue.record("not stopped"); return }
        #expect(await f.transport.methods == [.secretsProve, .heavyStop])
        #expect(f.gate.password.isEmpty)
    }

    @Test func cancellingTheGateStopsNothing() async throws {
        let f = WindowFixture()
        async let stopping = f.queue.stop(row(), typed: "h-1")
        try await storeSoon("the gate asks") { f.gate.phase == .asking }
        f.gate.cancel()
        guard case .cancelled = await stopping else { Issue.record("not cancelled"); return }
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func aTaskThatHadEndedAnswersStoppedFalse() async throws {
        let f = WindowFixture()
        f.grantLive()
        await f.transport.set { $0.stopOutcome = QueueStopOutcome(id: "h-1", stopped: false,
                                                                  pids: [1]) }
        let outcome = await f.queue.stop(row(), typed: "h-1")
        guard case .done(let answer) = outcome else { Issue.record("\(outcome)"); return }
        #expect(!answer.stopped)
    }

    @Test func capacityIsOneToSixteen() async {
        let f = WindowFixture()
        #expect(QueueActions.capacityRange == 1...16)
        for capacity in [1, 8, 16] {
            #expect(await f.queue.setCapacity(capacity) == nil)
        }
        #expect(await f.transport.count(.queueSetCapacity) == 3)
        guard case .queueSetCapacity(let value)? = await f.transport.request(.queueSetCapacity)
        else { Issue.record("no queue.set_capacity"); return }
        #expect(value == 1)
    }

    @Test func aCapacityOutsideTheRangeSendsNothing() async {
        let f = WindowFixture()
        for capacity in [0, 17, -3] {
            let failure = await f.queue.setCapacity(capacity)
            #expect(failure == .app(.capacityOutOfRange(capacity)))
        }
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func aCapacityFailureIsTheAppsError() async {
        let f = WindowFixture()
        await f.transport.fail(.queueSetCapacity,
                               with: .service(HostlerError(code: .capacityOutOfRange,
                                                           message: "capacity must be 1 to 16",
                                                           details: .object([:]), recovery: nil)))
        let failure = await f.queue.setCapacity(4)
        #expect(failure == .app(.service(message: "capacity must be 1 to 16", recovery: nil)))
    }
}

/// The screen's two CLI actions (spec §9.3, §11.2; plan Task 11): a release and a takeover,
/// both with the `screen` token, run through the service's own binary.
@Suite(.timeLimit(.minutes(1))) struct ScreenActionsTests {
    private func claim() -> Claim {
        Claim(id: "c-screen", resource: ResourceInfo(
            id: ResourceId("screen"), kind: .screen, display: "screen", attributes: [:],
            available: true), kind: .hold,
            workspace: WorkspaceRef(id: "w-1", path: "/work/app"), grantedAtMs: Mk.now)
    }

    private func workspace() -> WorkspaceInfo {
        Mk.workspace("w-1", project: "p-1", path: "/work/app")
    }

    private func actions(_ runner: FakeCommandRunner) -> ScreenActions {
        ScreenActions(runner: runner, cliExecutable: "/bin/hostler")
    }

    @Test func releaseRunsTheCLIInTheClaimsWorkspace() async throws {
        let runner = FakeCommandRunner()
        #expect(await actions(runner).release(claim(), typed: "screen") == nil)
        let invocation = try #require(runner.invocations.first)
        #expect(invocation.arguments == ["release", "c-screen", "--force", "--confirmation",
                                         "screen"])
        #expect(invocation.cwd == "/work/app")
    }

    @Test func takeOverHoldsTheScreenInTheChosenWorkspace() async throws {
        let runner = FakeCommandRunner()
        #expect(await actions(runner).takeOver(in: workspace(), typed: "screen") == nil)
        let invocation = try #require(runner.invocations.first)
        #expect(invocation.arguments == ["hold", "screen", "--force", "--confirmation", "screen",
                                         "--ttl", "30"])
        #expect(invocation.cwd == "/work/app")
    }

    @Test func aWrongTokenSendsNothing() async {
        let runner = FakeCommandRunner()
        #expect(await actions(runner).release(claim(), typed: "Screen")
            == .confirmationMismatch(expected: "screen"))
        #expect(await actions(runner).takeOver(in: workspace(), typed: "")
            == .confirmationMismatch(expected: "screen"))
        #expect(runner.invocations.isEmpty)
    }

    @Test func withoutABinaryNothingRuns() async {
        let runner = FakeCommandRunner()
        let actions = ScreenActions(runner: runner, cliExecutable: nil)
        #expect(await actions.release(claim(), typed: "screen") == .noCommandLine)
        #expect(runner.invocations.isEmpty)
    }
}
