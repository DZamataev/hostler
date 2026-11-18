import Testing
@testable import HostlerKit

/// The rule `AppKitWindowRaiser` raises by on reopen (the reference app's
/// `Tests/Services/WindowEligibilityTests.swift`, with hostler's kinds).
@Suite struct WindowEligibilityTests {
    private func window(_ kind: WindowSnapshot.Kind, visible: Bool = true,
                        miniaturized: Bool = false) -> WindowSnapshot {
        WindowSnapshot(kind: kind, isVisible: visible, isMiniaturized: miniaturized)
    }

    @Test func theMainWindowAndSettingsAreEligibleWhenVisible() {
        #expect(WindowEligibility.isEligible(window(.main)))
        #expect(WindowEligibility.isEligible(window(.settings)))
    }

    @Test func aMiniaturizedWindowIsEligible() {
        #expect(WindowEligibility.isEligible(window(.main, visible: false, miniaturized: true)))
    }

    @Test func aClosedRetainedWindowIsNot() {
        #expect(!WindowEligibility.isEligible(window(.main, visible: false)))
        #expect(!WindowEligibility.isEligible(window(.settings, visible: false)))
    }

    @Test func theStatusItemPopoverSheetsChoosersAndUnknownWindowsNeverAre() {
        for kind in [WindowSnapshot.Kind.statusItem, .popover, .sheet, .panelChooser, .other] {
            #expect(!WindowEligibility.isEligible(window(kind)), "\(kind)")
        }
    }
}
