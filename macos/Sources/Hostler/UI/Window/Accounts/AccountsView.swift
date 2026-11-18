import HostlerKit
import SwiftUI

/// The accounts of one project (spec §9.3, §9.6): the accounts `hostler.toml` declares and the
/// ones stored in the database, each with its purpose, its fields (set or missing) and its
/// holder. Set a field, reveal every field, remove a field or the account, and answer this
/// project's pending account requests.
struct AccountsView: View {
    let store: HostlerStore
    let actions: SecretsActions
    let requests: RequestsController
    let reveal: RevealController

    @State private var projectID: String?
    /// Each project's `account.list` read, keyed by project id (fix r1-2): a project's rows and
    /// actions wait for a read of that project, and a late answer for another one is dropped.
    @State private var reader: AccountsReader
    @State private var setTarget: AccountSetTarget?
    @State private var removeTarget: AccountRemoveTarget?
    @State private var failure: ActionFailure?

    init(store: HostlerStore, actions: SecretsActions, requests: RequestsController,
         reveal: RevealController) {
        self.store = store
        self.actions = actions
        self.requests = requests
        self.reveal = reveal
        _reader = State(initialValue: AccountsReader(
            client: actions.client, scope: { [weak store] in store?.accountReadScope(of: $0) }))
    }

    /// The projects snapshot this section reads (fix r1-1): "no project is approved" only means
    /// a loaded, empty answer.
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.projects != nil, syncState: store.syncState)
    }

    /// The selected project's read: `nil` before its first read and after a switch.
    private var readState: AccountReadState? {
        guard let project else { return nil }
        return reader.state(for: project.info.id)
    }

    /// A row's actions are enabled only when the selected project's read succeeded.
    private var rowsAreLoaded: Bool {
        guard let project else { return false }
        return reader.isLoaded(project.info.id)
    }

    private var projects: [ProjectView] { store.model.projects?.projects ?? [] }

    /// The picker offers the approved projects: an unapproved one may not be read
    /// (`project_not_approved`), so it is approved in Projects first.
    private var approved: [ProjectView] { projects.filter(\.info.approved) }

    private var project: ProjectView? {
        approved.first { $0.info.id == projectID } ?? approved.first
    }

    private var workspaces: [WorkspaceInfo] {
        projects.flatMap(\.workspaces)
    }

    private var rows: [AccountRow] {
        guard let project, rowsAreLoaded else { return [] }
        return accountRows(declared: project.declared?.accounts ?? [],
                           stored: readState?.accounts ?? [], workspaces: workspaces)
    }

    private var cards: [RequestCard] {
        guard let project else { return [] }
        return RequestsController.cards(store.model)
            .filter { $0.kind == .account && $0.projectId == project.info.id }
    }

    var body: some View {
        Group {
            if snapshotState.isLoaded {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        header
                        if approved.isEmpty {
                            emptyState(String(localized: "No project is approved for secrets yet. Approve one in Projects."))
                        } else if let project {
                            if !cards.isEmpty { requestsSection() }
                            accountsSection(project)
                        }
                    }
                    .padding(16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ReadingView(state: snapshotState)
            }
        }
        .navigationTitle("Accounts")
        .navigationSubtitle(snapshotState.isLoaded ? (project?.info.name ?? "") : "")
        .accessibilityIdentifier("accounts.view")
        // Keyed on the read's whole scope (clean read C-B1): a reconnect, a new service
        // instance or a project replaced under the same id reads again.
        .task(id: store.accountReadScope(of: project?.info.id)) { await load() }
        // A hold or a release changes the holder a row shows; `account.list` is the only
        // source, so it is read again when the claims snapshot moves on.
        .onChange(of: store.model.claims?.asOfSeq) { Task { await load() } }
        .sheet(item: $setTarget) { target in
            SetValueSheet(
                title: target.sheet.needsTypedName
                    ? String(localized: "Set a field of \(target.account)")
                    : String(localized: "Set \(target.sheet.fields.first ?? "")"),
                subtitle: String(localized: "Project \(target.projectName) · account \(target.account)"),
                // An existing field is the sheet's only field; an account with none asks the
                // person for the name (fix r1-3).
                fields: target.sheet.fields,
                command: CommandHelp.text(.setAccountField, name: target.account,
                                          field: target.sheet.fields.first ?? "…"),
                perform: { submitted, entry in
                    guard let project = self.project else { return .noCommandLine }
                    return await actions.setAccountField(project, account: target.account,
                                                         sheet: target.sheet,
                                                         submitted: submitted, entry: entry)
                })
            .onDisappear { Task { await load() } }
        }
        .alert(removeTitle, isPresented: removePresented, presenting: removeTarget) { target in
            Button("Remove", role: .destructive) { remove(target) }
            Button("Cancel", role: .cancel) {}
        } message: { target in
            Text(target.message)
        }
        .actionFailureAlert($failure)
    }

    // MARK: The header

    private var header: some View {
        HStack(spacing: 10) {
            WindowIcon.accounts.image
                .font(.system(size: 26, weight: .regular))
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("Accounts")
                    .font(.title2.weight(.semibold))
                Text("Project accounts and who holds them; values stay hidden until you reveal one.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            if approved.count > 1 {
                Picker(String(localized: "Project"), selection: projectBinding) {
                    ForEach(approved, id: \.info.id) { entry in
                        Text(entry.info.name).tag(entry.info.id)
                    }
                }
                .frame(maxWidth: 220)
                .accessibilityLabel("Project")
                .accessibilityIdentifier("accounts.project")
            } else if let project {
                Text(project.info.name)
                    .font(.callout.weight(.medium))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var projectBinding: Binding<String> {
        Binding(get: { project?.info.id ?? "" }, set: { projectID = $0 })
    }

    @ViewBuilder private func emptyState(_ text: String) -> some View {
        SectionBox {
            HStack(spacing: 8) {
                WindowIcon.warning.image
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: The accounts

    @ViewBuilder private func accountsSection(_ project: ProjectView) -> some View {
        // The count is the service's: nothing until this project's read succeeded (fix r1-1).
        GroupedSection(title: String(localized: "Accounts"),
                       count: rowsAreLoaded ? rows.count : nil,
                       isExpanded: .constant(true)) {
            switch readState {
            case .loaded:
                if rows.isEmpty {
                    EmptyLine(text: String(localized:
                        "No account is declared or stored for this project."))
                } else {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(rows) { row in
                            accountRow(row, project: project)
                            if row.id != rows.last?.id { Divider() }
                        }
                    }
                }
            case .failed(let error):
                HStack(spacing: 6) {
                    WindowIcon.warning.image
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .accessibilityHidden(true)
                    Text(error.errorDescription ?? String(localized: "Could not be read"))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 4)
                    Button("Try Again") { Task { await load() } }
                        .controlSize(.small)
                        .accessibilityLabel("Try reading the accounts again")
                }
            case .reading, nil:
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Reading the accounts")
                    Text("Reading the accounts…").secondaryCaption()
                }
                .accessibilityIdentifier("accounts.reading")
            }
        }
    }

    private func accountRow(_ row: AccountRow, project: ProjectView) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(row.name)
                    .font(.callout.weight(.medium))
                    .textSelection(.enabled)
                if !row.declared {
                    Text("stored").secondaryCaption()
                }
                if let purpose = row.purpose {
                    Text(purpose).secondaryCaption()
                }
                Spacer(minLength: 4)
                if let chip = row.holderChip {
                    StandingChipView(chip: StandingChip(workspace: chip))
                }
                rowMenu(row, project: project)
            }
            HStack(spacing: 4) {
                ForEach(row.fields) { field in
                    AccountFieldChip(field: field)
                }
                if row.fields.isEmpty {
                    Text("no fields").secondaryCaption()
                }
            }
            RevealView(reveal: reveal, subject: .account(project: ProjectRef(project), name: row.name))
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(row.name), \(row.stateText)")
    }

    private func rowMenu(_ row: AccountRow, project: ProjectView) -> some View {
        Menu {
            let fieldNames = row.fields.map(\.name)
            if fieldNames.isEmpty {
                Button("Set a field…") {
                    setTarget = AccountSetTarget(
                        project: project,
                        sheet: AccountFieldSheet(account: row.name, existingField: nil))
                }
            } else {
                Menu("Set a field…") {
                    ForEach(fieldNames, id: \.self) { field in
                        Button(field) {
                            setTarget = AccountSetTarget(
                                project: project,
                                sheet: AccountFieldSheet(account: row.name,
                                                         existingField: field))
                        }
                    }
                    Divider()
                    Button("New field…") {
                        setTarget = AccountSetTarget(
                            project: project,
                            sheet: AccountFieldSheet(account: row.name, existingField: nil))
                    }
                }
                Menu("Remove a field…") {
                    ForEach(row.fields.filter(\.isSet), id: \.name) { field in
                        Button(field.name) {
                            removeTarget = AccountRemoveTarget(project: project, account: row.name,
                                                               field: field.name)
                        }
                    }
                }
            }
            Divider()
            Button("Remove Account…") {
                removeTarget = AccountRemoveTarget(project: project, account: row.name,
                                                   field: nil)
            }
        } label: {
            WindowIcon.ellipsis.image
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(CommandHelp.text(.setAccountField, name: row.name, field: "…"))
        .accessibilityLabel("Actions for \(row.name)")
        .accessibilityIdentifier("account.menu.\(row.name)")
    }

    // MARK: Requests

    @ViewBuilder private func requestsSection() -> some View {
        GroupedSection(title: String(localized: "Requests"), count: cards.count,
                       isExpanded: .constant(true)) {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(cards) { card in
                    RequestCardView(card: card, requests: requests, projects: store.model.projects,
                                    presenter: .window)
                }
            }
        }
    }

    // MARK: Actions

    /// Reads the selected project's accounts. `reader.select` marks that project as reading at
    /// once, so its rows and actions wait for this read and no other project's data shows.
    private func load() async {
        guard snapshotState.isLoaded, let project else { return }
        reader.select(project.info.id)
        await reader.load(project)
    }

    private func remove(_ target: AccountRemoveTarget) {
        Task {
            if let failure = await actions.removeAccount(target.project, name: target.account,
                                                         field: target.field) {
                self.failure = failure
            } else {
                await load()
            }
        }
    }

    private var removeTitle: String {
        guard let removeTarget else { return String(localized: "Remove") }
        return removeTarget.field.map { String(localized: "Remove \($0)?") }
            ?? String(localized: "Remove \(removeTarget.account)?")
    }

    private var removePresented: Binding<Bool> {
        Binding(get: { removeTarget != nil }, set: { if !$0 { removeTarget = nil } })
    }
}

/// One field of an account as a small chip: green when set, grey when missing.
struct AccountFieldChip: View {
    var field: AccountFieldRow

    /// "set" / "missing" in the interface's language (fix F-8): both keys are in the catalog.
    private var fieldStateText: String {
        field.isSet ? String(localized: "set") : String(localized: "missing")
    }

    var body: some View {
        HStack(spacing: 3) {
            (field.isSet ? WindowIcon.checkmark : WindowIcon.clock).image
                .font(.system(size: 8, weight: .semibold))
            Text(field.name)
                .font(.caption2)
        }
        .padding(.horizontal, 5)
        .padding(.vertical, 2)
        .background(Capsule().fill(field.isSet ? Color.green.opacity(0.14)
                                               : Color.secondary.opacity(0.10)))
        .overlay(Capsule().strokeBorder(field.isSet ? Color.green.opacity(0.35)
                                                    : Color.secondary.opacity(0.25)))
        .foregroundStyle(field.isSet ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(field.name), \(fieldStateText)")
    }
}

/// The account field a `SetValueSheet` sets (fix r1-3): an existing field is locked to itself,
/// an account with none takes the name the person types.
struct AccountSetTarget: Identifiable {
    var project: ProjectView
    var sheet: AccountFieldSheet

    var id: String { "\(project.info.id)|\(sheet.account)|\(sheet.fields.first ?? "-")" }
    var account: String { sheet.account }
    var projectName: String { project.info.name }
}

/// The field (or the whole account) a person confirmed removing.
struct AccountRemoveTarget: Identifiable {
    var project: ProjectView
    var account: String
    var field: String?

    var id: String { "\(project.info.id)|\(account)|\(field ?? "-")" }

    var message: String {
        if let field {
            return String(localized: "hostler deletes \(field) from its own data. The KeePass entry's own content stays.")
        }
        return String(localized: "hostler deletes the account's fields and, once none is left, its hostler-account tag. The KeePass entry's own content stays.")
    }
}
