import Foundation
import OSLog
import Testing
@testable import HostlerKit

/// Reads an `AsyncStream` one element at a time from a test.
final class StreamReader<Element: Sendable>: @unchecked Sendable {
    private let lines: LineQueue<Element>

    init(_ stream: AsyncStream<Element>) {
        lines = LineQueue()
        let lines = self.lines
        Task {
            for await item in stream { lines.push(item) }
            lines.finish()
        }
    }

    /// The next element (`nil` at the end), at most 10 s away.
    func next(_ what: String = "the next element") async throws -> Element? {
        let lines = self.lines
        return try await withDeadline(what) { await lines.pop() }
    }
}

private final class LineQueue<Element: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var ready: [Element?] = []
    private var waiting: CheckedContinuation<Element?, Never>?
    private var finished = false

    func push(_ item: Element?) {
        let taken = lock.withLock { () -> CheckedContinuation<Element?, Never>? in
            if let waiting {
                self.waiting = nil
                return waiting
            }
            ready.append(item)
            return nil
        }
        taken?.resume(returning: item)
    }

    func finish() {
        lock.withLock { finished = true }
        push(nil)
    }

    func pop() async -> Element? {
        await withCheckedContinuation { continuation in
            let item = lock.withLock { () -> Element?? in
                if !ready.isEmpty { return .some(ready.removeFirst()) }
                if finished { return .some(nil) }
                waiting = continuation
                return .none
            }
            if let item { continuation.resume(returning: item) }
        }
    }
}

/// A connection over a real socket to the scripted fake server.
struct Wire {
    let server: FakeSocketServer
    let connection: HostlerConnection
    let peer: FakePeer
    let incoming: StreamReader<ConnectionIncoming>

    static func open() async throws -> Wire {
        let server = try FakeSocketServer()
        let stream = try await POSIXSocketConnector().connect(path: server.path, timeout: .seconds(5))
        let connection = HostlerConnection(stream: stream)
        await connection.start()
        let peer = try await server.nextPeer()
        return Wire(server: server, connection: connection, peer: peer,
                    incoming: StreamReader(connection.incoming))
    }

    var client: HostlerClient { HostlerClient(transport: connection) }

    func close() async {
        await connection.close()
        peer.close()
        server.stop()
    }
}

let statusJSON = """
    {"service_version":"0.0.4","contract_version":5,"pid":7,"running_heavy":0,"waiting_heavy":0}
    """

@Suite(.timeLimit(.minutes(1))) struct ConnectionTests {
    @Test func helloIsFirstAndIdsIncreaseFromOne() async throws {
        let wire = try await Wire.open()
        let connection = wire.connection
        let hello = Task { try await connection.hello(client: "hostler-app 0.0.4") }
        let frame = try await wire.peer.nextFrame()
        #expect(frame == .object(["id": .int(1), "method": .string("hello"),
                                  "params": .object(["contract_version": .int(5),
                                                     "client": .string("hostler-app 0.0.4")])]))
        try await wire.peer.answer(frame["id"], result: """
            {"contract_version":5,"service_version":"0.0.4","boot_id":"b1","instance":"i1"}
            """)
        let reply = try await withDeadline("hello") { try await hello.value }
        #expect(reply.bootId == "b1")
        #expect(reply.instance == "i1")
        #expect(await connection.helloReply == reply)

        let client = wire.client
        let status = Task { try await client.serviceStatus() }
        let second = try await wire.peer.nextFrame()
        #expect(second["id"] == .int(2))
        #expect(second["method"] == .string("service.status"))
        #expect(second["params"] == nil)
        try await wire.peer.answer(second["id"], result: statusJSON)
        #expect(try await withDeadline("status") { try await status.value }.pid == 7)
        await wire.close()
    }

    @Test func concurrentCallsAreAnsweredOutOfOrder() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let status = Task { try await client.serviceStatus() }
        let first = try await wire.peer.nextFrame()
        let projects = Task { try await client.projectList() }
        let second = try await wire.peer.nextFrame()
        #expect(first["method"] == .string("service.status"))
        #expect(second["method"] == .string("project.list"))
        try await wire.peer.answer(second["id"], result: "[]")
        try await wire.peer.answer(first["id"], result: statusJSON)
        #expect(try await withDeadline("projects") { try await projects.value } == [])
        #expect(try await withDeadline("status") { try await status.value }.pid == 7)
        await wire.close()
    }

    @Test func anEventBetweenACallAndItsAnswerReachesIncoming() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let status = Task { try await client.serviceStatus() }
        let frame = try await wire.peer.nextFrame()
        let line = #"{"event":{"type":"resync","data":{"reason":"lagged"}},"seq":9}"#
        try await wire.peer.send(line)
        try await wire.peer.answer(frame["id"], result: statusJSON)
        #expect(try await withDeadline("status") { try await status.value }.pid == 7)
        #expect(try await wire.incoming.next()
            == .event(SeqEvent(seq: 9, event: .resync(reason: .lagged), frameBytes: line.utf8.count)))
        await wire.close()
    }

    @Test func everyEventFixtureArrivesWithItsSeq() async throws {
        let wire = try await Wire.open()
        let names = try FileManager.default.contentsOfDirectory(atPath: ContractFixtures.directory.path)
            .filter { $0.hasPrefix("event.") }.sorted()
        #expect(names.count == 10)
        for name in names {
            let fixture = try ContractFixtures.json(name)
            let line = try fixture.encoded()
            try await wire.peer.sendRaw(line + Data("\n".utf8))
            let expected = SeqEvent(
                seq: { if case .int(let n)? = fixture["seq"] { UInt64(n) } else { 0 } }(),
                event: try JSONDecoder().decode(Event.self, from: fixture["event"]!.encoded()),
                frameBytes: line.count)
            #expect(try await wire.incoming.next(name) == .event(expected), "\(name)")
        }
        await wire.close()
    }

    @Test func anEventThatDoesNotDecodeIsReportedNotDropped() async throws {
        let wire = try await Wire.open()
        try await wire.peer.send(#"{"event":{"type":"no.such.event","data":{}},"seq":3}"#)
        #expect(try await wire.incoming.next() == .unreadableEvent)
        await wire.close()
    }

    @Test func aTimeoutLeavesALateAnswerIgnored() async throws {
        let wire = try await Wire.open()
        let connection = wire.connection
        let late = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await connection.call(.serviceStatus, timeout: .milliseconds(200))
                return nil
            } catch {
                return error
            }
        }
        let frame = try await wire.peer.nextFrame()
        #expect(try await withDeadline("the timeout") { await late.value } == .timedOut(.serviceStatus))
        try await wire.peer.answer(frame["id"], result: statusJSON)
        // The next call gets its own answer, not the late one.
        let client = wire.client
        let next = Task { try await client.projectList() }
        let nextFrame = try await wire.peer.nextFrame()
        #expect(nextFrame["id"] == .int(2))
        try await wire.peer.answer(nextFrame["id"], result: "[]")
        #expect(try await withDeadline("next") { try await next.value } == [])
        await wire.close()
    }

    @Test func aServiceErrorIsTheServicesError() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let call = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) { _ = try await client.secretsStatus(); return nil } catch { return error }
        }
        let frame = try await wire.peer.nextFrame()
        try await wire.peer.answerError(frame["id"], code: "secrets_locked", message: "locked",
                                        recovery: "hostler unlock")
        #expect(try await withDeadline("error") { await call.value } == .service(HostlerError(
            code: .secretsLocked, message: "locked", details: .object([:]), recovery: "hostler unlock")))
        await wire.close()
    }

    @Test func aProveOrUnlockAnswerReportsTheGrant() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let prove = Task { try await client.secretsProve(Prove(password: SecretText("pw"))) }
        let frame = try await wire.peer.nextFrame()
        #expect(frame["method"] == .string("secrets.prove"))
        try await wire.peer.answer(frame["id"], result: #"{"expires_at_ms":121000}"#)
        #expect(try await withDeadline("prove") { try await prove.value } == ProofGrant(expiresAtMs: 121000))
        #expect(try await wire.incoming.next() == .granted(ProofGrant(expiresAtMs: 121000), asOfSeq: nil))
        await wire.close()
    }

    @Test func aLineOverTheCapClosesTheConnection() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let call = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) { _ = try await client.serviceStatus(); return nil } catch { return error }
        }
        _ = try await wire.peer.nextFrame()
        let over = Data(repeating: UInt8(ascii: "z"), count: Contract.maxFrameBytes + 1)
        let peer = wire.peer
        Task { try? await peer.sendRaw(over) }
        #expect(try await withDeadline("the call fails") { await call.value } == .notConnected)
        #expect(try await wire.incoming.next() == .closed(.protocolError(
            "the service sent a frame longer than \(Contract.maxFrameBytes) bytes")))
        #expect(try await wire.incoming.next() == nil)
        await wire.close()
    }

    @Test func thePeerClosingFailsPendingCallsAndLaterOnes() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let call = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) { _ = try await client.serviceStatus(); return nil } catch { return error }
        }
        _ = try await wire.peer.nextFrame()
        wire.peer.close()
        #expect(try await withDeadline("the call fails") { await call.value } == .notConnected)
        #expect(try await wire.incoming.next() == .closed(nil))
        await #expect(throws: HostlerCallError.notConnected) {
            _ = try await client.serviceStatus()
        }
        await wire.close()
    }

    @Test func anUnlockAnswerReportsItsGrantWithItsStatusPoint() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let unlock = Task {
            try await client.secretsUnlock(Unlock(password: SecretText("pw"), forgetKeyFile: false,
                                                  create: false))
        }
        let frame = try await wire.peer.nextFrame()
        try await wire.peer.answer(frame["id"], result: """
            {"status":{"state":"unlocked","database":"/d","key_file":null,"pending_requests":0,"preserved_files":[],"as_of_seq":7},"grant":{"expires_at_ms":9}}
            """)
        _ = try await withDeadline("unlock") { try await unlock.value }
        #expect(try await wire.incoming.next() == .granted(ProofGrant(expiresAtMs: 9), asOfSeq: 7))
        await wire.close()
    }

    @Test func aFailedProveVoidsTheGrant() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let prove = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await client.secretsProve(Prove(password: SecretText("wrong")))
                return nil
            } catch {
                return error
            }
        }
        let frame = try await wire.peer.nextFrame()
        try await wire.peer.answerError(frame["id"], code: "unlock_failed", message: "wrong password")
        #expect(try await withDeadline("prove") { await prove.value }
            == .service(HostlerError(code: .unlockFailed, message: "wrong password",
                                     details: .object([:]), recovery: nil)))
        #expect(try await wire.incoming.next() == .grantVoided)
        await wire.close()
    }

    /// R1-1: the routing follows the method, never the caller: nothing is sent on the wrong
    /// path, and no id is used.
    @Test func aMethodOnTheWrongPathIsRefusedBeforeAnythingIsSent() async throws {
        let wire = try await Wire.open()
        let connection = wire.connection
        let project = ProjectRef(commonDir: "/w/.git", name: "w")
        await #expect(throws: HostlerCallError.wrongPath(.secretReveal)) {
            _ = try await connection.call(.secretReveal(project: project, name: "K"),
                                          timeout: .seconds(5))
        }
        await #expect(throws: HostlerCallError.wrongPath(.accountReveal)) {
            _ = try await connection.call(.accountReveal(project: project, name: "K"),
                                          timeout: .seconds(5))
        }
        await #expect(throws: HostlerCallError.wrongPath(.serviceStatus)) {
            _ = try await connection.callSensitive(.serviceStatus, timeout: .seconds(5))
        }
        for method in HostlerMethod.allCases where method.spec.sensitiveAnswer {
            #expect([HostlerMethod.secretMaterialize, .accountMaterialize, .secretReveal,
                      .accountReveal].contains(method))
        }
        // The next frame the peer sees is id 1: nothing went out before it.
        let client = wire.client
        let status = Task { try await client.serviceStatus() }
        let frame = try await wire.peer.nextFrame()
        #expect(frame["id"] == .int(1))
        #expect(frame["method"] == .string("service.status"))
        try await wire.peer.answer(frame["id"], result: statusJSON)
        _ = try await withDeadline("status") { try await status.value }
        await wire.close()
    }

    /// R1-6: a request over the frame limit is refused before an id is taken or a byte sent.
    @Test func aRequestOverTheFrameLimitIsNotSent() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let huge = SecretText(String(repeating: "v", count: Contract.maxFrameBytes))
        await #expect(throws: HostlerCallError.tooLarge(.secretSet)) {
            _ = try await client.secretSet(SecretSet(
                project: ProjectRef(commonDir: "/w/.git", name: "w"), name: "BIG", value: huge))
        }
        let status = Task { try await client.serviceStatus() }
        let frame = try await wire.peer.nextFrame()
        #expect(frame["id"] == .int(1))
        try await wire.peer.answer(frame["id"], result: statusJSON)
        _ = try await withDeadline("status") { try await status.value }
        await wire.close()
    }

    /// R1-2: a cancelled call resumes at once with `.cancelled`; its late answer is ignored and
    /// the connection stays usable.
    @Test func aCancelledCallEndsAtOnceAndItsLateAnswerIsIgnored() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let call = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) { _ = try await client.serviceStatus(); return nil }
            catch { return error }
        }
        let frame = try await wire.peer.nextFrame()
        call.cancel()
        #expect(try await withDeadline("the cancel") { await call.value } == .cancelled(.serviceStatus))
        try await wire.peer.answer(frame["id"], result: statusJSON)
        let next = Task { try await client.projectList() }
        let nextFrame = try await wire.peer.nextFrame()
        #expect(nextFrame["id"] == .int(2))
        try await wire.peer.answer(nextFrame["id"], result: "[]")
        #expect(try await withDeadline("next") { try await next.value } == [])
        await wire.close()
    }

    @Test func aCallCancelledBeforeItStartsSendsNothing() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let call = Task { () -> HostlerCallError? in
            while !Task.isCancelled { await Task.yield() }
            do throws(HostlerCallError) { _ = try await client.serviceStatus(); return nil }
            catch { return error }
        }
        call.cancel()
        #expect(try await withDeadline("the cancel") { await call.value } == .cancelled(.serviceStatus))
        let next = Task { try await client.projectList() }
        let nextFrame = try await wire.peer.nextFrame()
        #expect(nextFrame["id"] == .int(1))
        try await wire.peer.answer(nextFrame["id"], result: "[]")
        _ = try await withDeadline("next") { try await next.value }
        await wire.close()
    }

    /// R1-2: a sensitive request whose write never finishes: the cancel ends the call at once
    /// and closes the connection, so the writer fails (and erases its frame) instead of
    /// waiting for ever.
    @Test func aSensitiveRequestCancelledWhileItsWriteIsBlockedClosesTheConnection() async throws {
        let stream = BlockingWriteStream()
        let connection = HostlerConnection(stream: stream)
        await connection.start()
        let incoming = StreamReader(connection.incoming)
        let client = HostlerClient(transport: connection)
        let unlock = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await client.secretsUnlock(Unlock(password: SecretText("pw"),
                                                          forgetKeyFile: false, create: false))
                return nil
            } catch {
                return error
            }
        }
        try await withDeadline("the write starts") {
            while !stream.isWriting { try await Task.sleep(for: .milliseconds(10)) }
        }
        unlock.cancel()
        #expect(try await withDeadline("the cancel") { await unlock.value } == .cancelled(.secretsUnlock))
        try await withDeadline("the writer ends") {
            while !stream.writeEnded { try await Task.sleep(for: .milliseconds(10)) }
        }
        #expect(stream.isClosed)
        #expect(try await incoming.next() == .grantVoided)
        #expect(try await incoming.next() == .closed(.failed("a write to the service did not finish")))
    }

    @Test func aSensitiveAnswerCancelledBeforeItComesIsDroppedUnread() async throws {
        let marker = "MARKER-\(UUID().uuidString)"
        let wire = try await Wire.open()
        let client = wire.client
        let reveal = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await client.secretReveal(project: ProjectRef(commonDir: "/w/.git", name: "w"),
                                                  name: "K")
                return nil
            } catch {
                return error
            }
        }
        let frame = try await wire.peer.nextFrame()
        reveal.cancel()
        #expect(try await withDeadline("the cancel") { await reveal.value } == .cancelled(.secretReveal))
        try await wire.peer.answer(frame["id"], result: #"{"name":"K","value":"\#(marker)"}"#)
        let next = Task { try await client.projectList() }
        let nextFrame = try await wire.peer.nextFrame()
        try await wire.peer.answer(nextFrame["id"], result: "[]")
        #expect(try await withDeadline("next") { try await next.value } == [])
        await wire.close()
        #expect(try SensitiveAnswerTests.appLog().allSatisfy { !$0.contains(marker) })
    }
}

/// Values never leave the sensitive path (Review Focus 2).
@Suite(.timeLimit(.minutes(1)), .serialized) struct SensitiveAnswerTests {
    static let project = ProjectRef(commonDir: "/work/app/.git", name: "app")

    /// Every message this process logged under the app's subsystem in the last minute.
    static func appLog() throws -> [String] {
        let store = try OSLogStore(scope: .currentProcessIdentifier)
        let since = store.position(date: Date().addingTimeInterval(-60))
        return try store.getEntries(at: since)
            .compactMap { $0 as? OSLogEntryLog }
            .filter { $0.subsystem == Log.subsystem }
            .map(\.composedMessage)
    }

    @Test func aRevealAnswerIsReadRawIntoSecretText() async throws {
        let marker = "MARKER-\(UUID().uuidString)"
        let wire = try await Wire.open()
        let client = wire.client
        let reveal = Task { try await client.secretReveal(project: Self.project, name: "API_KEY") }
        let frame = try await wire.peer.nextFrame()
        #expect(frame["method"] == .string("secret.reveal"))
        try await wire.peer.answer(frame["id"], result: #"{"name":"API_KEY","value":"\#(marker)"}"#)
        let secret = try await withDeadline("reveal") { try await reveal.value }
        #expect(secret.name == "API_KEY")
        #expect(secret.value.reveal() == marker)
        #expect(!String(reflecting: secret).contains(marker))
        #expect(!String(describing: secret).contains(marker))
        await wire.close()
        #expect(try Self.appLog().allSatisfy { !$0.contains(marker) })
    }

    @Test func anUndecodableRevealAnswerIsAFixedError() async throws {
        let marker = "MARKER-\(UUID().uuidString)"
        let wire = try await Wire.open()
        let client = wire.client
        let reveal = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) { _ = try await client.secretReveal(project: Self.project, name: "K"); return nil }
            catch { return error }
        }
        let frame = try await wire.peer.nextFrame()
        try await wire.peer.answer(frame["id"], result: #"{"name":7,"value":"\#(marker)"}"#)
        let error = try #require(try await withDeadline("reveal") { await reveal.value })
        #expect(error == .undecodable(.secretReveal))
        #expect(!String(reflecting: error).contains(marker))
        #expect(!(error.errorDescription ?? "").contains(marker))
        await wire.close()
        #expect(try Self.appLog().allSatisfy { !$0.contains(marker) })
    }

    @Test func anUnreadableLineAfterASensitiveCallIsDroppedWithAFixedError() async throws {
        let marker = "MARKER-\(UUID().uuidString)"
        let wire = try await Wire.open()
        let client = wire.client
        let reveal = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) { _ = try await client.accountReveal(project: Self.project, name: "acct"); return nil }
            catch { return error }
        }
        let frame = try await wire.peer.nextFrame()
        try await wire.peer.send(#"{"id":\#(FakePeer.text(frame["id"])),"result":{"password":"\#(marker)""#)
        let error = try #require(try await withDeadline("reveal") { await reveal.value })
        #expect(error == .transport(HostlerConnection.unreadableAnswer))
        #expect(!String(reflecting: error).contains(marker))
        #expect(try await wire.incoming.next()
            == .closed(.protocolError(HostlerConnection.unreadableAnswer)))
        await wire.close()
        let log = try Self.appLog()
        // The capture works (the reader's own line is there) and holds no value.
        #expect(log.contains { $0.hasPrefix("an unreadable frame from the service (") })
        #expect(log.allSatisfy { !$0.contains(marker) })
    }

    @Test func aLateSensitiveAnswerIsDroppedUnread() async throws {
        let marker = "MARKER-\(UUID().uuidString)"
        let wire = try await Wire.open()
        let connection = wire.connection
        let reveal = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await connection.callSensitive(
                    .secretReveal(project: Self.project, name: "K"), timeout: .milliseconds(200))
                return nil
            } catch {
                return error
            }
        }
        let frame = try await wire.peer.nextFrame()
        #expect(try await withDeadline("timeout") { await reveal.value } == .timedOut(.secretReveal))
        // Late, and not the answer type: still routed raw by id, so never decoded or logged.
        try await wire.peer.answer(frame["id"], result: #""\#(marker)""#)
        let client = wire.client
        let next = Task { try await client.projectList() }
        let nextFrame = try await wire.peer.nextFrame()
        try await wire.peer.answer(nextFrame["id"], result: "[]")
        #expect(try await withDeadline("next") { try await next.value } == [])
        await wire.close()
        #expect(try Self.appLog().allSatisfy { !$0.contains(marker) })
    }

    /// Clean read C-B3: ids of sensitive calls that timed out stay routed raw, but never more
    /// than `maxAbandonedSensitive`: reaching it closes the connection (a reconnect starts with
    /// none), so a peer that never answers cannot grow the set without bound.
    @Test func repeatedSensitiveTimeoutsStayBoundedAndRecycleTheConnection() async throws {
        let wire = try await Wire.open()
        let connection = wire.connection
        let limit = HostlerConnection.maxAbandonedSensitive
        for round in 1...limit {
            let reveal = Task { () -> HostlerCallError? in
                do throws(HostlerCallError) {
                    _ = try await connection.callSensitive(
                        .secretReveal(project: Self.project, name: "K"), timeout: .milliseconds(50))
                    return nil
                } catch {
                    return error
                }
            }
            _ = try await wire.peer.nextFrame()
            #expect(try await withDeadline("timeout \(round)") { await reveal.value }
                == .timedOut(.secretReveal))
            #expect(await connection.sensitiveIdCount == (round < limit ? round : 0))
        }
        // The limit was reached: the connection closed, and the next call is not sent.
        #expect(try await wire.incoming.next() == .closed(.failed(HostlerConnection.tooManyAbandoned)))
        let after = try? await connection.callSensitive(
            .secretReveal(project: Self.project, name: "K"), timeout: .milliseconds(50))
        #expect(after == nil)
        await wire.close()
    }

    @Test func aLateAnswerBeforeTheLimitIsStillRoutedRawAndFreesItsPlace() async throws {
        let marker = "MARKER-\(UUID().uuidString)"
        let wire = try await Wire.open()
        let connection = wire.connection
        let reveal = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await connection.callSensitive(
                    .secretReveal(project: Self.project, name: "K"), timeout: .milliseconds(50))
                return nil
            } catch {
                return error
            }
        }
        let frame = try await wire.peer.nextFrame()
        #expect(try await withDeadline("timeout") { await reveal.value } == .timedOut(.secretReveal))
        #expect(await connection.sensitiveIdCount == 1)
        try await wire.peer.answer(frame["id"], result: #""\#(marker)""#)
        try await withDeadline("the late answer is consumed") {
            while await connection.sensitiveIdCount != 0 { try await Task.sleep(for: .milliseconds(2)) }
        }
        await wire.close()
        #expect(try Self.appLog().allSatisfy { !$0.contains(marker) })
    }

    @Test func aSensitiveRequestCarriesItsPasswordOnlyOnTheWire() async throws {
        let wire = try await Wire.open()
        let client = wire.client
        let unlock = Task { () -> HostlerCallError? in
            do throws(HostlerCallError) {
                _ = try await client.secretsUnlock(Unlock(password: SecretText("pw-on-the-wire"),
                                                          forgetKeyFile: false, create: false))
                return nil
            } catch {
                return error
            }
        }
        let frame = try await wire.peer.nextFrame()
        #expect(frame["params"]?["password"] == .string("pw-on-the-wire"))
        try await wire.peer.answerError(frame["id"], code: "unlock_failed", message: "wrong password")
        #expect(try await withDeadline("unlock") { await unlock.value }
            == .service(HostlerError(code: .unlockFailed, message: "wrong password",
                                     details: .object([:]), recovery: nil)))
        await wire.close()
        #expect(try Self.appLog().allSatisfy { !$0.contains("pw-on-the-wire") })
    }
}

@Suite struct FrameScannerTests {
    static func head(_ text: String) -> FrameHead? {
        Data(text.utf8).withUnsafeBytes { FrameScanner.head($0) }
    }

    static func id(_ text: String) -> UInt64? {
        Data(text.utf8).withUnsafeBytes { raw in FrameScanner.head(raw)?.id(in: raw) }
    }

    @Test func itFindsTheTopLevelKeysAndValueRanges() throws {
        let text = #"{"id":12,"result":{"a":[1,"}",{"b":"\"]"}]},"x":null}"#
        let head = try #require(Self.head(text))
        #expect(Set(head.members.keys) == ["id", "result", "x"])
        let bytes = Array(text.utf8)
        let result = try #require(head.members["result"])
        #expect(String(decoding: bytes[result], as: UTF8.self) == #"{"a":[1,"}",{"b":"\"]"}]}"#)
        #expect(Self.id(text) == 12)
    }

    @Test func malformedLinesAreNil() {
        for text in ["", "[]", "{", #"{"id":1"#, #"{"id":1,}"#, #"{"a":"unterminated}"#,
                     #"{"id":1} trailing"#, #"{"a" 1}"#, #"{"a":{"b":1}"#] {
            #expect(Self.head(text) == nil, "\(text)")
        }
    }

    @Test func onlyAPlainUnsignedIntegerIsAnId() {
        #expect(Self.id(#"{"id":"7"}"#) == nil)
        #expect(Self.id(#"{"id":-1}"#) == nil)
        #expect(Self.id(#"{"id":1.5}"#) == nil)
        #expect(Self.id(#"{"id":18446744073709551616}"#) == nil)
        #expect(Self.id(#"{"id":18446744073709551615}"#) == UInt64.max)
        #expect(Self.id(#"{ "id" : 3 , "error" : {} }"#) == 3)
    }

    @Test func everyFrameFixtureScans() throws {
        for name in ["frame.response.json", "frame.error.json", "frame.event.json"] {
            let data = try ContractFixtures.data(name)
            let head = data.withUnsafeBytes { FrameScanner.head($0) }
            #expect(head != nil, "\(name)")
        }
    }
}

/// A stream whose writes never finish until it is closed (a service that stopped reading).
final class BlockingWriteStream: SocketStream, @unchecked Sendable {
    private let lock = NSLock()
    private var closed = false
    private var writing = false
    private var ended = false
    private let incoming: AsyncThrowingStream<Data, Error>
    private let continuation: AsyncThrowingStream<Data, Error>.Continuation

    init() { (incoming, continuation) = AsyncThrowingStream<Data, Error>.makeStream() }

    var isClosed: Bool { lock.withLock { closed } }
    var isWriting: Bool { lock.withLock { writing } }
    var writeEnded: Bool { lock.withLock { ended } }

    func write(_ data: Data) async throws {
        lock.withLock { writing = true }
        defer { lock.withLock { ended = true } }
        while !isClosed { try await Task.sleep(for: .milliseconds(10)) }
        throw SocketError.failed("closed")
    }

    func lines() -> AsyncThrowingStream<Data, Error> { incoming }

    func close() {
        lock.withLock { closed = true }
        continuation.finish()
    }
}
