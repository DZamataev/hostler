import AppKit
import Foundation

/// The pasteboard as the reveal's Copy uses it (spec §9.6). OS-facing, so the clearing rule is
/// tested against a private pasteboard and a fake sleeper.
@MainActor
public protocol PasteboardWriting: AnyObject {
    /// Changes whenever anything writes the pasteboard.
    var changeCount: Int { get }
    /// Replaces the pasteboard with `value` as plain text, marked concealed; returns the
    /// change count after the write.
    func writeConcealed(_ value: SecretText) -> Int
    func clear()
}

/// An `NSPasteboard` (the general one in the app). The value is written as UTF-8 bytes from
/// the `SecretText`, never through a Swift `String`, with the `org.nspasteboard.ConcealedType`
/// marker that clipboard managers honour by not recording it.
@MainActor
public final class SystemPasteboard: PasteboardWriting {
    /// nspasteboard.org's marker for passwords and other values a clipboard history must skip.
    public static let concealedType = NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType")

    private let pasteboard: NSPasteboard

    public init(_ pasteboard: NSPasteboard = .general) {
        self.pasteboard = pasteboard
    }

    public var changeCount: Int { pasteboard.changeCount }

    public func writeConcealed(_ value: SecretText) -> Int {
        pasteboard.clearContents()
        let item = NSPasteboardItem()
        value.withBytes { bytes in
            _ = item.setData(Data(bytes), forType: .string)
        }
        _ = item.setData(Data(), forType: Self.concealedType)
        pasteboard.writeObjects([item])
        return pasteboard.changeCount
    }

    public func clear() {
        pasteboard.clearContents()
    }
}

/// Copy with a clear (spec §9.6, the reference app's invariant 4 pattern): after `clearAfter` the
/// pasteboard is cleared, but only if its change count is still the one our write left —
/// anything the person copied since stays untouched. A newer copy replaces the pending clear.
@MainActor
public final class ConcealedClipboard {
    public static let clearAfter: Duration = .seconds(60)

    private let pasteboard: any PasteboardWriting
    private let sleeper: any Sleeping
    private let delay: Duration
    /// The change count our last write left; `nil` once cleared or replaced by someone else.
    private var written: Int?
    private var pending: Task<Void, Never>?

    public init(pasteboard: any PasteboardWriting, sleeper: any Sleeping,
                clearAfter: Duration = ConcealedClipboard.clearAfter) {
        self.pasteboard = pasteboard
        self.sleeper = sleeper
        delay = clearAfter
    }

    /// Copies `value` and schedules the clear.
    public func copy(_ value: SecretText) {
        pending?.cancel()
        let count = pasteboard.writeConcealed(value)
        written = count
        let sleeper = sleeper
        let delay = delay
        pending = Task { [weak self] in
            do { try await sleeper.sleep(for: delay) } catch { return }
            self?.clearIfStillOurs(count)
        }
    }

    /// Clears now if the pasteboard still holds our last copy (the app quitting).
    public func clearIfStillOurs() {
        guard let written else { return }
        pending?.cancel()
        clearIfStillOurs(written)
    }

    private func clearIfStillOurs(_ count: Int) {
        guard written == count else { return }
        written = nil
        pending = nil
        if pasteboard.changeCount == count { pasteboard.clear() }
    }
}
