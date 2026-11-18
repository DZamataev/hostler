import HostlerKit
import Testing
@testable import Hostler

@Suite @MainActor struct AppModelTests {
    /// `AppEnvironment` builds with fakes only (plan Task 2), and nothing is built before
    /// `start()`.
    @Test func aModelOfFakesHasNoStatusItemBeforeStart() {
        let model = AppModel(env: .fake(makeStatusItem: { _ in FakeStatusItemHost() }))
        #expect(model.statusItem == nil)
        #expect(model.dockIcon.owners.isEmpty)
    }

    @Test func startBuildsTheStatusItemOnce() {
        let built = Box(0)
        let model = AppModel(env: .fake(makeStatusItem: { _ in
            built.value += 1
            return FakeStatusItemHost()
        }))
        model.start()
        model.start()
        #expect(built.value == 1)
        #expect(model.statusItem != nil)
    }

    @Test func closeMenuPanelClosesTheShownPanel() {
        let host = FakeStatusItemHost()
        let model = AppModel(env: .fake(makeStatusItem: { _ in host }))
        model.start()
        host.revealPanel()
        model.closeMenuPanel()
        #expect(host.closePanelCount == 1)
        #expect(!host.isPanelShown)
    }

    /// The Dock icon follows the environment's policy: the main window owns it.
    @Test func theDockIconUsesTheEnvironmentsPolicy() {
        let policy = FakeActivationPolicy()
        let model = AppModel(env: .fake(activationPolicy: policy))
        model.dockIcon.open(.main)
        model.dockIcon.close(.main)
        #expect(policy.calls == [true, false])
    }

    /// Until the connection exists (Task 7) and the store derives it (Task 8), the image says
    /// the service is not reachable.
    @Test func theImageStartsOffline() {
        #expect(AppModel(env: .fake()).statusItemAppearance == .offline)
    }

    @Test func theLiveEnvironmentBuildsWithoutTouchingTheStatusBar() {
        let env = AppEnvironment.live(launch: .plain)
        #expect(env.makeStatusItem != nil)
    }
}
