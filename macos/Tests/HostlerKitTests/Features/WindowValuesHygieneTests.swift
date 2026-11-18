import Foundation
import Testing

/// A grep test over Task 11's views (Review Focus 2, spec §9.6): no value can be printed,
/// turned into a `String` outside Task 9's `RevealView`, or typed anywhere but a `SecureField`;
/// every revealed value shows through `RevealView` with its subject, so it is hidden from the
/// accessibility tree and from screen sharing by that one component.
@Suite struct WindowValuesHygieneTests {
    static let macos = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent()

    static let files = [
        "Sources/Hostler/UI/Window/Queue/QueueView.swift",
        "Sources/Hostler/UI/Window/Screen/ScreenView.swift",
        "Sources/Hostler/UI/Window/Accounts/AccountsView.swift",
        "Sources/Hostler/UI/Window/Secrets/SecretsView.swift",
        "Sources/Hostler/UI/Window/Secrets/SetValueSheet.swift",
        "Sources/Hostler/UI/Window/Projects/ProjectsView.swift",
        "Sources/Hostler/UI/Window/MainWindow.swift",
    ]

    static func read(_ file: String) throws -> String {
        try String(contentsOf: macos.appending(path: file), encoding: .utf8)
    }

    @Test func nothingPrints() throws {
        for file in Self.files {
            let text = try Self.read(file)
            for banned in ["print(", "debugPrint(", "NSLog(", "dump(", "os_log(", "Logger("] {
                #expect(!text.contains(banned), "\(file) calls \(banned)")
            }
        }
    }

    @Test func aValueIsNeverAStringHere() throws {
        for file in Self.files {
            let text = try Self.read(file)
            #expect(!text.contains(".reveal()"), "\(file) turns a SecretText into a String")
            // The actions take a `SecretEntry`, never a value built from a `String`.
            #expect(!text.contains("SecretText("), "\(file) builds a SecretText")
        }
    }

    @Test func valuesAreTypedOnlyIntoSecretEntryFields() throws {
        let sheet = try Self.read("Sources/Hostler/UI/Window/Secrets/SetValueSheet.swift")
        #expect(sheet.contains("SecretEntryField("))
        // The only `TextField` is the field *name* of an account that declares none (a name is
        // not a value); the value never goes through one.
        #expect(!sheet.contains("$entry.text"))
        for file in Self.files where file.contains("/Secrets/") || file.contains("/Accounts/") {
            let text = try Self.read(file)
            #expect(!text.contains("$entry.text"), "\(file) binds a value field directly")
        }
    }

    @Test func everyRevealGoesThroughTheRevealsView() throws {
        var reveals = 0
        for file in Self.files {
            let text = try Self.read(file)
            for line in text.split(separator: "\n") where line.contains("RevealView(") {
                reveals += 1
            }
            // Whatever is shown is a subject, which carries no value: the controller holds the
            // values until the view draws them.
            #expect(!text.contains("shown.fields"), "\(file) reads the revealed fields itself")
        }
        #expect(reveals == 2)   // the account's rows and the secret's rows
    }

    /// A view may hold the field a person types into (`SecretEntry`, cleared on every outcome);
    /// it never holds a value (`SecretText`) or a revealed one.
    @Test func noViewKeepsAValueInItsState() throws {
        for file in Self.files {
            let text = try Self.read(file)
            for line in text.split(separator: "\n") where line.contains("@State") {
                for value in ["SecretText", "RevealedValue", "RevealedField"] {
                    #expect(!line.contains(value), "\(file): \(line)")
                }
            }
        }
    }
}
