import Foundation
import Testing
import HostlerKit

@Suite struct ContractVersionTests {
    @Test func theContractIsVersion5() {
        #expect(Contract.version == 5)
    }

    @Test func framesAreAtMost8MiB() {
        #expect(Contract.maxFrameBytes == 8 << 20)
    }

    @Test func theHelloFixtureSpeaksThisVersion() throws {
        let request = try JSONDecoder().decode(
            Request.self, from: ContractFixtures.json("request.hello.json").removing("id").encoded())
        #expect(request == .hello(contractVersion: Contract.version, client: "cli"))
    }
}

@Suite struct JSONValueTests {
    private func read(_ text: String) throws -> JSONValue {
        try JSONValue(json: Data(text.utf8))
    }

    @Test func numbersBooleansAndNullStayApart() throws {
        #expect(try read("1") == .int(1))
        #expect(try read("-3") == .int(-3))
        #expect(try read("18446744073709551615") == .uint(.max))
        #expect(try read("1.5") == .double(1.5))
        #expect(try read("true") == .bool(true))
        #expect(try read("null") == .null)
        #expect(try read(#""1""#) == .string("1"))
    }

    @Test func objectsIgnoreKeyOrderAndReencode() throws {
        let a = try read(#"{"a":1,"b":[true,null,"x"],"c":{"d":2}}"#)
        let b = try read(#"{"c":{"d":2},"b":[true,null,"x"],"a":1}"#)
        #expect(a == b)
        #expect(try JSONValue(json: a.encoded()) == a)
        #expect(a["c"]?["d"] == .int(2))
        #expect(a["missing"] == nil)
    }
}
