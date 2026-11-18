import HostlerKit
import SwiftUI

/// Debug builds only (lead ruling P5-L6): a "Debug › Reveal Popover" command in the app's main
/// menu, which calls the status item's own `revealPanel()`. A UI driver reaches it through the
/// main window's accessibility menu bar, so it can open the popover in a running app whose
/// window is open — the case where reopen (the reference app's, unchanged) raises the window instead.
/// A Release build has an empty `Commands` and none of these strings; `scripts/macos.sh app`
/// checks the Release binary for `marker`.
struct DebugCommands: Commands {
    let model: AppModel

    #if DEBUG
    static let isAvailable = true
    static let menuTitle = "Debug"
    static let revealTitle = "Reveal Popover"
    /// The command's accessibility identifier, and the marker `scripts/macos.sh app` looks for
    /// in the Release binary: a string nothing but this file contains (a title such as
    /// "Reveal Popover" could one day also come from a string catalog).
    static let marker = "io.github.dzamataev.hostler.debug.reveal-popover"

    @MainActor static func revealPopover(model: AppModel) {
        model.statusItem?.revealPanel()
    }

    var body: some Commands {
        CommandMenu(Self.menuTitle) {
            Button(Self.revealTitle) { Self.revealPopover(model: model) }
                .accessibilityIdentifier(Self.marker)
        }
    }
    #else
    static let isAvailable = false

    var body: some Commands {
        EmptyCommands()
    }
    #endif
}
