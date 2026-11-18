import AppKit
import HostlerKit
import SwiftUI

/// The status item's popover (spec §9.2), top to bottom: the header; the connection banner
/// instead of everything else while the service is not reachable; the secrets database with
/// Unlock or Lock; the pending request cards; then, in the only region that scrolls
/// (the reference app's `MenuPanelView` rule), the heavy queue, devices and the screen in short and
/// the commands. 320 pt wide (spec §9.2).
///
/// The host rebuilds it on every show, so `@State` starts fresh each opening; everything typed
/// is cleared when it closes (`AppModel.popoverClosed()`, from the controller's close path).
struct PopoverView: View {
    let model: AppModel
    /// The status item's screen caps the panel on a short display; refreshed on every show.
    let maxHeight: CGFloat

    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    @State private var confirmingLock = false
    @State private var startError: HostlerAppError?
    @State private var starting = false

    private var connectionState: ConnectionState { model.connection?.state ?? .connecting }
    private var isLive: Bool {
        if case .live = connectionState { return true }
        return false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
                .padding(.horizontal, 12)
                .padding(.top, 12)
                .padding(.bottom, 8)

            if let banner = PopoverPresentation.banner(connectionState) {
                bannerView(banner)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 8)
            } else {
                fixedValues
            }

            // Only this region scrolls once the height cap is reached.
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if isLive {
                        scrollingCards
                        liveSummary
                        divider
                    }
                    commandRows
                }
                .padding(.bottom, 10)
            }
        }
        .frame(width: 320, alignment: .leading)
        .frame(maxHeight: max(maxHeight, 0), alignment: .top)
        // The open and close hooks run from `StatusItemController`'s show and close paths
        // (Task 13 F-1); the root is rebuilt per show, so this runs on every show.
        .onAppear {
            model.openMainWindow = { [openWindow] in openWindow(id: WindowID.main) }
        }
    }

    // MARK: - Header (§9.2 item 1)

    private var header: some View {
        let (dot, text) = PopoverPresentation.header(connectionState)
        return HStack(spacing: 6) {
            Text("hostler").font(.headline)
            Circle()
                .fill(dot == .live ? Color.green : dot == .refused ? Color.red : Color.gray)
                .frame(width: 7, height: 7)
                .accessibilityHidden(true)
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
            Button {
                routeAfterClosing { openWindow(id: WindowID.main) }
            } label: {
                Icon.window.image
            }
            .buttonStyle(.borderless)
            .help(String(localized: "Open hostler"))
            .accessibilityLabel(String(localized: "Open hostler"))
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(String(localized: "hostler, \(text)"))
    }

    // MARK: - Banner (§9.2 item 2, §9.7)

    private func bannerView(_ banner: PopoverPresentation.Banner) -> some View {
        ValueBox {
            Text(banner.title).font(.callout.weight(.semibold))
            if let detail = banner.detail {
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ValueErrorLines(startError)
            if !banner.actions.isEmpty {
                HStack {
                    if starting {
                        ProgressView().controlSize(.small)
                            .accessibilityLabel(String(localized: "Starting the service"))
                    }
                    Spacer()
                    ForEach(Array(banner.actions.enumerated()), id: \.offset) { _, action in
                        bannerButton(action)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func bannerButton(_ action: PopoverPresentation.BannerAction) -> some View {
        switch action {
        case .start:
            Button(String(localized: "Start")) { start() }
                .disabled(starting)
                .help(String(localized: "Runs the registered hostler's service start"))
                .accessibilityLabel(String(localized: "Start hostler's service"))
        case .retry:
            Button(String(localized: "Retry")) { model.connection?.retryNow() }
                .accessibilityLabel(String(localized: "Retry connecting"))
        case .copy(let text):
            Button(String(localized: "Copy")) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(text, forType: .string)
            }
            .help(text)
            .accessibilityLabel(String(localized: "Copy the command"))
        }
    }

    private func start() {
        guard let connection = model.connection else { return }
        starting = true
        startError = nil
        Task {
            if case .failure(let error) = await connection.startService(with: model.serviceStarter) {
                startError = error
            }
            starting = false
        }
    }

    // MARK: - The lock, the gate, the first cards (§9.2 items 3–4)

    private var cards: [RequestCard] {
        model.store.map { RequestsController.cards($0.model) } ?? []
    }

    @ViewBuilder
    private var fixedValues: some View {
        VStack(alignment: .leading, spacing: 8) {
            if model.gate.isAsking, model.gate.presenter == .popover {
                ValueBox { PersonGateForm(gate: model.gate) }
            }
            lockRow
            if model.unlock.isFormOpen, lockState != .unlocked {
                UnlockView(unlock: model.unlock, creates: lockState == .missing) {
                    Task { await model.chooseKeyFile() }
                }
            }
            ForEach(cards.prefix(PopoverPresentation.fixedCards)) { card in
                RequestCardView(card: card, requests: model.requests,
                                projects: model.store?.model.projects)
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
    }

    private var lockState: LockState? { model.store?.model.secrets?.state }

    @ViewBuilder
    private var lockRow: some View {
        HStack(spacing: 6) {
            Image(systemName: lockState == .unlocked ? "lock.open" : "lock")
                .frame(width: 16)
                .accessibilityHidden(true)
            Text(PopoverPresentation.lockText(lockState))
            Spacer()
            switch lockState {
            case .unlocked?:
                if !confirmingLock {
                    Button(String(localized: "Lock")) { confirmingLock = true }
                        .controlSize(.small)
                        .accessibilityLabel(String(localized: "Lock the secrets database"))
                }
            case .locked?, .missing?:
                if !model.unlock.isFormOpen {
                    Button(lockState == .missing ? String(localized: "Create…") : String(localized: "Unlock…")) {
                        model.unlock.isFormOpen = true
                    }
                    .controlSize(.small)
                    .accessibilityLabel(lockState == .missing
                                        ? String(localized: "Create the secrets database")
                                        : String(localized: "Unlock the secrets database"))
                }
            case nil:
                EmptyView()
            }
        }
        .accessibilityElement(children: .contain)
        if confirmingLock, lockState == .unlocked {
            // Inline, not an alert: a sheet on the popover is another window, and a click in
            // it would close the popover.
            ValueBox {
                Text(String(localized: "Lock the secrets database?")).font(.callout.weight(.semibold))
                Text(PopoverPresentation.lockWarning).font(.caption).foregroundStyle(.secondary)
                ValueErrorLines(model.unlock.error)
                HStack {
                    Spacer()
                    Button(String(localized: "Cancel")) { confirmingLock = false }
                        .accessibilityLabel(String(localized: "Keep it unlocked"))
                    Button(String(localized: "Lock")) {
                        Task {
                            if await model.unlock.lock() { confirmingLock = false }
                        }
                    }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityLabel(String(localized: "Lock now"))
                }
            }
        }
    }

    // MARK: - The scrolling region (§9.2 items 4–8)

    @ViewBuilder
    private var scrollingCards: some View {
        let rest = cards.dropFirst(PopoverPresentation.fixedCards)
        if !rest.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(rest)) { card in
                    RequestCardView(card: card, requests: model.requests,
                                    projects: model.store?.model.projects)
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 8)
        }
    }

    @ViewBuilder
    private var liveSummary: some View {
        if let store = model.store {
            TimelineView(.periodic(from: .now, by: 30)) { context in
                let nowMs = RequestCardView.ms(context.date)
                VStack(alignment: .leading, spacing: 8) {
                    if case .syncing = store.syncState {
                        PopoverCaption(text: String(localized: "Reading hostler…"))
                    }
                    queueSection(store.model.queue, nowMs: nowMs)
                    devicesSection(store)
                    Text(PopoverPresentation.screen(store.model.claims, formatTime: Self.time))
                        .font(.callout)
                        .accessibilityLabel(PopoverPresentation.screen(store.model.claims,
                                                                       formatTime: Self.time))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 4)
            }
        }
    }

    private func queueSection(_ queue: QueueSnapshot?, nowMs: Int64) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            PopoverCaption(text: PopoverPresentation.queueSummary(queue))
            ForEach((queue?.running ?? []) + (queue?.foreignRunning ?? []), id: \.id) { entry in
                HStack(spacing: 6) {
                    Text(entry.command)
                        .font(.system(.caption, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    Text(PopoverPresentation.workspaceName(entry))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Text(PopoverPresentation.elapsed(sinceMs: entry.grantedAtMs, nowMs: nowMs))
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
        }
    }

    private func devicesSection(_ store: HostlerStore) -> some View {
        let (shown, more) = PopoverPresentation.devices(store.model.devices?.devices ?? [],
                                                        standing: { store.standing(of: $0) })
        return VStack(alignment: .leading, spacing: 3) {
            PopoverCaption(text: shown.isEmpty ? String(localized: "No devices") : String(localized: "Devices"))
            ForEach(shown, id: \.resource) { device in
                deviceRow(device, standing: store.standing(of: device))
            }
            if let more {
                Button(more) { routeAfterClosing { openWindow(id: WindowID.main) } }
                    .buttonStyle(.link)
                    .font(.caption)
                    .accessibilityLabel(String(localized: "\(more): open hostler"))
            }
        }
    }

    private func deviceRow(_ device: DeviceInfo, standing: [DeviceStanding]) -> some View {
        HStack(spacing: 6) {
            Circle()
                .fill(device.connected ? (device.ready ? Color.green : Color.orange) : Color.gray)
                .frame(width: 7, height: 7)
                .accessibilityHidden(true)
            Text(PopoverPresentation.deviceName(device)).lineLimit(1)
            if standing.contains(where: { if case .testing = $0 { true } else { false } }) {
                Image(systemName: "hand.tap")
                    .foregroundStyle(.secondary)
                    .help(String(localized: "An agent-device session is testing on it"))
                    .accessibilityLabel(String(localized: "testing"))
            }
            Spacer(minLength: 4)
            ForEach(PopoverPresentation.holders(standing), id: \.self) { chip in
                Text(chip.name)
                    .font(.caption2)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(Capsule().fill((chip.color?.color ?? .gray).opacity(0.25)))
                    .lineLimit(1)
            }
            if let battery = PopoverPresentation.battery(device) {
                Text(battery).font(.caption).monospacedDigit().foregroundStyle(.secondary)
            }
        }
        .font(.callout)
        .accessibilityElement(children: .combine)
    }

    static func time(_ ms: Int64) -> String {
        Date(timeIntervalSince1970: Double(ms) / 1000).formatted(date: .omitted, time: .shortened)
    }

    // MARK: - Commands (§9.2 item 8)

    private var commandRows: some View {
        VStack(alignment: .leading, spacing: 1) {
            PopoverCommandRow(icon: .window, title: String(localized: "Open hostler"), shortcut: "⌘O") {
                routeAfterClosing { openWindow(id: WindowID.main) }
            }
            .keyboardShortcut("o")
            PopoverCommandRow(icon: .settings, title: String(localized: "Settings…"), shortcut: "⌘,") {
                routeAfterClosing { openSettings() }
            }
            .keyboardShortcut(",")
            PopoverCommandRow(icon: .quit, title: String(localized: "Quit hostler"), shortcut: "⌘Q") {
                NSApp.terminate(nil)
            }
            .keyboardShortcut("q")
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
    }

    private var divider: some View {
        Divider().padding(.horizontal, 6).padding(.vertical, 4)
    }

    /// Closes the panel, activates the app, then routes (the reference app): the window's activation
    /// must not race the popover's dismissal.
    private func routeAfterClosing(_ route: () -> Void) {
        model.closeMenuPanel()
        NSApp.activate(ignoringOtherApps: true)
        route()
    }
}
