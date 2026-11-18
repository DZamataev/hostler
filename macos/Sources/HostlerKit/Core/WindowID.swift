import Foundation

/// The `id` every SwiftUI `Window` scene is declared with and every `openWindow(id:)` call
/// opens by (the reference app's `Core/WindowID.swift`). One shared source rather than a bare string
/// at each site — a typo on either side is a silently dead button no test can see, because
/// `openWindow` for an unknown id does nothing observable. In HostlerKit rather than the app
/// target because `AppKitWindowRaiser` classifies windows by it.
public enum WindowID {
    public static let main = "main"
}
