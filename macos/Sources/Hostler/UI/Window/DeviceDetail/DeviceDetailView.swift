import HostlerKit
import SwiftUI

/// A device's detail (spec §9.3, DeviceDeck's page): a header, then collapsible grouped sections
/// for telemetry, the claims, the agent-device sessions, the installed apps, the note and the
/// device's claim history.
struct DeviceDetailView: View {
    let deviceID: String
    let store: HostlerStore
    let actions: DeviceActions

    @State private var expanded: Set<String> = ["telemetry", "claims", "sessions", "installs", "note"]
    @State private var noteText = ""
    @State private var noteIsEdited = false
    @State private var renameTarget: DeviceSheetItem?
    @State private var removeTarget: DeviceSheetItem?
    @State private var releaseTarget: ClaimRow?
    @State private var failure: ActionFailure?

    private var device: DeviceInfo? {
        store.model.devices?.devices.first { $0.resource.rawValue == deviceID }
    }

    /// The note the service has now (fix F-7): the field follows it unless the person is editing.
    private var serviceNote: String? { device?.note }

    /// A device missing from an unread snapshot is not a removed device (fix R1-6).
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.devices != nil, syncState: store.syncState)
    }

    var body: some View {
        ScrollView {
            if let device {
                VStack(alignment: .leading, spacing: 18) {
                    header(device)
                    telemetry(device)
                    claimsSection(device)
                    sessions(device)
                    installedApps(device)
                    note(device)
                    history(device)
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            } else if !snapshotState.isLoaded {
                ReadingView(state: snapshotState)
            } else {
                EmptyLine(text: String(localized: "This device is no longer in hostler's list."))
                    .padding(16)
            }
        }
        .navigationTitle(device?.displayName ?? String(localized: "Device"))
        .navigationSubtitle(device?.summaryText ?? "")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    if let device { renameTarget = DeviceSheetItem(device: device) }
                } label: {
                    WindowIcon.pencil.image
                }
                .help(CommandHelp.text(.renameDevice, device: device?.displayName))
                .accessibilityLabel("Rename device")
                .disabled(device == nil)
            }
        }
        .onAppear {
            noteText = NoteField.text(serviceNote: serviceNote, keepsEdit: false, field: noteText)
        }
        .onChange(of: deviceID) {
            noteText = NoteField.text(serviceNote: serviceNote, keepsEdit: false, field: noteText)
            noteIsEdited = false
        }
        // An external change (an agent, the CLI, another window) reaches the field; a person's
        // unsaved edit is kept (fix F-7).
        .onChange(of: serviceNote) {
            noteText = NoteField.text(serviceNote: serviceNote, keepsEdit: noteIsEdited,
                                      field: noteText)
        }
        .sheet(item: $renameTarget) { item in
            RenameDeviceSheet(device: item.device, actions: actions)
        }
        .sheet(item: $releaseTarget) { row in
            ConfirmationSheet(
                title: String(localized: "End \(row.workspace.name)'s \(row.kindText)"),
                explanation: String(localized:
                    "The claim ends and the run that uses the device keeps running. hostler releases it only after this confirmation."),
                token: row.confirmationToken,
                command: CommandHelp.text(.releaseClaim, claim: row.id,
                                          token: row.confirmationToken),
                actionTitle: String(localized: "Release"),
                perform: { typed in
                    await actions.release(row.claim, token: row.confirmationToken, typed: typed)
                })
        }
        .alert(removeTitle, isPresented: removePresented, presenting: removeTarget) { item in
            Button(String(localized: "Remove"), role: .destructive) { remove(item.device) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text(String(localized: "hostler forgets the device. Nothing on the device changes."))
        }
        .actionFailureAlert($failure)
    }

    // MARK: The header

    private func header(_ device: DeviceInfo) -> some View {
        HStack(alignment: .top, spacing: 12) {
            WindowIcon.device(device).image
                .font(.system(size: 30, weight: .regular))
                .foregroundStyle(device.connected ? AnyShapeStyle(.primary)
                                                  : AnyShapeStyle(.secondary))
                .frame(width: 34)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(device.displayName)
                        .font(.title2.weight(.semibold))
                        .textSelection(.enabled)
                    StandingChipsView(chips: standingChips(store.standing(of: device)), emptyText: nil)
                }
                Text(device.summaryText)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                HStack(spacing: 10) {
                    DeviceStateCell(state: DeviceState.of(device))
                    Text("id \(device.hardwareId)")
                        .secondaryCaption()
                        .textSelection(.enabled)
                    if !device.registered {
                        Text("not registered")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    if device.bootedByHostler {
                        Text("booted by hostler")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            Spacer(minLength: 8)
            headerActions(device)
        }
    }

    private func headerActions(_ device: DeviceInfo) -> some View {
        HStack(spacing: 8) {
            Button {
                renameTarget = DeviceSheetItem(device: device)
            } label: {
                Label { Text("Rename…") } icon: { WindowIcon.pencil.image }
            }
            .help(CommandHelp.text(.renameDevice, device: device.displayName))
            .accessibilityLabel("Rename device")

            Menu {
                Section("Reserve") {
                    ForEach(reservedWorkspaces, id: \.id) { workspace in
                        Button("Unreserve \(WorkspaceChip.folderName(of: workspace.path))") {
                            reserve(device, .unreserve, workspace)
                        }
                        .help(CommandHelp.text(.unreserveDevice, device: device.displayName,
                                               workspace: WorkspaceChip.folderName(of: workspace.path)))
                    }
                    ForEach(otherWorkspaces, id: \.id) { workspace in
                        Button("Reserve for \(WorkspaceChip.folderName(of: workspace.path))") {
                            reserve(device, .reserve, workspace)
                        }
                        .help(CommandHelp.text(.reserveDevice, device: device.displayName,
                                               workspace: WorkspaceChip.folderName(of: workspace.path)))
                    }
                }
                Button("Forget agent-device sessions") { forgetSessions(device) }
                    .help(CommandHelp.text(.forgetSessions, device: device.displayName))
                Divider()
                Button("Remove…", role: .destructive) {
                    removeTarget = DeviceSheetItem(device: device)
                }
                .help(CommandHelp.text(.removeDevice, device: device.displayName))
            } label: {
                Label { Text("More") } icon: { WindowIcon.ellipsis.image }
            }
            .accessibilityLabel("More device actions")
        }
    }

    // MARK: Telemetry

    private func telemetry(_ device: DeviceInfo) -> some View {
        GroupedSection(title: String(localized: "Telemetry"),
                       isExpanded: expandedBinding("telemetry")) {
            let facts = TelemetryFacts.of(device.telemetry,
                                          nowMs: Int64((Date().timeIntervalSince1970 * 1000).rounded()))
            VStack(alignment: .leading, spacing: 6) {
                FactRow(label: String(localized: "Battery"), value: facts.battery,
                        symbol: .battery)
                FactRow(label: String(localized: "Charging"), value: facts.charging,
                        symbol: .bolt)
                FactRow(label: String(localized: "Wi-Fi"),
                        value: facts.wifiName.map { "\(facts.wifi) · \($0)" } ?? facts.wifi,
                        symbol: .wifi)
                FactRow(label: String(localized: "Cellular data"), value: facts.cellular)
                FactRow(label: String(localized: "VPN"), value: facts.vpn)
                if let taken = facts.takenText {
                    FactRow(label: String(localized: "Read"), value: taken)
                }
            }
        }
    }

    // MARK: Claims

    private func claimsSection(_ device: DeviceInfo) -> some View {
        let rows = claimRows(claims(of: device.resource, in: store.model.claims),
                             nowMs: Int64((Date().timeIntervalSince1970 * 1000).rounded()))
        return GroupedSection(title: String(localized: "Claims"), count: rows.count,
                              isExpanded: expandedBinding("claims")) {
            if rows.isEmpty {
                EmptyLine(text: String(localized: "No claims."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(rows) { row in
                        HStack(spacing: 8) {
                            StandingChipView(chip: StandingChip(workspace: row.workspace))
                            Text(row.kindText)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            if let command = row.command {
                                CommandText(command: command)
                            }
                            Spacer(minLength: 8)
                            Text(row.sinceText).secondaryCaption()
                            if let ttl = row.ttlText {
                                Text(ttl).secondaryCaption()
                            }
                            if row.canRenew {
                                Button("Renew") { renew(row.claim) }
                                    .controlSize(.small)
                                    .help(CommandHelp.text(.renewHold, claim: row.id))
                                    .accessibilityLabel("Renew the hold of \(row.workspace.name)")
                            }
                            Button("Release…") { releaseTarget = row }
                                .controlSize(.small)
                                .help(CommandHelp.text(.releaseClaim, claim: row.id,
                                                       token: row.confirmationToken))
                                .accessibilityLabel("Release \(row.workspace.name)'s claim")
                        }
                    }
                }
            }
        }
    }

    // MARK: agent-device

    private func sessions(_ device: DeviceInfo) -> some View {
        GroupedSection(title: String(localized: "agent-device"), count: device.sessions.count,
                       isExpanded: expandedBinding("sessions")) {
            if device.sessions.isEmpty {
                EmptyLine(text: device.checkingSessions
                          ? String(localized: "Reading agent-device's sessions…")
                          : String(localized: "No agent-device session."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(device.sessions, id: \.session) { session in
                        HStack(spacing: 8) {
                            WindowIcon.handTap.image
                                .foregroundStyle(.secondary)
                                .accessibilityHidden(true)
                            Text(session.session)
                                .font(.system(.callout, design: .monospaced))
                                .lineLimit(1)
                            StandingChipView(chip: StandingChip(workspace: WorkspaceChip(
                                workspaceId: session.workspaceId,
                                path: session.workspacePath)))
                            if session.kept {
                                Text("kept").secondaryCaption()
                            }
                            Spacer(minLength: 8)
                            Text(Span.since(session.seenSinceMs, nowMs: now)).secondaryCaption()
                        }
                    }
                }
            }
        }
    }

    // MARK: Installed apps

    private func installedApps(_ device: DeviceInfo) -> some View {
        let rows = installRows(of: device, installs: store.model.installs, nowMs: now,
                               workspaces: workspaces)
        return GroupedSection(title: String(localized: "Installed apps"), count: rows.count,
                              isExpanded: expandedBinding("installs")) {
            if rows.isEmpty {
                EmptyLine(text: String(localized: "No install seen in the last 24 hours."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(rows) { row in
                        HStack(spacing: 8) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(row.appId)
                                    .font(.system(.callout, design: .monospaced))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                HStack(spacing: 6) {
                                    Text(row.changeText).secondaryCaption()
                                    if let version = row.versionText {
                                        Text(version).secondaryCaption()
                                    }
                                    Text(row.timeText).secondaryCaption()
                                }
                            }
                            Spacer(minLength: 8)
                            if let workspace = row.workspace {
                                StandingChipView(chip: StandingChip(workspace: workspace))
                            }
                            Text(row.sourceText)
                                .secondaryCaption()
                                .frame(maxWidth: 300, alignment: .trailing)
                                .help(row.sourceText)
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(row.appId), \(row.changeText), \(row.sourceText)")
                    }
                }
            }
        }
    }

    // MARK: Note

    private func note(_ device: DeviceInfo) -> some View {
        GroupedSection(title: String(localized: "Note"), isExpanded: expandedBinding("note"),
                       trailing: AnyView(
            HStack(spacing: 6) {
                Button("Clear") { clearNote(device) }
                    .controlSize(.small)
                    .disabled(noteText.isEmpty && (device.note ?? "").isEmpty)
                    .help(CommandHelp.text(.clearNote, device: device.displayName))
                    .accessibilityLabel("Clear the note")
                    .accessibilityIdentifier("device.note.clear")
                Button("Save") { saveNote(device) }
                    .controlSize(.small)
                    .disabled(!noteIsEdited)
                    .help(CommandHelp.text(.noteDevice, device: device.displayName,
                                           note: noteText))
                    .accessibilityLabel("Save the note")
            })) {
            ZStack(alignment: .topLeading) {
                TextEditor(text: $noteText)
                    .font(.body)
                    .scrollContentBackground(.hidden)
                    .onChange(of: noteText) { noteIsEdited = true }
                    .accessibilityLabel("Note")
                if noteText.isEmpty {
                    Text(String(localized: "No note. Write one and Save."))
                        .font(.body)
                        .foregroundStyle(.tertiary)
                        .padding(.top, 2)
                        .padding(.leading, 5)
                        .allowsHitTesting(false)
                }
            }
            .frame(minHeight: 54, maxHeight: 90)
            .padding(6)
            .background(RoundedRectangle(cornerRadius: 6)
                .fill(Color(nsColor: .textBackgroundColor).opacity(0.6)))
        }
    }

    // MARK: History

    private func history(_ device: DeviceInfo) -> some View {
        // P5-L16: the stable id, so a rename never moves history away and an alias collision
        // never attaches another resource's rows.
        let rows = claimHistory(of: device.resource, in: store.model.claimHistory)
        return GroupedSection(title: String(localized: "History"), count: rows.count,
                              isExpanded: expandedBinding("history")) {
            if rows.isEmpty {
                EmptyLine(text: String(localized: "No claims in the last 24 hours."))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(rows, id: \.id) { item in
                        HStack(spacing: 8) {
                            Text(item.kind.title)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .frame(width: 70, alignment: .leading)
                            StandingChipView(chip: StandingChip(workspace: WorkspaceChip(
                                workspaceId: nil, path: item.workspacePath)))
                            if let command = item.command {
                                CommandText(command: command)
                            }
                            Spacer(minLength: 8)
                            Text(item.reason.rawValue).secondaryCaption()
                            Text(Span.since(item.endedAtMs, nowMs: now)).secondaryCaption()
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(item.kind.title), \(item.reason.rawValue)")
                    }
                }
            }
        }
    }

    // MARK: Helpers

    private var now: Int64 { Int64((Date().timeIntervalSince1970 * 1000).rounded()) }

    private var workspaces: [WorkspaceInfo] {
        store.model.projects?.projects.flatMap(\.workspaces) ?? []
    }

    private var reservedWorkspaces: [WorkspaceInfo] {
        guard let device else { return [] }
        let ids = Set((store.model.claims?.resources.first { $0.resource.id == device.resource }?
            .claims ?? []).filter { $0.kind == .reservation }.map(\.workspace.id))
        return workspaces.filter { ids.contains($0.id) }
    }

    private var otherWorkspaces: [WorkspaceInfo] {
        let reserved = Set(reservedWorkspaces.map(\.id))
        return workspaces.filter { !reserved.contains($0.id) }
    }

    private func expandedBinding(_ key: String) -> Binding<Bool> {
        Binding(get: { expanded.contains(key) },
                set: { isOn in
                    if isOn { expanded.insert(key) } else { expanded.remove(key) }
                })
    }

    private func reserve(_ device: DeviceInfo, _ action: ReserveAction,
                         _ workspace: WorkspaceInfo) {
        Task {
            let failure = action == .reserve
                ? await actions.reserve(device.resource, for: workspace.id)
                : await actions.unreserve(device.resource, for: workspace.id)
            if let failure { self.failure = failure }
        }
    }

    private func renew(_ claim: Claim) {
        Task {
            if let failure = await actions.renew(claim) { self.failure = failure }
        }
    }

    private func forgetSessions(_ device: DeviceInfo) {
        Task {
            if let failure = await actions.forgetSessions(device) { self.failure = failure }
        }
    }

    private var removeTitle: String {
        removeTarget.map { String(localized: "Remove \($0.device.displayName)?") }
            ?? String(localized: "Remove the device?")
    }

    private var removePresented: Binding<Bool> {
        Binding(get: { removeTarget != nil }, set: { if !$0 { removeTarget = nil } })
    }

    private func remove(_ device: DeviceInfo) {
        removeTarget = nil
        Task {
            if let failure = await actions.remove(device) { self.failure = failure }
        }
    }

    private func saveNote(_ device: DeviceInfo) {
        let text = noteText
        Task {
            if let failure = await actions.setNote(device, note: text) {
                self.failure = failure
            } else {
                // The service records the text without its surrounding whitespace; the field
                // shows what it recorded, and `serviceNote` keeps following it afterwards.
                noteText = text.trimmingCharacters(in: .whitespacesAndNewlines)
                noteIsEdited = false
            }
        }
    }

    /// `devices.note` with no note — the CLI's `--clear` (fix F-7). The field empties only once
    /// the service took it; until then it keeps what the person typed.
    private func clearNote(_ device: DeviceInfo) {
        Task {
            if let failure = await actions.clearNote(device) {
                self.failure = failure
            } else {
                noteText = NoteField.text(serviceNote: nil, keepsEdit: false, field: noteText)
                noteIsEdited = false
            }
        }
    }
}

/// Which way a reservation goes.
enum ReserveAction {
    case reserve
    case unreserve
}
