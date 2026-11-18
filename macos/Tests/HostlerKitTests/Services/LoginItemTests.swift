import Foundation
import Testing
@testable import HostlerKit

/// Open at login (plan Task 9): the toggle follows the system; `notFound` (not in
/// /Applications) disables it with the install hint; a failure shows the status text. Nothing
/// here registers a real login item.
@Suite @MainActor struct LoginItemTests {
    final class FakeLoginItem: LoginItemControlling, @unchecked Sendable {
        private let lock = NSLock()
        private var current: LoginItemStatus
        var failure: (any Error)?
        private(set) var calls: [String] = []

        init(_ status: LoginItemStatus) { current = status }

        func status() -> LoginItemStatus { lock.withLock { current } }
        func register() throws {
            try lock.withLock {
                calls.append("register")
                if let failure { throw failure }
                current = .enabled
            }
        }
        func unregister() throws {
            try lock.withLock {
                calls.append("unregister")
                if let failure { throw failure }
                current = .notRegistered
            }
        }
    }

    struct Refused: LocalizedError {
        var errorDescription: String? { "Operation not permitted" }
    }

    @Test func theToggleFollowsTheSystem() {
        let item = FakeLoginItem(.enabled)
        let setting = LoginItemSetting(item: item, appURL: Self.installed)
        setting.refresh()
        #expect(setting.isOn)
        #expect(setting.isAvailable)
        #expect(setting.hint == nil)
        setting.set(false)
        #expect(item.calls == ["unregister"])
        #expect(!setting.isOn)
        setting.set(true)
        #expect(setting.isOn)
    }

    static let installed = URL(fileURLWithPath: "/Applications/Hostler.app")
    static let elsewhere = URL(fileURLWithPath: "/Users/someone/dev/hostler/tmp/app/Hostler.app")

    /// Task 13 F-9: macOS reports `notFound` for an app in /Applications until its first
    /// registration, so the location decides, not that status.
    @Test func fromApplicationsTheToggleIsAvailableEvenWhileNotFound() {
        let item = FakeLoginItem(.notFound)
        let setting = LoginItemSetting(item: item, appURL: Self.installed)
        setting.refresh()
        #expect(setting.isAvailable)
        #expect(!setting.isOn)
        #expect(setting.hint == nil)
        setting.set(true)
        #expect(item.calls == ["register"])
        #expect(setting.isOn)
    }

    @Test func elsewhereTheToggleIsDisabledWithTheInstallHint() {
        let setting = LoginItemSetting(item: FakeLoginItem(.notFound), appURL: Self.elsewhere)
        setting.refresh()
        #expect(!setting.isAvailable)
        #expect(!setting.isOn)
        #expect(setting.hint == "Install the app in /Applications first (scripts/macos.sh install).")
        // Not even a stale `notRegistered` makes it available there.
        let other = LoginItemSetting(item: FakeLoginItem(.notRegistered), appURL: Self.elsewhere)
        other.refresh()
        #expect(!other.isAvailable)
    }

    @Test func elsewhereAnEnabledItemCanStillBeTurnedOff() {
        let item = FakeLoginItem(.enabled)
        let setting = LoginItemSetting(item: item, appURL: Self.elsewhere)
        setting.refresh()
        #expect(setting.isAvailable)
        #expect(setting.isOn)
        setting.set(false)
        #expect(item.calls == ["unregister"])
    }

    @Test func theLocationRule() {
        #expect(LoginItemSetting.isInApplications(Self.installed))
        #expect(LoginItemSetting.isInApplications(URL(fileURLWithPath: "/Applications/Utilities/../Hostler.app")))
        #expect(!LoginItemSetting.isInApplications(Self.elsewhere))
        #expect(!LoginItemSetting.isInApplications(URL(fileURLWithPath: "/Applications-old/Hostler.app")))
        #expect(!LoginItemSetting.isInApplications(URL(fileURLWithPath: "/tmp/Applications/Hostler.app")))
    }

    @Test func approvalPendingSaysWhereToAllowIt() {
        let setting = LoginItemSetting(item: FakeLoginItem(.requiresApproval), appURL: Self.installed)
        setting.refresh()
        #expect(setting.isOn)
        #expect(setting.hint == LoginItemSetting.approvalHint)
    }

    @Test func aFailureShowsItsTextAndTheStatus() {
        let item = FakeLoginItem(.notRegistered)
        item.failure = Refused()
        let setting = LoginItemSetting(item: item, appURL: Self.installed)
        setting.refresh()
        setting.set(true)
        #expect(setting.error == "Operation not permitted (status: not registered)")
        #expect(!setting.isOn)
    }
}
