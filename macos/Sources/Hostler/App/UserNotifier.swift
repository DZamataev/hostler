import AppKit
import CryptoKit
import Foundation
import HostlerKit
import Observation
import UserNotifications

/// macOS's notification permission for this app, as Settings shows it (P5-L5: every state is
/// a normal one; notifications are a convenience, since everything they announce is also in the
/// popover or the window).
enum NotificationPermission: Equatable, Sendable {
    /// Not read yet.
    case unknown
    /// macOS has not asked; it asks when the first kind is on.
    case notAsked
    /// The macOS prompt is up (`requestAuthorization` returns only when it is answered, which
    /// may take minutes): nothing is queued or retried meanwhile.
    case waiting
    case allowed
    case denied
    /// `requestAuthorization` threw (e.g. `UNErrorDomain` 1 on a build whose signature does
    /// not bind its Info.plist, Task 0 §3): notifications stay off.
    case refused(domain: String, code: Int)

    /// The line Settings shows; `nil` when allowed.
    var settingsText: String? {
        switch self {
        case .unknown: String(localized: "Checking macOS's notification permission…")
        case .notAsked: String(localized: "macOS asks for permission when a kind is turned on.")
        case .waiting: String(localized: "Waiting for your answer in the macOS prompt.")
        case .allowed: nil
        case .denied:
            String(localized: "Notifications are off for hostler. Allow them in System Settings › Notifications › Hostler.")
        case .refused(let domain, let code):
            String(localized: "macOS refused notifications for this build (\(domain) \(code)).")
        }
    }
}

/// The OS side of notifications, behind a protocol so `UserNotifier` is tested with a fake and
/// hosted tests never touch the notification center (P5-L5).
@MainActor
protocol UserNotificationCentering: AnyObject {
    /// Called once at launch, before anything else: the real center sets its delegate here,
    /// so a click on a notification that launched the app is delivered.
    func activate()
    /// `nil`: not determined yet.
    func isAuthorized() async -> Bool?
    func requestAuthorization() async throws -> Bool
    func add(_ notification: AppNotification) async throws
    /// A click on one of our notifications.
    var onTap: (@MainActor (NotificationTarget) -> Void)? { get set }
}

/// Where the notification settings live (`UserDefaults` in the app).
protocol NotificationSettingsStoring: AnyObject {
    func load() -> NotificationSettings
    func save(_ settings: NotificationSettings)
}

final class DefaultsNotificationSettings: NotificationSettingsStoring {
    static let key = "notificationSettings"
    private let defaults: UserDefaults

    init(_ defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func load() -> NotificationSettings {
        guard let data = defaults.data(forKey: Self.key),
              let settings = try? JSONDecoder().decode(NotificationSettings.self, from: data)
        else { return NotificationSettings() }
        return settings
    }

    func save(_ settings: NotificationSettings) {
        if let data = try? JSONEncoder().encode(settings) { defaults.set(data, forKey: Self.key) }
    }
}

/// `Notifying` through `UNUserNotificationCenter` (spec §9.4, plan Task 9).
/// - Authorization is asked once, when the first kind is on (at start, or when Settings turns
///   one on); a long wait for the answer and a refusal are normal states, shown in Settings.
/// - A notification is posted only while allowed and its kind is on; nothing is queued for
///   later.
/// - A click opens its target through `onOpen` (`AppModel.open(_:)`).
@MainActor
@Observable
final class UserNotifier: Notifying {
    private(set) var permission: NotificationPermission = .unknown
    var settings: NotificationSettings {
        didSet {
            guard settings != oldValue else { return }
            store.save(settings)
            onSettingsChange?(settings)
            if settings.anyOn, permission == .notAsked { ask() }
        }
    }
    @ObservationIgnored var onOpen: (@MainActor (NotificationTarget) -> Void)?
    /// The store's copy follows (`HostlerStore.notificationSettings`).
    @ObservationIgnored var onSettingsChange: (@MainActor (NotificationSettings) -> Void)?

    @ObservationIgnored private let center: any UserNotificationCentering
    @ObservationIgnored private let store: any NotificationSettingsStoring
    @ObservationIgnored private var started = false

    init(center: any UserNotificationCentering, store: any NotificationSettingsStoring) {
        self.center = center
        self.store = store
        settings = store.load()
        center.onTap = { [weak self] target in self?.onOpen?(target) }
    }

    /// At launch (never while hosting tests): reads the permission, and asks if a kind is on.
    func start() {
        guard !started else { return }
        started = true
        center.activate()
        refresh(askIfNeeded: true)
    }

    /// Re-reads the permission (Settings shows); it never interrupts a prompt that waits.
    func refresh(askIfNeeded: Bool = false) {
        guard permission != .waiting else { return }
        Task { [weak self, center] in
            let authorized = await center.isAuthorized()
            guard let self, self.permission != .waiting else { return }
            switch authorized {
            case true?: self.permission = .allowed
            case false?:
                if case .refused = self.permission { return }
                self.permission = .denied
            case nil:
                if case .refused = self.permission { return }
                self.permission = .notAsked
                if askIfNeeded, self.settings.anyOn { self.ask() }
            }
        }
    }

    func post(_ notification: AppNotification) {
        guard permission == .allowed, settings.allows(notification.kind) else { return }
        Task { [center] in
            do {
                try await center.add(notification)
            } catch {
                Log.app.error("a notification was not posted")
            }
        }
    }

    private func ask() {
        permission = .waiting
        Task { [weak self, center] in
            let result: NotificationPermission
            do {
                result = try await center.requestAuthorization() ? .allowed : .denied
            } catch {
                let ns = error as NSError
                result = .refused(domain: ns.domain, code: ns.code)
            }
            self?.permission = result
        }
    }
}

extension NotificationSettings {
    var anyOn: Bool { NotificationKind.allCases.contains { allows($0) } }
}

/// The real center: `UNUserNotificationCenter.current()` and its delegate.
@MainActor
final class SystemNotificationCenter: NSObject, UserNotificationCentering,
    UNUserNotificationCenterDelegate
{
    var onTap: (@MainActor (NotificationTarget) -> Void)?
    /// Reached on first use only (`UserNotifier.start()`), so building the environment while
    /// hosting tests touches no notification center (P5-L5).
    private var attached: UNUserNotificationCenter?

    private var center: UNUserNotificationCenter {
        if let attached { return attached }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        attached = center
        return center
    }

    func activate() {
        _ = center
    }

    func isAuthorized() async -> Bool? {
        switch await center.notificationSettings().authorizationStatus {
        case .authorized, .provisional, .ephemeral: true
        case .denied: false
        case .notDetermined: nil
        @unknown default: nil
        }
    }

    func requestAuthorization() async throws -> Bool {
        try await center.requestAuthorization(options: [.alert, .sound])
    }

    func add(_ notification: AppNotification) async throws {
        try await center.add(Self.request(for: notification))
        // Debug only and with no content (Task 13 F-10): a verifier can see delivery in the log.
        Log.app.debug("a notification was handed to Notification Center")
    }

    /// What Notification Center receives and keeps: the notification's (generic, bounded) title
    /// and body, its target for the click, and an identifier made from a digest of its id —
    /// a seizure's id holds workspace paths, which are not shown and not kept either
    /// (review t9-r1 R1-1).
    nonisolated static func request(for notification: AppNotification) -> UNNotificationRequest {
        let content = UNMutableNotificationContent()
        content.title = notification.title
        content.body = notification.body
        content.userInfo = [NotificationTargetCoding.key: NotificationTargetCoding.encode(notification.target)]
        return UNNotificationRequest(
            identifier: "\(notification.kind.rawValue):\(digest(notification.id))", content: content,
            trigger: nil)
    }

    /// The first 16 hex digits of the id's SHA-256: stable per subject, so a repeat replaces
    /// the earlier banner, and says nothing about the subject.
    nonisolated static func digest(_ id: String) -> String {
        SHA256.hash(data: Data(id.utf8)).prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        Log.app.debug("a notification is being presented")
        return [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
    ) async {
        let text = response.notification.request.content.userInfo[NotificationTargetCoding.key] as? String
        await MainActor.run {
            self.onTap?(text.flatMap(NotificationTargetCoding.decode) ?? .popover)
        }
    }
}

/// A notification's target in its `userInfo`, as a string.
enum NotificationTargetCoding {
    static let key = "hostler.target"

    static func encode(_ target: NotificationTarget) -> String {
        switch target {
        case .popover: "popover"
        case .queue: "queue"
        case .device(let id): "device:\(id.rawValue)"
        }
    }

    static func decode(_ text: String) -> NotificationTarget? {
        switch text {
        case "popover": return .popover
        case "queue": return .queue
        default:
            guard text.hasPrefix("device:") else { return nil }
            return .device(ResourceId(rawValue: String(text.dropFirst("device:".count))))
        }
    }
}
