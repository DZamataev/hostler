import AppKit
import Foundation

// Ported from the reference app's `Services/WindowRaiser.swift`; the window kinds are hostler's
// (the main window and Settings are the only user windows).

/// What the eligibility rule sees of a window: a snapshot rather than an `NSWindow`, so the
/// rule is a pure function unit-tested without AppKit and the raiser stays thin.
public struct WindowSnapshot: Equatable, Sendable {
    public enum Kind: Equatable, Sendable, CaseIterable {
        case main, settings
        case statusItem, popover, sheet, panelChooser, other
    }

    public let kind: Kind
    public let isVisible: Bool
    public let isMiniaturized: Bool

    public init(kind: Kind, isVisible: Bool, isMiniaturized: Bool) {
        self.kind = kind
        self.isVisible = isVisible
        self.isMiniaturized = isMiniaturized
    }
}

public enum WindowEligibility {
    /// Eligible: the main window and Settings when visible or miniaturized. Everything else
    /// (status item, popover, sheets, open/save panels, unknown) never; a closed (not visible,
    /// not miniaturized) retained window never.
    public static func isEligible(_ window: WindowSnapshot) -> Bool {
        guard window.isVisible || window.isMiniaturized else { return false }
        switch window.kind {
        case .main, .settings:
            return true
        case .statusItem, .popover, .sheet, .panelChooser, .other:
            return false
        }
    }
}

/// The `NSWindow` identifiers the raiser classifies by. SwiftUI's own windows carry their
/// `WindowID` scene id; the Settings scene's window is named by SwiftUI.
public enum WindowIdentifier {
    /// The SwiftUI Settings scene's window: SwiftUI names it (the reference app's probe), not ours
    /// to change.
    public static let settings = "com_apple_SwiftUI_Settings_window"
}

@MainActor
public protocol WindowRaising: AnyObject {
    /// Whether any eligible window exists right now.
    var hasEligibleWindows: Bool { get }
    /// Activates the app, deminiaturizes eligible minimized windows and orders every eligible
    /// window front; makes one key. Creates nothing, closes nothing.
    func raiseAll()
}

/// Raises the app's existing user windows. Deliberately thin: every decision is the pure
/// `WindowEligibility` rule, while the AppKit half — reading `NSApp.windows`, activating,
/// ordering — is covered by `macos/SMOKE_TEST.md`, not a unit test.
@MainActor
public final class AppKitWindowRaiser: WindowRaising {
    public init() {}

    public var hasEligibleWindows: Bool { !eligibleWindows().isEmpty }

    public func raiseAll() {
        let windows = eligibleWindows()
        guard !windows.isEmpty else { return }
        NSApp.activate(ignoringOtherApps: true)
        for window in windows where window.isMiniaturized {
            window.deminiaturize(nil)
        }
        for window in windows {
            window.orderFront(nil)
        }
        keyTarget(among: windows)?.makeKey()
    }

    private func eligibleWindows() -> [NSWindow] {
        NSApp.windows.filter { WindowEligibility.isEligible(snapshot(of: $0)) }
    }

    private func snapshot(of window: NSWindow) -> WindowSnapshot {
        WindowSnapshot(kind: Self.kind(of: window),
                       isVisible: window.isVisible,
                       isMiniaturized: window.isMiniaturized)
    }

    /// The one window that becomes key: the current key window when it is eligible, else the
    /// first eligible window that can take key focus.
    private func keyTarget(among windows: [NSWindow]) -> NSWindow? {
        if let key = NSApp.keyWindow, windows.contains(where: { $0 === key }) { return key }
        return windows.first(where: \.canBecomeKey)
    }

    /// Classifies by identifier. A SwiftUI `Window` scene's window carries exactly its scene id
    /// (the reference app's probe: `id=dictation-history`); matched by prefix so a suffix SwiftUI
    /// might add later cannot unclassify the window.
    private static func kind(of window: NSWindow) -> WindowSnapshot.Kind {
        // A sheet belongs to its owner and must keep its modal relationship.
        if window.sheetParent != nil { return .sheet }
        // A file chooser is its own window (probe: `class=NSSavePanel` for a begun panel).
        if window is NSSavePanel { return .panelChooser }
        if let id = window.identifier?.rawValue {
            if id.hasPrefix(WindowID.main) { return .main }
            if id == WindowIdentifier.settings { return .settings }
        }
        // Class names are AppKit's only signal for its own status-item and popover windows
        // (both probe-verified present in `NSApp.windows` with nil identifiers).
        switch window.className {
        case "NSStatusBarWindow": return .statusItem
        case "_NSPopoverWindow": return .popover
        default: return .other
        }
    }
}
