import AppKit
import HostlerKit
import SwiftUI

/// Single, main-actor-isolated instance shared by the SwiftUI scenes and the app delegate
/// (the reference app's `AppRoot`).
@MainActor
enum AppRoot {
    /// The launch hooks exist in Debug builds only (lead ruling P5-L5, R1-2): `LaunchOptions`
    /// compiles them in Debug and nowhere else.
    static let launch = LaunchOptions(environment: ProcessInfo.processInfo.environment)
    static let model = AppModel(env: .live(launch: launch))
}

/// the reference app's `AppDelegate` (its app file), with hostler's launch rules:
/// nothing starts while hosting tests, and the debug popover hook (lead ruling P5-L5).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    typealias Scheduler = @MainActor (Duration, @escaping @MainActor () -> Void) -> Void

    private let model: AppModel
    private let launch: LaunchOptions
    private let replyToTermination: @MainActor (Bool) -> Void
    private let isAppActive: @MainActor () -> Bool
    private let now: () -> Date
    private let schedule: Scheduler
    private var becameActiveAt: Date?

    override convenience init() {
        self.init(model: AppRoot.model, launch: AppRoot.launch,
                  replyToTermination: { NSApp.reply(toApplicationShouldTerminate: $0) })
    }

    init(model: AppModel, launch: LaunchOptions,
         replyToTermination: @escaping @MainActor (Bool) -> Void,
         isAppActive: @escaping @MainActor () -> Bool = { NSApp.isActive },
         now: @escaping () -> Date = { Date() },
         schedule: @escaping Scheduler = AppDelegate.afterDelay) {
        self.model = model
        self.launch = launch
        self.replyToTermination = replyToTermination
        self.isAppActive = isAppActive
        self.now = now
        self.schedule = schedule
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Hosted tests launch this app as their host: then it touches nothing of the system —
        // no status item, no notification or login-item call, no connection (P5-L5).
        guard launch.startsTheApp else { return }
        model.start()
        if launch.openPopoverAtLaunch {
            // Debug only. Not now: the status button's window is not laid out yet and the
            // popover would not show (Task 0 facts §2.7).
            schedule(LaunchOptions.openPopoverDelay) { [model] in
                model.statusItem?.revealPanel()
            }
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        Task { [model, replyToTermination] in
            await model.shutdown()
            replyToTermination(true)
        }
        return .terminateLater
    }

    /// The status item is the app's permanent surface and launch shows no window at all;
    /// closing the last window must leave the app running (spec §9.1).
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// Opening the running app again; `false` keeps AppKit from opening a window of its own.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        // The reopening click activates the app just before this arrives, so an activation
        // that recent means the app was in the background a moment ago.
        // shortcut: a 0.5 s window on activation order, revisit if a slow reopen is misread.
        let justActivated = becameActiveAt.map { now().timeIntervalSince($0) < 0.5 } ?? false
        model.reopen(wasActive: isAppActive() && !justActivated)
        return false
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        becameActiveAt = now()
    }

    static func afterDelay(_ delay: Duration, _ work: @escaping @MainActor () -> Void) {
        Task { @MainActor in
            try? await Task.sleep(for: delay)
            work()
        }
    }
}

@main
struct HostlerApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let model = AppRoot.model

    var body: some Scene {
        // The first scene is deliberately a hidden extra: without a `MenuBarExtra`, SwiftUI
        // opens the first `Settings`/`Window` scene at launch, and the hidden one keeps
        // launch window-free (spec §9.1; Task 0 facts §1). The real status item is app-owned —
        // `StatusItemController`, built from `AppModel.start()` — so no icon comes from
        // here; this scene only creates a zero-height status window.
        MenuBarExtra("hostler", isInserted: .constant(false)) {
            EmptyView()
        }

        Window("hostler", id: WindowID.main) {
            // Task 9 (P5-L15): the live store, and the person gate as a sheet of this window.
            MainWindow(model: model, store: model.store)
                // The toolbar's two segmented filters, the Unregistered toggle and Refresh fit
                // in both languages at this width (the Russian kind filter is the widest text);
                // below it the window is not offered, so no control ever folds into the
                // toolbar's overflow by accident.
                .frame(minWidth: 1100, minHeight: 620)
                .personGateSheet(model.gate)
                .onAppear { model.dockIcon.open(.main) }
                .onDisappear { model.dockIcon.close(.main) }
        }
        .defaultSize(width: 1100, height: 720)
        // Debug builds only: Debug › Reveal Popover (P5-L6). Empty in Release.
        .commands { DebugCommands(model: model) }

        // `Settings` alone could resolve to a type of ours later, so qualify the scene.
        SwiftUI.Settings {
            SettingsView(model: model)
                .onAppear { model.dockIcon.open(.settings) }
                .onDisappear { model.dockIcon.close(.settings) }
        }
    }
}
