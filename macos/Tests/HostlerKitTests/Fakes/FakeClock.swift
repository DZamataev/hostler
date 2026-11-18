import Foundation
@testable import HostlerKit

/// A settable wall clock. A class with a lock so a test can move it while a feature holds it.
final class FakeClock: WallClock, @unchecked Sendable {
    private let lock = NSLock()
    private var current: Date

    init(_ start: Date = Date(timeIntervalSinceReferenceDate: 1_000)) { current = start }

    func now() -> Date { lock.withLock { current } }

    func advance(by seconds: TimeInterval) { lock.withLock { current += seconds } }
}
