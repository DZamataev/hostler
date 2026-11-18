import AppKit
import Testing
@testable import Hostler

/// Task 13 F-6: the window stops excluding itself from capture as soon as nothing is revealed
/// (the 30 s auto-hide, Hide, a lock), not only when AppKit happens to remove the view.
@Suite @MainActor struct WindowSharingGuardTests {
    static func window() -> NSWindow {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
                              styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        return window
    }

    @Test func activeExcludesTheWindowAndInactiveRestoresIt() {
        let window = Self.window()
        #expect(window.sharingType == .readOnly)
        let view = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(view)
        #expect(window.sharingType == .readOnly)
        view.setActive(true)
        #expect(window.sharingType == .none)
        // The auto-hide: the value goes, the view stays.
        view.setActive(false)
        #expect(window.sharingType == .readOnly)
        view.setActive(true)
        #expect(window.sharingType == .none)
        // SwiftUI dismantling the view restores it too.
        WindowSharingGuard.dismantleNSView(view, coordinator: ())
        #expect(window.sharingType == .readOnly)
    }

    @Test func activeBeforeItIsInAWindowAppliesOnArrivalAndRestoresOnLeaving() {
        let window = Self.window()
        let view = WindowSharingGuard.GuardView()
        view.setActive(true)
        window.contentView?.addSubview(view)
        #expect(window.sharingType == .none)
        view.removeFromSuperview()
        #expect(window.sharingType == .readOnly)
    }

    @Test func itRestoresTheValueTheWindowHadBefore() {
        let window = Self.window()
        window.sharingType = .readWrite
        let view = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(view)
        view.setActive(true)
        view.setActive(true)
        #expect(window.sharingType == .none)
        view.setActive(false)
        #expect(window.sharingType == .readWrite)
    }

    // MARK: Task 13 re-run G-1: counted per window, the original setting remembered once

    @Test func twoConsecutiveRevealsBothRestore() {
        let window = Self.window()
        let first = WindowSharingGuard.GuardView()
        let second = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(first)
        window.contentView?.addSubview(second)
        first.setActive(true)
        first.setActive(false)       // the first value hides
        #expect(window.sharingType == .readOnly)
        second.setActive(true)       // a second reveal in the same run
        #expect(window.sharingType == .none)
        second.setActive(false)
        #expect(window.sharingType == .readOnly)
        // And the first row once more.
        first.setActive(true)
        first.setActive(false)
        #expect(window.sharingType == .readOnly)
    }

    @Test func overlappingRevealsRestoreOnlyWhenTheLastOneHides() {
        let window = Self.window()
        let a = WindowSharingGuard.GuardView()
        let b = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(a)
        window.contentView?.addSubview(b)
        // B appears before A has gone (the order SwiftUI may update rows in).
        a.setActive(true)
        b.setActive(true)
        a.setActive(false)
        #expect(window.sharingType == .none)
        b.setActive(false)
        #expect(window.sharingType == .readOnly)
        // The other order.
        b.setActive(true)
        a.setActive(true)
        b.setActive(false)
        #expect(window.sharingType == .none)
        a.setActive(false)
        #expect(window.sharingType == .readOnly)
        #expect(WindowSharingGuard.Registry.count(for: window) == 0)
    }

    @Test func aRevealThenALockRestores() {
        let window = Self.window()
        let view = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(view)
        view.setActive(true)
        // A lock hides the value: the view updates with `active` false.
        view.setActive(false)
        #expect(window.sharingType == .readOnly)
        view.setActive(true)
        #expect(window.sharingType == .none)
        WindowSharingGuard.dismantleNSView(view, coordinator: ())
        #expect(window.sharingType == .readOnly)
    }

    @Test func closingAndReopeningTheWindowWhileShownRestoresAndReapplies() {
        let window = Self.window()
        let view = WindowSharingGuard.GuardView()
        view.setActive(true)
        window.contentView?.addSubview(view)
        #expect(window.sharingType == .none)
        view.removeFromSuperview()           // the window closes with the value shown
        #expect(window.sharingType == .readOnly)
        window.contentView?.addSubview(view)  // reopened: still shown
        #expect(window.sharingType == .none)
        view.setActive(false)
        #expect(window.sharingType == .readOnly)
    }

    @Test func theOriginalIsRememberedOnceNotTheExcludedOne() {
        let window = Self.window()
        window.sharingType = .readWrite
        let a = WindowSharingGuard.GuardView()
        let b = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(a)
        window.contentView?.addSubview(b)
        a.setActive(true)
        b.setActive(true)
        a.setActive(false)
        b.setActive(false)
        #expect(window.sharingType == .readWrite)
        b.setActive(true)
        b.setActive(false)
        #expect(window.sharingType == .readWrite)
    }

    @Test func twoWindowsAreCountedApart() {
        let one = Self.window()
        let two = Self.window()
        let a = WindowSharingGuard.GuardView()
        let b = WindowSharingGuard.GuardView()
        one.contentView?.addSubview(a)
        two.contentView?.addSubview(b)
        a.setActive(true)
        b.setActive(true)
        a.setActive(false)
        #expect(one.sharingType == .readOnly)
        #expect(two.sharingType == .none)
        b.setActive(false)
        #expect(two.sharingType == .readOnly)
    }

    /// What macOS 26 does with an on-screen window (Task 13 re-run G-1, checked with a probe
    /// window): once `sharingType` was set to `.none`, the getter keeps answering `.none`,
    /// while WindowServer does honour a later `.readOnly`. This window answers the same way and
    /// records what was set.
    final class OneWayGetterWindow: NSWindow {
        private var everExcluded = false
        private(set) var lastSet: NSWindow.SharingType?

        override var sharingType: NSWindow.SharingType {
            get { everExcluded ? .none : super.sharingType }
            set {
                lastSet = newValue
                if newValue == .none { everExcluded = true }
                super.sharingType = newValue
            }
        }
    }

    static func oneWayWindow() -> OneWayGetterWindow {
        let window = OneWayGetterWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
                                        styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        return window
    }

    @Test func theSecondRevealRestoresEvenThoughTheGetterStaysExcluded() {
        let window = Self.oneWayWindow()
        let first = WindowSharingGuard.GuardView()
        let second = WindowSharingGuard.GuardView()
        window.contentView?.addSubview(first)
        window.contentView?.addSubview(second)
        first.setActive(true)
        first.setActive(false)
        #expect(window.lastSet == .readOnly)
        second.setActive(true)
        #expect(window.lastSet == NSWindow.SharingType.none)
        second.setActive(false)
        // The original remembered at the first reveal, not the getter's `.none`.
        #expect(window.lastSet == .readOnly)
        // Overlapping, and a third cycle.
        first.setActive(true)
        second.setActive(true)
        first.setActive(false)
        #expect(window.lastSet == NSWindow.SharingType.none)
        second.setActive(false)
        #expect(window.lastSet == .readOnly)
    }
}
