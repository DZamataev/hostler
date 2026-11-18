import Foundation
import Testing
@testable import HostlerKit

/// Records the requests an action sends and answers them with contract values.
actor ActionTransport: HostlerTransport {
    private(set) var calls: [Request] = []
    var failures: [HostlerMethod: HostlerCallError] = [:]
    var device: DeviceInfo
    var claim: Claim

    init(device: DeviceInfo, claim: Claim) {
        self.device = device
        self.claim = claim
    }

    func fail(_ method: HostlerMethod, with error: HostlerCallError) { failures[method] = error }

    func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        calls.append(request)
        if let failure = failures[request.method] { throw failure }
        func encode<T: Encodable>(_ value: T) -> Data {
            (try? JSONEncoder().encode(value)) ?? Data()
        }
        switch request {
        case .devicesAdd, .devicesRename, .devicesNote:
            return encode(device)
        case .devicesRemove:
            return encode(DeviceRemoved(removed: device.resource))
        case .devicesForgetSessions:
            return encode(ForgetRecord(
                id: "f1", resource: device.resource, sessions: [], byWorkspacePath: nil,
                byPid: nil, reason: "the person asked", atMs: 1))
        case .claimsReserve, .claimsRenew:
            return encode(claim)
        default:
            return Data("null".utf8)
        }
    }

    func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        throw .notConnected
    }
}

/// The window's actions (plan Task 10): each request, the `person` flags the service needs, the
/// CLI argument lists and working directories, and the rule that nothing runs unless the
/// confirmation token matches.
@Suite struct DeviceActionsTests {
    let now: Int64 = Mk.now

    private func device(alias: String? = "pixel") -> DeviceInfo {
        DeviceInfo(resource: ResourceId(rawValue: "d1"), hardwareId: "HW-1", platform: .android,
                   kind: .physical, registered: alias != nil, alias: alias,
                   aliasByPerson: alias != nil, model: "Pixel 9", connected: true, ready: true,
                   transports: [], bootedByHostler: false, shuttingDown: false, removing: false,
                   sessions: [], checkingSessions: false, firstSeenMs: now, lastSeenMs: now)
    }

    private func claim() -> Claim {
        Claim(id: "c1", resource: Mk.resource("d1"), kind: .hold,
              workspace: WorkspaceRef(id: "w1", path: "/work/app"),
              binding: .ttl(ttlMs: 1_800_000, expiresAtMs: now + 1_800_000),
              command: "make device", grantedAtMs: now)
    }

    private func workspace() -> WorkspaceInfo {
        Mk.workspace("w1", project: "p1", path: "/work/app")
    }

    private func makeActions(_ transport: ActionTransport,
                             runner: FakeCommandRunner = FakeCommandRunner(),
                             cli: String? = "/bin/hostler") -> DeviceActions
    {
        DeviceActions(client: HostlerClient(transport: transport), runner: runner,
                      cliExecutable: cli, appPid: 99,
                      appPath: "/Applications/Hostler.app")
    }

    // MARK: The registry

    @Test func addSendsThePersonFlagAndTheAlias() async throws {
        let transport = ActionTransport(device: device(alias: "kitchen"), claim: claim())
        let actions = makeActions(transport)
        let failure = await actions.add(device(), alias: "  kitchen ")
        #expect(failure == nil)
        let calls = await transport.calls
        guard case .devicesAdd(let name, let alias, let person)? = calls.first else {
            Issue.record("expected devices.add, got \(String(describing: calls.first))")
            return
        }
        #expect(name == "pixel")
        #expect(alias == "kitchen")
        #expect(person)
    }

    @Test func anEmptyAliasAsksTheServiceForItsDefault() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let failure = await makeActions(transport).add(device(), alias: "   ")
        #expect(failure == nil)
        let calls = await transport.calls
        guard case .devicesAdd(_, let alias, _)? = calls.first else {
            Issue.record("expected devices.add")
            return
        }
        #expect(alias == nil)
    }

    @Test func renameNoteAndRemoveArePersonActions() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let actions = makeActions(transport)
        #expect(await actions.rename(device(), to: "phone") == nil)
        #expect(await actions.setNote(device(), note: "kitchen shelf") == nil)
        #expect(await actions.setNote(device(), note: "  ") == nil)
        #expect(await actions.remove(device()) == nil)
        let calls = await transport.calls
        #expect(calls.count == 4)
        guard case .devicesRename(let name, let alias, let person) = calls[0] else {
            Issue.record("expected devices.rename")
            return
        }
        #expect(name == "pixel" && alias == "phone" && person)
        guard case .devicesNote(let noted, let note) = calls[1] else {
            Issue.record("expected devices.note")
            return
        }
        #expect(noted == "pixel" && note == "kitchen shelf")
        guard case .devicesNote(_, let cleared) = calls[2] else {
            Issue.record("expected devices.note")
            return
        }
        #expect(cleared == nil)
        guard case .devicesRemove(let removed, let person) = calls[3] else {
            Issue.record("expected devices.remove")
            return
        }
        #expect(removed == "pixel" && person)
    }

    @Test func anUnchangedRenameSendsNothing() async {
        let transport = ActionTransport(device: device(), claim: claim())
        #expect(await makeActions(transport).rename(device(), to: "pixel") == nil)
        #expect(await transport.calls.isEmpty)
        #expect(await makeActions(transport).rename(device(), to: "") == nil)
        #expect(await transport.calls.isEmpty)
    }

    @Test func forgetSessionsRecordsTheAppAndIsPersonOnly() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        #expect(await makeActions(transport).forgetSessions(device()) == nil)
        let calls = await transport.calls
        guard case .devicesForgetSessions(let name, let person, let path, let pid)? = calls.first
        else {
            Issue.record("expected devices.forget_sessions")
            return
        }
        #expect(name == "pixel")
        #expect(person)
        #expect(path == "/Applications/Hostler.app")
        #expect(pid == 99)
    }

    // MARK: Claims

    /// Fix F-7: Clear is `devices.note` with no note — the CLI's `--clear`, sent to the service.
    @Test func clearNoteSendsNoNote() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        #expect(await makeActions(transport).clearNote(device()) == nil)
        let calls = await transport.calls
        #expect(calls.count == 1)
        guard case .devicesNote(let noted, let note)? = calls.first else {
            Issue.record("expected devices.note")
            return
        }
        #expect(noted == "pixel")
        #expect(note == nil)
    }

    @Test func reserveUnreserveAndRenew() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let actions = makeActions(transport)
        #expect(await actions.reserve(ResourceId(rawValue: "d1"), for: "w1") == nil)
        #expect(await actions.unreserve(ResourceId(rawValue: "d1"), for: "w1") == nil)
        #expect(await actions.renew(claim()) == nil)
        let calls = await transport.calls
        #expect(calls.count == 3)
        guard case .claimsReserve(let resource, let workspaceId) = calls[0] else {
            Issue.record("expected claims.reserve")
            return
        }
        #expect(resource.rawValue == "d1" && workspaceId == "w1")
        guard case .claimsUnreserve(let resource, let workspaceId) = calls[1] else {
            Issue.record("expected claims.unreserve")
            return
        }
        #expect(resource.rawValue == "d1" && workspaceId == "w1")
        guard case .claimsRenew(let claimId, let renewedFor) = calls[2] else {
            Issue.record("expected claims.renew")
            return
        }
        #expect(claimId == "c1" && renewedFor == "w1")
    }

    // MARK: The confirmation token

    @Test func theConfirmationTokenIsTheAliasAndTheResourceWithoutOne() {
        #expect(DeviceActions.confirmationToken(of: device()) == "pixel")
        #expect(DeviceActions.confirmationToken(of: device(alias: nil)) == "d1")
        #expect(DeviceActions.confirmationMatches("pixel", device: device()))
        #expect(DeviceActions.confirmationMatches(" pixel ", device: device()))
        #expect(!DeviceActions.confirmationMatches("Pixel", device: device()))
        #expect(!DeviceActions.confirmationMatches("", device: device()))
    }

    @Test func releaseRefusesAMismatchedTokenAndSendsNothing() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let failure = await makeActions(transport, runner: runner)
            .release(claim(), token: "pixel", typed: "someone")
        #expect(failure == .confirmationMismatch(expected: "pixel"))
        #expect(runner.invocations.isEmpty)
        #expect(await transport.calls.isEmpty)
    }

    @Test func releaseRunsTheCLIInTheClaimsWorkspace() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let failure = await makeActions(transport, runner: runner)
            .release(claim(), token: "pixel", typed: "pixel")
        #expect(failure == nil)
        let invocations = runner.invocations
        #expect(invocations.count == 1)
        let invocation = try #require(invocations.first)
        #expect(invocation.executable == "/bin/hostler")
        #expect(invocation.arguments == ["release", "c1", "--force", "--confirmation", "pixel"])
        #expect(invocation.cwd == "/work/app")
        #expect(invocation.timeout == CLIInvocation.takeoverTimeout)
        #expect(await transport.calls.isEmpty)
    }

    @Test func takeOverRefusesAMismatchedTokenAndSendsNothing() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let failure = await makeActions(transport, runner: runner)
            .takeOver(device(), in: workspace(), typed: "other")
        #expect(failure == .confirmationMismatch(expected: "pixel"))
        #expect(runner.invocations.isEmpty)
    }

    @Test func takeOverRunsTheHoldInThatWorkspace() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let failure = await makeActions(transport, runner: runner)
            .takeOver(device(), in: workspace(), typed: "pixel")
        #expect(failure == nil)
        let invocation = try #require(runner.invocations.first)
        #expect(invocation.arguments == ["hold", "device", "pixel", "--force", "--confirmation",
                                        "pixel", "--ttl", "30"])
        #expect(invocation.cwd == "/work/app")
        #expect(invocation.timeout == CLIInvocation.takeoverTimeout)
    }

    @Test func aCLIActionWithoutABinaryRefusesWithoutRunning() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let failure = await makeActions(transport, runner: runner, cli: nil)
            .release(claim(), token: "pixel", typed: "pixel")
        #expect(failure == .noCommandLine)
        #expect(runner.invocations.isEmpty)
    }

    @Test func aCLIErrorIsTheAppsError() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner(result: .failed(HostlerError(
            code: .permissionDenied, message: "not now", details: .null, recovery: "try later")))
        let failure = await makeActions(transport, runner: runner)
            .release(claim(), token: "pixel", typed: "pixel")
        #expect(failure == .app(.service(message: "not now", recovery: "try later")))
    }

    // MARK: Doctor

    private func doctorReport() -> DoctorReport {
        DoctorReport(checks: [
            Check(id: "home", status: .ok, message: "the home is writable", details: .null),
            Check(id: "tools", status: .problem, message: "adb is missing", details: .null),
        ], fixed: [])
    }

    @Test func doctorReadsTheReportAtExitStatusOne() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let report = doctorReport()
        let runner = FakeCommandRunner(result: .answered(try report.value(), exit: 1))
        let result = await makeActions(transport, runner: runner).runDoctor(in: workspace())
        guard case .success(let read) = result else {
            Issue.record("expected the report, got \(result)")
            return
        }
        #expect(read == report)
        let invocation = try #require(runner.invocations.first)
        #expect(invocation.executable == "/bin/hostler")
        // The runner appends `--json` itself.
        #expect(invocation.arguments == ["doctor"])
        #expect(invocation.cwd == "/work/app")
    }

    @Test func doctorReportsAnEnvelopeError() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let error = HostlerError(code: .toolMissing, message: "no adb", details: .null,
                                 recovery: "install it")
        let runner = FakeCommandRunner(result: .failed(error))
        let result = await makeActions(transport, runner: runner).runDoctor(in: workspace())
        #expect(result == .failure(.app(.service(message: "no adb", recovery: "install it"))))
    }

    @Test func doctorWithoutABinaryRefuses() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let result = await makeActions(transport, runner: runner, cli: nil)
            .runDoctor(in: workspace())
        #expect(result == .failure(.noCommandLine))
        #expect(runner.invocations.isEmpty)
    }

    @Test func doctorReportsAnUnreadableAnswer() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner(result: .answered(.object(["checks": .string("x")]),
                                                         exit: 1))
        let result = await makeActions(transport, runner: runner).runDoctor(in: workspace())
        #expect(result == .failure(.app(.service(
            message: "hostler's doctor answer could not be read", recovery: nil))))
    }

    @Test func doctorReportsACommandFailureAndACancellation() async {
        let transport = ActionTransport(device: device(), claim: claim())
        let failed = FakeCommandRunner(result: .cliFailed(exit: 3, stderr: "broken"))
        #expect(await makeActions(transport, runner: failed).runDoctor(in: workspace())
            == .failure(.app(.cliFailed(exit: 3, stderr: "broken"))))
        let cancelled = FakeCommandRunner(result: .cancelled(command: "hostler doctor"))
        #expect(await makeActions(transport, runner: cancelled).runDoctor(in: workspace())
            == .failure(.app(.cliCancelled(command: "hostler doctor"))))
    }

    @Test func theTokenOfAScreenOrAnAccountIsTheCLIsToken() {
        #expect(confirmationToken(for: Mk.resource("screen", kind: .screen)) == "screen")
        let account = ResourceInfo(id: ResourceId(rawValue: "account:looky/qa-user"),
                                   kind: .account, display: "account qa-user",
                                   attributes: [:], available: true)
        #expect(confirmationToken(for: account) == "qa-user")
        // A device's token is its display (the alias the catalog reports).
        let device = ResourceInfo(id: ResourceId(rawValue: "device:android:1"), kind: .device,
                                  display: "samsung", attributes: [:], available: true)
        #expect(confirmationToken(for: device) == "samsung")
        #expect(ResourceId(rawValue: "screen").token == "screen")
        #expect(ResourceId(rawValue: "account:p/name").token == "name")
        #expect(ResourceId(rawValue: "account:p/").token == "account:p/")
        #expect(ResourceId(rawValue: "device:ios:abc").token == "device:ios:abc")
    }

    @Test func aClaimRowsTokenFollowsItsResource() {
        let screenClaim = Claim(id: "c1", resource: Mk.resource("screen", kind: .screen),
                                kind: .lease, workspace: WorkspaceRef(id: "w1",
                                                                      path: "/work/app"),
                                grantedAtMs: 1)
        let accountClaim = Claim(
            id: "c2",
            resource: ResourceInfo(id: ResourceId(rawValue: "account:looky/qa-user"),
                                   kind: .account, display: "account qa-user",
                                   attributes: [:], available: true),
            kind: .hold, workspace: WorkspaceRef(id: "w1", path: "/work/app"), grantedAtMs: 1)
        let rows = claimRows([screenClaim, accountClaim], nowMs: 2)
        #expect(rows.map(\.confirmationToken) == ["qa-user", "screen"])
        #expect(rows.map(\.resourceKind) == [.account, .screen])
    }

    @Test func aReleaseOfAScreenSendsItsOwnToken() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let runner = FakeCommandRunner()
        let screenClaim = Claim(id: "c1", resource: Mk.resource("screen", kind: .screen),
                                kind: .hold, workspace: WorkspaceRef(id: "w1",
                                                                     path: "/work/app"),
                                grantedAtMs: 1)
        let failure = await makeActions(transport, runner: runner)
            .release(screenClaim, token: "screen", typed: "screen")
        #expect(failure == nil)
        let invocation = try #require(runner.invocations.first)
        #expect(invocation.arguments
            == ["release", "c1", "--force", "--confirmation", "screen"])
        #expect(invocation.cwd == "/work/app")
    }

    // MARK: Failures and the workspace's own action

    /// Fix F-7: a request names a device by something the service resolves. The display name of
    /// an unregistered device can be its model, which `devices.note` does not know; the hardware
    /// id and the resource id it does.
    @Test func aDeviceWithoutAnAliasIsNamedByIdentityTheServiceResolves() async throws {
        let unregistered = device(alias: nil)
        let transport = ActionTransport(device: unregistered, claim: claim())
        let actions = makeActions(transport)
        #expect(DeviceActions.serviceName(of: device(alias: "pixel")) == "pixel")
        #expect(DeviceActions.serviceName(of: unregistered) == "HW-1")
        var unknownId = unregistered
        unknownId.hardwareId = ""
        #expect(DeviceActions.serviceName(of: unknownId) == "d1")
        #expect(await actions.setNote(unregistered, note: "x") == nil)
        #expect(await actions.rename(unregistered, to: "new") == nil)
        #expect(await actions.clearNote(unregistered) == nil)
        let calls = await transport.calls
        guard case .devicesNote(let noted, _) = calls[0],
              case .devicesRename(let renamed, _, _) = calls[1],
              case .devicesNote(let cleared, let note) = calls[2] else {
            Issue.record("expected devices.note and devices.rename")
            return
        }
        #expect(noted == "HW-1" && renamed == "HW-1" && cleared == "HW-1" && note == nil)
    }

    @Test func aServiceErrorBecomesAnAppError() async {
        let transport = ActionTransport(device: device(), claim: claim())
        await transport.fail(.devicesAdd, with: .service(HostlerError(
            code: .aliasTaken, message: "that alias is taken", details: .null,
            recovery: "pick another")))
        let failure = await makeActions(transport).add(device(), alias: "pixel")
        #expect(failure == .app(.service(message: "that alias is taken",
                                        recovery: "pick another")))
    }

    @Test func aNotConnectedCallIsReported() async {
        let transport = ActionTransport(device: device(), claim: claim())
        await transport.fail(.claimsReserve, with: .notConnected)
        #expect(await makeActions(transport).reserve(Mk.resource("d1").id, for: "w1")
            == .app(.service(message: "hostler's service is not running", recovery: nil)))
    }

    @Test func withdrawSendsClaimsWithdraw() async throws {
        let transport = ActionTransport(device: device(), claim: claim())
        let request = WaitingRequest(requestId: "r1", ticket: 2, workspacePath: "/work/app",
                                     kind: .lease, sinceMs: now)
        #expect(await WorkspaceActions(client: HostlerClient(transport: transport))
            .withdraw(request) == nil)
        let calls = await transport.calls
        guard case .claimsWithdraw(let id, let timedOut)? = calls.first else {
            Issue.record("expected claims.withdraw")
            return
        }
        #expect(id == "r1")
        #expect(!timedOut)
    }

    @Test func theNoConnectionTransportReportsIt() async {
        let client = HostlerClient(transport: NotConnectedTransport())
        await #expect(throws: HostlerCallError.notConnected) {
            try await client.devicesList(all: true)
        }
    }
}

private extension Encodable {
    /// The value as the JSON a CLI envelope carries, for a `CLIResult.answered`.
    func value() throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(self))
    }
}
