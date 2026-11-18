import Foundation

/// What the app does at launch, read once from its environment (lead ruling P5-L5).
public struct LaunchOptions: Equatable, Sendable {
    /// The app was launched as the host of `HostlerTests`. It then starts nothing: no status
    /// item, no notification or login-item call, no connection.
    public var isHostingTests: Bool
    /// Debug only: `HOSTLER_APP_TEST_OPEN_POPOVER=1` reveals the popover a moment after launch,
    /// for a UI driver that cannot click a status item hidden by a menu-bar manager (Task 0
    /// facts §2).
    public var openPopoverAtLaunch: Bool
    /// Debug only: `HOSTLER_APP_TEST_KEEP_POPOVER=1` keeps the popover open when the app
    /// resigns active, so a foreground UI driver can read the result of its press. Every other
    /// dismissal path stays.
    public var keepPopoverOnResignActive: Bool

    /// Revealing straight from `applicationDidFinishLaunching` shows nothing (the status
    /// button's window is not laid out yet); one second later it opens (Task 0 facts §2.7).
    public static let openPopoverDelay: Duration = .seconds(1)

    #if DEBUG
    // Debug builds only (R1-2): a Release binary holds neither name, and
    // `scripts/macos.sh app` fails if it does.
    public static let openPopoverVariable = "HOSTLER_APP_TEST_OPEN_POPOVER"
    public static let keepPopoverVariable = "HOSTLER_APP_TEST_KEEP_POPOVER"
    #endif
    /// Set by Xcode's test runner in the host app's environment.
    public static let xcTestVariable = "XCTestConfigurationFilePath"

    public init(isHostingTests: Bool, openPopoverAtLaunch: Bool, keepPopoverOnResignActive: Bool) {
        self.isHostingTests = isHostingTests
        self.openPopoverAtLaunch = openPopoverAtLaunch
        self.keepPopoverOnResignActive = keepPopoverOnResignActive
    }

    /// A Release build never reads the hooks (they are not compiled in), so its behaviour is
    /// exactly the reference app's. The XCTest rule holds in every build.
    public init(environment: [String: String]) {
        isHostingTests = environment[Self.xcTestVariable] != nil
        #if DEBUG
        openPopoverAtLaunch = environment[Self.openPopoverVariable] == "1"
        keepPopoverOnResignActive = environment[Self.keepPopoverVariable] == "1"
        #else
        openPopoverAtLaunch = false
        keepPopoverOnResignActive = false
        #endif
    }

    /// Whether launch builds the status item and starts the model.
    public var startsTheApp: Bool { !isHostingTests }
}
