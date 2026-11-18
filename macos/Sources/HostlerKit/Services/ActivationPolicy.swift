import AppKit

/// Ported verbatim from the reference app's `Services/ActivationPolicyService.swift`.
///
/// Shows or hides the Dock icon of an `LSUIElement` app.
@MainActor public protocol ActivationPolicyControlling: AnyObject {
    func setDockIconVisible(_ visible: Bool)
}

/// AppKit glue with no logic of its own, so it carries no unit test and is covered by
/// `macos/SMOKE_TEST.md` instead.
@MainActor public final class NSAppActivationPolicy: ActivationPolicyControlling {
    public init() {}

    public func setDockIconVisible(_ visible: Bool) {
        NSApp.setActivationPolicy(visible ? .regular : .accessory)
        // Becoming `.regular` does not by itself put the app in front or install its menu bar.
        if visible { NSApp.activate(ignoringOtherApps: true) }
    }
}
