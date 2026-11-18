import Foundation
import Testing
import HostlerKit

@Suite struct RequestEncodingTests {
    @Test func aUnitRequestHasNoParams() throws {
        #expect(try jsonOf(Request.serviceStatus) == .object(["method": .string("service.status")]))
        #expect(try jsonOf(Request.requestsSnapshot) == .object(["method": .string("requests.snapshot")]))
    }

    /// `refuse_if_unlocked` is sent only when true, exactly as Rust skips `false`.
    @Test func shutdownWithoutRefuseIfUnlockedIsTheRustFrame() throws {
        let rust = try ContractFixtures.json("request.service.shutdown.json").removing("id")
        let plain = Request.serviceShutdown(ifIdle: true, refuseIfUnlocked: false)
        #expect(try jsonOf(plain) == rust)
        #expect(try jsonOf(plain) == .object([
            "method": .string("service.shutdown"),
            "params": .object(["if_idle": .bool(true)]),
        ]))
        let refusing = Request.serviceShutdown(ifIdle: true, refuseIfUnlocked: true)
        #expect(try jsonOf(refusing)["params"]?["refuse_if_unlocked"] == .bool(true))
        // A frame without the key reads `false`.
        let read = try JSONDecoder().decode(Request.self, from: rust.encoded())
        #expect(read == plain)
    }

    /// An `Option` the service always writes is sent as `null`; one it may skip is left out.
    @Test func optionalsFollowRustsSerialization() throws {
        let remove = Request.accountRemove(AccountRemove(project: ProjectRef(commonDir: "/w/.git", name: "w"), name: "qa", field: nil))
        #expect(try jsonOf(remove)["params"]?["field"] == .null)
        let status = ServiceStatus(serviceVersion: "0.0.4", contractVersion: 5, pid: 1, runningHeavy: 0, waitingHeavy: 0)
        let json = try jsonOf(status)
        #expect(json["exe"] == nil)
        #expect(json["instance"] == nil)
    }

    @Test func theMethodTableIsRusts() {
        #expect(HostlerMethod.allCases.count == 76)
        #expect(HostlerMethod.secretsUnlock.spec.timeout == .milliseconds(120_000))
        #expect(HostlerMethod.devicesShow.spec.timeout == .milliseconds(10_000))
        #expect(HostlerMethod.queueSnapshot.spec.timeout == .seconds(5))
        let gated = HostlerMethod.allCases.filter { $0.spec.gated }
        #expect(gated == [.secretReveal, .accountReveal, .secretsRemovePreserved, .accountDecline, .accountAnswer, .heavyStop])
        let valueAnswers = HostlerMethod.allCases.filter { $0.spec.sensitiveAnswer }
        #expect(valueAnswers == [.secretMaterialize, .accountMaterialize, .secretReveal, .accountReveal])
        #expect(!HostlerMethod.secretMaterialize.spec.app)
        #expect(!HostlerMethod.accountMaterialize.spec.app)
        #expect(!HostlerMethod.serviceShutdown.spec.app)
        #expect(HostlerMethod.secretsProve.spec.sensitiveRequest)
    }

    @Test func everyRequestNamesItsMethod() throws {
        #expect(Request.secretReveal(project: ProjectRef(commonDir: "/w/.git", name: "w"), name: "API_KEY").method == .secretReveal)
        #expect(Request.hello(contractVersion: Contract.version, client: "app").method == .hello)
    }
}
