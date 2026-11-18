import Foundation

/// When a running heavy task looks stuck (spec §9.4; plan Task 8): it has run longer than
/// `max(2 × the longest of the last 10 finished tasks of its kind, 30 min)`; with fewer than 3
/// finished tasks of its kind to compare with, longer than 60 min. Pure.
public enum StuckRule {
    public static let floorMs: Int64 = 30 * 60 * 1000
    public static let fewHistoryBoundMs: Int64 = 60 * 60 * 1000
    /// How many finished tasks of the kind are compared.
    public static let window = 10
    /// Fewer finished tasks than this and the bound is `fewHistoryBoundMs`.
    public static let minimumHistory = 3

    /// The run time after which a task of `kind` is stuck, in ms.
    ///
    /// A finished task's run time is `finishedAtMs − grantedAtMs`; tasks that never ran
    /// (withdrawn or timed out while waiting) have no `grantedAtMs` and do not count.
    public static func boundMs(kind: String, history: [HeavyHistoryItem]) -> Int64 {
        let runs = history
            .filter { $0.kind == kind && $0.grantedAtMs != nil }
            .sorted { $0.finishedAtMs > $1.finishedAtMs }
            .prefix(window)
            .map { max(0, $0.finishedAtMs - ($0.grantedAtMs ?? $0.finishedAtMs)) }
        guard runs.count >= minimumHistory, let longest = runs.max() else {
            return fewHistoryBoundMs
        }
        let (doubled, overflow) = longest.multipliedReportingOverflow(by: 2)
        return max(overflow ? Int64.max : doubled, floorMs)
    }

    /// Whether `entry` has run longer than its kind's bound at `nowMs`. A waiting task is
    /// never stuck; a task being stopped still is, until it ends.
    public static func isStuck(_ entry: HeavyEntry, history: [HeavyHistoryItem], nowMs: Int64) -> Bool {
        guard entry.state != .waiting, let granted = entry.grantedAtMs else { return false }
        return nowMs - granted > boundMs(kind: entry.kind, history: history)
    }

    /// The running tasks of `queue` that are stuck at `nowMs`, in the queue's order.
    public static func stuck(in queue: QueueSnapshot, history: [HeavyHistoryItem], nowMs: Int64) -> [HeavyEntry] {
        queue.running.filter { isStuck($0, history: history, nowMs: nowMs) }
    }
}
