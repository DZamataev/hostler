import Foundation

// The window's queue and screen (plan Task 11; spec §9.3, §11.1): the actions a person takes on
// a heavy task and on the screen, and the pure model both screens draw. The queue's rows come
// from Task 8's snapshots; the view only lays them out.

// MARK: - Outcomes of the actions that may pass the person gate

/// How a window action that may pass the person gate (spec §8.5) ended: the call's value, the
/// person closing the sheet, or a failure the app shows. `QueueActions.stop` and
/// `SecretsActions.removePreserved` answer this, so a view tells "cancelled" from "failed".
public enum ActionOutcome<Value: Sendable>: Sendable {
    case done(Value)
    case cancelled
    case failed(ActionFailure)
}

// MARK: - The filter

/// The queue's segmented filter (spec §9.3: All / Running / Waiting / Finished).
public enum QueueFilter: String, CaseIterable, Identifiable, Sendable {
    case all
    case running
    case waiting
    case finished

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .all: String(localized: "All")
        case .running: String(localized: "Running")
        case .waiting: String(localized: "Waiting")
        case .finished: String(localized: "Finished")
        }
    }
}

/// How many rows each filter shows (the segments carry their counts, as DeviceDeck's Jobs do).
public struct QueueCounts: Equatable, Sendable {
    public var running: Int
    public var waiting: Int
    public var finished: Int

    public init(running: Int = 0, waiting: Int = 0, finished: Int = 0) {
        self.running = running
        self.waiting = waiting
        self.finished = finished
    }

    public var all: Int { running + waiting + finished }

    public func count(_ filter: QueueFilter) -> Int {
        switch filter {
        case .all: all
        case .running: running
        case .waiting: waiting
        case .finished: finished
        }
    }

    /// The segment's label: "All 3", "Running 1", … (DeviceDeck's "Running 0").
    public func segmentTitle(_ filter: QueueFilter) -> String {
        "\(filter.title) \(count(filter))"
    }
}

// MARK: - One row

/// One task of the queue list: a running or waiting entry, or a finished history item. `id` is
/// unique across the three because a finished task keeps the id it ran under.
public struct QueueRow: Identifiable, Equatable, Sendable {
    /// Where the task is now. `stopping` is spec §11.1's persisted stop in progress.
    public enum Phase: String, Equatable, Sendable, CaseIterable {
        case waiting
        case running
        case stopping
        case finished

        public var title: String {
            switch self {
            case .waiting: String(localized: "waiting")
            case .running: String(localized: "running")
            case .stopping: String(localized: "stopping")
            case .finished: String(localized: "finished")
            }
        }
    }

    public var id: String
    public var taskId: String
    public var ticket: UInt64
    public var kind: String
    public var command: String
    public var cwd: String
    public var workspacePath: String?
    public var workspace: WorkspaceChip?
    public var phase: Phase
    /// Its place in line while it waits (1 is next).
    public var position: Int?
    /// How many wait, position included.
    public var of: Int?
    public var outcome: HeavyOutcome?
    public var exitCode: Int32?
    public var enqueuedAtMs: Int64
    public var grantedAtMs: Int64?
    public var finishedAtMs: Int64?
    public var holderPid: UInt32?
    public var source: EntrySource?
    /// A task another tool keeps (rn-looky's ledger): shown, never stopped or withdrawn here.
    public var isForeign: Bool
    /// "running 4 min" / "2 min 10 s".
    public var durationText: String
    /// "started 4 min ago" / "finished 2 min ago".
    public var timeText: String
    /// "2 of 3" while it waits.
    public var positionText: String?
    /// The outcome badge's text ("ok", "stopped", …); finished rows only.
    public var outcomeText: String?

    /// A running task of hostler's own: the service can stop it (§11.1). A stopping one cannot
    /// be stopped a second time, a foreign one is never stopped, and rn-looky's are refused.
    public var canStop: Bool {
        phase == .running && !isForeign && source != .rnLookyLedger
    }

    /// A waiting request of hostler's: `heavy.withdraw` removes it.
    public var canWithdraw: Bool {
        phase == .waiting && !isForeign && source != .rnLookyLedger
    }

    /// The task's outcome as words; `nil` while it has not finished.
    public static func outcomeTitle(_ outcome: HeavyOutcome) -> String {
        switch outcome {
        case .ok: String(localized: "ok")
        case .failed: String(localized: "failed")
        case .withdrawn: String(localized: "withdrawn")
        case .timedOut: String(localized: "timed out")
        case .reaped: String(localized: "reaped")
        case .stopped: String(localized: "stopped")
        case .forceReleased: String(localized: "force released")
        }
    }
}

// MARK: - The rows

/// Every row of the queue's list, filtered (pure; `nowMs` is the caller's clock). Running tasks
/// keep the service's order, waiting ones are ordered by ticket, and finished ones are newest
/// first. All three groups are shown together under `all`, in that order.
public func queueRows(queue: QueueSnapshot?, history: [HeavyHistoryItem],
                      filter: QueueFilter = .all, nowMs: Int64,
                      workspaces: [WorkspaceInfo] = []) -> [QueueRow] {
    let running = (queue?.running ?? []) + (queue?.foreignRunning ?? [])
    let foreign = Set((queue?.foreignRunning ?? []).map(\.id))
    let runningRows = running.map { entry in
        entryRow(entry, foreign: foreign.contains(entry.id), workspaces: workspaces,
                 nowMs: nowMs)
    }
    let waitingCount = queue?.waiting.count ?? 0
    let waitingRows = (queue?.waiting ?? []).enumerated().map { index, entry in
        var waiting = entryRow(entry, foreign: false, workspaces: workspaces, nowMs: nowMs)
        waiting.position = index + 1
        waiting.of = waitingCount
        waiting.positionText = String(localized: "\(index + 1) of \(waitingCount)")
        waiting.timeText = String(localized: "waiting \(Span.duration(nowMs - entry.enqueuedAtMs))")
        return waiting
    }
    let finishedRows = history
        .sorted { a, b in
            a.finishedAtMs == b.finishedAtMs
                ? (a.ticket, a.id) > (b.ticket, b.id) : a.finishedAtMs > b.finishedAtMs
        }
        .map { item in finishedRow(item, workspaces: workspaces, nowMs: nowMs) }

    return (runningRows + waitingRows + finishedRows).filter { row in
        switch filter {
        case .all: true
        case .running: row.phase == .running || row.phase == .stopping
        case .waiting: row.phase == .waiting
        case .finished: row.phase == .finished
        }
    }
}

/// The counts the segments show, over every row (not the filtered ones).
public func queueCounts(queue: QueueSnapshot?, history: [HeavyHistoryItem]) -> QueueCounts {
    QueueCounts(running: (queue?.running.count ?? 0) + (queue?.foreignRunning.count ?? 0),
                waiting: queue?.waiting.count ?? 0, finished: history.count)
}

/// One task's relative time: "just now" or "3 min ago".
private func agoText(_ startMs: Int64, nowMs: Int64) -> String {
    let seconds = max(0, nowMs - startMs) / 1000
    if seconds < 60 { return String(localized: "just now") }
    let minutes = seconds / 60
    if minutes < 60 { return String(localized: "\(minutes) min ago") }
    let hours = minutes / 60
    if hours < 24 { return String(localized: "\(hours) h ago") }
    return String(localized: "\(hours / 24) d ago")
}

private func entryRow(_ entry: HeavyEntry, foreign: Bool, workspaces: [WorkspaceInfo],
                      nowMs: Int64) -> QueueRow {
    let since = entry.grantedAtMs ?? entry.enqueuedAtMs
    let phase: QueueRow.Phase = switch entry.state {
    case .waiting: .waiting
    case .running: .running
    case .stopping: .stopping
    }
    let timeText = switch phase {
    case .waiting:
        String(localized: "waiting \(Span.duration(nowMs - entry.enqueuedAtMs))")
    case .running, .stopping: String(localized: "started \(agoText(since, nowMs: nowMs))")
    case .finished: ""
    }
    return QueueRow(
        id: "\(phase.rawValue)|\(entry.id)", taskId: entry.id, ticket: entry.ticket,
        kind: entry.kind, command: entry.command, cwd: entry.cwd,
        workspacePath: entry.workspaceRoot, workspace: WorkspaceChip.of(
            path: entry.workspaceRoot ?? entry.cwd, workspaceId: nil, workspaces: workspaces),
        phase: phase, outcome: nil, exitCode: nil, enqueuedAtMs: entry.enqueuedAtMs,
        grantedAtMs: entry.grantedAtMs, finishedAtMs: nil, holderPid: entry.holder.pid,
        source: entry.source, isForeign: foreign,
        durationText: Span.duration(nowMs - since), timeText: timeText,
        positionText: nil, outcomeText: nil)
}

private func finishedRow(_ item: HeavyHistoryItem, workspaces: [WorkspaceInfo],
                         nowMs: Int64) -> QueueRow {
    let startedAt = item.grantedAtMs ?? item.enqueuedAtMs
    return QueueRow(
        id: "finished|\(item.id)", taskId: item.id, ticket: item.ticket, kind: item.kind,
        command: item.command, cwd: item.cwd, workspacePath: item.workspaceRoot,
        workspace: WorkspaceChip.of(path: item.workspaceRoot ?? item.cwd, workspaceId: nil,
                                    workspaces: workspaces),
        phase: .finished, outcome: item.outcome, exitCode: item.exitCode,
        enqueuedAtMs: item.enqueuedAtMs, grantedAtMs: item.grantedAtMs,
        finishedAtMs: item.finishedAtMs, holderPid: nil, source: nil, isForeign: false,
        durationText: Span.duration(item.finishedAtMs - startedAt),
        timeText: String(localized: "finished \(agoText(item.finishedAtMs, nowMs: nowMs))"),
        positionText: nil, outcomeText: QueueRow.outcomeTitle(item.outcome))
}

// MARK: - The queue's actions

/// The queue's actions (plan Task 11; spec §9.3, §11.1), all against the finished service
/// methods:
/// - withdraw a waiting request (`heavy.withdraw`);
/// - stop a running task: the person types its id, then `heavy.stop {id, confirmation}` goes
///   through the person gate (the service stops and verifies, Task 6; the app never signals a
///   process itself);
/// - set the capacity (`queue.set_capacity`, 1–16).
///
/// A wrong typed id, or a capacity outside 1–16, sends nothing: the test proves it.
@MainActor
public struct QueueActions {
    public var client: HostlerClient
    public var gate: PersonGate

    /// The capacity the service and the plan allow (spec §11.1: 1–16).
    public static let capacityRange: ClosedRange<Int> = 1...16

    public init(client: HostlerClient, gate: PersonGate) {
        self.client = client
        self.gate = gate
    }

    /// Whether `typed` is `taskId`, trimmed: nothing is sent otherwise.
    public static func confirmationMatches(_ typed: String, taskId: String) -> Bool {
        typed.trimmingCharacters(in: .whitespacesAndNewlines) == taskId
    }

    public func withdraw(_ row: QueueRow) async -> ActionFailure? {
        do throws(HostlerCallError) {
            try await client.heavyWithdraw(id: row.taskId, timedOut: false)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// Stops `row` behind the person gate. `typed` must be the task's id; otherwise nothing is
    /// sent and the answer is `.failed(.confirmationMismatch)`.
    public func stop(_ row: QueueRow, typed: String) async -> ActionOutcome<QueueStopOutcome> {
        guard Self.confirmationMatches(typed, taskId: row.taskId) else {
            return .failed(.confirmationMismatch(expected: row.taskId))
        }
        let id = row.taskId
        let result = await gate.perform(String(localized: "Stop \(row.command)"), from: .window) {
            (client: HostlerClient) async throws(HostlerCallError) -> QueueStopOutcome in
            try await client.heavyStop(id: id, confirmation: id)
        }
        switch result {
        case .done(let outcome): return .done(outcome)
        case .cancelled: return .cancelled
        case .failed(let error): return .failed(.app(error))
        }
    }

    /// `queue.set_capacity`, refusing a value outside 1–16 without sending anything.
    public func setCapacity(_ capacity: Int) async -> ActionFailure? {
        guard Self.capacityRange.contains(capacity) else {
            return .app(.capacityOutOfRange(capacity))
        }
        do throws(HostlerCallError) {
            try await client.queueSetCapacity(capacity: UInt8(capacity))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }
}

// MARK: - The screen

/// A request of another workspace that waits for the screen.
public struct ScreenWaitingRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var request: WaitingRequest
    public var workspace: WorkspaceChip
    public var kindText: String
    public var sinceText: String
}

/// What the Screen section shows (spec §9.3): the screen's resource, who holds it (a lease is a
/// run using it, a hold is a hold), since when, the TTL left, its command, and who waits.
public struct ScreenModel: Equatable, Sendable {
    public var resourceName: String
    public var isFree: Bool
    public var claims: [ClaimRow]
    public var waiting: [ScreenWaitingRow]

    public var holder: ClaimRow? { claims.first }
}

/// The screen's facts from the claims snapshot (pure). The screen is "free" when nothing holds
/// it, which also covers a snapshot that does not list it yet.
public func screenModel(_ snapshot: ClaimsSnapshot?, nowMs: Int64) -> ScreenModel {
    let state = snapshot?.resources.first { $0.resource.kind == .screen }
    let claims = claimRows(state?.claims ?? [], nowMs: nowMs)
    let waiting = (state?.waiting ?? []).map { request in
        ScreenWaitingRow(
            id: request.requestId, request: request,
            workspace: WorkspaceChip(workspaceId: nil, path: request.workspacePath),
            kindText: request.kind.title,
            sinceText: Span.since(request.sinceMs, nowMs: nowMs))
    }
    return ScreenModel(resourceName: state?.resource.display ?? "screen",
                       isFree: claims.isEmpty, claims: claims, waiting: waiting)
}

/// The screen's two actions (plan Task 11), both the CLI's (§9.7: a takeover stops the run that
/// holds it, and only the CLI knows that procedure): `hostler release <claim> --force
/// --confirmation screen` and `hostler hold screen --force --confirmation screen --ttl 30`.
/// The token is always the word `screen` (the service's `--confirmation` token for it), and
/// nothing runs unless the person typed it.
public struct ScreenActions: Sendable {
    public var runner: any HostlerCommandRunning
    /// The `hostler` binary the CLI actions run: `ServiceStatus.exe`.
    public var cliExecutable: String?

    public static let confirmationToken = "screen"

    public init(runner: any HostlerCommandRunning, cliExecutable: String?) {
        self.runner = runner
        self.cliExecutable = cliExecutable
    }

    public static func confirmationMatches(_ typed: String) -> Bool {
        typed.trimmingCharacters(in: .whitespacesAndNewlines) == confirmationToken
    }

    /// Ends the claim without stopping its run, in the claim's workspace.
    public func release(_ claim: Claim, typed: String) async -> ActionFailure? {
        guard Self.confirmationMatches(typed) else {
            return .confirmationMismatch(expected: Self.confirmationToken)
        }
        return await run(arguments: ["release", claim.id, "--force", "--confirmation",
                                     Self.confirmationToken],
                         cwd: claim.workspace.path)
    }

    /// Takes the screen over for `workspace`: the CLI stops the run that holds it, then holds
    /// it for that workspace for 30 minutes.
    public func takeOver(in workspace: WorkspaceInfo, typed: String) async -> ActionFailure? {
        guard Self.confirmationMatches(typed) else {
            return .confirmationMismatch(expected: Self.confirmationToken)
        }
        return await run(arguments: ["hold", "screen", "--force", "--confirmation",
                                     Self.confirmationToken, "--ttl", "30"],
                         cwd: workspace.path)
    }

    private func run(arguments: [String], cwd: String) async -> ActionFailure? {
        guard let executable = cliExecutable else { return .noCommandLine }
        let invocation = CLIInvocation(executable: executable, arguments: arguments, cwd: cwd,
                                       timeout: CLIInvocation.takeoverTimeout)
        let result = await runner.run(invocation)
        if let error = result.appError { return .app(error) }
        return nil
    }
}
