import Foundation
import HostlerKit
import Testing
@testable import Hostler

/// Lead ruling P5-L6 (5): a Debug-only "Debug › Reveal Popover" command in the main menu, so a
/// UI driver can open the popover in a running app whose main window is open. The hosted tests
/// run the Debug build; `scripts/macos.sh app` checks the Release binary has no such command.
@Suite @MainActor struct DebugCommandsTests {
    @Test func theDebugBuildHasTheCommand() {
        #expect(DebugCommands.isAvailable)
        #expect(DebugCommands.menuTitle == "Debug")
        #expect(DebugCommands.revealTitle == "Reveal Popover")
    }

    /// `scripts/macos.sh app` greps the Release binary for the Debug menu's marker and both
    /// launch-hook names (R1-2), byte-wise.
    @Test func theReleaseCheckLooksForEveryDebugOnlyMarker() throws {
        let script = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("scripts/macos.sh")
        let text = try String(contentsOf: script, encoding: .utf8)
        #expect(text.contains("    \(DebugCommands.marker)\n"))
        #expect(text.contains("    \(LaunchOptions.openPopoverVariable)\n"))
        #expect(text.contains("    \(LaunchOptions.keepPopoverVariable)\n"))
        #expect(text.contains("LC_ALL=C grep -rqaF \"$marker\""))
    }

    @Test func revealPopoverRevealsTheSamePanel() {
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }))
        model.start()

        DebugCommands.revealPopover(model: model)

        #expect(host.revealPanelCount == 1)
        #expect(host.closePanelCount == 0)
    }

    /// It reveals only; an open panel is left as it is (revealPanel never closes).
    @Test func revealPopoverOnAShownPanelDoesNotCloseIt() {
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }))
        model.start()
        host.revealPanel()

        DebugCommands.revealPopover(model: model)

        #expect(host.closePanelCount == 0)
        #expect(host.isPanelShown)
    }

    /// It must not change reopen: with an inactive app and an open window, reopen still raises
    /// the window and does not reveal the panel.
    @Test func reopenIsUnchanged() {
        let raiser = FakeWindowRaiser()
        raiser.hasEligibleWindows = true
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }, windowRaiser: raiser))
        model.start()

        model.reopen(wasActive: false)

        #expect(raiser.raiseAllCount == 1)
        #expect(host.revealPanelCount == 0)
    }
}
