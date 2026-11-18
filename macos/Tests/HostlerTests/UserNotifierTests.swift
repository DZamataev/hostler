import Foundation
import HostlerKit
import Testing
import UserNotifications
@testable import Hostler

/// Notifications (spec §9.4; P5-L5): authorization is asked on the first enabled kind; a
/// thrown `requestAuthorization`, a prompt left unanswered and a denial are normal states;
/// nothing is posted (or queued) unless allowed and the kind is on; a click opens its target.
@Suite(.timeLimit(.minutes(1))) @MainActor struct UserNotifierTests {
    static let note = AppNotification(kind: .request, id: "s-1", title: "API_KEY requested",
                                      body: "app", target: .popover)

    func eventually(_ what: String, _ condition: @MainActor () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(10)
        while !condition() {
            if Date() > deadline { Issue.record("timed out: \(what)"); return }
            try await Task.sleep(for: .milliseconds(2))
        }
    }

    @Test func itAsksAtStartWhenAKindIsOnAndPostsOnceAllowed() async throws {
        let center = FakeNotificationCenter(authorized: nil)
        let notifier = UserNotifier(center: center, store: MemoryNotificationSettings())
        notifier.post(Self.note)
        #expect(center.added.isEmpty)
        notifier.start()
        #expect(center.activated == 1)
        try await eventually("allowed") { notifier.permission == .allowed }
        #expect(center.asked == 1)
        notifier.post(Self.note)
        try await eventually("posted") { center.added == [Self.note] }
    }

    @Test func withEveryKindOffItDoesNotAskUntilOneIsTurnedOn() async throws {
        var off = NotificationSettings()
        off.newDevice = false
        off.request = false
        off.stuckTask = false
        off.seizure = false
        let center = FakeNotificationCenter(authorized: nil)
        let store = MemoryNotificationSettings(off)
        let notifier = UserNotifier(center: center, store: store)
        notifier.start()
        try await eventually("read") { notifier.permission == .notAsked }
        #expect(center.asked == 0)
        notifier.settings.request = true
        #expect(store.stored.request)
        try await eventually("allowed") { notifier.permission == .allowed }
        #expect(center.asked == 1)
    }

    @Test func aThrownAuthorizationIsShownAsRefusedAndNothingPosts() async throws {
        let center = FakeNotificationCenter(authorized: nil)
        center.answer = .failure(NSError(domain: "UNErrorDomain", code: 1))
        let notifier = UserNotifier(center: center, store: MemoryNotificationSettings())
        notifier.start()
        try await eventually("refused") { notifier.permission == .refused(domain: "UNErrorDomain", code: 1) }
        #expect(notifier.permission.settingsText == "macOS refused notifications for this build (UNErrorDomain 1).")
        notifier.post(Self.note)
        try await Task.sleep(for: .milliseconds(20))
        #expect(center.added.isEmpty)
        // A later read does not turn the refusal into "not asked" and ask again.
        notifier.refresh(askIfNeeded: true)
        try await Task.sleep(for: .milliseconds(20))
        #expect(center.asked == 1)
    }

    @Test func aPromptLeftUnansweredIsWaitingAndNothingIsQueuedOrRetried() async throws {
        let center = FakeNotificationCenter(authorized: nil)
        center.answer = nil
        let notifier = UserNotifier(center: center, store: MemoryNotificationSettings())
        notifier.start()
        try await eventually("waiting") { notifier.permission == .waiting }
        #expect(notifier.permission.settingsText == "Waiting for your answer in the macOS prompt.")
        notifier.post(Self.note)
        notifier.refresh(askIfNeeded: true)
        notifier.settings.seizure = false
        notifier.settings.seizure = true
        try await Task.sleep(for: .milliseconds(20))
        #expect(center.asked == 1)
        #expect(center.added.isEmpty)
    }

    @Test func deniedSaysWhereToAllowIt() async throws {
        let center = FakeNotificationCenter(authorized: false)
        let notifier = UserNotifier(center: center, store: MemoryNotificationSettings())
        notifier.start()
        try await eventually("denied") { notifier.permission == .denied }
        #expect(center.asked == 0)
        #expect(notifier.permission.settingsText?.contains("System Settings") == true)
    }

    @Test func aKindTurnedOffIsNotPosted() async throws {
        let center = FakeNotificationCenter(authorized: true)
        let notifier = UserNotifier(center: center, store: MemoryNotificationSettings())
        notifier.start()
        try await eventually("allowed") { notifier.permission == .allowed }
        notifier.settings.request = false
        notifier.post(Self.note)
        try await Task.sleep(for: .milliseconds(20))
        #expect(center.added.isEmpty)
    }

    @Test func aClickOpensItsTarget() {
        let center = FakeNotificationCenter()
        let notifier = UserNotifier(center: center, store: MemoryNotificationSettings())
        var opened: [NotificationTarget] = []
        notifier.onOpen = { opened.append($0) }
        center.onTap?(.queue)
        #expect(opened == [.queue])
    }

    @Test func theTargetSurvivesTheUserInfo() {
        for target in [NotificationTarget.popover, .queue, .device(ResourceId(rawValue: "android:R5"))] {
            #expect(NotificationTargetCoding.decode(NotificationTargetCoding.encode(target)) == target)
        }
        #expect(NotificationTargetCoding.decode("nonsense") == nil)
    }

    @Test func settingsPersist() {
        let store = MemoryNotificationSettings()
        let notifier = UserNotifier(center: FakeNotificationCenter(), store: store)
        notifier.settings.stuckTask = false
        let again = UserNotifier(center: FakeNotificationCenter(), store: store)
        #expect(again.settings.stuckTask == false)
    }

    /// R1-1: what Notification Center receives is the notification's title and body, the
    /// target, and a digest of the id — never the id itself (a seizure's id holds paths).
    @Test func theSubmittedRequestCarriesOnlyTheTextsTheTargetAndADigest() {
        let marker = "MARKER-7f3a"
        let note = AppNotification(
            kind: .seizure, id: "seizure:claims|screen|/work/\(marker)|/work/other|1",
            title: "A claim was taken over",
            body: "Another workspace took it over. Open hostler to see which.", target: .popover)
        let request = SystemNotificationCenter.request(for: note)
        #expect(request.content.title == note.title)
        #expect(request.content.body == note.body)
        #expect(request.content.subtitle.isEmpty)
        #expect(request.content.userInfo.count == 1)
        #expect(request.content.userInfo[NotificationTargetCoding.key] as? String == "popover")
        #expect(!request.identifier.contains(marker))
        #expect(request.identifier == "seizure:" + SystemNotificationCenter.digest(note.id))
        #expect(SystemNotificationCenter.digest(note.id).count == 16)
        #expect(SystemNotificationCenter.request(for: note).identifier == request.identifier)
        let all = [request.content.title, request.content.body, request.content.subtitle,
                   request.identifier, "\(request.content.userInfo)"]
        #expect(all.allSatisfy { !$0.contains(marker) })
    }
}
