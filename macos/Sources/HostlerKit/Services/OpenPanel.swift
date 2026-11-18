import AppKit
import Foundation

/// The key-file chooser (spec §9.6: "optional key file chosen with an open panel"). OS-facing,
/// so the unlock form is tested with a fake.
@MainActor
public protocol OpenPanelPresenting: AnyObject {
    /// One existing file chosen by the person; `nil` when cancelled.
    func chooseFile(message: String) async -> URL?
}

/// `NSOpenPanel`, run modally: one file, no folders. The caller closes the popover first and
/// reveals it again after (the reference app's `transcribeFile`), since a click in the panel would
/// dismiss it anyway.
@MainActor
public final class NSOpenPanelPresenter: OpenPanelPresenting {
    public init() {}

    public func chooseFile(message: String) async -> URL? {
        let panel = NSOpenPanel()
        panel.message = message
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.resolvesAliases = true
        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK else { return nil }
        return panel.url
    }
}
