import Foundation
import Testing
@testable import HostlerKit

/// The request cards (spec §8.2, §8.4, §9.2; plan Task 9): a secret's Store is `secret.set` with
/// that request's project and name; an account's Store sends every field in one gated
/// `account.answer`; Decline; the fields are cleared on every outcome.
@Suite(.timeLimit(.minutes(1))) @MainActor struct RequestsControllerTests {
    @MainActor struct Fixture {
        let gate = GateFixture()
        let requests: RequestsController
        var transport: FakeValuesTransport { gate.transport }

        init() {
            requests = RequestsController(client: gate.client, gate: gate.gate, log: gate.log)
        }

        /// The project `p-1` ("app") with its declarations, as the projects snapshot has it.
        static let projects = ProjectsSnapshot(
            projects: [ProjectView(
                info: ProjectInfo(id: "p-1", name: "app", commonDir: "/work/app/.git",
                                  approved: true, createdAtMs: 1),
                workspaces: [],
                declared: ProjectDecl(
                    secrets: [DeclaredSecret(name: "API_KEY", purpose: "the shop's API")],
                    accounts: [DeclaredAccount(name: "login", purpose: "the test user",
                                               fields: ["user", "password"])],
                    apps: DeclaredApps(android: [], ios: [])),
                declaredAtMs: 1)],
            atMs: 1, asOfSeq: 1)

        static func model(secrets: [SecretRequestInfo] = [], accounts: [AccountRequestInfo] = [],
                          projects: ProjectsSnapshot? = Fixture.projects) -> StoreModel {
            var model = StoreModel()
            model.secretRequests = secrets
            model.accountRequests = accounts
            model.projects = projects
            return model
        }
    }

    static let secret = FakeValuesTransport.secretRequest(id: "s-1", createdAtMs: 2_000)
    static let account = FakeValuesTransport.accountRequest(id: "a-1", createdAtMs: 3_000)

    @Test func cardsArePendingRequestsNewestFirstWithTheDeclaredPurpose() {
        let older = FakeValuesTransport.secretRequest(id: "s-0", name: "OLD", createdAtMs: 500)
        let done = FakeValuesTransport.secretRequest(id: "s-9", state: .stored, createdAtMs: 9_000)
        let unknown = FakeValuesTransport.secretRequest(id: "s-2", project: "p-404",
                                                        name: "OTHER", createdAtMs: 2_500)
        let cards = RequestsController.cards(
            Fixture.model(secrets: [older, Self.secret, done, unknown], accounts: [Self.account]))
        #expect(cards.map(\.id) == ["a-1", "s-2", "s-1", "s-0"])
        #expect(cards[0].fields == ["user", "password"])
        #expect(cards[0].purpose == "the test user")
        #expect(cards[0].isAccount)
        #expect(cards[2].fields == ["value"])
        #expect(cards[2].purpose == "the shop's API")
        #expect(cards[2].reason == "the tests need it")
        #expect(cards[2].requesterPid == 4242)
        #expect(cards[1].purpose == nil)
    }

    @Test func aSecretStoreSetsThatRequestsProjectAndNameAndClearsTheField() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(secrets: [Self.secret])).first)
        f.requests.entry(card, field: "value").text = "sk-live-123"
        let ok = await f.requests.store(card, projects: Fixture.projects)
        #expect(ok)
        #expect(f.requests.entry(card, field: "value").isEmpty)
        let calls = await f.transport.calls
        #expect(calls.count == 1)
        guard case .secretSet(let set) = calls.first else { Issue.record("\(calls)"); return }
        #expect(set.project == ProjectRef(commonDir: "/work/app/.git", name: "app"))
        #expect(set.name == "API_KEY")
        #expect(set.value == SecretText("sk-live-123"))
        #expect(f.requests.errors["s-1"] == nil)
    }

    @Test func anEmptyValueOrAnUnknownProjectSendsNothing() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(secrets: [Self.secret])).first)
        #expect(await f.requests.store(card, projects: Fixture.projects) == false)
        #expect(f.requests.errors["s-1"] == .emptyValue(field: "API_KEY"))
        f.requests.entry(card, field: "value").text = "sk"
        #expect(await f.requests.store(card, projects: nil) == false)
        #expect(f.requests.errors["s-1"] == .projectUnknown(name: "app"))
        #expect(f.requests.entry(card, field: "value").isEmpty)
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func aFailedStoreShowsTheErrorAndClearsTheField() async throws {
        let f = Fixture()
        let locked = HostlerError(code: .secretsLocked, message: "the secrets database is locked",
                                  details: .object([:]), recovery: "unlock it in the hostler app")
        await f.transport.queue(.service(locked), for: .secretSet)
        let card = try #require(RequestsController.cards(Fixture.model(secrets: [Self.secret])).first)
        f.requests.entry(card, field: "value").text = "sk"
        #expect(await f.requests.store(card, projects: Fixture.projects) == false)
        #expect(f.requests.errors["s-1"] == .service(
            message: "The secrets database is locked. Unlock it in hostler's menu bar panel.",
            recovery: nil))
        #expect(f.requests.entry(card, field: "value").isEmpty)
    }

    @Test func anAccountStoreSendsEveryFieldInOneGatedAnswer() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(accounts: [Self.account])).first)
        f.requests.entry(card, field: "user").text = "qa@example.com"
        f.requests.entry(card, field: "password").text = "pa55"
        async let stored = f.requests.store(card, projects: Fixture.projects)
        try await f.gate.answer("correct horse")
        #expect(await stored)
        let calls = await f.transport.calls
        #expect(calls.map(\.method) == [.secretsProve, .accountAnswer])
        guard case .accountAnswer(let answer) = calls.last else { Issue.record("\(calls)"); return }
        #expect(answer.requestId == "a-1")
        #expect(answer.fields.map(\.name) == ["user", "password"])
        #expect(answer.fields.map(\.value) == [SecretText("qa@example.com"), SecretText("pa55")])
        #expect(f.requests.entry(card, field: "user").isEmpty)
        #expect(f.requests.entry(card, field: "password").isEmpty)
    }

    @Test func anAccountStoreNeedsEveryField() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(accounts: [Self.account])).first)
        f.requests.entry(card, field: "user").text = "qa@example.com"
        #expect(f.requests.canStore(card) == false)
        #expect(await f.requests.store(card, projects: Fixture.projects) == false)
        #expect(f.requests.errors["a-1"] == .emptyValue(field: "password"))
        #expect(f.requests.entry(card, field: "user").isEmpty)
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func aCancelledGateStoresNothingAndClearsTheFields() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(accounts: [Self.account])).first)
        f.requests.entry(card, field: "user").text = "u"
        f.requests.entry(card, field: "password").text = "p"
        #expect(f.requests.canStore(card))
        async let stored = f.requests.store(card, projects: Fixture.projects)
        try await storeSoon("the gate asks") { f.gate.gate.phase == .asking }
        #expect(f.requests.entry(card, field: "user").isEmpty)
        f.gate.gate.cancel()
        #expect(await stored == false)
        #expect(f.requests.errors["a-1"] == nil)
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func declineASecretIsNotGated() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(secrets: [Self.secret])).first)
        f.requests.entry(card, field: "value").text = "typed then declined"
        #expect(await f.requests.decline(card))
        let calls = await f.transport.calls
        guard case .secretDecline(let decline) = calls.first else { Issue.record("\(calls)"); return }
        #expect(decline == Decline(id: "s-1"))
        #expect(calls.count == 1)
        #expect(f.requests.entry(card, field: "value").isEmpty)
    }

    @Test func declineAnAccountGoesThroughTheGate() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(accounts: [Self.account])).first)
        async let declined = f.requests.decline(card)
        try await f.gate.answer("correct horse")
        #expect(await declined)
        #expect(await f.transport.methods == [.secretsProve, .accountDecline])
    }

    @Test func pruneDropsTheFieldsOfRequestsNoLongerPending() throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(secrets: [Self.secret])).first)
        let entry = f.requests.entry(card, field: "value")
        entry.text = "typed"
        f.requests.prune(keeping: [])
        #expect(entry.isEmpty)
        #expect(f.requests.entry(card, field: "value") !== entry)
    }

    @Test func clearAllEmptiesEveryField() throws {
        let f = Fixture()
        let cards = RequestsController.cards(Fixture.model(secrets: [Self.secret],
                                                           accounts: [Self.account]))
        for card in cards { for field in card.fields { f.requests.entry(card, field: field).text = "x" } }
        f.requests.clearAll()
        for card in cards { for field in card.fields { #expect(f.requests.entry(card, field: field).isEmpty) } }
    }

    @Test func nothingTypedReachesTheLog() async throws {
        let f = Fixture()
        let card = try #require(RequestsController.cards(Fixture.model(secrets: [Self.secret])).first)
        f.requests.entry(card, field: "value").text = "value-marker-9001"
        _ = await f.requests.store(card, projects: Fixture.projects)
        #expect(!f.gate.log.lines.isEmpty)
        #expect(f.gate.log.lines.allSatisfy { !$0.contains("marker-9001") })
    }
}
