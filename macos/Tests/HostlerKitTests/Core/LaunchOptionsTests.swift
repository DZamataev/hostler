import Testing
@testable import HostlerKit

/// What the app does at launch, read from its environment (lead rulings P5-L5, fix round 1
/// R1-2). The hooks exist only in Debug builds; `swift test` and the hosted tests build Debug,
/// and `scripts/macos.sh app` checks the Release binary holds neither hook name.
@Suite struct LaunchOptionsTests {
    @Test func aPlainLaunchStartsAndHasNoHooks() {
        let options = LaunchOptions(environment: [:])
        #expect(options == LaunchOptions(isHostingTests: false, openPopoverAtLaunch: false,
                                         keepPopoverOnResignActive: false))
        #expect(options.startsTheApp)
    }

    /// Hosted tests launch the real app as their host; it must not touch the system then.
    /// This rule holds in every build, Release included.
    @Test func anXCTestHostIsRecognised() {
        let options = LaunchOptions(
            environment: ["XCTestConfigurationFilePath": "/tmp/x.xctestconfiguration"])
        #expect(options.isHostingTests)
        #expect(options.startsTheApp == false)
    }

    #if DEBUG
    @Test func theHookNamesAreTheDocumentedOnes() {
        #expect(LaunchOptions.openPopoverVariable == "HOSTLER_APP_TEST_OPEN_POPOVER")
        #expect(LaunchOptions.keepPopoverVariable == "HOSTLER_APP_TEST_KEEP_POPOVER")
    }

    @Test func theDebugHooksAreReadInADebugBuild() {
        let options = LaunchOptions(
            environment: ["HOSTLER_APP_TEST_OPEN_POPOVER": "1",
                          "HOSTLER_APP_TEST_KEEP_POPOVER": "1"])
        #expect(options.openPopoverAtLaunch)
        #expect(options.keepPopoverOnResignActive)
        #expect(options.startsTheApp)
    }

    @Test func eachHookIsReadOnItsOwn() {
        let open = LaunchOptions(environment: ["HOSTLER_APP_TEST_OPEN_POPOVER": "1"])
        #expect(open.openPopoverAtLaunch && !open.keepPopoverOnResignActive)
        let keep = LaunchOptions(environment: ["HOSTLER_APP_TEST_KEEP_POPOVER": "1"])
        #expect(!keep.openPopoverAtLaunch && keep.keepPopoverOnResignActive)
    }

    @Test func onlyTheValueOneEnablesAHook() {
        for value in ["0", "", "true", "yes", " 1"] {
            let options = LaunchOptions(
                environment: ["HOSTLER_APP_TEST_OPEN_POPOVER": value,
                              "HOSTLER_APP_TEST_KEEP_POPOVER": value])
            #expect(!options.openPopoverAtLaunch, "\(value)")
            #expect(!options.keepPopoverOnResignActive, "\(value)")
        }
    }
    #endif

    /// Task 0 facts §2.7: revealing straight from `applicationDidFinishLaunching` shows
    /// nothing; one second later it opens.
    @Test func thePopoverHookWaitsOneSecond() {
        #expect(LaunchOptions.openPopoverDelay == .seconds(1))
    }
}
