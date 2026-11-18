import Foundation
@testable import HostlerKit

/// Records every CLI run and answers with `result`.
final class FakeCommandRunner: HostlerCommandRunning, @unchecked Sendable {
    private let lock = NSLock()
    private var runs: [CLIInvocation] = []
    let result: CLIResult

    init(result: CLIResult = .ok(.null)) { self.result = result }

    var invocations: [CLIInvocation] { lock.withLock { runs } }

    func run(_ invocation: CLIInvocation) async -> CLIResult {
        lock.withLock { runs.append(invocation) }
        return result
    }
}

/// Records every launch and answers with `outcome`.
final class FakeProcessLauncher: ProcessLaunching, @unchecked Sendable {
    struct Launch: Equatable {
        var executable: String
        var arguments: [String]
        var environment: [String: String]
        var cwd: String
        var timeout: Duration
        var stdoutLimit: Int
        var stderrLimit: Int
    }

    private let lock = NSLock()
    private var all: [Launch] = []
    let outcome: ProcessOutcome

    init(_ outcome: ProcessOutcome) { self.outcome = outcome }

    var launches: [Launch] { lock.withLock { all } }

    func launch(executable: String, arguments: [String], environment: [String: String],
                cwd: String, timeout: Duration, stdoutLimit: Int, stderrLimit: Int) async
        -> ProcessOutcome
    {
        lock.withLock {
            all.append(Launch(executable: executable, arguments: arguments,
                              environment: environment, cwd: cwd, timeout: timeout,
                              stdoutLimit: stdoutLimit, stderrLimit: stderrLimit))
        }
        return outcome
    }
}

/// Counts starts; never starts anything.
final class FakeStarter: ServiceStarting, @unchecked Sendable {
    private let lock = NSLock()
    private var offers: [StartOffer] = []

    var calls: [StartOffer] { lock.withLock { offers } }

    func start(_ offer: StartOffer) async -> Result<Void, HostlerAppError> {
        lock.withLock { offers.append(offer) }
        return .success(())
    }
}

/// A fixed registration.
struct FakeRegistration: RegistrationReading {
    var value: Registration?
    func registration() -> Registration? { value }
}
