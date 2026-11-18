import Foundation
import Observation
import ServiceManagement

/// The login item's state as the app shows it (`SMAppService.Status`).
public enum LoginItemStatus: Equatable, Sendable {
    case enabled
    case notRegistered
    /// Registered, but the person must allow it in System Settings › General › Login Items.
    case requiresApproval
    /// macOS cannot register this copy: the app is not in /Applications (Task 0 facts §4).
    case notFound
}

/// "Open at login" (plan Task 9): `SMAppService.mainApp`, behind a protocol so the Settings
/// toggle is tested with a fake and hosted tests never register anything.
public protocol LoginItemControlling: Sendable {
    func status() -> LoginItemStatus
    func register() throws
    func unregister() throws
}

public struct MainAppLoginItem: LoginItemControlling {
    public init() {}

    public func status() -> LoginItemStatus {
        switch SMAppService.mainApp.status {
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .notFound
        case .notRegistered: .notRegistered
        @unknown default: .notRegistered
        }
    }

    public func register() throws { try SMAppService.mainApp.register() }
    public func unregister() throws { try SMAppService.mainApp.unregister() }
}

/// The Settings toggle. The system is the truth: `refresh()` reads it each time Settings
/// shows, so a login item removed in System Settings never shows as on.
@MainActor
@Observable
public final class LoginItemSetting {
    public static let installHint = String(localized:
        "Install the app in /Applications first (scripts/macos.sh install).")
    public static let approvalHint = String(localized:
        "Allow hostler in System Settings › General › Login Items.")

    public private(set) var status: LoginItemStatus = .notRegistered
    /// The last change's failure, with the status after it.
    public private(set) var error: String?

    @ObservationIgnored private let item: any LoginItemControlling
    @ObservationIgnored private let installed: Bool

    /// - Parameter appURL: the running app's bundle (`Bundle.main.bundleURL`).
    public init(item: any LoginItemControlling, appURL: URL) {
        self.item = item
        installed = Self.isInApplications(appURL)
    }

    /// The bundle sits directly in /Applications.
    public static func isInApplications(_ url: URL) -> Bool {
        url.standardizedFileURL.deletingLastPathComponent().path == "/Applications"
    }

    public var isOn: Bool { status == .enabled || status == .requiresApproval }

    /// From /Applications the toggle is always available: macOS reports `notFound` there until
    /// the first registration (Task 0 §4, Task 13 F-9). Elsewhere it is not, except to turn off
    /// an item that is on.
    public var isAvailable: Bool { installed || isOn }

    /// The line under the toggle, if any.
    public var hint: String? {
        if status == .requiresApproval { return Self.approvalHint }
        return installed || isOn ? nil : Self.installHint
    }

    public func refresh() {
        status = item.status()
    }

    public func set(_ on: Bool) {
        error = nil
        do {
            if on { try item.register() } else { try item.unregister() }
        } catch {
            let after = item.status()
            self.error = String(localized: "\(error.localizedDescription) (status: \(Self.text(after)))")
        }
        status = item.status()
    }

    static func text(_ status: LoginItemStatus) -> String {
        switch status {
        case .enabled: String(localized: "enabled")
        case .notRegistered: String(localized: "not registered")
        case .requiresApproval: String(localized: "waiting for approval")
        case .notFound: String(localized: "not found")
        }
    }
}
