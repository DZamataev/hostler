import Foundation
@testable import HostlerKit

/// The grant the connection would hold (Task 7's `ConnectionController.grant`): set by a
/// successful prove or unlock, cleared by a failed one, and by a test (a lock, a reconnect).
final class GrantBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: ProofGrant?
    var grant: ProofGrant? {
        get { lock.withLock { value } }
        set { lock.withLock { value = newValue } }
    }
}

/// A scripted service for the values features (Task 9): the master password, the gate's
/// grant (120 s from the fake clock), the limit's wait, scripted errors per method, and the
/// values a reveal answers. Every request is recorded.
actor FakeValuesTransport: HostlerTransport {
    let clock: FakeClock
    let grants: GrantBox

    var masterPassword = "correct horse"
    /// While set, prove and unlock answer `proof_limited` with this wait.
    var limitedMs: UInt64?
    /// Errors the next calls of a method throw, in order (consumed).
    var queuedErrors: [HostlerMethod: [HostlerCallError]] = [:]
    /// Secret values by name, for `secret.reveal`.
    var secretValues: [String: String] = [:]
    /// Account fields by account name, for `account.reveal`.
    var accountFields: [String: [(String, String)]] = [:]
    var lockState: LockState = .locked
    /// While true, `secrets.unlock` waits until `releaseUnlock()`.
    var holdUnlock = false
    private var unlockWaiters: [CheckedContinuation<Void, Never>] = []
    /// While true, `secret.reveal` and `account.reveal` wait until `releaseReveal()`.
    var holdReveal = false
    private var revealWaiters: [CheckedContinuation<Void, Never>] = []

    func releaseReveal() {
        holdReveal = false
        let waiting = revealWaiters
        revealWaiters = []
        waiting.forEach { $0.resume() }
    }

    var revealWaiting: Int { revealWaiters.count }

    private(set) var calls: [Request] = []

    init(clock: FakeClock, grants: GrantBox = GrantBox()) {
        self.clock = clock
        self.grants = grants
    }

    func set(_ change: @Sendable (isolated FakeValuesTransport) -> Void) { change(self) }

    func queue(_ error: HostlerCallError, for method: HostlerMethod) {
        queuedErrors[method, default: []].append(error)
    }

    func releaseUnlock() {
        holdUnlock = false
        let waiting = unlockWaiters
        unlockWaiters = []
        waiting.forEach { $0.resume() }
    }

    var unlockWaiting: Int { unlockWaiters.count }

    func count(_ method: HostlerMethod) -> Int { calls.filter { $0.method == method }.count }
    var methods: [HostlerMethod] { calls.map(\.method) }

    private var grantNow: ProofGrant {
        ProofGrant(expiresAtMs: Int64((clock.now().timeIntervalSince1970 * 1000).rounded()) + 120_000)
    }

    private func checkPassword(_ password: SecretText) throws(HostlerCallError) {
        if let limitedMs {
            grants.grant = nil
            throw .service(Errors.proofLimited(limitedMs))
        }
        guard password == SecretText(masterPassword) else {
            grants.grant = nil
            throw .service(Errors.unlockFailed)
        }
    }

    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        calls.append(request)
        if var errors = queuedErrors[request.method], !errors.isEmpty {
            let error = errors.removeFirst()
            queuedErrors[request.method] = errors
            if request.method == .secretsProve || request.method == .secretsUnlock {
                grants.grant = nil
            }
            throw error
        }
        switch request {
        case .secretsProve(let prove):
            try checkPassword(prove.password)
            let grant = grantNow
            grants.grant = grant
            return encode(grant)
        case .secretsUnlock(let unlock):
            if holdUnlock { await withCheckedContinuation { unlockWaiters.append($0) } }
            if !unlock.create { try checkPassword(unlock.password) }
            lockState = .unlocked
            let grant = grantNow
            grants.grant = grant
            return encode(UnlockOutcome(status: Mk.secrets(seq: 1, state: .unlocked, pending: 0),
                                        grant: grant))
        case .secretsLock:
            lockState = .locked
            grants.grant = nil
            return encode(Mk.secrets(seq: 2, state: .locked, pending: 0))
        case .secretSet(let set):
            return encode(SetOutcome(name: set.name, answeredRequests: ["s-1"]))
        case .secretDecline(let decline):
            return encode(decline.id.map { [Self.secretRequest(id: $0, state: .declined)] } ?? [])
        case .accountAnswer(let answer):
            try admit(.accountAnswer)
            return encode(AccountAnswerOutcome(name: "login", storedFields: answer.fields.map(\.name),
                                               answeredRequests: [answer.requestId]))
        case .accountDecline(let id):
            try admit(.accountDecline)
            return encode(Self.accountRequest(id: id, state: .declined))
        default:
            throw .transport("the fake values service does not answer \(request.method.rawValue)")
        }
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        calls.append(request)
        if var errors = queuedErrors[request.method], !errors.isEmpty {
            let error = errors.removeFirst()
            queuedErrors[request.method] = errors
            throw error
        }
        if holdReveal { await withCheckedContinuation { revealWaiters.append($0) } }
        switch request {
        case .secretReveal(_, let name):
            try admit(.secretReveal)
            guard let value = secretValues[name] else { throw .service(Errors.secretMissing) }
            return SensitiveData(SecretText(encode(MaterializedSecret(name: name,
                                                                       value: SecretText(value)))))
        case .accountReveal(_, let name):
            try admit(.accountReveal)
            let fields = (accountFields[name] ?? []).map {
                MaterializedSecret(name: $0.0, value: SecretText($0.1))
            }
            return SensitiveData(SecretText(encode(MaterializedAccount(name: name, fields: fields))))
        default:
            throw .transport("the fake values service does not answer \(request.method.rawValue)")
        }
    }

    /// The service's gate: a live grant on this connection, or `proof_required`.
    private func admit(_ method: HostlerMethod) throws(HostlerCallError) {
        let nowMs = Int64((clock.now().timeIntervalSince1970 * 1000).rounded())
        guard let grant = grants.grant, nowMs < grant.expiresAtMs else {
            throw .service(Errors.proofRequired)
        }
    }

    private func encode<T: Encodable>(_ value: T) -> Data {
        // swiftlint:disable:next force_try
        try! JSONEncoder().encode(value)
    }

    static func secretRequest(id: String, project: String = "p-1", name: String = "API_KEY",
                              state: RequestState = .pending, createdAtMs: Int64 = 1_000,
                              pid: UInt32 = 4242) -> SecretRequestInfo {
        SecretRequestInfo(id: id, projectId: project, projectName: "app", name: name,
                          reason: "the tests need it", state: state, requesterPid: pid,
                          createdAtMs: createdAtMs)
    }

    static func accountRequest(id: String, project: String = "p-1", name: String = "login",
                               fields: [String] = ["user", "password"],
                               state: RequestState = .pending,
                               createdAtMs: Int64 = 1_000) -> AccountRequestInfo {
        AccountRequestInfo(id: id, projectId: project, projectName: "app", name: name,
                           fields: fields, reason: "sign in", state: state, requesterPid: 4343,
                           createdAtMs: createdAtMs)
    }
}

/// The service's errors as the tests need them (texts as `contract/secrets.rs` writes them).
enum Errors {
    static let unlockFailed = HostlerError(code: .unlockFailed,
                                           message: "wrong master password or key file",
                                           details: .object([:]), recovery: "hostler unlock")
    static let proofRequired = HostlerError(
        code: .proofRequired,
        message: "this needs the master password: a person proves it on this connection first",
        details: .object([:]),
        recovery: "type the master password in the hostler app, or rerun the command at a terminal")
    static let secretMissing = HostlerError(code: .secretMissing, message: "no such secret",
                                            details: .object([:]), recovery: nil)
    static func proofLimited(_ ms: UInt64) -> HostlerError {
        let secs = (ms + 999) / 1000
        return HostlerError(
            code: .proofLimited,
            message: "too many wrong master passwords; next try in \(secs) s. Another process of "
                + "your user may be guessing (the service log names each wrong password's process)",
            details: .object(["retry_after_ms": .uint(ms)]),
            recovery: "wait \(secs) s, then try again")
    }
}

/// Records what the values features log (a `ValuesLogging` fake): the events and the codes.
@MainActor
final class FakeValuesLog: ValuesLogging {
    private(set) var lines: [String] = []
    func event(_ text: StaticString) { lines.append("\(text)") }
    func failure(_ text: StaticString, code: ErrorCode?) {
        lines.append("\(text) \(code?.rawValue ?? "-")")
    }
}

/// A gate wired to a fake service and clock, the way the app wires it to the connection.
@MainActor
struct GateFixture {
    let clock = FakeClock(Date(timeIntervalSince1970: 1_800_000_000))
    let grants = GrantBox()
    let transport: FakeValuesTransport
    let client: HostlerClient
    let log = FakeValuesLog()
    let limit: ProofLimit
    let gate: PersonGate

    init() {
        transport = FakeValuesTransport(clock: clock, grants: grants)
        client = HostlerClient(transport: transport)
        limit = ProofLimit(clock: clock)
        let grants = grants
        gate = PersonGate(client: client, clock: clock, limit: limit,
                          grant: { grants.grant }, log: log)
    }

    /// Waits until the gate asks, then types `password` and submits it.
    func answer(_ password: String) async throws {
        try await storeSoon("the gate asks") { gate.phase == .asking }
        gate.password.text = password
        gate.submit()
    }
}
