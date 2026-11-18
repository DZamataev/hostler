import Foundation
import Testing
@testable import HostlerKit

@Suite struct StuckRuleTests {
    let minute: Int64 = 60_000

    @Test func withFewerThanThreeFinishedTasksTheBoundIsAnHour() {
        let history = [Mk.finished("a", ranMs: 1 * minute, finishedAt: Mk.now - 1),
                       Mk.finished("b", ranMs: 1 * minute, finishedAt: Mk.now - 2)]
        #expect(StuckRule.boundMs(kind: "build", history: history) == 60 * minute)
        #expect(StuckRule.boundMs(kind: "build", history: []) == 60 * minute)
    }

    @Test func theBoundIsTwiceTheLongestOfTheLastTenAndAtLeastThirtyMinutes() {
        // Short tasks: the 30-minute floor.
        let short = (0..<5).map { Mk.finished("s\($0)", ranMs: 2 * minute, finishedAt: Mk.now - Int64($0)) }
        #expect(StuckRule.boundMs(kind: "build", history: short) == 30 * minute)

        // 20 minutes longest among the last ten → 40 minutes.
        var history = (0..<10).map { Mk.finished("t\($0)", ranMs: Int64($0 + 1) * 2 * minute, finishedAt: Mk.now - Int64($0)) }
        #expect(StuckRule.boundMs(kind: "build", history: history) == 40 * minute)

        // An older, longer task (the 11th) does not count; another kind does not either.
        history.append(Mk.finished("old", ranMs: 300 * minute, finishedAt: Mk.now - 1000))
        history.append(Mk.finished("other", kind: "test", ranMs: 300 * minute, finishedAt: Mk.now))
        #expect(StuckRule.boundMs(kind: "build", history: history) == 40 * minute)
    }

    @Test func tasksThatNeverRanDoNotCount() {
        let history = [Mk.finished("a", ranMs: 50 * minute, finishedAt: Mk.now - 1),
                       Mk.finished("w1", ranMs: 0, finishedAt: Mk.now - 2, granted: false),
                       Mk.finished("w2", ranMs: 0, finishedAt: Mk.now - 3, granted: false),
                       Mk.finished("b", ranMs: 1 * minute, finishedAt: Mk.now - 4)]
        #expect(StuckRule.boundMs(kind: "build", history: history) == 60 * minute)
    }

    @Test func aRunningTaskIsStuckOnlyPastTheBound() {
        let history = (0..<3).map { Mk.finished("t\($0)", ranMs: 25 * minute, finishedAt: Mk.now - Int64($0)) }
        let task = Mk.entry("h1", grantedAtMs: Mk.now)
        #expect(!StuckRule.isStuck(task, history: history, nowMs: Mk.now + 50 * minute))
        #expect(StuckRule.isStuck(task, history: history, nowMs: Mk.now + 50 * minute + 1))
        let stopping = Mk.entry("h2", state: .stopping, grantedAtMs: Mk.now)
        #expect(StuckRule.isStuck(stopping, history: history, nowMs: Mk.now + 51 * minute))
        let waiting = Mk.entry("h3", state: .waiting, grantedAtMs: nil)
        #expect(!StuckRule.isStuck(waiting, history: history, nowMs: Mk.now + 500 * minute))
        let queue = Mk.queue(seq: 1, running: [task, Mk.entry("h4", grantedAtMs: Mk.now + 40 * minute)])
        #expect(StuckRule.stuck(in: queue, history: history, nowMs: Mk.now + 51 * minute).map(\.id) == ["h1"])
    }
}

@Suite struct NotificationRulesTests {
    /// Seizures before this never notify (the store's first resync read).
    let since = Mk.now - 1000

    private func facts(_ model: StoreModel, at nowMs: Int64 = Mk.now) -> NotificationFacts {
        NotificationFacts(model: model, nowMs: nowMs, seizuresSinceMs: since)
    }

    /// A model that has read every part once.
    private func baseline() -> StoreModel {
        var machine = ResyncMachine()
        _ = machine.handle(.connected(instance: "A", connection: 1))
        _ = machine.handle(.readsSucceeded(generation: 1, Mk.reads(seq: 1)))
        return machine.model
    }

    private func seized(_ claim: String, resource: String = "screen", from: String = "/work/app",
                        by: String = "/work/other", at: Int64) -> ClaimHistoryItem {
        ClaimHistoryItem(id: claim, resource: resource, kind: .lease, workspacePath: from,
                         endedAtMs: at, reason: .seized, detail: by)
    }

    private func foreignSeizure(_ id: String, at: Int64 = Mk.now) -> SeizureRecord {
        SeizureRecord(id: id, resource: "Pixel", fromWorkspacePath: "/work/tool", fromKind: .foreign,
                      fromSource: "agent-device", byWorkspacePath: "/work/app", byPid: 7, atMs: at,
                      stranded: false, forced: false)
    }

    @Test func atTheFirstReadDevicesAndOtherKindsAreABaselineAndRequestsGiveOneSummary() {
        var model = baseline()
        model.secretRequests = [Mk.secretRequest("r1"), Mk.secretRequest("r2")]
        model.accountRequests = [Mk.accountRequest("a1")]
        model.devices = Mk.devices(seq: 1, [Mk.device("d1", registered: false)])
        model.queue = Mk.queue(seq: 1, running: [Mk.entry("h1", grantedAtMs: Mk.now - 61 * 60_000)])
        model.claimHistory = [seized("c1", at: Mk.now)]
        let (notes, notified) = NotificationRules.evaluate(
            old: NotificationFacts(), new: facts(model), settings: NotificationSettings(),
            notified: NotifiedSubjects())
        #expect(notes == [AppNotification(kind: .request, id: "requests-waiting",
                                          title: "3 requests are waiting", body: "Open hostler to answer.",
                                          target: .popover)])
        #expect(notified.active == ["secret-request:r1", "secret-request:r2", "account-request:a1",
                                    "device:d1", "stuck:h1",
                                    "seizure:claims|screen|/work/app|/work/other|\(Mk.now)"])
        // The requests counted in the summary never notify one by one.
        var more = model
        more.secretRequests.append(Mk.secretRequest("r3", created: Mk.now + 1))
        let (next, _) = NotificationRules.evaluate(old: facts(model), new: facts(more),
                                                   settings: NotificationSettings(), notified: notified)
        #expect(next.map(\.id) == ["secret-request:r3"])
    }

    @Test func oneWaitingRequestAtTheFirstReadSaysSoAndNoneSaysNothing() {
        var model = baseline()
        model.secretRequests = [Mk.secretRequest("r1")]
        let (one, _) = NotificationRules.evaluate(old: NotificationFacts(), new: facts(model),
                                                  settings: NotificationSettings(), notified: NotifiedSubjects())
        #expect(one.map(\.title) == ["A request is waiting"])
        let (none, _) = NotificationRules.evaluate(old: NotificationFacts(), new: facts(baseline()),
                                                   settings: NotificationSettings(), notified: NotifiedSubjects())
        #expect(none.isEmpty)
        var off = NotificationSettings()
        off.request = false
        let (gated, _) = NotificationRules.evaluate(old: NotificationFacts(), new: facts(model),
                                                    settings: off, notified: NotifiedSubjects())
        #expect(gated.isEmpty)
    }

    @Test func eachKindNotifiesOnce() {
        let old = baseline()
        var new = old
        new.devices = Mk.devices(seq: 2, [Mk.device("d1", registered: false), Mk.device("d2"),
                                          Mk.device("d3", registered: false, connected: false)])
        new.secretRequests = [Mk.secretRequest("r1"), Mk.secretRequest("r2", state: .stored)]
        new.accountRequests = [Mk.accountRequest("a1")]
        new.queue = Mk.queue(seq: 2, running: [Mk.entry("h1", grantedAtMs: Mk.now - 61 * 60_000)])
        // One seizure of a hold and the lease under it: two history rows, one subject.
        new.claimHistory = [seized("c1", at: Mk.now), seized("c2", at: Mk.now)]
        new.seizures = [foreignSeizure("s9", at: Mk.now + 1)]

        let (notes, notified) = NotificationRules.evaluate(
            old: facts(old), new: facts(new), settings: NotificationSettings(), notified: NotifiedSubjects())
        #expect(notes.map(\.id) == ["device:d1", "secret-request:r1", "account-request:a1", "stuck:h1",
                                     "seizure:claims|screen|/work/app|/work/other|\(Mk.now)", "seizure:s9"])
        #expect(notes.map(\.kind) == [.newDevice, .request, .request, .stuckTask, .seizure, .seizure])
        #expect(notes.map(\.target) == [.device(ResourceId(rawValue: "d1")), .popover, .popover, .queue,
                                         .popover, .popover])
        #expect(notes[1].title == "Secret requested")
        #expect(notes[1].body == "An agent is waiting for a secret. Open hostler to answer.")
        #expect(notes[4].title == "A claim was taken over")
        #expect(notes[4].body == "Another workspace took it over. Open hostler to see which.")

        // The same facts again, or the same subject after it went away and came back: nothing.
        let (again, _) = NotificationRules.evaluate(
            old: facts(new), new: facts(new), settings: NotificationSettings(), notified: notified)
        #expect(again.isEmpty)
        var gone = new
        gone.queue = Mk.queue(seq: 3)
        gone.secretRequests = []
        let (_, afterGone) = NotificationRules.evaluate(
            old: facts(new), new: facts(gone), settings: NotificationSettings(), notified: notified)
        let (back, _) = NotificationRules.evaluate(
            old: facts(gone), new: facts(new), settings: NotificationSettings(), notified: afterGone)
        #expect(back.isEmpty)
    }

    @Test func seizuresBeforeTheFirstReadAndHostlerSeizureEventsAreNoSubjects() {
        var model = baseline()
        model.claimHistory = [seized("c0", at: since - 1), seized("c1", at: since)]
        // An event of a hostler claim's seizure: its history row is the subject.
        model.seizures = [Mk.seizure("s1"), foreignSeizure("s0", at: since - 1)]
        #expect(Set(facts(model).seizures!.keys) == ["seizure:claims|screen|/work/app|/work/other|\(since)"])
        // Before any read, no seizure facts at all.
        #expect(NotificationFacts(model: model, nowMs: Mk.now, seizuresSinceMs: nil).seizures == nil)
    }

    @Test func aDisabledKindIsRecordedButNotPosted() {
        let old = baseline()
        var new = old
        new.seizures = [foreignSeizure("s1")]
        new.secretRequests = [Mk.secretRequest("r1")]
        var settings = NotificationSettings()
        settings.seizure = false
        let (notes, notified) = NotificationRules.evaluate(
            old: facts(old), new: facts(new), settings: settings, notified: NotifiedSubjects())
        #expect(notes.map(\.id) == ["secret-request:r1"])
        #expect(notified.contains("seizure:s1"))
        #expect(NotificationKind.allCases.allSatisfy { NotificationSettings().allows($0) })
    }

    @Test func theLedgerStaysBoundedOverManySubjects() {
        // 20,000 requests, each pending for one evaluation, then answered.
        var notified = NotifiedSubjects()
        var old = facts(baseline())
        var posted = 0
        var largest = 0
        for index in 0..<20_000 {
            var model = baseline()
            model.secretRequests = [Mk.secretRequest("r\(index)")]
            let new = facts(model)
            let (notes, next) = NotificationRules.evaluate(old: old, new: new,
                                                           settings: NotificationSettings(), notified: notified)
            posted += notes.count
            notified = next
            largest = max(largest, notified.count)
            old = new
        }
        #expect(posted == 20_000)
        #expect(largest <= NotifiedSubjects.recentLimit + 1)
        // The latest subjects are still remembered; the oldest have aged out.
        #expect(notified.contains("secret-request:r19999"))
        #expect(notified.contains("secret-request:r\(20_000 - NotifiedSubjects.recentLimit)"))
        #expect(!notified.contains("secret-request:r0"))
    }

    @Test func aSubjectThatComesBackWithinTheWindowDoesNotRepeat() {
        var notified = NotifiedSubjects()
        notified.settle(current: ["device:d1"], notified: ["device:d1"])
        notified.settle(current: [], notified: [])
        #expect(notified.contains("device:d1"))
        #expect(notified.active.isEmpty)
        notified.settle(current: ["device:d1"], notified: [])
        #expect(notified.active == ["device:d1"])
        #expect(notified.count == 1)
    }

    @Test func settingsRoundTripAsJSON() throws {
        var settings = NotificationSettings()
        settings.newDevice = false
        let back = try JSONDecoder().decode(NotificationSettings.self, from: JSONEncoder().encode(settings))
        #expect(back == settings)
    }
}
