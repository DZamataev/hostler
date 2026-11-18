import Foundation
@testable import HostlerKit

/// A scripted service behind `HostlerTransport` for the store's tests (Task 8): it answers the
/// resync's reads from its current state, every snapshot at its current `seq`, and can hold
/// the reads back until a test releases them.
actor FakeServiceTransport: HostlerTransport {
    var instance: String? = "A"
    var seq: UInt64 = 0
    var capacity: UInt8 = 2
    var running: [HeavyEntry] = []
    var claims: [ResourceState] = []
    var lock: LockState = .unlocked
    var devices: [DeviceInfo] = []
    var projects: [ProjectView] = []
    var secretRequests: [SecretRequestInfo] = []
    var queueHistory: [HeavyHistoryItem] = []
    var claimHistory: [ClaimHistoryItem] = []
    var installs: [InstallEvent] = []
    var secretLists: [String: SecretList] = [:]
    /// Methods that fail with this error until cleared.
    var failing: [HostlerMethod: HostlerCallError] = [:]

    private(set) var calls: [Request] = []
    /// While true, every `secret.list` waits until the test answers it with `answerSecretList`.
    var gateSecretLists = false
    private var secretListWaiters: [CheckedContinuation<Result<SecretList, HostlerCallError>, Never>] = []
    var waitingSecretLists: Int { secretListWaiters.count }

    /// Answers the `index`-th waiting `secret.list` (in arrival order, of those still waiting).
    func answerSecretList(_ index: Int, with result: Result<SecretList, HostlerCallError>) {
        secretListWaiters.remove(at: index).resume(returning: result)
    }
    private var holding = false
    private var held: [CheckedContinuation<Void, Never>] = []

    var heldCount: Int { held.count }

    func callCount(_ method: HostlerMethod) -> Int { calls.filter { $0.method == method }.count }

    func set(_ change: @Sendable (isolated FakeServiceTransport) -> Void) { change(self) }

    /// Reads wait from now on, until `release()`.
    func holdReads() { holding = true }

    func release() {
        holding = false
        let waiting = held
        held = []
        for continuation in waiting { continuation.resume() }
    }

    // The service's snapshots now.
    func queueSnapshot() -> QueueSnapshot { Mk.queue(seq: seq, capacity: capacity, running: running) }
    func claimsSnapshot() -> ClaimsSnapshot { Mk.claims(seq: seq, claims) }
    func secretsStatus() -> SecretsStatus {
        Mk.secrets(seq: seq, state: lock, pending: UInt32(pendingSecrets().count))
    }
    func devicesSnapshot() -> DevicesSnapshot { Mk.devices(seq: seq, devices) }
    func projectsSnapshot() -> ProjectsSnapshot { Mk.projects(seq: seq, projects) }
    func pendingSecrets() -> [SecretRequestInfo] {
        secretRequests.filter { $0.state == .pending }
            .sorted { ($0.createdAtMs, $0.id) > ($1.createdAtMs, $1.id) }
    }

    /// Moves the service on by one event; returns it with its number.
    func publish(_ make: @Sendable (UInt64) -> Event) -> (Event, UInt64) {
        seq += 1
        return (make(seq), seq)
    }

    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        calls.append(request)
        if holding {
            await withCheckedContinuation { held.append($0) }
        }
        if let error = failing[request.method] { throw error }
        switch request {
        case .statusOverview:
            return encode(Overview(serviceVersion: "0.0.4", heavy: queueSnapshot(),
                                   claims: claimsSnapshot(), secrets: secretsStatus()))
        case .devicesList:
            return encode(devicesSnapshot())
        case .projectsSnapshot:
            return encode(projectsSnapshot())
        case .requestsSnapshot:
            return encode(Mk.requests(seq: seq, secrets: pendingSecrets()))
        case .serviceStatus:
            return encode(Mk.service(instance: instance))
        case .queueHistory(let query):
            return encode(Array(queueHistory.prefix(Int(query.limit))))
        case .claimsHistory(let query):
            return encode(Array(claimHistory.prefix(Int(query.limit))))
        case .devicesInstalls(_, let query):
            return encode(Array(installs.prefix(Int(query.limit))))
        case .secretList(let project, _):
            if gateSecretLists {
                let result = await withCheckedContinuation { secretListWaiters.append($0) }
                return encode(try result.get())
            }
            guard let list = secretLists[project.name] else { throw .notConnected }
            return encode(list)
        default:
            throw .transport("the fake service does not answer \(request.method.rawValue)")
        }
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> SensitiveData {
        throw .transport("the fake service has no sensitive answers")
    }

    private func encode<T: Encodable>(_ value: T) -> Data {
        // swiftlint:disable:next force_try
        try! JSONEncoder().encode(value)
    }
}

/// Records what the store posts.
@MainActor
final class FakeNotifier: Notifying {
    private(set) var posted: [AppNotification] = []
    func post(_ notification: AppNotification) { posted.append(notification) }
}

/// Branches from a table, by checkout path.
struct FakeGitHead: GitHeadReading {
    var branches: [String: String]
    func branch(at checkout: URL) -> String? { branches[checkout.path] }
}

/// Waits (yielding) until `condition` holds, failing after `seconds` (P5-L5: no test hangs).
@MainActor
func storeSoon(_ what: String, seconds: Double = 10,
                _ condition: @MainActor () async -> Bool) async throws {
    let deadline = Date().addingTimeInterval(seconds)
    while !(await condition()) {
        if Date() > deadline { throw StoreSoonTimeout(what: what) }
        try await Task.sleep(for: .milliseconds(1))
    }
}

struct StoreSoonTimeout: Error, CustomStringConvertible {
    let what: String
    var description: String { "timed out waiting for \(what)" }
}
