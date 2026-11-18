import Foundation
import Testing
@testable import HostlerKit

/// Sleeps that a test controls: each waits until released, and their durations are recorded.
actor HeldSleeper {
    private(set) var requested: [Duration] = []
    private var waiting: [CheckedContinuation<Void, Never>] = []
    var pending: Int { waiting.count }

    func sleep(_ duration: Duration) async {
        requested.append(duration)
        await withCheckedContinuation { waiting.append($0) }
    }

    func releaseAll() {
        let all = waiting
        waiting = []
        for continuation in all { continuation.resume() }
    }
}

@MainActor
@Suite(.timeLimit(.minutes(1))) struct HostlerStoreTests {
    let fake = FakeServiceTransport()
    let clock = FakeClock(Date(timeIntervalSince1970: TimeInterval(Mk.now) / 1000))
    let notifier = FakeNotifier()
    let sleeper = HeldSleeper()

    func makeStore(branches: [String: String] = [:]) -> HostlerStore {
        let sleeper = sleeper
        return HostlerStore(client: HostlerClient(transport: fake), clock: clock, notifier: notifier,
                            gitHead: FakeGitHead(branches: branches),
                            sleep: { await sleeper.sleep($0) })
    }

    /// A store that went live on instance "A".
    func liveStore(branches: [String: String] = [:]) async throws -> HostlerStore {
        let store = makeStore(branches: branches)
        store.connected(instance: "A", connection: 1)
        try await storeSoon("live") { store.isLive }
        await store.settle()
        return store
    }

    /// Publishes one event on the fake service and hands it to the store.
    func push(_ store: HostlerStore, on connection: ConnectionToken? = nil,
              _ make: @Sendable (UInt64) -> Event) async {
        let (event, seq) = await fake.publish(make)
        store.receive(event, seq: seq, connection: connection ?? store.machine.connection ?? 1)
    }

    // MARK: Resync

    @Test func aResyncFillsEveryPartTheHistoriesIncluded() async throws {
        let device = Mk.device("device:android:1")
        let project = Mk.project("p1", workspaces: [Mk.workspace("w1", project: "p1", path: "/work/app")])
        await fake.set {
            $0.seq = 40
            $0.capacity = 3
            $0.running = [Mk.entry("h1")]
            $0.claims = [Mk.state("screen", claims: [Mk.claim("c1", on: "screen", kind: .lease,
                                                              workspace: "w1", path: "/work/app")])]
            $0.lock = .locked
            $0.devices = [device]
            $0.projects = [project]
            $0.secretRequests = [Mk.secretRequest("r1")]
            $0.queueHistory = [Mk.finished("h0", ranMs: 1000, finishedAt: Mk.now - 10)]
            $0.claimHistory = [Mk.claimEnd("c0")]
            $0.installs = [Mk.install("i1")]
        }
        let store = try await liveStore(branches: ["/work/app": "main"])

        let model = store.model
        #expect(model.queue == Mk.queue(seq: 40, capacity: 3, running: [Mk.entry("h1")]))
        #expect(model.claims == (await fake.claimsSnapshot()))
        #expect(model.secrets == Mk.secrets(seq: 40, state: .locked, pending: 1))
        #expect(model.devices == Mk.devices(seq: 40, [device]))
        #expect(model.projects == Mk.projects(seq: 40, [project]))
        #expect(model.secretRequests == [Mk.secretRequest("r1")])
        #expect(model.service == Mk.service(instance: "A"))
        #expect(model.queueHistory == [Mk.finished("h0", ranMs: 1000, finishedAt: Mk.now - 10)])
        #expect(model.claimHistory == [Mk.claimEnd("c0")])
        #expect(model.installs == [Mk.install("i1")])
        #expect(model.seqs == Dictionary(uniqueKeysWithValues: SnapshotKind.allCases.map { ($0, 40) }))
        #expect(model.instance == "A")
        #expect(store.syncState == .live)
        #expect(store.branches == ["w1": "main"])

        // Exactly the plan's reads, each once.
        let dayAgo = Mk.now - 24 * 3600 * 1000
        let calls = await fake.calls
        #expect(calls.count == 8)
        for request in [
            Request.statusOverview, .devicesList(all: true), .projectsSnapshot, .requestsSnapshot,
            .serviceStatus, .queueHistory(HistoryQuery(sinceMs: nil, limit: 200)),
            .claimsHistory(HistoryQuery(sinceMs: dayAgo, limit: 500)),
            .devicesInstalls(device: nil, query: HistoryQuery(sinceMs: dayAgo, limit: 500)),
        ] {
            #expect(calls.contains(request), "missing \(request.method.rawValue)")
        }
    }

    @Test func eachEventKindReplacesUpsertsOrAppendsWhileLive() async throws {
        let store = try await liveStore()
        await push(store) { .queueChanged(Mk.queue(seq: $0, capacity: 5)) }
        await push(store) { .claimsChanged(Mk.claims(seq: $0, [Mk.state("screen")])) }
        await push(store) { .secretsLockChanged(Mk.secrets(seq: $0, state: .locked)) }
        await push(store) { .devicesChanged(Mk.devices(seq: $0, [Mk.device("d1")])) }
        await push(store) { .projectsChanged(Mk.projects(seq: $0, [Mk.project("p1")])) }
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1")) }
        await push(store) { _ in .accountsRequestChanged(Mk.accountRequest("a1")) }
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1", state: .declined)) }
        await push(store) { _ in .claimsSeized(Mk.seizure("s1")) }
        await push(store) { _ in .devicesInstalled(Mk.install("i1")) }
        await push(store) { _ in .devicesInstalled(Mk.install("i1")) }

        let model = store.model
        #expect(model.queue == Mk.queue(seq: 1, capacity: 5))
        #expect(model.claims == Mk.claims(seq: 2, [Mk.state("screen")]))
        #expect(model.secrets == Mk.secrets(seq: 3, state: .locked))
        #expect(model.devices == Mk.devices(seq: 4, [Mk.device("d1")]))
        #expect(model.projects == Mk.projects(seq: 5, [Mk.project("p1")]))
        #expect(model.secretRequests == [Mk.secretRequest("r1", state: .declined)])
        #expect(model.accountRequests == [Mk.accountRequest("a1")])
        #expect(model.seizures == [Mk.seizure("s1")])
        #expect(model.installs == [Mk.install("i1")])
        #expect(store.lastEventAt == clock.now())
    }

    @Test func aResyncEventRereadsEverything() async throws {
        let store = try await liveStore()
        #expect(await fake.callCount(.statusOverview) == 1)
        await fake.set { $0.capacity = 9 }
        await push(store) { _ in .resync(reason: .lagged) }
        #expect(!store.isLive)
        #expect(store.syncState == .syncing(catchingUp: false))
        try await storeSoon("live again") { store.isLive }
        #expect(await fake.callCount(.statusOverview) == 2)
        #expect(store.model.queue?.capacity == 9)
    }

    @Test func anOfflineToLiveTransitionRereads() async throws {
        let store = try await liveStore()
        clock.advance(by: 30)
        store.dropped(connection: 1)
        #expect(store.syncState == .offline(since: clock.now()))
        #expect(store.offlineSince == clock.now())
        // The previous model stays on screen while offline.
        #expect(store.model.queue == Mk.queue(seq: 0))
        await fake.set { $0.seq = 7; $0.capacity = 4 }
        store.connected(instance: "A", connection: 2)
        try await storeSoon("live again") { store.isLive }
        #expect(await fake.callCount(.statusOverview) == 2)
        #expect(store.model.queue == Mk.queue(seq: 7, capacity: 4))
        #expect(store.offlineSince == nil)
    }

    @Test func refreshRereads() async throws {
        let store = try await liveStore()
        store.refresh()
        try await storeSoon("reread") { await fake.callCount(.statusOverview) == 2 }
        try await storeSoon("live") { store.isLive }
    }

    @Test func aFailedReadKeepsTheModelShowsTheErrorAndRetriesAfterFiveSeconds() async throws {
        let store = try await liveStore()
        await fake.set { $0.failing = [.devicesList: .timedOut(.devicesList)]; $0.capacity = 6 }
        store.refresh()
        try await storeSoon("the failure") {
            if case .failed = store.syncState { return true }
            return false
        }
        #expect(store.syncState == .failed(.service(message: "hostler did not answer devices.list in time",
                                                    recovery: nil)))
        #expect(store.model.queue == Mk.queue(seq: 0))
        try await storeSoon("the retry's sleep") { await sleeper.pending == 1 }
        #expect(await sleeper.requested == [.seconds(5)])
        await fake.set { $0.failing = [:] }
        await sleeper.releaseAll()
        try await storeSoon("live") { store.isLive }
        #expect(store.model.queue?.capacity == 6)
        #expect(await fake.callCount(.devicesList) == 3)
    }

    // MARK: R2-6

    @Test func floodDuringResync() async throws {
        await fake.holdReads()
        let store = makeStore()
        store.connected(instance: "A", connection: 1)
        try await storeSoon("the first reads") { await fake.heldCount == 8 }

        var largest = 0
        for index in 0..<20_000 {
            if index < 2_100 {
                // More records than the hold takes: the resync must start again once.
                let (event, seq) = await fake.publish { .devicesInstalled(Mk.install("i\($0)", at: Mk.now + Int64($0))) }
                if case .devicesInstalled(let install) = event {
                    await fake.set { $0.installs.insert(install, at: 0) }
                }
                store.receive(event, seq: seq, connection: 1)
            } else if index % 2 == 0 {
                await fake.set { $0.capacity = UInt8(index % 200 + 1) }
                let capacity = await fake.capacity
                await push(store) { .queueChanged(Mk.queue(seq: $0, capacity: capacity)) }
            } else {
                let request = Mk.secretRequest("r\(index % 40)", state: index % 3 == 0 ? .stored : .pending,
                                               created: Mk.now + Int64(index % 40))
                await fake.set { fake in
                    fake.secretRequests.removeAll { $0.id == request.id }
                    fake.secretRequests.append(request)
                }
                await push(store) { _ in .secretsRequestChanged(request) }
            }
            largest = max(largest, store.machine.hold.count)
            #expect(store.machine.hold.count <= EventHold.maxEntries)
            #expect(store.machine.hold.bytes <= EventHold.maxBytes)
        }
        #expect(largest == EventHold.maxEntries)
        #expect(store.machine.abandonedTotal == 1)
        // The restarted resync's reads are held too.
        try await storeSoon("the second reads") { await fake.heldCount == 16 }
        #expect(!store.isLive)

        await fake.release()
        try await storeSoon("live") { store.isLive }
        await store.settle()
        let model = store.model
        let seq = await fake.seq
        #expect(seq == 20_000)
        #expect(model.queue == (await fake.queueSnapshot()))
        #expect(model.claims == (await fake.claimsSnapshot()))
        #expect(model.secrets == (await fake.secretsStatus()))
        #expect(model.devices == (await fake.devicesSnapshot()))
        #expect(model.projects == (await fake.projectsSnapshot()))
        #expect(model.secretRequests == (await fake.pendingSecrets()))
        #expect(model.installs == Array((await fake.installs).prefix(500)))
        #expect(model.seqs == Dictionary(uniqueKeysWithValues: SnapshotKind.allCases.map { ($0, seq) }))
        // Two resyncs were read (8 reads each); the first one's answers were dropped.
        #expect(await fake.callCount(.statusOverview) == 2)
    }

    @Test func aLateFrameOfTheOldConnectionNeverReachesTheNewOne() async throws {
        let store = try await liveStore()
        await fake.set { $0.seq = 100 }
        store.dropped(connection: 1)
        // Read from connection 1 before it closed, handed over after its `dropped`.
        store.receive(.queueChanged(Mk.queue(seq: 100, capacity: 9)), seq: 100, connection: 1)
        #expect(store.machine.hold.isEmpty)
        // The service restarted (instance B numbers from 1); connection 2 reads it.
        await fake.set { $0.instance = "B"; $0.seq = 1; $0.capacity = 1 }
        store.connected(instance: "B", connection: 2)
        // Another late frame of connection 1, now during the resync, and one after it.
        store.receive(.queueChanged(Mk.queue(seq: 101, capacity: 8)), seq: 101, connection: 1)
        try await storeSoon("live") { store.isLive }
        await store.settle()
        store.receive(.queueChanged(Mk.queue(seq: 102, capacity: 7)), seq: 102, connection: 1)
        #expect(store.model.queue == Mk.queue(seq: 1, capacity: 1))
        #expect(store.model.instance == "B")
        #expect(store.machine.foreignFramesDiscarded == 3)
        // A late `dropped` of connection 1 does not take connection 2 down.
        store.dropped(connection: 1)
        #expect(store.isLive)
        await push(store) { .queueChanged(Mk.queue(seq: $0, capacity: 2)) }
        #expect(store.model.queue == Mk.queue(seq: 2, capacity: 2))
    }

    @Test func connectionChangeDuringReadRestarts() async throws {
        await fake.holdReads()
        let store = makeStore()
        store.connected(instance: "A", connection: 1)
        try await storeSoon("the first reads") { await fake.heldCount == 8 }
        await push(store) { .queueChanged(Mk.queue(seq: $0, capacity: 7)) }

        // The service restarts: a new instance numbers from 1 again.
        await fake.set { $0.instance = "B"; $0.seq = 3; $0.capacity = 1 }
        store.dropped(connection: 1)
        store.connected(instance: "B", connection: 2)
        #expect(store.model.seqs.isEmpty)
        try await storeSoon("the second reads") { await fake.heldCount == 16 }
        await fake.release()
        try await storeSoon("live") { store.isLive }
        await store.settle()
        #expect(store.model.instance == "B")
        #expect(store.model.queue == Mk.queue(seq: 3, capacity: 1))
        #expect(store.model.seqs[.queue] == 3)
        #expect(store.machine.abandonedTotal == 1)
        // The new run's numbers apply live.
        await push(store) { .queueChanged(Mk.queue(seq: $0, capacity: 2)) }
        #expect(store.model.queue == Mk.queue(seq: 4, capacity: 2))
    }

    @Test func afterThreeAbandonedResyncsTheNextWaitsAndTheFooterSaysCatchingUp() async throws {
        await fake.holdReads()
        let store = makeStore()
        store.connected(instance: "A", connection: 1)
        for _ in 0..<3 { await push(store) { _ in .resync(reason: .overflow) } }
        #expect(store.syncState == .syncing(catchingUp: true))
        try await storeSoon("the catch-up wait") { await sleeper.pending == 1 }
        #expect(await sleeper.requested == [.seconds(5)])
        await fake.release()
        await sleeper.releaseAll()
        try await storeSoon("live") { store.isLive }
        #expect(store.machine.abandonedInARow == 0)
    }

    // MARK: Histories, secrets, branches

    @Test func aFinishedTaskRereadsTheQueueHistory() async throws {
        await fake.set { $0.running = [Mk.entry("h1")] }
        let store = try await liveStore()
        await fake.set { $0.running = []; $0.queueHistory = [Mk.finished("h1", ranMs: 5, finishedAt: Mk.now)] }
        await push(store) { .queueChanged(Mk.queue(seq: $0)) }
        try await storeSoon("the history") { store.model.queueHistory.map(\.id) == ["h1"] }
        #expect(await fake.callCount(.queueHistory) == 2)
        #expect(await fake.callCount(.claimsHistory) == 1)
    }

    @Test func anEndedClaimRereadsTheClaimHistory() async throws {
        let lease = Mk.claim("c1", on: "screen", kind: .lease, workspace: "w1", path: "/work/app")
        await fake.set { $0.claims = [Mk.state("screen", claims: [lease])] }
        let store = try await liveStore()
        await fake.set { $0.claimHistory = [Mk.claimEnd("c1")] }
        await push(store) { .claimsChanged(Mk.claims(seq: $0, [Mk.state("screen")])) }
        try await storeSoon("the history") { store.model.claimHistory == [Mk.claimEnd("c1")] }
        #expect(await fake.callCount(.queueHistory) == 1)
    }

    @Test func projectSecretsLoadOnDemandAndRereadOnLockChangesAndStoredRequests() async throws {
        let project = Mk.project("p1")
        let list = SecretList(project: project.info, secrets: [])
        await fake.set { $0.projects = [project]; $0.secretLists = ["p1": list] }
        let store = try await liveStore()
        #expect(await fake.callCount(.secretList) == 0)
        store.loadSecrets(of: project)
        try await storeSoon("the list") { store.projectSecrets["p1"] == list }
        await push(store) { .secretsLockChanged(Mk.secrets(seq: $0, state: .locked)) }
        try await storeSoon("the lock re-read") { await fake.callCount(.secretList) == 2 }
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1", state: .stored, project: "p1")) }
        try await storeSoon("the stored re-read") { await fake.callCount(.secretList) == 3 }
        // A request for a project that is not loaded reads nothing.
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r2", state: .stored, project: "p9")) }
        store.refreshSecrets()
        try await storeSoon("the focus re-read") { await fake.callCount(.secretList) == 4 }
        await store.settle()
        #expect(await fake.callCount(.secretList) == 4)
    }

    @Test func anOlderSecretsAnswerNeverOverwritesANewerOne() async throws {
        let project = Mk.project("p1")
        let before = SecretList(project: project.info, secrets: [
            SecretInfo(name: "API_KEY", declared: true, state: .missing)])
        let after = SecretList(project: project.info, secrets: [
            SecretInfo(name: "API_KEY", declared: true, state: .set)])
        await fake.set { $0.projects = [project]; $0.gateSecretLists = true }
        let store = try await liveStore()
        store.loadSecrets(of: project)
        try await storeSoon("the first read") { await fake.waitingSecretLists == 1 }
        await push(store) { .secretsLockChanged(Mk.secrets(seq: $0, state: .locked)) }
        try await storeSoon("the second read") { await fake.waitingSecretLists == 2 }

        // The newer read answers first; the older one answers late, with old data.
        await fake.answerSecretList(1, with: .success(after))
        try await storeSoon("the newer answer") { store.projectSecrets["p1"] == after }
        await fake.answerSecretList(0, with: .success(before))
        await store.settle()
        #expect(store.projectSecrets["p1"] == after)

        // A newer read that succeeds, then an older one that fails late: the error is dropped.
        store.refreshSecrets()
        try await storeSoon("the third read") { await fake.waitingSecretLists == 1 }
        store.refreshSecrets()
        try await storeSoon("the fourth read") { await fake.waitingSecretLists == 2 }
        await fake.answerSecretList(1, with: .success(before))
        try await storeSoon("the fourth answer") { store.projectSecrets["p1"] == before }
        await fake.answerSecretList(0, with: .failure(.timedOut(.secretList)))
        await store.settle()
        #expect(store.projectSecrets["p1"] == before)
        #expect(store.projectSecretErrors["p1"] == nil)

        // An answer to a read sent on a connection that has since dropped is dropped too.
        store.refreshSecrets()
        try await storeSoon("the fifth read") { await fake.waitingSecretLists == 1 }
        store.dropped(connection: 1)
        await fake.answerSecretList(0, with: .success(after))
        await store.settle()
        #expect(store.projectSecrets["p1"] == before)
    }

    @Test func branchesFollowProjectsChanges() async throws {
        let store = try await liveStore(branches: ["/work/app": "main", "/work/app-2": "feature/x"])
        #expect(store.branches.isEmpty)
        let project = Mk.project("p1", workspaces: [
            Mk.workspace("w1", project: "p1", path: "/work/app"),
            Mk.workspace("w2", project: "p1", path: "/work/app-2"),
            Mk.workspace("w3", project: "p1", path: "/work/gone"),
        ])
        await push(store) { .projectsChanged(Mk.projects(seq: $0, [project])) }
        #expect(store.branches == ["w1": "main", "w2": "feature/x"])
        #expect(store.workspaceRows(of: project).map(\.branch) == ["main", "feature/x", nil])
    }

    // MARK: Notifications

    @Test func atTheFirstResyncWaitingRequestsGiveOneSummaryAndNewThingsNotifyOnce() async throws {
        await fake.set {
            $0.secretRequests = [Mk.secretRequest("r0")]
            $0.devices = [Mk.device("d0", registered: false)]
        }
        let store = try await liveStore()
        // P5-L11: one summary for what waits; the device is a baseline.
        #expect(notifier.posted.map(\.id) == ["requests-waiting"])
        #expect(notifier.posted.first?.title == "A request is waiting")

        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1")) }
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1")) }
        await push(store) { .devicesChanged(Mk.devices(seq: $0, [Mk.device("d0", registered: false),
                                                                  Mk.device("d1", registered: false)])) }
        await push(store) { _ in .claimsSeized(Mk.foreignSeizure("s1")) }
        #expect(notifier.posted.map(\.id) == ["requests-waiting", "secret-request:r1", "device:d1", "seizure:s1"])

        // A resync that reads the same things again posts nothing new.
        await fake.set { $0.secretRequests.append(Mk.secretRequest("r1")) }
        store.refresh()
        try await storeSoon("live") { store.isLive }
        await store.settle()
        #expect(notifier.posted.count == 4)
    }

    @Test func aSeizureLostInAResyncNotifiesOnceFromTheHistoryRead() async throws {
        let store = try await liveStore()
        #expect(notifier.posted.isEmpty)
        // The service could not deliver the `claims.seized` event: it sends `resync`, and the
        // seizure is only in the history the resync reads (a hold and its lease: two rows).
        clock.advance(by: 5)
        let at = Mk.now + 5_000
        await fake.set {
            $0.claimHistory = [
                ClaimHistoryItem(id: "c1", resource: "screen", kind: .hold, workspacePath: "/work/app",
                                 endedAtMs: at, reason: .seized, detail: "/work/other"),
                ClaimHistoryItem(id: "c2", resource: "screen", kind: .lease, workspacePath: "/work/app",
                                 endedAtMs: at, reason: .seized, detail: "/work/other"),
                // Seized before the app's first read: never notifies.
                ClaimHistoryItem(id: "c0", resource: "screen", kind: .lease, workspacePath: "/work/app",
                                 endedAtMs: Mk.now - 60_000, reason: .seized, detail: "/work/other"),
            ]
        }
        await push(store) { _ in .resync(reason: .overflow) }
        try await storeSoon("live") { store.isLive }
        await store.settle()
        #expect(notifier.posted.map(\.id) == ["seizure:claims|screen|/work/app|/work/other|\(at)"])
        #expect(notifier.posted.first?.title == "A claim was taken over")
        store.refresh()
        try await storeSoon("live") { store.isLive }
        await store.settle()
        #expect(notifier.posted.count == 1)
    }

    @Test func aLiveSeizureOfHostlersClaimsNotifiesOnceFromItsHistoryRow() async throws {
        let store = try await liveStore()
        clock.advance(by: 5)
        let at = Mk.now + 5_000
        await fake.set {
            $0.claimHistory = [ClaimHistoryItem(id: "c1", resource: "screen", kind: .lease,
                                                workspacePath: "/work/app", endedAtMs: at, reason: .seized,
                                                detail: "/work/other")]
        }
        await push(store) { _ in .claimsSeized(Mk.seizure("s1", at: at)) }
        try await storeSoon("the history re-read") { await fake.callCount(.claimsHistory) == 2 }
        await store.settle()
        #expect(notifier.posted.map(\.id) == ["seizure:claims|screen|/work/app|/work/other|\(at)"])
        #expect(store.model.seizures.map(\.id) == ["s1"])
    }

    @Test func aTaskThatBecomesStuckNotifiesOnceOnATick() async throws {
        await fake.set { $0.running = [Mk.entry("h1", grantedAtMs: Mk.now)] }
        let store = try await liveStore()
        clock.advance(by: 59 * 60)
        store.tick()
        #expect(notifier.posted.isEmpty)
        clock.advance(by: 2 * 60)
        store.tick()
        store.tick()
        #expect(notifier.posted.map(\.id) == ["stuck:h1"])
        #expect(notifier.posted.first?.target == .queue)
    }

    @Test func settingsGateEachKind() async throws {
        let store = try await liveStore()
        store.notificationSettings.request = false
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1")) }
        await push(store) { _ in .claimsSeized(Mk.foreignSeizure("s1")) }
        #expect(notifier.posted.map(\.id) == ["seizure:s1"])
    }

    // MARK: Derived

    @Test func statusItemAppearanceFollowsTheModel() async throws {
        let store = makeStore()
        #expect(store.statusItemAppearance == .offline)
        await fake.set { $0.secretRequests = [Mk.secretRequest("r1")] }
        store.connected(instance: "A", connection: 1)
        try await storeSoon("live") { store.isLive }
        #expect(store.statusItemAppearance == .attention)
        await push(store) { _ in .secretsRequestChanged(Mk.secretRequest("r1", state: .stored)) }
        #expect(store.statusItemAppearance == .idle)
        store.dropped(connection: 1)
        #expect(store.statusItemAppearance == .offline)
    }

    @Test func standingOfADeviceUsesTheClaimsAndWorkspaces() async throws {
        let device = Mk.device("device:android:1")
        await fake.set {
            $0.devices = [device]
            $0.claims = [Mk.state("device:android:1", claims: [
                Mk.claim("c1", on: "device:android:1", kind: .reservation, workspace: "w1", path: "/work/app"),
            ])]
        }
        let store = try await liveStore()
        #expect(store.standing(of: device) == [.reserved(by: WorkspaceChip(workspaceId: "w1", path: "/work/app"))])
        #expect(store.standing(of: Mk.device("device:android:2")) == [.free])
    }

    /// Clean read C-B1: the scope an `account.list` read is bound to.
    @Test func theAccountReadScopeFollowsTheConnectionTheInstanceAndTheProject() async throws {
        await fake.set { $0.projects = [Mk.project("p1")] }
        let store = try await liveStore()
        #expect(store.accountReadScope(of: "p1") == AccountReadScope(
            connection: 1, instance: "A", projectId: "p1", commonDir: "/work/p1/.git", name: "p1"))
        #expect(store.accountReadScope(of: "p404") == nil)
        #expect(store.accountReadScope(of: nil) == nil)
        store.dropped(connection: 1)
        #expect(store.accountReadScope(of: "p1") == nil)
        await fake.set { $0.instance = "B" }
        store.connected(instance: "B", connection: 2)
        try await storeSoon("live again") { store.isLive }
        #expect(store.accountReadScope(of: "p1")?.connection == 2)
        #expect(store.accountReadScope(of: "p1")?.instance == "B")
    }
}
