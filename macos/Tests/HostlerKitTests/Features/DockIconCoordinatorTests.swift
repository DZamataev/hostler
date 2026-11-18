import Foundation
import Testing
@testable import HostlerKit

/// Ported from the reference app's `Tests/Features/DockIconCoordinatorTests.swift`,
/// with hostler's owners: the main window and Settings (R1-9). Unlock and the request cards
/// live inline in the popover and own no Dock icon.
@MainActor
@Suite struct DockIconCoordinatorTests {
    private func make() -> (DockIconCoordinator, FakeActivationPolicy) {
        let policy = FakeActivationPolicy()
        return (DockIconCoordinator(policy: policy), policy)
    }

    @Test func openingTheFirstWindowShowsTheIcon() {
        let (coordinator, policy) = make()
        coordinator.open(.main)
        #expect(policy.calls == [true])
    }

    @Test func closingTheLastWindowHidesIt() {
        let (coordinator, policy) = make()
        coordinator.open(.main)
        coordinator.close(.main)
        #expect(policy.calls == [true, false])
        #expect(coordinator.owners.isEmpty)
    }

    /// The whole reason for a set rather than a flag: closing one of two windows must not
    /// take the icon away from the other.
    @Test func closingOneOfTwoOwnersKeepsTheIcon() {
        let (coordinator, policy) = make()
        coordinator.open(.main)
        coordinator.open(.settings)
        coordinator.close(.main)
        // No policy call at all: the icon was already visible and must stay visible. Calling
        // the policy again would re-run NSApp.activate and steal focus.
        #expect(policy.calls == [true])
        #expect(coordinator.owners == [.settings])
        coordinator.close(.settings)
        #expect(policy.calls == [true, false])
    }

    @Test func settingsKeepsTheIconUntilTheMainWindowAlsoCloses() {
        let (coordinator, policy) = make()
        coordinator.open(.settings)
        coordinator.open(.main)
        coordinator.close(.settings)
        #expect(policy.calls == [true])
        coordinator.close(.main)
        #expect(policy.calls == [true, false])
    }

    @Test func openingTheSameOwnerTwiceCallsThePolicyOnce() {
        let (coordinator, policy) = make()
        coordinator.open(.main)
        coordinator.open(.main)
        #expect(policy.calls == [true])
    }

    @Test func closingAnOwnerThatWasNeverOpenedDoesNothing() {
        let (coordinator, policy) = make()
        coordinator.close(.settings)
        #expect(policy.calls.isEmpty)
    }

    /// R1-9: only real windows own the icon. Unlock is inline in the popover.
    @Test func theOwnersAreTheMainWindowAndSettingsOnly() {
        #expect(Set(DockIconCoordinator.Owner.allCases) == [.main, .settings])
    }
}
