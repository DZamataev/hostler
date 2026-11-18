import Foundation
import OSLog
import Testing
import HostlerKit

@Suite struct SecretTextTests {
    let value = "s3cr3t-fixture-value"

    @Test func noTextShowsTheValue() {
        let secret = SecretText(value)
        var dumped = ""
        dump(secret, to: &dumped)
        for text in [
            "\(secret)", String(describing: secret), String(reflecting: secret),
            secret.description, secret.debugDescription, dumped,
        ] {
            #expect(!text.contains(value), "\(text)")
        }
        #expect("\(secret)" == "<redacted>")
        #expect(String(reflecting: secret) == "<redacted>")
    }

    @Test func aGeneratedTypeHoldingOneShowsNoValue() throws {
        let secret = MaterializedSecret(name: "API_KEY", value: SecretText(value))
        let account = MaterializedAccount(name: "qa", fields: [secret])
        let request = Request.secretSet(SecretSet(project: ProjectRef(commonDir: "/w/.git", name: "w"), name: "API_KEY", value: SecretText(value)))
        for subject in [String(describing: secret), String(reflecting: account), "\(request)"] {
            #expect(!subject.contains(value), "\(subject)")
            #expect(subject.contains("<redacted>"))
        }
        var dumped = ""
        dump(account, to: &dumped)
        #expect(!dumped.contains(value))
    }

    @Test func aLoggerInterpolationShowsNoValue() throws {
        let secret = SecretText(value)
        let marker = "secret-text-test-\(UUID().uuidString)"
        let logger = Logger(subsystem: "io.github.dzamataev.hostler.tests", category: "secret-text")
        // `.public`, so the test sees what a log stream with private data enabled would show.
        logger.log("\(marker, privacy: .public) \(secret, privacy: .public)")
        let store = try OSLogStore(scope: .currentProcessIdentifier)
        let since = store.position(date: Date().addingTimeInterval(-60))
        let message = try store.getEntries(at: since)
            .compactMap { $0 as? OSLogEntryLog }
            .map(\.composedMessage)
            .first { $0.contains(marker) }
        let logged = try #require(message, "the log entry was not found")
        #expect(logged == "\(marker) <redacted>")
    }

    @Test func itKeepsTheBytesAndRevealsThemForAView() {
        let secret = SecretText(value)
        #expect(secret.byteCount == value.utf8.count)
        #expect(secret.reveal() == value)
        #expect(secret.withBytes { Array($0) } == Array(value.utf8))
        #expect(SecretText(Data(value.utf8)) == secret)
        #expect(SecretText("").byteCount == 0)
        #expect(SecretText("").reveal() == "")
        #expect(SecretText("ключ").byteCount == 8)
    }

    @Test func equalityComparesTheBytes() {
        #expect(SecretText("abcd") == SecretText("abcd"))
        #expect(SecretText("abcd") != SecretText("abce"))
        #expect(SecretText("abcd") != SecretText("abcde"))
        #expect(SecretText("") == SecretText(""))
    }

    @Test func onTheWireItIsAJSONString() throws {
        let data = try JSONEncoder().encode([SecretText("a\"b")])
        #expect(String(decoding: data, as: UTF8.self) == #"["a\"b"]"#)
        let back = try JSONDecoder().decode([SecretText].self, from: data)
        #expect(back == [SecretText("a\"b")])
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode([SecretText].self, from: Data("[1]".utf8))
        }
    }

    @Test func itCrossesIntoATask() async {
        let secret = SecretText(value)
        let length = await Task.detached { secret.byteCount }.value
        #expect(length == value.utf8.count)
    }
}
