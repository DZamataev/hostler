import Foundation
import Testing
@testable import HostlerKit

/// The unlock form (spec §9.6, plan Task 9): the password goes from the field to the service
/// as a `SecretText` and the field is cleared on every outcome; create needs two matching
/// entries; the key file is passed; the limit's wait is honoured; nothing reaches a log.
@Suite(.timeLimit(.minutes(1))) @MainActor struct UnlockControllerTests {
    final class FakeOpenPanel: OpenPanelPresenting {
        var answer: URL?
        private(set) var asked = 0
        func chooseFile(message: String) async -> URL? {
            asked += 1
            return answer
        }
    }

    @MainActor struct Fixture {
        let gate = GateFixture()
        let panel = FakeOpenPanel()
        let unlock: UnlockController

        init() {
            unlock = UnlockController(client: gate.client, limit: gate.limit, openPanel: panel,
                                      log: gate.log)
        }

        var transport: FakeValuesTransport { gate.transport }

        func sentUnlock() async -> Unlock? {
            for request in await transport.calls.reversed() {
                if case .secretsUnlock(let unlock) = request { return unlock }
            }
            return nil
        }
    }

    @Test func successSendsTheTypedPasswordAndClearsTheField() async throws {
        let f = Fixture()
        f.unlock.isFormOpen = true
        f.unlock.password.text = "correct horse"
        let ok = await f.unlock.unlock(create: false)
        #expect(ok)
        #expect(f.unlock.password.isEmpty)
        #expect(f.unlock.error == nil)
        #expect(!f.unlock.isFormOpen)
        let sent = try #require(await f.sentUnlock())
        #expect(sent.password == SecretText("correct horse"))
        #expect(sent.create == false)
        #expect(sent.keyFile == nil)
        #expect(sent.forgetKeyFile == false)
    }

    @Test func theFieldIsClearedTheMomentTheCallIsSent() async throws {
        let f = Fixture()
        await f.transport.set { $0.holdUnlock = true }
        f.unlock.password.text = "correct horse"
        async let ok = f.unlock.unlock(create: false)
        try await storeSoon("the unlock is in flight") { await f.transport.unlockWaiting == 1 }
        #expect(f.unlock.password.isEmpty)
        #expect(f.unlock.isWorking)
        await f.transport.releaseUnlock()
        #expect(await ok)
        #expect(!f.unlock.isWorking)
    }

    @Test func aWrongPasswordShowsTheServicesErrorAndClearsTheField() async throws {
        let f = Fixture()
        f.unlock.isFormOpen = true
        f.unlock.password.text = "wrong"
        let ok = await f.unlock.unlock(create: false)
        #expect(!ok)
        #expect(f.unlock.password.isEmpty)
        // Task 13 F-5: the app's words, no CLI line.
        #expect(f.unlock.error == .service(message: "Wrong master password or key file.",
                                           recovery: "Check the password and the key file, then try again."))
        #expect(f.unlock.isFormOpen)
    }

    @Test func anEmptyPasswordSendsNothing() async {
        let f = Fixture()
        let ok = await f.unlock.unlock(create: false)
        #expect(!ok)
        #expect(f.unlock.error == .emptyValue(field: "The master password"))
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func createNeedsTwoMatchingEntries() async throws {
        let f = Fixture()
        f.unlock.password.text = "new one"
        f.unlock.confirmation.text = "new 0ne"
        #expect(await f.unlock.unlock(create: true) == false)
        #expect(f.unlock.error == .passwordsDiffer)
        #expect(f.unlock.password.isEmpty)
        #expect(f.unlock.confirmation.isEmpty)
        #expect(await f.transport.calls.isEmpty)

        f.unlock.password.text = "new one"
        f.unlock.confirmation.text = "new one"
        #expect(await f.unlock.unlock(create: true))
        let sent = try #require(await f.sentUnlock())
        #expect(sent.create)
        #expect(sent.password == SecretText("new one"))
        #expect(f.unlock.confirmation.isEmpty)
    }

    @Test func theChosenKeyFileIsPassedAndForgetToo() async throws {
        let f = Fixture()
        f.panel.answer = URL(fileURLWithPath: "/Users/someone/keys/hostler.keyx")
        await f.unlock.chooseKeyFile()
        #expect(f.panel.asked == 1)
        #expect(f.unlock.keyFile == "/Users/someone/keys/hostler.keyx")
        f.unlock.forgetKeyFile = true
        f.unlock.password.text = "correct horse"
        #expect(await f.unlock.unlock(create: false))
        let sent = try #require(await f.sentUnlock())
        #expect(sent.keyFile == "/Users/someone/keys/hostler.keyx")
        #expect(sent.forgetKeyFile)
        // A success clears the choice for the next unlock.
        #expect(f.unlock.keyFile == nil)
        #expect(!f.unlock.forgetKeyFile)
    }

    @Test func aCancelledChooserKeepsTheEarlierChoice() async {
        let f = Fixture()
        f.panel.answer = URL(fileURLWithPath: "/k/one.keyx")
        await f.unlock.chooseKeyFile()
        f.panel.answer = nil
        await f.unlock.chooseKeyFile()
        #expect(f.unlock.keyFile == "/k/one.keyx")
        f.unlock.clearKeyFile()
        #expect(f.unlock.keyFile == nil)
    }

    @Test func theLimitsWaitIsShownAndNothingIsSentBeforeIt() async throws {
        let f = Fixture()
        await f.transport.set { $0.limitedMs = 8_000 }
        f.unlock.password.text = "correct horse"
        #expect(await f.unlock.unlock(create: false) == false)
        #expect(f.unlock.error == nil)
        #expect(f.gate.limit.waitText == ProofLimit.waitText(seconds: 8))
        f.unlock.password.text = "correct horse"
        #expect(await f.unlock.unlock(create: false) == false)
        #expect(f.unlock.password.isEmpty)
        #expect(await f.transport.count(.secretsUnlock) == 1)
        f.gate.clock.advance(by: 8)
        await f.transport.set { $0.limitedMs = nil }
        f.unlock.password.text = "correct horse"
        #expect(await f.unlock.unlock(create: false))
        #expect(await f.transport.count(.secretsUnlock) == 2)
    }

    @Test func lockSendsLockAndReportsAFailure() async throws {
        let f = Fixture()
        #expect(await f.unlock.lock())
        #expect(await f.transport.methods == [.secretsLock])
        await f.transport.queue(.notConnected, for: .secretsLock)
        #expect(await f.unlock.lock() == false)
        #expect(f.unlock.error == .service(message: "hostler's service is not running", recovery: nil))
    }

    @Test func resetClearsTheFieldsAndTheErrorButKeepsTheKeyFile() async {
        let f = Fixture()
        f.panel.answer = URL(fileURLWithPath: "/k/one.keyx")
        await f.unlock.chooseKeyFile()
        f.unlock.password.text = "typed"
        f.unlock.confirmation.text = "typed"
        _ = await f.unlock.unlock(create: true)  // succeeds, then type again
        f.unlock.password.text = "typed again"
        f.unlock.reset()
        #expect(f.unlock.password.isEmpty)
        #expect(f.unlock.confirmation.isEmpty)
        #expect(f.unlock.error == nil)
    }

    @Test func nothingTypedReachesTheLog() async {
        let f = Fixture()
        f.unlock.password.text = "wrong-marker-5512"
        _ = await f.unlock.unlock(create: false)
        f.unlock.password.text = "correct horse"
        _ = await f.unlock.unlock(create: false)
        #expect(f.gate.log.lines.count >= 2)
        for line in f.gate.log.lines {
            #expect(!line.contains("marker-5512"))
            #expect(!line.contains("correct horse"))
        }
    }
}
