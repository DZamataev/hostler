import Foundation
import Observation
import OSLog

/// How a gated action ended.
public enum GateResult<Value: Sendable>: Sendable {
    case done(Value)
    /// The person closed the sheet; nothing was sent after the proof, if any.
    case cancelled
    case failed(HostlerAppError)
}

/// Where the values features log (the reference app's `Log`, behind a protocol so a test can read
/// every line). The texts are `StaticString`s: nothing typed, revealed or answered by the
/// service can be logged through it, only a fixed event and an error code.
@MainActor
public protocol ValuesLogging: AnyObject {
    func event(_ text: StaticString)
    func failure(_ text: StaticString, code: ErrorCode?)
}

/// The app's `ValuesLogging`: `Log.ui`.
@MainActor
public final class OSValuesLog: ValuesLogging {
    public init() {}

    public func event(_ text: StaticString) {
        Log.ui.info("\(text.description, privacy: .public)")
    }

    public func failure(_ text: StaticString, code: ErrorCode?) {
        Log.ui.error("\(text.description, privacy: .public) \(code?.rawValue ?? "-", privacy: .public)")
    }
}

/// The service-wide limit on password checks as the app sees it (spec §8.5): after a
/// `proof_limited` answer, nothing is sent before the wait ends, and the text counts down.
/// One instance is shared by the unlock form and the person gate, because the service's limit
/// covers both.
@MainActor
@Observable
public final class ProofLimit {
    /// When the service takes the next password check; `nil` when nothing waits.
    public private(set) var retryAt: Date?
    @ObservationIgnored private let clock: any WallClock

    public init(clock: any WallClock) {
        self.clock = clock
    }

    public var isWaiting: Bool { secondsLeft != nil }

    /// Whole seconds until the next check, rounded up; `nil` when nothing waits.
    public var secondsLeft: Int? {
        guard let retryAt else { return nil }
        let left = retryAt.timeIntervalSince(clock.now())
        guard left > 0 else { return nil }
        return max(1, Int(left.rounded(.up)))
    }

    /// The text spec §8.5 gives the app (R2-1), counting down; `nil` when nothing waits.
    public var waitText: String? {
        secondsLeft.map(Self.waitText(seconds:))
    }

    public static func waitText(seconds: Int) -> String {
        String(localized: "Too many wrong passwords; next try in \(seconds) s. Another process of your user may be guessing.")
    }

    /// Records the wait of a `proof_limited` answer (`details.retry_after_ms`); `true` when
    /// `error` was one.
    @discardableResult
    public func record(_ error: HostlerCallError) -> Bool {
        guard case .service(let service) = error, service.code == .proofLimited else { return false }
        let ms: Double
        switch service.details["retry_after_ms"] {
        case .int(let value)?: ms = Double(value)
        case .uint(let value)?: ms = Double(value)
        case .double(let value)?: ms = value
        default: ms = 1_000
        }
        retryAt = clock.now().addingTimeInterval(max(ms, 1) / 1000)
        return true
    }
}

/// The person gate (spec §8.5, §9.6): one controller for every gated action — reveal, the
/// account answer and decline, the preserved-file delete, the heavy stop.
///
/// `perform` runs the action at once while the connection holds a live grant (the last prove
/// or unlock on it, less a 2 s margin). Otherwise it asks for the master password (the sheet
/// reads `phase`, `password`, `message` and `limit`), proves it with `secrets.prove`, then runs
/// the action. A `proof_required` from the action (a reconnect or a lock voided the grant)
/// gives the sheet once more, then the error. The password field is cleared on every outcome.
///
/// This is a proof of intent, not authentication of the app (spec §8.2); the texts say no more.
@MainActor
@Observable
public final class PersonGate {
    public enum Phase: Equatable, Sendable {
        case idle
        /// The action is running.
        case running
        /// The sheet is shown and waits for the password.
        case asking
        /// The typed password is being checked (Argon2, about 1 s).
        case proving
    }

    /// Who shows the sheet: closing that surface cancels the request.
    public enum Presenter: Equatable, Sendable {
        case popover
        case window
    }

    /// The sheet's explanation (spec §8.2, §8.5): it says what the proof is and what it is not.
    public static let explanation = String(localized:
        "This confirms it is you. It does not stop other programs of your user from using hostler's socket.")

    /// What the sheet says after a wrong master password.
    public static let wrongPassword = String(localized: "Wrong password — try again.")

    /// A grant is used only while more than this is left of it, so an action never races its
    /// expiry on the way to the service.
    public static let grantMarginMs: Int64 = 2_000

    public private(set) var phase: Phase = .idle
    /// The master password field of the sheet.
    public let password = SecretEntry()
    /// What the sheet is for ("Reveal API_KEY").
    public private(set) var purpose: String?
    public private(set) var presenter: Presenter?
    /// The last failed proof's message and recovery (a wrong password); `nil` otherwise.
    public private(set) var message: String?
    public private(set) var recovery: String?
    public let limit: ProofLimit

    @ObservationIgnored private let client: HostlerClient
    @ObservationIgnored private let clock: any WallClock
    @ObservationIgnored private let grant: @MainActor () -> ProofGrant?
    @ObservationIgnored private let log: any ValuesLogging
    @ObservationIgnored private var waiter: CheckedContinuation<Bool, Never>?

    /// - Parameter grant: the connection's grant (`ConnectionController.grant`).
    public init(client: HostlerClient, clock: any WallClock, limit: ProofLimit,
                grant: @escaping @MainActor () -> ProofGrant?, log: any ValuesLogging) {
        self.client = client
        self.clock = clock
        self.limit = limit
        self.grant = grant
        self.log = log
    }

    public var isAsking: Bool { phase == .asking || phase == .proving }

    /// The connection holds a grant with more than the margin left.
    public var hasLiveGrant: Bool {
        guard let grant = grant() else { return false }
        let nowMs = Int64((clock.now().timeIntervalSince1970 * 1000).rounded(.down))
        return grant.expiresAtMs - Self.grantMarginMs > nowMs
    }

    /// Runs `action` behind the gate (see the type's comment).
    public func perform<Value: Sendable>(
        _ purpose: String, from presenter: Presenter = .window,
        _ action: @escaping @Sendable (HostlerClient) async throws(HostlerCallError) -> Value
    ) async -> GateResult<Value> {
        guard phase == .idle else { return .failed(.gateBusy) }
        self.purpose = purpose
        self.presenter = presenter
        defer { finish() }
        var needsProof = !hasLiveGrant
        var reprovesLeft = 1
        while true {
            if needsProof {
                guard await askForProof() else {
                    log.event("gate: cancelled")
                    return .cancelled
                }
            }
            phase = .running
            do {
                let value = try await action(client)
                log.event("gate: action done")
                return .done(value)
            } catch {
                if case .service(let service) = error, service.code == .proofRequired,
                   reprovesLeft > 0 {
                    log.failure("gate: the grant was refused; asking again", code: service.code)
                    reprovesLeft -= 1
                    needsProof = true
                    continue
                }
                log.failure("gate: action failed", code: Self.code(error))
                return .failed(HostlerAppError(call: error))
            }
        }
    }

    /// The sheet's Confirm: proves the typed password. Nothing is sent while the limit's wait
    /// runs or the field is empty; the field is cleared either way.
    public func submit() {
        guard phase == .asking, waiter != nil else {
            password.clear()
            return
        }
        guard !limit.isWaiting else {
            password.clear()
            return
        }
        guard let typed = password.take() else { return }
        phase = .proving
        message = nil
        recovery = nil
        let client = client
        Task { [weak self] in
            let result: Result<ProofGrant, HostlerCallError>
            do throws(HostlerCallError) {
                result = .success(try await client.secretsProve(Prove(password: typed)))
            } catch {
                result = .failure(error)
            }
            self?.proved(result)
        }
    }

    /// The sheet's Cancel.
    public func cancel() {
        password.clear()
        guard let waiter else { return }
        self.waiter = nil
        waiter.resume(returning: false)
    }

    /// The surface `presenter` closed: a request it shows is cancelled; others stay.
    public func dismissed(_ presenter: Presenter) {
        guard self.presenter == presenter else { return }
        cancel()
    }

    // MARK: - Private

    private func askForProof() async -> Bool {
        phase = .asking
        log.event("gate: asking for the master password")
        return await withCheckedContinuation { waiter = $0 }
    }

    private func proved(_ result: Result<ProofGrant, HostlerCallError>) {
        guard phase == .proving else { return }
        switch result {
        case .success:
            log.event("gate: proved")
            message = nil
            recovery = nil
            let waiter = waiter
            self.waiter = nil
            phase = .running
            waiter?.resume(returning: true)
        case .failure(let error):
            log.failure("gate: prove failed", code: Self.code(error))
            phase = .asking
            if limit.record(error) {
                // The countdown shows instead.
            } else if case .service(let service) = error, service.code == .unlockFailed {
                // The gate's own words: the service's recovery names the CLI's unlock, which is
                // not what this sheet asks for (review t9-r1 R1-4).
                message = Self.wrongPassword
                recovery = nil
            } else {
                let shown = HostlerAppError(call: error)
                message = shown.errorDescription
                recovery = shown.recoverySuggestion
            }
        }
    }

    private func finish() {
        phase = .idle
        password.clear()
        purpose = nil
        presenter = nil
        message = nil
        recovery = nil
        waiter = nil
    }

    static func code(_ error: HostlerCallError) -> ErrorCode? {
        if case .service(let service) = error { return service.code }
        return nil
    }
}
