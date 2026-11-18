import AppKit
import HostlerKit
import SwiftUI

/// The registered projects (spec §9.3): each one's approval state and workspaces, the approval
/// itself (a confirmation says the project may then ask for secrets), and the config health —
/// `hostler doctor --json` in a chosen workspace, with each check's status, message, fix command
/// (Copy) and who acts.
struct ProjectsView: View {
    let store: HostlerStore
    let actions: ProjectsActions
    /// `hostler doctor --json` (Task 10's runner: the report is readable at exit status 1).
    let doctor: DeviceActions

    @State private var approveTarget: ProjectRow?
    @State private var report: ProjectsDoctorReport?
    @State private var runningDoctorFor: String?
    @State private var failure: ActionFailure?

    private var rows: [ProjectRow] { projectRows(store.model.projects) }

    /// The projects snapshot this section reads (fix r1-1): "no project is registered" only
    /// means a loaded, empty answer.
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.projects != nil, syncState: store.syncState)
    }

    private var approvedCount: Int { rows.filter(\.approved).count }

    var body: some View {
        Group {
            if snapshotState.isLoaded {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        header
                        if rows.isEmpty {
                            emptyState
                        } else {
                            ForEach(rows) { row in
                                projectCard(row)
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
        .navigationTitle("Projects")
        .navigationSubtitle(snapshotState.isLoaded
            ? (rows.isEmpty
                ? String(localized: "No projects")
                : String(localized: "\(rows.count) projects · \(approvedCount) approved"))
            : "")
        .accessibilityIdentifier("projects.view")
        .alert(approveTitle, isPresented: approvePresented, presenting: approveTarget) { row in
            Button("Approve", role: .destructive) { approve(row) }
            Button("Cancel", role: .cancel) {}
        } message: { row in
            Text(String(localized:
                "\(row.name) may then ask for secrets and accounts. hostler records the approval and shows the same to its agents."))
        }
        .sheet(item: $report) { item in
            ProjectsDoctorSheet(item: item)
        }
        .actionFailureAlert($failure)
    }

    // MARK: The header

    private var header: some View {
        HStack(spacing: 10) {
            WindowIcon.projects.image
                .font(.system(size: 26, weight: .regular))
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("Projects")
                    .font(.title2.weight(.semibold))
                Text("hostler's projects, their approval, and their checkouts' health.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
    }

    private var emptyState: some View {
        SectionBox {
            HStack(spacing: 8) {
                WindowIcon.warning.image
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text("No project is registered yet — `hostler init` in a checkout registers one.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: One project

    private func projectCard(_ row: ProjectRow) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(row.name)
                    .font(.headline)
                    .textSelection(.enabled)
                Text(row.approvalText)
                    .font(.caption2.weight(.medium))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(row.approved ? Color.green.opacity(0.16)
                                                             : Color.orange.opacity(0.16)))
                    .overlay(Capsule().strokeBorder(row.approved ? Color.green.opacity(0.35)
                                                                  : Color.orange.opacity(0.35)))
                    .foregroundStyle(row.approved ? AnyShapeStyle(.primary)
                                                  : AnyShapeStyle(.secondary))
                Spacer(minLength: 4)
                if !row.approved {
                    Button("Approve…") { approveTarget = row }
                        .help(CommandHelp.text(.approveProject, workspace: row.name))
                        .accessibilityLabel("Approve \(row.name) for secrets")
                        .accessibilityIdentifier("project.approve.\(row.id)")
                }
                doctorMenu(row)
            }
            Text(row.path)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .lineLimit(1)
                .truncationMode(.middle)
            HStack(spacing: 10) {
                if row.declaredSecrets > 0 {
                    Text(row.declaredSecrets == 1
                         ? String(localized: "1 declared secret")
                         : String(localized: "\(row.declaredSecrets) declared secrets"))
                        .secondaryCaption()
                }
                if row.declaredAccounts > 0 {
                    Text(row.declaredAccounts == 1
                         ? String(localized: "1 declared account")
                         : String(localized: "\(row.declaredAccounts) declared accounts"))
                        .secondaryCaption()
                }
                if row.workspaces.isEmpty {
                    Text("No checkout of it is registered yet.").secondaryCaption()
                }
            }
            if !row.workspaces.isEmpty {
                SectionBox {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(row.workspaces) { workspace in
                            HStack(spacing: 7) {
                                StatusDot(color: workspace.color.color, diameter: 8)
                                Text(workspace.folderName)
                                    .font(.callout)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                if let branch = store.branches[workspace.id] {
                                    Text(branch).secondaryCaption()
                                }
                                Spacer(minLength: 4)
                                Text(workspace.path)
                                    .secondaryCaption()
                                    .textSelection(.enabled)
                            }
                            .accessibilityElement(children: .ignore)
                            .accessibilityLabel("\(workspace.folderName), \(workspace.path)")
                        }
                    }
                }
            }
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor).opacity(0.4)))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Color(nsColor: .separatorColor).opacity(0.5)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("project.\(row.id)")
    }

    /// Run doctor in one of the project's workspaces (the CLI reads the workspace's config).
    private func doctorMenu(_ row: ProjectRow) -> some View {
        Menu {
            if row.workspaces.isEmpty {
                Text("No checkout of it is registered yet.")
            }
            ForEach(row.workspaces) { workspace in
                Button(workspace.folderName) { runDoctor(in: workspace) }
            }
        } label: {
            if runningDoctorFor == row.id {
                ProgressView().controlSize(.small)
            } else {
                Label { Text("Run doctor") } icon: { WindowIcon.doctor.image }
            }
        }
        .disabled(row.workspaces.isEmpty || runningDoctorFor != nil)
        .help(CommandHelp.text(.runDoctor, workspace: row.workspaces.first?.folderName))
        .accessibilityLabel("Run doctor in a workspace of \(row.name)")
        .accessibilityIdentifier("project.doctor.\(row.id)")
    }

    private var approveTitle: String {
        guard let approveTarget else { return String(localized: "Approve") }
        return String(localized: "Approve \(approveTarget.name)?")
    }

    private var approvePresented: Binding<Bool> {
        Binding(get: { approveTarget != nil }, set: { if !$0 { approveTarget = nil } })
    }

    // MARK: Actions

    private func approve(_ row: ProjectRow) {
        Task {
            if let failure = await actions.approve(row) { self.failure = failure }
        }
    }

    private func runDoctor(in workspace: ProjectWorkspaceRow) {
        runningDoctorFor = workspace.workspace.projectId
        Task {
            switch await doctor.runDoctor(in: workspace.workspace) {
            case .success(let report):
                self.report = ProjectsDoctorReport(report: report,
                                                   workspaceName: workspace.folderName)
            case .failure(let failure):
                self.failure = failure
            }
            runningDoctorFor = nil
        }
    }
}

/// The report a doctor run answered for a workspace (the sheet's item).
struct ProjectsDoctorReport: Identifiable {
    var report: DoctorReport
    var workspaceName: String

    var id: String { workspaceName }
}

/// `hostler doctor`'s report as the Projects section shows it: each check's status, message,
/// the fix's text and command (Copy), and who acts.
struct ProjectsDoctorSheet: View {
    let item: ProjectsDoctorReport

    @Environment(\.dismiss) private var dismiss
    @State private var copied: String?

    private var model: DoctorReportModel { doctorModel(item.report) }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("hostler doctor — \(item.workspaceName)")
                    .font(.headline)
                Spacer(minLength: 4)
                Text(model.summary).secondaryCaption()
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(model.rows) { row in
                        checkRow(row)
                    }
                }
                .padding(2)
            }
            HStack {
                Text("The app runs `hostler doctor --json` and never `--fix`.")
                    .secondaryCaption()
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityLabel("Done")
            }
        }
        .padding(16)
        .frame(width: 640, height: 460)
        .accessibilityIdentifier("projects.doctor")
    }

    private func checkRow(_ row: DoctorCheckRow) -> some View {
        HStack(alignment: .top, spacing: 8) {
            icon(for: row.status)
                .font(.caption)
                .foregroundStyle(color(for: row.status))
                .frame(width: 16)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(row.id)
                        .font(.callout.weight(.medium))
                        .textSelection(.enabled)
                    Text(row.statusText).secondaryCaption()
                }
                Text(row.message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let text = row.fixText {
                    Text(text)
                        .font(.callout)
                }
                if let command = row.fixCommand {
                    HStack(spacing: 6) {
                        Text(command)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                        Button(copied == command ? String(localized: "Copied") : String(localized: "Copy")) {
                            copy(command)
                        }
                        .controlSize(.small)
                        .accessibilityLabel("Copy \(command)")
                    }
                }
                if let who = row.whoText {
                    Text(who).secondaryCaption()
                }
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(row.id), \(row.statusText), \(row.message)")
    }

    /// A command, not a value: the general pasteboard, no concealed marker, no clear.
    private func copy(_ command: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(command, forType: .string)
        copied = command
    }

    private func icon(for status: CheckStatus) -> Image {
        switch status {
        case .ok: WindowIcon.checkmark.image
        case .warn: WindowIcon.warning.image
        case .problem: WindowIcon.error.image
        case .skipped: WindowIcon.skip.image
        }
    }

    private func color(for status: CheckStatus) -> Color {
        switch status {
        case .ok: .green
        case .warn: .yellow
        case .problem: .red
        case .skipped: .secondary
        }
    }
}
