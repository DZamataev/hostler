import Foundation
@testable import HostlerKit

/// A scripted service for Task 11's window actions: the queue's methods, the secrets and
/// accounts ones, the project approval and the two gated ones (which need a live grant, as the
/// service does). Every request is recorded, and a method can be made to fail.
actor FakeWindowTransport: HostlerTransport {
    let clock: FakeClock
    let grants: GrantBox

    /// A method's answer fails with this error until it is cleared.
    var failing: [HostlerMethod: HostlerCallError] = [:]
    /// The stop's answer (a task that had ended answers `stopped: false`).
    var stopOutcome: QueueStopOutcome?
    /// `account.list`'s answer for a project name.
    var accounts: [String: [AccountInfo]] = [:]
    /// While true, every `account.list` waits until the test answers it (`answerAccountRead`).
    var gateAccounts = false
    /// `secrets.status`'s answer (the preserved files).
    var preserved: [String] = []

    private var accountWaiters: [CheckedContinuation<Result<[AccountInfo], HostlerCallError>,
                                                       Never>] = []

    /// How many `account.list` calls wait for an answer, in arrival order.
    var waitingAccountReads: Int { accountWaiters.count }

    /// Answers the `index`-th waiting `account.list` (in arrival order).
    func answerAccountRead(_ index: Int, with result: Result<[AccountInfo], HostlerCallError>) {
        accountWaiters.remove(at: index).resume(returning: result)
    }

    private(set) var calls: [Request] = []

    init(clock: FakeClock, grants: GrantBox = GrantBox()) {
        self.clock = clock
        self.grants = grants
    }

    func fail(_ method: HostlerMethod, with error: HostlerCallError) {
        failing[method] = error
    }

    func set(_ change: @Sendable (isolated FakeWindowTransport) -> Void) { change(self) }

    var methods: [HostlerMethod] { calls.map(\.method) }
    func count(_ method: HostlerMethod) -> Int { calls.filter { $0.method == method }.count }

    /// The one request of `method`.
    func request(_ method: HostlerMethod) -> Request? {
        calls.first { $0.method == method }
    }

    private var nowMs: Int64 {
        Int64((clock.now().timeIntervalSince1970 * 1000).rounded())
    }

    /// The service's gate: a live grant on this connection, or `proof_required`.
    private func admit(_ method: HostlerMethod) throws(HostlerCallError) {
        guard let grant = grants.grant, nowMs < grant.expiresAtMs else {
            throw .service(Errors.proofRequired)
        }
    }

    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        calls.append(request)
        if let error = failing[request.method] { throw error }
        switch request {
        case .secretsProve(let prove):
            guard prove.password == SecretText("correct horse") else {
                grants.grant = nil
                throw .service(Errors.unlockFailed)
            }
            let grant = ProofGrant(expiresAtMs: nowMs + 120_000)
            grants.grant = grant
            return encode(grant)
        case .heavyWithdraw:
            return encode(EmptyAnswer())
        case .heavyStop(let id, let confirmation):
            try admit(.heavyStop)
            guard confirmation == id else { throw .service(Errors.badRequest) }
            return encode(stopOutcome ?? QueueStopOutcome(id: id, stopped: true, pids: [4242]))
        case .queueSetCapacity:
            return encode(EmptyAnswer())
        case .secretsRemovePreserved(let path):
            try admit(.secretsRemovePreserved)
            preserved.removeAll { $0 == path }
            return encode(SecretsStatus(state: .unlocked, database: "/db/hostler.kdbx",
                                        pendingRequests: 0, preservedFiles: preserved,
                                        asOfSeq: 1))
        case .secretSet(let set):
            return encode(SetOutcome(name: set.name, answeredRequests: []))
        case .secretRemove:
            return encode(Removed(removed: true))
        case .secretShare(let share):
            return encode(ShareOutcome(name: share.name, from: "app", to: share.toProjectId))
        case .secretUnshare:
            return encode(Unshared(unshared: true))
        case .accountList(let project):
            if gateAccounts {
                let result = await withCheckedContinuation { accountWaiters.append($0) }
                return encode(try result.get())
            }
            return encode(accounts[project.name] ?? [])
        case .accountSet(let set):
            return encode(AccountFieldSet(name: set.name, field: set.field))
        case .accountRemove:
            return encode(Removed(removed: true))
        case .projectApprove(let ref):
            return encode(ProjectInfo(id: "p-1", name: ref.name, commonDir: ref.commonDir,
                                      approved: true, createdAtMs: 1))
        default:
            throw .transport("the fake window service does not answer \(request.method.rawValue)")
        }
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        throw .transport("the fake window service has no sensitive answers")
    }

    private func encode<T: Encodable>(_ value: T) -> Data {
        // swiftlint:disable:next force_try
        try! JSONEncoder().encode(value)
    }
}

/// A body the fake answers for calls whose answer the tests ignore.
private struct EmptyAnswer: Codable, Sendable {}

extension Errors {
    static let badRequest = HostlerError(code: .badRequest, message: "type the task's id to confirm",
                                         details: .object([:]), recovery: nil)
}

/// A window wired to a fake service the way `AppModel` wires the real one: one connection's
/// client, the connection's grant, and one gate.
@MainActor
struct WindowFixture {
    let clock = FakeClock(Date(timeIntervalSince1970: 1_800_000_000))
    let grants = GrantBox()
    let transport: FakeWindowTransport
    let client: HostlerClient
    let log = FakeValuesLog()
    let limit: ProofLimit
    let gate: PersonGate

    init() {
        transport = FakeWindowTransport(clock: clock, grants: grants)
        client = HostlerClient(transport: transport)
        limit = ProofLimit(clock: clock)
        let grants = grants
        gate = PersonGate(client: client, clock: clock, limit: limit,
                          grant: { grants.grant }, log: log)
    }

    var queue: QueueActions { QueueActions(client: client, gate: gate) }

    func secrets(preservedFiles: [String] = []) -> SecretsActions {
        SecretsActions(client: client, gate: gate, preservedFiles: preservedFiles)
    }

    var projects: ProjectsActions { ProjectsActions(client: client) }

    /// The gate is asking: types `password` and submits it.
    func answer(_ password: String) async throws {
        try await storeSoon("the gate asks") { gate.phase == .asking }
        gate.password.text = password
        gate.submit()
    }

    /// A live grant, as a successful unlock or prove leaves one (`ConnectionController.grant`).
    func grantLive() {
        grants.grant = ProofGrant(
            expiresAtMs: Int64((clock.now().timeIntervalSince1970 * 1000).rounded()) + 120_000)
    }
}

// MARK: - The values the tests build

extension Mk {
    /// A waiting or running heavy entry of the queue's fake.
    static func queueEntry(_ id: String, ticket: UInt64, state: HeavyState,
                           command: String = "make test", kind: String = "build",
                           workspace: String? = "/work/app", enqueuedAt: Int64 = now - 60_000,
                           grantedAt: Int64? = nil,
                           source: EntrySource = .hostler) -> HeavyEntry {
        HeavyEntry(id: id, ticket: ticket, kind: kind, command: command, cwd: workspace ?? "/tmp",
                   workspaceRoot: workspace, holder: pid, state: state, enqueuedAtMs: enqueuedAt,
                   grantedAtMs: grantedAt, source: source)
    }

    static func historyItem(_ id: String, ticket: UInt64 = 1, ranMs: Int64 = 120_000,
                            finishedAt: Int64, outcome: HeavyOutcome = .ok,
                            exitCode: Int32? = 0, command: String = "make test",
                            workspace: String? = "/work/app") -> HeavyHistoryItem {
        HeavyHistoryItem(id: id, ticket: ticket, kind: "build", command: command,
                         cwd: workspace ?? "/tmp", workspaceRoot: workspace,
                         enqueuedAtMs: finishedAt - ranMs - 5_000,
                         grantedAtMs: finishedAt - ranMs, finishedAtMs: finishedAt,
                         exitCode: exitCode, outcome: outcome)
    }

    static func projectView(_ id: String, name: String = "app", approved: Bool = true,
                            commonDir: String = "/work/app/.git",
                            workspaces: [WorkspaceInfo] = [],
                            declared: ProjectDecl? = nil) -> ProjectView {
        ProjectView(
            info: ProjectInfo(id: id, name: name, commonDir: commonDir, approved: approved,
                              createdAtMs: now),
            workspaces: workspaces, declared: declared)
    }

    static func secretInfo(_ name: String, purpose: String? = nil, declared: Bool = true,
                           state: SecretState = .missing,
                           source: SecretSource? = nil) -> SecretInfo {
        SecretInfo(name: name, purpose: purpose, declared: declared, state: state, source: source)
    }

    static func account(_ name: String, fields: [String], holder: ClaimSummary? = nil)
        -> AccountInfo
    {
        AccountInfo(name: name, fields: fields, holder: holder)
    }

    static func claimSummary(_ resource: String, workspace: String = "/work/app",
                             pid: UInt32? = 99) -> ClaimSummary {
        ClaimSummary(id: "c-\(resource)", resource: resource, kind: .hold,
                     workspacePath: workspace, pid: pid, sinceMs: now, linked: 0)
    }
}
