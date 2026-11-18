import Foundation
import Testing
@testable import HostlerKit

/// The Queue section's model (spec §9.3, §11.1; plan Task 11): the All / Running / Waiting /
/// Finished filter and its counts, the rows' positions, durations and outcome badges, and what
/// the section may do with each row.
@Suite struct QueuePresentationTests {
    private let now: Int64 = 1_800_000_000_000

    private func queue() -> QueueSnapshot {
        QueueSnapshot(
            capacity: 2,
            running: [Mk.queueEntry("h-run", ticket: 1, state: .running,
                                    command: "make test", grantedAt: now - 240_000),
                      Mk.queueEntry("h-stop", ticket: 2, state: .stopping, command: "pod install",
                                    grantedAt: now - 60_000)],
            waiting: [Mk.queueEntry("h-w1", ticket: 3, state: .waiting, command: "gradle build",
                                    enqueuedAt: now - 120_000),
                      Mk.queueEntry("h-w2", ticket: 4, state: .waiting, command: "xcodebuild",
                                    enqueuedAt: now - 30_000)],
            foreignRunning: [Mk.queueEntry("ledger-1", ticket: 5, state: .running,
                                           command: "rn-looky deploy", kind: "deploy",
                                           grantedAt: now - 30_000, source: .rnLookyLedger)],
            asOfSeq: 9)
    }

    private func history() -> [HeavyHistoryItem] {
        [Mk.historyItem("h-ok", finishedAt: now - 120_000, outcome: .ok),
         Mk.historyItem("h-bad", ticket: 2, ranMs: 65_000, finishedAt: now - 600_000,
                        outcome: .failed, exitCode: 2),
         Mk.historyItem("h-stopped", ticket: 3, finishedAt: now - 1_800_000,
                        outcome: .stopped, exitCode: 143)]
    }

    @Test func theCountsCoverEveryGroup() {
        let counts = queueCounts(queue: queue(), history: history())
        #expect(counts.running == 3)   // two hostler tasks and rn-looky's
        #expect(counts.waiting == 2)
        #expect(counts.finished == 3)
        #expect(counts.all == 8)
        #expect(counts.count(.running) == 3)
        #expect(counts.segmentTitle(.all) == "All 8")
        #expect(counts.segmentTitle(.running) == "Running 3")
        #expect(counts.segmentTitle(.waiting) == "Waiting 2")
        #expect(counts.segmentTitle(.finished) == "Finished 3")
    }

    @Test func anEmptyQueueCountsZeroEverywhere() {
        let counts = queueCounts(queue: nil, history: [])
        #expect(counts == QueueCounts())
        #expect(QueueFilter.allCases.map(counts.segmentTitle)
            == ["All 0", "Running 0", "Waiting 0", "Finished 0"])
    }

    @Test func theFilterKeepsItsOwnRows() {
        let rows = { (filter: QueueFilter) in
            queueRows(queue: self.queue(), history: self.history(), filter: filter,
                      nowMs: self.now)
        }
        #expect(rows(.all).count == 8)
        #expect(rows(.running).map(\.taskId) == ["h-run", "h-stop", "ledger-1"])
        #expect(rows(.waiting).map(\.taskId) == ["h-w1", "h-w2"])
        #expect(rows(.finished).map(\.taskId) == ["h-ok", "h-bad", "h-stopped"])
        // All shows running (the service's order), then waiting by ticket, then the newest end.
        #expect(rows(.all).map(\.taskId)
            == ["h-run", "h-stop", "ledger-1", "h-w1", "h-w2", "h-ok", "h-bad", "h-stopped"])
        #expect(rows(.running).allSatisfy { $0.phase == .running || $0.phase == .stopping })
        #expect(rows(.waiting).allSatisfy { $0.phase == .waiting })
        #expect(rows(.finished).allSatisfy { $0.phase == .finished })
    }

    @Test func theRowsHaveDistinctIdsPerPhase() {
        let rows = queueRows(queue: queue(), history: [Mk.historyItem("h-run", finishedAt: now)],
                             nowMs: now)
        #expect(Set(rows.map(\.id)).count == rows.count)
        #expect(rows.contains { $0.id == "running|h-run" })
        #expect(rows.contains { $0.id == "finished|h-run" })
    }

    @Test func aWaitingRowCarriesItsPlaceInLineAndItsWait() throws {
        let waiting = queueRows(queue: queue(), history: [], filter: .waiting, nowMs: now)
        let first = try #require(waiting.first)
        #expect(first.position == 1)
        #expect(first.of == 2)
        #expect(first.positionText == "1 of 2")
        #expect(first.timeText == "waiting 2 min")
        #expect(first.durationText == "2 min")
        #expect(waiting[1].position == 2)
        #expect(waiting[1].positionText == "2 of 2")
        #expect(waiting[1].timeText == "waiting 30 s")
    }

    @Test func aRunningRowCountsFromItsGrant() throws {
        let running = queueRows(queue: queue(), history: [], filter: .running, nowMs: now)
        let first = try #require(running.first)
        #expect(first.timeText == "started 4 min ago")
        #expect(first.durationText == "4 min")
        #expect(first.position == nil)
        #expect(first.phase == .running)
        #expect(running[1].phase == .stopping)
        #expect(running[1].timeText == "started 1 min ago")
    }

    @Test func aWaitUnderAMinuteReadsInSeconds() {
        let snapshot = QueueSnapshot(capacity: 1, running: [], waiting: [
            Mk.queueEntry("h-new", ticket: 1, state: .waiting, enqueuedAt: now - 5_000),
        ], foreignRunning: [], asOfSeq: 1)
        let row = queueRows(queue: snapshot, history: [], nowMs: now)[0]
        #expect(row.timeText == "waiting 5 s")
    }

    @Test func aFinishedRowHasItsOutcomeBadgeAndDuration() throws {
        let finished = queueRows(queue: nil, history: history(), filter: .finished, nowMs: now)
        let ok = try #require(finished.first { $0.taskId == "h-ok" })
        #expect(ok.outcome == .ok)
        #expect(ok.outcomeText == "ok")
        #expect(ok.durationText == "2 min")
        #expect(ok.timeText == "finished 2 min ago")
        #expect(ok.exitCode == 0)
        let failed = try #require(finished.first { $0.taskId == "h-bad" })
        #expect(failed.outcomeText == "failed")
        #expect(failed.exitCode == 2)
        #expect(failed.durationText == "1 min 5 s")
        #expect(failed.timeText == "finished 10 min ago")
    }

    @Test func theOutcomeBadgesSpellEveryOutcome() {
        #expect(HeavyOutcome.allCases.map(QueueRow.outcomeTitle)
            == ["ok", "failed", "withdrawn", "timed out", "reaped", "stopped", "force released"])
    }

    @Test func onlyHostlersRunningTasksCanBeStopped() {
        let rows = queueRows(queue: queue(), history: history(), nowMs: now)
        #expect(rows.filter { $0.canStop }.map(\.taskId) == ["h-run"])
        // A stopping task keeps its slot and refuses a second stop (spec §11.1).
        #expect(rows.first { $0.taskId == "h-stop" }?.canStop == false)
        // rn-looky's task is shown, never stopped.
        #expect(rows.first { $0.taskId == "ledger-1" }?.canStop == false)
        #expect(rows.first { $0.taskId == "ledger-1" }?.isForeign == true)
        #expect(rows.filter { $0.canWithdraw }.map(\.taskId) == ["h-w1", "h-w2"])
        #expect(rows.first { $0.taskId == "h-ok" }?.canStop == false)
    }

    @Test func theWorkspaceChipUsesTheWorkspacesColor() throws {
        let workspaces = [Mk.workspace("w-1", project: "p-1", path: "/work/app")]
        let row = try #require(queueRows(queue: queue(), history: [], nowMs: now,
                                         workspaces: workspaces).first)
        #expect(row.workspace?.name == "app")
        #expect(row.workspace?.color == WorkspaceColor(workspaceId: "w-1"))
        #expect(row.workspacePath == "/work/app")
    }

    @Test func aTaskWithoutAWorkspaceFallsBackToItsCwd() throws {
        let snapshot = QueueSnapshot(capacity: 1, running: [
            Mk.queueEntry("h-1", ticket: 1, state: .running, workspace: nil,
                          grantedAt: now),
        ], waiting: [], foreignRunning: [], asOfSeq: 1)
        let row = try #require(queueRows(queue: snapshot, history: [], nowMs: now).first)
        #expect(row.workspace?.name == "tmp")
        #expect(row.workspace?.color == nil)
    }

    @Test func theRowCarriesEveryFieldTheDetailShows() throws {
        let row = try #require(queueRows(queue: queue(), history: [], nowMs: now).first)
        #expect(row.taskId == "h-run")
        #expect(row.ticket == 1)
        #expect(row.kind == "build")
        #expect(row.command == "make test")
        #expect(row.cwd == "/work/app")
        #expect(row.holderPid == Mk.pid.pid)
        #expect(row.source == .hostler)
        #expect(row.grantedAtMs == now - 240_000)
        #expect(row.enqueuedAtMs == now - 60_000)
        #expect(row.finishedAtMs == nil)
        #expect(row.outcome == nil)
    }

    @Test func thePhaseTitlesReadAsThePlanSays() {
        #expect(QueueRow.Phase.allCases.map(\.title)
            == ["waiting", "running", "stopping", "finished"])
        #expect(QueueFilter.allCases.map(\.title) == ["All", "Running", "Waiting", "Finished"])
    }
}

/// The Screen section's model (spec §9.3, §11.2): who holds the screen, since when, and who
/// waits; free when nothing holds it.
@Suite struct ScreenPresentationTests {
    private let now: Int64 = 1_800_000_000_000

    private func screenState(claims: [Claim] = [], waiting: Int = 0) -> ClaimsSnapshot {
        let resource = ResourceInfo(id: ResourceId("screen"), kind: .screen, display: "screen",
                                    attributes: [:], available: true)
        return ClaimsSnapshot(
            resources: [ResourceState(
                resource: resource, claims: claims, foreign: [],
                waiting: (0..<waiting).map {
                    WaitingRequest(requestId: "w\($0)", ticket: 1,
                                   workspacePath: "/work/w\($0)", kind: .hold,
                                   sinceMs: now - 60_000)
                }, seizing: false)],
            asOfSeq: 1)
    }

    private func claim(kind: ClaimKind = .hold) -> Claim {
        Claim(id: "c-screen", resource: ResourceInfo(id: ResourceId("screen"), kind: .screen,
                                                     display: "screen", attributes: [:],
                                                     available: true),
              kind: kind, workspace: WorkspaceRef(id: "w-1", path: "/work/app"),
              binding: .ttl(ttlMs: 1_800_000, expiresAtMs: now + 1_320_000),
              command: "peekaboo run", grantedAtMs: now - 480_000)
    }

    @Test func aFreeScreen() {
        let model = screenModel(screenState(), nowMs: now)
        #expect(model.isFree)
        #expect(model.resourceName == "screen")
        #expect(model.claims.isEmpty)
        #expect(model.waiting.isEmpty)
        #expect(model.holder == nil)
    }

    @Test func aMissingSnapshotIsFree() {
        let model = screenModel(nil, nowMs: now)
        #expect(model.isFree)
        #expect(model.resourceName == "screen")
    }

    @Test func theHolderHasItsChipKindSinceAndTimeLeft() throws {
        let model = screenModel(screenState(claims: [claim()]), nowMs: now)
        #expect(!model.isFree)
        let holder = try #require(model.holder)
        #expect(holder.id == "c-screen")
        #expect(holder.workspace.name == "app")
        #expect(holder.workspace.color == WorkspaceColor(workspaceId: "w-1"))
        #expect(holder.kindText == "hold")
        #expect(holder.command == "peekaboo run")
        #expect(holder.sinceText == "8 min")
        #expect(holder.ttlText == "22 min left")
        #expect(holder.canRenew)
    }

    @Test func aRunningLeaseReadsAsARun() {
        let model = screenModel(screenState(claims: [claim(kind: .lease)]), nowMs: now)
        #expect(model.holder?.kindText == "lease")
    }

    @Test func theWaitingRequestsAndWhoWaits() throws {
        let model = screenModel(screenState(claims: [claim()], waiting: 2), nowMs: now)
        #expect(model.waiting.map(\.request.requestId) == ["w0", "w1"])
        let first = try #require(model.waiting.first)
        #expect(first.workspace.name == "w0")
        #expect(first.kindText == "hold")
        #expect(first.sinceText == "1 min")
    }
}
