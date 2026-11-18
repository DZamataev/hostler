import Foundation
import Observation

// The window's Secrets and Accounts sections (plan Task 11; spec §9.3, §9.6): their pure rows
// and the actions behind them. Values only ever travel as `SecretText`: a view types one into
// a `SecretEntry`, the action takes it (clearing the field), and nothing here turns a value
// into a `String` or logs one.

// MARK: - Secrets

/// One row of the Secrets section: a secret declared by `hostler.toml`, stored in the database,
/// or both (`secret.list` carries them merged).
public struct SecretRow: Identifiable, Equatable, Sendable {
    public var name: String
    public var purpose: String?
    public var isSet: Bool
    public var declared: Bool
    /// "shared from <project>" when another project shares it in.
    public var sharedFrom: String?
    /// The sharing project's id, for `secret.unshare`.
    public var sharedFromProjectId: String?

    public var id: String { name }

    /// "set" or "missing" (spec §9.3).
    public var stateText: String {
        isSet ? String(localized: "set") : String(localized: "missing")
    }
}

/// A project's secrets as rows, in `secret.list`'s order (declared ones first, then stored).
public func secretRows(_ list: SecretList?) -> [SecretRow] {
    (list?.secrets ?? []).map { secret in
        var sharedFrom: String?
        var sharedFromProjectId: String?
        if case .sharedFrom(let projectId, let project)? = secret.source {
            sharedFrom = project
            sharedFromProjectId = projectId
        }
        return SecretRow(name: secret.name, purpose: secret.purpose,
                         isSet: secret.state == .set, declared: secret.declared,
                         sharedFrom: sharedFrom, sharedFromProjectId: sharedFromProjectId)
    }
}

// MARK: - Accounts

/// One field of an account: declared, stored, or both.
public struct AccountFieldRow: Identifiable, Equatable, Sendable {
    public var name: String
    public var isSet: Bool

    public var id: String { name }
}

/// One account of a project: the declaration (`hostler.toml`) and the stored entry
/// (`account.list`) merged. A declared-only account has no fields set; a stored-only one has no
/// purpose.
public struct AccountRow: Identifiable, Equatable, Sendable {
    public var name: String
    public var purpose: String?
    public var declared: Bool
    public var fields: [AccountFieldRow]
    /// Who holds it now (`account.list`'s holder), as a chip in that workspace's color.
    public var holder: ClaimSummary?
    public var holderChip: WorkspaceChip?

    public var id: String { name }

    /// The fields nothing is stored for yet.
    public var missing: [String] { fields.filter { !$0.isSet }.map(\.name) }

    /// "all 2 fields set" / "1 of 2 fields missing" / "no fields".
    public var stateText: String {
        if fields.isEmpty { return String(localized: "no fields") }
        let missing = missing.count
        if missing == 0 {
            return fields.count == 1 ? String(localized: "1 field set")
                : String(localized: "all \(fields.count) fields set")
        }
        return String(localized: "\(missing) of \(fields.count) fields missing")
    }
}

/// A project's accounts as rows: the declared ones in `hostler.toml`'s order, then the stored
/// ones it does not declare; each field is declared-first, then stored-only (pure).
public func accountRows(declared: [DeclaredAccount], stored: [AccountInfo],
                        workspaces: [WorkspaceInfo] = []) -> [AccountRow] {
    var names: [String] = []
    for account in declared where !names.contains(account.name) { names.append(account.name) }
    for account in stored where !names.contains(account.name) { names.append(account.name) }

    return names.compactMap { name in
        let declaration = declared.first { $0.name == name }
        let entry = stored.first { $0.name == name }
        guard declaration != nil || entry != nil else { return nil }
        var fields: [AccountFieldRow] = []
        for field in declaration?.fields ?? [] where !fields.contains(where: { $0.name == field }) {
            fields.append(AccountFieldRow(name: field, isSet: entry?.fields.contains(field) == true))
        }
        for field in entry?.fields ?? [] where !fields.contains(where: { $0.name == field }) {
            fields.append(AccountFieldRow(name: field, isSet: true))
        }
        let holder = entry?.holder
        return AccountRow(
            name: name, purpose: declaration?.purpose, declared: declaration != nil,
            fields: fields, holder: holder,
            holderChip: holder.map {
                WorkspaceChip.of(path: $0.workspacePath, workspaceId: nil, workspaces: workspaces)
            })
    }
}

// MARK: - The actions

/// The Secrets and Accounts sections' actions (plan Task 11; spec §9.6), against the finished
/// service methods:
/// - `secret.set`, `secret.remove`, `secret.share`, `secret.unshare`;
/// - `account.set`, `account.remove` (a field, or the whole account), `account.list`;
/// - `secrets.remove_preserved`, behind the person gate (§8.5): only a path `secrets.status`
///   lists is deleted, and nothing is sent for any other.
///
/// Typed values arrive as a `SecretEntry`; `take()` turns one into a `SecretText` and clears
/// the field. An empty field sends nothing and answers `emptyValue`, as the request cards do.
@MainActor
public struct SecretsActions {
    public var client: HostlerClient
    public var gate: PersonGate
    /// The paths `secrets.status` listed (`preserved_files`): the only ones Delete acts on.
    public var preservedFiles: [String]

    public init(client: HostlerClient, gate: PersonGate, preservedFiles: [String] = []) {
        self.client = client
        self.gate = gate
        self.preservedFiles = preservedFiles
    }

    // MARK: Secrets

    /// `secret.set` for `project`'s secret `name`.
    public func setSecret(_ project: ProjectView, name: String, entry: SecretEntry)
        async -> ActionFailure?
    {
        guard let value = entry.take() else {
            return .app(.emptyValue(field: name))
        }
        do throws(HostlerCallError) {
            _ = try await client.secretSet(SecretSet(project: ProjectRef(project), name: name,
                                                     value: value))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    public func removeSecret(_ project: ProjectView, name: String) async -> ActionFailure? {
        do throws(HostlerCallError) {
            _ = try await client.secretRemove(project: ProjectRef(project), name: name)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `secret.share`: `project`'s secret `name` with the project `toProjectId`.
    public func share(_ project: ProjectView, name: String, toProjectId: String)
        async -> ActionFailure?
    {
        do throws(HostlerCallError) {
            _ = try await client.secretShare(Share(from: ProjectRef(project), name: name,
                                                   toProjectId: toProjectId))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `secret.unshare`: the share of `sourceProject`'s secret `name` to `toProjectId` (undone
    /// from the project that receives it).
    public func unshare(from sourceProject: ProjectView, name: String, toProjectId: String)
        async -> ActionFailure?
    {
        do throws(HostlerCallError) {
            _ = try await client.secretUnshare(Share(from: ProjectRef(sourceProject), name: name,
                                                     toProjectId: toProjectId))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    // MARK: Preserved files

    /// Deletes one preserved `hostler.kdbx.prev-*` file behind the person gate (spec §8.5).
    /// A path `secrets.status` did not list sends nothing.
    public func removePreserved(_ path: String) async -> ActionOutcome<SecretsStatus> {
        guard preservedFiles.contains(path) else {
            return .failed(.app(.unknownPreservedFile(path)))
        }
        let name = (path as NSString).lastPathComponent
        let result = await gate.perform(String(localized: "Delete \(name)"), from: .window) {
            (client: HostlerClient) async throws(HostlerCallError) -> SecretsStatus in
            try await client.secretsRemovePreserved(path: path)
        }
        switch result {
        case .done(let status): return .done(status)
        case .cancelled: return .cancelled
        case .failed(let error): return .failed(.app(error))
        }
    }

    // MARK: Accounts

    /// `account.list`: the stored accounts of `project` (never a value).
    public func accounts(of project: ProjectView) async -> Result<[AccountInfo], ActionFailure> {
        do throws(HostlerCallError) {
            return .success(try await client.accountList(project: ProjectRef(project)))
        } catch {
            return .failure(.app(HostlerAppError(call: error)))
        }
    }

    /// `account.set` with the field a set sheet submitted (fix r1-3): the field it was opened
    /// for, or the name the person typed for an account that has none. Nothing is sent while
    /// that name is empty.
    public func setAccountField(_ project: ProjectView, account: String,
                                sheet: AccountFieldSheet, submitted: String?,
                                entry: SecretEntry) async -> ActionFailure?
    {
        guard let field = sheet.resolvedField(submitted: submitted) else {
            entry.clear()
            return .app(.emptyValue(field: String(localized: "the field")))
        }
        return await setAccountField(project, account: account, field: field, entry: entry)
    }

    /// `account.set`: one field of an account, creating the account when it is new.
    public func setAccountField(_ project: ProjectView, account: String, field: String,
                                entry: SecretEntry) async -> ActionFailure?
    {
        let field = field.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !field.isEmpty else { return .app(.emptyValue(field: String(localized: "the field"))) }
        guard let value = entry.take() else {
            return .app(.emptyValue(field: field))
        }
        do throws(HostlerCallError) {
            _ = try await client.accountSet(AccountSet(project: ProjectRef(project),
                                                       name: account, field: field, value: value))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `account.remove`: one field (`field`), or every field of hostler's with `nil`.
    public func removeAccount(_ project: ProjectView, name: String, field: String? = nil)
        async -> ActionFailure?
    {
        do throws(HostlerCallError) {
            _ = try await client.accountRemove(AccountRemove(project: ProjectRef(project),
                                                             name: name, field: field))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }
}

// MARK: - The per-project account reads

/// What the Accounts section shows of one project's `account.list` read (fix r1-2): nothing
/// until a read starts, the reading line, the list, or the service's failure.
public enum AccountReadState: Equatable, Sendable {
    case reading
    case loaded([AccountInfo])
    case failed(HostlerAppError)

    public var isLoaded: Bool { if case .loaded = self { true } else { false } }

    /// The stored accounts a loaded read carries (none otherwise).
    public var accounts: [AccountInfo] {
        if case .loaded(let accounts) = self { accounts } else { [] }
    }
}

/// What an `account.list` read is bound to (clean read C-B1): the live connection, the
/// service's instance, and the project's identity as the projects snapshot holds it. A read
/// commits only while all of these are still the same, so an answer from before a reconnect,
/// a service restart or a project replaced under the same id is never shown.
public struct AccountReadScope: Equatable, Hashable, Sendable {
    public var connection: ConnectionToken
    public var instance: String?
    public var projectId: String
    public var commonDir: String
    public var name: String

    public init(connection: ConnectionToken, instance: String?, projectId: String,
                commonDir: String, name: String) {
        self.connection = connection
        self.instance = instance
        self.projectId = projectId
        self.commonDir = commonDir
        self.name = name
    }
}

extension HostlerStore {
    /// The scope an `account.list` of `projectId` is read in now; `nil` while there is no live
    /// connection or the snapshot holds no such project.
    public func accountReadScope(of projectId: String?) -> AccountReadScope? {
        guard let projectId, let connection = machine.liveConnection,
              let project = model.projects?.projects.first(where: { $0.info.id == projectId })
        else { return nil }
        return AccountReadScope(connection: connection, instance: model.instance,
                                projectId: projectId, commonDir: project.info.commonDir,
                                name: project.info.name)
    }
}

/// The Accounts section's reads, keyed by project id (fix r1-2). One read per project at a time,
/// and a read's answer is applied only while its project is still the selected one and it is the
/// newest read for that project: a slow answer for the project the person switched away from is
/// discarded, so its rows can never appear under another project or overwrite a newer read.
///
/// The view reads `state(for: selectedProjectId)` only, so selection changes show a reading line
/// rather than the previous project's rows; a row's actions are enabled only when that state is
/// `.loaded` (a read that succeeded).
@MainActor
@Observable
public final class AccountsReader {
    /// The project the section shows; `nil` before the picker resolves one.
    public private(set) var selectedProjectId: String?
    /// Every project's read state, by project id.
    public private(set) var states: [String: AccountReadState] = [:]

    @ObservationIgnored private let client: HostlerClient
    @ObservationIgnored private let scope: @MainActor (String) -> AccountReadScope?
    @ObservationIgnored private var tokens: [String: UInt64] = [:]
    @ObservationIgnored private var nextToken: UInt64 = 0

    /// - Parameter scope: the current scope of a project's read
    ///   (`HostlerStore.accountReadScope(of:)` in the app).
    public init(client: HostlerClient,
                scope: @escaping @MainActor (String) -> AccountReadScope?) {
        self.client = client
        self.scope = scope
    }

    /// The person picked `projectId`: its rows wait for a read of its own.
    public func select(_ projectId: String?) {
        selectedProjectId = projectId
        guard let projectId else { return }
        states[projectId] = .reading
    }

    public func state(for projectId: String?) -> AccountReadState? {
        projectId.flatMap { states[$0] }
    }

    /// Whether `projectId`'s read succeeded: the only state a row's actions are enabled in.
    public func isLoaded(_ projectId: String?) -> Bool {
        state(for: projectId)?.isLoaded == true
    }

    /// `account.list` for `project`. The answer is applied only while `project` is still the
    /// selected project and no newer read of it started meanwhile.
    ///
    /// The read is bound to the scope it starts in (clean read C-B1): with no live scope, or a
    /// `project` that is not the snapshot's project of that id any more, nothing is sent; an
    /// answer commits only while the scope is still the same.
    public func load(_ project: ProjectView) async {
        let projectId = project.info.id
        nextToken += 1
        let token = nextToken
        tokens[projectId] = token
        states[projectId] = .reading
        guard let started = scope(projectId), started.commonDir == project.info.commonDir,
              started.name == project.info.name
        else { return }
        do throws(HostlerCallError) {
            let accounts = try await client.accountList(project: ProjectRef(project))
            commit(.loaded(accounts), of: projectId, read: token, in: started)
        } catch {
            commit(.failed(HostlerAppError(call: error)), of: projectId, read: token, in: started)
        }
    }

    /// Drops a project's read state (its project left the snapshot).
    public func forget(_ projectId: String) {
        states[projectId] = nil
        tokens[projectId] = nil
    }

    private func commit(_ state: AccountReadState, of projectId: String, read token: UInt64,
                        in started: AccountReadScope) {
        guard selectedProjectId == projectId, tokens[projectId] == token,
              scope(projectId) == started
        else { return }
        states[projectId] = state
    }
}

/// The field a set sheet is opened for (fix r1-3): an existing field is locked to itself, and
/// an account with no known field asks the person for the name. The sheet shows its field-name
/// `TextField` exactly when `fields` is empty.
public struct AccountFieldSheet: Equatable, Sendable {
    public var account: String
    /// The fields the sheet offers: `[]` to type a new one.
    public var fields: [String]

    /// - Parameter existingField: the field's name, or `nil` for "a new field".
    public init(account: String, existingField: String?) {
        self.account = account
        self.fields = existingField.map { [$0] } ?? []
    }

    public var needsTypedName: Bool { fields.isEmpty }

    /// The field `account.set` sends: the locked one, or the submitted name trimmed; `nil` when
    /// neither names a field.
    public func resolvedField(submitted: String?) -> String? {
        if let locked = fields.first { return locked }
        let typed = submitted?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return typed.isEmpty ? nil : typed
    }
}

extension ProjectRef {
    /// The reference `secret.*`, `account.*` and `project.approve` send for a project the
    /// projects snapshot carries.
    public init(_ project: ProjectView) {
        self.init(commonDir: project.info.commonDir, name: project.info.name)
    }
}
