import Foundation
import HostlerKit
import Testing
@testable import Hostler

/// Task 7's wiring: `start()` starts the connection, `shutdown()` closes it; a connection is
/// built only from the environment.
@Suite(.timeLimit(.minutes(1))) @MainActor struct AppModelConnectionTests {
    final class Counter: SocketConnecting, @unchecked Sendable {
        private let lock = NSLock()
        private var count = 0
        var connects: Int { lock.withLock { count } }
        func connect(path: String, timeout: Duration) async throws(SocketError) -> any SocketStream {
            lock.withLock { count += 1 }
            throw .notRunning(path: path)
        }
    }

    struct NeverSleeps: Sleeping {
        func sleep(for duration: Duration) async throws {
            try await Task.sleep(for: .seconds(3600))
        }
    }

    struct NoRegistration: RegistrationReading {
        func registration() -> Registration? { nil }
    }

    static func environment(_ connector: Counter) -> ConnectionEnvironment {
        ConnectionEnvironment(
            location: .success(SocketLocator.location(home: "/tmp/hostler-app-model", euid: 501)),
            defaultHome: "/Users/someone/Library/Application Support/io.github.dzamataev.hostler",
            euid: 501, connector: connector, registration: NoRegistration(),
            fileExists: { _ in false }, sleeper: NeverSleeps(), clientName: "hostler-app test")
    }

    @Test func noConnectionWithoutAnEnvironment() {
        let model = AppModel(env: .fake())
        #expect(model.connection == nil)
        #expect(model.store == nil)
    }

    @Test func theStoreIsTheConnectionsSink() throws {
        let model = AppModel(env: .fake(connection: Self.environment(Counter())))
        let connection = try #require(model.connection)
        let store = try #require(model.store)
        #expect(connection.sink === store)
    }

    @Test func startConnectsAndShutdownStops() async throws {
        let connector = Counter()
        let model = AppModel(env: .fake(connection: Self.environment(connector)))
        let connection = try #require(model.connection)
        #expect(connector.connects == 0)
        model.start()
        let clock = ContinuousClock()
        let end = clock.now + .seconds(10)
        while connection.state == .connecting, clock.now < end {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(connection.state == .notRunning(
            offer: .typeCommand("HOSTLER_HOME=/tmp/hostler-app-model hostler service run")))
        #expect(connector.connects == 1)
        await model.shutdown()
        #expect(model.hasShutDown)
        connection.retryNow()
        try await Task.sleep(for: .milliseconds(100))
        #expect(connector.connects == 1)
    }
}
