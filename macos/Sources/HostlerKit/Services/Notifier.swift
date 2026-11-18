import Foundation

/// Posts the app's notifications (spec §9.4). OS-facing: Task 9's `UserNotifier` posts through
/// `UNUserNotificationCenter`; tests use `FakeNotifier`; `SilentNotifier` posts nothing.
@MainActor
public protocol Notifying: AnyObject {
    func post(_ notification: AppNotification)
}

/// A notifier that drops everything: the store's default until the app gives it the real one.
@MainActor
public final class SilentNotifier: Notifying {
    public init() {}
    public func post(_ notification: AppNotification) {}
}
