import Foundation
import Testing
@testable import HostlerKit

/// Records what the controller tells the store.
@MainActor
final class RecordingSink: ConnectionEventSink {
    enum Call: Equatable {
        case connected(instance: String?, connection: ConnectionToken)
        case dropped(ConnectionToken)
        case receive(seq: UInt64, connection: ConnectionToken, frameBytes: Int?)
    }

    var calls: [Call] = []
    var events: [Event] = []

    func connected(instance: String?, connection: ConnectionToken) {
        calls.append(.connected(instance: instance, connection: connection))
    }

    func dropped(connection: ConnectionToken) {
        calls.append(.dropped(connection))
    }

    func receive(_ event: Event, seq: UInt64, connection: ConnectionToken, frameBytes: Int?) {
        calls.append(.receive(seq: seq, connection: connection, frameBytes: frameBytes))
        events.append(event)
    }

    var connections: [Call] {
        calls.filter { if case .receive = $0 { false } else { true } }
    }
}

/// Each connection state from a fake (spec §9.7, P5-L12), reconnects, tokens and the grant.
@MainActor
@Suite(.timeLimit(.minutes(1)), .serialized) struct ConnectionControllerTests {
    static let user = "/Users/someone"
    static let defaultHome = SocketLocator.defaultHome(homeDirectory: user)
    static let location = SocketLocator.location(home: defaultHome, euid: 501, fromEnvironment: false)
    static let registered = Registration(executable: "/opt/hostler", setsEnvironment: false)

    static func make(_ connector: FakeConnector, registration: Registration? = nil,
                     location: Result<SocketLocation, HostlerAppError> = .success(location),
                     connectTimeout: Duration = .seconds(2))
        -> (ConnectionController, FakeSleeper, RecordingSink)
    {
        let sleeper = FakeSleeper()
        let controller = ConnectionController(environment: ConnectionEnvironment(
            location: location, defaultHome: defaultHome, euid: 501, connector: connector,
            registration: FakeRegistration(value: registration),
            fileExists: { $0 == "/opt/hostler" }, sleeper: sleeper,
            clientName: "hostler-app test", connectTimeout: connectTimeout))
        let sink = RecordingSink()
        controller.sink = sink
        return (controller, sleeper, sink)
    }

    static func line(_ fixture: String) throws -> String {
        String(decoding: try ContractFixtures.json(fixture).encoded(), as: UTF8.self)
    }

    static func seq(_ fixture: String) throws -> UInt64 {
        guard case .int(let n)? = try ContractFixtures.json(fixture)["seq"] else { return 0 }
        return UInt64(n)
    }

    static func lockChanged(_ state: String, seq: Int) -> String {
        """
        {"event":{"type":"secrets.lock_changed","data":{"state":"\(state)","database":"/d","key_file":null,"pending_requests":0,"preserved_files":[],"as_of_seq":\(seq)}},"seq":\(seq)}
        """
    }

    // MARK: - States

    @Test func aMissingSocketWithoutARegistrationOffersInstallAndBacksOff() async throws {
        let (controller, sleeper, _) = Self.make(FakeConnector([.fail(.notRunning(path: "/s"))]))
        controller.start()
        try await eventually("not running") {
            controller.state == .notRunning(offer: .typeCommand("hostler service install"))
        }
        try await eventually("backoff") { sleeper.waitingCount == 1 }
        #expect(sleeper.durations == [.milliseconds(500)])
        await controller.stop()
    }

    @Test func aMissingSocketWithARegistrationOffersStart() async throws {
        let (controller, _, _) = Self.make(FakeConnector([.fail(.notRunning(path: "/s"))]),
                                           registration: Self.registered)
        controller.start()
        try await eventually("not running") {
            controller.state == .notRunning(offer: .start(executable: "/opt/hostler"))
        }
        await controller.stop()
    }

    @Test func aForeignOwnerIsRefusedAndWaitsForRetry() async throws {
        let connector = FakeConnector([.fail(.foreignOwner(path: "/s/hostler.sock", uid: 0))])
        let (controller, sleeper, _) = Self.make(connector)
        controller.start()
        try await eventually("foreign") { controller.state == .foreignOwner(path: "/s/hostler.sock") }
        #expect(connector.connectedPaths.first == Self.location.socketPath)
        try await Task.sleep(for: .milliseconds(200))
        #expect(connector.connectCount == 1)
        #expect(sleeper.durations.isEmpty)
        controller.retryNow()
        try await eventually("retried") { connector.connectCount == 2 }
        await controller.stop()
    }

    @Test func eaccesIsDeniedAndWaitsForRetry() async throws {
        let error = SocketError.denied(path: "/s", reason: "Permission denied")
        let connector = FakeConnector([.fail(error)])
        let (controller, sleeper, _) = Self.make(connector)
        controller.start()
        try await eventually("denied") {
            controller.state == .denied(reason: "this app may not reach /s: Permission denied")
        }
        try await Task.sleep(for: .milliseconds(200))
        #expect(connector.connectCount == 1)
        #expect(sleeper.durations.isEmpty)
        controller.retryNow()
        try await eventually("retried") { connector.connectCount == 2 }
        await controller.stop()
    }

    @Test func aRelativeHomeIsShownAndNothingConnects() async throws {
        let connector = FakeConnector([.fail(.notRunning(path: "/s"))])
        let (controller, _, _) = Self.make(connector, location: .failure(.badHome("rel")))
        controller.start()
        try await eventually("bad home") { controller.state == .badHome("rel") }
        #expect(connector.connectCount == 0)
        await controller.stop()
    }

    /// Task 13 F-2: on a custom home the mismatch's recovery names that home, as the
    /// not-running banner's command does; on the default home it stays as the service wrote it.
    @Test func aMismatchRecoveryNamesACustomHome() async throws {
        let custom = SocketLocator.location(home: "/tmp/hv 13", euid: 501, fromEnvironment: true)
        let connector = FakeConnector([.stream(FakeSocketStream(respond: FakeService.standard(contract: 4)))])
        let (controller, _, _) = Self.make(connector, location: .success(custom))
        controller.start()
        try await eventually("mismatch") {
            if case .mismatch = controller.state { true } else { false }
        }
        #expect(controller.state == .mismatch(
            client: 5, service: 4, serviceVersion: "0.0.4",
            recovery: "HOSTLER_HOME='/tmp/hv 13' hostler service start --replace"))
        await controller.stop()
    }

    @Test func theHomeRuleForCommands() {
        let custom = SocketLocator.location(home: "/tmp/hv13", euid: 501, fromEnvironment: true)
        #expect(commandForHome("hostler service restart", home: custom, defaultHome: Self.defaultHome,
                               euid: 501) == "HOSTLER_HOME=/tmp/hv13 hostler service restart")
        #expect(commandForHome("hostler service restart", home: Self.location,
                               defaultHome: Self.defaultHome, euid: 501) == "hostler service restart")
        // Not a hostler command: left as it is.
        #expect(commandForHome("brew upgrade hostler", home: custom, defaultHome: Self.defaultHome,
                               euid: 501) == "brew upgrade hostler")
    }

    @Test func aContractMismatchWaitsForRetryThenAsksServiceStatusFirst() async throws {
        let old = FakeSocketStream(respond: FakeService.standard(contract: 4))
        let stillOld = FakeSocketStream(respond: FakeService.standard(contract: 4))
        let poll = FakeSocketStream(respond: FakeService.standard(contract: 5))
        let upgraded = FakeSocketStream(respond: FakeService.standard(serviceVersion: "0.0.5"))
        let connector = FakeConnector([.stream(old), .stream(stillOld), .stream(poll),
                                       .stream(upgraded)])
        let (controller, sleeper, sink) = Self.make(connector)
        controller.start()
        let mismatch = ConnectionState.mismatch(client: 5, service: 4, serviceVersion: "0.0.4",
                                                recovery: "hostler service start --replace")
        try await eventually("mismatch") { controller.state == mismatch }
        #expect(old.isClosed)
        // No poll by itself (P5-L12): only Retry or the popover asks again.
        try await Task.sleep(for: .milliseconds(200))
        #expect(connector.connectCount == 1)
        #expect(sleeper.durations.isEmpty)

        controller.retryNow()
        try await eventually("polled once") { connector.connectCount == 2 && stillOld.isClosed }
        #expect(stillOld.methods == ["service.status"])
        #expect(controller.state == mismatch)

        controller.retryNow()
        try await eventually("live after the upgrade") {
            controller.state == .live(serviceVersion: "0.0.5")
        }
        // The poll was a lifecycle connection: service.status alone, no hello; then closed.
        #expect(poll.methods == ["service.status"])
        #expect(poll.isClosed)
        #expect(sleeper.durations.isEmpty)
        // Connections 1 (mismatch), 2 and 3 (polls), 4 (live).
        #expect(sink.connections == [.connected(instance: "run-1", connection: 4)])
        await controller.stop()
    }

    /// R1-4: the mismatched service goes away: the next attempts follow the backoff.
    @Test func aMismatchedServiceThatDisappearsBacksOff() async throws {
        let old = FakeSocketStream(respond: FakeService.standard(contract: 4))
        let connector = FakeConnector([.stream(old), .fail(.notRunning(path: "/s"))])
        let (controller, sleeper, _) = Self.make(connector)
        controller.start()
        try await eventually("mismatch") {
            if case .mismatch = controller.state { true } else { false }
        }
        controller.retryNow()
        try await eventually("not running") {
            controller.state == .notRunning(offer: .typeCommand("hostler service install"))
        }
        for count in 1...3 {
            try await eventually("backoff \(count)") { sleeper.waitingCount == 1 }
            #expect(connector.connectCount == 1 + count)
            sleeper.releaseWaiting()
        }
        try await eventually("fourth wait") { sleeper.waitingCount == 1 }
        #expect(sleeper.durations == [.milliseconds(500), .seconds(1), .seconds(2), .seconds(5)])
        #expect(connector.connectCount == 5)
        await controller.stop()
    }

    @Test func anotherHelloErrorIsAProtocolErrorAndBacksOff() async throws {
        let stream = FakeSocketStream { frame in
            ["{\"id\":\(FakePeer.text(frame["id"])),\"error\":{\"code\":\"internal\",\"message\":\"boom\",\"details\":{},\"recovery\":null}}"]
        }
        let (controller, sleeper, _) = Self.make(FakeConnector([.stream(stream)]))
        controller.start()
        try await eventually("protocol error") { controller.state == .protocolError("boom") }
        try await eventually("backoff") { sleeper.waitingCount == 1 }
        await controller.stop()
    }

    // MARK: - R1-3: a connect that never completes

    @Test func aConnectThatNeverCompletesIsBoundedByTheController() async throws {
        let connector = FakeConnector([.hang])
        let (controller, sleeper, sink) = Self.make(connector, connectTimeout: .milliseconds(100))
        controller.start()
        try await eventually("timed out") {
            controller.state == .protocolError(
                "connecting to the hostler service timed out (\(Self.location.socketPath))")
        }
        try await eventually("backoff") { sleeper.waitingCount == 1 }
        #expect(sleeper.durations == [.milliseconds(500)])
        #expect(connector.requestedTimeouts == [.milliseconds(100)])
        #expect(sink.calls.isEmpty)
        await controller.stop()
    }

    @Test func retryNowAbandonsAHangingConnectAndStartsAnother() async throws {
        let live = FakeSocketStream(respond: FakeService.standard())
        let connector = FakeConnector([.hang, .stream(live)])
        let (controller, sleeper, sink) = Self.make(connector, connectTimeout: .seconds(60))
        controller.start()
        try await eventually("hanging") { connector.hangingCount == 1 }
        controller.retryNow()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        #expect(connector.hangingCount == 0)
        #expect(sleeper.durations.isEmpty)
        // The abandoned attempt was connection 1.
        #expect(sink.connections == [.connected(instance: "run-1", connection: 2)])
        await controller.stop()
    }

    @Test func stopEndsAHangingConnectAtOnce() async throws {
        let connector = FakeConnector([.hang])
        let (controller, _, sink) = Self.make(connector, connectTimeout: .seconds(60))
        controller.start()
        try await eventually("hanging") { connector.hangingCount == 1 }
        let clock = ContinuousClock()
        let start = clock.now
        await controller.stop()
        #expect(clock.now - start < .seconds(2))
        #expect(connector.hangingCount == 0)
        #expect(connector.connectCount == 1)
        #expect(sink.calls.isEmpty)
    }

    // MARK: - Reconnects and tokens

    @Test func aDropBacksOffAndGoesLiveAgainWithTheNextToken() async throws {
        let first = FakeSocketStream(respond: FakeService.standard(instance: "run-1"))
        let second = FakeSocketStream(respond: FakeService.standard(instance: "run-2"))
        let (controller, sleeper, sink) = Self.make(
            FakeConnector([.stream(first), .stream(second)]))
        controller.start()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        #expect(first.methods == ["hello"])
        #expect(controller.currentConnection == 1)
        #expect(sink.connections == [.connected(instance: "run-1", connection: 1)])

        first.drop()
        try await eventually("connecting") { controller.state == .connecting }
        #expect(controller.currentConnection == nil)
        try await eventually("backoff") { sleeper.waitingCount == 1 }
        #expect(sleeper.durations == [.milliseconds(500)])
        sleeper.releaseWaiting()
        try await eventually("live again") { controller.helloReply?.instance == "run-2" }
        #expect(controller.state == .live(serviceVersion: "0.0.4"))
        #expect(controller.currentConnection == 2)
        #expect(sink.connections == [.connected(instance: "run-1", connection: 1), .dropped(1),
                                     .connected(instance: "run-2", connection: 2)])
        await controller.stop()
        #expect(sink.connections.last == .dropped(2))
    }

    @Test func aFailedAttemptStillUsesItsNumber() async throws {
        let live = FakeSocketStream(respond: FakeService.standard())
        let (controller, sleeper, sink) = Self.make(
            FakeConnector([.fail(.notRunning(path: "/s")), .fail(.notRunning(path: "/s")),
                           .stream(live)]))
        sleeper.releaseAll()
        controller.start()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        #expect(sink.connections == [.connected(instance: "run-1", connection: 3)])
        await controller.stop()
    }

    @Test func theBackoffIsHalfOneTwoFiveTenTwentyThenThirtySeconds() async throws {
        let (controller, sleeper, _) = Self.make(FakeConnector([.fail(.notRunning(path: "/s"))]))
        controller.start()
        for count in 1...8 {
            try await eventually("sleep \(count)") { sleeper.durations.count == count }
            sleeper.releaseWaiting()
        }
        #expect(sleeper.durations == [.milliseconds(500), .seconds(1), .seconds(2), .seconds(5),
                                      .seconds(10), .seconds(20), .seconds(30), .seconds(30)])
        await controller.stop()
    }

    @Test func aLiveConnectionResetsTheBackoff() async throws {
        let live = FakeSocketStream(respond: FakeService.standard())
        let connector = FakeConnector([.fail(.notRunning(path: "/s")), .fail(.notRunning(path: "/s")),
                                       .stream(live), .fail(.notRunning(path: "/s"))])
        let (controller, sleeper, _) = Self.make(connector)
        controller.start()
        for count in 1...2 {
            try await eventually("sleep \(count)") { sleeper.waitingCount == 1 }
            sleeper.releaseWaiting()
        }
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        live.drop()
        try await eventually("backoff after the drop") { sleeper.waitingCount == 1 }
        #expect(sleeper.durations == [.milliseconds(500), .seconds(1), .milliseconds(500)])
        await controller.stop()
    }

    @Test func retryNowEndsTheWaitAtOnce() async throws {
        let live = FakeSocketStream(respond: FakeService.standard())
        let connector = FakeConnector([.fail(.notRunning(path: "/s")), .stream(live)])
        let (controller, sleeper, _) = Self.make(connector)
        controller.start()
        try await eventually("waiting") { sleeper.waitingCount == 1 }
        controller.retryNow()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        #expect(sleeper.durations == [.milliseconds(500)])
        await controller.stop()
    }

    // MARK: - Events

    @Test func everyEventFrameReachesTheSinkInOrderWithItsTokenAndSize() async throws {
        let stream = FakeSocketStream(respond: FakeService.standard())
        let (controller, _, sink) = Self.make(FakeConnector([.stream(stream)]))
        controller.start()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        let lines = try ["event.queue.changed.json", "event.resync.json",
                         "event.devices.installed.json"].map(Self.line)
        for line in lines { stream.push(line) }
        try await eventually("three events") { sink.events.count == 3 }
        let seqs = try ["event.queue.changed.json", "event.resync.json",
                        "event.devices.installed.json"].map { name -> UInt64 in
            guard case .int(let n)? = try ContractFixtures.json(name)["seq"] else { return 0 }
            return UInt64(n)
        }
        #expect(sink.calls == [.connected(instance: "run-1", connection: 1)]
            + zip(seqs, lines).map { .receive(seq: $0, connection: 1, frameBytes: $1.utf8.count) })
        if case .queueChanged = sink.events[0] {} else { Issue.record("first is \(sink.events[0])") }
        #expect(sink.events[1] == .resync(reason: .overflow))
        if case .devicesInstalled = sink.events[2] {} else { Issue.record("third is \(sink.events[2])") }
        await controller.stop()
    }

    /// A replaced connection's frames never reach the sink: an undecodable event ends
    /// connection 1, and a valid frame read after it on that connection is dropped.
    @Test func framesOfAReplacedConnectionNeverReachTheSink() async throws {
        let first = FakeSocketStream(respond: FakeService.standard(instance: "run-1"))
        let second = FakeSocketStream(respond: FakeService.standard(instance: "run-1"))
        let (controller, sleeper, sink) = Self.make(FakeConnector([.stream(first), .stream(second)]))
        sleeper.releaseAll()
        controller.start()
        try await eventually("live") { controller.currentConnection == 1 }
        let valid = try Self.line("event.queue.changed.json")
        first.push(#"{"event":{"type":"no.such.event","data":{}},"seq":99}"#)
        first.push(valid)
        try await eventually("connection 2") { controller.currentConnection == 2 }
        #expect(first.isClosed)
        second.push(valid)
        try await eventually("the event of 2") { sink.events.count == 1 }
        #expect(sink.calls == [.connected(instance: "run-1", connection: 1), .dropped(1),
                               .connected(instance: "run-1", connection: 2),
                               .receive(seq: try Self.seq("event.queue.changed.json"),
                                        connection: 2, frameBytes: valid.utf8.count)])
        await controller.stop()
    }

    // MARK: - The grant (R1-5)

    @Test func theGrantIsKeptFromProveAndDroppedOnEveryLaterTransition() async throws {
        let failProve = ProveSwitch()
        let first = FakeSocketStream { frame in
            let id = FakePeer.text(frame["id"])
            if frame["method"] == .string("secrets.prove") {
                if failProve.on {
                    return ["{\"id\":\(id),\"error\":{\"code\":\"unlock_failed\",\"message\":\"wrong\",\"details\":{},\"recovery\":null}}"]
                }
                return ["{\"id\":\(id),\"result\":{\"expires_at_ms\":5000}}"]
            }
            if frame["method"] == .string("secrets.unlock") {
                return ["""
                    {"id":\(id),"result":{"status":{"state":"unlocked","database":"/d","key_file":null,"pending_requests":0,"preserved_files":[],"as_of_seq":20},"grant":{"expires_at_ms":7000}}}
                    """]
            }
            return FakeService.standard()(frame)
        }
        let second = FakeSocketStream(respond: FakeService.standard())
        let (controller, sleeper, _) = Self.make(FakeConnector([.stream(first), .stream(second)]))
        sleeper.releaseAll()
        controller.start()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        let client = controller.client
        #expect(controller.grant == nil)

        // A prove's grant; a later transition to unlocked voids it (generation moved).
        first.push(Self.lockChanged("unlocked", seq: 5))
        _ = try await client.secretsProve(Prove(password: SecretText("pw")))
        try await eventually("grant") { controller.grant == ProofGrant(expiresAtMs: 5000) }
        first.push(Self.lockChanged("unlocked", seq: 6))
        try await eventually("an unlock transition drops it") { controller.grant == nil }

        // A lock transition voids a new one too.
        _ = try await client.secretsProve(Prove(password: SecretText("pw")))
        try await eventually("grant again") { controller.grant != nil }
        first.push(Self.lockChanged("locked", seq: 7))
        try await eventually("a lock drops it") { controller.grant == nil }

        // An unlock's grant survives its own transition (seq ≤ its as_of_seq), not a later one.
        _ = try await client.secretsUnlock(Unlock(password: SecretText("pw"), forgetKeyFile: false,
                                                  create: false))
        try await eventually("unlock grant") { controller.grant == ProofGrant(expiresAtMs: 7000) }
        first.push(Self.lockChanged("unlocked", seq: 20))
        first.push(try Self.line("event.queue.changed.json"))
        try await Task.sleep(for: .milliseconds(100))
        #expect(controller.grant == ProofGrant(expiresAtMs: 7000))
        first.push(Self.lockChanged("locked", seq: 21))
        try await eventually("a later lock drops it") { controller.grant == nil }

        // A failed prove voids it.
        _ = try await client.secretsProve(Prove(password: SecretText("pw")))
        try await eventually("grant before the failure") { controller.grant != nil }
        failProve.on = true
        await #expect(throws: HostlerCallError.self) {
            _ = try await client.secretsProve(Prove(password: SecretText("wrong")))
        }
        try await eventually("a failed prove drops it") { controller.grant == nil }

        // A reconnect drops it.
        failProve.on = false
        _ = try await client.secretsProve(Prove(password: SecretText("pw")))
        try await eventually("grant before the drop") { controller.grant != nil }
        first.drop()
        try await eventually("reconnected") { controller.currentConnection == 2 }
        #expect(controller.grant == nil)
        await controller.stop()
    }

    // MARK: - Start and stop

    @Test func noStateTransitionEverStartsTheService() async throws {
        let live = FakeSocketStream(respond: FakeService.standard())
        let connector = FakeConnector([.fail(.notRunning(path: "/s")), .stream(live),
                                       .fail(.notRunning(path: "/s"))])
        let starter = FakeStarter()
        let (controller, sleeper, _) = Self.make(connector, registration: Self.registered)
        controller.start()
        try await eventually("not running") {
            controller.state == .notRunning(offer: .start(executable: "/opt/hostler"))
        }
        sleeper.releaseWaiting()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        live.drop()
        try await eventually("connecting") { controller.state == .connecting }
        sleeper.releaseWaiting()
        try await eventually("not running again") {
            controller.state == .notRunning(offer: .start(executable: "/opt/hostler"))
        }
        #expect(starter.calls.isEmpty)
        await controller.stop()
    }

    @Test func startServiceRunsOnlyTheCurrentStartOffer() async throws {
        let starter = FakeStarter()
        let (live, _, _) = Self.make(FakeConnector([.stream(FakeSocketStream(respond: FakeService.standard()))]))
        live.start()
        try await eventually("live") { live.state == .live(serviceVersion: "0.0.4") }
        #expect(failure(await live.startService(with: starter))
            == .nothingToStart(command: "hostler service install"))
        #expect(starter.calls.isEmpty)
        await live.stop()

        let afterStart = FakeSocketStream(respond: FakeService.standard())
        let connector = FakeConnector([.fail(.notRunning(path: "/s")), .stream(afterStart)])
        let (down, sleeper, _) = Self.make(connector, registration: Self.registered)
        down.start()
        try await eventually("not running") {
            down.state == .notRunning(offer: .start(executable: "/opt/hostler"))
        }
        try await eventually("waiting") { sleeper.waitingCount == 1 }
        #expect(failure(await down.startService(with: starter)) == nil)
        #expect(starter.calls == [.start(executable: "/opt/hostler")])
        try await eventually("live") { down.state == .live(serviceVersion: "0.0.4") }
        await down.stop()
    }

    @Test func stopClosesTheConnectionDropsItOnceAndConnectsNoMore() async throws {
        let stream = FakeSocketStream(respond: FakeService.standard())
        let connector = FakeConnector([.stream(stream)])
        let (controller, _, sink) = Self.make(connector)
        controller.start()
        try await eventually("live") { controller.state == .live(serviceVersion: "0.0.4") }
        await controller.stop()
        #expect(stream.isClosed)
        #expect(sink.connections == [.connected(instance: "run-1", connection: 1), .dropped(1)])
        try await Task.sleep(for: .milliseconds(100))
        #expect(connector.connectCount == 1)
        #expect(sink.connections.count == 2)
        await #expect(throws: HostlerCallError.notConnected) {
            _ = try await controller.client.serviceStatus()
        }
    }
}

/// Makes the fake service fail a prove.
final class ProveSwitch: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    var on: Bool {
        get { lock.withLock { value } }
        set { lock.withLock { value = newValue } }
    }
}
