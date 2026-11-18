import Foundation
@testable import HostlerKit

/// An in-memory connection whose service side is a script: every frame the client writes is
/// recorded and answered by `respond`; the test pushes events and drops the connection.
final class FakeSocketStream: SocketStream, @unchecked Sendable {
    typealias Responder = @Sendable (JSONValue) -> [String]

    private let lock = NSLock()
    private let incoming: AsyncThrowingStream<Data, Error>
    private let continuation: AsyncThrowingStream<Data, Error>.Continuation
    private let respond: Responder
    private var frames: [JSONValue] = []
    private var closed = false

    init(respond: @escaping Responder) {
        self.respond = respond
        (incoming, continuation) = AsyncThrowingStream<Data, Error>.makeStream()
    }

    /// Every frame the client sent.
    var written: [JSONValue] { lock.withLock { frames } }
    var methods: [String] {
        written.compactMap { if case .string(let m)? = $0["method"] { m } else { nil } }
    }
    var isClosed: Bool { lock.withLock { closed } }

    func write(_ data: Data) async throws {
        if isClosed { throw SocketError.failed("closed") }
        for line in data.split(separator: UInt8(ascii: "\n")) {
            let frame = try JSONValue(json: Data(line))
            lock.withLock { frames.append(frame) }
            for answer in respond(frame) { push(answer) }
        }
    }

    func lines() -> AsyncThrowingStream<Data, Error> { incoming }

    func close() {
        lock.withLock { closed = true }
        continuation.finish()
    }

    /// A line from the service.
    func push(_ line: String) { continuation.yield(Data(line.utf8)) }

    /// The service goes away.
    func drop() {
        lock.withLock { closed = true }
        continuation.finish()
    }
}

/// Scripted service answers.
enum FakeService {
    /// A contract-5 service: answers `hello` and `service.status`; any other method `null`.
    static func standard(serviceVersion: String = "0.0.4", instance: String = "run-1",
                         contract: UInt32 = 5) -> FakeSocketStream.Responder {
        { frame in
            let id = FakePeer.text(frame["id"])
            switch frame["method"] {
            case .string("hello")?:
                if contract != Contract.version {
                    return [mismatch(id: id, service: contract, serviceVersion: serviceVersion)]
                }
                return ["""
                    {"id":\(id),"result":{"contract_version":5,"service_version":"\(serviceVersion)","boot_id":"b","instance":"\(instance)"}}
                    """]
            case .string("service.status")?:
                return ["""
                    {"id":\(id),"result":{"service_version":"\(serviceVersion)","contract_version":\(contract),"pid":1,"running_heavy":0,"waiting_heavy":0,"exe":"/opt/hostler"}}
                    """]
            default:
                return ["{\"id\":\(id),\"result\":null}"]
            }
        }
    }

    static func mismatch(id: String, service: UInt32, serviceVersion: String) -> String {
        """
        {"id":\(id),"error":{"code":"contract_mismatch","message":"this hostler speaks contract 5, the service contract \(service)","details":{"client_contract":5,"service_contract":\(service),"service_version":"\(serviceVersion)"},"recovery":"hostler service start --replace"}}
        """
    }
}

/// A connector that plays a script: each connect takes the next step; the last step repeats.
final class FakeConnector: SocketConnecting, @unchecked Sendable {
    enum Step {
        case fail(SocketError)
        case stream(FakeSocketStream)
        /// Never completes; ends only when the connecting task is cancelled.
        case hang
    }

    private let lock = NSLock()
    private var steps: [Step]
    private var paths: [String] = []
    private var timeouts: [Duration] = []
    private var hanging = 0

    init(_ steps: [Step]) { self.steps = steps }

    var connectCount: Int { lock.withLock { paths.count } }
    var connectedPaths: [String] { lock.withLock { paths } }
    var requestedTimeouts: [Duration] { lock.withLock { timeouts } }
    /// Connects waiting in a `.hang` step now.
    var hangingCount: Int { lock.withLock { hanging } }

    /// Adds steps after the current ones.
    func then(_ more: [Step]) { lock.withLock { steps.append(contentsOf: more) } }

    func connect(path: String, timeout: Duration) async throws(SocketError) -> any SocketStream {
        let step = lock.withLock { () -> Step in
            paths.append(path)
            timeouts.append(timeout)
            return steps.count > 1 ? steps.removeFirst() : steps[0]
        }
        switch step {
        case .fail(let error): throw error
        case .stream(let stream): return stream
        case .hang:
            lock.withLock { hanging += 1 }
            defer { lock.withLock { hanging -= 1 } }
            // Ignores its timeout on purpose: the controller's own bound must end it.
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(10))
            }
            throw .cancelled
        }
    }
}

/// A sleeper the test drives: each `sleep` waits until released or cancelled.
final class FakeSleeper: Sleeping, @unchecked Sendable {
    private let lock = NSLock()
    private var requested: [Duration] = []
    private var waiting: [Int: CheckedContinuation<Void, Error>] = [:]
    private var cancelled: Set<Int> = []
    private var nextID = 0
    private var autoRelease = false

    /// Every duration asked for, in order.
    var durations: [Duration] { lock.withLock { requested } }
    var waitingCount: Int { lock.withLock { waiting.count } }

    /// From now on every sleep returns at once (and the waiting ones are released).
    func releaseAll() {
        lock.withLock { autoRelease = true }
        releaseWaiting()
    }

    /// Releases the sleeps waiting now.
    func releaseWaiting() {
        let all = lock.withLock { () -> [CheckedContinuation<Void, Error>] in
            defer { waiting = [:] }
            return Array(waiting.values)
        }
        all.forEach { $0.resume() }
    }

    func sleep(for duration: Duration) async throws {
        let id = lock.withLock { () -> Int in
            nextID += 1
            requested.append(duration)
            return nextID
        }
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                enum Now { case wait, go, cancel }
                let now = lock.withLock { () -> Now in
                    if cancelled.contains(id) { return .cancel }
                    if autoRelease { return .go }
                    waiting[id] = continuation
                    return .wait
                }
                switch now {
                case .wait: break
                case .go: continuation.resume()
                case .cancel: continuation.resume(throwing: CancellationError())
                }
            }
        } onCancel: {
            let taken = lock.withLock { () -> CheckedContinuation<Void, Error>? in
                cancelled.insert(id)
                return waiting.removeValue(forKey: id)
            }
            taken?.resume(throwing: CancellationError())
        }
    }
}
