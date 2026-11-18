import Foundation
import Testing

/// A grep test (Review Focus 2): the client and socket code logs methods, ids, byte counts and
/// fixed texts only, never a frame, a line, a value or a decoder's message; and it never prints.
@Suite struct NoFrameInLogsTests {
    static let sources = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().appending(path: "Sources/HostlerKit")

    static let files = ["Client/Connection.swift", "Client/ConnectionController.swift",
                        "Client/FrameScanner.swift", "Services/Socket.swift",
                        "Services/HostlerCLI.swift", "Services/ServiceStarter.swift"]

    /// What a log line may interpolate.
    static let allowed: Set<String> = [
        "method.rawValue, privacy: .public", "entry.method.rawValue, privacy: .public",
        "id", "frame.data.count", "range.count", "data.count", "byteCount",
        "reply.serviceVersion, privacy: .public", "reason.localizedDescription, privacy: .public",
        "subcommand, privacy: .public", "output.exit", "output.stdout.count",
    ]

    /// Every `Log.<category>.<level>(…)` call's text, parentheses balanced.
    static func logCalls(_ text: String) -> [String] {
        var calls: [String] = []
        var search = text.startIndex
        while let found = text.range(of: "Log.", range: search..<text.endIndex) {
            guard let open = text[found.upperBound...].firstIndex(of: "(") else { break }
            var depth = 0
            var index = open
            while index < text.endIndex {
                if text[index] == "(" { depth += 1 }
                if text[index] == ")" {
                    depth -= 1
                    if depth == 0 { break }
                }
                index = text.index(after: index)
            }
            calls.append(String(text[open...min(index, text.index(before: text.endIndex))]))
            search = index < text.endIndex ? text.index(after: index) : text.endIndex
        }
        return calls
    }

    /// The `\(…)` interpolations of `call`.
    static func interpolations(_ call: String) -> [String] {
        var found: [String] = []
        var rest = call[...]
        while let start = rest.range(of: "\\(") {
            var depth = 1
            var index = start.upperBound
            while index < rest.endIndex, depth > 0 {
                if rest[index] == "(" { depth += 1 }
                if rest[index] == ")" { depth -= 1 }
                if depth > 0 { index = rest.index(after: index) }
            }
            found.append(String(rest[start.upperBound..<index]))
            rest = index < rest.endIndex ? rest[rest.index(after: index)...] : ""
        }
        return found
    }

    @Test func everyLogInterpolationIsAMethodAnIdACountOrAFixedText() throws {
        var calls = 0
        for file in Self.files {
            let text = try String(contentsOf: Self.sources.appending(path: file), encoding: .utf8)
            for call in Self.logCalls(text) {
                calls += 1
                for value in Self.interpolations(call) {
                    #expect(Self.allowed.contains(value), "\(file): logs \\(\(value))")
                }
            }
        }
        #expect(calls >= 9)
    }

    @Test func nothingPrints() throws {
        for file in Self.files {
            let text = try String(contentsOf: Self.sources.appending(path: file), encoding: .utf8)
            for banned in ["print(", "debugPrint(", "NSLog(", "dump(", "os_log("] {
                #expect(!text.contains(banned), "\(file) calls \(banned)")
            }
        }
    }

    @Test func theScannerFindsInterpolations() {
        let calls = Self.logCalls(#"x; Log.client.info("a \(line) b \(n.count)") ; y"#)
        #expect(calls == [#"("a \(line) b \(n.count)")"#])
        #expect(Self.interpolations(calls[0]) == ["line", "n.count"])
    }
}
