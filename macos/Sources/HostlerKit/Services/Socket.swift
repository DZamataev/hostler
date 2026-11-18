import Darwin
import Foundation

/// Why the socket could not be used. No case carries a frame's text.
public enum SocketError: Error, Sendable, Equatable {
    /// No service listens there (`ENOENT`, `ECONNREFUSED`).
    case notRunning(path: String)
    /// This process may not reach the socket (`EACCES`, `EPERM`): a sandbox, a permission.
    case denied(path: String, reason: String)
    /// The socket file belongs to another user; it is never used (`check_socket_owner`).
    case foreignOwner(path: String, uid: UInt32)
    /// The peer broke the framing (a line longer than `Contract.maxFrameBytes`).
    case protocolError(String)
    /// `connect` did not complete within its bound (a service that does not accept).
    case timedOut(path: String)
    /// The waiting task was cancelled (Retry, the app quitting).
    case cancelled
    /// Any other failure of the socket calls, with the call and `strerror`.
    case failed(String)
}

extension SocketError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .notRunning(let path):
            String(localized: "the hostler service is not running (\(path))")
        case .denied(let path, let reason):
            String(localized: "this app may not reach \(path): \(reason)")
        case .foreignOwner(let path, let uid):
            String(localized:
                "\(path) belongs to uid \(uid), not to the current user; refusing to use it")
        case .protocolError(let what): what
        case .timedOut(let path):
            String(localized: "connecting to the hostler service timed out (\(path))")
        case .cancelled: String(localized: "connecting to the hostler service was cancelled")
        case .failed(let what): what
        }
    }
}

/// Opens a connection to a socket path (the POSIX one in the app; fakes in tests). It returns
/// within `timeout`, and at once when the calling task is cancelled.
public protocol SocketConnecting: Sendable {
    func connect(path: String, timeout: Duration) async throws(SocketError) -> any SocketStream
}

/// One connection: newline-delimited frames both ways.
public protocol SocketStream: Sendable {
    /// Writes `data` (one or more whole frames, each ending in `\n`).
    func write(_ data: Data) async throws
    /// The incoming lines, without their `\n`. Call once. The stream finishes when the peer
    /// closes or `close()` is called, and throws `SocketError.protocolError` on a line over
    /// the frame limit (the connection is then closed).
    func lines() -> AsyncThrowingStream<Data, Error>
    /// Shuts the connection down; a pending read or write ends at once.
    func close()
}

/// Marks every descriptor the app opens close-on-exec: `Process` children (the CLI runner)
/// inherit open descriptors otherwise, and a child holding the app's socket keeps a
/// connection, or a test's listener, alive. (macOS has no `SOCK_CLOEXEC`.)
func markCloseOnExec(_ fd: Int32) {
    _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
}

/// `socket(AF_UNIX)` + a non-blocking `connect` polled against a deadline, after the owner
/// check (Task 0: Network.framework cannot reach a Unix socket headless). The waiting runs on
/// a dispatch thread and is cancellable; on a timeout or a cancel the descriptor is closed.
public struct POSIXSocketConnector: SocketConnecting {
    typealias ConnectCall = @Sendable (Int32, UnsafePointer<sockaddr>, socklen_t) -> Int32

    /// The uid the socket file must belong to: this process's effective uid (tests change it
    /// to see the refusal without a second user).
    let owner: uid_t
    /// `connect(2)` (tests replace it with one that never completes).
    let connectCall: ConnectCall

    public init() {
        owner = geteuid()
        connectCall = { Darwin.connect($0, $1, $2) }
    }

    init(owner: uid_t = geteuid(), connectCall: @escaping ConnectCall = { Darwin.connect($0, $1, $2) }) {
        self.owner = owner
        self.connectCall = connectCall
    }

    public func connect(path: String, timeout: Duration) async throws(SocketError) -> any SocketStream {
        let flag = CancelFlag()
        let owner = self.owner
        let connectCall = self.connectCall
        let result: Result<Int32, SocketError> = await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                DispatchQueue.global().async {
                    continuation.resume(returning: Self.open(
                        path: path, owner: owner, timeout: timeout, cancel: flag,
                        connectCall: connectCall))
                }
            }
        } onCancel: {
            flag.set()
        }
        switch result {
        case .success(let fd): return POSIXSocketStream(fd: fd)
        case .failure(let error): throw error
        }
    }

    static func open(path: String, owner: uid_t, timeout: Duration, cancel: CancelFlag,
                     connectCall: ConnectCall) -> Result<Int32, SocketError> {
        if cancel.isSet { return .failure(.cancelled) }
        // The owner check first, as the CLI's `check_socket_owner`.
        var info = stat()
        if lstat(path, &info) != 0 {
            return .failure(classify(errno, path: path, call: "lstat"))
        }
        if info.st_uid != owner {
            return .failure(.foreignOwner(path: path, uid: info.st_uid))
        }

        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard bytes.count < capacity else {
            return .failure(.failed("the socket path is \(bytes.count) bytes; at most \(capacity - 1) fit"))
        }
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            buffer.copyBytes(from: bytes)
            buffer[bytes.count] = 0
        }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        if fd < 0 { return .failure(.failed("socket: \(String(cString: strerror(errno)))")) }
        markCloseOnExec(fd)
        var on: Int32 = 1
        _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        let flags = fcntl(fd, F_GETFL)
        _ = fcntl(fd, F_SETFL, flags | O_NONBLOCK)

        let clock = ContinuousClock()
        let deadline = clock.now + timeout
        func fail(_ error: SocketError) -> Result<Int32, SocketError> {
            Darwin.close(fd)
            return .failure(error)
        }
        while true {
            if cancel.isSet { return fail(.cancelled) }
            if clock.now >= deadline { return fail(.timedOut(path: path)) }
            let result = withUnsafePointer(to: &address) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    connectCall(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            if result == 0 || errno == EISCONN { break }
            let code = errno
            switch code {
            case EINTR:
                continue
            case EINPROGRESS, EALREADY:
                // Wait for the socket to become writable, in slices, then read its outcome.
                var poller = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
                var ready: Int32 = 0
                while ready == 0 {
                    if cancel.isSet { return fail(.cancelled) }
                    if clock.now >= deadline { return fail(.timedOut(path: path)) }
                    ready = poll(&poller, 1, 50)
                    if ready < 0, errno == EINTR { ready = 0 }
                }
                var error: Int32 = 0
                var length = socklen_t(MemoryLayout<Int32>.size)
                _ = getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &length)
                if error != 0 { return fail(classify(error, path: path, call: "connect")) }
            case EAGAIN:
                // The listener's queue is full: try again shortly, within the bound.
                _ = poll(nil, 0, 50)
                continue
            default:
                return fail(classify(code, path: path, call: "connect"))
            }
            break
        }
        _ = fcntl(fd, F_SETFL, flags & ~O_NONBLOCK)
        return .success(fd)
    }

    static func classify(_ code: Int32, path: String, call: String) -> SocketError {
        let reason = String(cString: strerror(code))
        switch code {
        case ENOENT, ECONNREFUSED, ENOTDIR: return .notRunning(path: path)
        case EACCES, EPERM: return .denied(path: path, reason: reason)
        default: return .failed("\(call) \(path): \(reason)")
        }
    }
}

/// A flag set by a cancellation handler and read by a blocking loop.
final class CancelFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    var isSet: Bool { lock.withLock { value } }
    func set() { lock.withLock { value = true } }
}

/// A connected Unix socket. Reading happens on a dedicated thread with `read(2)` into a
/// buffer, split on `\n` (P5-L5): `FileHandle.read(upToCount:)`, `readToEnd()` and
/// `FileHandle.bytes` wait for EOF, which a hostler connection never sends. `close()` shuts
/// the socket down, which ends a blocked `read(2)`; the descriptor itself is closed when the
/// last reference goes, so no other thread can be using a reused number.
public final class POSIXSocketStream: SocketStream, @unchecked Sendable {
    let fd: Int32
    private let lock = NSLock()
    private var isShutDown = false
    private var readerStarted = false
    private let writeQueue = DispatchQueue(label: "io.github.dzamataev.hostler.app.socket-write")
    let maxLineBytes: Int

    init(fd: Int32, maxLineBytes: Int = Contract.maxFrameBytes) {
        self.fd = fd
        self.maxLineBytes = maxLineBytes
    }

    deinit {
        Darwin.close(fd)
    }

    public func write(_ data: Data) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            writeQueue.async {
                continuation.resume(with: Result { try self.writeAll(data) })
            }
        }
    }

    private func writeAll(_ data: Data) throws {
        if lock.withLock({ isShutDown }) { throw SocketError.failed("the connection is closed") }
        try data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
            guard var base = buffer.baseAddress else { return }
            var left = buffer.count
            while left > 0 {
                let sent = Darwin.send(fd, base, left, 0)
                if sent < 0 {
                    if errno == EINTR { continue }
                    throw SocketError.failed("writing to the service failed: \(String(cString: strerror(errno)))")
                }
                left -= sent
                base += sent
            }
        }
    }

    public func lines() -> AsyncThrowingStream<Data, Error> {
        let (stream, continuation) = AsyncThrowingStream<Data, Error>.makeStream()
        let start = lock.withLock { () -> Bool in
            if readerStarted { return false }
            readerStarted = true
            return true
        }
        guard start else {
            continuation.finish(throwing: SocketError.failed("lines() was already called"))
            return stream
        }
        continuation.onTermination = { [weak self] reason in
            if case .cancelled = reason { self?.close() }
        }
        let thread = Thread { [self] in
            self.readLoop(continuation)
        }
        thread.name = "hostler socket reader"
        thread.start()
        return stream
    }

    private func readLoop(_ continuation: AsyncThrowingStream<Data, Error>.Continuation) {
        let chunk = 64 * 1024
        let buffer = UnsafeMutableRawPointer.allocate(byteCount: chunk, alignment: 1)
        defer {
            memset_s(buffer, chunk, 0, chunk)
            buffer.deallocate()
        }
        var pending = Data()
        // After a sensitive call any line may hold values: the bytes of each line are erased
        // from this buffer once handed on.
        defer { pending.resetBytes(in: 0..<pending.count) }
        while true {
            let count = Darwin.read(fd, buffer, chunk)
            if count == 0 {
                continuation.finish()
                return
            }
            if count < 0 {
                if errno == EINTR { continue }
                if lock.withLock({ isShutDown }) {
                    continuation.finish()
                } else {
                    continuation.finish(throwing: SocketError.failed(
                        "reading from the service failed: \(String(cString: strerror(errno)))"))
                }
                return
            }
            // The bytes before `scanned` hold no newline: only the new ones are searched.
            let scanned = pending.count
            pending.append(buffer.assumingMemoryBound(to: UInt8.self), count: count)
            memset_s(buffer, chunk, 0, count)
            var start = 0
            var searchFrom = scanned
            while let newline = Self.newline(in: pending, from: searchFrom) {
                if newline - start > maxLineBytes {
                    tooLong(continuation)
                    return
                }
                let base = pending.startIndex
                continuation.yield(Data(pending[(base + start)..<(base + newline)]))
                start = newline + 1
                searchFrom = start
            }
            if start > 0 {
                let base = pending.startIndex
                pending.resetBytes(in: base..<(base + start))
                pending.removeSubrange(base..<(base + start))
            }
            if pending.count > maxLineBytes {
                tooLong(continuation)
                return
            }
        }
    }

    /// The offset of the first `\n` at or after `offset`.
    private static func newline(in data: Data, from offset: Int) -> Int? {
        data.withUnsafeBytes { raw -> Int? in
            guard let base = raw.baseAddress, offset < raw.count,
                  let found = memchr(base + offset, 0x0A, raw.count - offset)
            else { return nil }
            return base.distance(to: UnsafeRawPointer(found))
        }
    }

    private func tooLong(_ continuation: AsyncThrowingStream<Data, Error>.Continuation) {
        close()
        continuation.finish(throwing: SocketError.protocolError(
            String(localized: "the service sent a frame longer than \(String(maxLineBytes)) bytes")))
    }

    public func close() {
        let first = lock.withLock { () -> Bool in
            defer { isShutDown = true }
            return !isShutDown
        }
        if first { _ = Darwin.shutdown(fd, SHUT_RDWR) }
    }
}
