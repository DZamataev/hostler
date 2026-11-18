import HostlerKit
import SwiftUI

/// One project's secrets (spec §9.3, §9.6): the declared and stored ones with their purpose,
/// set or missing, and where a shared one comes from. Set or edit, reveal, delete, share with
/// another project, unshare; this project's pending requests; and the preserved
/// `hostler.kdbx.prev-*` files, which only the person gate deletes.
struct SecretsView: View {
    let store: HostlerStore
    let actions: SecretsActions
    let requests: RequestsController
    let reveal: RevealController

    @State private var projectID: String?
    @State private var setTarget: SecretSetTarget?
    @State private var removeTarget: SecretRow?
    @State private var preservedTarget: String?
    @State private var failure: ActionFailure?

    /// The projects snapshot this section reads (fix r1-1): "no project is registered" only
    /// means a loaded, empty answer.
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.projects != nil, syncState: store.syncState)
    }

    /// Whether the selected project's `secret.list` answered with a list (the only state its
    /// rows and their count show in).
    private var secretsAreLoaded: Bool {
        guard let project else { return false }
        return store.projectSecrets[project.info.id] != nil
    }

    /// Whether the selected project's `secret.list` has answered at all (a list or a failure):
    /// the marker Task 8's store keeps per project, so a first read is never "no secrets".
    private var secretsAreRead: Bool {
        guard let project else { return false }
        return secretsAreLoaded || store.projectSecretErrors[project.info.id] != nil
    }

    /// `secrets.status` is the source of the preserved files: nothing until it is read.
    private var statusState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.secrets != nil, syncState: store.syncState)
    }

    private var projects: [ProjectView] { store.model.projects?.projects ?? [] }

    private var project: ProjectView? {
        projects.first { $0.info.id == projectID } ?? projects.first
    }

    private var rows: [SecretRow] {
        guard let project else { return [] }
        return secretRows(store.projectSecrets[project.info.id])
    }

    private var loadFailure: HostlerAppError? {
        project.flatMap { store.projectSecretErrors[$0.info.id] }
    }

    private var cards: [RequestCard] {
        guard let project else { return [] }
        return RequestsController.cards(store.model)
            .filter { $0.kind == .secret && $0.projectId == project.info.id }
    }

    private var preserved: [String] { store.model.secrets?.preservedFiles ?? [] }

    private var otherProjects: [ProjectView] {
        guard let project else { return [] }
        return projects.filter { $0.info.id != project.info.id }
    }

    var body: some View {
        Group {
            if snapshotState.isLoaded {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        header
                        if let project {
                            if let state = store.model.secrets?.state, state != .unlocked {
                                lockedBanner(state)
                            }
                            if !cards.isEmpty { requestsSection() }
                            secretsSection(project)
                            preservedSection
                        } else {
                            SectionBox {
                                HStack(spacing: 8) {
                                    WindowIcon.warning.image
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .accessibilityHidden(true)
                                    Text("No project is registered yet. `hostler init` in a checkout registers one.")
                                        .font(.callout)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                    .padding(16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ReadingView(state: snapshotState)
            }
        }
        .navigationTitle("Secrets")
        .navigationSubtitle(snapshotState.isLoaded ? (project?.info.name ?? "") : "")
        .accessibilityIdentifier("secrets.view")
        .task(id: project?.info.id) { loadSecrets() }
        .sheet(item: $setTarget) { target in
            SetValueSheet(
                title: String(localized: "Set \(target.name)"),
                subtitle: String(localized: "Project \(target.projectName)"),
                fields: nil,
                command: CommandHelp.text(.setSecret, name: target.name),
                perform: { _, entry in
                    await actions.setSecret(target.project, name: target.name, entry: entry)
                })
            .onDisappear {
                if let project = project { store.loadSecrets(of: project) }
            }
        }
        .alert(removeTitle, isPresented: removePresented, presenting: removeTarget) { target in
            Button("Delete", role: .destructive) { remove(target) }
            Button("Cancel", role: .cancel) {}
        } message: { target in
            Text(String(localized:
                "hostler deletes \(target.name) from \(project?.info.name ?? String(localized: "the project")). The KeePass entry's own content stays."))
        }
        .alert(preservedTitle, isPresented: preservedPresented, presenting: preservedTarget) {
            path in
            Button("Delete", role: .destructive) { removePreserved(path) }
            Button("Cancel", role: .cancel) {}
        } message: { path in
            Text(String(localized:
                "hostler deletes \(Self.fileName(path)) after the master password. It is one of its kept previous database files."))
        }
        .actionFailureAlert($failure)
    }

    // MARK: The header

    private var header: some View {
        HStack(spacing: 10) {
            WindowIcon.secrets.image
                .font(.system(size: 26, weight: .regular))
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("Secrets")
                    .font(.title2.weight(.semibold))
                Text("This project's secrets, declared or stored; values stay hidden until you reveal one.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            if projects.count > 1 {
                Picker(String(localized: "Project"), selection: projectBinding) {
                    ForEach(projects, id: \.info.id) { entry in
                        Text(entry.info.name).tag(entry.info.id)
                    }
                }
                .frame(maxWidth: 220)
                .accessibilityLabel("Project")
                .accessibilityIdentifier("secrets.project")
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

    /// The lock state in one line; unlock lives in the menu bar panel (spec §9.2 item 3).
    private func lockedBanner(_ state: LockState) -> some View {
        SectionBox {
            HStack(spacing: 8) {
                WindowIcon.warning.image
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .accessibilityHidden(true)
                Text(state == .missing
                     ? String(localized: "No secrets database exists yet. Create one in hostler's menu bar panel.")
                     : String(localized: "The secrets database is locked. Unlock it in hostler's menu bar panel."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: The secrets

    @ViewBuilder private func secretsSection(_ project: ProjectView) -> some View {
        GroupedSection(title: String(localized: "Secrets"),
                       count: secretsAreLoaded ? rows.count : nil,
                       isExpanded: .constant(true)) {
            if !secretsAreRead {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Reading the secrets")
                    Text("Reading the secrets…").secondaryCaption()
                }
                .accessibilityIdentifier("secrets.reading")
            } else if let loadFailure {
                HStack(spacing: 6) {
                    WindowIcon.warning.image
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .accessibilityHidden(true)
                    // While the lock banner above says why, the read's own failure would repeat
                    // it (Task 13 F-5): say what follows instead.
                    Text(store.model.secrets.map { $0.state != .unlocked } == true
                         ? String(localized: "The secrets are listed once the database is unlocked.")
                         : loadFailure.errorDescription ?? String(localized: "Could not be read"))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 4)
                    Button("Try Again") { store.loadSecrets(of: project) }
                        .controlSize(.small)
                        .accessibilityLabel("Try reading the secrets again")
                }
            } else if rows.isEmpty {
                EmptyLine(text: String(localized:
                    "No secret is declared or stored for this project."))
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(rows) { row in
                        secretRow(row, project: project)
                        if row.id != rows.last?.id { Divider() }
                    }
                }
            }
        }
    }

    private func secretRow(_ row: SecretRow, project: ProjectView) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(row.name)
                    .font(.callout.weight(.medium))
                    .textSelection(.enabled)
                Text(row.isSet ? String(localized: "set") : String(localized: "missing"))
                    .font(.caption2.weight(.medium))
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(Capsule().fill(row.isSet ? Color.green.opacity(0.16)
                                                         : Color.orange.opacity(0.16)))
                    .foregroundStyle(row.isSet ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                if !row.declared {
                    Text("stored").secondaryCaption()
                }
                if let purpose = row.purpose {
                    Text(purpose).secondaryCaption()
                }
                if let sharedFrom = row.sharedFrom {
                    Text("shared from \(sharedFrom)").secondaryCaption()
                }
                Spacer(minLength: 4)
                rowMenu(row, project: project)
            }
            HStack(spacing: 6) {
                Button("Set…") {
                    setTarget = SecretSetTarget(project: project, name: row.name)
                }
                .controlSize(.small)
                .help(CommandHelp.text(.setSecret, name: row.name))
                .accessibilityLabel("Set \(row.name)")
                .accessibilityIdentifier("secret.set.\(row.name)")
                RevealView(reveal: reveal,
                           subject: .secret(project: ProjectRef(project), name: row.name))
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(row.name), \(row.stateText)")
    }

    private func rowMenu(_ row: SecretRow, project: ProjectView) -> some View {
        Menu {
            if let sourceId = row.sharedFromProjectId,
               let source = projects.first(where: { $0.info.id == sourceId }) {
                let projectId = project.info.id
                Button("Unshare…") {
                    Task {
                        if let failure = await actions.unshare(from: source, name: row.name,
                                                               toProjectId: projectId) {
                            self.failure = failure
                        } else {
                            store.loadSecrets(of: project)
                        }
                    }
                }
            } else {
                Menu("Share with…") {
                    if otherProjects.isEmpty {
                        Text("No other project is registered.")
                    }
                    ForEach(otherProjects, id: \.info.id) { target in
                        Button(target.info.name) {
                            Task {
                                if let failure = await actions.share(project, name: row.name,
                                                                     toProjectId: target.info.id) {
                                    self.failure = failure
                                }
                            }
                        }
                    }
                }
                Button("Delete…") { removeTarget = row }
            }
        } label: {
            WindowIcon.ellipsis.image
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(CommandHelp.text(.removeSecret, name: row.name))
        .accessibilityLabel("Actions for \(row.name)")
        .accessibilityIdentifier("secret.menu.\(row.name)")
    }

    // MARK: The preserved files

    @ViewBuilder private var preservedSection: some View {
        GroupedSection(title: String(localized: "Preserved files"),
                       count: statusState.isLoaded ? preserved.count : nil,
                       isExpanded: .constant(true)) {
            if !statusState.isLoaded {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Reading the preserved files")
                    Text("Reading the preserved files…").secondaryCaption()
                }
                .accessibilityIdentifier("secrets.preserved.reading")
            } else if preserved.isEmpty {
                EmptyLine(text: String(localized: "No previous database file is kept."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(preserved, id: \.self) { path in
                        HStack(spacing: 8) {
                            Text(Self.fileName(path))
                                .font(.system(.callout, design: .monospaced))
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .textSelection(.enabled)
                            Spacer(minLength: 4)
                            Button("Delete…") { preservedTarget = path }
                                .controlSize(.small)
                                .accessibilityLabel("Delete \(Self.fileName(path))")
                                .accessibilityIdentifier("secrets.preserved.delete")
                        }
                        .accessibilityElement(children: .contain)
                        .accessibilityLabel(path)
                    }
                }
            }
        }
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

    private func loadSecrets() {
        if let project { store.loadSecrets(of: project) }
    }

    private func remove(_ row: SecretRow) {
        guard let project else { return }
        Task {
            if let failure = await actions.removeSecret(project, name: row.name) {
                self.failure = failure
            } else {
                store.loadSecrets(of: project)
            }
        }
    }

    private func removePreserved(_ path: String) {
        let actions = SecretsActions(client: actions.client, gate: actions.gate,
                                     preservedFiles: preserved)
        Task {
            switch await actions.removePreserved(path) {
            case .done:
                // The answer's list is fresh, but the store only takes secrets statuses from
                // `secrets.lock_changed` events, and a delete changes no lock: re-read.
                store.refresh()
            case .cancelled:
                break
            case .failed(let failure):
                self.failure = failure
            }
        }
    }

    private var removeTitle: String {
        guard let removeTarget else { return String(localized: "Delete") }
        return String(localized: "Delete \(removeTarget.name)?")
    }

    private var removePresented: Binding<Bool> {
        Binding(get: { removeTarget != nil }, set: { if !$0 { removeTarget = nil } })
    }

    private var preservedTitle: String {
        guard let preservedTarget else { return String(localized: "Delete") }
        return String(localized: "Delete \(Self.fileName(preservedTarget))?")
    }

    private var preservedPresented: Binding<Bool> {
        Binding(get: { preservedTarget != nil }, set: { if !$0 { preservedTarget = nil } })
    }

    private static func fileName(_ path: String) -> String {
        (path as NSString).lastPathComponent
    }
}

/// The secret a `SetValueSheet` sets.
struct SecretSetTarget: Identifiable {
    var project: ProjectView
    var name: String

    var id: String { "\(project.info.id)|\(name)" }
    var projectName: String { project.info.name }
}
