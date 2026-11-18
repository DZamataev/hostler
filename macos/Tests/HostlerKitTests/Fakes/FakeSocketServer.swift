import Darwin
import Foundation
@testable import HostlerKit

/// Fails a hang instead of blocking the run (P5-L5): `body` races a deadline. The body runs in
/// an unstructured task, so one stuck in a read cannot hold the test past the deadline.
struct DeadlineExceeded: Error, CustomStringConvertible {
    let what: String
    var description: String { "deadline exceeded: \(what)" }
}

func withDeadline<T: Sendable>(_ what: String, _ limit: Duration = .seconds(10),
                               _ body: @escaping @Sendable () async throws -> T) async throws -> T {
    let once = ResumeOnce<T>()
    return try await withCheckedThrowingContinuation { continuation in
        once.continuation = continuation
        Task {
            do { once.resume(.success(try await body())) } catch { once.resume(.failure(error)) }
        }
        Task {
            try? await Task.sleep(for: limit)
            once.resume(.failure(DeadlineExceeded(what: what)))
        }
    }
}

private final class ResumeOnce<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    var continuation: CheckedContinuation<T, Error>?

    func resume(_ result: Result<T, Error>) {
        let taken = lock.withLock { () -> CheckedContinuation<T, Error>? in
            defer { continuation = nil }
            return continuation
        }
        taken?.resume(with: result)
    }
}

/// Waits until `condition` holds on the main actor, checking every 10 ms, at most `limit`.
@MainActor
func eventually(_ what: String, _ limit: Duration = .seconds(10),
                _ condition: @MainActor () -> Bool) async throws {
    let clock = ContinuousClock()
    let end = clock.now + limit
    while !condition() {
        if clock.now >= end { throw DeadlineExceeded(what: what) }
        try await Task.sleep(for: .milliseconds(10))
    }
}

/// A real Unix socket in a short private directory under `/tmp` that a test scripts: it
/// accepts connections and hands each to the test as a `FakePeer`.
final class FakeSocketServer: @unchecked Sendable {
    let directory: String
    let path: String
    private let listener: Int32
    private let peers: AsyncStream<FakePeer>
    private let peersContinuation: AsyncStream<FakePeer>.Continuation
    private let queue = PeerQueue()

    init() throws {
        var template = Array("/tmp/hk-fake-XXXXXX".utf8CString)
        guard let made = mkdtemp(&template) else { throw SocketError.failed("mkdtemp") }
        directory = String(cString: made)
        path = directory + "/s.sock"
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        // Close-on-exec: a child another test spawns must not keep this listener alive.
        markCloseOnExec(fd)
        listener = fd
        let bound = Self.bind(fd, to: path)
        guard bound, listen(fd, 8) == 0 else {
            throw SocketError.failed("bind/listen: \(String(cString: strerror(errno)))")
        }
        (peers, peersContinuation) = AsyncStream<FakePeer>.makeStream()
        let continuation = peersContinuation
        let state = self.state
        // `accept` polled with a short timeout: on macOS closing a listener does not wake a
        // blocked `accept`, which would keep the socket listening.
        let thread = Thread {
            var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            while !state.isStopping {
                let ready = poll(&poller, 1, 50)
                if ready <= 0 { continue }
                let client = accept(fd, nil, nil)
                if client >= 0 {
                    markCloseOnExec(client)
                    continuation.yield(FakePeer(fd: client))
                }
            }
            Darwin.close(fd)
            continuation.finish()
            state.markClosed()
        }
        thread.start()
        let peers = self.peers
        Task { [queue] in
            for await peer in peers { await queue.push(peer) }
        }
    }

    /// Binds `fd` to the Unix socket `path`.
    static func bind(_ fd: Int32, to path: String) -> Bool {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            buffer.copyBytes(from: bytes)
            buffer[bytes.count] = 0
        }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        return withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        } == 0
    }

    /// The next accepted connection, at most 10 s away.
    func nextPeer() async throws -> FakePeer {
        let queue = self.queue
        return try await withDeadline("a client connects") { await queue.pop() }
    }

    private let state = ListenerState()

    /// Stops accepting and closes the listener; the socket file stays (a stale socket).
    func stopListeningKeepingFile() {
        state.stop()
    }

    /// Stops accepting and removes the directory.
    func stop() {
        stopListeningKeepingFile()
        try? FileManager.default.removeItem(atPath: directory)
    }
}

private final class ListenerState: @unchecked Sendable {
    private let lock = NSLock()
    private var stopping = false
    private let closed = DispatchSemaphore(value: 0)
    private var waited = false

    var isStopping: Bool { lock.withLock { stopping } }

    func markClosed() { closed.signal() }

    /// Asks the accept thread to stop and waits until it closed the listener.
    func stop() {
        let wait = lock.withLock { () -> Bool in
            stopping = true
            defer { waited = true }
            return !waited
        }
        if wait { _ = closed.wait(timeout: .now() + 5) }
    }
}

private actor PeerQueue {
    private var ready: [FakePeer] = []
    private var waiting: [CheckedContinuation<FakePeer, Never>] = []

    func push(_ peer: FakePeer) {
        if waiting.isEmpty { ready.append(peer) } else { waiting.removeFirst().resume(returning: peer) }
    }

    func pop() async -> FakePeer {
        if !ready.isEmpty { return ready.removeFirst() }
        return await withCheckedContinuation { waiting.append($0) }
    }
}

/// The service's side of one fake connection.
final class FakePeer: @unchecked Sendable {
    let fd: Int32
    private let reader: LineReader

    init(fd: Int32) {
        self.fd = fd
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        let stream = POSIXSocketStream(fd: fd)
        reader = LineReader(stream: stream)
        self.stream = stream
    }

    private let stream: POSIXSocketStream

    /// The next frame the client sent, at most 10 s away.
    func nextFrame() async throws -> JSONValue {
        let reader = self.reader
        let line = try await withDeadline("the client sends a frame") { try await reader.next() }
        guard let line else { throw SocketError.failed("the client closed the connection") }
        return try JSONValue(json: line)
    }

    /// The next line the client sent, or `nil` once the client closed.
    func nextFrameOrEnd() async throws -> Data? {
        let reader = self.reader
        return try await withDeadline("the client sends or closes") { try await reader.next() }
    }

    /// Sends `text` and a newline.
    func send(_ text: String) async throws {
        try await sendRaw(Data((text + "\n").utf8))
    }

    func sendRaw(_ data: Data) async throws {
        let stream = self.stream
        try await withDeadline("the fake server writes") { try await stream.write(data) }
    }

    /// Answers a call `id` with `result` (JSON text).
    func answer(_ id: JSONValue?, result: String) async throws {
        try await send("{\"id\":\(Self.text(id)),\"result\":\(result)}")
    }

    func answerError(_ id: JSONValue?, code: String, message: String,
                     details: String = "{}", recovery: String? = nil) async throws {
        let recoveryText = recovery.map { "\"\($0)\"" } ?? "null"
        try await send("{\"id\":\(Self.text(id)),\"error\":{\"code\":\"\(code)\",\"message\":\"\(message)\",\"details\":\(details),\"recovery\":\(recoveryText)}}")
    }

    /// Reads the hello and answers it as a contract-5 service.
    func acceptHello(serviceVersion: String = "0.0.4", instance: String = "run-1") async throws {
        let hello = try await nextFrame()
        guard hello["method"] == .string("hello") else {
            throw SocketError.failed("the first frame was not hello")
        }
        try await answer(hello["id"], result: """
            {"contract_version":5,"service_version":"\(serviceVersion)","boot_id":"boot-1","instance":"\(instance)"}
            """)
    }

    /// Closes the service's side.
    func close() { stream.close() }

    static func text(_ id: JSONValue?) -> String {
        switch id {
        case .int(let value)?: String(value)
        case .uint(let value)?: String(value)
        default: "null"
        }
    }
}

/// Reads a stream's lines one at a time (a task drains the stream into a queue; macOS 14
/// has no `next(isolation:)`).
final class LineReader: @unchecked Sendable {
    private let lock = NSLock()
    private var ready: [Result<Data?, Error>] = []
    private var waiting: CheckedContinuation<Data?, Error>?
    private var ended = false

    init(stream: any SocketStream) {
        let lines = stream.lines()
        Task { [self] in
            do {
                for try await line in lines { self.deliver(.success(line)) }
                self.deliver(.success(nil))
            } catch {
                self.deliver(.failure(error))
            }
        }
    }

    private func deliver(_ item: Result<Data?, Error>) {
        let taken = lock.withLock { () -> CheckedContinuation<Data?, Error>? in
            if let waiting {
                self.waiting = nil
                if case .success(nil) = item { ended = true }
                if case .failure = item { ended = true }
                return waiting
            }
            ready.append(item)
            return nil
        }
        taken?.resume(with: item)
    }

    /// The next line; `nil` at the end (and after it).
    func next() async throws -> Data? {
        try await withCheckedThrowingContinuation { continuation in
            let item = lock.withLock { () -> Result<Data?, Error>? in
                if ended { return .success(nil) }
                if !ready.isEmpty {
                    let item = ready.removeFirst()
                    if case .success(nil) = item { ended = true }
                    if case .failure = item { ended = true }
                    return item
                }
                waiting = continuation
                return nil
            }
            if let item { continuation.resume(with: item) }
        }
    }
}
