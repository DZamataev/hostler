import Foundation
import Observation

/// One pending request as a card (spec §9.2 item 4): a secret's or an account's.
public struct RequestCard: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case secret
        case account
    }

    /// The request's id.
    public var id: String
    public var kind: Kind
    public var projectId: String
    public var projectName: String
    /// The secret's or the account's name.
    public var name: String
    /// The project's declared purpose for it (`hostler.toml`), when declared.
    public var purpose: String?
    /// The agent's reason.
    public var reason: String
    public var requesterPid: UInt32
    public var createdAtMs: Int64
    /// The values the card asks for: `["value"]` for a secret, the requested fields for an
    /// account. One `SecureField` each.
    public var fields: [String]

    public var isAccount: Bool { kind == .account }

    public init(id: String, kind: Kind, projectId: String, projectName: String, name: String,
                purpose: String?, reason: String, requesterPid: UInt32, createdAtMs: Int64,
                fields: [String]) {
        self.id = id
        self.kind = kind
        self.projectId = projectId
        self.projectName = projectName
        self.name = name
        self.purpose = purpose
        self.reason = reason
        self.requesterPid = requesterPid
        self.createdAtMs = createdAtMs
        self.fields = fields
    }

    /// The field name of a secret card's one value.
    public static let secretField = "value"
}

/// The request cards' work (spec §8.2, §8.4, §9.2; plan Task 9):
/// - a secret card's Store is `secret.set` with that request's project and name (it answers
///   the request); its Decline is `secret.decline { id }`;
/// - an account card's Store sends every requested field in one `account.answer`, and its
///   Decline is `account.decline`; both are gated, so they go through `PersonGate`.
///
/// The typed values live in `SecretEntry`s per card and field. They are cleared on every
/// outcome (the values leave the fields when the call is sent), when a request stops being
/// pending (`prune`) and when the surface closes (`clearAll`).
@MainActor
@Observable
public final class RequestsController {
    /// A Store or Decline is running, by request id.
    public private(set) var working: Set<String> = []
    /// The last failure, by request id.
    public private(set) var errors: [String: HostlerAppError] = [:]

    /// Created on first use by a view, so not observed: each `SecretEntry` is observable itself.
    @ObservationIgnored private var entries: [String: [String: SecretEntry]] = [:]
    @ObservationIgnored private let client: HostlerClient
    @ObservationIgnored private let gate: PersonGate
    @ObservationIgnored private let log: any ValuesLogging

    public init(client: HostlerClient, gate: PersonGate, log: any ValuesLogging) {
        self.client = client
        self.gate = gate
        self.log = log
    }

    /// The pending secret and account requests as cards, newest first (pure).
    public static func cards(_ model: StoreModel) -> [RequestCard] {
        let projects = model.projects?.projects ?? []
        func declared(_ projectId: String) -> ProjectDecl? {
            projects.first { $0.info.id == projectId }?.declared
        }
        let secrets = model.pendingSecretRequests.map { request in
            RequestCard(
                id: request.id, kind: .secret, projectId: request.projectId,
                projectName: request.projectName, name: request.name,
                purpose: declared(request.projectId)?.secrets.first { $0.name == request.name }?.purpose,
                reason: request.reason, requesterPid: request.requesterPid,
                createdAtMs: request.createdAtMs, fields: [RequestCard.secretField])
        }
        let accounts = model.pendingAccountRequests.map { request in
            RequestCard(
                id: request.id, kind: .account, projectId: request.projectId,
                projectName: request.projectName, name: request.name,
                purpose: declared(request.projectId)?.accounts.first { $0.name == request.name }?.purpose,
                reason: request.reason, requesterPid: request.requesterPid,
                createdAtMs: request.createdAtMs, fields: request.fields)
        }
        return (secrets + accounts).sorted { ($0.createdAtMs, $0.id) > ($1.createdAtMs, $1.id) }
    }

    /// The entry of `field` on `card` (created on first use).
    public func entry(_ card: RequestCard, field: String) -> SecretEntry {
        if let existing = entries[card.id]?[field] { return existing }
        let created = SecretEntry()
        entries[card.id, default: [:]][field] = created
        return created
    }

    /// Every field of `card` holds something.
    public func canStore(_ card: RequestCard) -> Bool {
        card.fields.allSatisfy { !entry(card, field: $0).isEmpty }
    }

    /// Stores the card's values. `true` when the service took them.
    public func store(_ card: RequestCard, projects: ProjectsSnapshot?,
                      from presenter: PersonGate.Presenter = .popover) async -> Bool {
        guard !working.contains(card.id) else { return false }
        errors[card.id] = nil
        if let empty = card.fields.first(where: { entry(card, field: $0).isEmpty }) {
            clear(card)
            errors[card.id] = .emptyValue(field: card.kind == .secret ? card.name : empty)
            return false
        }
        guard let view = projects?.projects.first(where: { $0.info.id == card.projectId }) else {
            clear(card)
            errors[card.id] = .projectUnknown(name: card.projectName)
            return false
        }
        let project = ProjectRef(commonDir: view.info.commonDir, name: view.info.name)
        let values = card.fields.compactMap { field in
            entry(card, field: field).take().map { ImportEntry(name: field, value: $0) }
        }
        working.insert(card.id)
        defer {
            working.remove(card.id)
            clear(card)
        }
        switch card.kind {
        case .secret:
            log.event("request: storing a secret")
            guard let value = values.first?.value else { return false }
            do throws(HostlerCallError) {
                _ = try await client.secretSet(SecretSet(project: project, name: card.name, value: value))
                log.event("request: secret stored")
                return true
            } catch {
                log.failure("request: storing a secret failed", code: PersonGate.code(error))
                errors[card.id] = HostlerAppError(call: error)
                return false
            }
        case .account:
            log.event("request: answering an account request")
            let answer = AccountAnswer(requestId: card.id, fields: values)
            return finish(card, await gate.perform(String(localized: "Store \(card.name)"), from: presenter) {
                (client: HostlerClient) async throws(HostlerCallError) -> AccountAnswerOutcome in
                try await client.accountAnswer(answer)
            })
        }
    }

    /// Declines the card's request. `true` when the service took it.
    public func decline(_ card: RequestCard,
                        from presenter: PersonGate.Presenter = .popover) async -> Bool {
        guard !working.contains(card.id) else { return false }
        errors[card.id] = nil
        clear(card)
        working.insert(card.id)
        defer { working.remove(card.id) }
        switch card.kind {
        case .secret:
            do throws(HostlerCallError) {
                _ = try await client.secretDecline(Decline(id: card.id))
                log.event("request: secret declined")
                return true
            } catch {
                log.failure("request: declining a secret failed", code: PersonGate.code(error))
                errors[card.id] = HostlerAppError(call: error)
                return false
            }
        case .account:
            let id = card.id
            return finish(card, await gate.perform(String(localized: "Decline \(card.name)"), from: presenter) {
                (client: HostlerClient) async throws(HostlerCallError) -> AccountRequestInfo in
                try await client.accountDecline(id: id)
            })
        }
    }

    /// Drops the fields of every request not in `ids` (no longer pending), cleared.
    public func prune(keeping ids: Set<String>) {
        for (id, fields) in entries where !ids.contains(id) {
            fields.values.forEach { $0.clear() }
            entries[id] = nil
            errors[id] = nil
        }
    }

    /// Clears every field (the surface closed).
    public func clearAll() {
        for fields in entries.values { fields.values.forEach { $0.clear() } }
    }

    // MARK: - Private

    private func clear(_ card: RequestCard) {
        entries[card.id]?.values.forEach { $0.clear() }
    }

    private func finish<Value>(_ card: RequestCard, _ result: GateResult<Value>) -> Bool {
        switch result {
        case .done:
            log.event("request: account request answered")
            return true
        case .cancelled:
            return false
        case .failed(let error):
            errors[card.id] = error
            return false
        }
    }
}
