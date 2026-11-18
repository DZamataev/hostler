import Darwin
import Foundation
import Observation

/// What the app knows about its service (spec §9.7). Every state but `.live` is shown with
/// what fixes it.
public enum ConnectionState: Sendable, Equatable {
    /// The first attempt, and after a live connection dropped.
    case connecting
    case live(serviceVersion: String)
    /// No service listens; the offer is a Start button or the command to type (`StartPolicy`).
    /// Retried with backoff, and at once when the popover opens (P5-L12).
    case notRunning(offer: StartOffer)
    /// This process may not reach the socket: the reason; never a Start. Waits for Retry or
    /// the popover opening (P5-L12).
    case denied(reason: String)
    /// The socket belongs to another user; never used. Waits for Retry.
    case foreignOwner(path: String)
    /// The service speaks another contract: both versions and the service's recovery. The
    /// app never replaces a service. Waits for Retry, which asks `service.status` first.
    case mismatch(client: UInt32, service: UInt32, serviceVersion: String, recovery: String?)
    /// The service broke the protocol, did not accept in time, or the connection failed
    /// otherwise; the text says how. Retried with backoff.
    case protocolError(String)
    /// `HOSTLER_HOME` is not absolute. Nothing to retry until the app is relaunched.
    case badHome(String)
}

/// Where the connection's events go: the store (`HostlerStore` conforms). Every call names
/// the connection it is about, so a frame of an older connection is never taken for a newer
/// one's (P5-L11).
@MainActor
public protocol ConnectionEventSink: AnyObject {
    /// `hello` answered on `connection`.
    func connected(instance: String?, connection: ConnectionToken)
    /// `connection` ended (closed, failed, replaced, the app quitting). Once per connection
    /// that was `connected`.
    func dropped(connection: ConnectionToken)
    /// An event frame of `connection`, in read order (`resync` included).
    func receive(_ event: Event, seq: UInt64, connection: ConnectionToken, frameBytes: Int?)
}

extension HostlerStore: ConnectionEventSink {}

/// Waits; throws when the waiting task is cancelled (`Task.sleep` in the app, a fake in tests).
public protocol Sleeping: Sendable {
    func sleep(for duration: Duration) async throws
}

public struct TaskSleeper: Sleeping {
    public init() {}
    public func sleep(for duration: Duration) async throws {
        try await Task.sleep(for: duration)
    }
}

/// What the controller needs from the OS, built once in `AppEnvironment` (fakes in tests).
public struct ConnectionEnvironment: Sendable {
    /// The app's home and socket, or why `HOSTLER_HOME` is refused.
    public var location: Result<SocketLocation, HostlerAppError>
    /// The per-user default home (the one a registration serves).
    public var defaultHome: String
    public var euid: UInt32
    public var connector: any SocketConnecting
    public var registration: any RegistrationReading
    public var fileExists: @Sendable (String) -> Bool
    public var sleeper: any Sleeping
    /// `hello`'s `client`: `hostler-app <version>`.
    public var clientName: String
    /// How long one `connect` may take (the CLI's `CONNECT_TIMEOUT`). The controller also
    /// enforces it, a second later, on a connector that does not.
    public var connectTimeout: Duration

    public init(location: Result<SocketLocation, HostlerAppError>, defaultHome: String,
                euid: UInt32, connector: any SocketConnecting, registration: any RegistrationReading,
                fileExists: @escaping @Sendable (String) -> Bool, sleeper: any Sleeping,
                clientName: String, connectTimeout: Duration = .seconds(2)) {
        self.location = location
        self.defaultHome = defaultHome
        self.euid = euid
        self.connector = connector
        self.registration = registration
        self.fileExists = fileExists
        self.sleeper = sleeper
        self.clientName = clientName
        self.connectTimeout = connectTimeout
    }

    /// The app's own: its environment, the POSIX socket, the LaunchAgent plist.
    public static func live(environment: [String: String] = ProcessInfo.processInfo.environment,
                            appVersion: String) -> ConnectionEnvironment {
        let userHome = SocketLocator.userHomeDirectory(env: environment)
        let euid = geteuid()
        let location: Result<SocketLocation, HostlerAppError>
        do {
            location = .success(try SocketLocator.resolve(env: environment,
                                                          homeDirectory: userHome, euid: euid))
        } catch {
            location = .failure(error)
        }
        return ConnectionEnvironment(
            location: location, defaultHome: SocketLocator.defaultHome(homeDirectory: userHome),
            euid: euid, connector: POSIXSocketConnector(),
            registration: LaunchAgentRegistration(homeDirectory: userHome),
            fileExists: { FileManager.default.isExecutableFile(atPath: $0) },
            sleeper: TaskSleeper(), clientName: "hostler-app \(appVersion)")
    }
}

/// The app's one connection to the service: its state, reconnects, the events for the store
/// (`sink`), and the grant of the last proof (spec §6.2, §8.5, §9.7).
///
/// Every connection it opens gets the next `ConnectionToken` (1, 2, 3…, never reused while the
/// app runs, a failed attempt included). It never starts the service: `startService(with:)` is
/// the only path to a start, and only the banner's Start button calls it.
@MainActor
@Observable
public final class ConnectionController {
    public private(set) var state: ConnectionState = .connecting
    /// The grant of the last successful `secrets.prove` or `secrets.unlock` on the current
    /// connection. Dropped on reconnect, on a failed prove or unlock, and on every lock-state
    /// transition after it (spec §8.5); the service stays the authority (`proof_required`).
    public private(set) var grant: ProofGrant?
    /// The current connection's `hello` reply.
    public private(set) var helloReply: HelloReply?
    /// The live connection's token; `nil` while there is none.
    public private(set) var currentConnection: ConnectionToken?

    /// Receives `connected`, `dropped` and every event frame (the store).
    @ObservationIgnored public weak var sink: (any ConnectionEventSink)?

    /// Typed calls on the current connection (`notConnected` while there is none).
    public nonisolated let client: HostlerClient

    /// The reconnect delays while the service is not running or failed: 0.5, 1, 2, 5, 10, 20,
    /// then 30 s each (P5-L12).
    public static let backoff: [Duration] = [.milliseconds(500), .seconds(1), .seconds(2),
                                             .seconds(5), .seconds(10), .seconds(20), .seconds(30)]

    @ObservationIgnored private let env: ConnectionEnvironment
    @ObservationIgnored private let box = CurrentConnection()
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var waiting: Task<Void, Never>?
    @ObservationIgnored private var connecting: Task<Result<any SocketStream, SocketError>, Never>?
    /// A connection between connect and a live `hello`, closed by `stop()`.
    @ObservationIgnored private var opening: HostlerConnection?
    @ObservationIgnored private var stopped = false
    @ObservationIgnored private var lastToken: UInt64 = 0
    /// The seq a grant belongs to: a `secrets.lock_changed` above it voids the grant.
    @ObservationIgnored private var grantFloor: UInt64 = 0
    @ObservationIgnored private var lastSeq: UInt64 = 0

    public init(environment: ConnectionEnvironment) {
        env = environment
        client = HostlerClient(transport: box)
    }

    /// The app's home and socket (`nil` when `HOSTLER_HOME` is refused).
    public var location: SocketLocation? { try? env.location.get() }

    /// Starts connecting; reconnects until `stop()`.
    public func start() {
        guard loop == nil, !stopped else { return }
        loop = Task { [weak self] in await self?.run() }
    }

    /// Retry at once (the Retry button, the popover opening): ends a wait, or abandons a
    /// connect that is taking long and starts a new one.
    public func retryNow() {
        waiting?.cancel()
        connecting?.cancel()
    }

    /// The Start button: runs the offer's start only for a `.start` offer of the current
    /// `.notRunning` state, then retries at once.
    public func startService(with starter: any ServiceStarting) async -> Result<Void, HostlerAppError> {
        guard case .notRunning(let offer) = state else {
            return .failure(.nothingToStart(command: StartPolicy.installCommand))
        }
        let result = await starter.start(offer)
        if case .success = result { retryNow() }
        return result
    }

    /// Closes the connection and stops reconnecting (the app quits). Returns once the loop has
    /// ended, so the sink has had its `dropped`.
    public func stop() async {
        stopped = true
        waiting?.cancel()
        connecting?.cancel()
        loop?.cancel()
        if let connection = opening { await connection.close() }
        if let connection = box.take() { await connection.close() }
        await loop?.value
        grant = nil
    }

    // MARK: - The loop

    private enum Next {
        case live(HostlerConnection, ConnectionToken)
        /// Wait the backoff, then connect.
        case backoff
        /// Wait for Retry (or the popover), then connect.
        case waitForRetry
        /// Wait for Retry, then ask `service.status` before a `hello`.
        case mismatch(SocketLocation)
        /// Connect again at once.
        case now
        case giveUp
    }

    private func run() async {
        var failures = 0
        var mismatched: SocketLocation?
        while !stopped {
            let next: Next
            if let location = mismatched {
                next = await pollOnce(location)
            } else {
                next = await connectOnce()
            }
            mismatched = nil
            if stopped {
                if case .live(let connection, let token) = next {
                    await connection.close()
                    ended(connection, token)
                }
                return
            }
            switch next {
            case .live(let connection, let token):
                failures = 0
                await consume(connection, token)
                ended(connection, token)
                if stopped { return }
                state = .connecting
                await wait(Self.delay(after: failures))
                failures += 1
            case .backoff:
                await wait(Self.delay(after: failures))
                failures += 1
            case .waitForRetry:
                await waitForRetry()
            case .mismatch(let location):
                mismatched = location
                await waitForRetry()
            case .now:
                continue
            case .giveUp:
                return
            }
        }
    }

    static func delay(after failures: Int) -> Duration {
        backoff[min(failures, backoff.count - 1)]
    }

    private func nextToken() -> ConnectionToken {
        lastToken += 1
        return ConnectionToken(lastToken)
    }

    private func connectOnce() async -> Next {
        let token = nextToken()
        let location: SocketLocation
        switch env.location {
        case .success(let found): location = found
        case .failure(let error):
            if case .badHome(let home) = error { state = .badHome(home) }
            else { state = .protocolError(error.localizedDescription) }
            return .giveUp
        }
        let stream: any SocketStream
        switch await connect(location.socketPath) {
        case .success(let opened): stream = opened
        case .failure(.cancelled): return stopped ? .giveUp : .now
        case .failure(let error): return failed(error, location)
        }
        let connection = HostlerConnection(stream: stream)
        opening = connection
        await connection.start()
        let reply: HelloReply
        do {
            reply = try await connection.hello(client: env.clientName)
        } catch {
            opening = nil
            await connection.close()
            if stopped { return .giveUp }
            if case .service(let service) = error, service.code == .contractMismatch {
                state = Self.mismatchState(service, forHome: { [env] in
                    commandForHome($0, home: location, defaultHome: env.defaultHome, euid: env.euid)
                })
                return .mismatch(location)
            }
            state = .protocolError(error.localizedDescription)
            return .backoff
        }
        opening = nil
        if stopped {
            await connection.close()
            return .giveUp
        }
        box.set(connection)
        helloReply = reply
        currentConnection = token
        grant = nil
        grantFloor = 0
        lastSeq = 0
        state = .live(serviceVersion: reply.serviceVersion)
        Log.client.info("connected to hostler \(reply.serviceVersion, privacy: .public)")
        sink?.connected(instance: reply.instance, connection: token)
        return .live(connection, token)
    }

    /// The connection `token` ended: everything about it goes, and the sink hears it once.
    private func ended(_ connection: HostlerConnection, _ token: ConnectionToken) {
        box.clear(connection)
        grant = nil
        helloReply = nil
        currentConnection = nil
        sink?.dropped(connection: token)
    }

    /// One bounded connect: the connector's own bound, and the controller's a second later
    /// for a connector that keeps none. Retry and stop cancel it.
    private func connect(_ path: String) async -> Result<any SocketStream, SocketError> {
        let connector = env.connector
        let timeout = env.connectTimeout
        let task = Task { () -> Result<any SocketStream, SocketError> in
            do throws(SocketError) {
                return .success(try await connector.connect(path: path, timeout: timeout))
            } catch {
                return .failure(error)
            }
        }
        connecting = task
        let overdue = TimeoutFlag()
        let watchdog = Task {
            do { try await Task.sleep(for: timeout + .seconds(1)) } catch { return }
            overdue.set()
            task.cancel()
        }
        let result = await task.value
        watchdog.cancel()
        connecting = nil
        if case .failure(.cancelled) = result, overdue.isSet { return .failure(.timedOut(path: path)) }
        return result
    }

    private func failed(_ error: SocketError, _ location: SocketLocation) -> Next {
        state = stateFor(error, location)
        switch error {
        case .denied, .foreignOwner: return .waitForRetry
        case .notRunning, .protocolError, .timedOut, .failed: return .backoff
        case .cancelled: return stopped ? .giveUp : .now
        }
    }

    private func consume(_ connection: HostlerConnection, _ token: ConnectionToken) async {
        for await item in connection.incoming {
            switch item {
            case .event(let numbered):
                lastSeq = numbered.seq
                if case .secretsLockChanged = numbered.event, numbered.seq > grantFloor {
                    grant = nil
                }
                sink?.receive(numbered.event, seq: numbered.seq, connection: token,
                              frameBytes: numbered.frameBytes)
            case .unreadableEvent:
                // Something was not delivered: this connection ends, and the next one's
                // `connected` makes the store read everything again.
                Log.client.error("an event did not decode; reconnecting")
                await connection.close()
                return
            case .granted(let newGrant, let asOfSeq):
                grant = newGrant
                grantFloor = asOfSeq ?? lastSeq
            case .grantVoided:
                grant = nil
            case .closed(let reason):
                if let reason {
                    Log.client.info("the connection ended: \(reason.localizedDescription, privacy: .public)")
                }
                return
            }
        }
    }

    /// After Retry on a mismatch: `service.status` on a lifecycle connection (no `hello`).
    /// The same contract → connect at once; still another → wait for Retry again; the socket
    /// gone or refused → that state, through the normal backoff or wait.
    private func pollOnce(_ location: SocketLocation) async -> Next {
        _ = nextToken()
        let stream: any SocketStream
        switch await connect(location.socketPath) {
        case .success(let opened): stream = opened
        case .failure(.cancelled): return stopped ? .giveUp : .mismatch(location)
        case .failure(let error): return failed(error, location)
        }
        let connection = HostlerConnection(stream: stream)
        opening = connection
        await connection.start()
        let status = try? await HostlerClient(transport: connection).serviceStatus()
        opening = nil
        await connection.close()
        if stopped { return .giveUp }
        if let status, status.contractVersion == Contract.version { return .now }
        return .mismatch(location)
    }

    private func wait(_ duration: Duration) async {
        let sleeper = env.sleeper
        let task = Task { _ = try? await sleeper.sleep(for: duration) }
        waiting = task
        await task.value
        waiting = nil
    }

    /// Waits until `retryNow()` or `stop()`.
    private func waitForRetry() async {
        let task = Task { _ = try? await Task.sleep(for: .seconds(1_000_000_000)) }
        waiting = task
        await task.value
        waiting = nil
    }

    // MARK: - States

    private func stateFor(_ error: SocketError, _ location: SocketLocation) -> ConnectionState {
        switch error {
        case .notRunning:
            return .notRunning(offer: startOffer(
                home: location, defaultHome: env.defaultHome,
                registration: env.registration.registration(), euid: env.euid,
                fileExists: env.fileExists))
        case .denied:
            return .denied(reason: error.localizedDescription)
        case .foreignOwner(let path, _):
            return .foreignOwner(path: path)
        case .protocolError(let text), .failed(let text):
            return .protocolError(text)
        case .timedOut, .cancelled:
            return .protocolError(error.localizedDescription)
        }
    }

    /// - Parameter forHome: the service's recovery as typed for the app's home (Task 13 F-2).
    static func mismatchState(_ error: HostlerError,
                              forHome: (String) -> String = { $0 }) -> ConnectionState {
        func number(_ key: String) -> UInt32 {
            switch error.details[key] {
            case .int(let value)?: UInt32(clamping: value)
            case .uint(let value)?: UInt32(clamping: value)
            default: 0
            }
        }
        var version = ""
        if case .string(let text)? = error.details["service_version"] { version = text }
        return .mismatch(client: number("client_contract"), service: number("service_contract"),
                         serviceVersion: version, recovery: error.recovery.map(forHome))
    }
}

/// Set once by a watchdog.
private final class TimeoutFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    var isSet: Bool { lock.withLock { value } }
    func set() { lock.withLock { value = true } }
}

/// The transport behind `ConnectionController.client`: the current connection, or
/// `notConnected`.
final class CurrentConnection: HostlerTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var connection: HostlerConnection?

    func set(_ connection: HostlerConnection) { lock.withLock { self.connection = connection } }

    /// Clears `connection` when it is still the current one.
    func clear(_ connection: HostlerConnection) {
        lock.withLock { if self.connection === connection { self.connection = nil } }
    }

    func take() -> HostlerConnection? {
        lock.withLock {
            defer { connection = nil }
            return connection
        }
    }

    private var current: HostlerConnection? { lock.withLock { connection } }

    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        guard let connection = current else { throw .notConnected }
        return try await connection.call(request, timeout: timeout)
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        guard let connection = current else { throw .notConnected }
        return try await connection.callSensitive(request, timeout: timeout)
    }
}
