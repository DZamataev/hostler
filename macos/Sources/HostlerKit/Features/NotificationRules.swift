import Foundation

/// The kinds of notification, each switchable in the app's settings (spec §9.4).
public enum NotificationKind: String, CaseIterable, Codable, Sendable {
    /// A connected device hostler has never registered.
    case newDevice
    /// A secret or account requested.
    case request
    /// A heavy task that looks stuck (`StuckRule`).
    case stuckTask
    /// A seizure or hand-over (`claims.seized`).
    case seizure
}

/// Which kinds notify. All on by default; Task 9's settings view stores it.
public struct NotificationSettings: Equatable, Codable, Sendable {
    public var newDevice = true
    public var request = true
    public var stuckTask = true
    public var seizure = true

    public init() {}

    public func allows(_ kind: NotificationKind) -> Bool {
        switch kind {
        case .newDevice: newDevice
        case .request: request
        case .stuckTask: stuckTask
        case .seizure: seizure
        }
    }
}

/// Where a click on a notification leads (spec §9.4: the popover or the matching section).
public enum NotificationTarget: Hashable, Sendable {
    case popover
    case device(ResourceId)
    case queue
}

/// One notification to post. `id` is stable per subject (a request id, a task id, …).
public struct AppNotification: Hashable, Sendable {
    public var kind: NotificationKind
    public var id: String
    public var title: String
    public var body: String
    public var target: NotificationTarget

    public init(kind: NotificationKind, id: String, title: String, body: String,
                target: NotificationTarget) {
        self.kind = kind
        self.id = id
        self.title = title
        self.body = body
        self.target = target
    }
}

/// One seizure or hand-over as the rules see it (R1-2): from a `claims.history` row whose reason
/// is `seized`, or from a `claims.seized` event of a foreign claim (which has no history row).
public struct SeizureFact: Hashable, Sendable {
    public var resource: String
    public var fromWorkspacePath: String
    public var byWorkspacePath: String
    public var atMs: Int64
}

/// What the rules compare between two moments: each part is `nil` while it is not known yet
/// (nothing read). The first time a part is known is its baseline: devices, stuck tasks and
/// seizures notify nothing then; pending requests give one summary (spec §9.4, P5-L11).
public struct NotificationFacts: Equatable, Sendable {
    /// Connected devices that are not registered, by resource id.
    public var unregisteredDevices: [ResourceId: DeviceInfo]?
    public var pendingSecretRequests: [String: SecretRequestInfo]?
    public var pendingAccountRequests: [String: AccountRequestInfo]?
    /// Running tasks over their `StuckRule` bound, by id.
    public var stuckTasks: [String: HeavyEntry]?
    /// Seizures since the store's first resync read, by subject key (`seizureKey`).
    public var seizures: [String: SeizureFact]?

    public init(unregisteredDevices: [ResourceId: DeviceInfo]? = nil,
                pendingSecretRequests: [String: SecretRequestInfo]? = nil,
                pendingAccountRequests: [String: AccountRequestInfo]? = nil,
                stuckTasks: [String: HeavyEntry]? = nil,
                seizures: [String: SeizureFact]? = nil) {
        self.unregisteredDevices = unregisteredDevices
        self.pendingSecretRequests = pendingSecretRequests
        self.pendingAccountRequests = pendingAccountRequests
        self.stuckTasks = stuckTasks
        self.seizures = seizures
    }

    /// The facts of `model` at `nowMs`. A model that has never been read has none.
    ///
    /// - Parameter seizuresSinceMs: when the store's first resync started reading; seizures
    ///   that ended before it never notify. `nil` (nothing read yet): no seizure facts.
    public init(model: StoreModel, nowMs: Int64, seizuresSinceMs: Int64?) {
        self.init()
        if let devices = model.devices {
            unregisteredDevices = Dictionary(
                devices.devices.filter { !$0.registered && $0.connected }.map { ($0.resource, $0) },
                uniquingKeysWith: { first, _ in first })
        }
        if model.seqs[.requests] != nil {
            pendingSecretRequests = Dictionary(
                model.pendingSecretRequests.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
            pendingAccountRequests = Dictionary(
                model.pendingAccountRequests.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        }
        if let queue = model.queue {
            stuckTasks = Dictionary(
                StuckRule.stuck(in: queue, history: model.queueHistory, nowMs: nowMs).map { ($0.id, $0) },
                uniquingKeysWith: { first, _ in first })
        }
        if let since = seizuresSinceMs, model.service != nil {
            seizures = Self.seizures(of: model, since: since)
        }
    }

    /// The seizures of `model` that ended at or after `since`:
    /// - every `claims.history` row with reason `seized`. The rows of one seizure (a hold and
    ///   the leases under it) end in one step: same resource, same holder's path, the taker's
    ///   path in `detail`, the same `ended_at_ms`, so they make one subject. The `claims.seized`
    ///   event of such a seizure carries no claim id, so it is not a subject itself: the store
    ///   re-reads the history when one arrives, and a seizure whose event was lost in a resync
    ///   is found in the history the resync reads;
    /// - every `claims.seized` event of a foreign claim (agent-device's), by its record id:
    ///   those end no hostler claim, so no history row has them.
    static func seizures(of model: StoreModel, since: Int64) -> [String: SeizureFact] {
        var facts: [String: SeizureFact] = [:]
        for row in model.claimHistory where row.reason == .seized && row.endedAtMs >= since {
            let by = row.detail ?? ""
            let key = "seizure:claims|\(row.resource)|\(row.workspacePath)|\(by)|\(row.endedAtMs)"
            facts[key] = SeizureFact(resource: row.resource, fromWorkspacePath: row.workspacePath,
                                     byWorkspacePath: by, atMs: row.endedAtMs)
        }
        for record in model.seizures where record.fromKind == .foreign && record.atMs >= since {
            facts["seizure:\(record.id)"] = SeizureFact(
                resource: record.resource, fromWorkspacePath: record.fromWorkspacePath,
                byWorkspacePath: record.byWorkspacePath, atMs: record.atMs)
        }
        return facts
    }

    /// Every subject these facts name.
    var subjects: Set<String> {
        var ids = Set<String>()
        for resource in (unregisteredDevices ?? [:]).keys { ids.insert("device:\(resource.rawValue)") }
        for id in (pendingSecretRequests ?? [:]).keys { ids.insert("secret-request:\(id)") }
        for id in (pendingAccountRequests ?? [:]).keys { ids.insert("account-request:\(id)") }
        for id in (stuckTasks ?? [:]).keys { ids.insert("stuck:\(id)") }
        for key in (seizures ?? [:]).keys { ids.insert(key) }
        return ids
    }
}

/// The subjects already notified (R1-4), with a bound. A subject stays remembered while the
/// facts still name it (a pending request, a stuck task, a connected unregistered device, a
/// seizure in the history window), so it never notifies twice while it lasts; the facts are
/// bounded by the model's own lists. Once a subject leaves the facts it moves to a window of
/// the `recentLimit` subjects that left most recently.
///
/// Repeat behaviour: a subject notifies again only if it leaves the facts, more than
/// `recentLimit` other subjects leave after it, and it then comes back (a request id, task id
/// or seizure does not come back; a device that is unplugged and plugged in again later can).
public struct NotifiedSubjects: Equatable, Sendable {
    public static let recentLimit = 2048

    /// The subjects the last facts named.
    public private(set) var active: Set<String> = []
    private var recentOrder: [String] = []
    private var recent: Set<String> = []

    public init() {}

    public func contains(_ id: String) -> Bool { active.contains(id) || recent.contains(id) }

    /// How many subjects are remembered (at most the facts' subjects plus `recentLimit`).
    public var count: Int { active.count + recent.count }

    /// Records `notified` as remembered and `current` as the subjects the facts now name.
    mutating func settle(current: Set<String>, notified: Set<String>) {
        let next = current.union(notified)
        for left in active.subtracting(next) where !recent.contains(left) {
            recent.insert(left)
            recentOrder.append(left)
        }
        for back in next where recent.contains(back) { recent.remove(back) }
        active = next
        while recent.count > Self.recentLimit, !recentOrder.isEmpty {
            recent.remove(recentOrder.removeFirst())
        }
        // Ids that came back stay in the order list until it is compacted.
        if recentOrder.count > 2 * Self.recentLimit {
            recentOrder = recentOrder.filter { recent.contains($0) }
        }
    }
}

/// Which notifications a change brings (spec §9.4; plan Task 8). Pure.
public enum NotificationRules {
    /// The notifications for what is in `new` and was not in `old`, gated by `settings`,
    /// skipping every subject in `notified`; returns them with the subjects remembered now.
    /// A part that `old` did not know yet is a baseline: its subjects are recorded, not posted,
    /// except pending requests, which give one summary when there are any.
    public static func evaluate(old: NotificationFacts, new: NotificationFacts,
                                settings: NotificationSettings,
                                notified: NotifiedSubjects) -> ([AppNotification], NotifiedSubjects) {
        var notes: [AppNotification] = []
        var posted = Set<String>()
        var notified = notified

        func consider(_ id: String, baseline: Bool, kind: NotificationKind, _ make: () -> AppNotification) {
            guard !notified.contains(id), !posted.contains(id) else { return }
            posted.insert(id)
            if !baseline, settings.allows(kind) { notes.append(make()) }
        }

        if let devices = new.unregisteredDevices {
            let old = old.unregisteredDevices
            for (resource, device) in devices.sorted(by: { $0.key.rawValue < $1.key.rawValue })
            where old?[resource] == nil {
                consider("device:\(resource.rawValue)", baseline: old == nil, kind: .newDevice) {
                    newDevice(device)
                }
            }
        }

        // Requests: the first time they are known, one summary for all that wait (P5-L11).
        let requestsBaseline = old.pendingSecretRequests == nil && old.pendingAccountRequests == nil
        var waiting = 0
        if let requests = new.pendingSecretRequests {
            let old = old.pendingSecretRequests
            for request in requests.values.sorted(by: { ($0.createdAtMs, $0.id) < ($1.createdAtMs, $1.id) })
            where old?[request.id] == nil {
                let id = "secret-request:\(request.id)"
                if requestsBaseline, !notified.contains(id) { waiting += 1 }
                consider(id, baseline: requestsBaseline, kind: .request) { secretRequested(request) }
            }
        }
        if let requests = new.pendingAccountRequests {
            let old = old.pendingAccountRequests
            for request in requests.values.sorted(by: { ($0.createdAtMs, $0.id) < ($1.createdAtMs, $1.id) })
            where old?[request.id] == nil {
                let id = "account-request:\(request.id)"
                if requestsBaseline, !notified.contains(id) { waiting += 1 }
                consider(id, baseline: requestsBaseline, kind: .request) { accountRequested(request) }
            }
        }
        if waiting > 0, settings.allows(.request) {
            notes.append(requestsWaiting(waiting))
        }

        if let tasks = new.stuckTasks {
            let old = old.stuckTasks
            for task in tasks.values.sorted(by: { ($0.ticket, $0.id) < ($1.ticket, $1.id) })
            where old?[task.id] == nil {
                consider("stuck:\(task.id)", baseline: old == nil, kind: .stuckTask) { stuck(task) }
            }
        }
        if let seizures = new.seizures {
            let old = old.seizures
            for (key, fact) in seizures.sorted(by: { ($0.value.atMs, $0.key) < ($1.value.atMs, $1.key) })
            where old?[key] == nil {
                consider(key, baseline: old == nil, kind: .seizure) { seized(fact, id: key) }
            }
        }
        notified.settle(current: new.subjects, notified: posted)
        return (notes, notified)
    }

    // MARK: Texts (English here; Task 12 adds Russian through the app's string catalog)
    //
    // Generic and bounded (review t9-r1 R1-1, P5-L17): Notification Center keeps banners and
    // history after the app hides its values or quits, so a title or body never carries free
    // text an agent (or a device) supplied — no request reason, command, project, secret,
    // account or folder name, no device name. Only fixed words, enums and counts; the id and the
    // target keep what navigation needs, and the popover and the window show the details.

    static func newDevice(_ device: DeviceInfo) -> AppNotification {
        AppNotification(
            kind: .newDevice, id: "device:\(device.resource.rawValue)",
            title: String(localized: "New device connected"),
            body: device.platform == .ios
                ? String(localized: "An iOS device is not registered with hostler yet. Open hostler to add it.")
                : String(localized: "An Android device is not registered with hostler yet. Open hostler to add it."),
            target: .device(device.resource))
    }

    static func secretRequested(_ request: SecretRequestInfo) -> AppNotification {
        AppNotification(
            kind: .request, id: "secret-request:\(request.id)",
            title: String(localized: "Secret requested"),
            body: String(localized: "An agent is waiting for a secret. Open hostler to answer."),
            target: .popover)
    }

    static func accountRequested(_ request: AccountRequestInfo) -> AppNotification {
        AppNotification(
            kind: .request, id: "account-request:\(request.id)",
            title: String(localized: "Account requested"),
            body: String(localized: "An agent is waiting for an account. Open hostler to answer."),
            target: .popover)
    }

    static func stuck(_ task: HeavyEntry) -> AppNotification {
        AppNotification(
            kind: .stuckTask, id: "stuck:\(task.id)",
            title: String(localized: "A heavy task looks stuck"),
            body: String(localized: "It has run longer than usual. Open hostler's queue to see it."),
            target: .queue)
    }

    static func seized(_ fact: SeizureFact, id: String) -> AppNotification {
        AppNotification(
            kind: .seizure, id: id,
            title: String(localized: "A claim was taken over"),
            body: String(localized: "Another workspace took it over. Open hostler to see which."),
            target: .popover)
    }

    static func requestsWaiting(_ count: Int) -> AppNotification {
        AppNotification(
            kind: .request, id: "requests-waiting",
            title: count == 1 ? String(localized: "A request is waiting")
                : String(localized: "\(count) requests are waiting"),
            body: String(localized: "Open hostler to answer."),
            target: .popover)
    }
}
