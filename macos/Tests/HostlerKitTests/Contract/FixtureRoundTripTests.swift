import Foundation
import Testing
import HostlerKit

/// Every Rust-written fixture decodes into its generated type and encodes back to the same JSON
/// (compared as `JSONValue`, so key order does not matter).
@Suite struct FixtureRoundTripTests {
    @Test func theFolderHoldsExactlyTheIndexedFixtures() throws {
        let files = try FileManager.default
            .contentsOfDirectory(atPath: ContractFixtures.directory.path)
            .filter { $0.hasSuffix(".json") }
            .sorted()
        #expect(files == contractFixtureNames.sorted())
        #expect(files.filter { $0.hasPrefix("request.") }.count == HostlerMethod.allCases.count)
        #expect(files.filter { $0.hasPrefix("error.") }.count == 40)
    }

    @Test(arguments: contractFixtureNames)
    func everyFixtureRoundTrips(_ name: String) throws {
        let fixture = try ContractFixtures.json(name)
        if name.hasPrefix("request.") {
            try requestRoundTrips(name, fixture)
        } else if name.hasPrefix("event.") {
            try eventFrameRoundTrips(fixture)
        } else if name.hasPrefix("frame.") {
            try frameRoundTrips(name, fixture)
        } else {
            let data = try ContractFixtures.data(name)
            let again = try #require(try reencodeContractFixture(name, data), "\(name) has no type")
            #expect(try JSONValue(json: again) == fixture)
        }
    }

    /// `request.<method>.json` is a client frame: the id, then the request's own keys.
    private func requestRoundTrips(_ name: String, _ frame: JSONValue) throws {
        guard case .int = frame["id"] else {
            Issue.record("\(name) has no id")
            return
        }
        let wire = frame.removing("id")
        let request = try JSONDecoder().decode(Request.self, from: wire.encoded())
        let method = String(name.dropFirst("request.".count).dropLast(".json".count))
        #expect(request.method.rawValue == method)
        #expect(try jsonOf(request) == wire)
    }

    private func eventFrameRoundTrips(_ frame: JSONValue) throws {
        guard case .int = frame["seq"] else {
            Issue.record("an event frame without seq")
            return
        }
        let event = try #require(frame["event"])
        let decoded = try JSONDecoder().decode(Event.self, from: event.encoded())
        #expect(try jsonOf(decoded) == event)
    }

    private func frameRoundTrips(_ name: String, _ frame: JSONValue) throws {
        switch name {
        case "frame.response.json":
            #expect(frame["id"] == .int(7))
            let result = try #require(frame["result"])
            let snapshot = try JSONDecoder().decode(QueueSnapshot.self, from: result.encoded())
            #expect(try jsonOf(snapshot) == result)
        case "frame.error.json":
            #expect(frame["id"] == .int(8))
            let error = try JSONDecoder().decode(HostlerError.self, from: #require(frame["error"]).encoded())
            #expect(error.code == .proofRequired)
            #expect(error.recovery != nil)
        case "frame.event.json":
            #expect(frame["seq"] == .int(9))
            let event = try JSONDecoder().decode(Event.self, from: #require(frame["event"]).encoded())
            #expect(event == .resync(reason: .lagged))
        default:
            Issue.record("an unknown frame \(name)")
        }
    }

    @Test func aMaterializedSecretHoldsItsValueInSecretText() throws {
        let secret = try ContractFixtures.decode(MaterializedSecret.self, "answer.MaterializedSecret.json")
        #expect(secret.name == "API_KEY")
        #expect(secret.value == SecretText("fixture-value-2"))
        #expect(secret.value.reveal() == "fixture-value-2")
    }

    @Test func unknownTagsAreRefusedWithoutQuotingTheInput() throws {
        let frame = Data(#"{"method":"no.such-method-PAYLOAD","params":{}}"#.utf8)
        do {
            _ = try JSONDecoder().decode(Request.self, from: frame)
            Issue.record("an unknown method decoded")
        } catch {
            #expect(!"\(error)".contains("PAYLOAD"))
        }
        let reading = Data(#"{"secret-PAYLOAD":1}"#.utf8)
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(ReadingUInt8.self, from: reading)
        }
    }
}
