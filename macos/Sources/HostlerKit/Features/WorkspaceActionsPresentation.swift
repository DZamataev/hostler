import Foundation

// The sidebar's project sections and the workspace detail's model (plan Task 10). Pure values
// over Task 8's snapshots; the views lay them out.

// MARK: - The sidebar

/// How long after its last use a workspace stays in the sidebar (plan Task 10: 30 days), unless
/// it holds or waits for something.
public let sidebarRecentWindowMs: Int64 = 30 * 24 * 3600 * 1000

/// A workspace in its project's sidebar section.
public struct SidebarWorkspace: Identifiable, Equatable, Sendable {
    public var row: WorkspaceRow
    public var lastSeenMs: Int64
    /// A request of it waits for a resource (so it is at work even when it holds nothing).
    public var isWaiting: Bool

    public var id: String { row.id }
}

/// A project's section of the sidebar: the workspaces seen in the last 30 days or holding
/// something, and the rest behind the section's menu ("Show older workspaces").
public struct SidebarProjectGroup: Identifiable, Equatable, Sendable {
    public var id: String
    public var name: String
    public var visible: [SidebarWorkspace]
    public var hidden: [SidebarWorkspace]
    /// The section's menu switched the older workspaces on.
    public var showsHidden: Bool

    public init(id: String, name: String, visible: [SidebarWorkspace],
                hidden: [SidebarWorkspace], showsHidden: Bool) {
        self.id = id
        self.name = name
        self.visible = visible
        self.hidden = hidden
        self.showsHidden = showsHidden
    }

    /// Every row to draw: the visible ones, then the older ones when they are shown.
    public var rows: [SidebarWorkspace] { showsHidden ? visible + hidden : visible }
}

/// The sidebar's sections, in the projects snapshot's order and each project's workspace order
/// (pure). A workspace is cut when it was last seen more than 30 days ago and holds nothing and
/// waits for nothing; `showsHidden` names the projects whose menu was switched on.
public func sidebarGroups(projects: [ProjectView], claims: ClaimsSnapshot?, queue: QueueSnapshot?,
                          branches: [String: String], nowMs: Int64,
                          showsHidden: Set<String> = []) -> [SidebarProjectGroup] {
    let waitingPaths = (claims?.resources ?? []).flatMap(\.waiting).map(\.workspacePath)
    return projects.map { project in
        let rows = workspaceRows(project: project, claims: claims, queue: queue,
                                 branches: branches)
        let lastSeen = Dictionary(uniqueKeysWithValues: project.workspaces.map {
            ($0.id, $0.lastSeenMs)
        })
        let entries = rows.map { row -> SidebarWorkspace in
            SidebarWorkspace(
                row: row, lastSeenMs: lastSeen[row.id] ?? 0,
                isWaiting: waitingPaths.contains { samePath($0, row.path) })
        }
        let (recent, older) = entries.reduce(into: ([SidebarWorkspace](), [SidebarWorkspace]())) {
            let atWork = $1.row.heldCount > 0 || $1.row.heavyRunning || $1.isWaiting
            let fresh = nowMs - $1.lastSeenMs <= sidebarRecentWindowMs
            if atWork || fresh { $0.0.append($1) } else { $0.1.append($1) }
        }
        return SidebarProjectGroup(id: project.info.id, name: project.info.name, visible: recent,
                                   hidden: older,
                                   showsHidden: showsHidden.contains(project.info.id))
    }
}

extension HeavyState {
    /// "waiting" / "running" / "stopping".
    public var title: String {
        switch self {
        case .waiting: String(localized: "waiting")
        case .running: String(localized: "running")
        case .stopping: String(localized: "stopping")
        }
    }
}

/// The sidebar footer's line under the state (fix R1-8): "live from the service" only while the
/// store is live; after a drop or a failed read it says when the last event came and what is
/// happening.
public func footerDetailText(lastEventAt: Date?, now: Date,
                             syncState: HostlerStore.SyncState) -> String {
    let nowMs = Int64((now.timeIntervalSince1970 * 1000).rounded())
    let event: String? = lastEventAt.map { last in
        let lastMs = Int64((last.timeIntervalSince1970 * 1000).rounded())
        return Span.since(lastMs, nowMs: nowMs)
    }
    switch syncState {
    case .live:
        guard let event else {
            return String(localized: "No event yet · live from the service")
        }
        return String(localized: "Last event \(event) · live from the service")
    case .syncing(let catchingUp):
        switch (event, catchingUp) {
        case (nil, true):
            return String(localized: "No event yet · catching up: re-reading every snapshot")
        case (nil, false):
            return String(localized: "No event yet · reading every snapshot")
        case (let event?, true):
            return String(localized:
                "Last event \(event) · catching up: re-reading every snapshot")
        case (let event?, false):
            return String(localized: "Last event \(event) · reading every snapshot")
        }
    case .failed:
        guard let event else {
            return String(localized: "No event yet · not reading; hostler retries")
        }
        return String(localized: "Last event \(event) · not reading; hostler retries")
    case .offline:
        guard let event else {
            return String(localized: "hostler is not connected; nothing has been read")
        }
        return String(localized: "Last event \(event) · offline, showing the last read")
    }
}

// MARK: - The workspace detail

/// A waiting request in the workspace's section: the resource, the kind, since when.
public struct WorkspaceWaitingRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var request: WaitingRequest
    public var resourceName: String
    public var kindText: String
    public var sinceText: String
}

/// A heavy task of the workspace: running or waiting, with its position among this workspace's
/// waiting tasks (fix R1-4: running tasks never count into a waiting position).
public struct WorkspaceHeavyRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var entry: HeavyEntry
    public var stateText: String
    public var command: String
    public var elapsedText: String
    /// 1, 2, … among this workspace's waiting entries in the queue's order; `nil` otherwise.
    public var position: Int?

    public var isWaiting: Bool { entry.state == .waiting }
}

/// The workspace's project's secrets, in short (the section links to Secrets for the rest).
public struct WorkspaceSecrets: Equatable, Sendable {
    public enum State: Equatable, Sendable {
        /// The store has not read this project's secrets yet.
        case notRead
        /// The read failed (the store's `projectSecretErrors`): its message.
        case failed(String?)
        case read
    }

    public var state: State
    public var total: Int
    public var missing: Int

    public var stateText: String {
        switch state {
        case .notRead: String(localized: "Not read yet")
        case .failed(let message): message ?? String(localized: "Could not be read")
        case .read:
            switch (missing, total) {
            case (0, 0): String(localized: "None declared")
            case (0, _): String(localized: "All \(total) set")
            case (_, 1): String(localized: "1 missing of \(total)")
            default: String(localized: "\(missing) missing of \(total)")
            }
        }
    }
}

/// What a workspace's detail shows (spec §9.3): its header, the claims it holds, what waits, its
/// heavy tasks, the devices it does not hold, and its project's secrets in short.
public struct WorkspaceDetailModel: Equatable, Sendable {
    public var workspace: WorkspaceInfo
    public var projectId: String
    public var projectName: String
    public var folderName: String
    public var branch: String?
    public var color: WorkspaceColor
    public var held: [ClaimRow]
    public var waiting: [WorkspaceWaitingRow]
    public var heavy: [WorkspaceHeavyRow]
    public var otherDevices: [DeviceRow]
    public var secrets: WorkspaceSecrets
}

public func workspaceDetail(of workspace: WorkspaceInfo, projectName: String,
                            devices: [DeviceInfo], claims: ClaimsSnapshot?,
                            queue: QueueSnapshot?, branches: [String: String],
                            secrets: SecretList?, secretsRead: Bool, nowMs: Int64,
                            secretsError: String? = nil,
                            standingOf: (DeviceInfo) -> [DeviceStanding] = { _ in [] })
    -> WorkspaceDetailModel
{
    let own = (claims?.resources ?? []).flatMap(\.claims).filter { claim in
        claim.workspace.id == workspace.id || samePath(claim.workspace.path, workspace.path)
    }
    var waiting: [WorkspaceWaitingRow] = []
    for state in claims?.resources ?? [] {
        for request in state.waiting where samePath(request.workspacePath, workspace.path) {
            waiting.append(WorkspaceWaitingRow(
                id: request.requestId, request: request, resourceName: state.resource.display,
                kindText: request.kind.title,
                sinceText: Span.since(request.sinceMs, nowMs: nowMs)))
        }
    }
    waiting.sort { a, b in
        a.request.sinceMs == b.request.sinceMs
            ? a.resourceName < b.resourceName : a.request.sinceMs < b.request.sinceMs
    }

    let isMine = { (entry: HeavyEntry) in
        samePath(entry.workspaceRoot ?? entry.cwd, workspace.path)
    }
    var heavy: [WorkspaceHeavyRow] = []
    for entry in (queue?.running ?? []) where isMine(entry) {
        let startedMs = entry.grantedAtMs ?? entry.enqueuedAtMs
        heavy.append(WorkspaceHeavyRow(
            id: entry.id, entry: entry, stateText: entry.state.title, command: entry.command,
            elapsedText: Span.duration(nowMs - startedMs), position: nil))
    }
    // A waiting position counts waiting tickets only (P5-L16): two running tasks do not make
    // this workspace's first waiting task "third".
    for (index, entry) in (queue?.waiting ?? []).filter(isMine).enumerated() {
        heavy.append(WorkspaceHeavyRow(
            id: entry.id, entry: entry, stateText: entry.state.title, command: entry.command,
            elapsedText: Span.duration(nowMs - entry.enqueuedAtMs),
            position: index + 1))
    }

    let heldIds = Set(own.map(\.resource.id))
    let otherDevices = deviceRows(
        devices.filter { $0.registered && !heldIds.contains($0.resource) },
        standingOf: standingOf
    ).sorted { a, b in
        if a.state.sortRank != b.state.sortRank { return a.state.sortRank < b.state.sortRank }
        return a.name.localizedStandardCompare(b.name) == .orderedAscending
    }

    return WorkspaceDetailModel(
        workspace: workspace, projectId: workspace.projectId, projectName: projectName,
        folderName: WorkspaceChip.folderName(of: workspace.path),
        branch: branches[workspace.id], color: WorkspaceColor(workspaceId: workspace.id),
        held: claimRows(own, nowMs: nowMs), waiting: waiting, heavy: heavy,
        otherDevices: otherDevices,
        secrets: workspaceSecrets(secrets, read: secretsRead, error: secretsError))
}

func workspaceSecrets(_ secrets: SecretList?, read: Bool, error: String? = nil) -> WorkspaceSecrets {
    guard read else { return WorkspaceSecrets(state: .notRead, total: 0, missing: 0) }
    guard let secrets else { return WorkspaceSecrets(state: .failed(error), total: 0, missing: 0) }
    return WorkspaceSecrets(state: .read, total: secrets.secrets.count,
                            missing: secrets.secrets.filter { $0.state == .missing }.count)
}
