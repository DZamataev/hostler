import Foundation
import Observation

/// What a reveal shows: a project's secret or account (spec §9.6; Task 11's Secrets and
/// Accounts sections use it).
public enum RevealSubject: Equatable, Sendable {
    case secret(project: ProjectRef, name: String)
    case account(project: ProjectRef, name: String)

    public var name: String {
        switch self {
        case .secret(_, let name), .account(_, let name): name
        }
    }
}

/// One revealed value: a secret's, or one field of an account. Its value stays a `SecretText`
/// until the view that shows it reads it; its description is `<redacted>`.
public struct RevealedField: Identifiable, Equatable, Sendable, CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    public var name: String
    public var value: SecretText
    /// The reveal it belongs to (`RevealedValue.generation`) and its place in it: the identity
    /// `RevealController.copy` looks the field up by (review t9-r1 R1-2).
    public var generation: UInt64
    public var index: Int
    public var id: String { "\(generation):\(index)" }

    public init(name: String, value: SecretText, generation: UInt64 = 0, index: Int = 0) {
        self.name = name
        self.value = value
        self.generation = generation
        self.index = index
    }

    public var description: String { "\(name)=<redacted>" }
    public var debugDescription: String { description }
    public var customMirror: Mirror { Mirror(self, children: ["name": name], displayStyle: .struct) }
}

/// What is on screen now: one subject, every field of it.
public struct RevealedValue: Equatable, Sendable, CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    public var subject: RevealSubject
    public var fields: [RevealedField]
    /// Every reveal shown gets the next number (1, 2, …) while the app runs.
    public var generation: UInt64

    public var description: String { "\(subject.name): \(fields.count) field(s), <redacted>" }
    public var debugDescription: String { description }
    public var customMirror: Mirror {
        Mirror(self, children: ["subject": subject.name], displayStyle: .struct)
    }
}

/// Reveal (spec §8.5, §9.6; plan Task 9). `secret.reveal` or `account.reveal` through the
/// person gate, one value on screen at a time:
/// - it hides after 30 s, when another value is revealed, on a lock (`lockStateChanged`), and
///   when its view goes away (the view calls `hide()`);
/// - Copy writes it to the pasteboard concealed, and the pasteboard is cleared after 60 s if
///   it still holds it (`ConcealedClipboard`).
///
/// The view hides the value from the accessibility tree and from screen sharing; while it is
/// on screen a program that records the screen can still read it (spec §9.6, the honest limit).
@MainActor
@Observable
public final class RevealController {
    public struct Failure: Equatable, Sendable {
        public var subject: RevealSubject
        public var error: HostlerAppError
    }

    public static let shownFor: Duration = .seconds(30)

    public private(set) var shown: RevealedValue?
    /// The subject a reveal is running for (the gate may be asking).
    public private(set) var revealing: RevealSubject?
    public private(set) var error: Failure?
    /// The name of the field last copied, while its value is shown.
    public private(set) var copied: String?

    @ObservationIgnored private let gate: PersonGate
    @ObservationIgnored private let clipboard: ConcealedClipboard
    @ObservationIgnored private let sleeper: any Sleeping
    @ObservationIgnored private let log: any ValuesLogging
    @ObservationIgnored private var hideTimer: Task<Void, Never>?
    @ObservationIgnored private var lastGeneration: UInt64 = 0
    /// The current reveal operation (clean read C-B2). Each reveal takes the next number, and
    /// `hide()` (also a lock and a dismissal) moves it on, so an answer that arrives for an
    /// operation that is no longer current is dropped, never shown.
    @ObservationIgnored private var operation: UInt64 = 0
    /// The surface the current reveal was asked from (Task 13 F-1).
    @ObservationIgnored private var presenter: PersonGate.Presenter?
    /// Answers dropped that way (tests, diagnostics).
    @ObservationIgnored public private(set) var lateAnswersDropped = 0

    public init(gate: PersonGate, clipboard: ConcealedClipboard, sleeper: any Sleeping,
                log: any ValuesLogging) {
        self.gate = gate
        self.clipboard = clipboard
        self.sleeper = sleeper
        self.log = log
    }

    /// Reveals `subject` (asking for the master password when no grant is live).
    public func reveal(_ subject: RevealSubject, from presenter: PersonGate.Presenter = .window) async {
        guard revealing == nil else { return }
        hide()
        operation += 1
        let current = operation
        error = nil
        revealing = subject
        self.presenter = presenter
        defer { revealing = nil }
        log.event("reveal: asked")
        let result: GateResult<[RevealedField]>
        switch subject {
        case .secret(let project, let name):
            result = await gate.perform(String(localized: "Reveal \(name)"), from: presenter) {
                (client: HostlerClient) async throws(HostlerCallError) -> [RevealedField] in
                let secret = try await client.secretReveal(project: project, name: name)
                return [RevealedField(name: secret.name, value: secret.value)]
            }
        case .account(let project, let name):
            result = await gate.perform(String(localized: "Reveal \(name)"), from: presenter) {
                (client: HostlerClient) async throws(HostlerCallError) -> [RevealedField] in
                let account = try await client.accountReveal(project: project, name: name)
                return account.fields.map { RevealedField(name: $0.name, value: $0.value) }
            }
        }
        // The surface went away, the value was hidden or the database locked while the answer
        // was on its way: drop it; its `SecretText`s go with `result` (zeroed in `deinit`).
        guard current == operation, revealing == subject else {
            if case .done = result { lateAnswersDropped += 1 }
            log.event("reveal: answer dropped (no longer current)")
            return
        }
        switch result {
        case .done(let fields):
            log.event("reveal: shown")
            lastGeneration += 1
            let generation = lastGeneration
            show(RevealedValue(
                subject: subject,
                fields: fields.enumerated().map { index, field in
                    RevealedField(name: field.name, value: field.value, generation: generation,
                                  index: index)
                },
                generation: generation))
        case .cancelled:
            break
        case .failed(let failure):
            log.event("reveal: failed")
            error = Failure(subject: subject, error: failure)
        }
    }

    /// Hides the value now (its `SecretText`s are zeroed once nothing else holds them), and
    /// voids a reveal still on its way.
    public func hide() {
        operation += 1
        hideTimer?.cancel()
        hideTimer = nil
        if shown != nil { log.event("reveal: hidden") }
        shown = nil
        copied = nil
    }

    /// Copies one shown field (concealed; cleared after 60 s if unchanged). The field is looked
    /// up in what is on screen now by its identity (generation and place), and the value copied
    /// is the one on screen: a field kept from an earlier reveal copies nothing, and the
    /// caller's `value` is never used (review t9-r1 R1-2).
    public func copy(_ field: RevealedField) {
        guard let shown, shown.generation == field.generation,
              shown.fields.indices.contains(field.index)
        else { return }
        let current = shown.fields[field.index]
        guard current.name == field.name else { return }
        clipboard.copy(current.value)
        copied = current.name
        log.event("reveal: copied")
    }

    /// The surface showing `subject` went away (the row scrolled out, the window closed): its
    /// value hides, and a reveal of it still on its way is voided.
    public func dismissed(_ subject: RevealSubject) {
        if shown?.subject == subject || revealing == subject { hide() }
    }

    /// The surface `presenter` closed (the popover's close path, a window's): what it revealed
    /// hides, and a reveal it asked for that is still on its way is voided (Task 13 F-1).
    public func dismissed(presenter: PersonGate.Presenter) {
        guard self.presenter == presenter, shown != nil || revealing != nil else { return }
        hide()
    }

    /// The database's state from the store: anything but unlocked hides the value at once.
    public func lockStateChanged(_ state: LockState?) {
        if state != .unlocked { hide() }
    }

    private func show(_ value: RevealedValue) {
        shown = value
        let sleeper = sleeper
        hideTimer = Task { [weak self] in
            do { try await sleeper.sleep(for: Self.shownFor) } catch { return }
            self?.hide()
        }
    }
}
