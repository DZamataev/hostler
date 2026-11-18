import Foundation
@testable import Hostler

/// The status item and popover in a headless test (the reference app's
/// `Tests/Fakes/FakeStatusItemHost.swift`): records what the model asks for.
@MainActor
final class FakeStatusItemHost: StatusItemHosting {
    private(set) var closePanelCount = 0
    private(set) var revealPanelCount = 0
    var isPanelShown = false

    func revealPanel() {
        revealPanelCount += 1
        isPanelShown = true
    }

    func closePanel() {
        closePanelCount += 1
        isPanelShown = false
    }
}
