// The wire contract of hostler, for the app.
//
// Generated from the Rust contract by crates/hostler/src/contract/swift (plan P5 Task 3).
// Never edit by hand: run scripts/macos.sh contract --write.

import Foundation

/// The contract's version and frame limit (`CONTRACT_VERSION`, `MAX_FRAME_BYTES`).
public enum Contract {
    public static let version: UInt32 = 5
    public static let maxFrameBytes = 8388608
}

// MARK: - Methods

/// Every wire method of the contract, in the Rust table's order. (Not `Method`: that is
/// Objective-C's, visible wherever Foundation is.)
public enum HostlerMethod: String, Sendable, CaseIterable {
    case hello = "hello"
    case serviceStatus = "service.status"
    case serviceShutdown = "service.shutdown"
    case heavyAcquire = "heavy.acquire"
    case heavyAttach = "heavy.attach"
    case heavyStarted = "heavy.started"
    case heavyRelease = "heavy.release"
    case heavyWithdraw = "heavy.withdraw"
    case heavyForceRelease = "heavy.force_release"
    case queueSnapshot = "queue.snapshot"
    case queueHistory = "queue.history"
    case queueSetCapacity = "queue.set_capacity"
    case secretsStatus = "secrets.status"
    case secretsUnlock = "secrets.unlock"
    case secretsLock = "secrets.lock"
    case projectResolve = "project.resolve"
    case projectList = "project.list"
    case projectApprove = "project.approve"
    case secretList = "secret.list"
    case secretSet = "secret.set"
    case secretRemove = "secret.remove"
    case secretImport = "secret.import"
    case secretRequest = "secret.request"
    case secretRequestWithdraw = "secret.request_withdraw"
    case secretRequests = "secret.requests"
    case secretDecline = "secret.decline"
    case secretShare = "secret.share"
    case secretUnshare = "secret.unshare"
    case secretMaterialize = "secret.materialize"
    case workspaceResolve = "workspace.resolve"
    case workspaceList = "workspace.list"
    case claimsAcquire = "claims.acquire"
    case claimsAttach = "claims.attach"
    case claimsStarted = "claims.started"
    case claimsRelease = "claims.release"
    case claimsForceRelease = "claims.force_release"
    case claimsWithdraw = "claims.withdraw"
    case claimsRenew = "claims.renew"
    case claimsSetCommand = "claims.set_command"
    case claimsReserve = "claims.reserve"
    case claimsUnreserve = "claims.unreserve"
    case claimsSnapshot = "claims.snapshot"
    case claimsHistory = "claims.history"
    case claimsSeizeBegin = "claims.seize_begin"
    case claimsSeizeCommit = "claims.seize_commit"
    case claimsSeizeAbort = "claims.seize_abort"
    case claimsSeizeProgress = "claims.seize_progress"
    case statusOverview = "status.overview"
    case accountList = "account.list"
    case accountSet = "account.set"
    case accountRemove = "account.remove"
    case accountCheck = "account.check"
    case accountMaterialize = "account.materialize"
    case devicesList = "devices.list"
    case devicesShow = "devices.show"
    case devicesResolve = "devices.resolve"
    case devicesAdd = "devices.add"
    case devicesRename = "devices.rename"
    case devicesNote = "devices.note"
    case devicesRemove = "devices.remove"
    case devicesInstalls = "devices.installs"
    case devicesBoot = "devices.boot"
    case devicesTools = "devices.tools"
    case devicesForgetSessions = "devices.forget_sessions"
    case secretsProve = "secrets.prove"
    case secretReveal = "secret.reveal"
    case accountReveal = "account.reveal"
    case secretsRemovePreserved = "secrets.remove_preserved"
    case accountRequest = "account.request"
    case accountRequestWithdraw = "account.request_withdraw"
    case accountRequests = "account.requests"
    case accountDecline = "account.decline"
    case accountAnswer = "account.answer"
    case heavyStop = "heavy.stop"
    case projectsSnapshot = "projects.snapshot"
    case requestsSnapshot = "requests.snapshot"
}

/// One row of the method table (`contract/methods.rs`).
public struct MethodSpec: Sendable, Equatable {
    public let method: HostlerMethod
    /// How long a client waits for the answer.
    public let timeout: Duration
    /// Refused without a live proof on the connection (spec §8.5).
    public let gated: Bool
    /// Offered by the app's facade, `HostlerClient`.
    public let app: Bool
    /// The params may carry a value (`sensitive_request`).
    public let sensitiveRequest: Bool
    /// The answer carries values; it is never read as a `JSONValue` (`sensitive_answer`).
    public let sensitiveAnswer: Bool
}

extension HostlerMethod {
    public var spec: MethodSpec {
        switch self {
        case .hello:
            MethodSpec(method: .hello, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .serviceStatus:
            MethodSpec(method: .serviceStatus, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .serviceShutdown:
            MethodSpec(method: .serviceShutdown, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .heavyAcquire:
            MethodSpec(method: .heavyAcquire, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .heavyAttach:
            MethodSpec(method: .heavyAttach, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .heavyStarted:
            MethodSpec(method: .heavyStarted, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .heavyRelease:
            MethodSpec(method: .heavyRelease, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .heavyWithdraw:
            MethodSpec(method: .heavyWithdraw, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .heavyForceRelease:
            MethodSpec(method: .heavyForceRelease, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .queueSnapshot:
            MethodSpec(method: .queueSnapshot, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .queueHistory:
            MethodSpec(method: .queueHistory, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .queueSetCapacity:
            MethodSpec(method: .queueSetCapacity, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretsStatus:
            MethodSpec(method: .secretsStatus, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretsUnlock:
            MethodSpec(method: .secretsUnlock, timeout: .milliseconds(120000), gated: false, app: true, sensitiveRequest: true, sensitiveAnswer: false)
        case .secretsLock:
            MethodSpec(method: .secretsLock, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .projectResolve:
            MethodSpec(method: .projectResolve, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .projectList:
            MethodSpec(method: .projectList, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .projectApprove:
            MethodSpec(method: .projectApprove, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretList:
            MethodSpec(method: .secretList, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretSet:
            MethodSpec(method: .secretSet, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: true, sensitiveAnswer: false)
        case .secretRemove:
            MethodSpec(method: .secretRemove, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretImport:
            MethodSpec(method: .secretImport, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: true, sensitiveAnswer: false)
        case .secretRequest:
            MethodSpec(method: .secretRequest, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretRequestWithdraw:
            MethodSpec(method: .secretRequestWithdraw, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretRequests:
            MethodSpec(method: .secretRequests, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretDecline:
            MethodSpec(method: .secretDecline, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretShare:
            MethodSpec(method: .secretShare, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretUnshare:
            MethodSpec(method: .secretUnshare, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretMaterialize:
            MethodSpec(method: .secretMaterialize, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: true, sensitiveAnswer: true)
        case .workspaceResolve:
            MethodSpec(method: .workspaceResolve, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .workspaceList:
            MethodSpec(method: .workspaceList, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsAcquire:
            MethodSpec(method: .claimsAcquire, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsAttach:
            MethodSpec(method: .claimsAttach, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsStarted:
            MethodSpec(method: .claimsStarted, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsRelease:
            MethodSpec(method: .claimsRelease, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsForceRelease:
            MethodSpec(method: .claimsForceRelease, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsWithdraw:
            MethodSpec(method: .claimsWithdraw, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsRenew:
            MethodSpec(method: .claimsRenew, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsSetCommand:
            MethodSpec(method: .claimsSetCommand, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsReserve:
            MethodSpec(method: .claimsReserve, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsUnreserve:
            MethodSpec(method: .claimsUnreserve, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsSnapshot:
            MethodSpec(method: .claimsSnapshot, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsHistory:
            MethodSpec(method: .claimsHistory, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsSeizeBegin:
            MethodSpec(method: .claimsSeizeBegin, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsSeizeCommit:
            MethodSpec(method: .claimsSeizeCommit, timeout: .milliseconds(15000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsSeizeAbort:
            MethodSpec(method: .claimsSeizeAbort, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .claimsSeizeProgress:
            MethodSpec(method: .claimsSeizeProgress, timeout: .milliseconds(5000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .statusOverview:
            MethodSpec(method: .statusOverview, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountList:
            MethodSpec(method: .accountList, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountSet:
            MethodSpec(method: .accountSet, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: true, sensitiveAnswer: false)
        case .accountRemove:
            MethodSpec(method: .accountRemove, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountCheck:
            MethodSpec(method: .accountCheck, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountMaterialize:
            MethodSpec(method: .accountMaterialize, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: true, sensitiveAnswer: true)
        case .devicesList:
            MethodSpec(method: .devicesList, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesShow:
            MethodSpec(method: .devicesShow, timeout: .milliseconds(10000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesResolve:
            MethodSpec(method: .devicesResolve, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesAdd:
            MethodSpec(method: .devicesAdd, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesRename:
            MethodSpec(method: .devicesRename, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesNote:
            MethodSpec(method: .devicesNote, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesRemove:
            MethodSpec(method: .devicesRemove, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesInstalls:
            MethodSpec(method: .devicesInstalls, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesBoot:
            MethodSpec(method: .devicesBoot, timeout: .milliseconds(200000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesTools:
            MethodSpec(method: .devicesTools, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .devicesForgetSessions:
            MethodSpec(method: .devicesForgetSessions, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .secretsProve:
            MethodSpec(method: .secretsProve, timeout: .milliseconds(120000), gated: false, app: true, sensitiveRequest: true, sensitiveAnswer: false)
        case .secretReveal:
            MethodSpec(method: .secretReveal, timeout: .milliseconds(60000), gated: true, app: true, sensitiveRequest: false, sensitiveAnswer: true)
        case .accountReveal:
            MethodSpec(method: .accountReveal, timeout: .milliseconds(60000), gated: true, app: true, sensitiveRequest: false, sensitiveAnswer: true)
        case .secretsRemovePreserved:
            MethodSpec(method: .secretsRemovePreserved, timeout: .milliseconds(60000), gated: true, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountRequest:
            MethodSpec(method: .accountRequest, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountRequestWithdraw:
            MethodSpec(method: .accountRequestWithdraw, timeout: .milliseconds(60000), gated: false, app: false, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountRequests:
            MethodSpec(method: .accountRequests, timeout: .milliseconds(60000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountDecline:
            MethodSpec(method: .accountDecline, timeout: .milliseconds(60000), gated: true, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .accountAnswer:
            MethodSpec(method: .accountAnswer, timeout: .milliseconds(60000), gated: true, app: true, sensitiveRequest: true, sensitiveAnswer: false)
        case .heavyStop:
            MethodSpec(method: .heavyStop, timeout: .milliseconds(60000), gated: true, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .projectsSnapshot:
            MethodSpec(method: .projectsSnapshot, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        case .requestsSnapshot:
            MethodSpec(method: .requestsSnapshot, timeout: .milliseconds(5000), gated: false, app: true, sensitiveRequest: false, sensitiveAnswer: false)
        }
    }
}

extension Request {
    /// The wire method of this request.
    public var method: HostlerMethod {
        switch self {
        case .hello: .hello
        case .serviceStatus: .serviceStatus
        case .serviceShutdown: .serviceShutdown
        case .heavyAcquire: .heavyAcquire
        case .heavyAttach: .heavyAttach
        case .heavyStarted: .heavyStarted
        case .heavyRelease: .heavyRelease
        case .heavyWithdraw: .heavyWithdraw
        case .heavyForceRelease: .heavyForceRelease
        case .queueSnapshot: .queueSnapshot
        case .queueHistory: .queueHistory
        case .queueSetCapacity: .queueSetCapacity
        case .secretsStatus: .secretsStatus
        case .secretsUnlock: .secretsUnlock
        case .secretsLock: .secretsLock
        case .projectResolve: .projectResolve
        case .projectList: .projectList
        case .projectApprove: .projectApprove
        case .secretList: .secretList
        case .secretSet: .secretSet
        case .secretRemove: .secretRemove
        case .secretImport: .secretImport
        case .secretRequest: .secretRequest
        case .secretRequestWithdraw: .secretRequestWithdraw
        case .secretRequests: .secretRequests
        case .secretDecline: .secretDecline
        case .secretShare: .secretShare
        case .secretUnshare: .secretUnshare
        case .secretMaterialize: .secretMaterialize
        case .workspaceResolve: .workspaceResolve
        case .workspaceList: .workspaceList
        case .claimsAcquire: .claimsAcquire
        case .claimsAttach: .claimsAttach
        case .claimsStarted: .claimsStarted
        case .claimsRelease: .claimsRelease
        case .claimsForceRelease: .claimsForceRelease
        case .claimsWithdraw: .claimsWithdraw
        case .claimsRenew: .claimsRenew
        case .claimsSetCommand: .claimsSetCommand
        case .claimsReserve: .claimsReserve
        case .claimsUnreserve: .claimsUnreserve
        case .claimsSnapshot: .claimsSnapshot
        case .claimsHistory: .claimsHistory
        case .claimsSeizeBegin: .claimsSeizeBegin
        case .claimsSeizeCommit: .claimsSeizeCommit
        case .claimsSeizeAbort: .claimsSeizeAbort
        case .claimsSeizeProgress: .claimsSeizeProgress
        case .statusOverview: .statusOverview
        case .accountList: .accountList
        case .accountSet: .accountSet
        case .accountRemove: .accountRemove
        case .accountCheck: .accountCheck
        case .accountMaterialize: .accountMaterialize
        case .devicesList: .devicesList
        case .devicesShow: .devicesShow
        case .devicesResolve: .devicesResolve
        case .devicesAdd: .devicesAdd
        case .devicesRename: .devicesRename
        case .devicesNote: .devicesNote
        case .devicesRemove: .devicesRemove
        case .devicesInstalls: .devicesInstalls
        case .devicesBoot: .devicesBoot
        case .devicesTools: .devicesTools
        case .devicesForgetSessions: .devicesForgetSessions
        case .secretsProve: .secretsProve
        case .secretReveal: .secretReveal
        case .accountReveal: .accountReveal
        case .secretsRemovePreserved: .secretsRemovePreserved
        case .accountRequest: .accountRequest
        case .accountRequestWithdraw: .accountRequestWithdraw
        case .accountRequests: .accountRequests
        case .accountDecline: .accountDecline
        case .accountAnswer: .accountAnswer
        case .heavyStop: .heavyStop
        case .projectsSnapshot: .projectsSnapshot
        case .requestsSnapshot: .requestsSnapshot
        }
    }
}

/// The app's typed calls: only the methods the table marks `app` (no materialize: the app
/// never builds one, though `Request` can express it). Timeouts come from the table.
public struct HostlerClient: Sendable {
    public let transport: any HostlerTransport

    public init(transport: any HostlerTransport) {
        self.transport = transport
    }
}

extension HostlerClient {
    /// `hello`, at most 5 s.
    public func hello(contractVersion: UInt32, client: String) async throws(HostlerCallError) -> HelloReply {
        try await answer(.hello(contractVersion: contractVersion, client: client))
    }

    /// `service.status`, at most 5 s.
    public func serviceStatus() async throws(HostlerCallError) -> ServiceStatus {
        try await answer(.serviceStatus)
    }

    /// `heavy.withdraw`, at most 5 s.
    public func heavyWithdraw(id: String, timedOut: Bool) async throws(HostlerCallError) {
        try await unit(.heavyWithdraw(id: id, timedOut: timedOut))
    }

    /// `queue.snapshot`, at most 5 s.
    public func queueSnapshot() async throws(HostlerCallError) -> QueueSnapshot {
        try await answer(.queueSnapshot)
    }

    /// `queue.history`, at most 5 s.
    public func queueHistory(_ params: HistoryQuery) async throws(HostlerCallError) -> [HeavyHistoryItem] {
        try await answer(.queueHistory(params))
    }

    /// `queue.set_capacity`, at most 5 s.
    public func queueSetCapacity(capacity: UInt8) async throws(HostlerCallError) {
        try await unit(.queueSetCapacity(capacity: capacity))
    }

    /// `secrets.status`, at most 5 s.
    public func secretsStatus() async throws(HostlerCallError) -> SecretsStatus {
        try await answer(.secretsStatus)
    }

    /// `secrets.unlock`, at most 120 s.
    public func secretsUnlock(_ params: Unlock) async throws(HostlerCallError) -> UnlockOutcome {
        try await answer(.secretsUnlock(params))
    }

    /// `secrets.lock`, at most 5 s.
    public func secretsLock() async throws(HostlerCallError) -> SecretsStatus {
        try await answer(.secretsLock)
    }

    /// `project.resolve`, at most 5 s.
    public func projectResolve(_ params: ProjectRef) async throws(HostlerCallError) -> ProjectInfo {
        try await answer(.projectResolve(params))
    }

    /// `project.list`, at most 5 s.
    public func projectList() async throws(HostlerCallError) -> [ProjectInfo] {
        try await answer(.projectList)
    }

    /// `project.approve`, at most 5 s.
    public func projectApprove(_ params: ProjectRef) async throws(HostlerCallError) -> ProjectInfo {
        try await answer(.projectApprove(params))
    }

    /// `secret.list`, at most 60 s.
    public func secretList(project: ProjectRef, declared: [DeclaredSecret]) async throws(HostlerCallError) -> SecretList {
        try await answer(.secretList(project: project, declared: declared))
    }

    /// `secret.set`, at most 60 s.
    public func secretSet(_ params: SecretSet) async throws(HostlerCallError) -> SetOutcome {
        try await answer(.secretSet(params))
    }

    /// `secret.remove`, at most 60 s.
    public func secretRemove(project: ProjectRef, name: String) async throws(HostlerCallError) -> Removed {
        try await answer(.secretRemove(project: project, name: name))
    }

    /// `secret.requests`, at most 5 s.
    public func secretRequests() async throws(HostlerCallError) -> [SecretRequestInfo] {
        try await answer(.secretRequests)
    }

    /// `secret.decline`, at most 5 s.
    public func secretDecline(_ params: Decline) async throws(HostlerCallError) -> [SecretRequestInfo] {
        try await answer(.secretDecline(params))
    }

    /// `secret.share`, at most 60 s.
    public func secretShare(_ params: Share) async throws(HostlerCallError) -> ShareOutcome {
        try await answer(.secretShare(params))
    }

    /// `secret.unshare`, at most 5 s.
    public func secretUnshare(_ params: Share) async throws(HostlerCallError) -> Unshared {
        try await answer(.secretUnshare(params))
    }

    /// `workspace.resolve`, at most 5 s.
    public func workspaceResolve(_ params: WorkspaceResolve) async throws(HostlerCallError) -> WorkspaceInfo {
        try await answer(.workspaceResolve(params))
    }

    /// `workspace.list`, at most 5 s.
    public func workspaceList() async throws(HostlerCallError) -> [WorkspaceInfo] {
        try await answer(.workspaceList)
    }

    /// `claims.withdraw`, at most 5 s.
    public func claimsWithdraw(requestId: String, timedOut: Bool) async throws(HostlerCallError) {
        try await unit(.claimsWithdraw(requestId: requestId, timedOut: timedOut))
    }

    /// `claims.renew`, at most 5 s.
    public func claimsRenew(claimId: String, workspaceId: String) async throws(HostlerCallError) -> Claim {
        try await answer(.claimsRenew(claimId: claimId, workspaceId: workspaceId))
    }

    /// `claims.reserve`, at most 5 s.
    public func claimsReserve(resource: ResourceId, workspaceId: String) async throws(HostlerCallError) -> Claim {
        try await answer(.claimsReserve(resource: resource, workspaceId: workspaceId))
    }

    /// `claims.unreserve`, at most 5 s.
    public func claimsUnreserve(resource: ResourceId, workspaceId: String) async throws(HostlerCallError) {
        try await unit(.claimsUnreserve(resource: resource, workspaceId: workspaceId))
    }

    /// `claims.snapshot`, at most 5 s.
    public func claimsSnapshot() async throws(HostlerCallError) -> ClaimsSnapshot {
        try await answer(.claimsSnapshot)
    }

    /// `claims.history`, at most 5 s.
    public func claimsHistory(_ params: HistoryQuery) async throws(HostlerCallError) -> [ClaimHistoryItem] {
        try await answer(.claimsHistory(params))
    }

    /// `status.overview`, at most 5 s.
    public func statusOverview() async throws(HostlerCallError) -> Overview {
        try await answer(.statusOverview)
    }

    /// `account.list`, at most 60 s.
    public func accountList(project: ProjectRef) async throws(HostlerCallError) -> [AccountInfo] {
        try await answer(.accountList(project: project))
    }

    /// `account.set`, at most 60 s.
    public func accountSet(_ params: AccountSet) async throws(HostlerCallError) -> AccountFieldSet {
        try await answer(.accountSet(params))
    }

    /// `account.remove`, at most 60 s.
    public func accountRemove(_ params: AccountRemove) async throws(HostlerCallError) -> Removed {
        try await answer(.accountRemove(params))
    }

    /// `devices.list`, at most 5 s.
    public func devicesList(all: Bool) async throws(HostlerCallError) -> DevicesSnapshot {
        try await answer(.devicesList(all: all))
    }

    /// `devices.show`, at most 10 s.
    public func devicesShow(device: String, freshSessions: Bool) async throws(HostlerCallError) -> DeviceDetail {
        try await answer(.devicesShow(device: device, freshSessions: freshSessions))
    }

    /// `devices.resolve`, at most 5 s.
    public func devicesResolve(device: String) async throws(HostlerCallError) -> DeviceInfo {
        try await answer(.devicesResolve(device: device))
    }

    /// `devices.add`, at most 60 s.
    public func devicesAdd(device: String, alias: String? = nil, person: Bool) async throws(HostlerCallError) -> DeviceInfo {
        try await answer(.devicesAdd(device: device, alias: alias, person: person))
    }

    /// `devices.rename`, at most 60 s.
    public func devicesRename(device: String, alias: String, person: Bool) async throws(HostlerCallError) -> DeviceInfo {
        try await answer(.devicesRename(device: device, alias: alias, person: person))
    }

    /// `devices.note`, at most 5 s.
    public func devicesNote(device: String, note: String? = nil) async throws(HostlerCallError) -> DeviceInfo {
        try await answer(.devicesNote(device: device, note: note))
    }

    /// `devices.remove`, at most 60 s.
    public func devicesRemove(device: String, person: Bool) async throws(HostlerCallError) -> DeviceRemoved {
        try await answer(.devicesRemove(device: device, person: person))
    }

    /// `devices.installs`, at most 5 s.
    public func devicesInstalls(device: String? = nil, query: HistoryQuery) async throws(HostlerCallError) -> [InstallEvent] {
        try await answer(.devicesInstalls(device: device, query: query))
    }

    /// `devices.tools`, at most 5 s.
    public func devicesTools(_ params: ToolPathsSet) async throws(HostlerCallError) -> ToolStatus {
        try await answer(.devicesTools(params))
    }

    /// `devices.forget_sessions`, at most 60 s.
    public func devicesForgetSessions(device: String, person: Bool, byWorkspacePath: String? = nil, byPid: UInt32? = nil) async throws(HostlerCallError) -> ForgetRecord {
        try await answer(.devicesForgetSessions(device: device, person: person, byWorkspacePath: byWorkspacePath, byPid: byPid))
    }

    /// `secrets.prove`, at most 120 s.
    public func secretsProve(_ params: Prove) async throws(HostlerCallError) -> ProofGrant {
        try await answer(.secretsProve(params))
    }

    /// `secret.reveal`, at most 60 s. Gated: needs a live proof on the connection (`secretsProve`). The answer holds values: read raw, kept in `SecretText`.
    public func secretReveal(project: ProjectRef, name: String) async throws(HostlerCallError) -> MaterializedSecret {
        try await sensitiveAnswer(.secretReveal(project: project, name: name))
    }

    /// `account.reveal`, at most 60 s. Gated: needs a live proof on the connection (`secretsProve`). The answer holds values: read raw, kept in `SecretText`.
    public func accountReveal(project: ProjectRef, name: String) async throws(HostlerCallError) -> MaterializedAccount {
        try await sensitiveAnswer(.accountReveal(project: project, name: name))
    }

    /// `secrets.remove_preserved`, at most 60 s. Gated: needs a live proof on the connection (`secretsProve`).
    public func secretsRemovePreserved(path: String) async throws(HostlerCallError) -> SecretsStatus {
        try await answer(.secretsRemovePreserved(path: path))
    }

    /// `account.requests`, at most 60 s.
    public func accountRequests() async throws(HostlerCallError) -> [AccountRequestInfo] {
        try await answer(.accountRequests)
    }

    /// `account.decline`, at most 60 s. Gated: needs a live proof on the connection (`secretsProve`).
    public func accountDecline(id: String) async throws(HostlerCallError) -> AccountRequestInfo {
        try await answer(.accountDecline(id: id))
    }

    /// `account.answer`, at most 60 s. Gated: needs a live proof on the connection (`secretsProve`).
    public func accountAnswer(_ params: AccountAnswer) async throws(HostlerCallError) -> AccountAnswerOutcome {
        try await answer(.accountAnswer(params))
    }

    /// `heavy.stop`, at most 60 s. Gated: needs a live proof on the connection (`secretsProve`).
    public func heavyStop(id: String, confirmation: String) async throws(HostlerCallError) -> QueueStopOutcome {
        try await answer(.heavyStop(id: id, confirmation: confirmation))
    }

    /// `projects.snapshot`, at most 5 s.
    public func projectsSnapshot() async throws(HostlerCallError) -> ProjectsSnapshot {
        try await answer(.projectsSnapshot)
    }

    /// `requests.snapshot`, at most 5 s.
    public func requestsSnapshot() async throws(HostlerCallError) -> RequestsSnapshot {
        try await answer(.requestsSnapshot)
    }
}

// MARK: - Types

public struct AccountAnswer: Codable, Sendable, Equatable {
    public var requestId: String
    public var fields: [ImportEntry]

    public init(requestId: String, fields: [ImportEntry]) {
        self.requestId = requestId
        self.fields = fields
    }

    private enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
        case fields
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.requestId = try container.decode(String.self, forKey: .requestId)
        self.fields = try container.decode([ImportEntry].self, forKey: .fields)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.requestId, forKey: .requestId)
        try container.encode(self.fields, forKey: .fields)
    }
}

public struct AccountAnswerOutcome: Codable, Sendable, Equatable {
    public var name: String
    public var storedFields: [String]
    public var answeredRequests: [String]

    public init(name: String, storedFields: [String], answeredRequests: [String]) {
        self.name = name
        self.storedFields = storedFields
        self.answeredRequests = answeredRequests
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case storedFields = "stored_fields"
        case answeredRequests = "answered_requests"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.storedFields = try container.decode([String].self, forKey: .storedFields)
        self.answeredRequests = try container.decode([String].self, forKey: .answeredRequests)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.storedFields, forKey: .storedFields)
        try container.encode(self.answeredRequests, forKey: .answeredRequests)
    }
}

public struct AccountFieldSet: Codable, Sendable, Equatable {
    public var name: String
    public var field: String

    public init(name: String, field: String) {
        self.name = name
        self.field = field
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case field
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.field = try container.decode(String.self, forKey: .field)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.field, forKey: .field)
    }
}

public struct AccountInfo: Codable, Sendable, Equatable {
    public var name: String
    public var fields: [String]
    public var holder: ClaimSummary?

    public init(name: String, fields: [String], holder: ClaimSummary? = nil) {
        self.name = name
        self.fields = fields
        self.holder = holder
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case fields
        case holder
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.fields = try container.decode([String].self, forKey: .fields)
        self.holder = try container.decodeIfPresent(ClaimSummary.self, forKey: .holder)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.fields, forKey: .fields)
        try container.encode(self.holder, forKey: .holder)
    }
}

public struct AccountMaterialize: Codable, Sendable, Equatable {
    public var runId: String
    public var claimId: String
    public var requester: ProcessIdentity
    public var project: ProjectRef
    public var name: String
    public var command: SecretText
    public var workspace: String

    public init(runId: String, claimId: String, requester: ProcessIdentity, project: ProjectRef, name: String, command: SecretText, workspace: String) {
        self.runId = runId
        self.claimId = claimId
        self.requester = requester
        self.project = project
        self.name = name
        self.command = command
        self.workspace = workspace
    }

    private enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case claimId = "claim_id"
        case requester
        case project
        case name
        case command
        case workspace
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.runId = try container.decode(String.self, forKey: .runId)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
        self.command = try container.decode(SecretText.self, forKey: .command)
        self.workspace = try container.decode(String.self, forKey: .workspace)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.runId, forKey: .runId)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.requester, forKey: .requester)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.workspace, forKey: .workspace)
    }
}

public struct AccountRemove: Codable, Sendable, Equatable {
    public var project: ProjectRef
    public var name: String
    public var field: String?

    public init(project: ProjectRef, name: String, field: String? = nil) {
        self.project = project
        self.name = name
        self.field = field
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
        case field
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
        self.field = try container.decodeIfPresent(String.self, forKey: .field)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.field, forKey: .field)
    }
}

public struct AccountRequestInfo: Codable, Sendable, Equatable {
    public var id: String
    public var projectId: String
    public var projectName: String
    public var name: String
    public var fields: [String]
    public var reason: String
    public var state: RequestState
    public var requesterPid: UInt32
    public var createdAtMs: Int64
    public var answeredAtMs: Int64?

    public init(id: String, projectId: String, projectName: String, name: String, fields: [String], reason: String, state: RequestState, requesterPid: UInt32, createdAtMs: Int64, answeredAtMs: Int64? = nil) {
        self.id = id
        self.projectId = projectId
        self.projectName = projectName
        self.name = name
        self.fields = fields
        self.reason = reason
        self.state = state
        self.requesterPid = requesterPid
        self.createdAtMs = createdAtMs
        self.answeredAtMs = answeredAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case projectName = "project_name"
        case name
        case fields
        case reason
        case state
        case requesterPid = "requester_pid"
        case createdAtMs = "created_at_ms"
        case answeredAtMs = "answered_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.projectId = try container.decode(String.self, forKey: .projectId)
        self.projectName = try container.decode(String.self, forKey: .projectName)
        self.name = try container.decode(String.self, forKey: .name)
        self.fields = try container.decode([String].self, forKey: .fields)
        self.reason = try container.decode(String.self, forKey: .reason)
        self.state = try container.decode(RequestState.self, forKey: .state)
        self.requesterPid = try container.decode(UInt32.self, forKey: .requesterPid)
        self.createdAtMs = try container.decode(Int64.self, forKey: .createdAtMs)
        self.answeredAtMs = try container.decodeIfPresent(Int64.self, forKey: .answeredAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.projectId, forKey: .projectId)
        try container.encode(self.projectName, forKey: .projectName)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.fields, forKey: .fields)
        try container.encode(self.reason, forKey: .reason)
        try container.encode(self.state, forKey: .state)
        try container.encode(self.requesterPid, forKey: .requesterPid)
        try container.encode(self.createdAtMs, forKey: .createdAtMs)
        try container.encode(self.answeredAtMs, forKey: .answeredAtMs)
    }
}

public struct AccountRequestNew: Codable, Sendable, Equatable {
    public var id: String
    public var project: ProjectRef
    public var name: String
    public var fields: [String]
    public var reason: String
    public var requester: ProcessIdentity

    public init(id: String, project: ProjectRef, name: String, fields: [String], reason: String, requester: ProcessIdentity) {
        self.id = id
        self.project = project
        self.name = name
        self.fields = fields
        self.reason = reason
        self.requester = requester
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case project
        case name
        case fields
        case reason
        case requester
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
        self.fields = try container.decode([String].self, forKey: .fields)
        self.reason = try container.decode(String.self, forKey: .reason)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.fields, forKey: .fields)
        try container.encode(self.reason, forKey: .reason)
        try container.encode(self.requester, forKey: .requester)
    }
}

public struct AccountSet: Codable, Sendable, Equatable {
    public var project: ProjectRef
    public var name: String
    public var field: String
    public var value: SecretText

    public init(project: ProjectRef, name: String, field: String, value: SecretText) {
        self.project = project
        self.name = name
        self.field = field
        self.value = value
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
        case field
        case value
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
        self.field = try container.decode(String.self, forKey: .field)
        self.value = try container.decode(SecretText.self, forKey: .value)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.field, forKey: .field)
        try container.encode(self.value, forKey: .value)
    }
}

public enum AcquireOutcome: Codable, Sendable, Equatable {
    case granted(entry: HeavyEntry)
    case queued(entry: HeavyEntry, position: UInt32, of: UInt32)
    case reentrant
    case busy(position: UInt32, of: UInt32)
    case finished(item: HeavyHistoryItem)

    private enum TagKeys: String, CodingKey {
        case tag = "outcome"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "granted":
            let payload = try _AcquireOutcomeGranted(from: decoder)
            self = .granted(entry: payload.entry)
        case "queued":
            let payload = try _AcquireOutcomeQueued(from: decoder)
            self = .queued(entry: payload.entry, position: payload.position, of: payload.of)
        case "reentrant":
            self = .reentrant
        case "busy":
            let payload = try _AcquireOutcomeBusy(from: decoder)
            self = .busy(position: payload.position, of: payload.of)
        case "finished":
            let payload = try _AcquireOutcomeFinished(from: decoder)
            self = .finished(item: payload.item)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "AcquireOutcome")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .granted(let entry):
            try container.encode("granted", forKey: .tag)
            try _AcquireOutcomeGranted(entry: entry).encode(to: encoder)
        case .queued(let entry, let position, let of):
            try container.encode("queued", forKey: .tag)
            try _AcquireOutcomeQueued(entry: entry, position: position, of: of).encode(to: encoder)
        case .reentrant:
            try container.encode("reentrant", forKey: .tag)
        case .busy(let position, let of):
            try container.encode("busy", forKey: .tag)
            try _AcquireOutcomeBusy(position: position, of: of).encode(to: encoder)
        case .finished(let item):
            try container.encode("finished", forKey: .tag)
            try _AcquireOutcomeFinished(item: item).encode(to: encoder)
        }
    }
}

public enum AdbState: Codable, Sendable, Equatable {
    case ours
    case missing
    case foreign(version: UInt32)

    private enum CodingKeys: String, CodingKey {
        case foreign = "foreign"
    }

    public init(from decoder: any Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let tag = try? single.decode(String.self) {
            switch tag {
            case "ours": self = .ours
            case "missing": self = .missing
            default: throw ContractCoding.unknownVariant(decoder, type: "AdbState")
            }
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard container.allKeys.count == 1, let key = container.allKeys.first else {
            throw ContractCoding.unknownVariant(decoder, type: "AdbState")
        }
        switch key {
        case .foreign:
            let payload = try container.decode(_AdbStateForeign.self, forKey: .foreign)
            self = .foreign(version: payload.version)
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .ours:
            var container = encoder.singleValueContainer()
            try container.encode("ours")
        case .missing:
            var container = encoder.singleValueContainer()
            try container.encode("missing")
        case .foreign(let version):
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(_AdbStateForeign(version: version), forKey: .foreign)
        }
    }
}

public struct AgentDeviceSession: Codable, Sendable, Equatable {
    public var session: String
    public var workspacePath: String
    public var workspaceId: String?
    public var seenSinceMs: Int64
    public var kept: Bool
    public var platform: Platform
    public var agentDeviceId: String

    public init(session: String, workspacePath: String, workspaceId: String? = nil, seenSinceMs: Int64, kept: Bool, platform: Platform, agentDeviceId: String) {
        self.session = session
        self.workspacePath = workspacePath
        self.workspaceId = workspaceId
        self.seenSinceMs = seenSinceMs
        self.kept = kept
        self.platform = platform
        self.agentDeviceId = agentDeviceId
    }

    private enum CodingKeys: String, CodingKey {
        case session
        case workspacePath = "workspace_path"
        case workspaceId = "workspace_id"
        case seenSinceMs = "seen_since_ms"
        case kept
        case platform
        case agentDeviceId = "agent_device_id"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.session = try container.decode(String.self, forKey: .session)
        self.workspacePath = try container.decode(String.self, forKey: .workspacePath)
        self.workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
        self.seenSinceMs = try container.decode(Int64.self, forKey: .seenSinceMs)
        self.kept = try container.decode(Bool.self, forKey: .kept)
        self.platform = try container.decode(Platform.self, forKey: .platform)
        self.agentDeviceId = try container.decode(String.self, forKey: .agentDeviceId)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.session, forKey: .session)
        try container.encode(self.workspacePath, forKey: .workspacePath)
        try container.encode(self.workspaceId, forKey: .workspaceId)
        try container.encode(self.seenSinceMs, forKey: .seenSinceMs)
        try container.encode(self.kept, forKey: .kept)
        try container.encode(self.platform, forKey: .platform)
        try container.encode(self.agentDeviceId, forKey: .agentDeviceId)
    }
}

public struct AppState: Codable, Sendable, Equatable {
    public var appId: String
    public var versionName: String?
    public var versionCode: String?
    public var installed: InstallTime?
    public var updated: InstallTime?

    public init(appId: String, versionName: String? = nil, versionCode: String? = nil, installed: InstallTime? = nil, updated: InstallTime? = nil) {
        self.appId = appId
        self.versionName = versionName
        self.versionCode = versionCode
        self.installed = installed
        self.updated = updated
    }

    private enum CodingKeys: String, CodingKey {
        case appId = "app_id"
        case versionName = "version_name"
        case versionCode = "version_code"
        case installed
        case updated
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.appId = try container.decode(String.self, forKey: .appId)
        self.versionName = try container.decodeIfPresent(String.self, forKey: .versionName)
        self.versionCode = try container.decodeIfPresent(String.self, forKey: .versionCode)
        self.installed = try container.decodeIfPresent(InstallTime.self, forKey: .installed)
        self.updated = try container.decodeIfPresent(InstallTime.self, forKey: .updated)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.appId, forKey: .appId)
        try container.encode(self.versionName, forKey: .versionName)
        try container.encode(self.versionCode, forKey: .versionCode)
        try container.encode(self.installed, forKey: .installed)
        try container.encode(self.updated, forKey: .updated)
    }
}

public enum Blocker: Codable, Sendable, Equatable {
    case claim(summary: ClaimSummary)
    case foreign(claim: ForeignClaim)
    case seizing(byWorkspace: String)
    case checking(resource: String, text: String)

    private enum TagKeys: String, CodingKey {
        case tag = "blocker"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "claim":
            let payload = try _BlockerClaim(from: decoder)
            self = .claim(summary: payload.summary)
        case "foreign":
            let payload = try _BlockerForeign(from: decoder)
            self = .foreign(claim: payload.claim)
        case "seizing":
            let payload = try _BlockerSeizing(from: decoder)
            self = .seizing(byWorkspace: payload.byWorkspace)
        case "checking":
            let payload = try _BlockerChecking(from: decoder)
            self = .checking(resource: payload.resource, text: payload.text)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "Blocker")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .claim(let summary):
            try container.encode("claim", forKey: .tag)
            try _BlockerClaim(summary: summary).encode(to: encoder)
        case .foreign(let claim):
            try container.encode("foreign", forKey: .tag)
            try _BlockerForeign(claim: claim).encode(to: encoder)
        case .seizing(let byWorkspace):
            try container.encode("seizing", forKey: .tag)
            try _BlockerSeizing(byWorkspace: byWorkspace).encode(to: encoder)
        case .checking(let resource, let text):
            try container.encode("checking", forKey: .tag)
            try _BlockerChecking(resource: resource, text: text).encode(to: encoder)
        }
    }
}

public struct Check: Codable, Sendable, Equatable {
    public var id: String
    public var status: CheckStatus
    public var message: String
    public var fix: Fix?
    public var details: JSONValue

    public init(id: String, status: CheckStatus, message: String, fix: Fix? = nil, details: JSONValue) {
        self.id = id
        self.status = status
        self.message = message
        self.fix = fix
        self.details = details
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case status
        case message
        case fix
        case details
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.status = try container.decode(CheckStatus.self, forKey: .status)
        self.message = try container.decode(String.self, forKey: .message)
        self.fix = try container.decodeIfPresent(Fix.self, forKey: .fix)
        self.details = try container.decode(JSONValue.self, forKey: .details)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.status, forKey: .status)
        try container.encode(self.message, forKey: .message)
        try container.encode(self.fix, forKey: .fix)
        try container.encode(self.details, forKey: .details)
    }
}

public enum CheckStatus: String, Codable, Sendable, CaseIterable {
    case ok
    case warn
    case problem
    case skipped
}

public struct Claim: Codable, Sendable, Equatable {
    public var id: String
    public var resource: ResourceInfo
    public var kind: ClaimKind
    public var parent: String?
    public var workspace: WorkspaceRef
    public var holder: ProcessIdentity?
    public var child: ProcessIdentity?
    public var group: RunGroup?
    public var binding: HoldBinding?
    public var command: String?
    public var grantedAtMs: Int64

    public init(id: String, resource: ResourceInfo, kind: ClaimKind, parent: String? = nil, workspace: WorkspaceRef, holder: ProcessIdentity? = nil, child: ProcessIdentity? = nil, group: RunGroup? = nil, binding: HoldBinding? = nil, command: String? = nil, grantedAtMs: Int64) {
        self.id = id
        self.resource = resource
        self.kind = kind
        self.parent = parent
        self.workspace = workspace
        self.holder = holder
        self.child = child
        self.group = group
        self.binding = binding
        self.command = command
        self.grantedAtMs = grantedAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case resource
        case kind
        case parent
        case workspace
        case holder
        case child
        case group
        case binding
        case command
        case grantedAtMs = "granted_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.resource = try container.decode(ResourceInfo.self, forKey: .resource)
        self.kind = try container.decode(ClaimKind.self, forKey: .kind)
        self.parent = try container.decodeIfPresent(String.self, forKey: .parent)
        self.workspace = try container.decode(WorkspaceRef.self, forKey: .workspace)
        self.holder = try container.decodeIfPresent(ProcessIdentity.self, forKey: .holder)
        self.child = try container.decodeIfPresent(ProcessIdentity.self, forKey: .child)
        self.group = try container.decodeIfPresent(RunGroup.self, forKey: .group)
        self.binding = try container.decodeIfPresent(HoldBinding.self, forKey: .binding)
        self.command = try container.decodeIfPresent(String.self, forKey: .command)
        self.grantedAtMs = try container.decode(Int64.self, forKey: .grantedAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.parent, forKey: .parent)
        try container.encode(self.workspace, forKey: .workspace)
        try container.encode(self.holder, forKey: .holder)
        try container.encode(self.child, forKey: .child)
        try container.encode(self.group, forKey: .group)
        try container.encode(self.binding, forKey: .binding)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.grantedAtMs, forKey: .grantedAtMs)
    }
}

public struct ClaimHistoryItem: Codable, Sendable, Equatable {
    public var id: String
    public var resource: String
    public var resourceId: ResourceId?
    public var kind: ClaimKind
    public var workspacePath: String
    public var command: String?
    public var grantedAtMs: Int64?
    public var endedAtMs: Int64
    public var reason: EndReason
    public var exitCode: Int32?
    public var detail: String?

    public init(id: String, resource: String, resourceId: ResourceId? = nil, kind: ClaimKind, workspacePath: String, command: String? = nil, grantedAtMs: Int64? = nil, endedAtMs: Int64, reason: EndReason, exitCode: Int32? = nil, detail: String? = nil) {
        self.id = id
        self.resource = resource
        self.resourceId = resourceId
        self.kind = kind
        self.workspacePath = workspacePath
        self.command = command
        self.grantedAtMs = grantedAtMs
        self.endedAtMs = endedAtMs
        self.reason = reason
        self.exitCode = exitCode
        self.detail = detail
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case resource
        case resourceId = "resource_id"
        case kind
        case workspacePath = "workspace_path"
        case command
        case grantedAtMs = "granted_at_ms"
        case endedAtMs = "ended_at_ms"
        case reason
        case exitCode = "exit_code"
        case detail
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.resource = try container.decode(String.self, forKey: .resource)
        self.resourceId = try container.decodeIfPresent(ResourceId.self, forKey: .resourceId)
        self.kind = try container.decode(ClaimKind.self, forKey: .kind)
        self.workspacePath = try container.decode(String.self, forKey: .workspacePath)
        self.command = try container.decodeIfPresent(String.self, forKey: .command)
        self.grantedAtMs = try container.decodeIfPresent(Int64.self, forKey: .grantedAtMs)
        self.endedAtMs = try container.decode(Int64.self, forKey: .endedAtMs)
        self.reason = try container.decode(EndReason.self, forKey: .reason)
        self.exitCode = try container.decodeIfPresent(Int32.self, forKey: .exitCode)
        self.detail = try container.decodeIfPresent(String.self, forKey: .detail)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.resourceId, forKey: .resourceId)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.workspacePath, forKey: .workspacePath)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.grantedAtMs, forKey: .grantedAtMs)
        try container.encode(self.endedAtMs, forKey: .endedAtMs)
        try container.encode(self.reason, forKey: .reason)
        try container.encode(self.exitCode, forKey: .exitCode)
        try container.encode(self.detail, forKey: .detail)
    }
}

public enum ClaimKind: String, Codable, Sendable, CaseIterable {
    case lease
    case hold
    case reservation
}

public enum ClaimOutcome: Codable, Sendable, Equatable {
    case granted(claim: Claim)
    case joined(claim: Claim)
    case reentrant(claimId: String)
    case queued(requestId: String, position: UInt32, of: UInt32, blockers: [Blocker])
    case busy(position: UInt32, of: UInt32, blockers: [Blocker])
    case finished(item: ClaimHistoryItem)

    private enum TagKeys: String, CodingKey {
        case tag = "outcome"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "granted":
            let payload = try _ClaimOutcomeGranted(from: decoder)
            self = .granted(claim: payload.claim)
        case "joined":
            let payload = try _ClaimOutcomeJoined(from: decoder)
            self = .joined(claim: payload.claim)
        case "reentrant":
            let payload = try _ClaimOutcomeReentrant(from: decoder)
            self = .reentrant(claimId: payload.claimId)
        case "queued":
            let payload = try _ClaimOutcomeQueued(from: decoder)
            self = .queued(requestId: payload.requestId, position: payload.position, of: payload.of, blockers: payload.blockers)
        case "busy":
            let payload = try _ClaimOutcomeBusy(from: decoder)
            self = .busy(position: payload.position, of: payload.of, blockers: payload.blockers)
        case "finished":
            let payload = try _ClaimOutcomeFinished(from: decoder)
            self = .finished(item: payload.item)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "ClaimOutcome")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .granted(let claim):
            try container.encode("granted", forKey: .tag)
            try _ClaimOutcomeGranted(claim: claim).encode(to: encoder)
        case .joined(let claim):
            try container.encode("joined", forKey: .tag)
            try _ClaimOutcomeJoined(claim: claim).encode(to: encoder)
        case .reentrant(let claimId):
            try container.encode("reentrant", forKey: .tag)
            try _ClaimOutcomeReentrant(claimId: claimId).encode(to: encoder)
        case .queued(let requestId, let position, let of, let blockers):
            try container.encode("queued", forKey: .tag)
            try _ClaimOutcomeQueued(requestId: requestId, position: position, of: of, blockers: blockers).encode(to: encoder)
        case .busy(let position, let of, let blockers):
            try container.encode("busy", forKey: .tag)
            try _ClaimOutcomeBusy(position: position, of: of, blockers: blockers).encode(to: encoder)
        case .finished(let item):
            try container.encode("finished", forKey: .tag)
            try _ClaimOutcomeFinished(item: item).encode(to: encoder)
        }
    }
}

public struct ClaimRelease: Codable, Sendable, Equatable {
    public var claimId: String
    public var workspaceId: String
    public var requester: ProcessIdentity
    public var exitCode: Int32?

    public init(claimId: String, workspaceId: String, requester: ProcessIdentity, exitCode: Int32? = nil) {
        self.claimId = claimId
        self.workspaceId = workspaceId
        self.requester = requester
        self.exitCode = exitCode
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case workspaceId = "workspace_id"
        case requester
        case exitCode = "exit_code"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
        self.exitCode = try container.decodeIfPresent(Int32.self, forKey: .exitCode)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.workspaceId, forKey: .workspaceId)
        try container.encode(self.requester, forKey: .requester)
        try container.encode(self.exitCode, forKey: .exitCode)
    }
}

public struct ClaimRequest: Codable, Sendable, Equatable {
    public var id: String
    public var selector: ClaimSelector
    public var workspaceId: String
    public var requester: ProcessIdentity
    public var purpose: Purpose
    public var reentrantTokens: [String]
    public var noWait: Bool

    public init(id: String, selector: ClaimSelector, workspaceId: String, requester: ProcessIdentity, purpose: Purpose, reentrantTokens: [String], noWait: Bool) {
        self.id = id
        self.selector = selector
        self.workspaceId = workspaceId
        self.requester = requester
        self.purpose = purpose
        self.reentrantTokens = reentrantTokens
        self.noWait = noWait
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case selector
        case workspaceId = "workspace_id"
        case requester
        case purpose
        case reentrantTokens = "reentrant_tokens"
        case noWait = "no_wait"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.selector = try container.decode(ClaimSelector.self, forKey: .selector)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
        self.purpose = try container.decode(Purpose.self, forKey: .purpose)
        self.reentrantTokens = try container.decode([String].self, forKey: .reentrantTokens)
        self.noWait = try container.decode(Bool.self, forKey: .noWait)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.selector, forKey: .selector)
        try container.encode(self.workspaceId, forKey: .workspaceId)
        try container.encode(self.requester, forKey: .requester)
        try container.encode(self.purpose, forKey: .purpose)
        try container.encode(self.reentrantTokens, forKey: .reentrantTokens)
        try container.encode(self.noWait, forKey: .noWait)
    }
}

public struct ClaimStarted: Codable, Sendable, Equatable {
    public var claimId: String
    public var child: ProcessIdentity
    public var group: RunGroup

    public init(claimId: String, child: ProcessIdentity, group: RunGroup) {
        self.claimId = claimId
        self.child = child
        self.group = group
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case child
        case group
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.child = try container.decode(ProcessIdentity.self, forKey: .child)
        self.group = try container.decode(RunGroup.self, forKey: .group)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.child, forKey: .child)
        try container.encode(self.group, forKey: .group)
    }
}

public struct ClaimSummary: Codable, Sendable, Equatable {
    public var id: String
    public var resource: String
    public var kind: ClaimKind
    public var workspacePath: String
    public var pid: UInt32?
    public var command: String?
    public var sinceMs: Int64
    public var expiresAtMs: Int64?
    public var linked: UInt32

    public init(id: String, resource: String, kind: ClaimKind, workspacePath: String, pid: UInt32? = nil, command: String? = nil, sinceMs: Int64, expiresAtMs: Int64? = nil, linked: UInt32) {
        self.id = id
        self.resource = resource
        self.kind = kind
        self.workspacePath = workspacePath
        self.pid = pid
        self.command = command
        self.sinceMs = sinceMs
        self.expiresAtMs = expiresAtMs
        self.linked = linked
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case resource
        case kind
        case workspacePath = "workspace_path"
        case pid
        case command
        case sinceMs = "since_ms"
        case expiresAtMs = "expires_at_ms"
        case linked
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.resource = try container.decode(String.self, forKey: .resource)
        self.kind = try container.decode(ClaimKind.self, forKey: .kind)
        self.workspacePath = try container.decode(String.self, forKey: .workspacePath)
        self.pid = try container.decodeIfPresent(UInt32.self, forKey: .pid)
        self.command = try container.decodeIfPresent(String.self, forKey: .command)
        self.sinceMs = try container.decode(Int64.self, forKey: .sinceMs)
        self.expiresAtMs = try container.decodeIfPresent(Int64.self, forKey: .expiresAtMs)
        self.linked = try container.decode(UInt32.self, forKey: .linked)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.workspacePath, forKey: .workspacePath)
        try container.encode(self.pid, forKey: .pid)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.sinceMs, forKey: .sinceMs)
        try container.encode(self.expiresAtMs, forKey: .expiresAtMs)
        try container.encode(self.linked, forKey: .linked)
    }
}

public struct ClaimsSnapshot: Codable, Sendable, Equatable {
    public var resources: [ResourceState]
    public var asOfSeq: UInt64

    public init(resources: [ResourceState], asOfSeq: UInt64) {
        self.resources = resources
        self.asOfSeq = asOfSeq
    }

    private enum CodingKeys: String, CodingKey {
        case resources
        case asOfSeq = "as_of_seq"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resources = try container.decode([ResourceState].self, forKey: .resources)
        self.asOfSeq = try container.decode(UInt64.self, forKey: .asOfSeq)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resources, forKey: .resources)
        try container.encode(self.asOfSeq, forKey: .asOfSeq)
    }
}

public struct DeclaredAccount: Codable, Sendable, Equatable {
    public var name: String
    public var purpose: String
    public var fields: [String]

    public init(name: String, purpose: String, fields: [String]) {
        self.name = name
        self.purpose = purpose
        self.fields = fields
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case purpose
        case fields
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.purpose = try container.decode(String.self, forKey: .purpose)
        self.fields = try container.decode([String].self, forKey: .fields)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.purpose, forKey: .purpose)
        try container.encode(self.fields, forKey: .fields)
    }
}

public struct DeclaredApps: Codable, Sendable, Equatable {
    public var android: [String]
    public var ios: [String]

    public init(android: [String], ios: [String]) {
        self.android = android
        self.ios = ios
    }

    private enum CodingKeys: String, CodingKey {
        case android
        case ios
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.android = try container.decode([String].self, forKey: .android)
        self.ios = try container.decode([String].self, forKey: .ios)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.android, forKey: .android)
        try container.encode(self.ios, forKey: .ios)
    }
}

public struct DeclaredSecret: Codable, Sendable, Equatable {
    public var name: String
    public var purpose: String
    public var env: String?

    public init(name: String, purpose: String, env: String? = nil) {
        self.name = name
        self.purpose = purpose
        self.env = env
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case purpose
        case env
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.purpose = try container.decode(String.self, forKey: .purpose)
        self.env = try container.decodeIfPresent(String.self, forKey: .env)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.purpose, forKey: .purpose)
        try container.encode(self.env, forKey: .env)
    }
}

public struct Decline: Codable, Sendable, Equatable {
    public var id: String?
    public var project: ProjectRef?
    public var name: String?

    public init(id: String? = nil, project: ProjectRef? = nil, name: String? = nil) {
        self.id = id
        self.project = project
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case project
        case name
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decodeIfPresent(String.self, forKey: .id)
        self.project = try container.decodeIfPresent(ProjectRef.self, forKey: .project)
        self.name = try container.decodeIfPresent(String.self, forKey: .name)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
    }
}

public struct DeviceDetail: Codable, Sendable, Equatable {
    public var info: DeviceInfo
    public var apps: [AppState]
    public var snapshotAtMs: Int64?
    public var forgets: [ForgetRecord]
    public var sessionsUnknown: String?

    public init(info: DeviceInfo, apps: [AppState], snapshotAtMs: Int64? = nil, forgets: [ForgetRecord] = [], sessionsUnknown: String? = nil) {
        self.info = info
        self.apps = apps
        self.snapshotAtMs = snapshotAtMs
        self.forgets = forgets
        self.sessionsUnknown = sessionsUnknown
    }

    private enum CodingKeys: String, CodingKey {
        case info
        case apps
        case snapshotAtMs = "snapshot_at_ms"
        case forgets
        case sessionsUnknown = "sessions_unknown"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.info = try container.decode(DeviceInfo.self, forKey: .info)
        self.apps = try container.decode([AppState].self, forKey: .apps)
        self.snapshotAtMs = try container.decodeIfPresent(Int64.self, forKey: .snapshotAtMs)
        self.forgets = try container.decodeIfPresent([ForgetRecord].self, forKey: .forgets) ?? []
        self.sessionsUnknown = try container.decodeIfPresent(String.self, forKey: .sessionsUnknown)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.info, forKey: .info)
        try container.encode(self.apps, forKey: .apps)
        try container.encode(self.snapshotAtMs, forKey: .snapshotAtMs)
        try container.encode(self.forgets, forKey: .forgets)
        try container.encodeIfPresent(self.sessionsUnknown, forKey: .sessionsUnknown)
    }
}

public struct DeviceInfo: Codable, Sendable, Equatable {
    public var resource: ResourceId
    public var hardwareId: String
    public var platform: Platform
    public var kind: DeviceKind
    public var registered: Bool
    public var alias: String?
    public var aliasByPerson: Bool
    public var model: String
    public var name: String?
    public var osVersion: String?
    public var note: String?
    public var connected: Bool
    public var ready: Bool
    public var transports: [Transport]
    public var coredeviceId: String?
    public var telemetry: Telemetry?
    public var bootedByHostler: Bool
    public var shuttingDown: Bool
    public var removing: Bool
    public var sessions: [AgentDeviceSession]
    public var checkingSessions: Bool
    public var firstSeenMs: Int64
    public var lastSeenMs: Int64

    public init(resource: ResourceId, hardwareId: String, platform: Platform, kind: DeviceKind, registered: Bool, alias: String? = nil, aliasByPerson: Bool, model: String, name: String? = nil, osVersion: String? = nil, note: String? = nil, connected: Bool, ready: Bool, transports: [Transport], coredeviceId: String? = nil, telemetry: Telemetry? = nil, bootedByHostler: Bool, shuttingDown: Bool, removing: Bool, sessions: [AgentDeviceSession], checkingSessions: Bool, firstSeenMs: Int64, lastSeenMs: Int64) {
        self.resource = resource
        self.hardwareId = hardwareId
        self.platform = platform
        self.kind = kind
        self.registered = registered
        self.alias = alias
        self.aliasByPerson = aliasByPerson
        self.model = model
        self.name = name
        self.osVersion = osVersion
        self.note = note
        self.connected = connected
        self.ready = ready
        self.transports = transports
        self.coredeviceId = coredeviceId
        self.telemetry = telemetry
        self.bootedByHostler = bootedByHostler
        self.shuttingDown = shuttingDown
        self.removing = removing
        self.sessions = sessions
        self.checkingSessions = checkingSessions
        self.firstSeenMs = firstSeenMs
        self.lastSeenMs = lastSeenMs
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case hardwareId = "hardware_id"
        case platform
        case kind
        case registered
        case alias
        case aliasByPerson = "alias_by_person"
        case model
        case name
        case osVersion = "os_version"
        case note
        case connected
        case ready
        case transports
        case coredeviceId = "coredevice_id"
        case telemetry
        case bootedByHostler = "booted_by_hostler"
        case shuttingDown = "shutting_down"
        case removing
        case sessions
        case checkingSessions = "checking_sessions"
        case firstSeenMs = "first_seen_ms"
        case lastSeenMs = "last_seen_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.hardwareId = try container.decode(String.self, forKey: .hardwareId)
        self.platform = try container.decode(Platform.self, forKey: .platform)
        self.kind = try container.decode(DeviceKind.self, forKey: .kind)
        self.registered = try container.decode(Bool.self, forKey: .registered)
        self.alias = try container.decodeIfPresent(String.self, forKey: .alias)
        self.aliasByPerson = try container.decode(Bool.self, forKey: .aliasByPerson)
        self.model = try container.decode(String.self, forKey: .model)
        self.name = try container.decodeIfPresent(String.self, forKey: .name)
        self.osVersion = try container.decodeIfPresent(String.self, forKey: .osVersion)
        self.note = try container.decodeIfPresent(String.self, forKey: .note)
        self.connected = try container.decode(Bool.self, forKey: .connected)
        self.ready = try container.decode(Bool.self, forKey: .ready)
        self.transports = try container.decode([Transport].self, forKey: .transports)
        self.coredeviceId = try container.decodeIfPresent(String.self, forKey: .coredeviceId)
        self.telemetry = try container.decodeIfPresent(Telemetry.self, forKey: .telemetry)
        self.bootedByHostler = try container.decode(Bool.self, forKey: .bootedByHostler)
        self.shuttingDown = try container.decode(Bool.self, forKey: .shuttingDown)
        self.removing = try container.decode(Bool.self, forKey: .removing)
        self.sessions = try container.decode([AgentDeviceSession].self, forKey: .sessions)
        self.checkingSessions = try container.decode(Bool.self, forKey: .checkingSessions)
        self.firstSeenMs = try container.decode(Int64.self, forKey: .firstSeenMs)
        self.lastSeenMs = try container.decode(Int64.self, forKey: .lastSeenMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.hardwareId, forKey: .hardwareId)
        try container.encode(self.platform, forKey: .platform)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.registered, forKey: .registered)
        try container.encode(self.alias, forKey: .alias)
        try container.encode(self.aliasByPerson, forKey: .aliasByPerson)
        try container.encode(self.model, forKey: .model)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.osVersion, forKey: .osVersion)
        try container.encode(self.note, forKey: .note)
        try container.encode(self.connected, forKey: .connected)
        try container.encode(self.ready, forKey: .ready)
        try container.encode(self.transports, forKey: .transports)
        try container.encode(self.coredeviceId, forKey: .coredeviceId)
        try container.encode(self.telemetry, forKey: .telemetry)
        try container.encode(self.bootedByHostler, forKey: .bootedByHostler)
        try container.encode(self.shuttingDown, forKey: .shuttingDown)
        try container.encode(self.removing, forKey: .removing)
        try container.encode(self.sessions, forKey: .sessions)
        try container.encode(self.checkingSessions, forKey: .checkingSessions)
        try container.encode(self.firstSeenMs, forKey: .firstSeenMs)
        try container.encode(self.lastSeenMs, forKey: .lastSeenMs)
    }
}

public enum DeviceKind: String, Codable, Sendable, CaseIterable {
    case physical
    case emulator
    case simulator
}

public struct DeviceRemoved: Codable, Sendable, Equatable {
    public var removed: ResourceId

    public init(removed: ResourceId) {
        self.removed = removed
    }

    private enum CodingKeys: String, CodingKey {
        case removed
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.removed = try container.decode(ResourceId.self, forKey: .removed)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.removed, forKey: .removed)
    }
}

public struct DevicesSnapshot: Codable, Sendable, Equatable {
    public var devices: [DeviceInfo]
    public var unidentified: [UnidentifiedTransport]
    public var unmappedSessions: [String]
    public var tools: ToolStatus
    public var atMs: Int64
    public var asOfSeq: UInt64

    public init(devices: [DeviceInfo], unidentified: [UnidentifiedTransport], unmappedSessions: [String], tools: ToolStatus, atMs: Int64, asOfSeq: UInt64) {
        self.devices = devices
        self.unidentified = unidentified
        self.unmappedSessions = unmappedSessions
        self.tools = tools
        self.atMs = atMs
        self.asOfSeq = asOfSeq
    }

    private enum CodingKeys: String, CodingKey {
        case devices
        case unidentified
        case unmappedSessions = "unmapped_sessions"
        case tools
        case atMs = "at_ms"
        case asOfSeq = "as_of_seq"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.devices = try container.decode([DeviceInfo].self, forKey: .devices)
        self.unidentified = try container.decode([UnidentifiedTransport].self, forKey: .unidentified)
        self.unmappedSessions = try container.decode([String].self, forKey: .unmappedSessions)
        self.tools = try container.decode(ToolStatus.self, forKey: .tools)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
        self.asOfSeq = try container.decode(UInt64.self, forKey: .asOfSeq)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.devices, forKey: .devices)
        try container.encode(self.unidentified, forKey: .unidentified)
        try container.encode(self.unmappedSessions, forKey: .unmappedSessions)
        try container.encode(self.tools, forKey: .tools)
        try container.encode(self.atMs, forKey: .atMs)
        try container.encode(self.asOfSeq, forKey: .asOfSeq)
    }
}

public struct DoctorReport: Codable, Sendable, Equatable {
    public var checks: [Check]
    public var fixed: [FixOutcome]

    public init(checks: [Check], fixed: [FixOutcome]) {
        self.checks = checks
        self.fixed = fixed
    }

    private enum CodingKeys: String, CodingKey {
        case checks
        case fixed
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.checks = try container.decode([Check].self, forKey: .checks)
        self.fixed = try container.decode([FixOutcome].self, forKey: .fixed)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.checks, forKey: .checks)
        try container.encode(self.fixed, forKey: .fixed)
    }
}

public enum EndReason: String, Codable, Sendable, CaseIterable {
    case released
    case expired
    case reaped
    case stranded
    case seized
    case withdrawn
    case timedOut = "timed_out"
    case deadlock
    case refused
}

public enum EntrySource: String, Codable, Sendable, CaseIterable {
    case hostler
    case rnLookyLedger = "rn_looky_ledger"
}

public struct Envelope<T: Codable & Sendable & Equatable>: Codable, Sendable, Equatable {
    public var ok: Bool
    public var data: T?
    public var error: HostlerError?

    public init(ok: Bool, data: T? = nil, error: HostlerError? = nil) {
        self.ok = ok
        self.data = data
        self.error = error
    }

    private enum CodingKeys: String, CodingKey {
        case ok
        case data
        case error
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.ok = try container.decode(Bool.self, forKey: .ok)
        self.data = try container.decodeIfPresent(T.self, forKey: .data)
        self.error = try container.decodeIfPresent(HostlerError.self, forKey: .error)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.ok, forKey: .ok)
        try container.encode(self.data, forKey: .data)
        try container.encode(self.error, forKey: .error)
    }
}

public enum ErrorCode: String, Codable, Sendable, CaseIterable {
    case serviceUnavailable = "service_unavailable"
    case contractMismatch = "contract_mismatch"
    case serviceBusy = "service_busy"
    case badRequest = "bad_request"
    case unknownEntry = "unknown_entry"
    case queueTimeout = "queue_timeout"
    case queueBusy = "queue_busy"
    case withdrawn
    case capacityOutOfRange = "capacity_out_of_range"
    case forceRequired = "force_required"
    case `internal`
    case notInteractive = "not_interactive"
    case notInProject = "not_in_project"
    case configInvalid = "config_invalid"
    case projectNotApproved = "project_not_approved"
    case secretsLocked = "secrets_locked"
    case unlockFailed = "unlock_failed"
    case secretsConflict = "secrets_conflict"
    case secretMissing = "secret_missing"
    case secretNotInProject = "secret_not_in_project"
    case secretExists = "secret_exists"
    case secretTooShort = "secret_too_short"
    case requestTimeout = "request_timeout"
    case unknownResource = "unknown_resource"
    case resourceInUse = "resource_in_use"
    case claimNotHeld = "claim_not_held"
    case seizureFailed = "seizure_failed"
    case accountMissing = "account_missing"
    case claimDeadlock = "claim_deadlock"
    case takenOver = "taken_over"
    case deviceNotFound = "device_not_found"
    case aliasTaken = "alias_taken"
    case deviceNotRegistered = "device_not_registered"
    case deviceNotReady = "device_not_ready"
    case bootFailed = "boot_failed"
    case toolMissing = "tool_missing"
    case permissionDenied = "permission_denied"
    case proofRequired = "proof_required"
    case proofLimited = "proof_limited"
    case stopFailed = "stop_failed"
}

public enum Event: Codable, Sendable, Equatable {
    case queueChanged(QueueSnapshot)
    case secretsRequestChanged(SecretRequestInfo)
    case secretsLockChanged(SecretsStatus)
    case claimsChanged(ClaimsSnapshot)
    case claimsSeized(SeizureRecord)
    case devicesChanged(DevicesSnapshot)
    case devicesInstalled(InstallEvent)
    case projectsChanged(ProjectsSnapshot)
    case accountsRequestChanged(AccountRequestInfo)
    case resync(reason: ResyncReason)

    private enum TagKeys: String, CodingKey {
        case tag = "type"
        case content = "data"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "queue.changed":
            self = .queueChanged(try container.decode(QueueSnapshot.self, forKey: .content))
        case "secrets.request_changed":
            self = .secretsRequestChanged(try container.decode(SecretRequestInfo.self, forKey: .content))
        case "secrets.lock_changed":
            self = .secretsLockChanged(try container.decode(SecretsStatus.self, forKey: .content))
        case "claims.changed":
            self = .claimsChanged(try container.decode(ClaimsSnapshot.self, forKey: .content))
        case "claims.seized":
            self = .claimsSeized(try container.decode(SeizureRecord.self, forKey: .content))
        case "devices.changed":
            self = .devicesChanged(try container.decode(DevicesSnapshot.self, forKey: .content))
        case "devices.installed":
            self = .devicesInstalled(try container.decode(InstallEvent.self, forKey: .content))
        case "projects.changed":
            self = .projectsChanged(try container.decode(ProjectsSnapshot.self, forKey: .content))
        case "accounts.request_changed":
            self = .accountsRequestChanged(try container.decode(AccountRequestInfo.self, forKey: .content))
        case "resync":
            let payload = try container.decode(_EventResync.self, forKey: .content)
            self = .resync(reason: payload.reason)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "Event")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .queueChanged(let value):
            try container.encode("queue.changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretsRequestChanged(let value):
            try container.encode("secrets.request_changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretsLockChanged(let value):
            try container.encode("secrets.lock_changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsChanged(let value):
            try container.encode("claims.changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsSeized(let value):
            try container.encode("claims.seized", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .devicesChanged(let value):
            try container.encode("devices.changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .devicesInstalled(let value):
            try container.encode("devices.installed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .projectsChanged(let value):
            try container.encode("projects.changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .accountsRequestChanged(let value):
            try container.encode("accounts.request_changed", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .resync(let reason):
            try container.encode("resync", forKey: .tag)
            try container.encode(_EventResync(reason: reason), forKey: .content)
        }
    }
}

public struct Fix: Codable, Sendable, Equatable {
    public var command: String?
    public var text: String
    public var who: Who
    public var safe: Bool

    public init(command: String? = nil, text: String, who: Who, safe: Bool) {
        self.command = command
        self.text = text
        self.who = who
        self.safe = safe
    }

    private enum CodingKeys: String, CodingKey {
        case command
        case text
        case who
        case safe
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.command = try container.decodeIfPresent(String.self, forKey: .command)
        self.text = try container.decode(String.self, forKey: .text)
        self.who = try container.decode(Who.self, forKey: .who)
        self.safe = try container.decode(Bool.self, forKey: .safe)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.text, forKey: .text)
        try container.encode(self.who, forKey: .who)
        try container.encode(self.safe, forKey: .safe)
    }
}

public struct FixOutcome: Codable, Sendable, Equatable {
    public var check: String
    public var command: String
    public var result: FixResult
    public var message: String

    public init(check: String, command: String, result: FixResult, message: String) {
        self.check = check
        self.command = command
        self.result = result
        self.message = message
    }

    private enum CodingKeys: String, CodingKey {
        case check
        case command
        case result
        case message
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.check = try container.decode(String.self, forKey: .check)
        self.command = try container.decode(String.self, forKey: .command)
        self.result = try container.decode(FixResult.self, forKey: .result)
        self.message = try container.decode(String.self, forKey: .message)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.check, forKey: .check)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.result, forKey: .result)
        try container.encode(self.message, forKey: .message)
    }
}

public enum FixResult: String, Codable, Sendable, CaseIterable {
    case fixed
    case stillFailing = "still_failing"
    case refused
}

public struct ForeignClaim: Codable, Sendable, Equatable {
    public var resource: ResourceId
    public var source: String
    public var description: String
    public var workspacePath: String?
    public var sinceMs: Int64?
    public var stop: JSONValue
    public var workspaceId: String?

    public init(resource: ResourceId, source: String, description: String, workspacePath: String? = nil, sinceMs: Int64? = nil, stop: JSONValue = .null, workspaceId: String? = nil) {
        self.resource = resource
        self.source = source
        self.description = description
        self.workspacePath = workspacePath
        self.sinceMs = sinceMs
        self.stop = stop
        self.workspaceId = workspaceId
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case source
        case description
        case workspacePath = "workspace_path"
        case sinceMs = "since_ms"
        case stop
        case workspaceId = "workspace_id"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.source = try container.decode(String.self, forKey: .source)
        self.description = try container.decode(String.self, forKey: .description)
        self.workspacePath = try container.decodeIfPresent(String.self, forKey: .workspacePath)
        self.sinceMs = try container.decodeIfPresent(Int64.self, forKey: .sinceMs)
        self.stop = try container.decodeIfPresent(JSONValue.self, forKey: .stop) ?? .null
        self.workspaceId = try container.decodeIfPresent(String.self, forKey: .workspaceId)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.source, forKey: .source)
        try container.encode(self.description, forKey: .description)
        try container.encode(self.workspacePath, forKey: .workspacePath)
        try container.encode(self.sinceMs, forKey: .sinceMs)
        try container.encode(self.stop, forKey: .stop)
        try container.encode(self.workspaceId, forKey: .workspaceId)
    }
}

public struct ForgetRecord: Codable, Sendable, Equatable {
    public var id: String
    public var resource: ResourceId
    public var sessions: [String]
    public var byWorkspacePath: String?
    public var byPid: UInt32?
    public var reason: String
    public var atMs: Int64

    public init(id: String, resource: ResourceId, sessions: [String], byWorkspacePath: String? = nil, byPid: UInt32? = nil, reason: String, atMs: Int64) {
        self.id = id
        self.resource = resource
        self.sessions = sessions
        self.byWorkspacePath = byWorkspacePath
        self.byPid = byPid
        self.reason = reason
        self.atMs = atMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case resource
        case sessions
        case byWorkspacePath = "by_workspace_path"
        case byPid = "by_pid"
        case reason
        case atMs = "at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.sessions = try container.decode([String].self, forKey: .sessions)
        self.byWorkspacePath = try container.decodeIfPresent(String.self, forKey: .byWorkspacePath)
        self.byPid = try container.decodeIfPresent(UInt32.self, forKey: .byPid)
        self.reason = try container.decode(String.self, forKey: .reason)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.sessions, forKey: .sessions)
        try container.encode(self.byWorkspacePath, forKey: .byWorkspacePath)
        try container.encode(self.byPid, forKey: .byPid)
        try container.encode(self.reason, forKey: .reason)
        try container.encode(self.atMs, forKey: .atMs)
    }
}

public struct HeavyAcquire: Codable, Sendable, Equatable {
    public var id: String
    public var kind: String
    public var command: String
    public var cwd: String
    public var workspaceRoot: String?
    public var holder: ProcessIdentity
    public var reentrantToken: String?
    public var noWait: Bool

    public init(id: String, kind: String, command: String, cwd: String, workspaceRoot: String? = nil, holder: ProcessIdentity, reentrantToken: String? = nil, noWait: Bool) {
        self.id = id
        self.kind = kind
        self.command = command
        self.cwd = cwd
        self.workspaceRoot = workspaceRoot
        self.holder = holder
        self.reentrantToken = reentrantToken
        self.noWait = noWait
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case kind
        case command
        case cwd
        case workspaceRoot = "workspace_root"
        case holder
        case reentrantToken = "reentrant_token"
        case noWait = "no_wait"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.kind = try container.decode(String.self, forKey: .kind)
        self.command = try container.decode(String.self, forKey: .command)
        self.cwd = try container.decode(String.self, forKey: .cwd)
        self.workspaceRoot = try container.decodeIfPresent(String.self, forKey: .workspaceRoot)
        self.holder = try container.decode(ProcessIdentity.self, forKey: .holder)
        self.reentrantToken = try container.decodeIfPresent(String.self, forKey: .reentrantToken)
        self.noWait = try container.decode(Bool.self, forKey: .noWait)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.cwd, forKey: .cwd)
        try container.encode(self.workspaceRoot, forKey: .workspaceRoot)
        try container.encode(self.holder, forKey: .holder)
        try container.encode(self.reentrantToken, forKey: .reentrantToken)
        try container.encode(self.noWait, forKey: .noWait)
    }
}

public struct HeavyEntry: Codable, Sendable, Equatable {
    public var id: String
    public var ticket: UInt64
    public var kind: String
    public var command: String
    public var cwd: String
    public var workspaceRoot: String?
    public var holder: ProcessIdentity
    public var child: ProcessIdentity?
    public var group: RunGroup?
    public var state: HeavyState
    public var enqueuedAtMs: Int64
    public var grantedAtMs: Int64?
    public var source: EntrySource

    public init(id: String, ticket: UInt64, kind: String, command: String, cwd: String, workspaceRoot: String? = nil, holder: ProcessIdentity, child: ProcessIdentity? = nil, group: RunGroup? = nil, state: HeavyState, enqueuedAtMs: Int64, grantedAtMs: Int64? = nil, source: EntrySource) {
        self.id = id
        self.ticket = ticket
        self.kind = kind
        self.command = command
        self.cwd = cwd
        self.workspaceRoot = workspaceRoot
        self.holder = holder
        self.child = child
        self.group = group
        self.state = state
        self.enqueuedAtMs = enqueuedAtMs
        self.grantedAtMs = grantedAtMs
        self.source = source
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case ticket
        case kind
        case command
        case cwd
        case workspaceRoot = "workspace_root"
        case holder
        case child
        case group
        case state
        case enqueuedAtMs = "enqueued_at_ms"
        case grantedAtMs = "granted_at_ms"
        case source
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.ticket = try container.decode(UInt64.self, forKey: .ticket)
        self.kind = try container.decode(String.self, forKey: .kind)
        self.command = try container.decode(String.self, forKey: .command)
        self.cwd = try container.decode(String.self, forKey: .cwd)
        self.workspaceRoot = try container.decodeIfPresent(String.self, forKey: .workspaceRoot)
        self.holder = try container.decode(ProcessIdentity.self, forKey: .holder)
        self.child = try container.decodeIfPresent(ProcessIdentity.self, forKey: .child)
        self.group = try container.decodeIfPresent(RunGroup.self, forKey: .group)
        self.state = try container.decode(HeavyState.self, forKey: .state)
        self.enqueuedAtMs = try container.decode(Int64.self, forKey: .enqueuedAtMs)
        self.grantedAtMs = try container.decodeIfPresent(Int64.self, forKey: .grantedAtMs)
        self.source = try container.decode(EntrySource.self, forKey: .source)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.ticket, forKey: .ticket)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.cwd, forKey: .cwd)
        try container.encode(self.workspaceRoot, forKey: .workspaceRoot)
        try container.encode(self.holder, forKey: .holder)
        try container.encode(self.child, forKey: .child)
        try container.encode(self.group, forKey: .group)
        try container.encode(self.state, forKey: .state)
        try container.encode(self.enqueuedAtMs, forKey: .enqueuedAtMs)
        try container.encode(self.grantedAtMs, forKey: .grantedAtMs)
        try container.encode(self.source, forKey: .source)
    }
}

public struct HeavyHistoryItem: Codable, Sendable, Equatable {
    public var id: String
    public var ticket: UInt64
    public var kind: String
    public var command: String
    public var cwd: String
    public var workspaceRoot: String?
    public var enqueuedAtMs: Int64
    public var grantedAtMs: Int64?
    public var finishedAtMs: Int64
    public var exitCode: Int32?
    public var outcome: HeavyOutcome

    public init(id: String, ticket: UInt64, kind: String, command: String, cwd: String, workspaceRoot: String? = nil, enqueuedAtMs: Int64, grantedAtMs: Int64? = nil, finishedAtMs: Int64, exitCode: Int32? = nil, outcome: HeavyOutcome) {
        self.id = id
        self.ticket = ticket
        self.kind = kind
        self.command = command
        self.cwd = cwd
        self.workspaceRoot = workspaceRoot
        self.enqueuedAtMs = enqueuedAtMs
        self.grantedAtMs = grantedAtMs
        self.finishedAtMs = finishedAtMs
        self.exitCode = exitCode
        self.outcome = outcome
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case ticket
        case kind
        case command
        case cwd
        case workspaceRoot = "workspace_root"
        case enqueuedAtMs = "enqueued_at_ms"
        case grantedAtMs = "granted_at_ms"
        case finishedAtMs = "finished_at_ms"
        case exitCode = "exit_code"
        case outcome
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.ticket = try container.decode(UInt64.self, forKey: .ticket)
        self.kind = try container.decode(String.self, forKey: .kind)
        self.command = try container.decode(String.self, forKey: .command)
        self.cwd = try container.decode(String.self, forKey: .cwd)
        self.workspaceRoot = try container.decodeIfPresent(String.self, forKey: .workspaceRoot)
        self.enqueuedAtMs = try container.decode(Int64.self, forKey: .enqueuedAtMs)
        self.grantedAtMs = try container.decodeIfPresent(Int64.self, forKey: .grantedAtMs)
        self.finishedAtMs = try container.decode(Int64.self, forKey: .finishedAtMs)
        self.exitCode = try container.decodeIfPresent(Int32.self, forKey: .exitCode)
        self.outcome = try container.decode(HeavyOutcome.self, forKey: .outcome)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.ticket, forKey: .ticket)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.cwd, forKey: .cwd)
        try container.encode(self.workspaceRoot, forKey: .workspaceRoot)
        try container.encode(self.enqueuedAtMs, forKey: .enqueuedAtMs)
        try container.encode(self.grantedAtMs, forKey: .grantedAtMs)
        try container.encode(self.finishedAtMs, forKey: .finishedAtMs)
        try container.encode(self.exitCode, forKey: .exitCode)
        try container.encode(self.outcome, forKey: .outcome)
    }
}

public enum HeavyOutcome: String, Codable, Sendable, CaseIterable {
    case ok
    case failed
    case withdrawn
    case timedOut = "timed_out"
    case reaped
    case stopped
    case forceReleased = "force_released"
}

public struct HeavyRelease: Codable, Sendable, Equatable {
    public var id: String
    public var exitCode: Int32?
    public var outcome: HeavyOutcome

    public init(id: String, exitCode: Int32? = nil, outcome: HeavyOutcome) {
        self.id = id
        self.exitCode = exitCode
        self.outcome = outcome
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case exitCode = "exit_code"
        case outcome
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.exitCode = try container.decodeIfPresent(Int32.self, forKey: .exitCode)
        self.outcome = try container.decode(HeavyOutcome.self, forKey: .outcome)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.exitCode, forKey: .exitCode)
        try container.encode(self.outcome, forKey: .outcome)
    }
}

public struct HeavyStarted: Codable, Sendable, Equatable {
    public var id: String
    public var child: ProcessIdentity
    public var group: RunGroup

    public init(id: String, child: ProcessIdentity, group: RunGroup) {
        self.id = id
        self.child = child
        self.group = group
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case child
        case group
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.child = try container.decode(ProcessIdentity.self, forKey: .child)
        self.group = try container.decode(RunGroup.self, forKey: .group)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.child, forKey: .child)
        try container.encode(self.group, forKey: .group)
    }
}

public enum HeavyState: String, Codable, Sendable, CaseIterable {
    case waiting
    case running
    case stopping
}

public struct HelloReply: Codable, Sendable, Equatable {
    public var contractVersion: UInt32
    public var serviceVersion: String
    public var bootId: String?
    public var instance: String?

    public init(contractVersion: UInt32, serviceVersion: String, bootId: String? = nil, instance: String? = nil) {
        self.contractVersion = contractVersion
        self.serviceVersion = serviceVersion
        self.bootId = bootId
        self.instance = instance
    }

    private enum CodingKeys: String, CodingKey {
        case contractVersion = "contract_version"
        case serviceVersion = "service_version"
        case bootId = "boot_id"
        case instance
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.contractVersion = try container.decode(UInt32.self, forKey: .contractVersion)
        self.serviceVersion = try container.decode(String.self, forKey: .serviceVersion)
        self.bootId = try container.decodeIfPresent(String.self, forKey: .bootId)
        self.instance = try container.decodeIfPresent(String.self, forKey: .instance)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.contractVersion, forKey: .contractVersion)
        try container.encode(self.serviceVersion, forKey: .serviceVersion)
        try container.encodeIfPresent(self.bootId, forKey: .bootId)
        try container.encodeIfPresent(self.instance, forKey: .instance)
    }
}

public struct HistoryQuery: Codable, Sendable, Equatable {
    public var sinceMs: Int64?
    public var limit: UInt32

    public init(sinceMs: Int64? = nil, limit: UInt32 = 100) {
        self.sinceMs = sinceMs
        self.limit = limit
    }

    private enum CodingKeys: String, CodingKey {
        case sinceMs = "since_ms"
        case limit
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.sinceMs = try container.decodeIfPresent(Int64.self, forKey: .sinceMs)
        self.limit = try container.decodeIfPresent(UInt32.self, forKey: .limit) ?? 100
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.sinceMs, forKey: .sinceMs)
        try container.encode(self.limit, forKey: .limit)
    }
}

public enum HoldBinding: Codable, Sendable, Equatable {
    case ttl(ttlMs: UInt64, expiresAtMs: Int64)
    case process(holder: ProcessIdentity)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "ttl":
            let payload = try _HoldBindingTtl(from: decoder)
            self = .ttl(ttlMs: payload.ttlMs, expiresAtMs: payload.expiresAtMs)
        case "process":
            let payload = try _HoldBindingProcess(from: decoder)
            self = .process(holder: payload.holder)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "HoldBinding")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .ttl(let ttlMs, let expiresAtMs):
            try container.encode("ttl", forKey: .tag)
            try _HoldBindingTtl(ttlMs: ttlMs, expiresAtMs: expiresAtMs).encode(to: encoder)
        case .process(let holder):
            try container.encode("process", forKey: .tag)
            try _HoldBindingProcess(holder: holder).encode(to: encoder)
        }
    }
}

public struct HostlerError: Codable, Sendable, Equatable {
    public var code: ErrorCode
    public var message: String
    public var details: JSONValue
    public var recovery: String?

    public init(code: ErrorCode, message: String, details: JSONValue, recovery: String? = nil) {
        self.code = code
        self.message = message
        self.details = details
        self.recovery = recovery
    }

    private enum CodingKeys: String, CodingKey {
        case code
        case message
        case details
        case recovery
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.code = try container.decode(ErrorCode.self, forKey: .code)
        self.message = try container.decode(String.self, forKey: .message)
        self.details = try container.decode(JSONValue.self, forKey: .details)
        self.recovery = try container.decodeIfPresent(String.self, forKey: .recovery)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.code, forKey: .code)
        try container.encode(self.message, forKey: .message)
        try container.encode(self.details, forKey: .details)
        try container.encode(self.recovery, forKey: .recovery)
    }
}

public struct ImportEntry: Codable, Sendable, Equatable {
    public var name: String
    public var value: SecretText

    public init(name: String, value: SecretText) {
        self.name = name
        self.value = value
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case value
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.value = try container.decode(SecretText.self, forKey: .value)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.value, forKey: .value)
    }
}

public struct ImportOutcome: Codable, Sendable, Equatable {
    public var stored: [String]
    public var answeredRequests: [String]

    public init(stored: [String], answeredRequests: [String]) {
        self.stored = stored
        self.answeredRequests = answeredRequests
    }

    private enum CodingKeys: String, CodingKey {
        case stored
        case answeredRequests = "answered_requests"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.stored = try container.decode([String].self, forKey: .stored)
        self.answeredRequests = try container.decode([String].self, forKey: .answeredRequests)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.stored, forKey: .stored)
        try container.encode(self.answeredRequests, forKey: .answeredRequests)
    }
}

public enum InstallChange: String, Codable, Sendable, CaseIterable {
    case installed
    case updated
    case removed
}

public struct InstallEvent: Codable, Sendable, Equatable {
    public var id: String
    public var device: ResourceId
    public var alias: String?
    public var appId: String
    public var change: InstallChange
    public var versionName: String?
    public var versionCode: String?
    public var when: InstallTime
    public var source: InstallSource
    public var recordedAtMs: Int64

    public init(id: String, device: ResourceId, alias: String? = nil, appId: String, change: InstallChange, versionName: String? = nil, versionCode: String? = nil, when: InstallTime, source: InstallSource, recordedAtMs: Int64) {
        self.id = id
        self.device = device
        self.alias = alias
        self.appId = appId
        self.change = change
        self.versionName = versionName
        self.versionCode = versionCode
        self.when = when
        self.source = source
        self.recordedAtMs = recordedAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case device
        case alias
        case appId = "app_id"
        case change
        case versionName = "version_name"
        case versionCode = "version_code"
        case when
        case source
        case recordedAtMs = "recorded_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.device = try container.decode(ResourceId.self, forKey: .device)
        self.alias = try container.decodeIfPresent(String.self, forKey: .alias)
        self.appId = try container.decode(String.self, forKey: .appId)
        self.change = try container.decode(InstallChange.self, forKey: .change)
        self.versionName = try container.decodeIfPresent(String.self, forKey: .versionName)
        self.versionCode = try container.decodeIfPresent(String.self, forKey: .versionCode)
        self.when = try container.decode(InstallTime.self, forKey: .when)
        self.source = try container.decode(InstallSource.self, forKey: .source)
        self.recordedAtMs = try container.decode(Int64.self, forKey: .recordedAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.alias, forKey: .alias)
        try container.encode(self.appId, forKey: .appId)
        try container.encode(self.change, forKey: .change)
        try container.encode(self.versionName, forKey: .versionName)
        try container.encode(self.versionCode, forKey: .versionCode)
        try container.encode(self.when, forKey: .when)
        try container.encode(self.source, forKey: .source)
        try container.encode(self.recordedAtMs, forKey: .recordedAtMs)
    }
}

public enum InstallSource: Codable, Sendable, Equatable {
    case claim(claimIds: [String], workspacePath: String, branch: String?, commit: String?, command: String?)
    case around(claimIds: [String], workspacePath: String)
    case notThroughHostler

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "claim":
            let payload = try _InstallSourceClaim(from: decoder)
            self = .claim(claimIds: payload.claimIds, workspacePath: payload.workspacePath, branch: payload.branch, commit: payload.commit, command: payload.command)
        case "around":
            let payload = try _InstallSourceAround(from: decoder)
            self = .around(claimIds: payload.claimIds, workspacePath: payload.workspacePath)
        case "not_through_hostler":
            self = .notThroughHostler
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "InstallSource")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .claim(let claimIds, let workspacePath, let branch, let commit, let command):
            try container.encode("claim", forKey: .tag)
            try _InstallSourceClaim(claimIds: claimIds, workspacePath: workspacePath, branch: branch, commit: commit, command: command).encode(to: encoder)
        case .around(let claimIds, let workspacePath):
            try container.encode("around", forKey: .tag)
            try _InstallSourceAround(claimIds: claimIds, workspacePath: workspacePath).encode(to: encoder)
        case .notThroughHostler:
            try container.encode("not_through_hostler", forKey: .tag)
        }
    }
}

public enum InstallTime: Codable, Sendable, Equatable {
    case device(atMs: Int64)
    case firstSeen(atMs: Int64)
    case changedBetween(fromMs: Int64, toMs: Int64)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "device":
            let payload = try _InstallTimeDevice(from: decoder)
            self = .device(atMs: payload.atMs)
        case "first_seen":
            let payload = try _InstallTimeFirstSeen(from: decoder)
            self = .firstSeen(atMs: payload.atMs)
        case "changed_between":
            let payload = try _InstallTimeChangedBetween(from: decoder)
            self = .changedBetween(fromMs: payload.fromMs, toMs: payload.toMs)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "InstallTime")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .device(let atMs):
            try container.encode("device", forKey: .tag)
            try _InstallTimeDevice(atMs: atMs).encode(to: encoder)
        case .firstSeen(let atMs):
            try container.encode("first_seen", forKey: .tag)
            try _InstallTimeFirstSeen(atMs: atMs).encode(to: encoder)
        case .changedBetween(let fromMs, let toMs):
            try container.encode("changed_between", forKey: .tag)
            try _InstallTimeChangedBetween(fromMs: fromMs, toMs: toMs).encode(to: encoder)
        }
    }
}

public enum LockState: String, Codable, Sendable, CaseIterable {
    case missing
    case locked
    case unlocked
}

public struct Materialize: Codable, Sendable, Equatable {
    public var runId: String
    public var project: ProjectRef
    public var names: [String]
    public var command: SecretText
    public var workspace: String

    public init(runId: String, project: ProjectRef, names: [String], command: SecretText, workspace: String) {
        self.runId = runId
        self.project = project
        self.names = names
        self.command = command
        self.workspace = workspace
    }

    private enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case project
        case names
        case command
        case workspace
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.runId = try container.decode(String.self, forKey: .runId)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.names = try container.decode([String].self, forKey: .names)
        self.command = try container.decode(SecretText.self, forKey: .command)
        self.workspace = try container.decode(String.self, forKey: .workspace)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.runId, forKey: .runId)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.names, forKey: .names)
        try container.encode(self.command, forKey: .command)
        try container.encode(self.workspace, forKey: .workspace)
    }
}

public struct MaterializedAccount: Codable, Sendable, Equatable {
    public var name: String
    public var fields: [MaterializedSecret]

    public init(name: String, fields: [MaterializedSecret]) {
        self.name = name
        self.fields = fields
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case fields
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.fields = try container.decode([MaterializedSecret].self, forKey: .fields)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.fields, forKey: .fields)
    }
}

public struct MaterializedSecret: Codable, Sendable, Equatable {
    public var name: String
    public var value: SecretText

    public init(name: String, value: SecretText) {
        self.name = name
        self.value = value
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case value
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.value = try container.decode(SecretText.self, forKey: .value)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.value, forKey: .value)
    }
}

public struct Overview: Codable, Sendable, Equatable {
    public var serviceVersion: String
    public var heavy: QueueSnapshot
    public var claims: ClaimsSnapshot
    public var secrets: SecretsStatus

    public init(serviceVersion: String, heavy: QueueSnapshot, claims: ClaimsSnapshot, secrets: SecretsStatus) {
        self.serviceVersion = serviceVersion
        self.heavy = heavy
        self.claims = claims
        self.secrets = secrets
    }

    private enum CodingKeys: String, CodingKey {
        case serviceVersion = "service_version"
        case heavy
        case claims
        case secrets
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.serviceVersion = try container.decode(String.self, forKey: .serviceVersion)
        self.heavy = try container.decode(QueueSnapshot.self, forKey: .heavy)
        self.claims = try container.decode(ClaimsSnapshot.self, forKey: .claims)
        self.secrets = try container.decode(SecretsStatus.self, forKey: .secrets)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.serviceVersion, forKey: .serviceVersion)
        try container.encode(self.heavy, forKey: .heavy)
        try container.encode(self.claims, forKey: .claims)
        try container.encode(self.secrets, forKey: .secrets)
    }
}

public enum Platform: String, Codable, Sendable, CaseIterable {
    case android
    case ios
}

public struct ProcessIdentity: Codable, Sendable, Equatable {
    public var pid: UInt32
    public var bootId: String
    public var startTime: UInt64

    public init(pid: UInt32, bootId: String, startTime: UInt64) {
        self.pid = pid
        self.bootId = bootId
        self.startTime = startTime
    }

    private enum CodingKeys: String, CodingKey {
        case pid
        case bootId = "boot_id"
        case startTime = "start_time"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.pid = try container.decode(UInt32.self, forKey: .pid)
        self.bootId = try container.decode(String.self, forKey: .bootId)
        self.startTime = try container.decode(UInt64.self, forKey: .startTime)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.pid, forKey: .pid)
        try container.encode(self.bootId, forKey: .bootId)
        try container.encode(self.startTime, forKey: .startTime)
    }
}

public struct ProjectDecl: Codable, Sendable, Equatable {
    public var secrets: [DeclaredSecret]
    public var accounts: [DeclaredAccount]
    public var apps: DeclaredApps

    public init(secrets: [DeclaredSecret], accounts: [DeclaredAccount], apps: DeclaredApps) {
        self.secrets = secrets
        self.accounts = accounts
        self.apps = apps
    }

    private enum CodingKeys: String, CodingKey {
        case secrets
        case accounts
        case apps
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.secrets = try container.decode([DeclaredSecret].self, forKey: .secrets)
        self.accounts = try container.decode([DeclaredAccount].self, forKey: .accounts)
        self.apps = try container.decode(DeclaredApps.self, forKey: .apps)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.secrets, forKey: .secrets)
        try container.encode(self.accounts, forKey: .accounts)
        try container.encode(self.apps, forKey: .apps)
    }
}

public struct ProjectInfo: Codable, Sendable, Equatable {
    public var id: String
    public var name: String
    public var commonDir: String
    public var approved: Bool
    public var groupUuid: String?
    public var createdAtMs: Int64
    public var approvedAtMs: Int64?

    public init(id: String, name: String, commonDir: String, approved: Bool, groupUuid: String? = nil, createdAtMs: Int64, approvedAtMs: Int64? = nil) {
        self.id = id
        self.name = name
        self.commonDir = commonDir
        self.approved = approved
        self.groupUuid = groupUuid
        self.createdAtMs = createdAtMs
        self.approvedAtMs = approvedAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case name
        case commonDir = "common_dir"
        case approved
        case groupUuid = "group_uuid"
        case createdAtMs = "created_at_ms"
        case approvedAtMs = "approved_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.name = try container.decode(String.self, forKey: .name)
        self.commonDir = try container.decode(String.self, forKey: .commonDir)
        self.approved = try container.decode(Bool.self, forKey: .approved)
        self.groupUuid = try container.decodeIfPresent(String.self, forKey: .groupUuid)
        self.createdAtMs = try container.decode(Int64.self, forKey: .createdAtMs)
        self.approvedAtMs = try container.decodeIfPresent(Int64.self, forKey: .approvedAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.commonDir, forKey: .commonDir)
        try container.encode(self.approved, forKey: .approved)
        try container.encode(self.groupUuid, forKey: .groupUuid)
        try container.encode(self.createdAtMs, forKey: .createdAtMs)
        try container.encode(self.approvedAtMs, forKey: .approvedAtMs)
    }
}

public struct ProjectRef: Codable, Sendable, Equatable {
    public var commonDir: String
    public var name: String

    public init(commonDir: String, name: String) {
        self.commonDir = commonDir
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case commonDir = "common_dir"
        case name
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.commonDir = try container.decode(String.self, forKey: .commonDir)
        self.name = try container.decode(String.self, forKey: .name)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.commonDir, forKey: .commonDir)
        try container.encode(self.name, forKey: .name)
    }
}

public struct ProjectView: Codable, Sendable, Equatable {
    public var info: ProjectInfo
    public var workspaces: [WorkspaceInfo]
    public var declared: ProjectDecl?
    public var declaredAtMs: Int64?

    public init(info: ProjectInfo, workspaces: [WorkspaceInfo], declared: ProjectDecl? = nil, declaredAtMs: Int64? = nil) {
        self.info = info
        self.workspaces = workspaces
        self.declared = declared
        self.declaredAtMs = declaredAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case info
        case workspaces
        case declared
        case declaredAtMs = "declared_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.info = try container.decode(ProjectInfo.self, forKey: .info)
        self.workspaces = try container.decode([WorkspaceInfo].self, forKey: .workspaces)
        self.declared = try container.decodeIfPresent(ProjectDecl.self, forKey: .declared)
        self.declaredAtMs = try container.decodeIfPresent(Int64.self, forKey: .declaredAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.info, forKey: .info)
        try container.encode(self.workspaces, forKey: .workspaces)
        try container.encode(self.declared, forKey: .declared)
        try container.encode(self.declaredAtMs, forKey: .declaredAtMs)
    }
}

public struct ProjectsSnapshot: Codable, Sendable, Equatable {
    public var projects: [ProjectView]
    public var atMs: Int64
    public var asOfSeq: UInt64

    public init(projects: [ProjectView], atMs: Int64, asOfSeq: UInt64) {
        self.projects = projects
        self.atMs = atMs
        self.asOfSeq = asOfSeq
    }

    private enum CodingKeys: String, CodingKey {
        case projects
        case atMs = "at_ms"
        case asOfSeq = "as_of_seq"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.projects = try container.decode([ProjectView].self, forKey: .projects)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
        self.asOfSeq = try container.decode(UInt64.self, forKey: .asOfSeq)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.projects, forKey: .projects)
        try container.encode(self.atMs, forKey: .atMs)
        try container.encode(self.asOfSeq, forKey: .asOfSeq)
    }
}

public struct ProofGrant: Codable, Sendable, Equatable {
    public var expiresAtMs: Int64

    public init(expiresAtMs: Int64) {
        self.expiresAtMs = expiresAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case expiresAtMs = "expires_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.expiresAtMs = try container.decode(Int64.self, forKey: .expiresAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.expiresAtMs, forKey: .expiresAtMs)
    }
}

public struct Prove: Codable, Sendable, Equatable {
    public var password: SecretText
    public var keyFile: String?

    public init(password: SecretText, keyFile: String? = nil) {
        self.password = password
        self.keyFile = keyFile
    }

    private enum CodingKeys: String, CodingKey {
        case password
        case keyFile = "key_file"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.password = try container.decode(SecretText.self, forKey: .password)
        self.keyFile = try container.decodeIfPresent(String.self, forKey: .keyFile)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.password, forKey: .password)
        try container.encode(self.keyFile, forKey: .keyFile)
    }
}

public enum Purpose: Codable, Sendable, Equatable {
    case lease(command: String)
    case hold(ttlMs: UInt64?, boundTo: ProcessIdentity?, distinct: Bool)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "lease":
            let payload = try _PurposeLease(from: decoder)
            self = .lease(command: payload.command)
        case "hold":
            let payload = try _PurposeHold(from: decoder)
            self = .hold(ttlMs: payload.ttlMs, boundTo: payload.boundTo, distinct: payload.distinct)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "Purpose")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .lease(let command):
            try container.encode("lease", forKey: .tag)
            try _PurposeLease(command: command).encode(to: encoder)
        case .hold(let ttlMs, let boundTo, let distinct):
            try container.encode("hold", forKey: .tag)
            try _PurposeHold(ttlMs: ttlMs, boundTo: boundTo, distinct: distinct).encode(to: encoder)
        }
    }
}

public struct QueueSnapshot: Codable, Sendable, Equatable {
    public var capacity: UInt8
    public var running: [HeavyEntry]
    public var waiting: [HeavyEntry]
    public var foreignRunning: [HeavyEntry]
    public var asOfSeq: UInt64

    public init(capacity: UInt8, running: [HeavyEntry], waiting: [HeavyEntry], foreignRunning: [HeavyEntry], asOfSeq: UInt64) {
        self.capacity = capacity
        self.running = running
        self.waiting = waiting
        self.foreignRunning = foreignRunning
        self.asOfSeq = asOfSeq
    }

    private enum CodingKeys: String, CodingKey {
        case capacity
        case running
        case waiting
        case foreignRunning = "foreign_running"
        case asOfSeq = "as_of_seq"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.capacity = try container.decode(UInt8.self, forKey: .capacity)
        self.running = try container.decode([HeavyEntry].self, forKey: .running)
        self.waiting = try container.decode([HeavyEntry].self, forKey: .waiting)
        self.foreignRunning = try container.decode([HeavyEntry].self, forKey: .foreignRunning)
        self.asOfSeq = try container.decode(UInt64.self, forKey: .asOfSeq)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.capacity, forKey: .capacity)
        try container.encode(self.running, forKey: .running)
        try container.encode(self.waiting, forKey: .waiting)
        try container.encode(self.foreignRunning, forKey: .foreignRunning)
        try container.encode(self.asOfSeq, forKey: .asOfSeq)
    }
}

public struct QueueStopOutcome: Codable, Sendable, Equatable {
    public var id: String
    public var stopped: Bool
    public var pids: [UInt32]

    public init(id: String, stopped: Bool, pids: [UInt32]) {
        self.id = id
        self.stopped = stopped
        self.pids = pids
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case stopped
        case pids
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.stopped = try container.decode(Bool.self, forKey: .stopped)
        self.pids = try container.decode([UInt32].self, forKey: .pids)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.stopped, forKey: .stopped)
        try container.encode(self.pids, forKey: .pids)
    }
}

public enum ReadingBool: Codable, Sendable, Equatable {
    case emulated
    case unknown
    case known(Bool)

    private enum CodingKeys: String, CodingKey {
        case known = "known"
    }

    public init(from decoder: any Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let tag = try? single.decode(String.self) {
            switch tag {
            case "emulated": self = .emulated
            case "unknown": self = .unknown
            default: throw ContractCoding.unknownVariant(decoder, type: "ReadingBool")
            }
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard container.allKeys.count == 1, let key = container.allKeys.first else {
            throw ContractCoding.unknownVariant(decoder, type: "ReadingBool")
        }
        switch key {
        case .known:
            self = .known(try container.decode(Bool.self, forKey: .known))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .emulated:
            var container = encoder.singleValueContainer()
            try container.encode("emulated")
        case .unknown:
            var container = encoder.singleValueContainer()
            try container.encode("unknown")
        case .known(let value):
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(value, forKey: .known)
        }
    }
}

public enum ReadingUInt8: Codable, Sendable, Equatable {
    case emulated
    case unknown
    case known(UInt8)

    private enum CodingKeys: String, CodingKey {
        case known = "known"
    }

    public init(from decoder: any Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let tag = try? single.decode(String.self) {
            switch tag {
            case "emulated": self = .emulated
            case "unknown": self = .unknown
            default: throw ContractCoding.unknownVariant(decoder, type: "ReadingUInt8")
            }
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard container.allKeys.count == 1, let key = container.allKeys.first else {
            throw ContractCoding.unknownVariant(decoder, type: "ReadingUInt8")
        }
        switch key {
        case .known:
            self = .known(try container.decode(UInt8.self, forKey: .known))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .emulated:
            var container = encoder.singleValueContainer()
            try container.encode("emulated")
        case .unknown:
            var container = encoder.singleValueContainer()
            try container.encode("unknown")
        case .known(let value):
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(value, forKey: .known)
        }
    }
}

public struct ReleaseOutcome: Codable, Sendable, Equatable {
    public var parent: Claim?
    public var seizedBy: String?

    public init(parent: Claim? = nil, seizedBy: String? = nil) {
        self.parent = parent
        self.seizedBy = seizedBy
    }

    private enum CodingKeys: String, CodingKey {
        case parent
        case seizedBy = "seized_by"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.parent = try container.decodeIfPresent(Claim.self, forKey: .parent)
        self.seizedBy = try container.decodeIfPresent(String.self, forKey: .seizedBy)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.parent, forKey: .parent)
        try container.encode(self.seizedBy, forKey: .seizedBy)
    }
}

public struct Removed: Codable, Sendable, Equatable {
    public var removed: Bool

    public init(removed: Bool) {
        self.removed = removed
    }

    private enum CodingKeys: String, CodingKey {
        case removed
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.removed = try container.decode(Bool.self, forKey: .removed)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.removed, forKey: .removed)
    }
}

public enum Request: Codable, Sendable, Equatable {
    case hello(contractVersion: UInt32, client: String)
    case serviceStatus
    case serviceShutdown(ifIdle: Bool, refuseIfUnlocked: Bool)
    case heavyAcquire(HeavyAcquire)
    case heavyAttach(id: String)
    case heavyStarted(HeavyStarted)
    case heavyRelease(HeavyRelease)
    case heavyWithdraw(id: String, timedOut: Bool)
    case heavyForceRelease(id: String)
    case queueSnapshot
    case queueHistory(HistoryQuery)
    case queueSetCapacity(capacity: UInt8)
    case secretsStatus
    case secretsUnlock(Unlock)
    case secretsLock
    case projectResolve(ProjectRef)
    case projectList
    case projectApprove(ProjectRef)
    case secretList(project: ProjectRef, declared: [DeclaredSecret])
    case secretSet(SecretSet)
    case secretRemove(project: ProjectRef, name: String)
    case secretImport(SecretImport)
    case secretRequest(SecretRequest)
    case secretRequestWithdraw(id: String, timedOut: Bool)
    case secretRequests
    case secretDecline(Decline)
    case secretShare(Share)
    case secretUnshare(Share)
    case secretMaterialize(Materialize)
    case workspaceResolve(WorkspaceResolve)
    case workspaceList
    case claimsAcquire(ClaimRequest)
    case claimsAttach(requestId: String)
    case claimsStarted(ClaimStarted)
    case claimsRelease(ClaimRelease)
    case claimsForceRelease(claimId: String, confirmation: String, workspaceId: String, requester: ProcessIdentity)
    case claimsWithdraw(requestId: String, timedOut: Bool)
    case claimsRenew(claimId: String, workspaceId: String)
    case claimsSetCommand(claimId: String, workspaceId: String, command: String)
    case claimsReserve(resource: ResourceId, workspaceId: String)
    case claimsUnreserve(resource: ResourceId, workspaceId: String)
    case claimsSnapshot
    case claimsHistory(HistoryQuery)
    case claimsSeizeBegin(SeizeBegin)
    case claimsSeizeCommit(SeizeCommit)
    case claimsSeizeAbort(planId: String)
    case claimsSeizeProgress(SeizeProgress)
    case statusOverview
    case accountList(project: ProjectRef)
    case accountSet(AccountSet)
    case accountRemove(AccountRemove)
    case accountCheck(project: ProjectRef, name: String)
    case accountMaterialize(AccountMaterialize)
    case devicesList(all: Bool)
    case devicesShow(device: String, freshSessions: Bool)
    case devicesResolve(device: String)
    case devicesAdd(device: String, alias: String?, person: Bool)
    case devicesRename(device: String, alias: String, person: Bool)
    case devicesNote(device: String, note: String?)
    case devicesRemove(device: String, person: Bool)
    case devicesInstalls(device: String?, query: HistoryQuery)
    case devicesBoot(resource: ResourceId, claimId: String, workspaceId: String)
    case devicesTools(ToolPathsSet)
    case devicesForgetSessions(device: String, person: Bool, byWorkspacePath: String?, byPid: UInt32?)
    case secretsProve(Prove)
    case secretReveal(project: ProjectRef, name: String)
    case accountReveal(project: ProjectRef, name: String)
    case secretsRemovePreserved(path: String)
    case accountRequest(AccountRequestNew)
    case accountRequestWithdraw(id: String, timedOut: Bool)
    case accountRequests
    case accountDecline(id: String)
    case accountAnswer(AccountAnswer)
    case heavyStop(id: String, confirmation: String)
    case projectsSnapshot
    case requestsSnapshot

    private enum TagKeys: String, CodingKey {
        case tag = "method"
        case content = "params"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "hello":
            let payload = try container.decode(_RequestHello.self, forKey: .content)
            self = .hello(contractVersion: payload.contractVersion, client: payload.client)
        case "service.status":
            self = .serviceStatus
        case "service.shutdown":
            let payload = try container.decode(_RequestServiceShutdown.self, forKey: .content)
            self = .serviceShutdown(ifIdle: payload.ifIdle, refuseIfUnlocked: payload.refuseIfUnlocked)
        case "heavy.acquire":
            self = .heavyAcquire(try container.decode(HeavyAcquire.self, forKey: .content))
        case "heavy.attach":
            let payload = try container.decode(_RequestHeavyAttach.self, forKey: .content)
            self = .heavyAttach(id: payload.id)
        case "heavy.started":
            self = .heavyStarted(try container.decode(HeavyStarted.self, forKey: .content))
        case "heavy.release":
            self = .heavyRelease(try container.decode(HeavyRelease.self, forKey: .content))
        case "heavy.withdraw":
            let payload = try container.decode(_RequestHeavyWithdraw.self, forKey: .content)
            self = .heavyWithdraw(id: payload.id, timedOut: payload.timedOut)
        case "heavy.force_release":
            let payload = try container.decode(_RequestHeavyForceRelease.self, forKey: .content)
            self = .heavyForceRelease(id: payload.id)
        case "queue.snapshot":
            self = .queueSnapshot
        case "queue.history":
            self = .queueHistory(try container.decode(HistoryQuery.self, forKey: .content))
        case "queue.set_capacity":
            let payload = try container.decode(_RequestQueueSetCapacity.self, forKey: .content)
            self = .queueSetCapacity(capacity: payload.capacity)
        case "secrets.status":
            self = .secretsStatus
        case "secrets.unlock":
            self = .secretsUnlock(try container.decode(Unlock.self, forKey: .content))
        case "secrets.lock":
            self = .secretsLock
        case "project.resolve":
            self = .projectResolve(try container.decode(ProjectRef.self, forKey: .content))
        case "project.list":
            self = .projectList
        case "project.approve":
            self = .projectApprove(try container.decode(ProjectRef.self, forKey: .content))
        case "secret.list":
            let payload = try container.decode(_RequestSecretList.self, forKey: .content)
            self = .secretList(project: payload.project, declared: payload.declared)
        case "secret.set":
            self = .secretSet(try container.decode(SecretSet.self, forKey: .content))
        case "secret.remove":
            let payload = try container.decode(_RequestSecretRemove.self, forKey: .content)
            self = .secretRemove(project: payload.project, name: payload.name)
        case "secret.import":
            self = .secretImport(try container.decode(SecretImport.self, forKey: .content))
        case "secret.request":
            self = .secretRequest(try container.decode(SecretRequest.self, forKey: .content))
        case "secret.request_withdraw":
            let payload = try container.decode(_RequestSecretRequestWithdraw.self, forKey: .content)
            self = .secretRequestWithdraw(id: payload.id, timedOut: payload.timedOut)
        case "secret.requests":
            self = .secretRequests
        case "secret.decline":
            self = .secretDecline(try container.decode(Decline.self, forKey: .content))
        case "secret.share":
            self = .secretShare(try container.decode(Share.self, forKey: .content))
        case "secret.unshare":
            self = .secretUnshare(try container.decode(Share.self, forKey: .content))
        case "secret.materialize":
            self = .secretMaterialize(try container.decode(Materialize.self, forKey: .content))
        case "workspace.resolve":
            self = .workspaceResolve(try container.decode(WorkspaceResolve.self, forKey: .content))
        case "workspace.list":
            self = .workspaceList
        case "claims.acquire":
            self = .claimsAcquire(try container.decode(ClaimRequest.self, forKey: .content))
        case "claims.attach":
            let payload = try container.decode(_RequestClaimsAttach.self, forKey: .content)
            self = .claimsAttach(requestId: payload.requestId)
        case "claims.started":
            self = .claimsStarted(try container.decode(ClaimStarted.self, forKey: .content))
        case "claims.release":
            self = .claimsRelease(try container.decode(ClaimRelease.self, forKey: .content))
        case "claims.force_release":
            let payload = try container.decode(_RequestClaimsForceRelease.self, forKey: .content)
            self = .claimsForceRelease(claimId: payload.claimId, confirmation: payload.confirmation, workspaceId: payload.workspaceId, requester: payload.requester)
        case "claims.withdraw":
            let payload = try container.decode(_RequestClaimsWithdraw.self, forKey: .content)
            self = .claimsWithdraw(requestId: payload.requestId, timedOut: payload.timedOut)
        case "claims.renew":
            let payload = try container.decode(_RequestClaimsRenew.self, forKey: .content)
            self = .claimsRenew(claimId: payload.claimId, workspaceId: payload.workspaceId)
        case "claims.set_command":
            let payload = try container.decode(_RequestClaimsSetCommand.self, forKey: .content)
            self = .claimsSetCommand(claimId: payload.claimId, workspaceId: payload.workspaceId, command: payload.command)
        case "claims.reserve":
            let payload = try container.decode(_RequestClaimsReserve.self, forKey: .content)
            self = .claimsReserve(resource: payload.resource, workspaceId: payload.workspaceId)
        case "claims.unreserve":
            let payload = try container.decode(_RequestClaimsUnreserve.self, forKey: .content)
            self = .claimsUnreserve(resource: payload.resource, workspaceId: payload.workspaceId)
        case "claims.snapshot":
            self = .claimsSnapshot
        case "claims.history":
            self = .claimsHistory(try container.decode(HistoryQuery.self, forKey: .content))
        case "claims.seize_begin":
            self = .claimsSeizeBegin(try container.decode(SeizeBegin.self, forKey: .content))
        case "claims.seize_commit":
            self = .claimsSeizeCommit(try container.decode(SeizeCommit.self, forKey: .content))
        case "claims.seize_abort":
            let payload = try container.decode(_RequestClaimsSeizeAbort.self, forKey: .content)
            self = .claimsSeizeAbort(planId: payload.planId)
        case "claims.seize_progress":
            self = .claimsSeizeProgress(try container.decode(SeizeProgress.self, forKey: .content))
        case "status.overview":
            self = .statusOverview
        case "account.list":
            let payload = try container.decode(_RequestAccountList.self, forKey: .content)
            self = .accountList(project: payload.project)
        case "account.set":
            self = .accountSet(try container.decode(AccountSet.self, forKey: .content))
        case "account.remove":
            self = .accountRemove(try container.decode(AccountRemove.self, forKey: .content))
        case "account.check":
            let payload = try container.decode(_RequestAccountCheck.self, forKey: .content)
            self = .accountCheck(project: payload.project, name: payload.name)
        case "account.materialize":
            self = .accountMaterialize(try container.decode(AccountMaterialize.self, forKey: .content))
        case "devices.list":
            let payload = try container.decode(_RequestDevicesList.self, forKey: .content)
            self = .devicesList(all: payload.all)
        case "devices.show":
            let payload = try container.decode(_RequestDevicesShow.self, forKey: .content)
            self = .devicesShow(device: payload.device, freshSessions: payload.freshSessions)
        case "devices.resolve":
            let payload = try container.decode(_RequestDevicesResolve.self, forKey: .content)
            self = .devicesResolve(device: payload.device)
        case "devices.add":
            let payload = try container.decode(_RequestDevicesAdd.self, forKey: .content)
            self = .devicesAdd(device: payload.device, alias: payload.alias, person: payload.person)
        case "devices.rename":
            let payload = try container.decode(_RequestDevicesRename.self, forKey: .content)
            self = .devicesRename(device: payload.device, alias: payload.alias, person: payload.person)
        case "devices.note":
            let payload = try container.decode(_RequestDevicesNote.self, forKey: .content)
            self = .devicesNote(device: payload.device, note: payload.note)
        case "devices.remove":
            let payload = try container.decode(_RequestDevicesRemove.self, forKey: .content)
            self = .devicesRemove(device: payload.device, person: payload.person)
        case "devices.installs":
            let payload = try container.decode(_RequestDevicesInstalls.self, forKey: .content)
            self = .devicesInstalls(device: payload.device, query: payload.query)
        case "devices.boot":
            let payload = try container.decode(_RequestDevicesBoot.self, forKey: .content)
            self = .devicesBoot(resource: payload.resource, claimId: payload.claimId, workspaceId: payload.workspaceId)
        case "devices.tools":
            self = .devicesTools(try container.decode(ToolPathsSet.self, forKey: .content))
        case "devices.forget_sessions":
            let payload = try container.decode(_RequestDevicesForgetSessions.self, forKey: .content)
            self = .devicesForgetSessions(device: payload.device, person: payload.person, byWorkspacePath: payload.byWorkspacePath, byPid: payload.byPid)
        case "secrets.prove":
            self = .secretsProve(try container.decode(Prove.self, forKey: .content))
        case "secret.reveal":
            let payload = try container.decode(_RequestSecretReveal.self, forKey: .content)
            self = .secretReveal(project: payload.project, name: payload.name)
        case "account.reveal":
            let payload = try container.decode(_RequestAccountReveal.self, forKey: .content)
            self = .accountReveal(project: payload.project, name: payload.name)
        case "secrets.remove_preserved":
            let payload = try container.decode(_RequestSecretsRemovePreserved.self, forKey: .content)
            self = .secretsRemovePreserved(path: payload.path)
        case "account.request":
            self = .accountRequest(try container.decode(AccountRequestNew.self, forKey: .content))
        case "account.request_withdraw":
            let payload = try container.decode(_RequestAccountRequestWithdraw.self, forKey: .content)
            self = .accountRequestWithdraw(id: payload.id, timedOut: payload.timedOut)
        case "account.requests":
            self = .accountRequests
        case "account.decline":
            let payload = try container.decode(_RequestAccountDecline.self, forKey: .content)
            self = .accountDecline(id: payload.id)
        case "account.answer":
            self = .accountAnswer(try container.decode(AccountAnswer.self, forKey: .content))
        case "heavy.stop":
            let payload = try container.decode(_RequestHeavyStop.self, forKey: .content)
            self = .heavyStop(id: payload.id, confirmation: payload.confirmation)
        case "projects.snapshot":
            self = .projectsSnapshot
        case "requests.snapshot":
            self = .requestsSnapshot
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "Request")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .hello(let contractVersion, let client):
            try container.encode("hello", forKey: .tag)
            try container.encode(_RequestHello(contractVersion: contractVersion, client: client), forKey: .content)
        case .serviceStatus:
            try container.encode("service.status", forKey: .tag)
        case .serviceShutdown(let ifIdle, let refuseIfUnlocked):
            try container.encode("service.shutdown", forKey: .tag)
            try container.encode(_RequestServiceShutdown(ifIdle: ifIdle, refuseIfUnlocked: refuseIfUnlocked), forKey: .content)
        case .heavyAcquire(let value):
            try container.encode("heavy.acquire", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .heavyAttach(let id):
            try container.encode("heavy.attach", forKey: .tag)
            try container.encode(_RequestHeavyAttach(id: id), forKey: .content)
        case .heavyStarted(let value):
            try container.encode("heavy.started", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .heavyRelease(let value):
            try container.encode("heavy.release", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .heavyWithdraw(let id, let timedOut):
            try container.encode("heavy.withdraw", forKey: .tag)
            try container.encode(_RequestHeavyWithdraw(id: id, timedOut: timedOut), forKey: .content)
        case .heavyForceRelease(let id):
            try container.encode("heavy.force_release", forKey: .tag)
            try container.encode(_RequestHeavyForceRelease(id: id), forKey: .content)
        case .queueSnapshot:
            try container.encode("queue.snapshot", forKey: .tag)
        case .queueHistory(let value):
            try container.encode("queue.history", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .queueSetCapacity(let capacity):
            try container.encode("queue.set_capacity", forKey: .tag)
            try container.encode(_RequestQueueSetCapacity(capacity: capacity), forKey: .content)
        case .secretsStatus:
            try container.encode("secrets.status", forKey: .tag)
        case .secretsUnlock(let value):
            try container.encode("secrets.unlock", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretsLock:
            try container.encode("secrets.lock", forKey: .tag)
        case .projectResolve(let value):
            try container.encode("project.resolve", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .projectList:
            try container.encode("project.list", forKey: .tag)
        case .projectApprove(let value):
            try container.encode("project.approve", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretList(let project, let declared):
            try container.encode("secret.list", forKey: .tag)
            try container.encode(_RequestSecretList(project: project, declared: declared), forKey: .content)
        case .secretSet(let value):
            try container.encode("secret.set", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretRemove(let project, let name):
            try container.encode("secret.remove", forKey: .tag)
            try container.encode(_RequestSecretRemove(project: project, name: name), forKey: .content)
        case .secretImport(let value):
            try container.encode("secret.import", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretRequest(let value):
            try container.encode("secret.request", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretRequestWithdraw(let id, let timedOut):
            try container.encode("secret.request_withdraw", forKey: .tag)
            try container.encode(_RequestSecretRequestWithdraw(id: id, timedOut: timedOut), forKey: .content)
        case .secretRequests:
            try container.encode("secret.requests", forKey: .tag)
        case .secretDecline(let value):
            try container.encode("secret.decline", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretShare(let value):
            try container.encode("secret.share", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretUnshare(let value):
            try container.encode("secret.unshare", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretMaterialize(let value):
            try container.encode("secret.materialize", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .workspaceResolve(let value):
            try container.encode("workspace.resolve", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .workspaceList:
            try container.encode("workspace.list", forKey: .tag)
        case .claimsAcquire(let value):
            try container.encode("claims.acquire", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsAttach(let requestId):
            try container.encode("claims.attach", forKey: .tag)
            try container.encode(_RequestClaimsAttach(requestId: requestId), forKey: .content)
        case .claimsStarted(let value):
            try container.encode("claims.started", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsRelease(let value):
            try container.encode("claims.release", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsForceRelease(let claimId, let confirmation, let workspaceId, let requester):
            try container.encode("claims.force_release", forKey: .tag)
            try container.encode(_RequestClaimsForceRelease(claimId: claimId, confirmation: confirmation, workspaceId: workspaceId, requester: requester), forKey: .content)
        case .claimsWithdraw(let requestId, let timedOut):
            try container.encode("claims.withdraw", forKey: .tag)
            try container.encode(_RequestClaimsWithdraw(requestId: requestId, timedOut: timedOut), forKey: .content)
        case .claimsRenew(let claimId, let workspaceId):
            try container.encode("claims.renew", forKey: .tag)
            try container.encode(_RequestClaimsRenew(claimId: claimId, workspaceId: workspaceId), forKey: .content)
        case .claimsSetCommand(let claimId, let workspaceId, let command):
            try container.encode("claims.set_command", forKey: .tag)
            try container.encode(_RequestClaimsSetCommand(claimId: claimId, workspaceId: workspaceId, command: command), forKey: .content)
        case .claimsReserve(let resource, let workspaceId):
            try container.encode("claims.reserve", forKey: .tag)
            try container.encode(_RequestClaimsReserve(resource: resource, workspaceId: workspaceId), forKey: .content)
        case .claimsUnreserve(let resource, let workspaceId):
            try container.encode("claims.unreserve", forKey: .tag)
            try container.encode(_RequestClaimsUnreserve(resource: resource, workspaceId: workspaceId), forKey: .content)
        case .claimsSnapshot:
            try container.encode("claims.snapshot", forKey: .tag)
        case .claimsHistory(let value):
            try container.encode("claims.history", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsSeizeBegin(let value):
            try container.encode("claims.seize_begin", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsSeizeCommit(let value):
            try container.encode("claims.seize_commit", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .claimsSeizeAbort(let planId):
            try container.encode("claims.seize_abort", forKey: .tag)
            try container.encode(_RequestClaimsSeizeAbort(planId: planId), forKey: .content)
        case .claimsSeizeProgress(let value):
            try container.encode("claims.seize_progress", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .statusOverview:
            try container.encode("status.overview", forKey: .tag)
        case .accountList(let project):
            try container.encode("account.list", forKey: .tag)
            try container.encode(_RequestAccountList(project: project), forKey: .content)
        case .accountSet(let value):
            try container.encode("account.set", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .accountRemove(let value):
            try container.encode("account.remove", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .accountCheck(let project, let name):
            try container.encode("account.check", forKey: .tag)
            try container.encode(_RequestAccountCheck(project: project, name: name), forKey: .content)
        case .accountMaterialize(let value):
            try container.encode("account.materialize", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .devicesList(let all):
            try container.encode("devices.list", forKey: .tag)
            try container.encode(_RequestDevicesList(all: all), forKey: .content)
        case .devicesShow(let device, let freshSessions):
            try container.encode("devices.show", forKey: .tag)
            try container.encode(_RequestDevicesShow(device: device, freshSessions: freshSessions), forKey: .content)
        case .devicesResolve(let device):
            try container.encode("devices.resolve", forKey: .tag)
            try container.encode(_RequestDevicesResolve(device: device), forKey: .content)
        case .devicesAdd(let device, let alias, let person):
            try container.encode("devices.add", forKey: .tag)
            try container.encode(_RequestDevicesAdd(device: device, alias: alias, person: person), forKey: .content)
        case .devicesRename(let device, let alias, let person):
            try container.encode("devices.rename", forKey: .tag)
            try container.encode(_RequestDevicesRename(device: device, alias: alias, person: person), forKey: .content)
        case .devicesNote(let device, let note):
            try container.encode("devices.note", forKey: .tag)
            try container.encode(_RequestDevicesNote(device: device, note: note), forKey: .content)
        case .devicesRemove(let device, let person):
            try container.encode("devices.remove", forKey: .tag)
            try container.encode(_RequestDevicesRemove(device: device, person: person), forKey: .content)
        case .devicesInstalls(let device, let query):
            try container.encode("devices.installs", forKey: .tag)
            try container.encode(_RequestDevicesInstalls(device: device, query: query), forKey: .content)
        case .devicesBoot(let resource, let claimId, let workspaceId):
            try container.encode("devices.boot", forKey: .tag)
            try container.encode(_RequestDevicesBoot(resource: resource, claimId: claimId, workspaceId: workspaceId), forKey: .content)
        case .devicesTools(let value):
            try container.encode("devices.tools", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .devicesForgetSessions(let device, let person, let byWorkspacePath, let byPid):
            try container.encode("devices.forget_sessions", forKey: .tag)
            try container.encode(_RequestDevicesForgetSessions(device: device, person: person, byWorkspacePath: byWorkspacePath, byPid: byPid), forKey: .content)
        case .secretsProve(let value):
            try container.encode("secrets.prove", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .secretReveal(let project, let name):
            try container.encode("secret.reveal", forKey: .tag)
            try container.encode(_RequestSecretReveal(project: project, name: name), forKey: .content)
        case .accountReveal(let project, let name):
            try container.encode("account.reveal", forKey: .tag)
            try container.encode(_RequestAccountReveal(project: project, name: name), forKey: .content)
        case .secretsRemovePreserved(let path):
            try container.encode("secrets.remove_preserved", forKey: .tag)
            try container.encode(_RequestSecretsRemovePreserved(path: path), forKey: .content)
        case .accountRequest(let value):
            try container.encode("account.request", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .accountRequestWithdraw(let id, let timedOut):
            try container.encode("account.request_withdraw", forKey: .tag)
            try container.encode(_RequestAccountRequestWithdraw(id: id, timedOut: timedOut), forKey: .content)
        case .accountRequests:
            try container.encode("account.requests", forKey: .tag)
        case .accountDecline(let id):
            try container.encode("account.decline", forKey: .tag)
            try container.encode(_RequestAccountDecline(id: id), forKey: .content)
        case .accountAnswer(let value):
            try container.encode("account.answer", forKey: .tag)
            try container.encode(value, forKey: .content)
        case .heavyStop(let id, let confirmation):
            try container.encode("heavy.stop", forKey: .tag)
            try container.encode(_RequestHeavyStop(id: id, confirmation: confirmation), forKey: .content)
        case .projectsSnapshot:
            try container.encode("projects.snapshot", forKey: .tag)
        case .requestsSnapshot:
            try container.encode("requests.snapshot", forKey: .tag)
        }
    }
}

public enum RequestState: String, Codable, Sendable, CaseIterable {
    case pending
    case stored
    case declined
    case withdrawn
    case timedOut = "timed_out"
}

public struct RequestsSnapshot: Codable, Sendable, Equatable {
    public var secrets: [SecretRequestInfo]
    public var accounts: [AccountRequestInfo]
    public var asOfSeq: UInt64

    public init(secrets: [SecretRequestInfo], accounts: [AccountRequestInfo], asOfSeq: UInt64) {
        self.secrets = secrets
        self.accounts = accounts
        self.asOfSeq = asOfSeq
    }

    private enum CodingKeys: String, CodingKey {
        case secrets
        case accounts
        case asOfSeq = "as_of_seq"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.secrets = try container.decode([SecretRequestInfo].self, forKey: .secrets)
        self.accounts = try container.decode([AccountRequestInfo].self, forKey: .accounts)
        self.asOfSeq = try container.decode(UInt64.self, forKey: .asOfSeq)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.secrets, forKey: .secrets)
        try container.encode(self.accounts, forKey: .accounts)
        try container.encode(self.asOfSeq, forKey: .asOfSeq)
    }
}

public struct ResourceId: RawRepresentable, Codable, Sendable, Hashable {
    public var rawValue: String

    public init(rawValue: String) {
        self.rawValue = rawValue
    }

    public init(_ rawValue: String) {
        self.rawValue = rawValue
    }

    public init(from decoder: any Decoder) throws {
        self.rawValue = try decoder.singleValueContainer().decode(String.self)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

public struct ResourceInfo: Codable, Sendable, Equatable {
    public var id: ResourceId
    public var kind: ResourceKind
    public var display: String
    public var attributes: [String: String]
    public var available: Bool

    public init(id: ResourceId, kind: ResourceKind, display: String, attributes: [String: String], available: Bool) {
        self.id = id
        self.kind = kind
        self.display = display
        self.attributes = attributes
        self.available = available
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case kind
        case display
        case attributes
        case available
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(ResourceId.self, forKey: .id)
        self.kind = try container.decode(ResourceKind.self, forKey: .kind)
        self.display = try container.decode(String.self, forKey: .display)
        self.attributes = try container.decode([String: String].self, forKey: .attributes)
        self.available = try container.decode(Bool.self, forKey: .available)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.display, forKey: .display)
        try container.encode(self.attributes, forKey: .attributes)
        try container.encode(self.available, forKey: .available)
    }
}

public enum ResourceKind: String, Codable, Sendable, CaseIterable {
    case screen
    case account
    case device
}

public struct ResourceState: Codable, Sendable, Equatable {
    public var resource: ResourceInfo
    public var claims: [Claim]
    public var foreign: [ForeignClaim]
    public var waiting: [WaitingRequest]
    public var seizing: Bool
    public var seizingBy: String?

    public init(resource: ResourceInfo, claims: [Claim], foreign: [ForeignClaim], waiting: [WaitingRequest], seizing: Bool, seizingBy: String? = nil) {
        self.resource = resource
        self.claims = claims
        self.foreign = foreign
        self.waiting = waiting
        self.seizing = seizing
        self.seizingBy = seizingBy
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case claims
        case foreign
        case waiting
        case seizing
        case seizingBy = "seizing_by"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceInfo.self, forKey: .resource)
        self.claims = try container.decode([Claim].self, forKey: .claims)
        self.foreign = try container.decode([ForeignClaim].self, forKey: .foreign)
        self.waiting = try container.decode([WaitingRequest].self, forKey: .waiting)
        self.seizing = try container.decode(Bool.self, forKey: .seizing)
        self.seizingBy = try container.decodeIfPresent(String.self, forKey: .seizingBy)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.claims, forKey: .claims)
        try container.encode(self.foreign, forKey: .foreign)
        try container.encode(self.waiting, forKey: .waiting)
        try container.encode(self.seizing, forKey: .seizing)
        try container.encode(self.seizingBy, forKey: .seizingBy)
    }
}

public enum ResyncReason: String, Codable, Sendable, CaseIterable {
    case lagged
    case overflow
    case oversized
}

public enum RunGroup: Codable, Sendable, Equatable {
    case unixGroup(pgid: UInt32, bootId: String)
    case windowsJob(name: String, bootId: String, session: UInt32?)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "unix_group":
            let payload = try _RunGroupUnixGroup(from: decoder)
            self = .unixGroup(pgid: payload.pgid, bootId: payload.bootId)
        case "windows_job":
            let payload = try _RunGroupWindowsJob(from: decoder)
            self = .windowsJob(name: payload.name, bootId: payload.bootId, session: payload.session)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "RunGroup")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .unixGroup(let pgid, let bootId):
            try container.encode("unix_group", forKey: .tag)
            try _RunGroupUnixGroup(pgid: pgid, bootId: bootId).encode(to: encoder)
        case .windowsJob(let name, let bootId, let session):
            try container.encode("windows_job", forKey: .tag)
            try _RunGroupWindowsJob(name: name, bootId: bootId, session: session).encode(to: encoder)
        }
    }
}

public struct SecretImport: Codable, Sendable, Equatable {
    public var project: ProjectRef
    public var entries: [ImportEntry]
    public var overwrite: Bool

    public init(project: ProjectRef, entries: [ImportEntry], overwrite: Bool) {
        self.project = project
        self.entries = entries
        self.overwrite = overwrite
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case entries
        case overwrite
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.entries = try container.decode([ImportEntry].self, forKey: .entries)
        self.overwrite = try container.decode(Bool.self, forKey: .overwrite)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.entries, forKey: .entries)
        try container.encode(self.overwrite, forKey: .overwrite)
    }
}

public struct SecretInfo: Codable, Sendable, Equatable {
    public var name: String
    public var purpose: String?
    public var declared: Bool
    public var state: SecretState
    public var source: SecretSource?

    public init(name: String, purpose: String? = nil, declared: Bool, state: SecretState, source: SecretSource? = nil) {
        self.name = name
        self.purpose = purpose
        self.declared = declared
        self.state = state
        self.source = source
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case purpose
        case declared
        case state
        case source
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.purpose = try container.decodeIfPresent(String.self, forKey: .purpose)
        self.declared = try container.decode(Bool.self, forKey: .declared)
        self.state = try container.decode(SecretState.self, forKey: .state)
        self.source = try container.decodeIfPresent(SecretSource.self, forKey: .source)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.purpose, forKey: .purpose)
        try container.encode(self.declared, forKey: .declared)
        try container.encode(self.state, forKey: .state)
        try container.encode(self.source, forKey: .source)
    }
}

public struct SecretList: Codable, Sendable, Equatable {
    public var project: ProjectInfo
    public var secrets: [SecretInfo]

    public init(project: ProjectInfo, secrets: [SecretInfo]) {
        self.project = project
        self.secrets = secrets
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case secrets
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectInfo.self, forKey: .project)
        self.secrets = try container.decode([SecretInfo].self, forKey: .secrets)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.secrets, forKey: .secrets)
    }
}

public struct SecretRequest: Codable, Sendable, Equatable {
    public var id: String
    public var project: ProjectRef
    public var name: String
    public var reason: String
    public var requester: ProcessIdentity

    public init(id: String, project: ProjectRef, name: String, reason: String, requester: ProcessIdentity) {
        self.id = id
        self.project = project
        self.name = name
        self.reason = reason
        self.requester = requester
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case project
        case name
        case reason
        case requester
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
        self.reason = try container.decode(String.self, forKey: .reason)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.reason, forKey: .reason)
        try container.encode(self.requester, forKey: .requester)
    }
}

public struct SecretRequestInfo: Codable, Sendable, Equatable {
    public var id: String
    public var projectId: String
    public var projectName: String
    public var name: String
    public var reason: String
    public var state: RequestState
    public var requesterPid: UInt32
    public var createdAtMs: Int64
    public var answeredAtMs: Int64?

    public init(id: String, projectId: String, projectName: String, name: String, reason: String, state: RequestState, requesterPid: UInt32, createdAtMs: Int64, answeredAtMs: Int64? = nil) {
        self.id = id
        self.projectId = projectId
        self.projectName = projectName
        self.name = name
        self.reason = reason
        self.state = state
        self.requesterPid = requesterPid
        self.createdAtMs = createdAtMs
        self.answeredAtMs = answeredAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case projectName = "project_name"
        case name
        case reason
        case state
        case requesterPid = "requester_pid"
        case createdAtMs = "created_at_ms"
        case answeredAtMs = "answered_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.projectId = try container.decode(String.self, forKey: .projectId)
        self.projectName = try container.decode(String.self, forKey: .projectName)
        self.name = try container.decode(String.self, forKey: .name)
        self.reason = try container.decode(String.self, forKey: .reason)
        self.state = try container.decode(RequestState.self, forKey: .state)
        self.requesterPid = try container.decode(UInt32.self, forKey: .requesterPid)
        self.createdAtMs = try container.decode(Int64.self, forKey: .createdAtMs)
        self.answeredAtMs = try container.decodeIfPresent(Int64.self, forKey: .answeredAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.projectId, forKey: .projectId)
        try container.encode(self.projectName, forKey: .projectName)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.reason, forKey: .reason)
        try container.encode(self.state, forKey: .state)
        try container.encode(self.requesterPid, forKey: .requesterPid)
        try container.encode(self.createdAtMs, forKey: .createdAtMs)
        try container.encode(self.answeredAtMs, forKey: .answeredAtMs)
    }
}

public struct SecretSet: Codable, Sendable, Equatable {
    public var project: ProjectRef
    public var name: String
    public var value: SecretText

    public init(project: ProjectRef, name: String, value: SecretText) {
        self.project = project
        self.name = name
        self.value = value
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
        case value
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
        self.value = try container.decode(SecretText.self, forKey: .value)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.value, forKey: .value)
    }
}

public enum SecretSource: Codable, Sendable, Equatable {
    case own
    case sharedFrom(projectId: String, project: String)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "own":
            self = .own
        case "shared_from":
            let payload = try _SecretSourceSharedFrom(from: decoder)
            self = .sharedFrom(projectId: payload.projectId, project: payload.project)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "SecretSource")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .own:
            try container.encode("own", forKey: .tag)
        case .sharedFrom(let projectId, let project):
            try container.encode("shared_from", forKey: .tag)
            try _SecretSourceSharedFrom(projectId: projectId, project: project).encode(to: encoder)
        }
    }
}

public enum SecretState: String, Codable, Sendable, CaseIterable {
    case set
    case missing
}

public struct SecretsStatus: Codable, Sendable, Equatable {
    public var state: LockState
    public var database: String
    public var keyFile: String?
    public var pendingRequests: UInt32
    public var preservedFiles: [String]
    public var asOfSeq: UInt64

    public init(state: LockState, database: String, keyFile: String? = nil, pendingRequests: UInt32, preservedFiles: [String], asOfSeq: UInt64) {
        self.state = state
        self.database = database
        self.keyFile = keyFile
        self.pendingRequests = pendingRequests
        self.preservedFiles = preservedFiles
        self.asOfSeq = asOfSeq
    }

    private enum CodingKeys: String, CodingKey {
        case state
        case database
        case keyFile = "key_file"
        case pendingRequests = "pending_requests"
        case preservedFiles = "preserved_files"
        case asOfSeq = "as_of_seq"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.state = try container.decode(LockState.self, forKey: .state)
        self.database = try container.decode(String.self, forKey: .database)
        self.keyFile = try container.decodeIfPresent(String.self, forKey: .keyFile)
        self.pendingRequests = try container.decode(UInt32.self, forKey: .pendingRequests)
        self.preservedFiles = try container.decode([String].self, forKey: .preservedFiles)
        self.asOfSeq = try container.decode(UInt64.self, forKey: .asOfSeq)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.state, forKey: .state)
        try container.encode(self.database, forKey: .database)
        try container.encode(self.keyFile, forKey: .keyFile)
        try container.encode(self.pendingRequests, forKey: .pendingRequests)
        try container.encode(self.preservedFiles, forKey: .preservedFiles)
        try container.encode(self.asOfSeq, forKey: .asOfSeq)
    }
}

public struct SeizeBegin: Codable, Sendable, Equatable {
    public var resource: ResourceId
    public var confirmation: String
    public var request: ClaimRequest

    public init(resource: ResourceId, confirmation: String, request: ClaimRequest) {
        self.resource = resource
        self.confirmation = confirmation
        self.request = request
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case confirmation
        case request
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.confirmation = try container.decode(String.self, forKey: .confirmation)
        self.request = try container.decode(ClaimRequest.self, forKey: .request)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.confirmation, forKey: .confirmation)
        try container.encode(self.request, forKey: .request)
    }
}

public struct SeizeCommit: Codable, Sendable, Equatable {
    public var planId: String
    public var requester: ProcessIdentity
    public var evidence: [StopEvidence]

    public init(planId: String, requester: ProcessIdentity, evidence: [StopEvidence]) {
        self.planId = planId
        self.requester = requester
        self.evidence = evidence
    }

    private enum CodingKeys: String, CodingKey {
        case planId = "plan_id"
        case requester
        case evidence
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.planId = try container.decode(String.self, forKey: .planId)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
        self.evidence = try container.decode([StopEvidence].self, forKey: .evidence)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.planId, forKey: .planId)
        try container.encode(self.requester, forKey: .requester)
        try container.encode(self.evidence, forKey: .evidence)
    }
}

public struct SeizeOutcome: Codable, Sendable, Equatable {
    public var outcome: ClaimOutcome
    public var attested: Bool

    public init(outcome: ClaimOutcome, attested: Bool) {
        self.outcome = outcome
        self.attested = attested
    }

    private enum CodingKeys: String, CodingKey {
        case outcome
        case attested
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.outcome = try container.decode(ClaimOutcome.self, forKey: .outcome)
        self.attested = try container.decode(Bool.self, forKey: .attested)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.outcome, forKey: .outcome)
        try container.encode(self.attested, forKey: .attested)
    }
}

public struct SeizePlan: Codable, Sendable, Equatable {
    public var planId: String
    public var claims: [Claim]
    public var foreign: [ForeignClaim]
    public var stops: [StopTarget]
    public var expiresAtMs: Int64

    public init(planId: String, claims: [Claim], foreign: [ForeignClaim], stops: [StopTarget], expiresAtMs: Int64) {
        self.planId = planId
        self.claims = claims
        self.foreign = foreign
        self.stops = stops
        self.expiresAtMs = expiresAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case planId = "plan_id"
        case claims
        case foreign
        case stops
        case expiresAtMs = "expires_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.planId = try container.decode(String.self, forKey: .planId)
        self.claims = try container.decode([Claim].self, forKey: .claims)
        self.foreign = try container.decode([ForeignClaim].self, forKey: .foreign)
        self.stops = try container.decode([StopTarget].self, forKey: .stops)
        self.expiresAtMs = try container.decode(Int64.self, forKey: .expiresAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.planId, forKey: .planId)
        try container.encode(self.claims, forKey: .claims)
        try container.encode(self.foreign, forKey: .foreign)
        try container.encode(self.stops, forKey: .stops)
        try container.encode(self.expiresAtMs, forKey: .expiresAtMs)
    }
}

public struct SeizeProgress: Codable, Sendable, Equatable {
    public var planId: String
    public var requester: ProcessIdentity

    public init(planId: String, requester: ProcessIdentity) {
        self.planId = planId
        self.requester = requester
    }

    private enum CodingKeys: String, CodingKey {
        case planId = "plan_id"
        case requester
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.planId = try container.decode(String.self, forKey: .planId)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.planId, forKey: .planId)
        try container.encode(self.requester, forKey: .requester)
    }
}

public struct SeizeProgressReply: Codable, Sendable, Equatable {
    public var expiresAtMs: Int64

    public init(expiresAtMs: Int64) {
        self.expiresAtMs = expiresAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case expiresAtMs = "expires_at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.expiresAtMs = try container.decode(Int64.self, forKey: .expiresAtMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.expiresAtMs, forKey: .expiresAtMs)
    }
}

public enum SeizedFrom: String, Codable, Sendable, CaseIterable {
    case lease
    case hold
    case reservation
    case foreign
}

public struct SeizureRecord: Codable, Sendable, Equatable {
    public var id: String
    public var resource: String
    public var fromWorkspacePath: String
    public var fromKind: SeizedFrom
    public var fromSource: String?
    public var fromPid: UInt32?
    public var byWorkspacePath: String
    public var byPid: UInt32
    public var atMs: Int64
    public var stranded: Bool
    public var forced: Bool

    public init(id: String, resource: String, fromWorkspacePath: String, fromKind: SeizedFrom, fromSource: String? = nil, fromPid: UInt32? = nil, byWorkspacePath: String, byPid: UInt32, atMs: Int64, stranded: Bool, forced: Bool) {
        self.id = id
        self.resource = resource
        self.fromWorkspacePath = fromWorkspacePath
        self.fromKind = fromKind
        self.fromSource = fromSource
        self.fromPid = fromPid
        self.byWorkspacePath = byWorkspacePath
        self.byPid = byPid
        self.atMs = atMs
        self.stranded = stranded
        self.forced = forced
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case resource
        case fromWorkspacePath = "from_workspace_path"
        case fromKind = "from_kind"
        case fromSource = "from_source"
        case fromPid = "from_pid"
        case byWorkspacePath = "by_workspace_path"
        case byPid = "by_pid"
        case atMs = "at_ms"
        case stranded
        case forced
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.resource = try container.decode(String.self, forKey: .resource)
        self.fromWorkspacePath = try container.decode(String.self, forKey: .fromWorkspacePath)
        self.fromKind = try container.decode(SeizedFrom.self, forKey: .fromKind)
        self.fromSource = try container.decodeIfPresent(String.self, forKey: .fromSource)
        self.fromPid = try container.decodeIfPresent(UInt32.self, forKey: .fromPid)
        self.byWorkspacePath = try container.decode(String.self, forKey: .byWorkspacePath)
        self.byPid = try container.decode(UInt32.self, forKey: .byPid)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
        self.stranded = try container.decode(Bool.self, forKey: .stranded)
        self.forced = try container.decode(Bool.self, forKey: .forced)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.fromWorkspacePath, forKey: .fromWorkspacePath)
        try container.encode(self.fromKind, forKey: .fromKind)
        try container.encode(self.fromSource, forKey: .fromSource)
        try container.encode(self.fromPid, forKey: .fromPid)
        try container.encode(self.byWorkspacePath, forKey: .byWorkspacePath)
        try container.encode(self.byPid, forKey: .byPid)
        try container.encode(self.atMs, forKey: .atMs)
        try container.encode(self.stranded, forKey: .stranded)
        try container.encode(self.forced, forKey: .forced)
    }
}

public enum ClaimSelector: Codable, Sendable, Equatable {
    case exact(resource: ResourceId)
    case any(kind: ResourceKind, attributes: [String: String])

    private enum TagKeys: String, CodingKey {
        case tag = "by"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "exact":
            let payload = try _ClaimSelectorExact(from: decoder)
            self = .exact(resource: payload.resource)
        case "any":
            let payload = try _ClaimSelectorAny(from: decoder)
            self = .any(kind: payload.kind, attributes: payload.attributes)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "ClaimSelector")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .exact(let resource):
            try container.encode("exact", forKey: .tag)
            try _ClaimSelectorExact(resource: resource).encode(to: encoder)
        case .any(let kind, let attributes):
            try container.encode("any", forKey: .tag)
            try _ClaimSelectorAny(kind: kind, attributes: attributes).encode(to: encoder)
        }
    }
}

public struct ServiceStatus: Codable, Sendable, Equatable {
    public var serviceVersion: String
    public var contractVersion: UInt32
    public var pid: UInt32
    public var runningHeavy: UInt32
    public var waitingHeavy: UInt32
    public var bootId: String?
    public var exe: String?
    public var instance: String?
    public var secretsUnlocked: Bool?

    public init(serviceVersion: String, contractVersion: UInt32, pid: UInt32, runningHeavy: UInt32, waitingHeavy: UInt32, bootId: String? = nil, exe: String? = nil, instance: String? = nil, secretsUnlocked: Bool? = nil) {
        self.serviceVersion = serviceVersion
        self.contractVersion = contractVersion
        self.pid = pid
        self.runningHeavy = runningHeavy
        self.waitingHeavy = waitingHeavy
        self.bootId = bootId
        self.exe = exe
        self.instance = instance
        self.secretsUnlocked = secretsUnlocked
    }

    private enum CodingKeys: String, CodingKey {
        case serviceVersion = "service_version"
        case contractVersion = "contract_version"
        case pid
        case runningHeavy = "running_heavy"
        case waitingHeavy = "waiting_heavy"
        case bootId = "boot_id"
        case exe
        case instance
        case secretsUnlocked = "secrets_unlocked"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.serviceVersion = try container.decode(String.self, forKey: .serviceVersion)
        self.contractVersion = try container.decode(UInt32.self, forKey: .contractVersion)
        self.pid = try container.decode(UInt32.self, forKey: .pid)
        self.runningHeavy = try container.decode(UInt32.self, forKey: .runningHeavy)
        self.waitingHeavy = try container.decode(UInt32.self, forKey: .waitingHeavy)
        self.bootId = try container.decodeIfPresent(String.self, forKey: .bootId)
        self.exe = try container.decodeIfPresent(String.self, forKey: .exe)
        self.instance = try container.decodeIfPresent(String.self, forKey: .instance)
        self.secretsUnlocked = try container.decodeIfPresent(Bool.self, forKey: .secretsUnlocked)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.serviceVersion, forKey: .serviceVersion)
        try container.encode(self.contractVersion, forKey: .contractVersion)
        try container.encode(self.pid, forKey: .pid)
        try container.encode(self.runningHeavy, forKey: .runningHeavy)
        try container.encode(self.waitingHeavy, forKey: .waitingHeavy)
        try container.encodeIfPresent(self.bootId, forKey: .bootId)
        try container.encodeIfPresent(self.exe, forKey: .exe)
        try container.encodeIfPresent(self.instance, forKey: .instance)
        try container.encodeIfPresent(self.secretsUnlocked, forKey: .secretsUnlocked)
    }
}

public struct SetOutcome: Codable, Sendable, Equatable {
    public var name: String
    public var answeredRequests: [String]

    public init(name: String, answeredRequests: [String]) {
        self.name = name
        self.answeredRequests = answeredRequests
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case answeredRequests = "answered_requests"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.answeredRequests = try container.decode([String].self, forKey: .answeredRequests)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.answeredRequests, forKey: .answeredRequests)
    }
}

public struct Share: Codable, Sendable, Equatable {
    public var from: ProjectRef
    public var name: String
    public var toProjectId: String

    public init(from: ProjectRef, name: String, toProjectId: String) {
        self.from = from
        self.name = name
        self.toProjectId = toProjectId
    }

    private enum CodingKeys: String, CodingKey {
        case from
        case name
        case toProjectId = "to_project_id"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.from = try container.decode(ProjectRef.self, forKey: .from)
        self.name = try container.decode(String.self, forKey: .name)
        self.toProjectId = try container.decode(String.self, forKey: .toProjectId)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.from, forKey: .from)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.toProjectId, forKey: .toProjectId)
    }
}

public struct ShareOutcome: Codable, Sendable, Equatable {
    public var name: String
    public var from: String
    public var to: String

    public init(name: String, from: String, to: String) {
        self.name = name
        self.from = from
        self.to = to
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case from
        case to
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.from = try container.decode(String.self, forKey: .from)
        self.to = try container.decode(String.self, forKey: .to)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.from, forKey: .from)
        try container.encode(self.to, forKey: .to)
    }
}

public struct ShutdownAccepted: Codable, Sendable, Equatable {
    public var stopping: Bool

    public init(stopping: Bool) {
        self.stopping = stopping
    }

    private enum CodingKeys: String, CodingKey {
        case stopping
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.stopping = try container.decode(Bool.self, forKey: .stopping)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.stopping, forKey: .stopping)
    }
}

public enum StopEvidence: Codable, Sendable, Equatable {
    case gone(target: UInt)
    case jobGone(target: UInt, session: UInt32)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "gone":
            let payload = try _StopEvidenceGone(from: decoder)
            self = .gone(target: payload.target)
        case "job_gone":
            let payload = try _StopEvidenceJobGone(from: decoder)
            self = .jobGone(target: payload.target, session: payload.session)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "StopEvidence")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .gone(let target):
            try container.encode("gone", forKey: .tag)
            try _StopEvidenceGone(target: target).encode(to: encoder)
        case .jobGone(let target, let session):
            try container.encode("job_gone", forKey: .tag)
            try _StopEvidenceJobGone(target: target, session: session).encode(to: encoder)
        }
    }
}

public enum StopTarget: Codable, Sendable, Equatable {
    case runGroup(claimId: String, holder: ProcessIdentity, child: ProcessIdentity?, group: RunGroup?)
    case process(claimId: String, process: ProcessIdentity)
    case foreign(source: String, resource: ResourceId, data: JSONValue)

    private enum TagKeys: String, CodingKey {
        case tag = "kind"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: TagKeys.self)
        switch try container.decode(String.self, forKey: .tag) {
        case "run_group":
            let payload = try _StopTargetRunGroup(from: decoder)
            self = .runGroup(claimId: payload.claimId, holder: payload.holder, child: payload.child, group: payload.group)
        case "process":
            let payload = try _StopTargetProcess(from: decoder)
            self = .process(claimId: payload.claimId, process: payload.process)
        case "foreign":
            let payload = try _StopTargetForeign(from: decoder)
            self = .foreign(source: payload.source, resource: payload.resource, data: payload.data)
        default:
            throw ContractCoding.unknownTag(forKey: .tag, in: container, type: "StopTarget")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case .runGroup(let claimId, let holder, let child, let group):
            try container.encode("run_group", forKey: .tag)
            try _StopTargetRunGroup(claimId: claimId, holder: holder, child: child, group: group).encode(to: encoder)
        case .process(let claimId, let process):
            try container.encode("process", forKey: .tag)
            try _StopTargetProcess(claimId: claimId, process: process).encode(to: encoder)
        case .foreign(let source, let resource, let data):
            try container.encode("foreign", forKey: .tag)
            try _StopTargetForeign(source: source, resource: resource, data: data).encode(to: encoder)
        }
    }
}

public struct Telemetry: Codable, Sendable, Equatable {
    public var batteryPercent: ReadingUInt8
    public var charging: ReadingBool
    public var wifi: ReadingBool
    public var wifiName: String?
    public var cellularData: ReadingBool
    public var vpn: ReadingBool
    public var atMs: Int64

    public init(batteryPercent: ReadingUInt8, charging: ReadingBool, wifi: ReadingBool, wifiName: String? = nil, cellularData: ReadingBool, vpn: ReadingBool, atMs: Int64) {
        self.batteryPercent = batteryPercent
        self.charging = charging
        self.wifi = wifi
        self.wifiName = wifiName
        self.cellularData = cellularData
        self.vpn = vpn
        self.atMs = atMs
    }

    private enum CodingKeys: String, CodingKey {
        case batteryPercent = "battery_percent"
        case charging
        case wifi
        case wifiName = "wifi_name"
        case cellularData = "cellular_data"
        case vpn
        case atMs = "at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.batteryPercent = try container.decode(ReadingUInt8.self, forKey: .batteryPercent)
        self.charging = try container.decode(ReadingBool.self, forKey: .charging)
        self.wifi = try container.decode(ReadingBool.self, forKey: .wifi)
        self.wifiName = try container.decodeIfPresent(String.self, forKey: .wifiName)
        self.cellularData = try container.decode(ReadingBool.self, forKey: .cellularData)
        self.vpn = try container.decode(ReadingBool.self, forKey: .vpn)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.batteryPercent, forKey: .batteryPercent)
        try container.encode(self.charging, forKey: .charging)
        try container.encode(self.wifi, forKey: .wifi)
        try container.encode(self.wifiName, forKey: .wifiName)
        try container.encode(self.cellularData, forKey: .cellularData)
        try container.encode(self.vpn, forKey: .vpn)
        try container.encode(self.atMs, forKey: .atMs)
    }
}

public struct ToolPathsSet: Codable, Sendable, Equatable {
    public var adb: String?
    public var agentDevice: String?
    public var node: String?
    public var detectedScript: String?
    public var explicitAgentDevice: Bool
    public var androidEnv: [String: String]

    public init(adb: String? = nil, agentDevice: String? = nil, node: String? = nil, detectedScript: String? = nil, explicitAgentDevice: Bool, androidEnv: [String: String]) {
        self.adb = adb
        self.agentDevice = agentDevice
        self.node = node
        self.detectedScript = detectedScript
        self.explicitAgentDevice = explicitAgentDevice
        self.androidEnv = androidEnv
    }

    private enum CodingKeys: String, CodingKey {
        case adb
        case agentDevice = "agent_device"
        case node
        case detectedScript = "detected_script"
        case explicitAgentDevice = "explicit_agent_device"
        case androidEnv = "android_env"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.adb = try container.decodeIfPresent(String.self, forKey: .adb)
        self.agentDevice = try container.decodeIfPresent(String.self, forKey: .agentDevice)
        self.node = try container.decodeIfPresent(String.self, forKey: .node)
        self.detectedScript = try container.decodeIfPresent(String.self, forKey: .detectedScript)
        self.explicitAgentDevice = try container.decode(Bool.self, forKey: .explicitAgentDevice)
        self.androidEnv = try container.decode([String: String].self, forKey: .androidEnv)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.adb, forKey: .adb)
        try container.encode(self.agentDevice, forKey: .agentDevice)
        try container.encode(self.node, forKey: .node)
        try container.encode(self.detectedScript, forKey: .detectedScript)
        try container.encode(self.explicitAgentDevice, forKey: .explicitAgentDevice)
        try container.encode(self.androidEnv, forKey: .androidEnv)
    }
}

public struct ToolStatus: Codable, Sendable, Equatable {
    public var adb: String?
    public var adbState: AdbState
    public var emulator: String?
    public var xcrun: Bool
    public var ideviceinfo: String?
    public var agentDevice: String?
    public var node: String?
    public var agentDeviceInUse: Bool
    public var sessionsKnown: Bool
    public var sessionsLastGoodMs: Int64?
    public var sessionsReason: String?
    public var problems: [String]

    public init(adb: String? = nil, adbState: AdbState, emulator: String? = nil, xcrun: Bool, ideviceinfo: String? = nil, agentDevice: String? = nil, node: String? = nil, agentDeviceInUse: Bool, sessionsKnown: Bool, sessionsLastGoodMs: Int64? = nil, sessionsReason: String? = nil, problems: [String]) {
        self.adb = adb
        self.adbState = adbState
        self.emulator = emulator
        self.xcrun = xcrun
        self.ideviceinfo = ideviceinfo
        self.agentDevice = agentDevice
        self.node = node
        self.agentDeviceInUse = agentDeviceInUse
        self.sessionsKnown = sessionsKnown
        self.sessionsLastGoodMs = sessionsLastGoodMs
        self.sessionsReason = sessionsReason
        self.problems = problems
    }

    private enum CodingKeys: String, CodingKey {
        case adb
        case adbState = "adb_state"
        case emulator
        case xcrun
        case ideviceinfo
        case agentDevice = "agent_device"
        case node
        case agentDeviceInUse = "agent_device_in_use"
        case sessionsKnown = "sessions_known"
        case sessionsLastGoodMs = "sessions_last_good_ms"
        case sessionsReason = "sessions_reason"
        case problems
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.adb = try container.decodeIfPresent(String.self, forKey: .adb)
        self.adbState = try container.decode(AdbState.self, forKey: .adbState)
        self.emulator = try container.decodeIfPresent(String.self, forKey: .emulator)
        self.xcrun = try container.decode(Bool.self, forKey: .xcrun)
        self.ideviceinfo = try container.decodeIfPresent(String.self, forKey: .ideviceinfo)
        self.agentDevice = try container.decodeIfPresent(String.self, forKey: .agentDevice)
        self.node = try container.decodeIfPresent(String.self, forKey: .node)
        self.agentDeviceInUse = try container.decode(Bool.self, forKey: .agentDeviceInUse)
        self.sessionsKnown = try container.decode(Bool.self, forKey: .sessionsKnown)
        self.sessionsLastGoodMs = try container.decodeIfPresent(Int64.self, forKey: .sessionsLastGoodMs)
        self.sessionsReason = try container.decodeIfPresent(String.self, forKey: .sessionsReason)
        self.problems = try container.decode([String].self, forKey: .problems)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.adb, forKey: .adb)
        try container.encode(self.adbState, forKey: .adbState)
        try container.encode(self.emulator, forKey: .emulator)
        try container.encode(self.xcrun, forKey: .xcrun)
        try container.encode(self.ideviceinfo, forKey: .ideviceinfo)
        try container.encode(self.agentDevice, forKey: .agentDevice)
        try container.encode(self.node, forKey: .node)
        try container.encode(self.agentDeviceInUse, forKey: .agentDeviceInUse)
        try container.encode(self.sessionsKnown, forKey: .sessionsKnown)
        try container.encode(self.sessionsLastGoodMs, forKey: .sessionsLastGoodMs)
        try container.encode(self.sessionsReason, forKey: .sessionsReason)
        try container.encode(self.problems, forKey: .problems)
    }
}

public struct Transport: Codable, Sendable, Equatable {
    public var kind: TransportKind
    public var address: String
    public var state: String

    public init(kind: TransportKind, address: String, state: String) {
        self.kind = kind
        self.address = address
        self.state = state
    }

    private enum CodingKeys: String, CodingKey {
        case kind
        case address
        case state
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.kind = try container.decode(TransportKind.self, forKey: .kind)
        self.address = try container.decode(String.self, forKey: .address)
        self.state = try container.decode(String.self, forKey: .state)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.address, forKey: .address)
        try container.encode(self.state, forKey: .state)
    }
}

public enum TransportKind: String, Codable, Sendable, CaseIterable {
    case usb
    case wifi
    case emulator
    case simulator
    case wired
    case network
}

public struct UnidentifiedTransport: Codable, Sendable, Equatable {
    public var platform: Platform
    public var transport: Transport
    public var model: String?
    public var hint: String?

    public init(platform: Platform, transport: Transport, model: String? = nil, hint: String? = nil) {
        self.platform = platform
        self.transport = transport
        self.model = model
        self.hint = hint
    }

    private enum CodingKeys: String, CodingKey {
        case platform
        case transport
        case model
        case hint
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.platform = try container.decode(Platform.self, forKey: .platform)
        self.transport = try container.decode(Transport.self, forKey: .transport)
        self.model = try container.decodeIfPresent(String.self, forKey: .model)
        self.hint = try container.decodeIfPresent(String.self, forKey: .hint)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.platform, forKey: .platform)
        try container.encode(self.transport, forKey: .transport)
        try container.encode(self.model, forKey: .model)
        try container.encode(self.hint, forKey: .hint)
    }
}

public struct Unlock: Codable, Sendable, Equatable {
    public var password: SecretText
    public var keyFile: String?
    public var forgetKeyFile: Bool
    public var create: Bool

    public init(password: SecretText, keyFile: String? = nil, forgetKeyFile: Bool, create: Bool) {
        self.password = password
        self.keyFile = keyFile
        self.forgetKeyFile = forgetKeyFile
        self.create = create
    }

    private enum CodingKeys: String, CodingKey {
        case password
        case keyFile = "key_file"
        case forgetKeyFile = "forget_key_file"
        case create
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.password = try container.decode(SecretText.self, forKey: .password)
        self.keyFile = try container.decodeIfPresent(String.self, forKey: .keyFile)
        self.forgetKeyFile = try container.decode(Bool.self, forKey: .forgetKeyFile)
        self.create = try container.decode(Bool.self, forKey: .create)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.password, forKey: .password)
        try container.encode(self.keyFile, forKey: .keyFile)
        try container.encode(self.forgetKeyFile, forKey: .forgetKeyFile)
        try container.encode(self.create, forKey: .create)
    }
}

public struct UnlockOutcome: Codable, Sendable, Equatable {
    public var status: SecretsStatus
    public var grant: ProofGrant

    public init(status: SecretsStatus, grant: ProofGrant) {
        self.status = status
        self.grant = grant
    }

    private enum CodingKeys: String, CodingKey {
        case status
        case grant
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.status = try container.decode(SecretsStatus.self, forKey: .status)
        self.grant = try container.decode(ProofGrant.self, forKey: .grant)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.status, forKey: .status)
        try container.encode(self.grant, forKey: .grant)
    }
}

public struct Unshared: Codable, Sendable, Equatable {
    public var unshared: Bool

    public init(unshared: Bool) {
        self.unshared = unshared
    }

    private enum CodingKeys: String, CodingKey {
        case unshared
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.unshared = try container.decode(Bool.self, forKey: .unshared)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.unshared, forKey: .unshared)
    }
}

public struct WaitingRequest: Codable, Sendable, Equatable {
    public var requestId: String
    public var ticket: UInt64
    public var workspacePath: String
    public var kind: ClaimKind
    public var sinceMs: Int64

    public init(requestId: String, ticket: UInt64, workspacePath: String, kind: ClaimKind, sinceMs: Int64) {
        self.requestId = requestId
        self.ticket = ticket
        self.workspacePath = workspacePath
        self.kind = kind
        self.sinceMs = sinceMs
    }

    private enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
        case ticket
        case workspacePath = "workspace_path"
        case kind
        case sinceMs = "since_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.requestId = try container.decode(String.self, forKey: .requestId)
        self.ticket = try container.decode(UInt64.self, forKey: .ticket)
        self.workspacePath = try container.decode(String.self, forKey: .workspacePath)
        self.kind = try container.decode(ClaimKind.self, forKey: .kind)
        self.sinceMs = try container.decode(Int64.self, forKey: .sinceMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.requestId, forKey: .requestId)
        try container.encode(self.ticket, forKey: .ticket)
        try container.encode(self.workspacePath, forKey: .workspacePath)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.sinceMs, forKey: .sinceMs)
    }
}

public enum Who: String, Codable, Sendable, CaseIterable {
    case agent
    case person
}

public struct WorkspaceInfo: Codable, Sendable, Equatable {
    public var id: String
    public var projectId: String
    public var path: String
    public var outcome: WorkspaceOutcome
    public var projectRepointed: Bool
    public var note: String?
    public var lastSeenMs: Int64

    public init(id: String, projectId: String, path: String, outcome: WorkspaceOutcome, projectRepointed: Bool, note: String? = nil, lastSeenMs: Int64) {
        self.id = id
        self.projectId = projectId
        self.path = path
        self.outcome = outcome
        self.projectRepointed = projectRepointed
        self.note = note
        self.lastSeenMs = lastSeenMs
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case projectId = "project_id"
        case path
        case outcome
        case projectRepointed = "project_repointed"
        case note
        case lastSeenMs = "last_seen_ms"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.projectId = try container.decode(String.self, forKey: .projectId)
        self.path = try container.decode(String.self, forKey: .path)
        self.outcome = try container.decode(WorkspaceOutcome.self, forKey: .outcome)
        self.projectRepointed = try container.decode(Bool.self, forKey: .projectRepointed)
        self.note = try container.decodeIfPresent(String.self, forKey: .note)
        self.lastSeenMs = try container.decode(Int64.self, forKey: .lastSeenMs)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.projectId, forKey: .projectId)
        try container.encode(self.path, forKey: .path)
        try container.encode(self.outcome, forKey: .outcome)
        try container.encode(self.projectRepointed, forKey: .projectRepointed)
        try container.encode(self.note, forKey: .note)
        try container.encode(self.lastSeenMs, forKey: .lastSeenMs)
    }
}

public enum WorkspaceOutcome: String, Codable, Sendable, CaseIterable {
    case registered
    case known
    case moved
    case reidentified
}

public struct WorkspaceRef: Codable, Sendable, Equatable {
    public var id: String
    public var path: String

    public init(id: String, path: String) {
        self.id = id
        self.path = path
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case path
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.path = try container.decode(String.self, forKey: .path)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.path, forKey: .path)
    }
}

public struct WorkspaceResolve: Codable, Sendable, Equatable {
    public var project: ProjectRef
    public var root: String
    public var declared: ProjectDecl?

    public init(project: ProjectRef, root: String, declared: ProjectDecl? = nil) {
        self.project = project
        self.root = root
        self.declared = declared
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case root
        case declared
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.root = try container.decode(String.self, forKey: .root)
        self.declared = try container.decodeIfPresent(ProjectDecl.self, forKey: .declared)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.root, forKey: .root)
        try container.encode(self.declared, forKey: .declared)
    }
}

// MARK: - The payloads of enum variants with fields

fileprivate struct _AcquireOutcomeGranted: Codable, Sendable, Equatable {
    fileprivate var entry: HeavyEntry

    fileprivate init(entry: HeavyEntry) {
        self.entry = entry
    }

    private enum CodingKeys: String, CodingKey {
        case entry
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.entry = try container.decode(HeavyEntry.self, forKey: .entry)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.entry, forKey: .entry)
    }
}

fileprivate struct _AcquireOutcomeQueued: Codable, Sendable, Equatable {
    fileprivate var entry: HeavyEntry
    fileprivate var position: UInt32
    fileprivate var of: UInt32

    fileprivate init(entry: HeavyEntry, position: UInt32, of: UInt32) {
        self.entry = entry
        self.position = position
        self.of = of
    }

    private enum CodingKeys: String, CodingKey {
        case entry
        case position
        case of
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.entry = try container.decode(HeavyEntry.self, forKey: .entry)
        self.position = try container.decode(UInt32.self, forKey: .position)
        self.of = try container.decode(UInt32.self, forKey: .of)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.entry, forKey: .entry)
        try container.encode(self.position, forKey: .position)
        try container.encode(self.of, forKey: .of)
    }
}

fileprivate struct _AcquireOutcomeBusy: Codable, Sendable, Equatable {
    fileprivate var position: UInt32
    fileprivate var of: UInt32

    fileprivate init(position: UInt32, of: UInt32) {
        self.position = position
        self.of = of
    }

    private enum CodingKeys: String, CodingKey {
        case position
        case of
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.position = try container.decode(UInt32.self, forKey: .position)
        self.of = try container.decode(UInt32.self, forKey: .of)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.position, forKey: .position)
        try container.encode(self.of, forKey: .of)
    }
}

fileprivate struct _AcquireOutcomeFinished: Codable, Sendable, Equatable {
    fileprivate var item: HeavyHistoryItem

    fileprivate init(item: HeavyHistoryItem) {
        self.item = item
    }

    private enum CodingKeys: String, CodingKey {
        case item
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.item = try container.decode(HeavyHistoryItem.self, forKey: .item)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.item, forKey: .item)
    }
}

fileprivate struct _AdbStateForeign: Codable, Sendable, Equatable {
    fileprivate var version: UInt32

    fileprivate init(version: UInt32) {
        self.version = version
    }

    private enum CodingKeys: String, CodingKey {
        case version
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.version = try container.decode(UInt32.self, forKey: .version)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.version, forKey: .version)
    }
}

fileprivate struct _BlockerClaim: Codable, Sendable, Equatable {
    fileprivate var summary: ClaimSummary

    fileprivate init(summary: ClaimSummary) {
        self.summary = summary
    }

    private enum CodingKeys: String, CodingKey {
        case summary
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.summary = try container.decode(ClaimSummary.self, forKey: .summary)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.summary, forKey: .summary)
    }
}

fileprivate struct _BlockerForeign: Codable, Sendable, Equatable {
    fileprivate var claim: ForeignClaim

    fileprivate init(claim: ForeignClaim) {
        self.claim = claim
    }

    private enum CodingKeys: String, CodingKey {
        case claim
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claim = try container.decode(ForeignClaim.self, forKey: .claim)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claim, forKey: .claim)
    }
}

fileprivate struct _BlockerSeizing: Codable, Sendable, Equatable {
    fileprivate var byWorkspace: String

    fileprivate init(byWorkspace: String) {
        self.byWorkspace = byWorkspace
    }

    private enum CodingKeys: String, CodingKey {
        case byWorkspace = "by_workspace"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.byWorkspace = try container.decode(String.self, forKey: .byWorkspace)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.byWorkspace, forKey: .byWorkspace)
    }
}

fileprivate struct _BlockerChecking: Codable, Sendable, Equatable {
    fileprivate var resource: String
    fileprivate var text: String

    fileprivate init(resource: String, text: String) {
        self.resource = resource
        self.text = text
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case text
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(String.self, forKey: .resource)
        self.text = try container.decode(String.self, forKey: .text)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.text, forKey: .text)
    }
}

fileprivate struct _ClaimOutcomeGranted: Codable, Sendable, Equatable {
    fileprivate var claim: Claim

    fileprivate init(claim: Claim) {
        self.claim = claim
    }

    private enum CodingKeys: String, CodingKey {
        case claim
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claim = try container.decode(Claim.self, forKey: .claim)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claim, forKey: .claim)
    }
}

fileprivate struct _ClaimOutcomeJoined: Codable, Sendable, Equatable {
    fileprivate var claim: Claim

    fileprivate init(claim: Claim) {
        self.claim = claim
    }

    private enum CodingKeys: String, CodingKey {
        case claim
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claim = try container.decode(Claim.self, forKey: .claim)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claim, forKey: .claim)
    }
}

fileprivate struct _ClaimOutcomeReentrant: Codable, Sendable, Equatable {
    fileprivate var claimId: String

    fileprivate init(claimId: String) {
        self.claimId = claimId
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
    }
}

fileprivate struct _ClaimOutcomeQueued: Codable, Sendable, Equatable {
    fileprivate var requestId: String
    fileprivate var position: UInt32
    fileprivate var of: UInt32
    fileprivate var blockers: [Blocker]

    fileprivate init(requestId: String, position: UInt32, of: UInt32, blockers: [Blocker]) {
        self.requestId = requestId
        self.position = position
        self.of = of
        self.blockers = blockers
    }

    private enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
        case position
        case of
        case blockers
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.requestId = try container.decode(String.self, forKey: .requestId)
        self.position = try container.decode(UInt32.self, forKey: .position)
        self.of = try container.decode(UInt32.self, forKey: .of)
        self.blockers = try container.decode([Blocker].self, forKey: .blockers)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.requestId, forKey: .requestId)
        try container.encode(self.position, forKey: .position)
        try container.encode(self.of, forKey: .of)
        try container.encode(self.blockers, forKey: .blockers)
    }
}

fileprivate struct _ClaimOutcomeBusy: Codable, Sendable, Equatable {
    fileprivate var position: UInt32
    fileprivate var of: UInt32
    fileprivate var blockers: [Blocker]

    fileprivate init(position: UInt32, of: UInt32, blockers: [Blocker]) {
        self.position = position
        self.of = of
        self.blockers = blockers
    }

    private enum CodingKeys: String, CodingKey {
        case position
        case of
        case blockers
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.position = try container.decode(UInt32.self, forKey: .position)
        self.of = try container.decode(UInt32.self, forKey: .of)
        self.blockers = try container.decode([Blocker].self, forKey: .blockers)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.position, forKey: .position)
        try container.encode(self.of, forKey: .of)
        try container.encode(self.blockers, forKey: .blockers)
    }
}

fileprivate struct _ClaimOutcomeFinished: Codable, Sendable, Equatable {
    fileprivate var item: ClaimHistoryItem

    fileprivate init(item: ClaimHistoryItem) {
        self.item = item
    }

    private enum CodingKeys: String, CodingKey {
        case item
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.item = try container.decode(ClaimHistoryItem.self, forKey: .item)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.item, forKey: .item)
    }
}

fileprivate struct _EventResync: Codable, Sendable, Equatable {
    fileprivate var reason: ResyncReason

    fileprivate init(reason: ResyncReason) {
        self.reason = reason
    }

    private enum CodingKeys: String, CodingKey {
        case reason
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.reason = try container.decode(ResyncReason.self, forKey: .reason)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.reason, forKey: .reason)
    }
}

fileprivate struct _HoldBindingTtl: Codable, Sendable, Equatable {
    fileprivate var ttlMs: UInt64
    fileprivate var expiresAtMs: Int64

    fileprivate init(ttlMs: UInt64, expiresAtMs: Int64) {
        self.ttlMs = ttlMs
        self.expiresAtMs = expiresAtMs
    }

    private enum CodingKeys: String, CodingKey {
        case ttlMs = "ttl_ms"
        case expiresAtMs = "expires_at_ms"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.ttlMs = try container.decode(UInt64.self, forKey: .ttlMs)
        self.expiresAtMs = try container.decode(Int64.self, forKey: .expiresAtMs)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.ttlMs, forKey: .ttlMs)
        try container.encode(self.expiresAtMs, forKey: .expiresAtMs)
    }
}

fileprivate struct _HoldBindingProcess: Codable, Sendable, Equatable {
    fileprivate var holder: ProcessIdentity

    fileprivate init(holder: ProcessIdentity) {
        self.holder = holder
    }

    private enum CodingKeys: String, CodingKey {
        case holder
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.holder = try container.decode(ProcessIdentity.self, forKey: .holder)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.holder, forKey: .holder)
    }
}

fileprivate struct _InstallSourceClaim: Codable, Sendable, Equatable {
    fileprivate var claimIds: [String]
    fileprivate var workspacePath: String
    fileprivate var branch: String?
    fileprivate var commit: String?
    fileprivate var command: String?

    fileprivate init(claimIds: [String], workspacePath: String, branch: String? = nil, commit: String? = nil, command: String? = nil) {
        self.claimIds = claimIds
        self.workspacePath = workspacePath
        self.branch = branch
        self.commit = commit
        self.command = command
    }

    private enum CodingKeys: String, CodingKey {
        case claimIds = "claim_ids"
        case workspacePath = "workspace_path"
        case branch
        case commit
        case command
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimIds = try container.decode([String].self, forKey: .claimIds)
        self.workspacePath = try container.decode(String.self, forKey: .workspacePath)
        self.branch = try container.decodeIfPresent(String.self, forKey: .branch)
        self.commit = try container.decodeIfPresent(String.self, forKey: .commit)
        self.command = try container.decodeIfPresent(String.self, forKey: .command)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimIds, forKey: .claimIds)
        try container.encode(self.workspacePath, forKey: .workspacePath)
        try container.encode(self.branch, forKey: .branch)
        try container.encode(self.commit, forKey: .commit)
        try container.encode(self.command, forKey: .command)
    }
}

fileprivate struct _InstallSourceAround: Codable, Sendable, Equatable {
    fileprivate var claimIds: [String]
    fileprivate var workspacePath: String

    fileprivate init(claimIds: [String], workspacePath: String) {
        self.claimIds = claimIds
        self.workspacePath = workspacePath
    }

    private enum CodingKeys: String, CodingKey {
        case claimIds = "claim_ids"
        case workspacePath = "workspace_path"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimIds = try container.decode([String].self, forKey: .claimIds)
        self.workspacePath = try container.decode(String.self, forKey: .workspacePath)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimIds, forKey: .claimIds)
        try container.encode(self.workspacePath, forKey: .workspacePath)
    }
}

fileprivate struct _InstallTimeDevice: Codable, Sendable, Equatable {
    fileprivate var atMs: Int64

    fileprivate init(atMs: Int64) {
        self.atMs = atMs
    }

    private enum CodingKeys: String, CodingKey {
        case atMs = "at_ms"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.atMs, forKey: .atMs)
    }
}

fileprivate struct _InstallTimeFirstSeen: Codable, Sendable, Equatable {
    fileprivate var atMs: Int64

    fileprivate init(atMs: Int64) {
        self.atMs = atMs
    }

    private enum CodingKeys: String, CodingKey {
        case atMs = "at_ms"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.atMs = try container.decode(Int64.self, forKey: .atMs)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.atMs, forKey: .atMs)
    }
}

fileprivate struct _InstallTimeChangedBetween: Codable, Sendable, Equatable {
    fileprivate var fromMs: Int64
    fileprivate var toMs: Int64

    fileprivate init(fromMs: Int64, toMs: Int64) {
        self.fromMs = fromMs
        self.toMs = toMs
    }

    private enum CodingKeys: String, CodingKey {
        case fromMs = "from_ms"
        case toMs = "to_ms"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.fromMs = try container.decode(Int64.self, forKey: .fromMs)
        self.toMs = try container.decode(Int64.self, forKey: .toMs)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.fromMs, forKey: .fromMs)
        try container.encode(self.toMs, forKey: .toMs)
    }
}

fileprivate struct _PurposeLease: Codable, Sendable, Equatable {
    fileprivate var command: String

    fileprivate init(command: String) {
        self.command = command
    }

    private enum CodingKeys: String, CodingKey {
        case command
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.command = try container.decode(String.self, forKey: .command)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.command, forKey: .command)
    }
}

fileprivate struct _PurposeHold: Codable, Sendable, Equatable {
    fileprivate var ttlMs: UInt64?
    fileprivate var boundTo: ProcessIdentity?
    fileprivate var distinct: Bool

    fileprivate init(ttlMs: UInt64? = nil, boundTo: ProcessIdentity? = nil, distinct: Bool) {
        self.ttlMs = ttlMs
        self.boundTo = boundTo
        self.distinct = distinct
    }

    private enum CodingKeys: String, CodingKey {
        case ttlMs = "ttl_ms"
        case boundTo = "bound_to"
        case distinct
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.ttlMs = try container.decodeIfPresent(UInt64.self, forKey: .ttlMs)
        self.boundTo = try container.decodeIfPresent(ProcessIdentity.self, forKey: .boundTo)
        self.distinct = try container.decode(Bool.self, forKey: .distinct)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.ttlMs, forKey: .ttlMs)
        try container.encode(self.boundTo, forKey: .boundTo)
        try container.encode(self.distinct, forKey: .distinct)
    }
}

fileprivate struct _RequestHello: Codable, Sendable, Equatable {
    fileprivate var contractVersion: UInt32
    fileprivate var client: String

    fileprivate init(contractVersion: UInt32, client: String) {
        self.contractVersion = contractVersion
        self.client = client
    }

    private enum CodingKeys: String, CodingKey {
        case contractVersion = "contract_version"
        case client
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.contractVersion = try container.decode(UInt32.self, forKey: .contractVersion)
        self.client = try container.decode(String.self, forKey: .client)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.contractVersion, forKey: .contractVersion)
        try container.encode(self.client, forKey: .client)
    }
}

fileprivate struct _RequestServiceShutdown: Codable, Sendable, Equatable {
    fileprivate var ifIdle: Bool
    fileprivate var refuseIfUnlocked: Bool

    fileprivate init(ifIdle: Bool, refuseIfUnlocked: Bool = false) {
        self.ifIdle = ifIdle
        self.refuseIfUnlocked = refuseIfUnlocked
    }

    private enum CodingKeys: String, CodingKey {
        case ifIdle = "if_idle"
        case refuseIfUnlocked = "refuse_if_unlocked"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.ifIdle = try container.decode(Bool.self, forKey: .ifIdle)
        self.refuseIfUnlocked = try container.decodeIfPresent(Bool.self, forKey: .refuseIfUnlocked) ?? false
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.ifIdle, forKey: .ifIdle)
        if self.refuseIfUnlocked != false { try container.encode(self.refuseIfUnlocked, forKey: .refuseIfUnlocked) }
    }
}

fileprivate struct _RequestHeavyAttach: Codable, Sendable, Equatable {
    fileprivate var id: String

    fileprivate init(id: String) {
        self.id = id
    }

    private enum CodingKeys: String, CodingKey {
        case id
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
    }
}

fileprivate struct _RequestHeavyWithdraw: Codable, Sendable, Equatable {
    fileprivate var id: String
    fileprivate var timedOut: Bool

    fileprivate init(id: String, timedOut: Bool) {
        self.id = id
        self.timedOut = timedOut
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case timedOut = "timed_out"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.timedOut = try container.decode(Bool.self, forKey: .timedOut)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.timedOut, forKey: .timedOut)
    }
}

fileprivate struct _RequestHeavyForceRelease: Codable, Sendable, Equatable {
    fileprivate var id: String

    fileprivate init(id: String) {
        self.id = id
    }

    private enum CodingKeys: String, CodingKey {
        case id
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
    }
}

fileprivate struct _RequestQueueSetCapacity: Codable, Sendable, Equatable {
    fileprivate var capacity: UInt8

    fileprivate init(capacity: UInt8) {
        self.capacity = capacity
    }

    private enum CodingKeys: String, CodingKey {
        case capacity
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.capacity = try container.decode(UInt8.self, forKey: .capacity)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.capacity, forKey: .capacity)
    }
}

fileprivate struct _RequestSecretList: Codable, Sendable, Equatable {
    fileprivate var project: ProjectRef
    fileprivate var declared: [DeclaredSecret]

    fileprivate init(project: ProjectRef, declared: [DeclaredSecret]) {
        self.project = project
        self.declared = declared
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case declared
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.declared = try container.decode([DeclaredSecret].self, forKey: .declared)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.declared, forKey: .declared)
    }
}

fileprivate struct _RequestSecretRemove: Codable, Sendable, Equatable {
    fileprivate var project: ProjectRef
    fileprivate var name: String

    fileprivate init(project: ProjectRef, name: String) {
        self.project = project
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
    }
}

fileprivate struct _RequestSecretRequestWithdraw: Codable, Sendable, Equatable {
    fileprivate var id: String
    fileprivate var timedOut: Bool

    fileprivate init(id: String, timedOut: Bool) {
        self.id = id
        self.timedOut = timedOut
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case timedOut = "timed_out"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.timedOut = try container.decode(Bool.self, forKey: .timedOut)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.timedOut, forKey: .timedOut)
    }
}

fileprivate struct _RequestClaimsAttach: Codable, Sendable, Equatable {
    fileprivate var requestId: String

    fileprivate init(requestId: String) {
        self.requestId = requestId
    }

    private enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.requestId = try container.decode(String.self, forKey: .requestId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.requestId, forKey: .requestId)
    }
}

fileprivate struct _RequestClaimsForceRelease: Codable, Sendable, Equatable {
    fileprivate var claimId: String
    fileprivate var confirmation: String
    fileprivate var workspaceId: String
    fileprivate var requester: ProcessIdentity

    fileprivate init(claimId: String, confirmation: String, workspaceId: String, requester: ProcessIdentity) {
        self.claimId = claimId
        self.confirmation = confirmation
        self.workspaceId = workspaceId
        self.requester = requester
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case confirmation
        case workspaceId = "workspace_id"
        case requester
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.confirmation = try container.decode(String.self, forKey: .confirmation)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
        self.requester = try container.decode(ProcessIdentity.self, forKey: .requester)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.confirmation, forKey: .confirmation)
        try container.encode(self.workspaceId, forKey: .workspaceId)
        try container.encode(self.requester, forKey: .requester)
    }
}

fileprivate struct _RequestClaimsWithdraw: Codable, Sendable, Equatable {
    fileprivate var requestId: String
    fileprivate var timedOut: Bool

    fileprivate init(requestId: String, timedOut: Bool) {
        self.requestId = requestId
        self.timedOut = timedOut
    }

    private enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
        case timedOut = "timed_out"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.requestId = try container.decode(String.self, forKey: .requestId)
        self.timedOut = try container.decode(Bool.self, forKey: .timedOut)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.requestId, forKey: .requestId)
        try container.encode(self.timedOut, forKey: .timedOut)
    }
}

fileprivate struct _RequestClaimsRenew: Codable, Sendable, Equatable {
    fileprivate var claimId: String
    fileprivate var workspaceId: String

    fileprivate init(claimId: String, workspaceId: String) {
        self.claimId = claimId
        self.workspaceId = workspaceId
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case workspaceId = "workspace_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.workspaceId, forKey: .workspaceId)
    }
}

fileprivate struct _RequestClaimsSetCommand: Codable, Sendable, Equatable {
    fileprivate var claimId: String
    fileprivate var workspaceId: String
    fileprivate var command: String

    fileprivate init(claimId: String, workspaceId: String, command: String) {
        self.claimId = claimId
        self.workspaceId = workspaceId
        self.command = command
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case workspaceId = "workspace_id"
        case command
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
        self.command = try container.decode(String.self, forKey: .command)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.workspaceId, forKey: .workspaceId)
        try container.encode(self.command, forKey: .command)
    }
}

fileprivate struct _RequestClaimsReserve: Codable, Sendable, Equatable {
    fileprivate var resource: ResourceId
    fileprivate var workspaceId: String

    fileprivate init(resource: ResourceId, workspaceId: String) {
        self.resource = resource
        self.workspaceId = workspaceId
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case workspaceId = "workspace_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.workspaceId, forKey: .workspaceId)
    }
}

fileprivate struct _RequestClaimsUnreserve: Codable, Sendable, Equatable {
    fileprivate var resource: ResourceId
    fileprivate var workspaceId: String

    fileprivate init(resource: ResourceId, workspaceId: String) {
        self.resource = resource
        self.workspaceId = workspaceId
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case workspaceId = "workspace_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.workspaceId, forKey: .workspaceId)
    }
}

fileprivate struct _RequestClaimsSeizeAbort: Codable, Sendable, Equatable {
    fileprivate var planId: String

    fileprivate init(planId: String) {
        self.planId = planId
    }

    private enum CodingKeys: String, CodingKey {
        case planId = "plan_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.planId = try container.decode(String.self, forKey: .planId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.planId, forKey: .planId)
    }
}

fileprivate struct _RequestAccountList: Codable, Sendable, Equatable {
    fileprivate var project: ProjectRef

    fileprivate init(project: ProjectRef) {
        self.project = project
    }

    private enum CodingKeys: String, CodingKey {
        case project
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
    }
}

fileprivate struct _RequestAccountCheck: Codable, Sendable, Equatable {
    fileprivate var project: ProjectRef
    fileprivate var name: String

    fileprivate init(project: ProjectRef, name: String) {
        self.project = project
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
    }
}

fileprivate struct _RequestDevicesList: Codable, Sendable, Equatable {
    fileprivate var all: Bool

    fileprivate init(all: Bool) {
        self.all = all
    }

    private enum CodingKeys: String, CodingKey {
        case all
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.all = try container.decode(Bool.self, forKey: .all)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.all, forKey: .all)
    }
}

fileprivate struct _RequestDevicesShow: Codable, Sendable, Equatable {
    fileprivate var device: String
    fileprivate var freshSessions: Bool

    fileprivate init(device: String, freshSessions: Bool) {
        self.device = device
        self.freshSessions = freshSessions
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case freshSessions = "fresh_sessions"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
        self.freshSessions = try container.decode(Bool.self, forKey: .freshSessions)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.freshSessions, forKey: .freshSessions)
    }
}

fileprivate struct _RequestDevicesResolve: Codable, Sendable, Equatable {
    fileprivate var device: String

    fileprivate init(device: String) {
        self.device = device
    }

    private enum CodingKeys: String, CodingKey {
        case device
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
    }
}

fileprivate struct _RequestDevicesAdd: Codable, Sendable, Equatable {
    fileprivate var device: String
    fileprivate var alias: String?
    fileprivate var person: Bool

    fileprivate init(device: String, alias: String? = nil, person: Bool) {
        self.device = device
        self.alias = alias
        self.person = person
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case alias
        case person
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
        self.alias = try container.decodeIfPresent(String.self, forKey: .alias)
        self.person = try container.decode(Bool.self, forKey: .person)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.alias, forKey: .alias)
        try container.encode(self.person, forKey: .person)
    }
}

fileprivate struct _RequestDevicesRename: Codable, Sendable, Equatable {
    fileprivate var device: String
    fileprivate var alias: String
    fileprivate var person: Bool

    fileprivate init(device: String, alias: String, person: Bool) {
        self.device = device
        self.alias = alias
        self.person = person
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case alias
        case person
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
        self.alias = try container.decode(String.self, forKey: .alias)
        self.person = try container.decode(Bool.self, forKey: .person)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.alias, forKey: .alias)
        try container.encode(self.person, forKey: .person)
    }
}

fileprivate struct _RequestDevicesNote: Codable, Sendable, Equatable {
    fileprivate var device: String
    fileprivate var note: String?

    fileprivate init(device: String, note: String? = nil) {
        self.device = device
        self.note = note
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case note
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
        self.note = try container.decodeIfPresent(String.self, forKey: .note)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.note, forKey: .note)
    }
}

fileprivate struct _RequestDevicesRemove: Codable, Sendable, Equatable {
    fileprivate var device: String
    fileprivate var person: Bool

    fileprivate init(device: String, person: Bool) {
        self.device = device
        self.person = person
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case person
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
        self.person = try container.decode(Bool.self, forKey: .person)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.person, forKey: .person)
    }
}

fileprivate struct _RequestDevicesInstalls: Codable, Sendable, Equatable {
    fileprivate var device: String?
    fileprivate var query: HistoryQuery

    fileprivate init(device: String? = nil, query: HistoryQuery) {
        self.device = device
        self.query = query
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case query
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decodeIfPresent(String.self, forKey: .device)
        self.query = try container.decode(HistoryQuery.self, forKey: .query)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.query, forKey: .query)
    }
}

fileprivate struct _RequestDevicesBoot: Codable, Sendable, Equatable {
    fileprivate var resource: ResourceId
    fileprivate var claimId: String
    fileprivate var workspaceId: String

    fileprivate init(resource: ResourceId, claimId: String, workspaceId: String) {
        self.resource = resource
        self.claimId = claimId
        self.workspaceId = workspaceId
    }

    private enum CodingKeys: String, CodingKey {
        case resource
        case claimId = "claim_id"
        case workspaceId = "workspace_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.workspaceId = try container.decode(String.self, forKey: .workspaceId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.workspaceId, forKey: .workspaceId)
    }
}

fileprivate struct _RequestDevicesForgetSessions: Codable, Sendable, Equatable {
    fileprivate var device: String
    fileprivate var person: Bool
    fileprivate var byWorkspacePath: String?
    fileprivate var byPid: UInt32?

    fileprivate init(device: String, person: Bool, byWorkspacePath: String? = nil, byPid: UInt32? = nil) {
        self.device = device
        self.person = person
        self.byWorkspacePath = byWorkspacePath
        self.byPid = byPid
    }

    private enum CodingKeys: String, CodingKey {
        case device
        case person
        case byWorkspacePath = "by_workspace_path"
        case byPid = "by_pid"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.device = try container.decode(String.self, forKey: .device)
        self.person = try container.decode(Bool.self, forKey: .person)
        self.byWorkspacePath = try container.decodeIfPresent(String.self, forKey: .byWorkspacePath)
        self.byPid = try container.decodeIfPresent(UInt32.self, forKey: .byPid)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.device, forKey: .device)
        try container.encode(self.person, forKey: .person)
        try container.encode(self.byWorkspacePath, forKey: .byWorkspacePath)
        try container.encode(self.byPid, forKey: .byPid)
    }
}

fileprivate struct _RequestSecretReveal: Codable, Sendable, Equatable {
    fileprivate var project: ProjectRef
    fileprivate var name: String

    fileprivate init(project: ProjectRef, name: String) {
        self.project = project
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
    }
}

fileprivate struct _RequestAccountReveal: Codable, Sendable, Equatable {
    fileprivate var project: ProjectRef
    fileprivate var name: String

    fileprivate init(project: ProjectRef, name: String) {
        self.project = project
        self.name = name
    }

    private enum CodingKeys: String, CodingKey {
        case project
        case name
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.project = try container.decode(ProjectRef.self, forKey: .project)
        self.name = try container.decode(String.self, forKey: .name)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.project, forKey: .project)
        try container.encode(self.name, forKey: .name)
    }
}

fileprivate struct _RequestSecretsRemovePreserved: Codable, Sendable, Equatable {
    fileprivate var path: String

    fileprivate init(path: String) {
        self.path = path
    }

    private enum CodingKeys: String, CodingKey {
        case path
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.path = try container.decode(String.self, forKey: .path)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.path, forKey: .path)
    }
}

fileprivate struct _RequestAccountRequestWithdraw: Codable, Sendable, Equatable {
    fileprivate var id: String
    fileprivate var timedOut: Bool

    fileprivate init(id: String, timedOut: Bool) {
        self.id = id
        self.timedOut = timedOut
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case timedOut = "timed_out"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.timedOut = try container.decode(Bool.self, forKey: .timedOut)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.timedOut, forKey: .timedOut)
    }
}

fileprivate struct _RequestAccountDecline: Codable, Sendable, Equatable {
    fileprivate var id: String

    fileprivate init(id: String) {
        self.id = id
    }

    private enum CodingKeys: String, CodingKey {
        case id
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
    }
}

fileprivate struct _RequestHeavyStop: Codable, Sendable, Equatable {
    fileprivate var id: String
    fileprivate var confirmation: String

    fileprivate init(id: String, confirmation: String) {
        self.id = id
        self.confirmation = confirmation
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case confirmation
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.confirmation = try container.decode(String.self, forKey: .confirmation)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.id, forKey: .id)
        try container.encode(self.confirmation, forKey: .confirmation)
    }
}

fileprivate struct _RunGroupUnixGroup: Codable, Sendable, Equatable {
    fileprivate var pgid: UInt32
    fileprivate var bootId: String

    fileprivate init(pgid: UInt32, bootId: String) {
        self.pgid = pgid
        self.bootId = bootId
    }

    private enum CodingKeys: String, CodingKey {
        case pgid
        case bootId = "boot_id"
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.pgid = try container.decode(UInt32.self, forKey: .pgid)
        self.bootId = try container.decode(String.self, forKey: .bootId)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.pgid, forKey: .pgid)
        try container.encode(self.bootId, forKey: .bootId)
    }
}

fileprivate struct _RunGroupWindowsJob: Codable, Sendable, Equatable {
    fileprivate var name: String
    fileprivate var bootId: String
    fileprivate var session: UInt32?

    fileprivate init(name: String, bootId: String, session: UInt32? = nil) {
        self.name = name
        self.bootId = bootId
        self.session = session
    }

    private enum CodingKeys: String, CodingKey {
        case name
        case bootId = "boot_id"
        case session
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.name = try container.decode(String.self, forKey: .name)
        self.bootId = try container.decode(String.self, forKey: .bootId)
        self.session = try container.decodeIfPresent(UInt32.self, forKey: .session)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.name, forKey: .name)
        try container.encode(self.bootId, forKey: .bootId)
        try container.encodeIfPresent(self.session, forKey: .session)
    }
}

fileprivate struct _SecretSourceSharedFrom: Codable, Sendable, Equatable {
    fileprivate var projectId: String
    fileprivate var project: String

    fileprivate init(projectId: String, project: String) {
        self.projectId = projectId
        self.project = project
    }

    private enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case project
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.projectId = try container.decode(String.self, forKey: .projectId)
        self.project = try container.decode(String.self, forKey: .project)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.projectId, forKey: .projectId)
        try container.encode(self.project, forKey: .project)
    }
}

fileprivate struct _ClaimSelectorExact: Codable, Sendable, Equatable {
    fileprivate var resource: ResourceId

    fileprivate init(resource: ResourceId) {
        self.resource = resource
    }

    private enum CodingKeys: String, CodingKey {
        case resource
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.resource, forKey: .resource)
    }
}

fileprivate struct _ClaimSelectorAny: Codable, Sendable, Equatable {
    fileprivate var kind: ResourceKind
    fileprivate var attributes: [String: String]

    fileprivate init(kind: ResourceKind, attributes: [String: String]) {
        self.kind = kind
        self.attributes = attributes
    }

    private enum CodingKeys: String, CodingKey {
        case kind
        case attributes
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.kind = try container.decode(ResourceKind.self, forKey: .kind)
        self.attributes = try container.decode([String: String].self, forKey: .attributes)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.kind, forKey: .kind)
        try container.encode(self.attributes, forKey: .attributes)
    }
}

fileprivate struct _StopEvidenceGone: Codable, Sendable, Equatable {
    fileprivate var target: UInt

    fileprivate init(target: UInt) {
        self.target = target
    }

    private enum CodingKeys: String, CodingKey {
        case target
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.target = try container.decode(UInt.self, forKey: .target)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.target, forKey: .target)
    }
}

fileprivate struct _StopEvidenceJobGone: Codable, Sendable, Equatable {
    fileprivate var target: UInt
    fileprivate var session: UInt32

    fileprivate init(target: UInt, session: UInt32) {
        self.target = target
        self.session = session
    }

    private enum CodingKeys: String, CodingKey {
        case target
        case session
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.target = try container.decode(UInt.self, forKey: .target)
        self.session = try container.decode(UInt32.self, forKey: .session)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.target, forKey: .target)
        try container.encode(self.session, forKey: .session)
    }
}

fileprivate struct _StopTargetRunGroup: Codable, Sendable, Equatable {
    fileprivate var claimId: String
    fileprivate var holder: ProcessIdentity
    fileprivate var child: ProcessIdentity?
    fileprivate var group: RunGroup?

    fileprivate init(claimId: String, holder: ProcessIdentity, child: ProcessIdentity? = nil, group: RunGroup? = nil) {
        self.claimId = claimId
        self.holder = holder
        self.child = child
        self.group = group
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case holder
        case child
        case group
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.holder = try container.decode(ProcessIdentity.self, forKey: .holder)
        self.child = try container.decodeIfPresent(ProcessIdentity.self, forKey: .child)
        self.group = try container.decodeIfPresent(RunGroup.self, forKey: .group)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.holder, forKey: .holder)
        try container.encode(self.child, forKey: .child)
        try container.encode(self.group, forKey: .group)
    }
}

fileprivate struct _StopTargetProcess: Codable, Sendable, Equatable {
    fileprivate var claimId: String
    fileprivate var process: ProcessIdentity

    fileprivate init(claimId: String, process: ProcessIdentity) {
        self.claimId = claimId
        self.process = process
    }

    private enum CodingKeys: String, CodingKey {
        case claimId = "claim_id"
        case process
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.claimId = try container.decode(String.self, forKey: .claimId)
        self.process = try container.decode(ProcessIdentity.self, forKey: .process)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.claimId, forKey: .claimId)
        try container.encode(self.process, forKey: .process)
    }
}

fileprivate struct _StopTargetForeign: Codable, Sendable, Equatable {
    fileprivate var source: String
    fileprivate var resource: ResourceId
    fileprivate var data: JSONValue

    fileprivate init(source: String, resource: ResourceId, data: JSONValue) {
        self.source = source
        self.resource = resource
        self.data = data
    }

    private enum CodingKeys: String, CodingKey {
        case source
        case resource
        case data
    }

    fileprivate init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.source = try container.decode(String.self, forKey: .source)
        self.resource = try container.decode(ResourceId.self, forKey: .resource)
        self.data = try container.decode(JSONValue.self, forKey: .data)
    }

    fileprivate func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(self.source, forKey: .source)
        try container.encode(self.resource, forKey: .resource)
        try container.encode(self.data, forKey: .data)
    }
}
