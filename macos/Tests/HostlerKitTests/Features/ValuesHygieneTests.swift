import Foundation
import Testing

/// A grep test over Task 9's sources (Review Focus 2): the values code never prints, logs only
/// fixed texts and error codes, never turns a value into a `String` outside the reveal's view,
/// shows that value hidden from the accessibility tree, and takes typed values only through
/// `SecureField`s.
@Suite struct ValuesHygieneTests {
    static let macos = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent()

    static let files = [
        "Sources/HostlerKit/Features/PersonGate.swift",
        "Sources/HostlerKit/Features/PersonGate+SecretEntry.swift",
        "Sources/HostlerKit/Features/UnlockController.swift",
        "Sources/HostlerKit/Features/RequestsController.swift",
        "Sources/HostlerKit/Features/RevealController.swift",
        "Sources/HostlerKit/Services/Pasteboard.swift",
        "Sources/HostlerKit/Services/LoginItem.swift",
        "Sources/HostlerKit/Services/OpenPanel.swift",
        "Sources/Hostler/App/UserNotifier.swift",
        "Sources/Hostler/UI/MenuBar/PopoverView.swift",
        "Sources/Hostler/UI/MenuBar/PopoverRows.swift",
        "Sources/Hostler/UI/MenuBar/PopoverPresentation.swift",
        "Sources/Hostler/UI/Values/UnlockView.swift",
        "Sources/Hostler/UI/Values/RequestCardView.swift",
        "Sources/Hostler/UI/Values/PersonGateSheet.swift",
        "Sources/Hostler/UI/Values/RevealView.swift",
        "Sources/Hostler/UI/Settings/SettingsView.swift",
    ]

    /// What a `Log.…` line in these files may interpolate: the fixed event and an error code.
    static let allowedInterpolations: Set<String> = [
        "text.description, privacy: .public", #"code?.rawValue ?? "-", privacy: .public"#,
    ]

    static func read(_ file: String) throws -> String {
        try String(contentsOf: macos.appending(path: file), encoding: .utf8)
    }

    @Test func nothingPrints() throws {
        for file in Self.files {
            let text = try Self.read(file)
            for banned in ["print(", "debugPrint(", "NSLog(", "dump(", "os_log("] {
                #expect(!text.contains(banned), "\(file) calls \(banned)")
            }
        }
    }

    @Test func logLinesInterpolateOnlyTheEventAndTheCode() throws {
        var calls = 0
        for file in Self.files {
            for call in NoFrameInLogsTests.logCalls(try Self.read(file)) {
                calls += 1
                for value in NoFrameInLogsTests.interpolations(call) {
                    #expect(Self.allowedInterpolations.contains(value), "\(file): logs \\(\(value))")
                }
            }
        }
        #expect(calls >= 3)
    }

    @Test func aValueBecomesAStringOnlyInTheRevealsView() throws {
        for file in Self.files where !file.hasSuffix("RevealView.swift") {
            let text = try Self.read(file)
            #expect(!text.contains(".reveal()"), "\(file) turns a SecretText into a String")
        }
        let reveal = try Self.read("Sources/Hostler/UI/Values/RevealView.swift")
        #expect(reveal.components(separatedBy: ".reveal()").count == 2)
    }

    @Test func theRevealedTextIsHiddenFromAccessibilityAndMarkedPrivate() throws {
        let reveal = try Self.read("Sources/Hostler/UI/Values/RevealView.swift")
        let shown = try #require(reveal.range(of: "Text(verbatim: field.value.reveal())"))
        let modifiers = reveal[shown.upperBound...].prefix(400)
        #expect(modifiers.contains(".accessibilityHidden(true)"))
        #expect(modifiers.contains(".privacySensitive()"))
        #expect(modifiers.contains(".textSelection(.disabled)"))
        #expect(reveal.contains("window.sharingType = .none"))
    }

    @Test func typedValuesGoThroughSecureFieldsOnly() throws {
        for file in Self.files where file.contains("/UI/") {
            let text = try Self.read(file)
            #expect(!text.contains("TextField("), "\(file) has a plain TextField")
        }
        let rows = try Self.read("Sources/Hostler/UI/MenuBar/PopoverRows.swift")
        #expect(rows.contains("SecureField(title, text: $entry.text)"))
    }

    /// Task 13 re-run G-2: the person gate's title (its sheet's heading) is always localized.
    @Test func everyGateTitleIsLocalized() throws {
        let features = Self.macos.appending(path: "Sources/HostlerKit/Features")
        var calls = 0
        for name in try FileManager.default.contentsOfDirectory(atPath: features.path)
        where name.hasSuffix(".swift") {
            let text = try String(contentsOf: features.appending(path: name), encoding: .utf8)
            var rest = text[...]
            while let found = rest.range(of: "gate.perform(") {
                calls += 1
                let after = rest[found.upperBound...]
                #expect(after.hasPrefix("String(localized: "), "\(name): \(after.prefix(60))")
                rest = after
            }
        }
        #expect(calls >= 6)
    }
}
