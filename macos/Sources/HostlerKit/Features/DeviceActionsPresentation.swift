import Foundation

// What the window shows of a device (plan Task 10): the table's rows, columns and filters, the
// state and standing texts, the detail's telemetry and history rows. Pure values and pure
// functions, tested with swift-testing; the views only lay them out.

// MARK: - Names and times

extension DeviceInfo {
    /// The device's name as the interface and the CLI show it: the person's alias, else the
    /// catalog's name, else its model.
    public var displayName: String { alias ?? name ?? model }

    /// What `--confirmation` must equal for a takeover: the service's token, the alias
    /// (`ResourceInfo.display`), and the resource id for a device with no alias yet.
    public var confirmationToken: String { alias ?? resource.rawValue }

    /// The line under the name: "Android · Pixel 8, Android 15".
    public var summaryText: String {
        var text = "\(platform.title) · \(model)"
        if let os = DeviceInfo.osText(osVersion) { text += ", \(os)" }
        return text
    }

    /// A person's spelling of the OS: simulators report
    /// `com.apple.CoreSimulator.SimRuntime.iOS-26-5`, which reads "iOS 26.5".
    public static func osText(_ version: String?) -> String? {
        guard let version, !version.isEmpty else { return nil }
        guard let range = version.range(of: "SimRuntime.") else { return version }
        let rest = String(version[range.upperBound...])
        guard let dash = rest.firstIndex(of: "-") else {
            return rest.replacingOccurrences(of: "-", with: " ")
        }
        let name = String(rest[..<dash])
        let numbers = rest[rest.index(after: dash)...].replacingOccurrences(of: "-", with: ".")
        return "\(name) \(numbers)"
    }
}

extension Platform {
    /// "Android" / "iOS".
    public var title: String {
        switch self {
        case .android: "Android"
        case .ios: "iOS"
        }
    }
}

extension DeviceKind {
    /// "physical" / "emulator" / "simulator".
    public var title: String {
        switch self {
        case .physical: String(localized: "physical")
        case .emulator: String(localized: "emulator")
        case .simulator: String(localized: "simulator")
        }
    }
}

extension ClaimKind {
    /// "lease" / "hold" / "reservation".
    public var title: String {
        switch self {
        case .lease: String(localized: "lease")
        case .hold: String(localized: "hold")
        case .reservation: String(localized: "reservation")
        }
    }
}

extension ResourceId {
    /// The id's confirmation token, as Rust's `ResourceId::token()` (P5-L16): an account's name
    /// (`account:<project>/<name>`), else the id itself (`screen`, `device:…`).
    public var token: String {
        let prefix = "account:"
        guard rawValue.hasPrefix(prefix) else { return rawValue }
        let rest = rawValue.dropFirst(prefix.count)
        guard let slash = rest.firstIndex(of: "/") else { return rawValue }
        let name = rest[rest.index(after: slash)...]
        return name.isEmpty ? rawValue : String(name)
    }
}

/// What `--confirmation` must equal for a claim on `resource`, as the CLI computes it
/// (`crates/hostler/src/claims/engine.rs`'s `token_of`): a device's display (its alias), else the
/// resource id's token (`screen`, an account's name).
public func confirmationToken(for resource: ResourceInfo) -> String {
    switch resource.kind {
    case .device: resource.display
    case .screen, .account: resource.id.token
    }
}

/// Short spans as the window's captions say them ("just now", "3 min", "2 h", "3 d", "12 min
/// left"). Pure; `nowMs` is the caller's clock (tests pass a fixed one).
public enum Span {
    public static func since(_ startMs: Int64, nowMs: Int64) -> String {
        let delta = max(0, nowMs - startMs)
        let seconds = delta / 1000
        if seconds < 60 { return String(localized: "just now") }
        let minutes = seconds / 60
        if minutes < 60 { return String(localized: "\(minutes) min") }
        let hours = minutes / 60
        if hours < 24 { return String(localized: "\(hours) h") }
        return String(localized: "\(hours / 24) d")
    }

    public static func remaining(_ endMs: Int64, nowMs: Int64) -> String {
        let delta = endMs - nowMs
        if delta <= 0 { return String(localized: "ended") }
        let minutes = delta / 60_000
        if minutes < 1 { return String(localized: "less than a minute left") }
        if minutes < 60 { return String(localized: "\(minutes) min left") }
        let hours = minutes / 60
        if hours < 48 { return String(localized: "\(hours) h left") }
        return String(localized: "\(hours / 24) days left")
    }

    /// A finished task's duration: "42 s", "3 min 20 s", "1 h 4 min".
    public static func duration(_ ms: Int64) -> String {
        let seconds = max(0, ms) / 1000
        if seconds < 60 { return String(localized: "\(seconds) s") }
        let minutes = seconds / 60
        if minutes < 60 {
            let rest = seconds % 60
            return rest == 0 ? String(localized: "\(minutes) min")
                : String(localized: "\(minutes) min \(rest) s")
        }
        let hours = minutes / 60
        let rest = minutes % 60
        return rest == 0 ? String(localized: "\(hours) h")
            : String(localized: "\(hours) h \(rest) min")
    }
}

// MARK: - State

/// A device's connection state as the State column says it (spec §9.3): "connected · usb",
/// "connected · Wi-Fi", "booting", "shutting down", "not connected", "shut down".
public struct DeviceState: Equatable, Sendable {
    /// The phases in the column's sort order: reachable first.
    public enum Phase: Int, CaseIterable, Comparable, Sendable {
        case connected = 0
        case booting = 1
        case shuttingDown = 2
        case notConnected = 3
        case shutDown = 4

        public static func < (a: Phase, b: Phase) -> Bool { a.rawValue < b.rawValue }
    }

    public var phase: Phase
    public var transport: TransportKind?
    public var text: String

    /// The device can be used now (connected, or booting towards it).
    public var isReachable: Bool { phase == .connected || phase == .booting }

    /// The State column's sort key: connected, booting, shutting down, not connected, shut down.
    public var sortRank: Int { phase.rawValue }

    public static func of(_ device: DeviceInfo) -> DeviceState {
        if device.shuttingDown {
            return DeviceState(phase: .shuttingDown, transport: nil,
                               text: String(localized: "shutting down"))
        }
        // A virtual device is `connected` while it is merely known: not ready means it is off.
        if device.kind != .physical, !device.ready {
            return DeviceState(phase: .shutDown, transport: nil,
                               text: String(localized: "shut down"))
        }
        if device.connected, !device.ready {
            // A phone the service sees but cannot use yet.
            return DeviceState(phase: .booting, transport: bestTransport(of: device),
                               text: String(localized: "booting"))
        }
        if device.connected {
            let transport = bestTransport(of: device)
            let text = transport.map { String(localized: "connected · \(transportTitle($0))") }
                ?? String(localized: "connected")
            return DeviceState(phase: .connected, transport: transport, text: text)
        }
        if device.kind == .physical {
            return DeviceState(phase: .notConnected, transport: nil,
                               text: String(localized: "not connected"))
        }
        return DeviceState(phase: .shutDown, transport: nil,
                           text: String(localized: "shut down"))
    }

    /// The transport a person reads first: a cable before Wi-Fi, the host's shim last.
    static let transportOrder: [TransportKind] = [.usb, .wired, .wifi, .network, .emulator,
                                                  .simulator]

    static func bestTransport(of device: DeviceInfo) -> TransportKind? {
        let present = Set(device.transports.map(\.kind))
        return transportOrder.first { present.contains($0) }
    }

    public static func transportTitle(_ kind: TransportKind) -> String {
        switch kind {
        case .usb: String(localized: "usb")
        case .wired: String(localized: "wired")
        case .wifi: String(localized: "Wi-Fi")
        case .network: String(localized: "network")
        case .emulator: String(localized: "emulator")
        case .simulator: String(localized: "simulator")
        }
    }
}

// MARK: - Standing chips

/// One chip of the Standing column (DeviceDeck's holder chip): a colored workspace name, a
/// source ("agent-device (session)"), "free" or "N waiting". The color is the workspace's, the
/// same as its sidebar dot.
public struct StandingChip: Identifiable, Equatable, Sendable {
    /// The small symbol before the text, when the chip is about a session or a wait.
    public enum Symbol: String, CaseIterable, Sendable {
        case handTap
        case pin
        case clock
    }

    public var id: String
    public var text: String
    public var color: WorkspaceColor?
    public var symbol: Symbol?
    /// A chip that is not a workspace's: shown in the secondary style.
    public var isSource: Bool

    /// A workspace's chip: its folder name in its color.
    public init(workspace: WorkspaceChip) {
        self.init(text: workspace.name, color: workspace.color)
    }

    public init(text: String, color: WorkspaceColor? = nil, symbol: Symbol? = nil,
                isSource: Bool = false) {
        self.text = text
        self.color = color
        self.symbol = symbol
        self.isSource = isSource
        self.id = "\(color?.rawValue ?? -1)|\(symbol?.rawValue ?? "")|\(text)"
    }
}

/// The chips of a device's standing for the table and the workspace's "Other devices"
/// (`standing(_:claims:workspaces:)`'s answer). The device's own connection state is the State
/// column's, so it adds no chip here.
public func standingChips(_ standing: [DeviceStanding]) -> [StandingChip] {
    var chips: [StandingChip] = []
    for standing in standing {
        switch standing {
        case .free:
            chips.append(StandingChip(text: String(localized: "free")))
        case .reserved(let by):
            chips.append(StandingChip(text: String(localized: "\(by.name) reserved"),
                                      color: by.color, symbol: .pin))
        case .held(let workspaces, let kind):
            for workspace in workspaces {
                chips.append(StandingChip(text: runChip(workspace: workspace.name, kind: kind),
                                          color: workspace.color))
            }
        case .testing(let session, let by):
            chips.append(StandingChip(text: String(localized: "agent-device (\(session))"),
                                      symbol: .handTap, isSource: true))
            chips.append(StandingChip(text: by.name, color: by.color))
        case .foreign(let source, let by):
            chips.append(StandingChip(text: source, isSource: true))
            if let by { chips.append(StandingChip(text: by.name, color: by.color)) }
        case .waiting(let count):
            chips.append(StandingChip(
                text: count == 1 ? String(localized: "1 waiting")
                    : String(localized: "\(count) waiting"),
                symbol: .clock, isSource: true))
        case .notConnected, .notBooted:
            break
        }
    }
    return chips
}

/// What a live claim means for the chip: a lease is a run using the device, a hold is a hold.
private func runChip(workspace: String, kind: ClaimKind) -> String {
    switch kind {
    case .lease: String(localized: "\(workspace) running")
    case .hold: String(localized: "\(workspace) hold")
    case .reservation: String(localized: "\(workspace) reserved")
    }
}

// MARK: - Filters

/// The toolbar's platform filter: All / iOS / Android.
public enum PlatformFilter: String, CaseIterable, Sendable {
    case all
    case ios
    case android

    public var title: String {
        switch self {
        case .all: String(localized: "All")
        case .ios: String(localized: "iOS")
        case .android: String(localized: "Android")
        }
    }

    public func includes(_ platform: Platform) -> Bool {
        switch self {
        case .all: true
        case .ios: platform == .ios
        case .android: platform == .android
        }
    }
}

/// The toolbar's kind filter: All / Physical / Virtual (emulators and simulators).
public enum KindFilter: String, CaseIterable, Sendable {
    case all
    case physical
    case virtual

    public var title: String {
        switch self {
        case .all: String(localized: "All")
        case .physical: String(localized: "Physical")
        case .virtual: String(localized: "Virtual")
        }
    }

    public func includes(_ kind: DeviceKind) -> Bool {
        switch self {
        case .all: true
        case .physical: kind == .physical
        case .virtual: kind != .physical
        }
    }
}

/// What All Devices shows: the toolbar's filters and "show unregistered" (spec §9.3).
///
/// The default matches `hostler devices list` (fix F-3): the registered devices **and** the
/// unregistered physical ones the CLI calls new. "Show unregistered" is `devices list --all`: it
/// adds every unregistered emulator and simulator too.
public struct DeviceFilter: Equatable, Sendable {
    public var platform: PlatformFilter
    public var kind: KindFilter
    /// Also every unregistered virtual device (`devices list --all`).
    public var showsUnregistered: Bool

    public init(platform: PlatformFilter = .all, kind: KindFilter = .all,
                showsUnregistered: Bool = false) {
        self.platform = platform
        self.kind = kind
        self.showsUnregistered = showsUnregistered
    }

    public func includes(_ device: DeviceInfo) -> Bool {
        guard platform.includes(device.platform), kind.includes(device.kind) else { return false }
        // `devices list` keeps a device that is registered or physical (service.rs `list`).
        return showsUnregistered || device.registered || device.kind == .physical
    }

    public func apply(_ devices: [DeviceInfo]) -> [DeviceInfo] {
        devices.filter(includes)
    }
}

// MARK: - Rows

/// One row of All Devices.
public struct DeviceRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var device: DeviceInfo
    /// The alias, the catalog's name or the model (the Device column's first line).
    public var name: String
    /// "Android · Pixel 8, Android 15" (the Device column's second line).
    public var subtitle: String
    /// "Android · physical" (the Type column).
    public var typeText: String
    public var state: DeviceState
    public var chips: [StandingChip]
    public var batteryPercent: Int?
    /// "82%", "unknown", "emulated", or `nil` without telemetry.
    public var batteryText: String?
    public var isCharging: Bool
    public var isUnregistered: Bool

    /// The Standing column's sort key: the chips' texts.
    public var standingText: String { chips.map(\.text).joined(separator: ", ") }

    // The table's sort keys: the values its headers compare, so the pure order and a SwiftUI
    // `Table`'s own sort order agree (a test checks it).

    public var nameSortKey: String { name.localizedLowercase }
    public var typeSortKey: String { typeText.localizedLowercase }
    public var stateSortRank: Int { state.sortRank }
    public var standingSortKey: String { standingText.localizedLowercase }
    /// Unknown after every reading.
    public var batterySortKey: Int { batteryPercent ?? 101 }
}

/// The rows of All Devices, in the devices snapshot's order (sort with `DeviceSort`).
///
/// - Parameter standingOf: the device's standing chips' source (`HostlerStore.standing(of:)`).
public func deviceRows(_ devices: [DeviceInfo], filter: DeviceFilter = DeviceFilter(),
                       standingOf: (DeviceInfo) -> [DeviceStanding]) -> [DeviceRow] {
    filter.apply(devices).map { device in
        let telemetry = device.telemetry
        return DeviceRow(
            id: device.resource.rawValue, device: device, name: device.displayName,
            subtitle: device.summaryText,
            typeText: "\(device.platform.title) · \(device.kind.title)",
            state: DeviceState.of(device),
            chips: standingChips(standingOf(device)),
            batteryPercent: batteryPercent(telemetry), batteryText: batteryText(telemetry),
            isCharging: telemetry?.charging == .known(true),
            isUnregistered: !device.registered)
    }
}

/// The battery's percentage when the device reported one (`ReadingUInt8`).
public func batteryPercent(_ telemetry: Telemetry?) -> Int? {
    guard case .known(let value)? = telemetry?.batteryPercent else { return nil }
    return Int(value)
}

/// "82%", "unknown", "emulated"; `nil` without telemetry.
public func batteryText(_ telemetry: Telemetry?) -> String? {
    guard let telemetry else { return nil }
    switch telemetry.batteryPercent {
    case .known(let value): return "\(value)%"
    case .unknown: return String(localized: "unknown")
    case .emulated: return String(localized: "emulated")
    }
}

/// The connected devices hostler has not registered: the "new device connected — Add…" bar.
public func newDevices(_ devices: [DeviceInfo]) -> [DeviceInfo] {
    // The devices `devices list` shows that hostler has not registered: a connected physical
    // one (fix F-3). A booted, unregistered simulator is `--all` territory, not a new device.
    devices.filter { $0.connected && !$0.registered && $0.kind == .physical }
}

/// "N reachable · M claimed", the window's subtitle on the device views (spec §9.3):
/// reachable = ready to use (a shut-down simulator is not); claimed = a device with a hostler
/// claim or a foreign claim.
public struct DeviceCounts: Equatable, Sendable {
    public var reachable: Int
    public var claimed: Int

    public var text: String { String(localized: "\(reachable) reachable · \(claimed) claimed") }
}

public func deviceCounts(_ devices: [DeviceInfo], claims: ClaimsSnapshot?) -> DeviceCounts {
    let claimed = Set((claims?.resources ?? []).filter { state in
        !state.claims.isEmpty || !state.foreign.isEmpty
    }.map(\.resource.id))
    return DeviceCounts(
        reachable: devices.filter(\.ready).count,
        claimed: devices.filter { claimed.contains($0.resource) }.count)
}

// MARK: - The note

/// The note field's text (fix F-7): what the person has typed and not saved stays; otherwise the
/// field follows the service's note, where `nil` is "no note".
public enum NoteField {
    public static func text(serviceNote: String?, keepsEdit: Bool, field: String) -> String {
        keepsEdit ? field : (serviceNote ?? "")
    }
}

// MARK: - Sorting

/// A sortable column of All Devices.
public enum DeviceColumn: String, CaseIterable, Sendable {
    case device
    case type
    case state
    case standing
    case battery

    public var title: String {
        switch self {
        case .device: String(localized: "Device")
        case .type: String(localized: "Type")
        case .state: String(localized: "State")
        case .standing: String(localized: "Standing")
        case .battery: String(localized: "Battery")
        }
    }
}

/// The table's sort order (the column headers' rule). The compared values are exactly the
/// texts the column shows, and a tie keeps a stable name order.
public struct DeviceSort: Equatable, Sendable {
    public var column: DeviceColumn
    public var ascending: Bool

    public init(column: DeviceColumn = .device, ascending: Bool = true) {
        self.column = column
        self.ascending = ascending
    }

    public func sorted(_ rows: [DeviceRow]) -> [DeviceRow] {
        rows.sorted { a, b in
            let left = key(a)
            let right = key(b)
            if left != right { return ascending ? left < right : right < left }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    /// The column's value for a row: the table's own key, then the name (a tie stays stable).
    func key(_ row: DeviceRow) -> String {
        switch column {
        case .device: row.nameSortKey
        case .type: row.typeSortKey
        case .state: String(format: "%03d %@", row.stateSortRank, row.nameSortKey)
        case .standing: row.standingSortKey
        case .battery: row.batteryPercent.map { String(format: "%03d", $0) }
                ?? "999 \(row.nameSortKey)"
        }
    }
}

// MARK: - Telemetry

/// The detail's Telemetry section: every reading in words, "unknown" when it is (spec §9.3).
public struct TelemetryFacts: Equatable, Sendable {
    public var battery: String
    public var charging: String
    public var wifi: String
    public var wifiName: String?
    public var cellular: String
    public var vpn: String
    public var takenText: String?

    public static func of(_ telemetry: Telemetry?, nowMs: Int64) -> TelemetryFacts {
        guard let telemetry else {
            let unknown = String(localized: "unknown")
            return TelemetryFacts(battery: unknown, charging: unknown, wifi: unknown,
                                  wifiName: nil, cellular: unknown, vpn: unknown,
                                  takenText: nil)
        }
        return TelemetryFacts(
            battery: batteryText(telemetry) ?? String(localized: "unknown"),
            charging: readingText(telemetry.charging), wifi: readingText(telemetry.wifi),
            wifiName: telemetry.wifiName, cellular: readingText(telemetry.cellularData),
            vpn: readingText(telemetry.vpn), takenText: Span.since(telemetry.atMs, nowMs: nowMs))
    }

    /// "yes" / "no" / "unknown" / "emulated".
    static func readingText(_ reading: ReadingBool) -> String {
        switch reading {
        case .known(true): String(localized: "yes")
        case .known(false): String(localized: "no")
        case .unknown: String(localized: "unknown")
        case .emulated: String(localized: "emulated")
        }
    }
}

// MARK: - Claims

/// One claim as the detail's Claims section and the workspace's "Held by this workspace" show
/// it: the workspace chip, the kind, the command, since when and the TTL left.
public struct ClaimRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var claim: Claim
    public var workspace: WorkspaceChip
    /// The claimed resource: its stable id (`device:…`, `screen`, `account:<project>/<name>`)
    /// and its kind, so a screen or an account is not treated as a device (fix R1-3).
    public var resource: ResourceInfo
    /// The resource's display (`samsung`, `screen`, `account qa-user`).
    public var resourceName: String
    public var kindText: String
    public var command: String?
    public var sinceText: String
    /// "12 min left", "while its process lives"; `nil` for a reservation.
    public var ttlText: String?
    public var expiresAtMs: Int64?
    /// What `--confirmation` must equal to end this claim: the device's alias, `screen`, or an
    /// account's name.
    public var confirmationToken: String

    /// Whether the claim can be renewed (`hold renew`): a TTL hold.
    public var canRenew: Bool { if case .ttl = claim.binding { true } else { false } }

    public var resourceKind: ResourceKind { resource.kind }
}

public func claimRows(_ claims: [Claim], nowMs: Int64) -> [ClaimRow] {
    claims.map { claim in
        let expiresAtMs: Int64?
        let ttlText: String?
        switch claim.binding {
        case .ttl(_, let expires):
            expiresAtMs = expires
            ttlText = Span.remaining(expires, nowMs: nowMs)
        case .process:
            expiresAtMs = nil
            ttlText = String(localized: "while its process lives")
        case nil:
            expiresAtMs = nil
            ttlText = nil
        }
        return ClaimRow(
            id: claim.id, claim: claim,
            workspace: WorkspaceChip(workspaceId: claim.workspace.id, path: claim.workspace.path),
            resource: claim.resource, resourceName: claim.resource.display,
            kindText: claim.kind.title, command: claim.command,
            sinceText: Span.since(claim.grantedAtMs, nowMs: nowMs), ttlText: ttlText,
            expiresAtMs: expiresAtMs,
            confirmationToken: confirmationToken(for: claim.resource))
    }.sorted { a, b in
        if a.kindText != b.kindText { return a.kindText < b.kindText }
        return a.resourceName.localizedStandardCompare(b.resourceName) == .orderedAscending
    }
}

/// A resource's claims from the snapshot (`nil` when it has none).
public func claims(of resource: ResourceId, in snapshot: ClaimsSnapshot?) -> [Claim] {
    snapshot?.resources.first { $0.resource.id == resource }?.claims ?? []
}

/// A resource's waiting requests from the snapshot.
public func waitingRequests(of resource: ResourceId,
                            in snapshot: ClaimsSnapshot?) -> [WaitingRequest] {
    snapshot?.resources.first { $0.resource.id == resource }?.waiting ?? []
}

// MARK: - Installs

/// One installed-apps row of the device detail: the app, its version, when the change was
/// recorded, and what it came from (§7.5: workspace, branch, commit, command, or "not through
/// hostler").
public struct InstallRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var appId: String
    public var versionText: String?
    public var changeText: String
    public var timeText: String
    public var sourceText: String
    public var workspace: WorkspaceChip?

    /// Whether the change came from a command (rather than from outside hostler).
    public var isAttributed: Bool { workspace != nil }
}

public func installRows(of device: DeviceInfo, installs: [InstallEvent], nowMs: Int64,
                        workspaces: [WorkspaceInfo] = []) -> [InstallRow] {
    installs.filter { $0.device == device.resource }.map { event in
        InstallRow(
            id: event.id, appId: event.appId, versionText: versionText(event),
            changeText: event.change.rawValue, timeText: installTimeText(event.when, nowMs: nowMs),
            sourceText: installSourceText(event.source),
            workspace: installWorkspace(event.source, workspaces: workspaces))
    }
}

func versionText(_ event: InstallEvent) -> String? {
    switch (event.versionName, event.versionCode) {
    case (let name?, let code?): "\(name) (\(code))"
    case (let name?, nil): name
    case (nil, let code?): code
    case (nil, nil): nil
    }
}

func installTimeText(_ time: InstallTime, nowMs: Int64) -> String {
    switch time {
    case .device(let atMs): Span.since(atMs, nowMs: nowMs)
    case .firstSeen(let atMs): String(localized: "first seen \(Span.since(atMs, nowMs: nowMs))")
    case .changedBetween(_, let toMs): Span.since(toMs, nowMs: nowMs)
    }
}

/// "app @ dev/4.13 · 1a2b3c4 · make device", "app (while it ran)", "not through hostler".
func installSourceText(_ source: InstallSource) -> String {
    switch source {
    case .claim(_, let path, let branch, let commit, let command):
        var parts = [WorkspaceChip.folderName(of: path)]
        if let branch { parts.append(branch) }
        if let commit, !commit.isEmpty { parts.append(String(commit.prefix(7))) }
        if let command, !command.isEmpty { parts.append(command) }
        return parts.joined(separator: " · ")
    case .around(_, let path):
        return String(localized: "\(WorkspaceChip.folderName(of: path)) (while it ran)")
    case .notThroughHostler:
        return String(localized: "not through hostler")
    }
}

func installWorkspace(_ source: InstallSource, workspaces: [WorkspaceInfo]) -> WorkspaceChip? {
    switch source {
    case .claim(_, let path, _, _, _), .around(_, let path):
        return WorkspaceChip.of(path: path, workspaceId: nil, workspaces: workspaces)
    case .notThroughHostler:
        return nil
    }
}

/// A device's claim history by its **stable** id (fix R1-5): a rename never moves its rows away,
/// an alias that collides with `screen` or an account never attaches another resource's rows,
/// and a row recorded before P5-L16 (`resourceId == nil`) is not guessed at.
public func claimHistory(of resource: ResourceId,
                         in history: [ClaimHistoryItem]) -> [ClaimHistoryItem] {
    history.filter { $0.resourceId == resource }
}

// MARK: - Reading state

/// What a window section shows before its snapshot arrives (fix R1-6): "no devices" must mean a
/// loaded, empty list, never a first read, a reconnect or an offline start.
public enum SnapshotState: Equatable, Sendable {
    case loading
    case catchingUp
    case offline
    case failed
    case loaded

    /// - Parameters:
    ///   - hasSnapshot: whether the store has the snapshot the section reads.
    ///   - syncState: the store's connection/read state.
    public init(hasSnapshot: Bool, syncState: HostlerStore.SyncState) {
        if hasSnapshot {
            self = .loaded
            return
        }
        switch syncState {
        case .live: self = .loading
        case .syncing(let catchingUp): self = catchingUp ? .catchingUp : .loading
        case .offline: self = .offline
        case .failed: self = .failed
        }
    }

    /// Whether a loaded snapshot is on screen (an empty one included).
    public var isLoaded: Bool { self == .loaded }

    /// The line over the detail ("Reading…").
    public var title: String {
        switch self {
        case .loading: String(localized: "Reading…")
        case .catchingUp: String(localized: "Catching up…")
        case .offline: String(localized: "Offline")
        case .failed: String(localized: "Not reading")
        case .loaded: ""
        }
    }

    /// The line under it.
    public var detail: String {
        switch self {
        case .loading: String(localized: "hostler is reading its first snapshot.")
        case .catchingUp:
            String(localized: "The connection was busy; hostler re-reads every snapshot.")
        case .offline:
            String(localized: "hostler's service is not connected, and nothing has been read yet.")
        case .failed: String(localized: "The last read failed; hostler retries by itself.")
        case .loaded: ""
        }
    }
}
