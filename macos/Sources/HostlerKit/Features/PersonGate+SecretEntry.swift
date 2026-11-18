import Foundation
import Observation

/// What a person types into a `SecureField`: a value, a field of an account, or the master
/// password (spec §9.6). The field is the only `String` such a value ever is in the app.
///
/// - `take()` turns the text into a `SecretText` and clears the field in the same step, so the
///   value leaves the field the moment it is sent.
/// - `description`, `debugDescription` and the mirror are `<redacted>`.
///
/// **The honest limit.** SwiftUI's `SecureField` binds a `String`, and AppKit's secure field
/// keeps its own copy while it is edited. Clearing sets the text to `""`; the old string's
/// memory is not zeroed (Swift strings cannot be). Keep entries short-lived: every outcome and
/// every close of the surface that shows one clears it.
@MainActor
@Observable
public final class SecretEntry: CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    /// The field's text; a view binds it to a `SecureField` and reads nothing else from it.
    public var text: String = ""

    public init() {}

    public var isEmpty: Bool { text.isEmpty }

    /// The typed value as a `SecretText`, the field cleared at once; `nil` when empty.
    public func take() -> SecretText? {
        guard !text.isEmpty else { return nil }
        let value = SecretText(text)
        clear()
        return value
    }

    public func clear() {
        if !text.isEmpty { text = "" }
    }

    public nonisolated var description: String { "<redacted>" }
    public nonisolated var debugDescription: String { "<redacted>" }
    public nonisolated var customMirror: Mirror {
        Mirror(self, children: [], displayStyle: .class)
    }
}
