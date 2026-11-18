import Foundation
@testable import HostlerKit

/// Small builders for the contract values the store tests use (Task 8).
enum Mk {
    static let pid = ProcessIdentity(pid: 42, bootId: "boot-1", startTime: 1)
    static let now: Int64 = 1_800_000_000_000

    static func queue(seq: UInt64, capacity: UInt8 = 2, running: [HeavyEntry] = [],
                      waiting: [HeavyEntry] = []) -> QueueSnapshot {
        QueueSnapshot(capacity: capacity, running: running, waiting: waiting, foreignRunning: [],
                      asOfSeq: seq)
    }

    static func entry(_ id: String, kind: String = "build", command: String = "make",
                      workspaceRoot: String? = "/work/app", state: HeavyState = .running,
                      grantedAtMs: Int64? = now, ticket: UInt64 = 1) -> HeavyEntry {
        HeavyEntry(id: id, ticket: ticket, kind: kind, command: command, cwd: workspaceRoot ?? "/tmp",
                   workspaceRoot: workspaceRoot, holder: pid, state: state, enqueuedAtMs: now - 1000,
                   grantedAtMs: grantedAtMs, source: .hostler)
    }

    static func finished(_ id: String, kind: String = "build", ranMs: Int64, finishedAt: Int64,
                         granted: Bool = true) -> HeavyHistoryItem {
        HeavyHistoryItem(id: id, ticket: 1, kind: kind, command: "make", cwd: "/work/app",
                         workspaceRoot: "/work/app", enqueuedAtMs: finishedAt - ranMs - 10,
                         grantedAtMs: granted ? finishedAt - ranMs : nil, finishedAtMs: finishedAt,
                         exitCode: 0, outcome: granted ? .ok : .withdrawn)
    }

    static func claims(seq: UInt64, _ resources: [ResourceState] = []) -> ClaimsSnapshot {
        ClaimsSnapshot(resources: resources, asOfSeq: seq)
    }

    static func resource(_ id: String, kind: ResourceKind = .device) -> ResourceInfo {
        ResourceInfo(id: ResourceId(rawValue: id), kind: kind, display: id, attributes: [:],
                     available: true)
    }

    static func claim(_ id: String, on resource: String, kind: ClaimKind, workspace: String,
                      path: String) -> Claim {
        Claim(id: id, resource: self.resource(resource), kind: kind,
              workspace: WorkspaceRef(id: workspace, path: path), grantedAtMs: now)
    }

    static func state(_ resource: String, claims: [Claim] = [], foreign: [ForeignClaim] = [],
                      waiting: Int = 0) -> ResourceState {
        ResourceState(
            resource: self.resource(resource), claims: claims, foreign: foreign,
            waiting: (0..<waiting).map {
                WaitingRequest(requestId: "w\($0)", ticket: UInt64($0), workspacePath: "/work/w\($0)",
                               kind: .lease, sinceMs: now)
            },
            seizing: false)
    }

    static func secrets(seq: UInt64, state: LockState = .unlocked, pending: UInt32 = 0) -> SecretsStatus {
        SecretsStatus(state: state, database: "/db/hostler.kdbx", pendingRequests: pending,
                      preservedFiles: [], asOfSeq: seq)
    }

    static let tools = ToolStatus(adbState: .ours, xcrun: true, agentDeviceInUse: false,
                                  sessionsKnown: true, problems: [])

    static func devices(seq: UInt64, _ devices: [DeviceInfo] = []) -> DevicesSnapshot {
        DevicesSnapshot(devices: devices, unidentified: [], unmappedSessions: [], tools: tools,
                        atMs: now, asOfSeq: seq)
    }

    static func device(_ id: String, kind: DeviceKind = .physical, registered: Bool = true,
                       connected: Bool = true, sessions: [AgentDeviceSession] = []) -> DeviceInfo {
        DeviceInfo(resource: ResourceId(rawValue: id), hardwareId: "HW-\(id)", platform: .android,
                   kind: kind, registered: registered, aliasByPerson: false, model: "Pixel \(id)",
                   connected: connected, ready: connected, transports: [], bootedByHostler: false,
                   shuttingDown: false, removing: false, sessions: sessions, checkingSessions: false,
                   firstSeenMs: now, lastSeenMs: now)
    }

    static func session(_ name: String, path: String, workspaceId: String? = nil) -> AgentDeviceSession {
        AgentDeviceSession(session: name, workspacePath: path, workspaceId: workspaceId,
                           seenSinceMs: now, kept: false, platform: .android, agentDeviceId: "ad-1")
    }

    static func projects(seq: UInt64, _ projects: [ProjectView] = []) -> ProjectsSnapshot {
        ProjectsSnapshot(projects: projects, atMs: now, asOfSeq: seq)
    }

    static func project(_ id: String, workspaces: [WorkspaceInfo] = []) -> ProjectView {
        ProjectView(info: ProjectInfo(id: id, name: id, commonDir: "/work/\(id)/.git", approved: true,
                                      createdAtMs: now),
                    workspaces: workspaces)
    }

    static func workspace(_ id: String, project: String, path: String) -> WorkspaceInfo {
        WorkspaceInfo(id: id, projectId: project, path: path, outcome: .known, projectRepointed: false,
                      lastSeenMs: now)
    }

    static func requests(seq: UInt64, secrets: [SecretRequestInfo] = [],
                         accounts: [AccountRequestInfo] = []) -> RequestsSnapshot {
        RequestsSnapshot(secrets: secrets, accounts: accounts, asOfSeq: seq)
    }

    static func secretRequest(_ id: String, state: RequestState = .pending, created: Int64 = now,
                              project: String = "p1") -> SecretRequestInfo {
        SecretRequestInfo(id: id, projectId: project, projectName: "app", name: "API_KEY",
                          reason: "e2e", state: state, requesterPid: 7, createdAtMs: created)
    }

    static func accountRequest(_ id: String, state: RequestState = .pending,
                               created: Int64 = now) -> AccountRequestInfo {
        AccountRequestInfo(id: id, projectId: "p1", projectName: "app", name: "qa", fields: ["password"],
                           reason: "login", state: state, requesterPid: 7, createdAtMs: created)
    }

    static func service(instance: String?) -> ServiceStatus {
        ServiceStatus(serviceVersion: "0.0.4", contractVersion: 5, pid: 1, runningHeavy: 0,
                      waitingHeavy: 0, instance: instance)
    }

    static func install(_ id: String, at: Int64 = now) -> InstallEvent {
        InstallEvent(id: id, device: ResourceId(rawValue: "device:android:1"), appId: "com.app",
                     change: .installed, when: .firstSeen(atMs: at), source: .notThroughHostler,
                     recordedAtMs: at)
    }

    static func seizure(_ id: String, at: Int64 = now) -> SeizureRecord {
        SeizureRecord(id: id, resource: "screen", fromWorkspacePath: "/work/app", fromKind: .lease,
                      byWorkspacePath: "/work/other", byPid: 77, atMs: at, stranded: false, forced: false)
    }

    static func foreignSeizure(_ id: String, at: Int64 = now) -> SeizureRecord {
        SeizureRecord(id: id, resource: "Pixel", fromWorkspacePath: "/work/tool", fromKind: .foreign,
                      fromSource: "agent-device", byWorkspacePath: "/work/app", byPid: 7, atMs: at,
                      stranded: false, forced: false)
    }

    static func claimEnd(_ id: String, at: Int64 = now) -> ClaimHistoryItem {
        ClaimHistoryItem(id: id, resource: "screen", kind: .lease, workspacePath: "/work/app",
                         endedAtMs: at, reason: .seized, detail: "/work/other")
    }

    /// Reads that all stand at `seq`.
    static func reads(seq: UInt64, instance: String? = "A", capacity: UInt8 = 2,
                      secretRequests: [SecretRequestInfo] = [], installs: [InstallEvent] = [],
                      claimHistory: [ClaimHistoryItem] = [],
                      queueHistory: [HeavyHistoryItem] = []) -> ResyncReads {
        ResyncReads(
            overview: Overview(serviceVersion: "0.0.4", heavy: queue(seq: seq, capacity: capacity),
                               claims: claims(seq: seq), secrets: secrets(seq: seq)),
            devices: devices(seq: seq), projects: projects(seq: seq),
            requests: requests(seq: seq, secrets: secretRequests), service: service(instance: instance),
            queueHistory: queueHistory, claimHistory: claimHistory, installs: installs)
    }

    static func held(_ seq: UInt64, _ event: Event, on connection: ConnectionToken = 1) -> HeldEvent {
        HeldEvent(seq: seq, event: event, bytes: HeldEvent.encodedSize(of: event), connection: connection)
    }
}
