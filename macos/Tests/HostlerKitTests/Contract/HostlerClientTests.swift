import Foundation
import Testing
import HostlerKit

/// A transport that records what the facade sends and answers from a table.
private actor FakeTransport: HostlerTransport {
    struct Sent: Equatable {
        let request: Request
        let timeout: Duration
        let sensitive: Bool
    }

    private(set) var sent: [Sent] = []
    private let answers: [HostlerMethod: Result<Data, HostlerCallError>]

    init(_ answers: [HostlerMethod: Result<Data, HostlerCallError>]) {
        self.answers = answers
    }

    private func answer(_ request: Request) throws(HostlerCallError) -> Data {
        guard let answer = answers[request.method] else { throw .notConnected }
        return try answer.get()
    }

    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        sent.append(Sent(request: request, timeout: timeout, sensitive: false))
        return try answer(request)
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> SensitiveData {
        sent.append(Sent(request: request, timeout: timeout, sensitive: true))
        return SensitiveData(SecretText(try answer(request)))
    }
}

@Suite struct HostlerClientTests {
    private let project = ProjectRef(commonDir: "/work/app/.git", name: "app")

    @Test func aCallSendsTheRequestWithTheTablesBoundAndDecodesTheAnswer() async throws {
        let fake = FakeTransport([.devicesList: .success(try ContractFixtures.data("answer.DevicesSnapshot.json"))])
        let snapshot = try await HostlerClient(transport: fake).devicesList(all: true)
        #expect(snapshot == (try ContractFixtures.decode(DevicesSnapshot.self, "answer.DevicesSnapshot.json")))
        #expect(await fake.sent == [.init(request: .devicesList(all: true), timeout: .seconds(5), sensitive: false)])
    }

    @Test func revealGoesThroughTheSensitivePath() async throws {
        let fake = FakeTransport([.secretReveal: .success(try ContractFixtures.data("answer.MaterializedSecret.json"))])
        let secret = try await HostlerClient(transport: fake).secretReveal(project: project, name: "API_KEY")
        #expect(secret.value == SecretText("fixture-value-2"))
        #expect(await fake.sent == [.init(request: .secretReveal(project: project, name: "API_KEY"), timeout: .seconds(60), sensitive: true)])
    }

    @Test func aUnitAnswerReturnsNothing() async throws {
        let fake = FakeTransport([.queueSetCapacity: .success(Data("null".utf8))])
        try await HostlerClient(transport: fake).queueSetCapacity(capacity: 2)
        #expect(await fake.sent.map(\.request) == [.queueSetCapacity(capacity: 2)])
    }

    @Test func aServiceErrorPassesThrough() async throws {
        let error = try ContractFixtures.decode(HostlerError.self, "error.proof_required.json")
        let fake = FakeTransport([.heavyStop: .failure(.service(error))])
        await #expect(throws: HostlerCallError.service(error)) {
            _ = try await HostlerClient(transport: fake).heavyStop(id: "h1", confirmation: "h1")
        }
    }

    @Test func anAnswerOfTheWrongShapeIsUndecodable() async throws {
        let fake = FakeTransport([
            .queueSnapshot: .success(Data(#"{"capacity":"not-a-number"}"#.utf8)),
            .accountReveal: .success(Data("[]".utf8)),
        ])
        let client = HostlerClient(transport: fake)
        await #expect(throws: HostlerCallError.undecodable(.queueSnapshot)) {
            _ = try await client.queueSnapshot()
        }
        await #expect(throws: HostlerCallError.undecodable(.accountReveal)) {
            _ = try await client.accountReveal(project: project, name: "qa")
        }
    }

    @Test func sensitiveDataShowsNoBytes() {
        let raw = SensitiveData(SecretText(#"{"name":"A","value":"fixture-value-9"}"#))
        #expect("\(raw)" == "<redacted>")
        #expect(!String(reflecting: raw).contains("fixture-value-9"))
    }
}
