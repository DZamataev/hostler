/// A mutable cell a test closure can write to (the reference app's `Support/Box.swift`). Only the
/// test's own (main) thread touches it.
final class Box<Value>: @unchecked Sendable {
    var value: Value
    init(_ value: Value) { self.value = value }
}
