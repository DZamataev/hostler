import HostlerKit
import SwiftUI

/// All Devices (spec §9.3, DeviceDeck's table): a sortable table of every device, the standing
/// chips with the holder in its workspace's color, the segmented filters in the toolbar, the bar
/// for a device hostler has not registered yet, and a row that opens the device detail.
struct DevicesView: View {
    let store: HostlerStore
    let actions: DeviceActions
    @Binding var selectedDevice: String?

    // The filters are per window and survive a window's reopening (plan Task 10).
    @SceneStorage("devices.platform") private var platformRaw = PlatformFilter.all.rawValue
    @SceneStorage("devices.kind") private var kindRaw = KindFilter.all.rawValue
    @SceneStorage("devices.showUnregistered") private var showsUnregistered = false

    /// Empty until a person clicks a header: the table then keeps the service's order.
    @State private var sortOrder: [KeyPathComparator<DeviceRow>] = []
    @State private var path: [String] = []
    @State private var addTarget: DeviceSheetItem?
    @State private var renameTarget: DeviceSheetItem?
    @State private var removeTarget: DeviceSheetItem?
    @State private var failure: ActionFailure?

    var body: some View {
        NavigationStack(path: $path) {
            VStack(spacing: 0) {
                if !newOnes.isEmpty {
                    NewDevicesBar(devices: newOnes, add: { addTarget = DeviceSheetItem(device: $0) })
                }
                if rows.isEmpty, !snapshotState.isLoaded {
                    ReadingView(state: snapshotState)
                } else if rows.isEmpty {
                    emptyState
                } else {
                    table
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationTitle("All Devices")
            .navigationSubtitle(snapshotState.isLoaded ? counts.text : "")
            .navigationDestination(for: String.self) { id in
                DeviceDetailView(deviceID: id, store: store, actions: actions)
            }
            .toolbar {
                DeviceToolbar(platform: platform, kind: kind,
                              showsUnregistered: $showsUnregistered, refresh: store.refresh)
            }
            .sheet(item: $addTarget) { item in
                AddDeviceSheet(device: item.device, actions: actions)
            }
            .sheet(item: $renameTarget) { item in
                RenameDeviceSheet(device: item.device, actions: actions)
            }
            .alert(removeTitle, isPresented: removePresented, presenting: removeTarget) { item in
                Button("Remove", role: .destructive) { remove(item.device) }
                Button("Cancel", role: .cancel) {}
            } message: { _ in
                Text("hostler forgets the device. Nothing on the device changes.")
            }
            .actionFailureAlert($failure)
        }
    }

    // MARK: The data

    private var platform: Binding<PlatformFilter> {
        Binding(get: { PlatformFilter(rawValue: platformRaw) ?? .all },
                set: { platformRaw = $0.rawValue })
    }

    private var kind: Binding<KindFilter> {
        Binding(get: { KindFilter(rawValue: kindRaw) ?? .all }, set: { kindRaw = $0.rawValue })
    }

    private var filter: DeviceFilter {
        DeviceFilter(platform: platform.wrappedValue, kind: kind.wrappedValue,
                     showsUnregistered: showsUnregistered)
    }

    private var devices: [DeviceInfo] { store.model.devices?.devices ?? [] }

    private var rows: [DeviceRow] {
        deviceRows(devices, filter: filter, standingOf: { store.standing(of: $0) })
    }

    /// The subtitle counts what the filters show, so it matches the table under it.
    private var counts: DeviceCounts {
        deviceCounts(filter.apply(devices), claims: store.model.claims)
    }

    /// Not-yet-read is not empty (fix R1-6): the table says "reading" until the store has a
    /// devices snapshot.
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.devices != nil, syncState: store.syncState)
    }

    private var newOnes: [DeviceInfo] { newDevices(devices) }

    private var workspaces: [WorkspaceInfo] {
        store.model.projects?.projects.flatMap(\.workspaces) ?? []
    }

    private func reservations(of device: DeviceInfo) -> [WorkspaceInfo] {
        let ids = Set((store.model.claims?.resources.first { $0.resource.id == device.resource }?
            .claims ?? []).filter { $0.kind == .reservation }.map(\.workspace.id))
        return workspaces.filter { ids.contains($0.id) }
    }

    private var removeTitle: String {
        removeTarget.map { String(localized: "Remove \($0.device.displayName)?") }
            ?? String(localized: "Remove the device?")
    }

    private var removePresented: Binding<Bool> {
        Binding(get: { removeTarget != nil }, set: { if !$0 { removeTarget = nil } })
    }

    // MARK: The table

    private var table: some View {
        Table(rows, selection: $selectedDevice, sortOrder: $sortOrder) {
            TableColumn("Device", value: \.nameSortKey) { row in
                DeviceNameCell(row: row, open: { open(row) })
            }
            .width(min: 170, ideal: 260, max: .infinity)

            TableColumn("Type", value: \.typeSortKey) { row in
                Text(row.typeText)
                    .font(.callout)
                    .lineLimit(1)
            }
            .width(min: 88, ideal: 112)

            TableColumn("State", value: \.stateSortRank) { row in
                DeviceStateCell(state: row.state)
            }
            .width(min: 118, ideal: 150)

            TableColumn("Standing", value: \.standingSortKey) { row in
                StandingChipsView(chips: row.chips, emptyText: nil)
                    .accessibilityLabel("Standing of \(row.name)")
            }
            .width(min: 132, ideal: 200)

            TableColumn("Battery", value: \.batterySortKey) { row in
                BatteryCell(row: row)
            }
            .width(min: 74, ideal: 90)

            TableColumn("") { row in
                HStack(spacing: 2) {
                    Button {
                        open(row)
                    } label: {
                        WindowIcon.chevron.image
                            .font(.caption)
                    }
                    .buttonStyle(.plain)
                    .help("Open the device")
                    .accessibilityLabel("Open \(row.name)")
                    .accessibilityIdentifier("device.open.\(row.id)")
                    DeviceRowMenu(
                        row: row, workspaces: workspaces, reserved: reservations(of: row.device),
                        open: { open(row) },
                        rename: { renameTarget = DeviceSheetItem(device: row.device) },
                        reserve: { reserve(row.device.resource, for: $0) },
                        unreserve: { unreserve(row.device.resource, for: $0) },
                        forgetSessions: { forgetSessions(row.device) },
                        remove: { removeTarget = DeviceSheetItem(device: row.device) })
                }
            }
            .width(50)
        }
        .tableStyle(.inset(alternatesRowBackgrounds: false))
        .accessibilityLabel("Devices")
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            WindowIcon.device.image
                .font(.system(size: 36, weight: .light))
                .foregroundStyle(.tertiary)
            Text("No devices")
                .font(.headline)
            Text(showsUnregistered
                 ? String(localized:
                    "hostler has not seen a device yet. Connect a phone or start an emulator.")
                 : String(localized:
                    "No registered device is here, and no new physical device is connected. Turn on Unregistered to see every device hostler found."))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            if !showsUnregistered {
                Button("Show unregistered devices") { showsUnregistered = true }
                    .accessibilityLabel("Show unregistered devices")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: The actions

    private func open(_ row: DeviceRow) {
        selectedDevice = row.id
        path.append(row.id)
    }

    private func reserve(_ resource: ResourceId, for workspace: WorkspaceInfo) {
        Task {
            if let failure = await actions.reserve(resource, for: workspace.id) {
                self.failure = failure
            }
        }
    }

    private func unreserve(_ resource: ResourceId, for workspace: WorkspaceInfo) {
        Task {
            if let failure = await actions.unreserve(resource, for: workspace.id) {
                self.failure = failure
            }
        }
    }

    private func forgetSessions(_ device: DeviceInfo) {
        Task {
            if let failure = await actions.forgetSessions(device) {
                self.failure = failure
            }
        }
    }

    private func remove(_ device: DeviceInfo) {
        removeTarget = nil
        Task {
            if let failure = await actions.remove(device) {
                self.failure = failure
            } else if path.last == device.resource.rawValue {
                path.removeLast()
            }
        }
    }
}

/// A device a sheet works on (sheets need an identity).
struct DeviceSheetItem: Identifiable {
    var device: DeviceInfo
    var id: String { device.resource.rawValue }
}

/// The bar over the table that lists the connected devices hostler has not registered.
struct NewDevicesBar: View {
    var devices: [DeviceInfo]
    var add: (DeviceInfo) -> Void

    var body: some View {
        HStack(spacing: 8) {
            WindowIcon.plus.image
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(devices.count == 1
                 ? String(localized: "A new device is connected — hostler has not registered it")
                 : String(localized:
                    "\(devices.count) new devices are connected — hostler has not registered them"))
                .font(.callout)
                .lineLimit(1)
            Spacer(minLength: 8)
            Menu {
                ForEach(devices, id: \.resource.rawValue) { device in
                    Button(device.displayName) { add(device) }
                        .help(CommandHelp.text(.addDevice, device: device.displayName,
                                               newName: device.displayName))
                        .accessibilityIdentifier("device.add.\(device.resource.rawValue)")
                }
            } label: {
                Label { Text("Add…") } icon: { WindowIcon.plus.image }
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .controlSize(.small)
            .help(CommandHelp.text(.addDevice))
            .accessibilityLabel(devices.count == 1 ? String(localized: "Add the new device")
                : String(localized: "Add a new device"))
            .accessibilityIdentifier("device.add.menu")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(.thinMaterial)
        .overlay(alignment: .bottom) { Divider() }
    }
}

/// The Device column: the kind's icon, the name, and "Android · Pixel 9, Android 16" under it.
struct DeviceNameCell: View {
    var row: DeviceRow
    var open: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            WindowIcon.device(row.device).image
                .font(.title3)
                .foregroundStyle(row.state.isReachable ? AnyShapeStyle(.primary)
                                                       : AnyShapeStyle(.secondary))
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(row.name)
                    .font(.callout.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(row.subtitle)
                    .secondaryCaption()
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2, perform: open)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(row.name), \(row.subtitle), \(row.state.text)")
        .accessibilityHint("Opens the device")
        .accessibilityIdentifier("device.\(row.id)")
    }
}

/// The State column: a dot and "connected · usb" / "shut down" / "booting".
struct DeviceStateCell: View {
    var state: DeviceState

    var body: some View {
        HStack(spacing: 6) {
            StatusDot(color: color)
            Text(state.text)
                .font(.callout)
                .lineLimit(1)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(state.text)
    }

    private var color: Color {
        switch state.phase {
        case .connected: .green
        case .booting: .yellow
        case .shuttingDown, .notConnected: .orange
        case .shutDown: .secondary
        }
    }
}

/// The Battery column: the percentage and a bolt while charging.
struct BatteryCell: View {
    var row: DeviceRow

    var body: some View {
        HStack(spacing: 4) {
            if let text = row.batteryText {
                WindowIcon.battery.image
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(text)
                    .font(.callout)
                    .monospacedDigit()
                if row.isCharging {
                    WindowIcon.bolt.image
                        .font(.caption2)
                        .foregroundStyle(.yellow)
                        .accessibilityHidden(true)
                }
            } else {
                Text("—").foregroundStyle(.tertiary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(row.batteryText ?? String(localized: "battery unknown"))
    }
}

/// The row's ⋯ menu: open, rename, reserve or unreserve for a workspace, forget agent-device's
/// sessions, remove. Every item names its CLI equivalent in its tooltip.
struct DeviceRowMenu: View {
    var row: DeviceRow
    var workspaces: [WorkspaceInfo]
    var reserved: [WorkspaceInfo]
    var open: () -> Void
    var rename: () -> Void
    var reserve: (WorkspaceInfo) -> Void
    var unreserve: (WorkspaceInfo) -> Void
    var forgetSessions: () -> Void
    var remove: () -> Void

    var body: some View {
        Menu {
            Button("Open Device", action: open)
                .accessibilityLabel("Open \(row.name)")
            Button("Rename…", action: rename)
                .help(CommandHelp.text(.renameDevice, device: row.name))
            if row.device.registered, !workspaces.isEmpty {
                Menu("Reserve for") {
                    ForEach(workspaces, id: \.id) { workspace in
                        Button(WorkspaceChip.folderName(of: workspace.path)) {
                            reserve(workspace)
                        }
                        .help(CommandHelp.text(.reserveDevice, device: row.name,
                                               workspace: WorkspaceChip.folderName(of: workspace.path)))
                    }
                }
            }
            ForEach(reserved, id: \.id) { workspace in
                Button("Unreserve \(WorkspaceChip.folderName(of: workspace.path))") {
                    unreserve(workspace)
                }
                .help(CommandHelp.text(.unreserveDevice, device: row.name,
                                       workspace: WorkspaceChip.folderName(of: workspace.path)))
            }
            Button("Forget agent-device sessions", action: forgetSessions)
                .help(CommandHelp.text(.forgetSessions, device: row.name))
            Divider()
            Button("Remove…", role: .destructive, action: remove)
                .help(CommandHelp.text(.removeDevice, device: row.name))
        } label: {
            WindowIcon.ellipsis.image
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Actions for \(row.name)")
        .accessibilityIdentifier("device.menu.\(row.id)")
    }
}

// MARK: - Sheets

/// `devices.add`'s sheet: the person gives the alias.
struct AddDeviceSheet: View {
    let device: DeviceInfo
    let actions: DeviceActions
    @Environment(\.dismiss) private var dismiss
    @State private var alias = ""
    @State private var failure: ActionFailure?
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add \(device.displayName)").font(.headline)
            Text(device.summaryText).secondaryCaption()
            TextField("Alias", text: $alias)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Alias")
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Add") { save() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(isSaving)
                    .accessibilityLabel("Add device")
            }
        }
        .padding(16)
        .frame(width: 380)
        .actionFailureAlert($failure)
        .onAppear { if alias.isEmpty { alias = device.alias ?? "" } }
    }

    private func save() {
        isSaving = true
        Task {
            if let failure = await actions.add(device, alias: alias) {
                self.failure = failure
                isSaving = false
            } else {
                dismiss()
            }
        }
    }
}

/// `devices.rename`'s sheet.
struct RenameDeviceSheet: View {
    let device: DeviceInfo
    let actions: DeviceActions
    @Environment(\.dismiss) private var dismiss
    @State private var alias = ""
    @State private var failure: ActionFailure?
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Rename \(device.displayName)").font(.headline)
            TextField("New alias", text: $alias)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("New alias")
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Rename") { save() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(isSaving || alias.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityLabel("Rename device")
                    .help(CommandHelp.text(.renameDevice, device: device.displayName,
                                           newName: alias))
            }
        }
        .padding(16)
        .frame(width: 380)
        .actionFailureAlert($failure)
        .onAppear { alias = device.alias ?? "" }
    }

    private func save() {
        isSaving = true
        Task {
            if let failure = await actions.rename(device, to: alias) {
                self.failure = failure
                isSaving = false
            } else {
                dismiss()
            }
        }
    }
}
