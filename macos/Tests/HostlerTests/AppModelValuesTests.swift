import Foundation
import HostlerKit
import Testing
@testable import Hostler

/// Task 9's wiring in `AppModel` (P5-L6, additive): the notifier feeds the store's settings; a
/// notification click routes; closing the popover clears what was typed; the values features
/// share one limit and one gate; nothing starts without `start()`.
@Suite(.timeLimit(.minutes(1))) @MainActor struct AppModelValuesTests {
    @Test func theFeaturesShareOneGateAndOneLimit() {
        let model = AppModel(env: .fake())
        #expect(model.unlock.limit === model.limit)
        #expect(model.gate.limit === model.limit)
    }

    @Test func notificationSettingsReachTheStore() throws {
        let model = AppModel(env: .fake(connection: AppModelConnectionTests.environment(
            AppModelConnectionTests.Counter())))
        let store = try #require(model.store)
        model.notifier.settings.newDevice = false
        #expect(store.notificationSettings.newDevice == false)
    }

    @Test func nothingIsAskedBeforeStart() {
        let center = FakeNotificationCenter()
        _ = AppModel(env: .fake(notificationCenter: center))
        #expect(center.activated == 0)
        #expect(center.asked == 0)
    }

    @Test func closingThePopoverClearsWhatWasTyped() async throws {
        let model = AppModel(env: .fake())
        model.unlock.password.text = "typed"
        model.unlock.confirmation.text = "typed"
        let card = RequestCard(id: "s-1", kind: .secret, projectId: "p", projectName: "app",
                               name: "API_KEY", purpose: nil, reason: "r", requesterPid: 1,
                               createdAtMs: 0, fields: ["value"])
        model.requests.entry(card, field: "value").text = "typed"
        async let gated = model.gate.perform("Reveal X", from: .popover) {
            (client: HostlerClient) async throws(HostlerCallError) -> Int in 1
        }
        let deadline = Date().addingTimeInterval(10)
        while model.gate.phase != .asking, Date() < deadline { try await Task.sleep(for: .milliseconds(2)) }
        model.gate.password.text = "typed"
        model.popoverClosed()
        #expect(model.unlock.password.isEmpty)
        #expect(model.unlock.confirmation.isEmpty)
        #expect(model.requests.entry(card, field: "value").isEmpty)
        #expect(model.gate.password.isEmpty)
        guard case .cancelled = await gated else { Issue.record("not cancelled"); return }
    }

    @Test func aRequestNotificationRevealsThePopover() {
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }))
        model.start()
        model.open(.popover)
        #expect(host.revealPanelCount == 1)
    }

    @Test func aQueueNotificationRaisesOrOpensTheWindow() {
        let raiser = FakeWindowRaiser()
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }, windowRaiser: raiser))
        model.start()
        var opened = 0
        model.openMainWindow = { opened += 1 }
        model.open(.queue)
        #expect(opened == 1)
        #expect(model.requestedSection == .queue)
        raiser.hasEligibleWindows = true
        model.open(.device(ResourceId(rawValue: "android:R5")))
        #expect(raiser.raiseAllCount == 1)
        #expect(opened == 1)
        #expect(model.requestedSection == .device(ResourceId(rawValue: "android:R5")))
    }

    /// R1-3: quitting clears every typed field and cancels a waiting gate synchronously, at the
    /// start of `shutdown()`, before it awaits the connection's close.
    @Test func shutdownClearsTypedValuesAndCancelsTheGateFirst() async throws {
        let model = AppModel(env: .fake(connection: AppModelConnectionTests.environment(
            AppModelConnectionTests.Counter())))
        model.unlock.password.text = "typed"
        model.unlock.confirmation.text = "typed"
        let card = RequestCard(id: "a-1", kind: .account, projectId: "p", projectName: "app",
                               name: "login", purpose: nil, reason: "r", requesterPid: 1,
                               createdAtMs: 0, fields: ["user", "password"])
        model.requests.entry(card, field: "user").text = "typed"
        model.requests.entry(card, field: "password").text = "typed"
        async let gated = model.gate.perform("Reveal X", from: .window) {
            (client: HostlerClient) async throws(HostlerCallError) -> Int in 1
        }
        let deadline = Date().addingTimeInterval(10)
        while model.gate.phase != .asking, Date() < deadline { try await Task.sleep(for: .milliseconds(2)) }
        model.gate.password.text = "typed"

        // The clearing is shutdown's synchronous start (before it awaits the connection), so
        // the first turn of the shutdown task leaves nothing typed.
        let shutdown = Task { await model.shutdown() }
        while model.gate.phase != .idle, Date() < deadline { await Task.yield() }
        #expect(model.unlock.password.isEmpty)
        #expect(model.unlock.confirmation.isEmpty)
        #expect(model.requests.entry(card, field: "user").isEmpty)
        #expect(model.requests.entry(card, field: "password").isEmpty)
        #expect(model.gate.password.isEmpty)
        guard case .cancelled = await gated else { Issue.record("not cancelled"); return }
        await shutdown.value
        #expect(model.hasShutDown)
    }

    /// Task 13 F-1: the close hook clears everything typed in the popover and closes its unlock
    /// form, unless the key-file chooser closed it on purpose.
    @Test func closingClosesTheUnlockFormUnlessTheKeyFileChooserIsOpen() async {
        let model = AppModel(env: .fake())
        model.unlock.isFormOpen = true
        model.unlock.password.text = "typed"
        model.popoverClosed()
        #expect(!model.unlock.isFormOpen)
        #expect(model.unlock.password.isEmpty)

        let panel = HeldOpenPanel()
        let model2 = AppModel(env: .fake(openPanel: panel))
        model2.unlock.isFormOpen = true
        let choosing = Task { await model2.chooseKeyFile() }
        while !panel.asked { await Task.yield() }
        model2.popoverClosed()
        #expect(model2.unlock.isFormOpen)
        panel.finish(URL(fileURLWithPath: "/k/one.keyx"))
        await choosing.value
        #expect(model2.unlock.isFormOpen)
        #expect(model2.unlock.keyFile == "/k/one.keyx")
    }

    /// Task 13 F-1 (b): opening the popover retries a connection that is not live at once.
    @Test func openingThePopoverRetriesAtOnce() async throws {
        let connector = AppModelConnectionTests.Counter()
        let model = AppModel(env: .fake(connection: AppModelConnectionTests.environment(connector)))
        model.start()
        let deadline = Date().addingTimeInterval(10)
        while connector.connects < 1, Date() < deadline { try await Task.sleep(for: .milliseconds(5)) }
        #expect(connector.connects == 1)
        model.popoverOpened()
        while connector.connects < 2, Date() < deadline { try await Task.sleep(for: .milliseconds(5)) }
        #expect(connector.connects == 2)
        await model.shutdown()
    }
}

/// An open panel that waits until the test answers it.
@MainActor final class HeldOpenPanel: OpenPanelPresenting {
    private(set) var asked = false
    private var continuation: CheckedContinuation<URL?, Never>?
    func chooseFile(message: String) async -> URL? {
        asked = true
        return await withCheckedContinuation { continuation = $0 }
    }
    func finish(_ url: URL?) { continuation?.resume(returning: url); continuation = nil }
}
