import AppKit
import HostlerKit
import Testing
@testable import Hostler

/// These run inside the real app, launched by xcodebuild as the test host.
@Suite @MainActor struct HostedLaunchTests {
    @Test func theTestsAreHostedInTheHostlerApp() {
        #expect(Bundle.main.bundleIdentifier == "io.github.dzamataev.hostler.app")
    }

    /// Lead ruling P5-L5 (3): the host app knows it hosts tests and started nothing — no
    /// status item, so no popover, notification or login-item call.
    @Test func theHostAppStartedNothing() {
        #expect(AppRoot.launch.isHostingTests)
        #expect(AppRoot.model.statusItem == nil)
        #expect(AppRoot.model.hasStarted == false)
    }

    /// HostlerKit as Xcode builds it for the app (a package target, Debug configuration) has
    /// the hooks compiled in; `scripts/macos.sh app` shows the Release build has not (R1-2).
    @Test func theAppsDebugBuildOfHostlerKitReadsTheHooks() {
        let options = LaunchOptions(environment: ["HOSTLER_APP_TEST_OPEN_POPOVER": "1",
                                                  "HOSTLER_APP_TEST_KEEP_POPOVER": "1"])
        #expect(options.openPopoverAtLaunch)
        #expect(options.keepPopoverOnResignActive)
    }

    /// The bundle carries the status item's template glyph (the reference app's invariant 17: the
    /// folder is declared in project.yml).
    @Test func theStatusItemGlyphIsBundled() {
        #expect(StatusItemImages.bundledGlyphURL() != nil)
    }

    /// Every state is an 18 × 18 template image, so a state change never changes the item's
    /// width and macOS picks the contrast.
    @Test func everyStateIsAnEqualSizedTemplateImage() {
        for appearance in StatusItemAppearance.allCases {
            let image = StatusItemImages.image(for: appearance)
            #expect(image.isTemplate, "\(appearance)")
            #expect(image.size == NSSize(width: 18, height: 18), "\(appearance)")
        }
    }

    @Test func eachStateHasItsOwnAccessibilityLabel() {
        let labels = Set(StatusItemAppearance.allCases.map(StatusItemImages.accessibilityLabel))
        #expect(labels.count == StatusItemAppearance.allCases.count)
    }
}
