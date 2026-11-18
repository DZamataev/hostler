import Foundation

// The resync cutover of spec §6.2 as pure values (plan Task 8, R1-5, R2-6):
// hold incoming events, read every snapshot and the recent histories, apply them, replay the
// held events newer than each snapshot, and only then apply events live. `HostlerStore` runs
// the reads `ResyncMachine` asks for and feeds their results back; nothing here does I/O.

// MARK: - The model

/// What a snapshot answers for, each with its own `as_of_seq` (spec §6.2).
public enum SnapshotKind: String, CaseIterable, Sendable, Hashable {
    case queue, claims, secrets, devices, projects
    /// The pending-requests snapshot; request events (per id) are compared with it.
    case requests
}

/// Everything the app knows from the service. A value: `ResyncMachine` produces new ones.
public struct StoreModel: Equatable, Sendable {
    public var service: ServiceStatus?
    public var queue: QueueSnapshot?
    public var claims: ClaimsSnapshot?
    public var secrets: SecretsStatus?
    public var devices: DevicesSnapshot?
    public var projects: ProjectsSnapshot?
    /// Every secret request seen, newest first (`state` says which are pending).
    public var secretRequests: [SecretRequestInfo] = []
    /// Every account request seen, newest first.
    public var accountRequests: [AccountRequestInfo] = []
    /// `queue.history`, newest first.
    public var queueHistory: [HeavyHistoryItem] = []
    /// `claims.history` (seizures among them, as `reason: seized`), newest first.
    public var claimHistory: [ClaimHistoryItem] = []
    /// `devices.installs` plus `devices.installed` events, newest first, distinct ids.
    public var installs: [InstallEvent] = []
    /// `claims.seized` events, newest first, distinct ids. No method reads seizure records
    /// back, so this list keeps what events brought across resyncs (report-t8, question 1).
    public var seizures: [SeizureRecord] = []
    /// The last `seq` applied per kind: a snapshot's `as_of_seq`, or a snapshot event's `seq`.
    public var seqs: [SnapshotKind: UInt64] = [:]
    /// The service run (`instance`) these numbers belong to.
    public var instance: String?

    public init() {}

    /// The secret and account requests still waiting for the person.
    public var pendingSecretRequests: [SecretRequestInfo] { secretRequests.filter { $0.state == .pending } }
    public var pendingAccountRequests: [AccountRequestInfo] { accountRequests.filter { $0.state == .pending } }

    /// At most this many records are kept per list; the oldest go first.
    public static let recordLimit = 1000
}

/// Which connection a frame came from (R1-1). The connection layer numbers its connections
/// 1, 2, 3, … in the order it opens them, never reusing a number within the app's run, and
/// passes the number with `connected`, `dropped` and every event frame. The store discards a
/// frame whose connection is not the current one, so a frame of an old connection (or one read
/// after its `dropped`) never reaches a newer connection's model.
public struct ConnectionToken: RawRepresentable, Hashable, Comparable, Sendable,
    ExpressibleByIntegerLiteral, CustomStringConvertible {
    public var rawValue: UInt64
    public init(rawValue: UInt64) { self.rawValue = rawValue }
    public init(_ rawValue: UInt64) { self.rawValue = rawValue }
    public init(integerLiteral value: UInt64) { rawValue = value }
    public static func < (a: Self, b: Self) -> Bool { a.rawValue < b.rawValue }
    public var description: String { "connection \(rawValue)" }
}

/// One event as it came off the connection.
public struct HeldEvent: Equatable, Sendable {
    public var seq: UInt64
    public var event: Event
    /// The encoded size the hold counts (the frame's bytes when the transport knows them).
    public var bytes: Int
    /// The connection the frame came from.
    public var connection: ConnectionToken

    public init(seq: UInt64, event: Event, bytes: Int, connection: ConnectionToken) {
        self.seq = seq
        self.event = event
        self.bytes = bytes
        self.connection = connection
    }

    /// The JSON size of `event`, for a caller that does not know the frame's.
    public static func encodedSize(of event: Event) -> Int {
        (try? JSONEncoder().encode(event).count) ?? 0
    }
}

/// How an event folds into the hold, as on the service (spec §6.2): the newest snapshot of a
/// kind and the newest state of a request replace older ones; every record stays.
enum HoldKey: Hashable {
    case snapshot(SnapshotKind)
    case secretRequest(String)
    case accountRequest(String)
    case record(UInt64)
    case resync

    init(_ held: HeldEvent) {
        switch held.event {
        case .queueChanged: self = .snapshot(.queue)
        case .claimsChanged: self = .snapshot(.claims)
        case .secretsLockChanged: self = .snapshot(.secrets)
        case .devicesChanged: self = .snapshot(.devices)
        case .projectsChanged: self = .snapshot(.projects)
        case .secretsRequestChanged(let info): self = .secretRequest(info.id)
        case .accountsRequestChanged(let info): self = .accountRequest(info.id)
        case .claimsSeized, .devicesInstalled: self = .record(held.seq)
        case .resync: self = .resync
        }
    }
}

// MARK: - The hold

/// The bounded buffer of events that arrive during a resync (R2-6): at most `maxEntries`
/// entries and `maxBytes` of encoded events, after folding.
public struct EventHold: Equatable, Sendable {
    public static let maxEntries = 2048
    public static let maxBytes = 16 * 1024 * 1024

    private var entries: [HoldKey: HeldEvent] = [:]
    public private(set) var bytes = 0
    public var count: Int { entries.count }
    public var isEmpty: Bool { entries.isEmpty }

    public init() {}

    /// Adds `event`, replacing the entry it folds into. `false`: the bound would be exceeded;
    /// nothing changed and the resync must start again.
    public mutating func add(_ event: HeldEvent) -> Bool {
        let key = HoldKey(event)
        let replaced = entries[key]
        let newCount = entries.count + (replaced == nil ? 1 : 0)
        let newBytes = bytes - (replaced?.bytes ?? 0) + event.bytes
        guard newCount <= Self.maxEntries, newBytes <= Self.maxBytes else { return false }
        entries[key] = event
        bytes = newBytes
        return true
    }

    /// The held events in `seq` order.
    public var ordered: [HeldEvent] { entries.values.sorted { $0.seq < $1.seq } }

    /// Drops every event that did not come from `connection`.
    public mutating func keepOnly(_ connection: ConnectionToken) {
        entries = entries.filter { $0.value.connection == connection }
        bytes = entries.values.reduce(0) { $0 + $1.bytes }
    }
}

// MARK: - The reads

/// Everything a resync reads (plan Task 8 "Read"), concurrently.
public struct ResyncReads: Equatable, Sendable {
    public var overview: Overview
    public var devices: DevicesSnapshot
    public var projects: ProjectsSnapshot
    public var requests: RequestsSnapshot
    public var service: ServiceStatus
    public var queueHistory: [HeavyHistoryItem]
    public var claimHistory: [ClaimHistoryItem]
    public var installs: [InstallEvent]

    public init(overview: Overview, devices: DevicesSnapshot, projects: ProjectsSnapshot,
                requests: RequestsSnapshot, service: ServiceStatus,
                queueHistory: [HeavyHistoryItem], claimHistory: [ClaimHistoryItem],
                installs: [InstallEvent]) {
        self.overview = overview
        self.devices = devices
        self.projects = projects
        self.requests = requests
        self.service = service
        self.queueHistory = queueHistory
        self.claimHistory = claimHistory
        self.installs = installs
    }
}

// MARK: - Cutover and live application

public enum Resync {
    /// Applies a resync's reads and replays the held events (spec §6.2):
    /// 1. each snapshot replaces its part and records its `as_of_seq`; the histories replace
    ///    the history lists;
    /// 2. the held events in `seq` order: a snapshot or request event only when its `seq` is
    ///    greater than its kind's `as_of_seq`; a record always, deduplicated by id.
    public static func cutover(_ model: StoreModel, reads: ResyncReads, held: [HeldEvent]) -> StoreModel {
        var next = model
        next.service = reads.service
        next.queue = reads.overview.heavy
        next.claims = reads.overview.claims
        next.secrets = reads.overview.secrets
        next.devices = reads.devices
        next.projects = reads.projects
        next.secretRequests = sortedRequests(reads.requests.secrets)
        next.accountRequests = sortedRequests(reads.requests.accounts)
        next.seqs = [
            .queue: reads.overview.heavy.asOfSeq,
            .claims: reads.overview.claims.asOfSeq,
            .secrets: reads.overview.secrets.asOfSeq,
            .devices: reads.devices.asOfSeq,
            .projects: reads.projects.asOfSeq,
            .requests: reads.requests.asOfSeq,
        ]
        next.queueHistory = reads.queueHistory
        next.claimHistory = reads.claimHistory
        next.installs = []
        for install in reads.installs where !next.installs.contains(where: { $0.id == install.id }) {
            next.installs.append(install)
        }
        for held in held.sorted(by: { $0.seq < $1.seq }) {
            apply(held.event, seq: held.seq, to: &next)
        }
        return next
    }

    /// Applies one event to a live model; `false` when it was ignored (an older snapshot, a
    /// request already covered by the requests snapshot, a record already present, `resync`).
    @discardableResult
    public static func apply(_ event: Event, seq: UInt64, to model: inout StoreModel) -> Bool {
        switch event {
        case .queueChanged(let snapshot):
            guard newer(seq, than: .queue, in: model) else { return false }
            model.queue = snapshot
            model.seqs[.queue] = seq
        case .claimsChanged(let snapshot):
            guard newer(seq, than: .claims, in: model) else { return false }
            model.claims = snapshot
            model.seqs[.claims] = seq
        case .secretsLockChanged(let status):
            guard newer(seq, than: .secrets, in: model) else { return false }
            model.secrets = status
            model.seqs[.secrets] = seq
        case .devicesChanged(let snapshot):
            guard newer(seq, than: .devices, in: model) else { return false }
            model.devices = snapshot
            model.seqs[.devices] = seq
        case .projectsChanged(let snapshot):
            guard newer(seq, than: .projects, in: model) else { return false }
            model.projects = snapshot
            model.seqs[.projects] = seq
        case .secretsRequestChanged(let info):
            guard newer(seq, than: .requests, in: model) else { return false }
            model.secretRequests = upsert(info, into: model.secretRequests)
        case .accountsRequestChanged(let info):
            guard newer(seq, than: .requests, in: model) else { return false }
            model.accountRequests = upsert(info, into: model.accountRequests)
        case .claimsSeized(let record):
            guard !model.seizures.contains(where: { $0.id == record.id }) else { return false }
            model.seizures.insert(record, at: 0)
            if model.seizures.count > StoreModel.recordLimit { model.seizures.removeLast() }
        case .devicesInstalled(let install):
            guard !model.installs.contains(where: { $0.id == install.id }) else { return false }
            model.installs.insert(install, at: 0)
            if model.installs.count > StoreModel.recordLimit { model.installs.removeLast() }
        case .resync:
            return false
        }
        return true
    }

    /// No `seq` recorded for the kind (a new instance, nothing read yet) accepts any event.
    private static func newer(_ seq: UInt64, than kind: SnapshotKind, in model: StoreModel) -> Bool {
        guard let last = model.seqs[kind] else { return true }
        return seq > last
    }

    private static func sortedRequests<R: PendingRequest>(_ requests: [R]) -> [R] {
        requests.sorted { ($0.createdAtMs, $0.id) > ($1.createdAtMs, $1.id) }
    }

    private static func upsert<R: PendingRequest>(_ request: R, into list: [R]) -> [R] {
        var list = list
        if let index = list.firstIndex(where: { $0.id == request.id }) {
            list[index] = request
        } else {
            list = sortedRequests(list + [request])
        }
        // Answered requests are kept for a while (newest first), never without bound; a pending
        // one is never dropped.
        while list.count > StoreModel.recordLimit,
              let oldest = list.lastIndex(where: { $0.state != .pending }) {
            list.remove(at: oldest)
        }
        return list
    }
}

/// What the two request kinds share, for ordering and upserts.
protocol PendingRequest {
    var id: String { get }
    var createdAtMs: Int64 { get }
    var state: RequestState { get }
}

extension SecretRequestInfo: PendingRequest {}
extension AccountRequestInfo: PendingRequest {}

// MARK: - The machine

/// The resync state machine (spec §6.2; plan Task 8 "Triggers", "Hold", "Read", "Apply",
/// "Replay", "Live"). Pure: inputs in, effects out; `HostlerStore` performs the effects.
public struct ResyncMachine: Equatable, Sendable {
    public enum Phase: Equatable, Sendable {
        /// No connection. A frame of a connection newer than every one seen is held until its
        /// `connected` (the connection layer may hand it over first); any other is discarded.
        case offline
        /// The reads of `generation` are in flight (or wait for their delay).
        case reading(generation: UInt64)
        /// The reads of `generation` failed; a retry is scheduled. Events are still held.
        case waitingRetry(generation: UInt64)
        /// The cutover is done; events apply directly.
        case live
    }

    public enum Input: Equatable, Sendable {
        /// A connection became live (connect, reconnect), with the run's `instance` from hello.
        case connected(instance: String?, connection: ConnectionToken)
        /// That connection dropped.
        case dropped(connection: ConnectionToken)
        /// An event from the connection.
        case event(HeldEvent)
        /// The reads of `generation` answered.
        case readsSucceeded(generation: UInt64, ResyncReads)
        /// The reads of `generation` failed.
        case readsFailed(generation: UInt64, HostlerCallError)
        /// The retry scheduled for `generation` is due.
        case retryDue(generation: UInt64)
        /// The person asked for a full re-read (Refresh, ⌘R).
        case refresh
    }

    public enum Effect: Equatable, Sendable {
        /// Read everything, after `delay`, and answer `readsSucceeded` / `readsFailed`.
        case read(generation: UInt64, delay: Duration)
        /// Answer `retryDue(generation)` after `delay`.
        case retry(generation: UInt64, delay: Duration)
    }

    /// A failed read is retried after this long (one at a time).
    public static let retryDelay: Duration = .seconds(5)
    /// After this many abandoned resyncs in a row, the next waits `catchUpDelay`.
    public static let abandonLimit = 3
    public static let catchUpDelay: Duration = .seconds(5)

    public private(set) var model = StoreModel()
    public private(set) var phase: Phase = .offline
    public private(set) var hold = EventHold()
    /// Increases with every resync started; answers for older generations are dropped.
    public private(set) var generation: UInt64 = 0
    /// Resyncs abandoned (hold overflow, connection change, `resync` mid-read) since the last
    /// one that completed.
    public private(set) var abandonedInARow = 0
    /// Every resync abandoned since the start (diagnostics and tests).
    public private(set) var abandonedTotal = 0
    /// The last failed read; cleared by a completed resync.
    public private(set) var lastError: HostlerCallError?
    /// The `instance` of the current (or last) connection.
    public private(set) var instance: String?
    /// Whether the last event handled while live changed the model.
    public private(set) var lastEventApplied = false
    /// The current connection (`nil` before the first `connected`); after `dropped` it stays
    /// the last one, which is retired.
    public private(set) var connection: ConnectionToken?
    /// The connection while there is one: `nil` offline.
    public var liveConnection: ConnectionToken? { phase == .offline ? nil : connection }
    /// The newest connection that is over (dropped or replaced); its frames, and those of any
    /// older one, are discarded.
    public private(set) var retiredThrough: ConnectionToken?
    /// Frames discarded because they came from another connection (diagnostics and tests).
    public private(set) var foreignFramesDiscarded = 0

    public init() {}

    /// True while a resync has waited out its catch-up delay (the footer says "catching up").
    public var isCatchingUp: Bool {
        switch phase {
        case .reading, .waitingRetry: abandonedInARow >= Self.abandonLimit
        case .offline, .live: false
        }
    }

    public mutating func handle(_ input: Input) -> [Effect] {
        lastEventApplied = false
        switch input {
        case .connected(let newInstance, let token):
            // A late notice for a connection that is already over changes nothing.
            if let retiredThrough, token <= retiredThrough { return [] }
            if let current = connection, current != token { retire(current) }
            connection = token
            switch phase {
            case .reading, .waitingRetry: abandon()
            case .offline, .live: break
            }
            adopt(instance: newInstance)
            // Only this connection's frames, handed over before its `connected`, stay held.
            hold.keepOnly(token)
            return [start(keepingHold: true)]

        case .dropped(let token):
            retire(token)
            guard token == connection else { return [] }
            if case .offline = phase {} else if case .live = phase {} else { abandon() }
            phase = .offline
            hold = EventHold()
            generation += 1
            return []

        case .event(let held):
            guard accepts(held.connection) else {
                foreignFramesDiscarded += 1
                return []
            }
            if case .resync = held.event {
                // The service could not deliver something: every held event may have a gap
                // before it, and reads already sent may predate it.
                switch phase {
                case .live: return [start(keepingHold: false)]
                case .reading, .waitingRetry:
                    abandon()
                    return [start(keepingHold: false)]
                case .offline:
                    // Reads start on `connected`, after this event; they cover it.
                    hold = EventHold()
                    return []
                }
            }
            switch phase {
            case .live:
                lastEventApplied = Resync.apply(held.event, seq: held.seq, to: &model)
                return []
            case .offline:
                if !hold.add(held) {
                    hold = EventHold()
                    _ = hold.add(held)
                }
                return []
            case .reading, .waitingRetry:
                if hold.add(held) { return [] }
                // Over the bound: start again. The event that did not fit opens the fresh
                // hold, since the new reads are sent after it.
                abandon()
                let effect = start(keepingHold: false)
                _ = hold.add(held)
                return [effect]
            }

        case .readsSucceeded(let answered, let reads):
            guard case .reading(let current) = phase, current == answered else { return [] }
            if let readInstance = reads.service.instance, let known = instance,
               readInstance != known {
                // The service restarted between hello and the reads: the numbers differ.
                abandon()
                adopt(instance: readInstance)
                return [start(keepingHold: false)]
            }
            model = Resync.cutover(model, reads: reads, held: hold.ordered)
            model.instance = reads.service.instance ?? instance
            hold = EventHold()
            phase = .live
            abandonedInARow = 0
            lastError = nil
            return []

        case .readsFailed(let answered, let error):
            guard case .reading(let current) = phase, current == answered else { return [] }
            lastError = error
            phase = .waitingRetry(generation: current)
            return [.retry(generation: current, delay: Self.retryDelay)]

        case .retryDue(let due):
            guard case .waitingRetry(let current) = phase, current == due else { return [] }
            // The hold still covers everything since the trigger, so it carries over.
            return [start(keepingHold: true)]

        case .refresh:
            guard case .live = phase else { return [] }
            return [start(keepingHold: false)]
        }
    }

    /// Starts a resync: a new generation, a fresh hold unless `keepingHold`, the reads
    /// (after the catch-up delay once `abandonLimit` resyncs in a row were abandoned).
    private mutating func start(keepingHold: Bool) -> Effect {
        generation += 1
        if !keepingHold { hold = EventHold() }
        phase = .reading(generation: generation)
        let delay: Duration = abandonedInARow >= Self.abandonLimit ? Self.catchUpDelay : .zero
        return .read(generation: generation, delay: delay)
    }

    private mutating func abandon() {
        abandonedInARow += 1
        abandonedTotal += 1
    }

    private mutating func retire(_ token: ConnectionToken) {
        retiredThrough = max(retiredThrough ?? token, token)
    }

    /// The current connection's frames; while offline, also those of a connection newer than
    /// every one seen (its `connected` is on the way).
    private func accepts(_ token: ConnectionToken) -> Bool {
        if let retiredThrough, token <= retiredThrough { return false }
        if phase == .offline { return connection.map { token > $0 } ?? true }
        return token == connection
    }

    /// Replaces the history lists with a refresh's answers (`nil`: not read). Live only.
    public mutating func replaceHistories(queue: [HeavyHistoryItem]?, claims: [ClaimHistoryItem]?) {
        guard case .live = phase else { return }
        if let queue { model.queueHistory = queue }
        if let claims { model.claimHistory = claims }
    }

    /// A different run numbers its events from 1 again: forget the per-kind numbers.
    private mutating func adopt(instance newInstance: String?) {
        if let newInstance, newInstance != model.instance {
            model.seqs = [:]
            model.instance = newInstance
        }
        instance = newInstance
    }
}
