import Foundation
import Testing
@testable import HostlerKit

/// The resync cutover as a pure machine (spec §6.2; plan Task 8 `ResyncTests`; R2-6).
@Suite struct ResyncTests {
    /// A machine connected to instance "A" whose first reads (generation 1) are in flight.
    private func reading() -> ResyncMachine {
        var machine = ResyncMachine()
        let effects = machine.handle(.connected(instance: "A", connection: 1))
        #expect(effects == [.read(generation: 1, delay: .zero)])
        return machine
    }

    private func live(seq: UInt64 = 12) -> ResyncMachine {
        var machine = reading()
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: seq)))
        #expect(machine.phase == .live)
        return machine
    }

    // MARK: Cutover by seq

    @Test func aHeldQueueEventAtOrBelowTheSnapshotIsDroppedAndANewerOneApplies() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(10, .queueChanged(Mk.queue(seq: 10, capacity: 7)))))
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12, capacity: 2)))
        #expect(machine.model.queue == Mk.queue(seq: 12, capacity: 2))
        #expect(machine.model.seqs[.queue] == 12)

        var newer = reading()
        _ = newer.handle(.event(Mk.held(13, .queueChanged(Mk.queue(seq: 13, capacity: 5)))))
        _ = newer.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12, capacity: 2)))
        #expect(newer.model.queue == Mk.queue(seq: 13, capacity: 5))
        #expect(newer.model.seqs[.queue] == 13)
        // Every other kind stays at its snapshot.
        #expect(newer.model.seqs[.claims] == 12)
        #expect(newer.model.seqs[.devices] == 12)
    }

    @Test func aHeldRequestEventNewerThanTheRequestsSnapshotUpdatesItAndAnOlderOneIsDropped() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(11, .secretsRequestChanged(Mk.secretRequest("r1", state: .declined)))))
        _ = machine.handle(.event(Mk.held(14, .secretsRequestChanged(Mk.secretRequest("r2", state: .stored)))))
        _ = machine.handle(.event(Mk.held(15, .accountsRequestChanged(Mk.accountRequest("a1", created: Mk.now + 5)))))
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(
            seq: 12, secretRequests: [Mk.secretRequest("r1"), Mk.secretRequest("r2", created: Mk.now + 1)])))
        // The snapshot's order (newest first); r2's newer state replaced it in place.
        #expect(machine.model.secretRequests == [Mk.secretRequest("r2", state: .stored), Mk.secretRequest("r1")])
        #expect(machine.model.secretRequests.first { $0.id == "r1" }?.state == .pending)
        #expect(machine.model.secretRequests.first { $0.id == "r2" }?.state == .stored)
        #expect(machine.model.accountRequests == [Mk.accountRequest("a1", created: Mk.now + 5)])
        #expect(machine.model.pendingSecretRequests.map(\.id) == ["r1"])
    }

    @Test func aRequestHeldTwiceKeepsOnlyItsNewestState() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(13, .secretsRequestChanged(Mk.secretRequest("r1")))))
        _ = machine.handle(.event(Mk.held(14, .secretsRequestChanged(Mk.secretRequest("r1", state: .stored)))))
        #expect(machine.hold.count == 1)
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12)))
        #expect(machine.model.secretRequests == [Mk.secretRequest("r1", state: .stored)])
    }

    @Test func aHeldInstallInTheHistoryAppearsOnceAndOneNotInItIsAdded() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(11, .devicesInstalled(Mk.install("i1", at: Mk.now)))))
        _ = machine.handle(.event(Mk.held(13, .devicesInstalled(Mk.install("i2", at: Mk.now + 1)))))
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12, installs: [Mk.install("i1", at: Mk.now)])))
        #expect(machine.model.installs.map(\.id) == ["i2", "i1"])
    }

    @Test func aHeldSeizureAlreadyKnownAppearsOnceAndANewOneIsAdded() {
        // Seizure records come only from events (no method reads them back), so they are
        // deduplicated against the ones the model already has, across resyncs.
        var machine = live()
        _ = machine.handle(.event(Mk.held(13, .claimsSeized(Mk.seizure("s1")))))
        _ = machine.handle(.refresh)
        _ = machine.handle(.event(Mk.held(13, .claimsSeized(Mk.seizure("s1")))))
        _ = machine.handle(.event(Mk.held(14, .claimsSeized(Mk.seizure("s2", at: Mk.now + 1)))))
        _ = machine.handle(.readsSucceeded(generation: 2, Mk.reads(seq: 14, claimHistory: [Mk.claimEnd("c1")])))
        #expect(machine.model.seizures.map(\.id) == ["s2", "s1"])
        #expect(machine.model.claimHistory == [Mk.claimEnd("c1")])
    }

    @Test func theReplayFollowsSeqWhateverTheOrderOfArrivalInTheHold() {
        var machine = reading()
        // A request replaced in the hold moves to its newer seq, after the seizure.
        _ = machine.handle(.event(Mk.held(13, .secretsRequestChanged(Mk.secretRequest("r1")))))
        _ = machine.handle(.event(Mk.held(14, .claimsSeized(Mk.seizure("s1")))))
        _ = machine.handle(.event(Mk.held(15, .claimsSeized(Mk.seizure("s2")))))
        _ = machine.handle(.event(Mk.held(16, .secretsRequestChanged(Mk.secretRequest("r1", state: .stored)))))
        #expect(machine.hold.ordered.map(\.seq) == [14, 15, 16])
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12)))
        // Records are added newest first, in the order they happened.
        #expect(machine.model.seizures.map(\.id) == ["s2", "s1"])
        #expect(machine.model.secretRequests == [Mk.secretRequest("r1", state: .stored)])
    }

    @Test func recordsAreKeptEvenWhenTheirSeqIsBelowEverySnapshot() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(3, .claimsSeized(Mk.seizure("s1")))))
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12)))
        #expect(machine.model.seizures.map(\.id) == ["s1"])
    }

    // MARK: Live

    @Test func afterTheCutoverAnOlderSnapshotEventIsIgnored() {
        var machine = live(seq: 12)
        _ = machine.handle(.event(Mk.held(12, .devicesChanged(Mk.devices(seq: 12, [Mk.device("d1")])))))
        #expect(machine.model.devices == Mk.devices(seq: 12))
        _ = machine.handle(.event(Mk.held(20, .devicesChanged(Mk.devices(seq: 20, [Mk.device("d1")])))))
        #expect(machine.model.devices == Mk.devices(seq: 20, [Mk.device("d1")]))
        #expect(machine.lastEventApplied)
        _ = machine.handle(.event(Mk.held(19, .devicesChanged(Mk.devices(seq: 19)))))
        #expect(!machine.lastEventApplied)
        #expect(machine.model.devices == Mk.devices(seq: 20, [Mk.device("d1")]))
        #expect(machine.model.seqs[.devices] == 20)
    }

    @Test func liveEventsReplaceUpsertAndAppend() {
        var machine = live(seq: 12)
        _ = machine.handle(.event(Mk.held(13, .claimsChanged(Mk.claims(seq: 13, [Mk.state("screen")])))))
        _ = machine.handle(.event(Mk.held(14, .secretsLockChanged(Mk.secrets(seq: 14, state: .locked)))))
        _ = machine.handle(.event(Mk.held(15, .projectsChanged(Mk.projects(seq: 15, [Mk.project("p1")])))))
        _ = machine.handle(.event(Mk.held(16, .secretsRequestChanged(Mk.secretRequest("r1")))))
        _ = machine.handle(.event(Mk.held(17, .secretsRequestChanged(Mk.secretRequest("r1", state: .stored)))))
        _ = machine.handle(.event(Mk.held(18, .devicesInstalled(Mk.install("i1")))))
        _ = machine.handle(.event(Mk.held(19, .devicesInstalled(Mk.install("i1")))))
        #expect(machine.model.claims == Mk.claims(seq: 13, [Mk.state("screen")]))
        #expect(machine.model.secrets == Mk.secrets(seq: 14, state: .locked))
        #expect(machine.model.projects == Mk.projects(seq: 15, [Mk.project("p1")]))
        #expect(machine.model.secretRequests == [Mk.secretRequest("r1", state: .stored)])
        #expect(machine.model.installs == [Mk.install("i1")])
        #expect(machine.model.seqs == [.queue: 12, .claims: 13, .secrets: 14, .devices: 12,
                                       .projects: 15, .requests: 12])
    }

    // MARK: Instances and connections

    @Test func aNewInstanceClearsThePerKindSeqsBeforeTheNextResync() {
        var machine = live(seq: 50)
        #expect(machine.model.seqs[.queue] == 50)
        _ = machine.handle(.dropped(connection: 1))
        let effects = machine.handle(.connected(instance: "B", connection: 2))
        #expect(effects == [.read(generation: 3, delay: .zero)])
        #expect(machine.model.seqs.isEmpty)
        #expect(machine.model.instance == "B")
        _ = machine.handle(.readsSucceeded(generation: 3, Mk.reads(seq: 4, instance: "B")))
        #expect(machine.model.seqs[.queue] == 4)
        // The new run's numbers apply.
        _ = machine.handle(.event(Mk.held(5, .queueChanged(Mk.queue(seq: 5, capacity: 9)), on: 2)))
        #expect(machine.model.queue?.capacity == 9)
    }

    @Test func aHighSeqFrameOfTheOldConnectionAfterDroppedNeverReachesTheNewInstance() {
        var machine = live(seq: 50)
        _ = machine.handle(.dropped(connection: 1))
        // Connection 1 had read this frame before it closed; it is handed over late.
        _ = machine.handle(.event(Mk.held(100, .queueChanged(Mk.queue(seq: 100, capacity: 9)), on: 1)))
        #expect(machine.hold.isEmpty)
        let r = machine.handle(.connected(instance: "B", connection: 2))
        #expect(r == [.read(generation: 3, delay: .zero)])
        // One more late frame of connection 1 during B's read, and a stale `connected` of it.
        _ = machine.handle(.event(Mk.held(101, .queueChanged(Mk.queue(seq: 101, capacity: 8)), on: 1)))
        let stale = machine.handle(.connected(instance: "A", connection: 1))
        #expect(stale.isEmpty)
        _ = machine.handle(.readsSucceeded(generation: 3, Mk.reads(seq: 1, instance: "B", capacity: 1)))
        #expect(machine.model.queue == Mk.queue(seq: 1, capacity: 1))
        _ = machine.handle(.event(Mk.held(102, .queueChanged(Mk.queue(seq: 102, capacity: 7)), on: 1)))
        #expect(machine.model.queue == Mk.queue(seq: 1, capacity: 1))
        #expect(machine.foreignFramesDiscarded == 3)
        #expect(machine.liveConnection == 2)
        // A late `dropped` of connection 1 leaves connection 2 live.
        let lateDrop = machine.handle(.dropped(connection: 1))
        #expect(lateDrop.isEmpty)
        #expect(machine.phase == .live)
    }

    @Test func onlyTheNewConnectionsEarlyFramesSurviveItsConnected() {
        var machine = live(seq: 50)
        _ = machine.handle(.dropped(connection: 1))
        // Frames of connection 3 arrive before its `connected`; connection 2 never connected.
        _ = machine.handle(.event(Mk.held(7, .queueChanged(Mk.queue(seq: 7, capacity: 3)), on: 2)))
        // Connection 2 fails before its hello: its later frames are not even held.
        _ = machine.handle(.dropped(connection: 2))
        _ = machine.handle(.event(Mk.held(9, .queueChanged(Mk.queue(seq: 9, capacity: 4)), on: 2)))
        #expect(machine.foreignFramesDiscarded == 1)
        _ = machine.handle(.event(Mk.held(8, .devicesInstalled(Mk.install("i1")), on: 3)))
        _ = machine.handle(.connected(instance: "B", connection: 3))
        #expect(machine.hold.ordered == [Mk.held(8, .devicesInstalled(Mk.install("i1")), on: 3)])
        #expect(machine.retiredThrough == 2)
    }

    @Test func theSameInstanceAfterAReconnectKeepsItsSeqs() {
        var machine = live(seq: 50)
        _ = machine.handle(.dropped(connection: 1))
        _ = machine.handle(.connected(instance: "A", connection: 2))
        #expect(machine.model.seqs[.queue] == 50)
    }

    @Test func connectionChangeDuringReadRestarts() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(13, .queueChanged(Mk.queue(seq: 13, capacity: 6)))))
        // A reconnect to a new run while generation 1 is in flight.
        let effects = machine.handle(.connected(instance: "B", connection: 2))
        #expect(effects == [.read(generation: 2, delay: .zero)])
        #expect(machine.hold.isEmpty)
        #expect(machine.abandonedInARow == 1)
        // The first reads answer late: discarded.
        let r1 = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 99)))
        #expect(r1.isEmpty)
        #expect(machine.phase == .reading(generation: 2))
        _ = machine.handle(.readsSucceeded(generation: 2, Mk.reads(seq: 3, instance: "B", capacity: 1)))
        #expect(machine.phase == .live)
        #expect(machine.model.queue == Mk.queue(seq: 3, capacity: 1))
        #expect(machine.model.instance == "B")
        #expect(machine.abandonedInARow == 0)
    }

    @Test func aDropDuringTheReadDiscardsItAndHoldsTheNextConnectionsEvents() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(13, .queueChanged(Mk.queue(seq: 13, capacity: 6)))))
        let r2 = machine.handle(.dropped(connection: 1))
        #expect(r2.isEmpty)
        #expect(machine.phase == .offline)
        #expect(machine.hold.isEmpty)
        // An event of the next connection that arrives before its `connected` is kept.
        _ = machine.handle(.event(Mk.held(21, .queueChanged(Mk.queue(seq: 21, capacity: 4)), on: 2)))
        let r3 = machine.handle(.connected(instance: "A", connection: 2))
        #expect(r3 == [.read(generation: 3, delay: .zero)])
        let r4 = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 99)))
        #expect(r4.isEmpty)
        _ = machine.handle(.readsSucceeded(generation: 3, Mk.reads(seq: 20)))
        #expect(machine.model.queue == Mk.queue(seq: 21, capacity: 4))
    }

    @Test func readsFromAnotherInstanceThanHelloRestart() {
        var machine = reading()
        let effects = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 5, instance: "C")))
        #expect(effects == [.read(generation: 2, delay: .zero)])
        #expect(machine.model.instance == "C")
        #expect(machine.instance == "C")
        #expect(machine.model.queue == nil)
    }

    // MARK: resync, refresh, failures

    @Test func aResyncEventWhileLiveStartsAResyncWithAFreshHold() {
        var machine = live(seq: 12)
        let effects = machine.handle(.event(Mk.held(30, .resync(reason: .lagged))))
        #expect(effects == [.read(generation: 2, delay: .zero)])
        #expect(machine.abandonedInARow == 0)
        // Held, not applied, until the reads answer.
        _ = machine.handle(.event(Mk.held(31, .queueChanged(Mk.queue(seq: 31, capacity: 8)))))
        #expect(machine.model.queue == Mk.queue(seq: 12))
        _ = machine.handle(.readsSucceeded(generation: 2, Mk.reads(seq: 30)))
        #expect(machine.model.queue == Mk.queue(seq: 31, capacity: 8))
    }

    @Test func aResyncEventDuringTheReadRestartsIt() {
        var machine = reading()
        _ = machine.handle(.event(Mk.held(13, .queueChanged(Mk.queue(seq: 13)))))
        let effects = machine.handle(.event(Mk.held(30, .resync(reason: .overflow))))
        #expect(effects == [.read(generation: 2, delay: .zero)])
        #expect(machine.hold.isEmpty)
        #expect(machine.abandonedInARow == 1)
    }

    @Test func refreshRereadsOnlyWhenLive() {
        var machine = reading()
        let r5 = machine.handle(.refresh)
        #expect(r5.isEmpty)
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 12)))
        let r6 = machine.handle(.refresh)
        #expect(r6 == [.read(generation: 2, delay: .zero)])
        #expect(machine.phase == .reading(generation: 2))
    }

    @Test func aFailedReadKeepsTheModelAndRetriesAfterFiveSecondsWithTheHold() {
        var machine = live(seq: 12)
        _ = machine.handle(.refresh)
        _ = machine.handle(.event(Mk.held(15, .queueChanged(Mk.queue(seq: 15, capacity: 3)))))
        let failed = machine.handle(.readsFailed(generation: 2, .timedOut(.statusOverview)))
        #expect(failed == [.retry(generation: 2, delay: .seconds(5))])
        #expect(machine.phase == .waitingRetry(generation: 2))
        #expect(machine.lastError == .timedOut(.statusOverview))
        #expect(machine.model.queue == Mk.queue(seq: 12))
        // A stale retry does nothing; the due one reads again, keeping the hold.
        let r7 = machine.handle(.retryDue(generation: 1))
        #expect(r7.isEmpty)
        let r8 = machine.handle(.retryDue(generation: 2))
        #expect(r8 == [.read(generation: 3, delay: .zero)])
        #expect(machine.hold.count == 1)
        _ = machine.handle(.readsSucceeded(generation: 3, Mk.reads(seq: 14)))
        #expect(machine.model.queue == Mk.queue(seq: 15, capacity: 3))
        #expect(machine.lastError == nil)
    }

    // MARK: The bound (R2-6)

    @Test func theHoldOverflowsOnEntriesAndRestartsWithAFreshHold() {
        var machine = reading()
        for index in 0..<EventHold.maxEntries {
            let r9 = machine.handle(.event(Mk.held(UInt64(100 + index), .devicesInstalled(Mk.install("i\(index)")))))
            #expect(r9.isEmpty)
        }
        #expect(machine.hold.count == EventHold.maxEntries)
        // One more entry would pass the bound: the resync starts again, and the event that did
        // not fit opens the fresh hold (the new reads come after it).
        let r10 = machine.handle(.event(Mk.held(5000, .queueChanged(Mk.queue(seq: 5000)))))
        #expect(r10 == [
            .read(generation: 2, delay: .zero),
        ])
        #expect(machine.hold.ordered == [Mk.held(5000, .queueChanged(Mk.queue(seq: 5000)))])
        #expect(machine.abandonedInARow == 1)
    }

    @Test func theHoldOverflowsOnBytes() {
        var hold = EventHold()
        let event = Event.queueChanged(Mk.queue(seq: 1))
        let r11 = hold.add(HeldEvent(seq: 1, event: event, bytes: EventHold.maxBytes - 10, connection: 1))
        #expect(r11)
        let r12 = hold.add(HeldEvent(seq: 2, event: .devicesInstalled(Mk.install("i1")), bytes: 10, connection: 1))
        #expect(r12)
        let overflowed = !hold.add(HeldEvent(seq: 3, event: .devicesInstalled(Mk.install("i2")), bytes: 1, connection: 1))
        #expect(overflowed)
        #expect(hold.count == 2)
        #expect(hold.bytes == EventHold.maxBytes)
        // A replacement counts the new size instead of the old one.
        let r13 = hold.add(HeldEvent(seq: 4, event: event, bytes: EventHold.maxBytes - 11, connection: 1))
        #expect(r13)
        #expect(hold.bytes == EventHold.maxBytes - 1)
        #expect(hold.ordered.map(\.seq) == [2, 4])
    }

    @Test func afterThreeAbandonedResyncsTheNextWaitsFiveSecondsAndSaysCatchingUp() {
        var machine = reading()
        for generation in 2...3 {
            let r14 = machine.handle(.event(Mk.held(1, .resync(reason: .lagged))))
            #expect(r14 == [
                .read(generation: UInt64(generation), delay: .zero),
            ])
            #expect(!machine.isCatchingUp)
        }
        let r15 = machine.handle(.event(Mk.held(1, .resync(reason: .lagged))))
        #expect(r15 == [
            .read(generation: 4, delay: .seconds(5)),
        ])
        #expect(machine.abandonedInARow == 3)
        #expect(machine.isCatchingUp)
        _ = machine.handle(.readsSucceeded(generation: 4, Mk.reads(seq: 9)))
        #expect(machine.abandonedInARow == 0)
        #expect(!machine.isCatchingUp)
        #expect(machine.abandonedTotal == 3)
    }

    @Test func floodDuringResyncNeverPassesTheBoundAndRestartsOnce() {
        var machine = reading()
        var service = FloodService()
        var largest = 0
        var reads: [ResyncMachine.Effect] = []
        for _ in 0..<FloodService.total {
            let held = service.next()
            reads += machine.handle(.event(Mk.held(held.seq, held.event)))
            largest = max(largest, machine.hold.count)
            #expect(machine.hold.bytes <= EventHold.maxBytes)
        }
        #expect(largest == EventHold.maxEntries)
        #expect(reads == [.read(generation: 2, delay: .zero)])
        #expect(machine.abandonedTotal == 1)
        _ = machine.handle(.readsSucceeded(generation: 1, service.reads()))
        #expect(machine.phase == .reading(generation: 2))
        _ = machine.handle(.readsSucceeded(generation: 2, service.reads()))
        #expect(machine.phase == .live)
        service.expectModel(machine.model)
    }
}

/// A scripted service for the flood: 2,100 installs first (more records than the hold takes),
/// then queue snapshots and the states of 40 requests, 20,000 events in all.
struct FloodService {
    static let total = 20_000
    static let installs = 2_100
    static let requestIds = 40

    private(set) var seq: UInt64 = 0
    private(set) var capacity: UInt8 = 1
    private(set) var requests: [String: SecretRequestInfo] = [:]
    private(set) var installs: [InstallEvent] = []

    mutating func next() -> HeldEvent {
        seq += 1
        let index = Int(seq) - 1
        if index < Self.installs {
            let install = Mk.install("i\(index)", at: Mk.now + Int64(index))
            installs.insert(install, at: 0)
            return HeldEvent(seq: seq, event: .devicesInstalled(install), bytes: 0, connection: 1)
        }
        if index % 2 == 0 {
            capacity = UInt8(index % 200 + 1)
            return HeldEvent(seq: seq, event: .queueChanged(Mk.queue(seq: seq, capacity: capacity)), bytes: 0, connection: 1)
        }
        let id = "r\(index % Self.requestIds)"
        let state: RequestState = index % 3 == 0 ? .stored : .pending
        let request = Mk.secretRequest(id, state: state, created: Mk.now + Int64(index % Self.requestIds))
        requests[id] = request
        return HeldEvent(seq: seq, event: .secretsRequestChanged(request), bytes: 0, connection: 1)
    }

    /// The service's answers now: every snapshot at the current `seq`; the newest 500 installs;
    /// the pending requests.
    func reads() -> ResyncReads {
        let pending = requests.values.filter { $0.state == .pending }
            .sorted { ($0.createdAtMs, $0.id) > ($1.createdAtMs, $1.id) }
        var reads = Mk.reads(seq: seq, capacity: capacity, secretRequests: pending,
                             installs: Array(installs.prefix(500)))
        reads.overview.heavy = Mk.queue(seq: seq, capacity: capacity)
        return reads
    }

    func expectModel(_ model: StoreModel) {
        let reads = reads()
        #expect(model.queue == reads.overview.heavy)
        #expect(model.claims == reads.overview.claims)
        #expect(model.secrets == reads.overview.secrets)
        #expect(model.devices == reads.devices)
        #expect(model.projects == reads.projects)
        #expect(model.secretRequests == reads.requests.secrets)
        #expect(model.installs == reads.installs)
        #expect(model.seqs == Dictionary(uniqueKeysWithValues: SnapshotKind.allCases.map { ($0, seq) }))
    }
}
