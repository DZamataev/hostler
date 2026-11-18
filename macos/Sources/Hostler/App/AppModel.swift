import Foundation
import HostlerKit
import Observation

/// Owns the services and the feature controllers, and routes the app's events to them
/// (the reference app's `App/AppModel.swift`, trimmed to what hostler has so far). `@Observable`
/// instead of `ObservableObject` (plan deviation 4).
@MainActor
@Observable
final class AppModel: PanelLifecycle {
    @ObservationIgnored let env: AppEnvironment
    @ObservationIgnored let dockIcon: DockIconCoordinator
    /// The connection to the service (Task 7); `nil` when the environment builds none.
    @ObservationIgnored let connection: ConnectionController?
    /// The store the connection feeds (Task 8), built with the connection; Task 9 wires the
    /// views and the status item to it.
    @ObservationIgnored let store: HostlerStore?

    /// The status item's image. `.offline` until the store (Task 8) derives it with
    /// `StatusItemAppearance.decide`; `start()` keeps it following the store (Task 9).
    var statusItemAppearance: StatusItemAppearance = .offline

    // Task 9: the values features and the app's settings.
    @ObservationIgnored let notifier: UserNotifier
    @ObservationIgnored let limit: ProofLimit
    @ObservationIgnored let gate: PersonGate
    @ObservationIgnored let unlock: UnlockController
    @ObservationIgnored let requests: RequestsController
    @ObservationIgnored let reveal: RevealController
    @ObservationIgnored let loginItem: LoginItemSetting
    @ObservationIgnored let clipboard: ConcealedClipboard
    /// The section a notification click asked the window for (Task 10 navigates to it).
    var requestedSection: NotificationTarget?
    /// Opens the main window; set by the popover when it shows (a SwiftUI `openWindow` action
    /// is only reachable from a view).
    @ObservationIgnored var openMainWindow: (@MainActor () -> Void)?
    @ObservationIgnored private var tick: Task<Void, Never>?

    /// The app's status item and popover; `nil` in tests and before `start()`.
    @ObservationIgnored private(set) var statusItem: (any StatusItemHosting)?
    @ObservationIgnored private(set) var hasStarted = false
    @ObservationIgnored private(set) var hasShutDown = false

    init(env: AppEnvironment) {
        self.env = env
        dockIcon = DockIconCoordinator(policy: env.activationPolicy)
        let connection = env.connection.map { ConnectionController(environment: $0) }
        self.connection = connection
        let notifier = UserNotifier(center: env.notificationCenter, store: env.notificationSettings)
        self.notifier = notifier
        let store = connection.map {
            HostlerStore(client: $0.client, clock: env.clock, notifier: notifier,
                         settings: notifier.settings)
        }
        self.store = store
        connection?.sink = store
        notifier.onSettingsChange = { [weak store] in store?.notificationSettings = $0 }

        // Task 9: one client for the values features, the connection's (nothing connected
        // without one).
        let client = connection?.client ?? HostlerClient(transport: NoServiceTransport())
        let log = OSValuesLog()
        limit = ProofLimit(clock: env.clock)
        gate = PersonGate(client: client, clock: env.clock, limit: limit,
                          grant: { [weak connection] in connection?.grant }, log: log)
        unlock = UnlockController(client: client, limit: limit, openPanel: env.openPanel, log: log)
        requests = RequestsController(client: client, gate: gate, log: log)
        clipboard = ConcealedClipboard(pasteboard: env.pasteboard, sleeper: env.sleeper)
        reveal = RevealController(gate: gate, clipboard: clipboard, sleeper: env.sleeper, log: log)
        loginItem = LoginItemSetting(item: env.loginItem, appURL: env.appURL)
        notifier.onOpen = { [weak self] target in self?.open(target) }
    }

    /// What the banner's Start button runs (`ConnectionController.startService(with:)`); it
    /// is never called by a state change.
    var serviceStarter: any ServiceStarting {
        CLIServiceStarter(runner: env.commandRunner, workingDirectory: NSHomeDirectory())
    }

    /// Called once at launch (never while hosting tests: `AppDelegate`).
    func start() {
        hasStarted = true
        // The app's own status item: the scene graph deliberately shows none (the hidden
        // `MenuBarExtra` anchor only keeps launch window-free), so the panel is built here.
        if statusItem == nil, let makeStatusItem = env.makeStatusItem {
            statusItem = makeStatusItem(self)
        }
        connection?.start()
        // Task 9: notifications, the store's view of the world, the minute tick (P5-L9).
        notifier.start()
        followStore()
        if let store, tick == nil {
            let sleeper = env.sleeper
            tick = Task { [weak store] in
                while !Task.isCancelled {
                    do { try await sleeper.sleep(for: AppModel.tickInterval) } catch { return }
                    store?.tick()
                }
            }
        }
    }

    /// How often the store re-evaluates its time-based rules (a stuck task).
    static let tickInterval: Duration = .seconds(60)

    /// Before the app quits: the connection closes (the store hears its `dropped`), then the
    /// store's own work stops.
    func shutdown() async {
        // Synchronously, before the first suspension: nothing typed or shown outlives the
        // decision to quit (review t9-r1 R1-3).
        tick?.cancel()
        tick = nil
        unlock.reset()
        requests.clearAll()
        gate.cancel()
        reveal.hide()
        clipboard.clearIfStillOurs()
        await connection?.stop()
        store?.shutdown()
        hasShutDown = true
    }

    // MARK: - The store (Task 9)

    /// Keeps the status item's image, the reveal and the request cards' fields following the
    /// store: re-armed on every change it reads (Observation).
    private func followStore() {
        guard let store else { return }
        let (appearance, lockState, pending) = withObservationTracking {
            (store.statusItemAppearance, store.model.secrets?.state,
             Set(RequestsController.cards(store.model).map(\.id)))
        } onChange: { [weak self] in
            Task { @MainActor in self?.followStore() }
        }
        if statusItemAppearance != appearance { statusItemAppearance = appearance }
        reveal.lockStateChanged(lockState)
        requests.prune(keeping: pending)
    }

    /// The popover opened: a connection that is not live retries at once (P5-L12).
    func popoverOpened() {
        guard let connection else { return }
        if case .live = connection.state { return }
        connection.retryNow()
    }

    /// The popover closed (`StatusItemController`'s close path, Task 13 F-1): nothing typed in
    /// it stays (spec §9.6), its unlock form closes (unless the key-file chooser closed the
    /// popover on purpose), a gate it shows is cancelled, and a reveal it shows hides.
    func popoverClosed() {
        unlock.reset()
        if !choosingKeyFile { unlock.isFormOpen = false }
        requests.clearAll()
        gate.dismissed(.popover)
        reveal.dismissed(presenter: .popover)
    }

    /// A notification's click (spec §9.4): requests open the popover; a device, the queue or
    /// a seizure open the window's section.
    func open(_ target: NotificationTarget) {
        switch target {
        case .popover:
            statusItem?.revealPanel()
        case .device, .queue:
            requestedSection = target
            if windowRaiser.hasEligibleWindows {
                windowRaiser.raiseAll()
            } else if let openMainWindow {
                openMainWindow()
            } else {
                statusItem?.revealPanel()
            }
        }
    }

    // MARK: - Windows

    /// The raiser the app was built with (tests: a fake).
    var windowRaiser: any WindowRaising { env.windowRaiser }

    /// The key-file chooser closes the popover (a click in the panel would anyway), then
    /// brings it back with the form still open (the reference app's `transcribeFile` pattern).
    func chooseKeyFile() async {
        choosingKeyFile = true
        closeMenuPanel()
        await unlock.chooseKeyFile()
        choosingKeyFile = false
        statusItem?.revealPanel()
    }

    /// The key-file chooser is up: the popover's close keeps the unlock form open.
    @ObservationIgnored private var choosingKeyFile = false

    /// Closes the popover, called by the panel's commands before they route to a window;
    /// a no-op when nothing is shown.
    func closeMenuPanel() {
        statusItem?.closePanel()
    }

    /// Opening the running app again (Finder, Spotlight, Launchpad, `open`): windows that were
    /// not in front come forward first; with none, or with them already in front, the popover
    /// opens — the way back to it when the status item is hidden (the reference app's
    /// `AppModel.reopen(wasActive:)`, R1-9).
    func reopen(wasActive: Bool) {
        if !wasActive, windowRaiser.hasEligibleWindows {
            windowRaiser.raiseAll()
        } else {
            statusItem?.revealPanel()
        }
    }
}

/// The values features' transport when the environment builds no connection (tests): every
/// call fails as not connected.
struct NoServiceTransport: HostlerTransport {
    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        throw .notConnected
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        throw .notConnected
    }
}
