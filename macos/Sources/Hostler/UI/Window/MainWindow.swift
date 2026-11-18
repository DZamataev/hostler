import HostlerKit
import SwiftUI

/// The window's sections (spec §9.3): the top group, then a workspace's detail.
enum WindowSection: Hashable, Sendable {
    case allDevices
    case queue
    case screen
    case accounts
    case secrets
    case projects
    case workspace(String)

    /// The top group, in the sidebar's order.
    static let topLevel: [WindowSection] = [.allDevices, .queue, .screen, .accounts, .secrets,
                                            .projects]

    var id: String {
        switch self {
        case .allDevices: "all-devices"
        case .queue: "queue"
        case .screen: "screen"
        case .accounts: "accounts"
        case .secrets: "secrets"
        case .projects: "projects"
        case .workspace(let id): "workspace:\(id)"
        }
    }

    var title: String {
        switch self {
        case .allDevices: String(localized: "All Devices")
        case .queue: String(localized: "Queue")
        case .screen: String(localized: "Screen")
        case .accounts: String(localized: "Accounts")
        case .secrets: String(localized: "Secrets")
        case .projects: String(localized: "Projects")
        case .workspace: String(localized: "Workspace")
        }
    }

    var icon: WindowIcon {
        switch self {
        case .allDevices: .allDevices
        case .queue: .queue
        case .screen: .screen
        case .accounts: .accounts
        case .secrets: .secrets
        case .projects: .projects
        case .workspace: .projects
        }
    }
}

/// The main window (spec §9.3): a `NavigationSplitView` with the sidebar and, in the detail
/// column, All Devices, a workspace's detail, or the section Task 11 builds.
///
/// `store` is the model Task 8 built; the app passes it once Task 9 joins the connection to it.
/// Without one the window waits, naming the connection's state.
struct MainWindow: View {
    let model: AppModel
    let store: HostlerStore?

    @State private var section: WindowSection? = .allDevices
    @SceneStorage("window.device") private var selectedDeviceID: String?

    init(model: AppModel, store: HostlerStore? = nil) {
        self.model = model
        self.store = store
    }

    var body: some View {
        NavigationSplitView {
            SidebarView(store: store, section: $section)
        } detail: {
            detail
        }
        .navigationSplitViewStyle(.balanced)
        .onAppear { followRequestedSection() }
        .onChange(of: model.requestedSection) { followRequestedSection() }
    }

    /// A notification's click asks for a section (`AppModel.open`): the sidebar follows it, and
    /// the request is taken once so it does not pull the person back later.
    private func followRequestedSection() {
        guard let target = model.requestedSection else { return }
        SectionRouting.apply(target, section: &section, device: &selectedDeviceID)
        model.requestedSection = nil
    }

    @ViewBuilder private var detail: some View {
        if let store {
            let actions = DeviceActions(
                client: client, runner: model.env.commandRunner,
                cliExecutable: store.model.service?.exe)
            switch section ?? .allDevices {
            case .allDevices:
                DevicesView(store: store, actions: actions, selectedDevice: $selectedDeviceID)
            case .workspace(let id):
                WorkspaceDetailView(workspaceID: id, store: store, actions: actions,
                                    openSection: { section = $0 })
            case .queue:
                QueueView(store: store,
                          actions: QueueActions(client: client, gate: model.gate))
            case .screen:
                ScreenView(store: store, actions: ScreenActions(
                    runner: model.env.commandRunner, cliExecutable: store.model.service?.exe))
            case .accounts:
                AccountsView(store: store, actions: secretsActions(store),
                             requests: model.requests, reveal: model.reveal)
            case .secrets:
                SecretsView(store: store, actions: secretsActions(store),
                            requests: model.requests, reveal: model.reveal)
            case .projects:
                ProjectsView(store: store, actions: ProjectsActions(client: client),
                             doctor: actions)
            }
        } else {
            WaitingForStoreView(connection: model.connection?.state)
        }
    }

    /// The client the actions send through (the connection's; a refusing one without it).
    private var client: HostlerClient {
        model.connection.map(\.client) ?? HostlerClient(transport: NotConnectedTransport())
    }

    /// The secrets and accounts actions: the person gate is the app's one (`AppModel.gate`), and
    /// `secrets.status` lists the only preserved files Delete may act on.
    private func secretsActions(_ store: HostlerStore) -> SecretsActions {
        SecretsActions(client: client, gate: model.gate,
                       preservedFiles: store.model.secrets?.preservedFiles ?? [])
    }
}

/// How a notification's click reaches the sidebar (plan Task 11): the queue opens its section,
/// a device opens All Devices with that device selected, and a request's click opens the
/// popover instead (it never comes here).
enum SectionRouting {
    static func apply(_ target: NotificationTarget, section: inout WindowSection?,
                      device: inout String?) {
        switch target {
        case .queue:
            section = .queue
        case .device(let id):
            device = id.rawValue
            section = .allDevices
        case .popover:
            break
        }
    }
}

/// The window before the store exists (or while the service does not answer): the connection's
/// state in one line.
struct WaitingForStoreView: View {
    var connection: ConnectionState?

    var body: some View {
        VStack(spacing: 8) {
            WindowIcon.offline.image
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(.tertiary)
            Text("hostler")
                .font(.title3.weight(.semibold))
            Text(text)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .navigationTitle("hostler")
    }

    private var text: String {
        switch connection {
        case .live:
            return String(localized:
                "hostler's service answered; the window fills in with its next read.")
        case .connecting, nil:
            return String(localized: "Connecting to hostler's service…")
        case .notRunning(.start):
            return String(localized: "hostler's service is not running.")
        case .notRunning(.typeCommand(let command)):
            return String(localized: "hostler's service is not running. Run \(command).")
        case .denied(let reason):
            return String(localized: "The socket cannot be reached: \(reason)")
        case .foreignOwner(let path):
            return String(localized: "The socket at \(path) belongs to another user.")
        case .mismatch(let client, let service, _, let recovery):
            let versions = String(localized:
                "The service speaks contract \(service) and this app speaks \(client).")
            return [versions, recovery].compactMap { $0 }.joined(separator: " ")
        case .protocolError(let text):
            return text
        case .badHome(let home):
            return String(localized: "HOSTLER_HOME must be an absolute path, not \(home).")
        }
    }
}
