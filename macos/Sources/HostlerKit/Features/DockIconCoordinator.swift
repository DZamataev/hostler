import Foundation

/// Ported from the reference app's `Features/DockIconCoordinator.swift`; only the owners differ.
///
/// Decides whether the Dock icon is visible. A set of owners rather than a flag, because the
/// main window and the Settings window can be open at once and closing one must not take
/// the icon away from the other. The popover is never an owner, and neither is anything shown
/// inside it (unlock, the request cards: R1-9).
@MainActor public final class DockIconCoordinator {
    public enum Owner: Hashable, Sendable, CaseIterable {
        case main
        case settings
    }

    public private(set) var owners: Set<Owner> = []
    private let policy: any ActivationPolicyControlling

    public init(policy: any ActivationPolicyControlling) {
        self.policy = policy
    }

    /// The policy is called ONLY on a real visibility transition, in both directions.
    /// `NSAppActivationPolicy.setDockIconVisible(true)` also calls
    /// `NSApp.activate(ignoringOtherApps:)`, so re-asserting visibility while another owner
    /// is still open would yank focus to this app at the moment the user closed a window —
    /// while they were working in a different app entirely.
    public func open(_ owner: Owner) {
        let wasEmpty = owners.isEmpty
        guard owners.insert(owner).inserted, wasEmpty else { return }
        policy.setDockIconVisible(true)
    }

    public func close(_ owner: Owner) {
        guard owners.remove(owner) != nil, owners.isEmpty else { return }
        policy.setDockIconVisible(false)
    }
}
