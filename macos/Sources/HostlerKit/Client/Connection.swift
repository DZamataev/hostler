import Foundation

/// One event as the service numbered it (spec §6.2): `seq` increases by one per event within
/// one service run.
public struct SeqEvent: Sendable, Equatable {
    public let seq: UInt64
    public let event: Event
    /// The frame's length on the wire, without its newline (the store's hold counts it).
    public let frameBytes: Int

    public init(seq: UInt64, event: Event, frameBytes: Int) {
        self.seq = seq
        self.event = event
        self.frameBytes = frameBytes
    }
}

/// What a connection reports besides answers, in arrival order.
public enum ConnectionIncoming: Sendable, Equatable {
    /// An event frame.
    case event(SeqEvent)
    /// An event frame that did not decode: something was not delivered, so the reader resyncs.
    case unreadableEvent
    /// A successful `secrets.prove` or `secrets.unlock` on this connection gave this grant.
    /// `asOfSeq`: for an unlock, its status's `as_of_seq` (the lock state the grant belongs to,
    /// its own `secrets.lock_changed` included); `nil` for a prove.
    case granted(ProofGrant, asOfSeq: UInt64?)
    /// A `secrets.prove` or `secrets.unlock` failed (or its outcome is unknown): the service
    /// voids the connection's grant on a failed attempt (spec §8.5).
    case grantVoided
    /// The connection ended: the peer closed it (`nil`) or it failed.
    case closed(SocketError?)
}

/// The wire to one service connection (spec §6.2): newline-delimited JSON frames, ids from 1,
/// answers matched by id, events streamed to `incoming`.
///
/// Values: the answer to a method with `sensitiveAnswer` is routed by its keys alone and copied
/// raw into a `SecretText`; it never becomes a `JSONValue`, a `String` or a log line. No frame
/// text is ever logged: log lines carry the method and byte counts only.
public actor HostlerConnection: HostlerTransport {
    private let stream: any SocketStream
    private var nextId: UInt64 = 1
    private var pending: [UInt64: Pending] = [:]
    /// Ids whose answers are read raw. An id stays here after a timeout, so a late answer is
    /// still routed raw (and dropped), never parsed.
    private var sensitiveIds: Set<UInt64> = []
    /// Sensitive calls that ended without their answer (timed out, cancelled): their ids stay in
    /// `sensitiveIds`, so a late answer is still routed raw and dropped unread (clean read C-B3).
    private var abandonedSensitive: Set<UInt64> = []
    /// At this many, the connection closes instead of keeping more: the next connection starts
    /// with none, and raw routing is never given up for an id that may still be answered.
    public static let maxAbandonedSensitive = 16
    static let tooManyAbandoned = "too many values were asked for without an answer; reconnecting"
    /// `sensitiveIds`' size (tests).
    var sensitiveIdCount: Int { sensitiveIds.count }
    private var isClosed = false
    /// Ids whose frame is being written. A call cancelled or timed out while its write is
    /// still blocked closes the connection, so no writer stays stuck holding its frame.
    private var writing: Set<UInt64> = []
    private var reader: Task<Void, Never>?
    private var started = false

    /// Events, grants and the end of the connection, in order. Finishes after `.closed`.
    public nonisolated let incoming: AsyncStream<ConnectionIncoming>
    private let incomingContinuation: AsyncStream<ConnectionIncoming>.Continuation

    /// The `hello` reply, once `hello` has succeeded.
    public private(set) var helloReply: HelloReply?

    /// The fixed text of an answer the reader could not read. Never the frame.
    public static let unreadableAnswer = String(localized:
        "an answer from the hostler service could not be read")

    private enum Reply: Sendable {
        case data(Data)
        case sensitive(SecretText)
        case failure(HostlerCallError)
    }

    private struct Pending {
        let method: HostlerMethod
        let continuation: CheckedContinuation<Reply, Never>
        var timer: Task<Void, Never>?
    }

    public init(stream: any SocketStream) {
        self.stream = stream
        (incoming, incomingContinuation) = AsyncStream<ConnectionIncoming>.makeStream()
    }

    /// Starts reading. Call once, before the first call.
    public func start() {
        guard !started else { return }
        started = true
        let lines = stream.lines()
        reader = Task { [weak self] in
            do {
                for try await line in lines {
                    guard let self else { return }
                    await self.handle(line)
                }
                await self?.ended(nil)
            } catch let error as SocketError {
                await self?.ended(error)
            } catch {
                await self?.ended(.failed("reading from the service failed"))
            }
        }
    }

    /// `hello` with this contract's version; it records the reply (`boot_id`, `instance`).
    public func hello(client: String) async throws(HostlerCallError) -> HelloReply {
        let reply = try await HostlerClient(transport: self)
            .hello(contractVersion: Contract.version, client: client)
        helloReply = reply
        return reply
    }

    /// Closes the connection: every pending call fails with `notConnected`.
    public func close() {
        stream.close()
        ended(nil)
    }

    // MARK: - HostlerTransport

    /// An ordinary call. A method whose answer holds values is refused here, before anything
    /// is sent: it goes through `callSensitive` only (as the CLI's `Client::call` refuses it).
    public func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        let method = request.method
        guard !method.spec.sensitiveAnswer else { throw .wrongPath(method) }
        switch await send(request, timeout: timeout) {
        case .data(let data): return data
        case .failure(let error): throw error
        case .sensitive: throw .transport(Self.unreadableAnswer)
        }
    }

    /// A call whose answer holds values (`MethodSpec.sensitiveAnswer`); any other method is
    /// refused before anything is sent.
    public func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        let method = request.method
        guard method.spec.sensitiveAnswer else { throw .wrongPath(method) }
        switch await send(request, timeout: timeout) {
        case .sensitive(let bytes): return SensitiveData(bytes)
        case .failure(let error): throw error
        case .data: throw .transport(Self.unreadableAnswer)
        }
    }

    // MARK: - Sending

    /// The client frame: `{"id": …}` plus the request's `method` and `params`.
    private struct OutgoingFrame: Encodable {
        let id: UInt64
        let request: Request

        private enum Keys: String, CodingKey { case id }

        func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: Keys.self)
            try container.encode(id, forKey: .id)
            try request.encode(to: encoder)
        }
    }

    /// Holds an encoded frame in one place, so the writer can erase it after sending (a frame
    /// of `secrets.unlock` or `secret.set` carries a password or a value).
    private final class FrameBuffer: @unchecked Sendable {
        var data: Data
        init(_ data: Data) { self.data = data }
        func erase() { data.resetBytes(in: 0..<data.count) }
    }

    /// Sends `request` and waits for its answer, its bound or the caller's cancellation. The
    /// routing (raw or not) follows the method's own spec, never the caller.
    private func send(_ request: Request, timeout: Duration) async -> Reply {
        let method = request.method
        if isClosed { return .failure(.notConnected) }
        if Task.isCancelled { return .failure(.cancelled(method)) }
        // The id is taken only once the frame is known to fit (no await in between).
        let id = nextId
        let frame: FrameBuffer
        do {
            frame = FrameBuffer(try JSONEncoder().encode(OutgoingFrame(id: id, request: request)))
        } catch {
            return .failure(.transport("the request \(method.rawValue) could not be encoded"))
        }
        // The service closes a connection on a longer line, failing every other call on it.
        guard frame.data.count <= Contract.maxFrameBytes else {
            Log.client.error("\(method.rawValue, privacy: .public) not sent: \(frame.data.count) bytes")
            frame.erase()
            return .failure(.tooLarge(method))
        }
        nextId += 1
        frame.data.append(UInt8(ascii: "\n"))
        if method.spec.sensitiveAnswer { sensitiveIds.insert(id) }
        Log.client.debug(
            "call \(method.rawValue, privacy: .public) id \(id) \(frame.data.count) bytes")
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Reply, Never>) in
                let timer = Task { [weak self] in
                    do { try await Task.sleep(for: timeout) } catch { return }
                    await self?.expire(id)
                }
                pending[id] = Pending(method: method, continuation: continuation, timer: timer)
                writing.insert(id)
                let stream = self.stream
                Task { [weak self] in
                    do {
                        try await stream.write(frame.data)
                        frame.erase()
                        await self?.written(id)
                    } catch {
                        frame.erase()
                        await self?.writeFailed(id)
                    }
                }
            }
        } onCancel: {
            Task { [weak self] in await self?.cancelCall(id) }
        }
    }

    private func written(_ id: UInt64) {
        writing.remove(id)
    }

    /// The caller went away: its continuation resumes with `.cancelled`, its timer stops. A
    /// late answer is ignored (a sensitive one still routed raw, by its kept id).
    private func cancelCall(_ id: UInt64) {
        guard let entry = pending[id] else { return }
        Log.client.info("\(entry.method.rawValue, privacy: .public) id \(id) cancelled")
        resolve(id, .failure(.cancelled(entry.method)))
        abandonWrite(id)
        abandonSensitive(id)
    }

    private func expire(_ id: UInt64) {
        guard let entry = pending[id] else { return }
        Log.client.info("no answer to \(entry.method.rawValue, privacy: .public) id \(id) in time")
        resolve(id, .failure(.timedOut(entry.method)))
        abandonWrite(id)
        abandonSensitive(id)
    }

    /// A sensitive call ended without its answer: its id is kept for raw routing, and at
    /// `maxAbandonedSensitive` the connection is recycled rather than keeping more.
    private func abandonSensitive(_ id: UInt64) {
        guard !isClosed, sensitiveIds.contains(id) else { return }
        abandonedSensitive.insert(id)
        guard abandonedSensitive.count >= Self.maxAbandonedSensitive else { return }
        Log.client.error("too many unanswered sensitive calls; closing the connection")
        sensitiveIds = []
        abandonedSensitive = []
        stream.close()
        ended(.failed(Self.tooManyAbandoned))
    }

    /// A call that ended while its frame is still being written: the write is blocked (a
    /// service that does not read), so the connection closes and the writer fails and erases
    /// its frame.
    private func abandonWrite(_ id: UInt64) {
        guard writing.contains(id) else { return }
        Log.client.error("a write did not finish; closing the connection")
        stream.close()
        ended(.failed("a write to the service did not finish"))
    }

    private func writeFailed(_ id: UInt64) {
        writing.remove(id)
        guard pending[id] != nil else { return }
        resolve(id, .failure(.notConnected))
    }

    private func finish(_ id: UInt64, _ reply: Reply) {
        // A late answer after a timeout or a cancel: ignored.
        guard pending[id] != nil else { return }
        resolve(id, reply)
    }

    /// Resumes the call `id` once. A prove or unlock that did not succeed voids the grant.
    private func resolve(_ id: UInt64, _ reply: Reply) {
        guard let entry = pending.removeValue(forKey: id) else { return }
        entry.timer?.cancel()
        if case .failure = reply, entry.method == .secretsProve || entry.method == .secretsUnlock {
            incomingContinuation.yield(.grantVoided)
        }
        entry.continuation.resume(returning: reply)
    }

    // MARK: - Reading

    private struct EventFrame: Decodable {
        let event: Event
        let seq: UInt64
    }

    private func handle(_ received: Data) {
        var line = received
        // After a sensitive call any line may carry values: erased once routed.
        defer { line.resetBytes(in: 0..<line.count) }
        // A closed connection reads nothing more: no line after it is routed, raw or not.
        if isClosed { return }
        let byteCount = line.count
        let routed: Bool = line.withUnsafeBytes { raw in
            guard let head = FrameScanner.head(raw) else { return false }
            let id = head.id(in: raw)
            if let id, sensitiveIds.contains(id) {
                sensitiveIds.remove(id)
                abandonedSensitive.remove(id)
                routeSensitive(id: id, head: head, raw: raw)
                return true
            }
            if let id, head.has("result") || head.has("error") {
                routeAnswer(id: id, head: head, raw: raw)
                return true
            }
            if !head.has("id"), head.has("event"), !head.has("result"), !head.has("error") {
                routeEvent(raw: raw)
                return true
            }
            return false
        }
        if !routed {
            // An unreadable line: the connection's framing can no longer be trusted, so it
            // closes (as the CLI's reader does); every pending call gets the fixed text.
            Log.client.error("an unreadable frame from the service (\(byteCount) bytes)")
            failAll(.transport(Self.unreadableAnswer))
            stream.close()
            ended(.protocolError(Self.unreadableAnswer))
        }
    }

    private func routeSensitive(id: UInt64, head: FrameHead, raw: UnsafeRawBufferPointer) {
        guard pending[id] != nil else { return }  // late: dropped unread
        if let range = head.members["error"] {
            finish(id, serviceError(raw, range))
        } else if let range = head.members["result"] {
            let bytes = SecretText(copying: UnsafeRawBufferPointer(rebasing: raw[range]))
            Log.client.debug("sensitive answer id \(id) \(range.count) bytes")
            finish(id, .sensitive(bytes))
        } else {
            finish(id, .failure(.transport(Self.unreadableAnswer)))
        }
    }

    private func routeAnswer(id: UInt64, head: FrameHead, raw: UnsafeRawBufferPointer) {
        guard let method = pending[id]?.method else { return }  // late: ignored
        if let range = head.members["error"], !isNull(raw, range) {
            finish(id, serviceError(raw, range))
            return
        }
        guard let range = head.members["result"] else {
            finish(id, .failure(.transport(Self.unreadableAnswer)))
            return
        }
        let data = Data(UnsafeRawBufferPointer(rebasing: raw[range]))
        Log.client.debug("answer \(method.rawValue, privacy: .public) id \(id) \(data.count) bytes")
        if let (grant, asOfSeq) = Self.grant(of: method, in: data) {
            incomingContinuation.yield(.granted(grant, asOfSeq: asOfSeq))
        }
        finish(id, .data(data))
    }

    private func routeEvent(raw: UnsafeRawBufferPointer) {
        let data = Data(raw)
        do {
            let frame = try JSONDecoder().decode(EventFrame.self, from: data)
            incomingContinuation.yield(.event(SeqEvent(seq: frame.seq, event: frame.event,
                                                       frameBytes: data.count)))
        } catch {
            Log.client.error("an event frame did not decode (\(data.count) bytes)")
            incomingContinuation.yield(.unreadableEvent)
        }
    }

    private func serviceError(_ raw: UnsafeRawBufferPointer, _ range: Range<Int>) -> Reply {
        let data = Data(UnsafeRawBufferPointer(rebasing: raw[range]))
        guard let error = try? JSONDecoder().decode(HostlerError.self, from: data) else {
            return .failure(.transport(Self.unreadableAnswer))
        }
        return .failure(.service(error))
    }

    private func isNull(_ raw: UnsafeRawBufferPointer, _ range: Range<Int>) -> Bool {
        range.count == 4 && Array(raw[range]) == Array("null".utf8)
    }

    /// The grant a successful prove or unlock answers.
    static func grant(of method: HostlerMethod, in data: Data) -> (ProofGrant, UInt64?)? {
        switch method {
        case .secretsProve:
            return (try? JSONDecoder().decode(ProofGrant.self, from: data)).map { ($0, nil) }
        case .secretsUnlock:
            guard let outcome = try? JSONDecoder().decode(UnlockOutcome.self, from: data)
            else { return nil }
            return (outcome.grant, outcome.status.asOfSeq)
        default:
            return nil
        }
    }

    private func failAll(_ error: HostlerCallError) {
        for id in Array(pending.keys) {
            resolve(id, .failure(error))
        }
    }

    private func ended(_ reason: SocketError?) {
        guard !isClosed else { return }
        isClosed = true
        failAll(.notConnected)
        incomingContinuation.yield(.closed(reason))
        incomingContinuation.finish()
    }
}
