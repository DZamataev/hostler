import AppKit
import HostlerKit
import Testing
@testable import Hostler

/// the reference app's `Tests/App/AppDelegateTests.swift`, with hostler's model and
/// the launch rules of lead ruling P5-L5.
@Suite @MainActor struct AppDelegateTests {
    private func delegate(model: AppModel,
                          launch: LaunchOptions = .plain,
                          replyToTermination: @escaping @MainActor (Bool) -> Void = { _ in },
                          isAppActive: @escaping @MainActor () -> Bool = { false },
                          now: @escaping () -> Date = { Date() },
                          schedule: @escaping AppDelegate.Scheduler = { _, _ in })
        -> AppDelegate {
        AppDelegate(model: model, launch: launch, replyToTermination: replyToTermination,
                    isAppActive: isAppActive, now: now, schedule: schedule)
    }

    // MARK: - Quitting and the last window

    @Test func terminationRepliesOnlyAfterTheModelHasShutDown() async {
        let model = AppModel(env: .fake())
        let replied = Box<Bool?>(nil)
        let shutDownAtReply = Box(false)
        let delegate = delegate(model: model, replyToTermination: {
            shutDownAtReply.value = model.hasShutDown
            replied.value = $0
        })

        let reply = delegate.applicationShouldTerminate(NSApplication.shared)

        #expect(reply == .terminateLater)
        await waitFor("termination reply") { replied.value != nil }
        #expect(replied.value == true)
        #expect(shutDownAtReply.value)
    }

    /// The app's real surface is the status item, not a window: with the hidden
    /// `MenuBarExtra` anchor showing nothing at launch, closing the last window must leave the
    /// app running (spec §9.1).
    @Test func closingTheLastWindowDoesNotTerminateTheApp() {
        let delegate = delegate(model: AppModel(env: .fake()))
        #expect(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApplication.shared)
                == false)
    }

    // MARK: - Launch (lead ruling P5-L5)

    @Test func aPlainLaunchBuildsTheStatusItem() {
        let host = FakeStatusItemHost()
        let built = Box(0)
        let model = AppModel(env: .fake(makeStatusItem: { _ in built.value += 1; return host }))
        let delegate = delegate(model: model)

        delegate.applicationDidFinishLaunching(Notification(name: NSApplication.didFinishLaunchingNotification))

        #expect(built.value == 1)
        #expect(model.statusItem === host)
        #expect(host.revealPanelCount == 0)
    }

    /// Hosted tests launch the real app; it must not touch the status bar or the system.
    @Test func aLaunchHostingTestsStartsNothing() {
        let built = Box(0)
        let scheduled = Box(0)
        let model = AppModel(env: .fake(makeStatusItem: { _ in
            built.value += 1
            return FakeStatusItemHost()
        }))
        let launch = LaunchOptions(isHostingTests: true, openPopoverAtLaunch: true,
                                   keepPopoverOnResignActive: false)
        let delegate = delegate(model: model, launch: launch,
                                schedule: { _, _ in scheduled.value += 1 })

        delegate.applicationDidFinishLaunching(Notification(name: NSApplication.didFinishLaunchingNotification))

        #expect(built.value == 0)
        #expect(model.statusItem == nil)
        #expect(scheduled.value == 0)
    }

    @Test func thePopoverHookRevealsThePanelOneSecondAfterLaunch() {
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }))
        let delays = Box<[Duration]>([])
        let pending = Box<[@MainActor () -> Void]>([])
        let launch = LaunchOptions(isHostingTests: false, openPopoverAtLaunch: true,
                                   keepPopoverOnResignActive: false)
        let delegate = delegate(model: model, launch: launch, schedule: { delay, work in
            delays.value.append(delay)
            pending.value.append(work)
        })

        delegate.applicationDidFinishLaunching(Notification(name: NSApplication.didFinishLaunchingNotification))

        // Not synchronously: the status button's window is not laid out yet (Task 0 §2.7).
        #expect(host.revealPanelCount == 0)
        #expect(delays.value == [.seconds(1)])
        pending.value.forEach { $0() }
        #expect(host.revealPanelCount == 1)
    }

    @Test func withoutTheHookNothingIsScheduled() {
        let scheduled = Box(0)
        let model = AppModel(env: .fake(makeStatusItem: { _ in FakeStatusItemHost() }))
        let delegate = delegate(model: model, schedule: { _, _ in scheduled.value += 1 })

        delegate.applicationDidFinishLaunching(Notification(name: NSApplication.didFinishLaunchingNotification))

        #expect(scheduled.value == 0)
    }

    // MARK: - Opening the running app again (Finder, Spotlight, Launchpad)

    private func reopenFixture(windows: Bool, isActive: Bool, activatedAgo: TimeInterval?)
        -> (raiser: FakeWindowRaiser, host: FakeStatusItemHost, handled: Bool) {
        let raiser = FakeWindowRaiser()
        raiser.hasEligibleWindows = windows
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }, windowRaiser: raiser))
        model.start()
        let start = Date(timeIntervalSinceReferenceDate: 1000)
        let clock = Box(start)
        let delegate = delegate(model: model, isAppActive: { isActive },
                                now: { clock.value })
        if let activatedAgo {
            delegate.applicationDidBecomeActive(Notification(name: NSApplication.didBecomeActiveNotification))
            clock.value = start.addingTimeInterval(activatedAgo)
        }
        let handled = delegate.applicationShouldHandleReopen(NSApplication.shared,
                                                             hasVisibleWindows: windows)
        return (raiser, host, handled)
    }

    @Test func reopeningAnInactiveAppWithWindowsRaisesThemFirst() {
        let r = reopenFixture(windows: true, isActive: false, activatedAgo: nil)
        #expect(r.raiser.raiseAllCount == 1)
        #expect(r.host.revealPanelCount == 0)
        #expect(r.handled == false)
    }

    /// The reopening click itself activates the app just before the reopen arrives; that
    /// activation must not count as the windows already being in front.
    @Test func theActivationOfTheReopeningClickStillRaisesTheWindows() {
        let r = reopenFixture(windows: true, isActive: true, activatedAgo: 0.1)
        #expect(r.raiser.raiseAllCount == 1)
        #expect(r.host.revealPanelCount == 0)
    }

    @Test func reopeningAnAlreadyActiveAppOpensThePanel() {
        let r = reopenFixture(windows: true, isActive: true, activatedAgo: 30)
        #expect(r.raiser.raiseAllCount == 0)
        #expect(r.host.revealPanelCount == 1)
    }

    /// The 0.5 s activation window's edge: an activation exactly 0.5 s old is no longer the
    /// reopening click's.
    @Test func anActivationHalfASecondOldCountsAsAlreadyActive() {
        let r = reopenFixture(windows: true, isActive: true, activatedAgo: 0.5)
        #expect(r.raiser.raiseAllCount == 0)
        #expect(r.host.revealPanelCount == 1)
    }

    @Test func reopeningWithNoWindowsOpensThePanel() {
        let r = reopenFixture(windows: false, isActive: false, activatedAgo: nil)
        #expect(r.raiser.raiseAllCount == 0)
        #expect(r.host.revealPanelCount == 1)
        #expect(r.handled == false)
    }
}

extension LaunchOptions {
    static let plain = LaunchOptions(isHostingTests: false, openPopoverAtLaunch: false,
                                     keepPopoverOnResignActive: false)
}

/// Polls `condition` on the main actor until it holds, for up to five seconds.
@MainActor func waitFor(_ what: String, _ condition: () -> Bool) async {
    let deadline = Date().addingTimeInterval(5)
    while !condition(), Date() < deadline {
        try? await Task.sleep(for: .milliseconds(10))
    }
    #expect(condition(), "timed out waiting for \(what)")
}
