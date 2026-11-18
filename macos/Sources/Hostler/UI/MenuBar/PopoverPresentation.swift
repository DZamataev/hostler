import Foundation
import HostlerKit

/// The popover's texts as pure functions (the reference app's `MenuPanelPresentation`), so the
/// rules are tested without a view (`PopoverPresentationTests`).
enum PopoverPresentation {
    // MARK: - The header (spec §9.2 item 1)

    enum Dot: Equatable {
        case live, offline, refused
    }

    static func header(_ state: ConnectionState) -> (dot: Dot, text: String) {
        switch state {
        case .live: (.live, String(localized: "Live"))
        case .connecting, .notRunning, .protocolError: (.offline, String(localized: "Offline"))
        case .denied, .foreignOwner, .mismatch, .badHome: (.refused, String(localized: "Refused"))
        }
    }

    // MARK: - The connection banner (spec §9.2 item 2, §9.7)

    enum BannerAction: Equatable {
        /// Runs the registered binary's `service start` (only for a `.start` offer).
        case start
        case retry
        /// Copies the command to type.
        case copy(String)
    }

    struct Banner: Equatable {
        var title: String
        var detail: String?
        var actions: [BannerAction]
    }

    /// What is wrong and what fixes it; `nil` while live.
    static func banner(_ state: ConnectionState) -> Banner? {
        switch state {
        case .live:
            return nil
        case .connecting:
            return Banner(title: String(localized: "Connecting to hostler…"), detail: nil, actions: [])
        case .notRunning(.start):
            return Banner(title: String(localized: "hostler's service is not running."),
                          detail: String(localized: "Start runs the registered service."),
                          actions: [.start, .retry])
        case .notRunning(.typeCommand(let command)):
            return Banner(title: String(localized: "hostler's service is not running."),
                          detail: String(localized: "Start it in a terminal: \(command)"),
                          actions: [.copy(command), .retry])
        case .denied(let reason):
            return Banner(title: String(localized: "This app may not reach hostler's socket."),
                          detail: reason, actions: [.retry])
        case .foreignOwner(let path):
            return Banner(title: String(localized: "hostler's socket belongs to another user."),
                          detail: String(localized: "The app does not use \(path)."), actions: [.retry])
        case .mismatch(let client, let service, let version, let recovery):
            let versions = String(localized:
                "This app speaks contract \(client); the service (hostler \(version)) speaks \(service).")
            return Banner(title: String(localized: "The app and the service do not match."),
                          detail: [versions, recovery].compactMap { $0 }.joined(separator: " "),
                          actions: recovery.map { [.copy($0), .retry] } ?? [.retry])
        case .protocolError(let text):
            return Banner(title: String(localized: "hostler's service did not answer properly."),
                          detail: text, actions: [.retry])
        case .badHome(let home):
            return Banner(title: String(localized: "HOSTLER_HOME must be an absolute path."),
                          detail: String(localized: "It is \"\(home)\"; fix it and open the app again."),
                          actions: [])
        }
    }

    // MARK: - The lock row (spec §9.2 item 3)

    static func lockText(_ state: LockState?) -> String {
        switch state {
        case .unlocked: String(localized: "Secrets unlocked")
        case .locked: String(localized: "Secrets locked")
        case .missing: String(localized: "No secrets database yet")
        case nil: String(localized: "Secrets: reading…")
        }
    }

    static let lockWarning = String(localized: "Agents' runs that need secrets will be refused.")

    // MARK: - Request cards (spec §9.2 item 4)

    /// The card's title: the name, and "account" for an account request.
    static func cardTitle(_ card: RequestCard) -> String {
        card.isAccount ? String(localized: "\(card.name) (account)") : card.name
    }

    /// "app · pid 4242 · 3 min ago".
    static func cardSubtitle(_ card: RequestCard, nowMs: Int64) -> String {
        String(localized:
            "\(card.projectName) · pid \(String(card.requesterPid)) · \(age(sinceMs: card.createdAtMs, nowMs: nowMs))")
    }

    static func age(sinceMs: Int64, nowMs: Int64) -> String {
        let seconds = max(0, (nowMs - sinceMs) / 1000)
        switch seconds {
        case ..<60: return String(localized: "just now")
        case ..<3600: return String(localized: "\(seconds / 60) min ago")
        case ..<86_400: return String(localized: "\(seconds / 3600) h ago")
        default: return String(localized: "\(seconds / 86_400) d ago")
        }
    }

    /// The cards shown above the scrolling region; the rest scroll with it (a long list must
    /// not push the lower region off a short display).
    static let fixedCards = 2

    // MARK: - The queue in short (spec §9.2 item 5)

    static func elapsed(sinceMs: Int64?, nowMs: Int64) -> String {
        guard let sinceMs else { return "" }
        let seconds = max(0, (nowMs - sinceMs) / 1000)
        if seconds < 60 { return String(localized: "\(seconds) s") }
        if seconds < 3600 { return String(localized: "\(seconds / 60) min") }
        return String(localized: "\(seconds / 3600) h \(seconds % 3600 / 60) min")
    }

    static func queueSummary(_ queue: QueueSnapshot?) -> String {
        guard let queue else { return String(localized: "Queue: reading…") }
        let running = queue.running.count + queue.foreignRunning.count
        if running == 0, queue.waiting.isEmpty { return String(localized: "No heavy tasks") }
        return String(localized: "\(running) running · \(queue.waiting.count) waiting · capacity \(queue.capacity)")
    }

    static func workspaceName(_ entry: HeavyEntry) -> String {
        entry.workspaceRoot.map(WorkspaceChip.folderName(of:)) ?? WorkspaceChip.folderName(of: entry.cwd)
    }

    // MARK: - Devices in short (spec §9.2 item 6)

    static let deviceRows = 8

    /// The rows shown and the "N more" line (Task 13 F-4): registered devices only (a new or
    /// shut-down unregistered simulator is the window's business); first the ones a workspace
    /// holds, reserves or tests on (their holder chip is what the popover is for), then the
    /// ready ones, then the connected ones, then the rest; by name, without regard to case.
    static func devices(_ devices: [DeviceInfo],
                        standing: (DeviceInfo) -> [DeviceStanding]) -> (shown: [DeviceInfo], more: String?) {
        func rank(_ device: DeviceInfo) -> Int {
            if standing(device).contains(where: \.isClaim) { return 0 }
            if device.connected, device.ready { return 1 }
            if device.connected { return 2 }
            return 3
        }
        let sorted = devices.filter(\.registered)
            .map { (device: $0, rank: rank($0)) }
            .sorted { a, b in
                if a.rank != b.rank { return a.rank < b.rank }
                return deviceName(a.device).localizedCaseInsensitiveCompare(deviceName(b.device))
                    == .orderedAscending
            }
            .map(\.device)
        let shown = Array(sorted.prefix(deviceRows))
        let rest = sorted.count - shown.count
        return (shown, rest > 0 ? moreText(rest) : nil)
    }

    /// The workspaces holding or reserving a device, as chips.
    static func holders(_ standing: [DeviceStanding]) -> [WorkspaceChip] {
        var chips: [WorkspaceChip] = []
        for item in standing {
            switch item {
            case .held(let by, _): chips += by
            case .reserved(let by): chips.append(by)
            default: break
            }
        }
        return chips.reduce(into: []) { if !$0.contains($1) { $0.append($1) } }
    }

    static func moreText(_ count: Int) -> String {
        String(localized: "\(count) more")
    }

    static func deviceName(_ device: DeviceInfo) -> String {
        device.alias ?? device.name ?? device.model
    }

    static func battery(_ device: DeviceInfo) -> String? {
        if case .known(let percent)? = device.telemetry?.batteryPercent { return "\(percent) %" }
        return nil
    }

    // MARK: - The screen (spec §9.2 item 7)

    /// "Screen: free", or its holder and since when.
    static func screen(_ claims: ClaimsSnapshot?, formatTime: (Int64) -> String) -> String {
        guard let claims else { return String(localized: "Screen: reading…") }
        guard let state = claims.resources.first(where: { $0.resource.kind == .screen }),
              let claim = state.claims.first(where: { $0.kind != .reservation }) ?? state.claims.first
        else { return String(localized: "Screen: free") }
        let holder = WorkspaceChip.folderName(of: claim.workspace.path)
        return String(localized: "Screen: \(holder) since \(formatTime(claim.grantedAtMs))")
    }
}

extension DeviceStanding {
    /// A workspace or another tool holds, reserves or tests on the device.
    var isClaim: Bool {
        switch self {
        case .held, .reserved, .testing, .foreign: true
        case .free, .waiting, .notConnected, .notBooted: false
        }
    }
}
