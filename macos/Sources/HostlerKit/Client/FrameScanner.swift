import Foundation

/// The top-level members of one server frame, found without decoding any value: each key's
/// value is a byte range of the line. The reader routes by the keys alone, as Rust's `route()`
/// does, so a line that may hold values (an answer to `secret.reveal`) is never parsed into a
/// `JSONValue` or a `String`; only the `result` bytes are copied, straight into a `SecretText`.
struct FrameHead: Equatable {
    /// The members, by key (keys without escapes; a key with an escape is kept as written).
    var members: [String: Range<Int>]

    /// The frame's `id` when it is a plain unsigned integer.
    func id(in line: UnsafeRawBufferPointer) -> UInt64? {
        guard let range = members["id"], !range.isEmpty, range.count <= 20 else { return nil }
        var value: UInt64 = 0
        for index in range {
            let byte = line[index]
            guard byte >= UInt8(ascii: "0"), byte <= UInt8(ascii: "9") else { return nil }
            let (times, overflow1) = value.multipliedReportingOverflow(by: 10)
            let (sum, overflow2) = times.addingReportingOverflow(UInt64(byte - UInt8(ascii: "0")))
            if overflow1 || overflow2 { return nil }
            value = sum
        }
        return value
    }

    func has(_ key: String) -> Bool { members[key] != nil }
}

enum FrameScanner {
    /// Scans one line. `nil` when it is not a well-formed JSON object (strings, nesting and
    /// literals are checked for balance only; the values themselves are read by their decoder).
    static func head(_ line: UnsafeRawBufferPointer) -> FrameHead? {
        var scanner = Cursor(bytes: line)
        scanner.skipSpace()
        guard scanner.take(UInt8(ascii: "{")) else { return nil }
        var members: [String: Range<Int>] = [:]
        scanner.skipSpace()
        if scanner.take(UInt8(ascii: "}")) {
            scanner.skipSpace()
            return scanner.atEnd ? FrameHead(members: members) : nil
        }
        while true {
            scanner.skipSpace()
            guard let keyRange = scanner.string() else { return nil }
            let key = String(decoding: line[(keyRange.lowerBound + 1)..<(keyRange.upperBound - 1)],
                             as: UTF8.self)
            scanner.skipSpace()
            guard scanner.take(UInt8(ascii: ":")) else { return nil }
            scanner.skipSpace()
            guard let valueRange = scanner.value() else { return nil }
            members[key] = valueRange
            scanner.skipSpace()
            if scanner.take(UInt8(ascii: ",")) { continue }
            guard scanner.take(UInt8(ascii: "}")) else { return nil }
            scanner.skipSpace()
            return scanner.atEnd ? FrameHead(members: members) : nil
        }
    }

    private struct Cursor {
        let bytes: UnsafeRawBufferPointer
        var at = 0

        var atEnd: Bool { at >= bytes.count }

        mutating func skipSpace() {
            while at < bytes.count {
                switch bytes[at] {
                case 0x20, 0x09, 0x0A, 0x0D: at += 1
                default: return
                }
            }
        }

        mutating func take(_ byte: UInt8) -> Bool {
            guard at < bytes.count, bytes[at] == byte else { return false }
            at += 1
            return true
        }

        /// A string, quotes included.
        mutating func string() -> Range<Int>? {
            let start = at
            guard take(UInt8(ascii: "\"")) else { return nil }
            while at < bytes.count {
                let byte = bytes[at]
                at += 1
                if byte == UInt8(ascii: "\\") {
                    guard at < bytes.count else { return nil }
                    at += 1
                } else if byte == UInt8(ascii: "\"") {
                    return start..<at
                } else if byte < 0x20 {
                    return nil
                }
            }
            return nil
        }

        /// Any value: a string, an object or array (balanced), or a bare literal or number.
        mutating func value() -> Range<Int>? {
            guard at < bytes.count else { return nil }
            let start = at
            switch bytes[at] {
            case UInt8(ascii: "\""):
                return string()
            case UInt8(ascii: "{"), UInt8(ascii: "["):
                var depth = 0
                while at < bytes.count {
                    switch bytes[at] {
                    case UInt8(ascii: "\""):
                        guard string() != nil else { return nil }
                        continue
                    case UInt8(ascii: "{"), UInt8(ascii: "["):
                        depth += 1
                    case UInt8(ascii: "}"), UInt8(ascii: "]"):
                        depth -= 1
                        if depth == 0 {
                            at += 1
                            return start..<at
                        }
                    default:
                        break
                    }
                    at += 1
                }
                return nil
            default:
                while at < bytes.count {
                    switch bytes[at] {
                    case UInt8(ascii: ","), UInt8(ascii: "}"), UInt8(ascii: "]"),
                         0x20, 0x09, 0x0A, 0x0D:
                        return at > start ? start..<at : nil
                    default:
                        at += 1
                    }
                }
                return at > start ? start..<at : nil
            }
        }
    }
}
