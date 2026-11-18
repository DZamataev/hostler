import Foundation
import HostlerKit
@testable import Hostler

extension AppEnvironment {
    /// An environment of fakes only: nothing in it reaches AppKit, the status bar or the system.
    @MainActor static func fake(
        makeStatusItem: (@MainActor (AppModel) -> any StatusItemHosting)? = nil,
        windowRaiser: any WindowRaising = FakeWindowRaiser(),
        activationPolicy: any ActivationPolicyControlling = FakeActivationPolicy(),
        connection: ConnectionEnvironment? = nil,
        commandRunner: any HostlerCommandRunning = NoCommandRunner(),
        notificationCenter: any UserNotificationCentering = FakeNotificationCenter(),
        notificationSettings: any NotificationSettingsStoring = MemoryNotificationSettings(),
        openPanel: any OpenPanelPresenting = NoOpenPanel(),
        pasteboard: any PasteboardWriting = MemoryPasteboard(),
        loginItem: any LoginItemControlling = NoLoginItem(),
        sleeper: any Sleeping = NeverWakes()
    ) -> AppEnvironment {
        AppEnvironment(makeStatusItem: makeStatusItem,
                       windowRaiser: windowRaiser,
                       activationPolicy: activationPolicy,
                       connection: connection,
                       commandRunner: commandRunner,
                       notificationCenter: notificationCenter,
                       notificationSettings: notificationSettings,
                       openPanel: openPanel,
                       pasteboard: pasteboard,
                       loginItem: loginItem,
                       clock: SystemClock(),
                       sleeper: sleeper,
                       appURL: URL(fileURLWithPath: "/tmp/hostler-tests/Hostler.app"))
    }
}

/// A CLI runner that runs nothing (Task 7's wiring; the CLI's own tests are in HostlerKitTests).
struct NoCommandRunner: HostlerCommandRunning {
    func run(_ invocation: CLIInvocation) async -> CLIResult {
        .notStarted("no CLI in tests")
    }
}

/// A notification center that records what it is asked (Task 9).
@MainActor
final class FakeNotificationCenter: UserNotificationCentering {
    var authorized: Bool?
    /// What `requestAuthorization` answers: a value, an error, or (nil) never — the prompt
    /// left unanswered.
    var answer: Result<Bool, any Error>? = .success(true)
    var onTap: (@MainActor (NotificationTarget) -> Void)?
    private(set) var activated = 0
    private(set) var asked = 0
    private(set) var added: [AppNotification] = []
    private var unanswered: [CheckedContinuation<Bool, any Error>] = []

    init(authorized: Bool? = nil) { self.authorized = authorized }

    func activate() { activated += 1 }
    func isAuthorized() async -> Bool? { authorized }

    func requestAuthorization() async throws -> Bool {
        asked += 1
        guard let answer else {
            return try await withCheckedThrowingContinuation { unanswered.append($0) }
        }
        let granted = try answer.get()
        authorized = granted
        return granted
    }

    func add(_ notification: AppNotification) async throws { added.append(notification) }
}

final class MemoryNotificationSettings: NotificationSettingsStoring {
    var stored: NotificationSettings
    init(_ settings: NotificationSettings = NotificationSettings()) { stored = settings }
    func load() -> NotificationSettings { stored }
    func save(_ settings: NotificationSettings) { stored = settings }
}

@MainActor
final class NoOpenPanel: OpenPanelPresenting {
    func chooseFile(message: String) async -> URL? { nil }
}

@MainActor
final class MemoryPasteboard: PasteboardWriting {
    var changeCount = 0
    func writeConcealed(_ value: SecretText) -> Int {
        changeCount += 1
        return changeCount
    }
    func clear() { changeCount += 1 }
}

struct NoLoginItem: LoginItemControlling {
    func status() -> LoginItemStatus { .notFound }
    func register() throws {}
    func unregister() throws {}
}

/// A sleeper that never wakes (until cancelled): no timer fires in a hosted test.
struct NeverWakes: Sleeping {
    func sleep(for duration: Duration) async throws {
        try await Task.sleep(for: .seconds(3600))
    }
}
