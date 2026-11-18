import Foundation
import Observation

/// The model the popover and the window read, kept live from the service's events
/// (spec §6.2, §9.1; plan Task 8). It runs `ResyncMachine`'s effects (the reads, the retry),
/// applies its model, refreshes the histories when something finishes, loads per-project
/// secrets on demand, and posts notifications through `Notifying`.
///
/// **Wiring** (Task 9 joins it to Task 7's `ConnectionController`). The connection layer
/// numbers every connection it opens with a `ConnectionToken` (1, 2, 3, …, never reused in
/// the app's run) and passes it with all three calls:
/// - the connection's hello answered (connect, reconnect, a new `instance`) →
///   `connected(instance: hello.instance, connection: token)`;
/// - that connection ending (closed, failed, replaced) → `dropped(connection: token)`;
/// - every event frame read from it, the `resync` event included, in the order read →
///   `receive(event, seq:, connection: token, frameBytes:)`.
///
/// Frames of a connection that is not the current one are discarded, also those read after
/// its `dropped`. A frame handed over before its own connection's `connected` is held and
/// kept for that connection's resync.
@MainActor
@Observable
public final class HostlerStore {
    /// The footer's state (spec §9.1: "Live" or "Offline since …").
    public enum SyncState: Equatable, Sendable {
        case offline(since: Date?)
        /// A resync is reading (`catchingUp` after `ResyncMachine.abandonLimit` restarts).
        case syncing(catchingUp: Bool)
        /// The last resync's reads failed; it retries by itself.
        case failed(HostlerAppError)
        case live
    }

    // MARK: State the views read

    public private(set) var machine = ResyncMachine()
    public var model: StoreModel { machine.model }

    public private(set) var lastEventAt: Date?
    public private(set) var offlineSince: Date?
    /// Each loaded project's secrets (`secret.list`), by project id.
    public private(set) var projectSecrets: [String: SecretList] = [:]
    /// The last failure loading a project's secrets, by project id.
    public private(set) var projectSecretErrors: [String: HostlerAppError] = [:]
    /// Each workspace's branch, by workspace id (`GitHeadReading`).
    public private(set) var branches: [String: String] = [:]

    public var notificationSettings: NotificationSettings

    public var syncState: SyncState {
        switch machine.phase {
        case .offline: return .offline(since: offlineSince)
        case .reading: return .syncing(catchingUp: machine.isCatchingUp)
        case .waitingRetry:
            return .failed(machine.lastError.map(HostlerAppError.init(call:))
                ?? .service(message: String(localized: "hostler could not be read"), recovery: nil))
        case .live: return .live
        }
    }

    public var isLive: Bool { machine.phase == .live }

    /// The status item's image (Task 2's rule over the store's numbers).
    public var statusItemAppearance: StatusItemAppearance {
        let pending = model.pendingSecretRequests.count + model.pendingAccountRequests.count
        let locked = model.secrets.map { $0.state == .locked && $0.pendingRequests > 0 } ?? false
        return .decide(isLive: isLive, pendingRequests: pending, lockedWithWaitingSecretUsers: locked)
    }

    /// The sidebar rows of `project`'s workspaces.
    public func workspaceRows(of project: ProjectView) -> [WorkspaceRow] {
        HostlerKit.workspaceRows(project: project, claims: model.claims, queue: model.queue,
                                 branches: branches)
    }

    /// The standing chips of `device`.
    public func standing(of device: DeviceInfo) -> [DeviceStanding] {
        let state = model.claims?.resources.first { $0.resource.id == device.resource }
        let workspaces = model.projects?.projects.flatMap(\.workspaces) ?? []
        return HostlerKit.standing(device: device, claims: state, workspaces: workspaces)
    }

    // MARK: Dependencies

    @ObservationIgnored private let client: HostlerClient
    @ObservationIgnored private let clock: any WallClock
    @ObservationIgnored private let notifier: any Notifying
    @ObservationIgnored private let gitHead: any GitHeadReading
    @ObservationIgnored private let sleep: @Sendable (Duration) async -> Void

    @ObservationIgnored private var lastFacts = NotificationFacts()
    /// When the first resync started reading: seizures that ended before it never notify.
    @ObservationIgnored private var seizuresSinceMs: Int64?
    @ObservationIgnored private var notified = NotifiedSubjects()
    @ObservationIgnored private var loadedSecretProjects: Set<String> = []
    @ObservationIgnored private var secretReads: [String: SecretRead] = [:]
    @ObservationIgnored private var nextSecretRead: UInt64 = 0
    @ObservationIgnored private var historyRefresh = HistoryRefresh()
    @ObservationIgnored private var tasks: [UInt64: Task<Void, Never>] = [:]
    @ObservationIgnored private var nextTaskId: UInt64 = 0

    public init(client: HostlerClient, clock: any WallClock = SystemClock(),
                notifier: any Notifying = SilentNotifier(),
                gitHead: any GitHeadReading = FileGitHead(),
                settings: NotificationSettings = NotificationSettings(),
                sleep: @escaping @Sendable (Duration) async -> Void = { duration in
                    try? await Task.sleep(for: duration)
                }) {
        self.client = client
        self.clock = clock
        self.notifier = notifier
        self.gitHead = gitHead
        self.notificationSettings = settings
        self.sleep = sleep
    }

    // MARK: Inputs from the connection

    /// A connection became live: connect, reconnect or a new `instance` (Task 7's `onResync`).
    public func connected(instance: String?, connection: ConnectionToken) {
        handle(.connected(instance: instance, connection: connection))
    }

    /// That connection ended. A late notice for an older connection changes nothing.
    public func dropped(connection: ConnectionToken) {
        let current = machine.connection == connection && machine.phase != .offline
        handle(.dropped(connection: connection))
        if current, offlineSince == nil { offlineSince = clock.now() }
    }

    /// An event frame. `frameBytes`: the frame's size when the transport knows it (the hold
    /// counts encoded bytes); otherwise the event is encoded to measure it, during a resync only.
    public func receive(_ event: Event, seq: UInt64, connection: ConnectionToken,
                        frameBytes: Int? = nil) {
        let holding = machine.phase != .live
        let bytes = frameBytes ?? (holding ? HeldEvent.encodedSize(of: event) : 0)
        let before = machine.model
        let discarded = machine.foreignFramesDiscarded
        handle(.event(HeldEvent(seq: seq, event: event, bytes: bytes, connection: connection)))
        guard machine.foreignFramesDiscarded == discarded else { return }
        lastEventAt = clock.now()
        if machine.phase == .live, !holding { afterLiveChange(from: before, event: event) }
    }

    /// Refresh (⌘R): a full re-read.
    public func refresh() {
        handle(.refresh)
    }

    /// Re-evaluates the time-based rules (a stuck task); the app calls it about once a minute.
    public func tick() {
        evaluateNotifications()
    }

    // MARK: Per-project secrets

    /// Loads `project`'s secrets (`secret.list` with its stored declarations) and keeps them
    /// re-read on `secrets.lock_changed`, on a stored request for it, and on `refreshSecrets()`.
    public func loadSecrets(of project: ProjectView) {
        loadedSecretProjects.insert(project.info.id)
        readSecrets(of: project.info.id)
    }

    /// Re-reads every loaded project's secrets (the window gaining focus).
    public func refreshSecrets() {
        for id in loadedSecretProjects.sorted() { readSecrets(of: id) }
    }

    /// Starts a read of `projectId`'s secrets (R1-3). A newer read of the same project cancels
    /// the older one, and an answer is committed only while its read is the newest for the
    /// project, the connection and instance are the ones it was sent on, and the project is
    /// still in the projects snapshot. A late answer, success or failure, is dropped.
    private func readSecrets(of projectId: String) {
        guard let view = model.projects?.projects.first(where: { $0.info.id == projectId }) else {
            return
        }
        nextSecretRead += 1
        let read = SecretRead(token: nextSecretRead, connection: machine.liveConnection,
                              instance: model.instance)
        secretReads[projectId]?.task?.cancel()
        secretReads[projectId] = read
        let ref = ProjectRef(commonDir: view.info.commonDir, name: view.info.name)
        let declared = view.declared?.secrets ?? []
        let client = client
        let task = launch { store in
            let result: Result<SecretList, HostlerCallError>
            do throws(HostlerCallError) {
                result = .success(try await client.secretList(project: ref, declared: declared))
            } catch {
                result = .failure(error)
            }
            store.commitSecrets(result, of: projectId, read: read)
        }
        secretReads[projectId]?.task = task
    }

    private func commitSecrets(_ result: Result<SecretList, HostlerCallError>, of projectId: String,
                               read: SecretRead) {
        guard secretReads[projectId]?.token == read.token else { return }
        secretReads[projectId] = nil
        guard read.connection != nil, read.connection == machine.liveConnection,
              read.instance == model.instance,
              model.projects?.projects.contains(where: { $0.info.id == projectId }) == true else {
            return
        }
        switch result {
        case .success(let list):
            projectSecrets[projectId] = list
            projectSecretErrors[projectId] = nil
        case .failure(let error):
            projectSecretErrors[projectId] = HostlerAppError(call: error)
        }
    }

    /// The newest read of a project's secrets.
    private struct SecretRead {
        var token: UInt64
        var connection: ConnectionToken?
        var instance: String?
        var task: Task<Void, Never>?
    }

    // MARK: Branches

    /// Re-reads every workspace's branch (on a projects change and on window focus).
    public func refreshBranches() {
        var next: [String: String] = [:]
        for workspace in model.projects?.projects.flatMap(\.workspaces) ?? [] {
            if let branch = gitHead.branch(at: URL(fileURLWithPath: workspace.path, isDirectory: true)) {
                next[workspace.id] = branch
            }
        }
        if next != branches { branches = next }
    }

    // MARK: The machine's effects

    private func handle(_ input: ResyncMachine.Input) {
        let wasLive = machine.phase == .live
        let before = machine.model
        let effects = machine.handle(input)
        for effect in effects { perform(effect) }
        if machine.phase == .live, !wasLive {
            offlineSince = nil
            afterCutover(from: before)
        }
    }

    private func perform(_ effect: ResyncMachine.Effect) {
        switch effect {
        case .read(let generation, let delay):
            let client = client
            let sleep = sleep
            launch { store in
                if delay > .zero { await sleep(delay) }
                guard store.machine.phase == .reading(generation: generation) else { return }
                if store.seizuresSinceMs == nil { store.seizuresSinceMs = store.nowMs }
                do throws(HostlerCallError) {
                    let reads = try await Self.readAll(client, nowMs: store.nowMs)
                    store.handle(.readsSucceeded(generation: generation, reads))
                } catch {
                    Log.store.error("a resync read failed")
                    store.handle(.readsFailed(generation: generation, error))
                }
            }
        case .retry(let generation, let delay):
            let sleep = sleep
            launch { store in
                await sleep(delay)
                store.handle(.retryDue(generation: generation))
            }
        }
    }

    /// The resync's reads, concurrently (plan Task 8 "Read").
    nonisolated static func readAll(_ client: HostlerClient, nowMs: Int64) async throws(HostlerCallError) -> ResyncReads {
        let dayAgoMs = nowMs - dayMs
        async let overview = client.statusOverview()
        async let devices = client.devicesList(all: true)
        async let projects = client.projectsSnapshot()
        async let requests = client.requestsSnapshot()
        async let service = client.serviceStatus()
        async let queueHistory = client.queueHistory(HistoryQuery(sinceMs: nil, limit: 200))
        async let claimHistory = client.claimsHistory(HistoryQuery(sinceMs: dayAgoMs, limit: 500))
        async let installs = client.devicesInstalls(device: nil,
                                                    query: HistoryQuery(sinceMs: dayAgoMs, limit: 500))
        do {
            return try await ResyncReads(
                overview: overview, devices: devices, projects: projects, requests: requests,
                service: service, queueHistory: queueHistory, claimHistory: claimHistory,
                installs: installs)
        } catch let error as HostlerCallError {
            throw error
        } catch {
            throw .transport(String(localized: "hostler's answers could not be read"))
        }
    }

    // MARK: After a change

    private func afterCutover(from before: StoreModel) {
        if before.projects != model.projects { refreshBranches() }
        refreshSecrets()
        evaluateNotifications()
    }

    private func afterLiveChange(from before: StoreModel, event: Event) {
        guard machine.lastEventApplied else { return }
        switch event {
        case .queueChanged(let queue):
            let gone = Set((before.queue?.running ?? []).map(\.id))
                .subtracting(queue.running.map(\.id))
            if !gone.isEmpty { requestHistories(queue: true, claims: false) }
        case .claimsChanged(let claims):
            let ids = { (snapshot: ClaimsSnapshot?) in
                Set((snapshot?.resources ?? []).flatMap { $0.claims.map(\.id) })
            }
            if !ids(before.claims).subtracting(ids(claims)).isEmpty {
                requestHistories(queue: false, claims: true)
            }
        case .secretsLockChanged:
            refreshSecrets()
        case .secretsRequestChanged(let request):
            if request.state == .stored, loadedSecretProjects.contains(request.projectId) {
                readSecrets(of: request.projectId)
            }
        case .projectsChanged:
            refreshBranches()
        case .claimsSeized(let record):
            // A seizure of hostler's own claims notifies from its history rows (R1-2).
            if record.fromKind != .foreign { requestHistories(queue: false, claims: true) }
        case .devicesChanged, .devicesInstalled, .accountsRequestChanged, .resync:
            break
        }
        evaluateNotifications()
    }

    private var nowMs: Int64 { Int64((clock.now().timeIntervalSince1970 * 1000).rounded()) }
    nonisolated static let dayMs: Int64 = 24 * 3600 * 1000

    private func evaluateNotifications() {
        let facts = NotificationFacts(model: model, nowMs: nowMs, seizuresSinceMs: seizuresSinceMs)
        let (notes, notified) = NotificationRules.evaluate(
            old: lastFacts, new: facts, settings: notificationSettings, notified: notified)
        lastFacts = facts
        self.notified = notified
        for note in notes { notifier.post(note) }
    }

    // MARK: History refresh (a task or claim ended while live)

    /// One refresh at a time; a request during one runs one more after it.
    private struct HistoryRefresh {
        var running = false
        var queue = false
        var claims = false
    }

    private func requestHistories(queue: Bool, claims: Bool) {
        historyRefresh.queue = historyRefresh.queue || queue
        historyRefresh.claims = historyRefresh.claims || claims
        guard !historyRefresh.running else { return }
        historyRefresh.running = true
        launch { store in await store.runHistoryRefresh() }
    }

    private func runHistoryRefresh() async {
        while historyRefresh.queue || historyRefresh.claims {
            let queue = historyRefresh.queue
            let claims = historyRefresh.claims
            historyRefresh.queue = false
            historyRefresh.claims = false
            let generation = machine.generation
            let dayAgoMs = nowMs - Self.dayMs
            do {
                let queueHistory = queue ? try await client.queueHistory(HistoryQuery(sinceMs: nil, limit: 200)) : nil
                let claimHistory = claims ? try await client.claimsHistory(HistoryQuery(sinceMs: dayAgoMs, limit: 500)) : nil
                // A resync that started meanwhile reads the histories itself.
                guard machine.generation == generation, machine.phase == .live else { continue }
                machine.replaceHistories(queue: queueHistory, claims: claimHistory)
                evaluateNotifications()
            } catch {
                Log.store.error("a history refresh failed")
            }
        }
        historyRefresh.running = false
    }

    // MARK: Tasks

    @discardableResult
    private func launch(_ body: @escaping @MainActor (HostlerStore) async -> Void) -> Task<Void, Never> {
        nextTaskId += 1
        let id = nextTaskId
        let task = Task { [weak self] in
            guard let self else { return }
            await body(self)
            self.tasks[id] = nil
        }
        tasks[id] = task
        return task
    }

    /// Waits until no read, retry or refresh the store started is running (tests).
    public func settle() async {
        while let task = tasks.values.first {
            await task.value
        }
    }

    /// Cancels everything the store started (the app quitting).
    public func shutdown() {
        for task in tasks.values { task.cancel() }
        tasks = [:]
    }
}

extension HostlerAppError {
    /// A failed call as the app shows it: the service's own message and recovery, or the
    /// call error's fixed text.
    ///
    /// Inside the app, a service error whose recovery is a CLI step the app itself performs
    /// (`hostler unlock`) is said in the app's words, with no CLI line (Task 13 F-5).
    public init(call error: HostlerCallError) {
        switch error {
        case .service(let service):
            self = Self.inApp(service)
        default:
            self = .service(message: error.errorDescription
                            ?? String(localized: "hostler could not be reached"),
                            recovery: error.recoverySuggestion)
        }
    }

    static func inApp(_ service: HostlerError) -> HostlerAppError {
        switch service.code {
        case .secretsLocked:
            if case .string("missing")? = service.details["state"] {
                return .service(message: String(localized:
                    "No secrets database exists yet. Create one in hostler's menu bar panel."),
                    recovery: nil)
            }
            return .service(message: String(localized:
                "The secrets database is locked. Unlock it in hostler's menu bar panel."),
                recovery: nil)
        case .unlockFailed:
            return .service(message: String(localized: "Wrong master password or key file."),
                            recovery: String(localized: "Check the password and the key file, then try again."))
        default:
            if let recovery = service.recovery, recovery.contains("hostler unlock") {
                return .service(message: service.message,
                                recovery: String(localized: "Unlock it in hostler's menu bar panel."))
            }
            return .service(message: service.message, recovery: service.recovery)
        }
    }
}
