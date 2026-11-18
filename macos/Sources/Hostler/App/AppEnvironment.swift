import AppKit
import HostlerKit

/// The composition root (the reference app's `App/AppEnvironment.swift`): the only place concrete
/// services are built (the reference app's invariant 1). Tests build `AppEnvironment.fake()` instead.
/// Later tasks add their services here: the connection (Task 7), the store's notifier
/// (Task 8), the pasteboard, login item and open panel (Task 9).
struct AppEnvironment {
    /// Builds the status item and popover; `nil` builds none (a test that does not need one).
    var makeStatusItem: (@MainActor (AppModel) -> any StatusItemHosting)?
    var windowRaiser: any WindowRaising
    var activationPolicy: any ActivationPolicyControlling
    /// The service connection's OS side (Task 7); `nil` builds no connection (tests).
    var connection: ConnectionEnvironment?
    /// The `hostler` CLI as a subprocess (Task 7): takeovers, doctor, Start.
    var commandRunner: any HostlerCommandRunning
    /// Task 9: the notification center and where the notification settings are kept.
    var notificationCenter: any UserNotificationCentering
    var notificationSettings: any NotificationSettingsStoring
    /// Task 9: the key-file chooser, the pasteboard, the login item, the clock and the timers
    /// of the values features (the reveal's 30 s, the pasteboard's 60 s).
    var openPanel: any OpenPanelPresenting
    var pasteboard: any PasteboardWriting
    var loginItem: any LoginItemControlling
    var clock: any WallClock
    var sleeper: any Sleeping
    /// The running app's bundle: where it is decides whether it can open at login (F-9).
    var appURL: URL

    @MainActor
    static func live(launch: LaunchOptions) -> AppEnvironment {
        AppEnvironment(
            makeStatusItem: { model in
                StatusItemController(model: model,
                                     dismissesOnResignActive: !launch.keepPopoverOnResignActive)
            },
            windowRaiser: AppKitWindowRaiser(),
            activationPolicy: NSAppActivationPolicy(),
            connection: .live(appVersion: Bundle.main.object(
                forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"),
            commandRunner: HostlerCLI(),
            notificationCenter: SystemNotificationCenter(),
            notificationSettings: DefaultsNotificationSettings(),
            openPanel: NSOpenPanelPresenter(),
            pasteboard: SystemPasteboard(),
            loginItem: MainAppLoginItem(),
            clock: SystemClock(),
            sleeper: TaskSleeper(),
            appURL: Bundle.main.bundleURL)
    }
}
