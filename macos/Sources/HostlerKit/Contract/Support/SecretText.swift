import Foundation

/// A secret value or a command that may hold one (Rust's `SecretValue` and `RawCommand`): the
/// only type the generated contract keeps values in (plan P5 Task 3, R2-8).
///
/// - Its bytes live in one buffer of its own, allocated and filled in `init` and never written
///   again until `deinit` zeroes them (`memset_s`, which the compiler may not drop) and frees
///   them.
/// - `description`, `debugDescription` and its mirror are `<redacted>`, so `print`,
///   `String(describing:)`, `dump`, string interpolation and a `Logger` never show the value.
/// - `==` compares in constant time for equal lengths.
/// - On the wire it is a JSON string.
///
/// **Why `@unchecked Sendable` is sound.** The storage is immutable after `init` and is only
/// read (`withBytes`, `reveal`, `==`, `encode`). The one write is in `deinit`, which runs only
/// when no other reference to the instance exists, so no other thread can be reading the bytes
/// then. Reads that overlap are reads of memory nobody writes, which is data-race free.
///
/// **The honest limit.** A `String` made by `reveal()`, by `encode(to:)` (the JSON encoder's
/// output) or by decoding (the decoder's input) is ordinary memory that cannot be zeroed; keep
/// those short-lived, and never log or store them.
public final class SecretText: @unchecked Sendable, Codable, Equatable, CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    private let bytes: UnsafeMutableRawBufferPointer
    /// The value's length in bytes (UTF-8).
    public let byteCount: Int

    /// Allocates the buffer (at least one byte, so it always has a base address), zeroes it and
    /// lets `fill` write the value; nothing writes it afterwards.
    private init(byteCount: Int, fill: (UnsafeMutableRawBufferPointer) -> Void) {
        self.byteCount = byteCount
        bytes = .allocate(byteCount: max(byteCount, 1), alignment: 1)
        bytes.initializeMemory(as: UInt8.self, repeating: 0)
        fill(UnsafeMutableRawBufferPointer(rebasing: bytes[0..<byteCount]))
    }

    /// Copies `source`; the caller's copy is the caller's to clear.
    public convenience init(copying source: UnsafeRawBufferPointer) {
        self.init(byteCount: source.count) { $0.copyMemory(from: source) }
    }

    /// Copies `data`; the caller's copy is the caller's to clear.
    public convenience init(_ data: Data) {
        self.init(byteCount: data.count) { buffer in
            data.withUnsafeBytes { buffer.copyMemory(from: $0) }
        }
    }

    /// Copies the string's UTF-8 straight into the buffer.
    public convenience init(_ string: String) {
        let utf8 = string.utf8
        self.init(byteCount: utf8.count) { buffer in
            buffer.copyBytes(from: utf8)
        }
    }

    deinit {
        _ = memset_s(bytes.baseAddress, bytes.count, 0, bytes.count)
        bytes.deallocate()
    }

    /// Reads the bytes in place; `body` must not keep the pointer.
    public func withBytes<R>(_ body: (UnsafeRawBufferPointer) throws -> R) rethrows -> R {
        try body(UnsafeRawBufferPointer(rebasing: bytes[0..<byteCount]))
    }

    /// The value as a `String`, for a view that shows it; see the honest limit above.
    public func reveal() -> String {
        withBytes { String(decoding: $0, as: UTF8.self) }
    }

    public var description: String { "<redacted>" }
    public var debugDescription: String { "<redacted>" }
    public var customMirror: Mirror { Mirror(self, children: [], displayStyle: .struct) }

    public static func == (lhs: SecretText, rhs: SecretText) -> Bool {
        guard lhs.byteCount == rhs.byteCount else { return false }
        return lhs.withBytes { a in
            rhs.withBytes { b in
                var difference: UInt8 = 0
                for i in 0..<a.count {
                    difference |= a[i] ^ b[i]
                }
                return difference == 0
            }
        }
    }

    public convenience init(from decoder: any Decoder) throws {
        self.init(try decoder.singleValueContainer().decode(String.self))
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(reveal())
    }
}
