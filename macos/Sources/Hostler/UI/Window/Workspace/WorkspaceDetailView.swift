import AppKit
import HostlerKit
import SwiftUI

/// A workspace's detail (spec §9.3, DeviceDeck's checkout page): its header with Finder and
/// Terminal, then collapsible grouped sections for what it holds, what waits for it, its heavy
/// tasks, the other devices, its project's secrets in short and its health.
struct WorkspaceDetailView: View {
    let workspaceID: String
    let store: HostlerStore
    let actions: DeviceActions
    var openSection: (WindowSection) -> Void

    @State private var expanded: Set<String> = ["held", "waiting", "heavy", "others", "secrets", "health"]
    @State private var releaseTarget: ClaimRow?
    @State private var takeOverTarget: DeviceSheetItem?
    @State private var doctorReport: DoctorReportItem?
    @State private var doctorIsRunning = false
    @State private var failure: ActionFailure?

    var body: some View {
        ScrollView {
            if let project = project, let workspace = project.workspaces.first(where: { $0.id == workspaceID }),
               let model = detail(workspace, project: project) {
                VStack(alignment: .leading, spacing: 18) {
                    header(model)
                    held(model)
                    waiting(model)
                    heavy(model)
                    others(model)
                    secrets(model)
                    health(model)
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .navigationTitle(model.folderName)
                .navigationSubtitle(model.branch ?? model.projectName)
                .task { store.loadSecrets(of: project) }
            } else if !snapshotState.isLoaded {
                ReadingView(state: snapshotState)
            } else {
                EmptyLine(text: String(localized: "This workspace is no longer registered."))
                    .padding(16)
            }
        }
        .sheet(item: $releaseTarget) { row in
            ConfirmationSheet(
                title: String(localized: "End this workspace's \(row.kindText)"),
                explanation: String(localized:
                    "The claim ends and the run that uses the resource keeps running. hostler releases it only after this confirmation."),
                token: row.confirmationToken,
                command: CommandHelp.text(.releaseClaim, claim: row.id,
                                          token: row.confirmationToken),
                actionTitle: String(localized: "Release"),
                perform: { typed in
                    await actions.release(row.claim, token: row.confirmationToken, typed: typed)
                })
        }
        .sheet(item: $takeOverTarget) { item in
            ConfirmationSheet(
                title: String(localized: "Take \(item.device.displayName) over"),
                explanation: String(localized:
                    "hostler stops every run that uses the device in the workspace that holds it, then holds it for this workspace for 30 minutes."),
                token: item.device.confirmationToken,
                command: CommandHelp.text(.takeOverDevice, device: item.device.displayName,
                                          token: item.device.confirmationToken,
                                          workspace: workspacePathName),
                actionTitle: String(localized: "Take Over"),
                perform: { typed in
                    guard let workspace else { return .noCommandLine }
                    return await actions.takeOver(item.device, in: workspace, typed: typed)
                })
        }
        .sheet(item: $doctorReport) { item in
            DoctorReportSheet(item: item)
        }
        .actionFailureAlert($failure)
    }

    // MARK: The data

    /// Projects not read yet are not a removed workspace (fix R1-6).
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.projects != nil, syncState: store.syncState)
    }

    private var project: ProjectView? {
        store.model.projects?.projects.first { project in
            project.workspaces.contains { $0.id == workspaceID }
        }
    }

    private var workspace: WorkspaceInfo? {
        project?.workspaces.first { $0.id == workspaceID }
    }

    private var workspacePathName: String {
        workspace.map { WorkspaceChip.folderName(of: $0.path) } ?? ""
    }

    private func detail(_ workspace: WorkspaceInfo, project: ProjectView) -> WorkspaceDetailModel? {
        let now = Int64((Date().timeIntervalSince1970 * 1000).rounded())
        return workspaceDetail(
            of: workspace, projectName: project.info.name,
            devices: store.model.devices?.devices ?? [], claims: store.model.claims,
            queue: store.model.queue, branches: store.branches,
            secrets: store.projectSecrets[project.info.id],
            secretsRead: store.projectSecrets[project.info.id] != nil
                || store.projectSecretErrors[project.info.id] != nil,
            nowMs: now,
            secretsError: store.projectSecretErrors[project.info.id]?.errorDescription,
            standingOf: { store.standing(of: $0) })
    }

    // MARK: The header

    private func header(_ model: WorkspaceDetailModel) -> some View {
        HStack(alignment: .top, spacing: 12) {
            RoundedRectangle(cornerRadius: 2)
                .fill(model.color.color)
                .frame(width: 4, height: 44)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(model.folderName)
                    .font(.title2.weight(.semibold))
                    .textSelection(.enabled)
                if let branch = model.branch {
                    HStack(spacing: 5) {
                        WindowIcon.chevron.image
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .accessibilityHidden(true)
                        Text(branch)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                Text(model.workspace.path)
                    .secondaryCaption()
                    .textSelection(.enabled)
                    .truncationMode(.middle)
            }
            Spacer(minLength: 8)
            HStack(spacing: 8) {
                Button {
                    NSWorkspace.shared.activateFileViewerSelecting(
                        [URL(fileURLWithPath: model.workspace.path, isDirectory: true)])
                } label: {
                    Label { Text("Finder") } icon: { WindowIcon.finder.image }
                }
                .help("Show the checkout in Finder")
                .accessibilityLabel("Show \(model.folderName) in Finder")

                Button {
                    openInTerminal(model.workspace.path)
                } label: {
                    Label { Text("Terminal") } icon: { WindowIcon.terminal.image }
                }
                .help("Open the checkout in Terminal")
                .accessibilityLabel("Open \(model.folderName) in Terminal")
            }
        }
    }

    // MARK: Held

    private func held(_ model: WorkspaceDetailModel) -> some View {
        GroupedSection(title: String(localized: "Held by this workspace"), count: model.held.count,
                       isExpanded: expandedBinding("held")) {
            if model.held.isEmpty {
                EmptyLine(text: String(localized: "Nothing held."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.held) { row in
                        HStack(spacing: 8) {
                            WindowIcon.resource(row.resourceKind).image
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .accessibilityHidden(true)
                            Text(row.resourceName)
                                .font(.callout.weight(.medium))
                                .lineLimit(1)
                            Text(row.kindText).secondaryCaption()
                            if let command = row.command {
                                CommandText(command: command)
                            }
                            Spacer(minLength: 8)
                            Text(row.sinceText).secondaryCaption()
                            if let ttl = row.ttlText {
                                Text(ttl).secondaryCaption()
                            }
                            if row.claim.kind == .reservation {
                                Button("Unreserve") { unreserve(row.claim.resource.id) }
                                    .controlSize(.small)
                                    .help(CommandHelp.text(.unreserveDevice,
                                                           device: row.resourceName,
                                                           workspace: model.folderName))
                                    .accessibilityLabel("Unreserve \(row.resourceName)")
                            } else {
                                if row.canRenew {
                                    Button("Renew") { renew(row.claim) }
                                        .controlSize(.small)
                                        .help(CommandHelp.text(.renewHold, claim: row.id))
                                        .accessibilityLabel("Renew the hold of \(row.resourceName)")
                                }
                                Button("Release…") { releaseTarget = row }
                                    .controlSize(.small)
                                    .help(CommandHelp.text(.releaseClaim, claim: row.id,
                                                           token: row.confirmationToken))
                                    .accessibilityLabel("Release \(row.resourceName)")
                            }
                        }
                        .accessibilityElement(children: .contain)
                    }
                }
            }
        }
    }

    // MARK: Waiting

    private func waiting(_ model: WorkspaceDetailModel) -> some View {
        GroupedSection(title: String(localized: "Waiting"), count: model.waiting.count,
                       isExpanded: expandedBinding("waiting")) {
            if model.waiting.isEmpty {
                EmptyLine(text: String(localized: "Nothing waits."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.waiting) { row in
                        HStack(spacing: 8) {
                            WindowIcon.clock.image
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .accessibilityHidden(true)
                            Text(row.resourceName).font(.callout.weight(.medium))
                            Text(row.kindText).secondaryCaption()
                            Spacer(minLength: 8)
                            Text(row.sinceText).secondaryCaption()
                            Button("Withdraw") { withdraw(row.request) }
                                .controlSize(.small)
                                .help("Withdraw this workspace's waiting request")
                                .accessibilityLabel("Withdraw the request for \(row.resourceName)")
                        }
                    }
                }
            }
        }
    }

    // MARK: Heavy

    private func heavy(_ model: WorkspaceDetailModel) -> some View {
        GroupedSection(title: String(localized: "Heavy tasks"), count: model.heavy.count,
                       isExpanded: expandedBinding("heavy")) {
            if model.heavy.isEmpty {
                EmptyLine(text: String(localized:
                    "No heavy task. `hostler run --heavy -- <cmd>` puts one here."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.heavy) { row in
                        HStack(spacing: 8) {
                            Text(row.stateText)
                                .font(.caption)
                                .foregroundStyle(row.isWaiting
                                                 ? AnyShapeStyle(.secondary)
                                                 : AnyShapeStyle(Color.green))
                                .frame(width: 60, alignment: .leading)
                            CommandText(command: row.command)
                            Spacer(minLength: 8)
                            if let position = row.position {
                                Text("position \(position)").secondaryCaption()
                            }
                            Text(row.elapsedText).secondaryCaption()
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(row.stateText), \(row.command)")
                    }
                }
            }
        }
    }

    // MARK: Other devices

    private func others(_ model: WorkspaceDetailModel) -> some View {
        GroupedSection(title: String(localized: "Other devices"), count: model.otherDevices.count,
                       isExpanded: expandedBinding("others")) {
            if model.otherDevices.isEmpty {
                EmptyLine(text: String(localized: "No other device."))
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(model.otherDevices) { row in
                        HStack(spacing: 8) {
                            WindowIcon.device(row.device).image
                                .font(.body)
                                .foregroundStyle(row.state.isReachable ? AnyShapeStyle(.primary)
                                                                       : AnyShapeStyle(.secondary))
                                .frame(width: 20)
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(row.name).font(.callout.weight(.medium)).lineLimit(1)
                                HStack(spacing: 6) {
                                    Text(row.subtitle).secondaryCaption()
                                    Text(row.state.text).secondaryCaption()
                                }
                            }
                            Spacer(minLength: 8)
                            StandingChipsView(chips: row.chips, emptyText: nil)
                            Button("Reserve here") { reserve(row.device.resource) }
                                .controlSize(.small)
                                .help(CommandHelp.text(.reserveDevice, device: row.name,
                                                       workspace: model.folderName))
                                .accessibilityLabel("Reserve \(row.name) for this workspace")
                            Button("Take over…") { takeOverTarget = DeviceSheetItem(device: row.device) }
                                .controlSize(.small)
                                .disabled(!row.state.isReachable)
                                .help(CommandHelp.text(
                                    .takeOverDevice, device: row.name,
                                    token: row.device.confirmationToken,
                                    workspace: model.folderName))
                                .accessibilityLabel("Take \(row.name) over")
                        }
                    }
                }
            }
        }
    }

    // MARK: Secrets and health

    private func secrets(_ model: WorkspaceDetailModel) -> some View {
        GroupedSection(title: String(localized: "Secrets in short"),
                       isExpanded: expandedBinding("secrets"),
                       trailing: AnyView(
                        Button("Open Secrets") { openSection(.secrets) }
                            .controlSize(.small)
                            .accessibilityLabel("Open Secrets"))) {
            HStack(spacing: 6) {
                WindowIcon.secrets.image
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(model.secrets.stateText)
                    .font(.callout)
                Spacer()
            }
        }
    }

    private func health(_ model: WorkspaceDetailModel) -> some View {
        GroupedSection(title: String(localized: "Health"), isExpanded: expandedBinding("health")) {
            HStack(spacing: 8) {
                WindowIcon.doctor.image
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text("`hostler doctor` checks this checkout's setup.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer()
                if doctorIsRunning {
                    ProgressView().controlSize(.small)
                }
                Button("Run doctor") { runDoctor() }
                    .controlSize(.small)
                    .disabled(doctorIsRunning)
                    .help(CommandHelp.text(.runDoctor, workspace: model.folderName))
                    .accessibilityLabel("Run hostler doctor in \(model.folderName)")
            }
        }
    }

    // MARK: The actions

    private func expandedBinding(_ key: String) -> Binding<Bool> {
        Binding(get: { expanded.contains(key) },
                set: { isOn in
                    if isOn { expanded.insert(key) } else { expanded.remove(key) }
                })
    }

    private func renew(_ claim: Claim) {
        Task {
            if let failure = await actions.renew(claim) { self.failure = failure }
        }
    }

    private func withdraw(_ request: WaitingRequest) {
        Task {
            if let failure = await WorkspaceActions(client: actions.client).withdraw(request) {
                self.failure = failure
            }
        }
    }

    private func unreserve(_ resource: ResourceId) {
        Task {
            if let failure = await actions.unreserve(resource, for: workspaceID) {
                self.failure = failure
            }
        }
    }

    private func reserve(_ resource: ResourceId) {
        Task {
            if let failure = await actions.reserve(resource, for: workspaceID) {
                self.failure = failure
            }
        }
    }

    private func runDoctor() {
        guard let workspace else { return }
        doctorIsRunning = true
        Task {
            switch await actions.runDoctor(in: workspace) {
            case .success(let report):
                doctorReport = DoctorReportItem(report: report,
                                                workspaceName: WorkspaceChip.folderName(of: workspace.path))
            case .failure(let failure):
                self.failure = failure
            }
            doctorIsRunning = false
        }
    }

    private func openInTerminal(_ path: String) {
        let folder = URL(fileURLWithPath: path, isDirectory: true)
        let terminal = URL(fileURLWithPath: "/System/Applications/Utilities/Terminal.app")
        NSWorkspace.shared.open([folder], withApplicationAt: terminal,
                                configuration: NSWorkspace.OpenConfiguration())
    }
}

/// A doctor report and where it was run (the sheet's identity).
struct DoctorReportItem: Identifiable {
    var report: DoctorReport
    var workspaceName: String

    var id: String { workspaceName }
}

/// `hostler doctor`'s report: every check with its status, message and fix (task 11's Projects
/// view shows the same report; this is the workspace's Health section).
struct DoctorReportSheet: View {
    let item: DoctorReportItem
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Doctor — \(item.workspaceName)")
                .font(.headline)
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(item.report.checks, id: \.id) { check in
                        HStack(alignment: .top, spacing: 8) {
                            icon(for: check.status)
                                .foregroundStyle(color(for: check.status))
                                .frame(width: 16)
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(check.id)
                                        .font(.callout.weight(.medium))
                                    Text(DoctorCheckRow.statusText(check.status)).secondaryCaption()
                                }
                                Text(check.message)
                                    .font(.callout)
                                    .foregroundStyle(.secondary)
                                if let fix = check.fix {
                                    Text(fix.command ?? fix.text)
                                        .font(.system(.caption, design: .monospaced))
                                        .textSelection(.enabled)
                                }
                            }
                            Spacer(minLength: 0)
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(check.id), \(DoctorCheckRow.statusText(check.status)), \(check.message)")
                    }
                }
                .padding(4)
            }
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityLabel("Done")
            }
        }
        .padding(16)
        .frame(width: 580, height: 420)
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
