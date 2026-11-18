import Foundation
import Testing
@testable import HostlerKit

/// Notification texts are generic and bounded (review t9-r1 R1-1, P5-L17): Notification Center
/// keeps banners and history after the app hides its values or quits, so no agent-supplied free
/// text (a request's reason, a command, a project, secret, account or folder name, a device's
/// own name) ever reaches a title or body. Ids and targets stay for navigation.
@Suite struct NotificationTextsTests {
    static let marker = "sk-live-MARKER-7f3a"

    /// Every notification the rules can build, each with the marker in every free-text field.
    static func everyNotification() -> [AppNotification] {
        let m = marker
        var device = Mk.device("d1", registered: false)
        device.name = "phone \(m)"
        device.model = "model \(m)"
        device.alias = "alias \(m)"
        let secret = SecretRequestInfo(
            id: "r1", projectId: "p", projectName: "proj \(m)", name: "KEY_\(m)",
            reason: "use \(m) please", state: .pending, requesterPid: 1, createdAtMs: Mk.now)
        let account = AccountRequestInfo(
            id: "a1", projectId: "p", projectName: "proj \(m)", name: "acct-\(m)",
            fields: ["f-\(m)"], reason: "reason \(m)", state: .pending, requesterPid: 1,
            createdAtMs: Mk.now)
        var task = Mk.entry("h1", grantedAtMs: Mk.now - 61 * 60_000)
        task.command = "curl -H 'Authorization: \(m)' https://example.invalid"
        task.cwd = "/work/\(m)"
        task.workspaceRoot = "/work/\(m)"
        let seizure = SeizureFact(resource: "res \(m)", fromWorkspacePath: "/work/from-\(m)",
                                  byWorkspacePath: "/work/by-\(m)", atMs: Mk.now)
        return [
            NotificationRules.newDevice(device),
            NotificationRules.secretRequested(secret),
            NotificationRules.accountRequested(account),
            NotificationRules.stuck(task),
            NotificationRules.seized(seizure, id: "seizure:s9"),
            NotificationRules.requestsWaiting(1),
            NotificationRules.requestsWaiting(3),
        ]
    }

    @Test func noFreeTextReachesATitleOrBody() {
        for note in Self.everyNotification() {
            #expect(!note.title.contains(Self.marker), "\(note.kind) title")
            #expect(!note.body.contains(Self.marker), "\(note.kind) body")
            #expect(!note.title.contains("MARKER"))
            #expect(!note.body.contains("MARKER"))
        }
    }

    @Test func textsAreBounded() {
        for note in Self.everyNotification() {
            #expect(note.title.count <= 60, "\(note.title)")
            #expect(note.body.count <= 120, "\(note.body)")
        }
    }

    @Test func idsAndTargetsStayForNavigation() {
        let notes = Self.everyNotification()
        #expect(notes.map(\.id) == ["device:d1", "secret-request:r1", "account-request:a1",
                                    "stuck:h1", "seizure:s9", "requests-waiting", "requests-waiting"])
        #expect(notes.map(\.target) == [.device(ResourceId(rawValue: "d1")), .popover, .popover,
                                        .queue, .popover, .popover, .popover])
    }

    @Test func theTexts() {
        let notes = Self.everyNotification()
        #expect(notes[0].title == "New device connected")
        #expect(notes[0].body == "An Android device is not registered with hostler yet. Open hostler to add it.")
        #expect(notes[1].title == "Secret requested")
        #expect(notes[1].body == "An agent is waiting for a secret. Open hostler to answer.")
        #expect(notes[2].title == "Account requested")
        #expect(notes[2].body == "An agent is waiting for an account. Open hostler to answer.")
        #expect(notes[3].title == "A heavy task looks stuck")
        #expect(notes[3].body == "It has run longer than usual. Open hostler's queue to see it.")
        #expect(notes[4].title == "A claim was taken over")
        #expect(notes[4].body == "Another workspace took it over. Open hostler to see which.")
    }
}
