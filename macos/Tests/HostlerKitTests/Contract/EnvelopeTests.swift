import Foundation
import Testing
import HostlerKit

/// The CLI's `--json` answer, `{ok, data, error}` (spec §6.3), generic over the generated types.
@Suite struct EnvelopeTests {
    @Test func anAnswerCarriesItsData() throws {
        let envelope = try ContractFixtures.decode(Envelope<DoctorReport>.self, "envelope.DoctorReport.json")
        #expect(envelope.ok)
        #expect(envelope.error == nil)
        let report = try #require(envelope.data)
        #expect(report.checks.map(\.status) == [.ok, .problem])
        let claim = try ContractFixtures.decode(Envelope<Claim>.self, "envelope.Claim.json")
        #expect(claim.data?.id == "c1")
    }

    @Test func aRefusalCarriesTheServiceErrorAndNoData() throws {
        let envelope = try ContractFixtures.decode(Envelope<Claim>.self, "envelope.error.json")
        #expect(!envelope.ok)
        #expect(envelope.data == nil)
        let error = try #require(envelope.error)
        #expect(error.code == .forceRequired)
        #expect(error.details["token"] == .string("screen"))
    }

    /// As Rust writes it: `data` and `error` are always present, `null` when absent.
    @Test func absentPartsAreWrittenAsNull() throws {
        let refusal = Envelope<Claim>(ok: false, error: HostlerError(code: .internal, message: "x", details: .object([:])))
        let json = try jsonOf(refusal)
        #expect(json["data"] == .null)
        #expect(json["error"]?["code"] == .string("internal"))
        let answer = try jsonOf(Envelope<ProofGrant>(ok: true, data: ProofGrant(expiresAtMs: 1)))
        #expect(answer["error"] == .null)
        #expect(answer["data"]?["expires_at_ms"] == .int(1))
    }
}
