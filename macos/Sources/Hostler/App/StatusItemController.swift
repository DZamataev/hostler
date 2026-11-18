import AppKit
import HostlerKit
import Observation
import SwiftUI

// A line-by-line port of the reference app's `App/StatusItemController.swift` (spec §9.1, P5-L1).
// Kept: `.applicationDefined`, the three dismissal paths, re-anchoring on the next turn and on
// the button window's notifications, closing on another display, the highlight cleared on
// every close path, the height cap, both mouse buttons. Deviations, each marked "hostler:"
// below: no title (the title writer, reserved widths and the recording clock are dropped), the
// image follows `AppModel.statusItemAppearance`, the panel is `PopoverView`, and a debug-only
// switch keeps the panel open when the app resigns active (lead ruling P5-L5).

/// The app's status item and popover as `AppModel` sees them: what the panel's commands
/// close, and what a headless test fakes in place of a real status bar.
@MainActor
protocol StatusItemHosting: AnyObject {
    func closePanel()
    /// Shows the panel if it is not already shown; never closes it.
    func revealPanel()
    var isPanelShown: Bool { get }
}

/// What the popover's show and close mean to the model (Task 13 F-1): "opened" retries a
/// connection that is not live (P5-L12); "closed" clears everything typed in the popover.
@MainActor
protocol PanelLifecycle: AnyObject {
    func popoverOpened()
    func popoverClosed()
}

/// The popover's sessions (Task 13 F-1). Every show is a new session: a new number for the
/// root view's identity — so SwiftUI builds it fresh, with fresh `@State`, as the reference app's
/// rebuild-per-show intends — and the "opened" hook; the close ends it with the "closed"
/// hook, once. Driven by `StatusItemController`'s own show and close paths, not by SwiftUI's
/// appear/disappear, which a long-lived hosting controller does not repeat.
@MainActor
final class PanelSession {
    private let lifecycle: any PanelLifecycle
    private(set) var number = 0
    private(set) var isOpen = false

    init(lifecycle: any PanelLifecycle) {
        self.lifecycle = lifecycle
    }

    /// A show: the new root's identity. A show while shown (a re-anchor) is a new root but
    /// not a second "opened".
    func willShow() -> Int {
        number += 1
        if !isOpen {
            isOpen = true
            lifecycle.popoverOpened()
        }
        return number
    }

    func didClose() {
        guard isOpen else { return }
        isOpen = false
        lifecycle.popoverClosed()
    }
}

/// Owns the app's single status item, its button's image and the popover that hosts
/// `PopoverView`. Created once from `AppModel.start()` through the environment's factory, so
/// the model can close the panel without touching AppKit.
@MainActor
final class StatusItemController: NSObject, StatusItemHosting, NSPopoverDelegate {
    private let model: AppModel
    private let statusItem: NSStatusItem
    private let popover = NSPopover()
    private let hostingController: NSHostingController<AnyView>
    private let now: () -> Date
    /// hostler: `false` only under the debug `HOSTLER_APP_TEST_KEEP_POPOVER=1` (P5-L5), so a
    /// foreground UI driver that hands focus back to another app can still read the panel.
    private let dismissesOnResignActive: Bool
    /// When the popover last closed, to tell a genuine reopen from the mouse-up half of the
    /// click that just closed it (see `StatusItemToggle`).
    private var lastClosedAt: Date?
    /// Watches clicks delivered to other apps while the popover is shown. `.transient` alone
    /// misses some of them — a click into another app's window on another display, made after
    /// scrolling it without focusing it, left the panel open — so such a click closes it here.
    private var outsideClickMonitor: Any?
    /// The same for our own windows and Escape, which `.applicationDefined` leaves to us.
    private var localEventMonitor: Any?
    private var resignActiveObserver: NSObjectProtocol?
    /// Follows the status button's window while the popover is shown (see `reanchor()`).
    private var buttonWindowObservers: [NSObjectProtocol] = []
    /// The display the panel settled on after opening; `nil` until that first pass.
    private var anchoredScreen: NSScreen?
    /// hostler (Task 13 F-1): each show's root identity and the model's open/close hooks.
    private let session: PanelSession

    var isPanelShown: Bool { popover.isShown }

    init(model: AppModel, dismissesOnResignActive: Bool = true,
         now: @escaping () -> Date = { Date() }) {
        self.model = model
        self.dismissesOnResignActive = dismissesOnResignActive
        self.now = now
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        session = PanelSession(lifecycle: model)
        hostingController = NSHostingController(
            rootView: AnyView(Self.panel(for: model, maxHeight: .greatestFiniteMagnitude, session: 0)))
        super.init()

        if let button = statusItem.button {
            button.target = self
            button.action = #selector(togglePanel)
            // Both clicks are the same command; a status button only sends on left mouse-up
            // by default.
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            // hostler: image only, so no `imagePosition` or title font (no title).
        }
        hostingController.sizingOptions = .preferredContentSize
        // Not `.transient`: activating the app to make the panel key also brings an open
        // window of ours (the main window, Settings) forward, and a transient popover took that
        // key change for a dismissal — it closed and reopened. Dismissal is ours instead:
        // a click elsewhere, Escape, the app losing focus (`installOutsideClickMonitor()`).
        popover.behavior = .applicationDefined
        popover.contentViewController = hostingController
        popover.delegate = self

        observe()
    }

    // MARK: - Toggling

    @objc private func togglePanel() {
        switch StatusItemToggle.decide(isShown: isPanelShown, lastClosedAt: lastClosedAt, now: now()) {
        case .close: closePanel()
        case .show: showPanel()
        case .ignore: break
        }
    }

    func closePanel() {
        guard isPanelShown else { return }
        popover.performClose(nil)
        recordClose()
        // Belt and braces with the delegate callbacks below: a close this method asks for
        // must never leave the button pressed (SMOKE_TEST.md's stuck-highlight check).
        statusItem.button?.highlight(false)
    }

    func revealPanel() {
        guard !isPanelShown else { return }
        showPanel()
    }

    /// Called on every close path so the toggle can recognise the mouse-up that follows a
    /// click's own dismissal (`StatusItemToggle`). Not on `willClose`: `performClose` and
    /// every AppKit dismissal reach `didClose`, and the shortest recency window is centred
    /// on the moment the popover is actually gone.
    private func recordClose() {
        lastClosedAt = now()
    }

    private func showPanel() {
        guard let button = statusItem.button else { return }
        // A new session: a root of a new identity (fresh `@State`, appear runs again) and the
        // model's "opened" hook (hostler, Task 13 F-1).
        let shown = session.willShow()
        hostingController.rootView = AnyView(
            Self.panel(for: model, maxHeight: maxPanelHeight(for: button), session: shown))
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        button.highlight(true)
        NSApp.activate(ignoringOtherApps: true)
        // The popover window must be key for ⌘O, ⌘Q, typing and Escape to reach the panel.
        popover.contentViewController?.view.window?.makeKey()
        installOutsideClickMonitor()
        followButtonWindow(of: button)
        // Clicking the menu bar of a display that is not the focused one moves the status
        // item's window there during this very click; AppKit has anchored the popover to the
        // frame it had before. A pass on the next turn finds it where it now is.
        anchoredScreen = nil
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.reanchor()
            self.anchoredScreen = self.statusItem.button?.window?.screen
        }
    }

    /// Showing an already shown popover again repositions it — window and arrow — against
    /// the button's current frame.
    private func reanchor() {
        guard isPanelShown, let button = statusItem.button else { return }
        popover.animates = false
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        popover.animates = true
    }

    /// The button's window also moves while the panel is open: another item in the menu bar
    /// changing width, or a display change. Each move re-anchors.
    private func followButtonWindow(of button: NSStatusBarButton) {
        stopFollowingButtonWindow()
        guard let window = button.window else { return }
        for name in [NSWindow.didMoveNotification, NSWindow.didResizeNotification,
                     NSWindow.didChangeScreenNotification] {
            buttonWindowObservers.append(NotificationCenter.default.addObserver(
                forName: name, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.buttonWindowMoved() }
            })
        }
    }

    /// A move on the same display keeps the panel and re-anchors it; a move to another
    /// display means the menu bar there was clicked, and the panel closes rather than jumping
    /// across — the same as clicking anywhere else outside it.
    private func buttonWindowMoved() {
        guard let anchoredScreen else { return }
        if let screen = statusItem.button?.window?.screen, screen != anchoredScreen {
            closePanel()
        } else {
            reanchor()
        }
    }

    private func stopFollowingButtonWindow() {
        buttonWindowObservers.forEach(NotificationCenter.default.removeObserver)
        buttonWindowObservers = []
    }

    /// Every way the panel is dismissed besides its own commands. A global monitor sees clicks
    /// sent to other apps; a local one sees clicks in our own windows (the status item's
    /// window excluded — its click is the toggle) and Escape. Pop-up menus inside the panel
    /// track in their own loop and never reach the local monitor.
    private func installOutsideClickMonitor() {
        removeOutsideClickMonitor()
        let clicks: NSEvent.EventTypeMask = [.leftMouseDown, .rightMouseDown, .otherMouseDown]
        outsideClickMonitor = NSEvent.addGlobalMonitorForEvents(matching: clicks) { [weak self] _ in
            MainActor.assumeIsolated { self?.closePanel() }
        }
        localEventMonitor = NSEvent.addLocalMonitorForEvents(matching: clicks.union(.keyDown)) {
            [weak self] event in
            // Local monitors run on the main thread; NSEvent is not Sendable, so the hop into
            // the main actor's isolation carries it unchecked.
            nonisolated(unsafe) let event = event
            nonisolated(unsafe) var result: NSEvent? = event
            MainActor.assumeIsolated { result = self?.handleLocal(event) ?? event }
            return result
        }
        // hostler: skipped only under the debug keep-popover switch (P5-L5); release always
        // installs it, exactly as the reference app does.
        guard dismissesOnResignActive else { return }
        resignActiveObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.closePanel() }
        }
    }

    private func handleLocal(_ event: NSEvent) -> NSEvent? {
        guard isPanelShown else { return event }
        let panelWindow = popover.contentViewController?.view.window
        if event.type == .keyDown {
            // Escape, while the panel is the key window.
            guard event.keyCode == 53, event.window === panelWindow else { return event }
            closePanel()
            return nil
        }
        if event.window !== panelWindow, event.window !== statusItem.button?.window {
            closePanel()
        }
        return event
    }

    private func removeOutsideClickMonitor() {
        if let outsideClickMonitor { NSEvent.removeMonitor(outsideClickMonitor) }
        outsideClickMonitor = nil
        if let localEventMonitor { NSEvent.removeMonitor(localEventMonitor) }
        localEventMonitor = nil
        if let resignActiveObserver { NotificationCenter.default.removeObserver(resignActiveObserver) }
        resignActiveObserver = nil
    }

    /// The panel. The root view is rebuilt per show, which resets the panel's `@State`, so
    /// every opening starts fresh (spec §9.1). hostler: `PopoverView` (Task 9) instead of
    /// the reference app's `MenuPanelView` and its selector models.
    private static func panel(for model: AppModel, maxHeight: CGFloat, session: Int) -> some View {
        // The identity changes per show, so SwiftUI discards the previous show's view and its
        // state instead of updating it in place (hostler, Task 13 F-1).
        PopoverView(model: model, maxHeight: maxHeight)
            .id(session)
    }

    /// Caps the panel on the status button's own screen, leaving the popover's arrow and
    /// margins their room (a short display scrolls instead of overflowing).
    private func maxPanelHeight(for button: NSStatusBarButton) -> CGFloat {
        guard let visibleHeight = (button.window?.screen ?? NSScreen.main)?.visibleFrame.height
        else { return .greatestFiniteMagnitude }
        return max(0, visibleHeight - 40)
    }

    // MARK: - NSPopoverDelegate

    // Cleared on both: every dismissal AppKit performs itself — Escape, outside click,
    // transient close — posts both callbacks, and neither alone should be trusted to be the
    // one that observes the close.
    func popoverWillClose(_ notification: Notification) {
        statusItem.button?.highlight(false)
        // Stop here, not on didClose: `isShown` stays true through the close animation, and a
        // window opened by a panel command can move the status item in that time — a re-anchor
        // then reopened the panel over the new window.
        stopFollowingButtonWindow()
        removeOutsideClickMonitor()
        anchoredScreen = nil
    }

    func popoverDidClose(_ notification: Notification) {
        removeOutsideClickMonitor()
        stopFollowingButtonWindow()
        anchoredScreen = nil
        recordClose()
        statusItem.button?.highlight(false)
        // hostler (Task 13 F-1): every close path reaches here; nothing typed outlives it.
        session.didClose()
    }

    // MARK: - The image (hostler: replaces the reference app's title machinery)

    /// Follows the model's appearance through Observation: each change re-renders the image
    /// and re-arms the tracking. Only the image changes, so the item's width never does.
    private func observe() {
        let appearance = withObservationTracking {
            model.statusItemAppearance
        } onChange: { [weak self] in
            Task { @MainActor in self?.observe() }
        }
        write(appearance)
    }

    private func write(_ appearance: StatusItemAppearance) {
        guard let button = statusItem.button else { return }
        button.image = StatusItemImages.image(for: appearance)
        let label = StatusItemImages.accessibilityLabel(for: appearance)
        button.toolTip = label
        button.setAccessibilityLabel(label)
    }
}

/// The status item's images: template images of one size, so macOS picks their contrast
/// against the menu bar (the reference app's invariant 12) and a state change never resizes the item.
@MainActor
enum StatusItemImages {
    static let size = NSSize(width: 18, height: 18)

    static func bundledGlyphURL(in bundle: Bundle = .main) -> URL? {
        bundle.url(forResource: "hostler-template", withExtension: "pdf",
                   subdirectory: "StatusItem")
    }

    private static let glyph: NSImage = {
        let custom = bundledGlyphURL().flatMap { NSImage(contentsOf: $0) }
        let image = custom?.isValid == true ? custom!
            : NSImage(systemSymbolName: "square.stack.3d.up", accessibilityDescription: nil)
                ?? NSImage(size: size)
        image.size = size
        image.isTemplate = true
        return image
    }()

    private static let images: [StatusItemAppearance: NSImage] = [
        .idle: glyph,
        .attention: draw { rect in
            // The glyph with a dot in its top-right corner, a clear ring around the dot so it
            // reads at menu-bar size.
            glyph.draw(in: rect)
            let dot = NSRect(x: rect.maxX - 7, y: rect.maxY - 7, width: 7, height: 7)
            NSGraphicsContext.current?.compositingOperation = .clear
            NSBezierPath(ovalIn: dot.insetBy(dx: -1.5, dy: -1.5)).fill()
            NSGraphicsContext.current?.compositingOperation = .sourceOver
            NSColor.black.setFill()
            NSBezierPath(ovalIn: dot).fill()
        },
        .offline: draw { rect in
            // Dimmed: template images keep their alpha.
            glyph.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 0.4)
        },
    ]

    static func image(for appearance: StatusItemAppearance) -> NSImage {
        images[appearance] ?? glyph
    }

    static func accessibilityLabel(for appearance: StatusItemAppearance) -> String {
        switch appearance {
        case .idle: String(localized: "hostler")
        case .attention: String(localized: "hostler: needs you")
        case .offline: String(localized: "hostler: service not reachable")
        }
    }

    private static func draw(_ body: @escaping (NSRect) -> Void) -> NSImage {
        let image = NSImage(size: size, flipped: false) { rect in
            body(rect)
            return true
        }
        image.isTemplate = true
        return image
    }
}
