import Foundation

/// The wall clock, behind a protocol so features are tested with `FakeClock`. Named
/// `WallClock` because Swift's own `Clock` protocol (for `Duration`) is a different thing.
public protocol WallClock: Sendable {
    func now() -> Date
}

public struct SystemClock: WallClock {
    public init() {}
    public func now() -> Date { Date() }
}
