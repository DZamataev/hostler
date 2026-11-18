import Foundation
import Testing

/// The app's string catalog (spec §9.5: the interface is English and Russian, following the
/// system; the CLI's output and the contract stay English). This test keeps the catalog honest:
/// every key the interface can show has a Russian value, the value is not an empty string and
/// not the English text left in by mistake, and every placeholder of the key survives.
///
/// A second test reads the sources: a simple key passed to `String(localized:)` must be in the
/// catalog, and the only `Text(verbatim:)` is the reveal's value, which is never translated.
@Suite struct LocalizationTests {
    static let macos = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent()

    static let catalogURL = macos.appending(path: "Sources/Hostler/Resources/Localizable.xcstrings")

    /// The tokens that stay as they are in Russian: names, protocols, and templates that hold
    /// no words at all.
    static let technical: Set<String> = [
        "Android", "Finder", "OK", "VPN", "Wi-Fi", "agent-device", "hostler", "iOS", "usb", "—",
        // Templates whose only words are names and contract-neutral tokens.
        "%lld", "agent-device (%@)", "Doctor — %@", "hostler doctor — %@", "hostler, %@",
        "%@ · pid %@ · %@", "id %@",
    ]

    struct Catalog {
        var keys: [String: (english: String, russian: String?)]
    }

    static func catalog() throws -> Catalog {
        let data = try Data(contentsOf: catalogURL)
        let root = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(root["sourceLanguage"] as? String == "en")
        let strings = try #require(root["strings"] as? [String: Any])
        var keys: [String: (english: String, russian: String?)] = [:]
        for (key, value) in strings {
            let entry = try #require(value as? [String: Any], "\(key)")
            let localizations = try #require(entry["localizations"] as? [String: Any], "\(key)")
            func text(_ language: String) -> String? {
                guard let unit = (localizations[language] as? [String: Any])?[
                    "stringUnit"] as? [String: Any] else { return nil }
                return unit["value"] as? String
            }
            keys[key] = (text("en") ?? key, text("ru"))
        }
        return Catalog(keys: keys)
    }

    static func placeholders(_ text: String) -> [String] {
        let regex = try! NSRegularExpression(pattern: "%(?:\\d+\\$)?[@a-zA-Z%]")
        let range = NSRange(text.startIndex..., in: text)
        return regex.matches(in: text, range: range).compactMap { match in
            Range(match.range, in: text).map { String(text[$0]) }
        }.sorted()
    }

    static func hasWords(_ text: String) -> Bool {
        var rest = text
        for placeholder in placeholders(text) { rest = rest.replacingOccurrences(
            of: placeholder, with: "") }
        return rest.contains { $0.isLetter || $0.isNumber }
    }

    @Test func theCatalogIsSubstantial() throws {
        let catalog = try Self.catalog()
        #expect(catalog.keys.count >= 400, "only \(catalog.keys.count) keys")
    }

    @Test func everyKeyHasRussianText() throws {
        for (key, entry) in try Self.catalog().keys {
            let russian = entry.russian ?? ""
            #expect(!russian.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                    "no Russian for \(key.debugDescription)")
        }
    }

    @Test func noRussianValueIsTheEnglishOneByMistake() throws {
        for (key, entry) in try Self.catalog().keys {
            guard let russian = entry.russian else { continue }
            guard russian == entry.english else { continue }
            if Self.technical.contains(key) || !Self.hasWords(key) { continue }
            Issue.record("the Russian text of \(key.debugDescription) is still English")
        }
    }

    @Test func everyPlaceholderSurvivesTranslation() throws {
        for (key, entry) in try Self.catalog().keys {
            guard let russian = entry.russian else { continue }
            #expect(Self.placeholders(key) == Self.placeholders(russian),
                    "the placeholders of \(key.debugDescription) changed in \(russian.debugDescription)")
        }
    }

    /// A plain key passed to `String(localized:)` — one line, no interpolation, no escape — is
    /// a key the catalog must carry. (An interpolated one is checked by the build's extraction;
    /// this catches the plain text someone adds without the catalog.)
    @Test func everyPlainKeyInTheSourcesIsInTheCatalog() throws {
        let keys = try Self.catalog().keys
        let regex = try NSRegularExpression(pattern: #"String\(localized:\s*"([^"\\\n]*)"\)"#)
        var checked = 0
        for url in try Self.sources() {
            let text = try String(contentsOf: url, encoding: .utf8)
            let range = NSRange(text.startIndex..., in: text)
            for match in regex.matches(in: text, range: range) {
                guard let keyRange = Range(match.range(at: 1), in: text) else { continue }
                let key = String(text[keyRange])
                checked += 1
                #expect(keys[key] != nil,
                        "\(url.lastPathComponent) has \(key.debugDescription) outside the catalog")
            }
        }
        #expect(checked >= 200, "only \(checked) plain keys found")
    }

    /// The reveal's value is drawn verbatim on purpose and is the only such text (spec §9.6).
    @Test func onlyTheRevealsValueIsVerbatim() throws {
        var files: [String] = []
        for url in try Self.sources() {
            if try String(contentsOf: url, encoding: .utf8).contains("Text(verbatim:") {
                files.append(url.lastPathComponent)
            }
        }
        #expect(files == ["RevealView.swift"], "\(files)")
    }

    static func sources() throws -> [URL] {
        let root = macos.appending(path: "Sources")
        let enumerator = try #require(FileManager.default.enumerator(
            at: root, includingPropertiesForKeys: nil))
        var urls: [URL] = []
        for case let url as URL in enumerator where url.pathExtension == "swift" {
            urls.append(url)
        }
        return urls.sorted { $0.path < $1.path }
    }
}
