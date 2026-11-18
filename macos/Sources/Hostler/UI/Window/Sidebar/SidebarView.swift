import HostlerKit
import SwiftUI

/// The window's sidebar (spec §9.3, DeviceDeck's structure): the top group, one section per
/// project with a row per workspace, and the footer with the service's state and Refresh.
struct SidebarView: View {
    let store: HostlerStore?
    @Binding var section: WindowSection?
    @State private var showingHidden: Set<String> = []

    var body: some View {
        List(selection: $section) {
            Section {
                ForEach(WindowSection.topLevel, id: \.self) { entry in
                    Label {
                        Text(entry.title)
                    } icon: {
                        entry.icon.image
                    }
                    .tag(entry)
                    .accessibilityLabel(entry.title)
                    .accessibilityIdentifier("sidebar.\(entry.id)")
                }
            }

            ForEach(groups) { group in
                Section {
                    ForEach(group.rows) { entry in
                        SidebarWorkspaceRow(entry: entry)
                            .tag(WindowSection.workspace(entry.id))
                    }
                } header: {
                    SidebarProjectHeader(
                        group: group,
                        toggle: { showingHidden.formSymmetricDifference([group.id]) })
                }
            }
        }
        .listStyle(.sidebar)
        .navigationSplitViewColumnWidth(min: 210, ideal: 250, max: 340)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            SidebarFooter(store: store)
        }
    }

    private var groups: [SidebarProjectGroup] {
        guard let store else { return [] }
        return sidebarGroups(
            projects: store.model.projects?.projects ?? [], claims: store.model.claims,
            queue: store.model.queue, branches: store.branches,
            nowMs: Int64((Date().timeIntervalSince1970 * 1000).rounded()), showsHidden: showingHidden)
    }
}

/// A project's section header: its name and the menu over its older workspaces.
struct SidebarProjectHeader: View {
    var group: SidebarProjectGroup
    var toggle: () -> Void

    var body: some View {
        HStack(spacing: 4) {
            Text(group.name)
                .lineLimit(1)
            Spacer(minLength: 2)
            if group.showsHidden || !group.hidden.isEmpty {
                Menu {
                    Toggle("Show workspaces used in the last 30 days only",
                           isOn: Binding(get: { !group.showsHidden }, set: { _ in toggle() }))
                    if !group.hidden.isEmpty {
                        Text(group.showsHidden
                             ? String(localized: "\(group.hidden.count) older workspaces are shown")
                             : String(localized: "\(group.hidden.count) older workspaces are hidden"))
                    }
                } label: {
                    WindowIcon.ellipsis.image
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help(group.showsHidden
                      ? String(localized: "Hide workspaces not seen in 30 days")
                      : String(localized: "Show \(group.hidden.count) workspaces not seen in 30 days"))
                .accessibilityLabel("Options for \(group.name)")
            }
        }
    }
}

/// One workspace's row: its color dot, folder name, branch, what it holds, and the markers for a
/// running heavy task and a waiting request.
struct SidebarWorkspaceRow: View {
    var entry: SidebarWorkspace

    var body: some View {
        HStack(alignment: .center, spacing: 7) {
            StatusDot(color: entry.row.color.color, diameter: 8)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 4) {
                    Text(entry.row.folderName)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if entry.row.heavyRunning {
                        WindowIcon.bolt.image
                            .font(.caption2)
                            .foregroundStyle(.yellow)
                    }
                    if entry.isWaiting {
                        WindowIcon.clock.image
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                if entry.row.branch != nil || entry.row.heldCount > 1 {
                    HStack(spacing: 6) {
                        if let branch = entry.row.branch {
                            Text(branch).secondaryCaption()
                        }
                        if entry.row.heldCount > 1 {
                            Text("\(entry.row.heldCount) held").secondaryCaption()
                        }
                    }
                }
            }
            Spacer(minLength: 2)
            if entry.row.heldCount > 0 {
                CountBadge(count: entry.row.heldCount)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityText)
        .accessibilityIdentifier("workspace.\(entry.id)")
    }

    private var accessibilityText: String {
        var parts = [entry.row.folderName]
        if let branch = entry.row.branch { parts.append(branch) }
        if entry.row.heldCount > 0 {
            parts.append(String(localized: "\(entry.row.heldCount) held"))
        }
        if entry.row.heavyRunning { parts.append(String(localized: "heavy task running")) }
        if entry.isWaiting { parts.append(String(localized: "waiting for a resource")) }
        return parts.joined(separator: ", ")
    }
}

/// The sidebar's footer: Live or Offline since HH:MM, when the last event came, and Refresh
/// (⌘R re-reads every snapshot).
struct SidebarFooter: View {
    let store: HostlerStore?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 10)) { context in
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    StatusDot(color: stateColor)
                    Text(stateText)
                        .font(.caption)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    Button {
                        store?.refresh()
                    } label: {
                        WindowIcon.refresh.image
                            .font(.caption)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(store == nil)
                    .keyboardShortcut("r", modifiers: .command)
                    .help("Refresh — re-read every snapshot (⌘R)")
                    .accessibilityLabel("Refresh")
                }
                Text(detailText(now: context.date))
                    .secondaryCaption()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.bar)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("sidebar.footer")
    }

    private var stateColor: Color {
        guard let store else { return .secondary }
        switch store.syncState {
        case .live: return .green
        case .syncing: return .orange
        case .failed: return .red
        case .offline: return .secondary
        }
    }

    private var stateText: String {
        guard let store else { return String(localized: "Not connected") }
        switch store.syncState {
        case .live: return String(localized: "Live")
        case .syncing(let catchingUp):
            return catchingUp ? String(localized: "Catching up…") : String(localized: "Reading…")
        case .failed: return String(localized: "Not reading")
        case .offline(let since):
            guard let since else { return String(localized: "Offline") }
            return String(localized:
                "Offline since \(since.formatted(date: .omitted, time: .shortened))")
        }
    }

    private func detailText(now: Date) -> String {
        guard let store else {
            return String(localized: "hostler's service is not connected")
        }
        return footerDetailText(lastEventAt: store.lastEventAt, now: now,
                                syncState: store.syncState)
    }
}
