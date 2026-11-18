import HostlerKit
import SwiftUI

/// The screen (spec §9.3, §11.2): who holds this machine's desktop session, since when, its
/// command and TTL, and who waits; Release and Take over… run the CLI (a takeover stops the run
/// that holds it, and only the CLI knows that procedure).
struct ScreenView: View {
    let store: HostlerStore
    let actions: ScreenActions

    @State private var expanded: Set<String> = ["holder", "waiting"]
    @State private var releaseTarget: ClaimRow?
    @State private var takeOverTarget: ScreenTakeOverTarget?
    @State private var failure: ActionFailure?

    private var nowMs: Int64 { Int64((Date().timeIntervalSince1970 * 1000).rounded()) }

    private var model: ScreenModel {
        screenModel(store.model.claims, nowMs: nowMs)
    }

    /// The claims snapshot this section reads (fix r1-1): a missing one is not "free", and no
    /// takeover is offered before the screen's holder is known.
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.claims != nil, syncState: store.syncState)
    }

    private var workspaces: [WorkspaceInfo] {
        (store.model.projects?.projects.flatMap(\.workspaces) ?? []).sorted {
            WorkspaceChip.folderName(of: $0.path)
                .localizedStandardCompare(WorkspaceChip.folderName(of: $1.path))
                == .orderedAscending
        }
    }

    var body: some View {
        Group {
            if snapshotState.isLoaded {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        header
                        holderSection
                        waitingSection
                    }
                    .padding(16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ReadingView(state: snapshotState)
            }
        }
        .navigationTitle("Screen")
        .navigationSubtitle(snapshotState.isLoaded
            ? (model.isFree
                ? String(localized: "free")
                : String(localized: "held by \(model.holder?.workspace.name ?? String(localized: "a workspace"))"))
            : "")
        .accessibilityIdentifier("screen.view")
        .sheet(item: $releaseTarget) { row in
            ConfirmationSheet(
                title: String(localized: "Release the screen"),
                explanation: String(localized:
                    "The claim ends and the run that uses the screen keeps running. hostler releases it only after this confirmation."),
                token: ScreenActions.confirmationToken,
                command: CommandHelp.text(.releaseScreen, claim: row.id,
                                          token: row.confirmationToken),
                actionTitle: String(localized: "Release"),
                perform: { typed in
                    await actions.release(row.claim, typed: typed)
                })
        }
        .sheet(item: $takeOverTarget) { target in
            ConfirmationSheet(
                title: String(localized: "Take the screen over"),
                explanation: String(localized:
                    "hostler stops every run that uses the screen in the workspace that holds it, then holds it for \(target.folderName) for 30 minutes."),
                token: ScreenActions.confirmationToken,
                command: CommandHelp.text(.takeOverScreen,
                                          token: ScreenActions.confirmationToken,
                                          workspace: target.folderName),
                actionTitle: String(localized: "Take Over"),
                perform: { typed in
                    await actions.takeOver(in: target.workspace, typed: typed)
                })
        }
        .actionFailureAlert($failure)
    }

    // MARK: The header

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            WindowIcon.screen.image
                .font(.system(size: 30, weight: .regular))
                .foregroundStyle(model.isFree ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                .frame(width: 34)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(model.resourceName)
                    .font(.title2.weight(.semibold))
                Text(model.isFree
                     ? String(localized: "Nothing holds this machine's desktop session.")
                     : String(localized: "An agent uses the desktop session through this claim."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            takeOverMenu
        }
    }

    private var takeOverMenu: some View {
        Menu {
            if workspaces.isEmpty {
                Text("No workspace is registered yet.")
            } else {
                ForEach(workspaces, id: \.id) { workspace in
                    Button(WorkspaceChip.folderName(of: workspace.path)) {
                        takeOverTarget = ScreenTakeOverTarget(workspace: workspace)
                    }
                }
            }
        } label: {
            Label { Text("Take over…") } icon: { WindowIcon.pin.image }
        }
        .disabled(workspaces.isEmpty)
        .help(CommandHelp.text(.takeOverScreen, token: ScreenActions.confirmationToken))
        .accessibilityLabel("Take the screen over for a workspace")
        .accessibilityIdentifier("screen.takeover")
    }

    // MARK: The holder

    @ViewBuilder private var holderSection: some View {
        GroupedSection(title: String(localized: "Holder"), count: model.claims.count,
                       isExpanded: expandedBinding("holder")) {
            if model.claims.isEmpty {
                EmptyLine(text: String(localized: "The screen is free."))
            } else {
                ForEach(model.claims) { row in
                    HStack(spacing: 8) {
                        StandingChipView(chip: StandingChip(workspace: row.workspace))
                        Text(row.kindText).secondaryCaption()
                        Text(row.sinceText).secondaryCaption()
                        if let ttl = row.ttlText { Text(ttl).secondaryCaption() }
                        Spacer(minLength: 4)
                        Button("Release…") { releaseTarget = row }
                            .help(CommandHelp.text(.releaseScreen, claim: row.id,
                                                   token: row.confirmationToken))
                            .accessibilityLabel("Release the screen")
                            .accessibilityIdentifier("screen.release")
                    }
                    if let command = row.command {
                        CommandText(command: command)
                            .padding(.leading, 4)
                    }
                    FactRow(label: String(localized: "Claim"), value: row.id)
                    if let expiresAt = row.expiresAtMs {
                        Text("Ends \(Self.time(expiresAt)) unless renewed.")
                            .secondaryCaption()
                    }
                }
            }
        }
    }

    // MARK: The waiting requests

    @ViewBuilder private var waitingSection: some View {
        GroupedSection(title: String(localized: "Waiting"), count: model.waiting.count,
                       isExpanded: expandedBinding("waiting")) {
            if model.waiting.isEmpty {
                EmptyLine(text: String(localized: "Nothing waits for the screen."))
            } else {
                ForEach(model.waiting) { row in
                    HStack(spacing: 8) {
                        StandingChipView(chip: StandingChip(workspace: row.workspace))
                        Text(row.kindText).secondaryCaption()
                        Text("waiting \(row.sinceText)").secondaryCaption()
                        Spacer(minLength: 0)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(row.workspace.name), \(row.kindText), waiting \(row.sinceText)")
                }
            }
        }
    }

    private func expandedBinding(_ key: String) -> Binding<Bool> {
        Binding(get: { expanded.contains(key) },
                set: { shown in
                    if shown { expanded.insert(key) } else { expanded.remove(key) }
                })
    }

    private static func time(_ ms: Int64) -> String {
        Date(timeIntervalSince1970: Double(ms) / 1000)
            .formatted(date: .omitted, time: .shortened)
    }
}

/// The workspace a takeover was chosen for (a sheet's item).
struct ScreenTakeOverTarget: Identifiable {
    var workspace: WorkspaceInfo
    var id: String { workspace.id }
    var folderName: String { WorkspaceChip.folderName(of: workspace.path) }
}
