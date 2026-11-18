import Foundation

/// What a window action failed with (plan Task 10): the app's own error, or a refusal the app
/// makes before anything is sent (a confirmation token that does not match).
public enum ActionFailure: Error, Equatable, Sendable, LocalizedError {
    case app(HostlerAppError)
    case confirmationMismatch(expected: String)
    /// A CLI action with no `hostler` binary to run (the service is not running).
    case noCommandLine

    /// The line the alert's title carries.
    public var message: String {
        switch self {
        case .app(let error):
            error.errorDescription ?? String(localized: "hostler could not do that")
        case .confirmationMismatch(let expected):
            String(localized: "Type \(expected) to confirm")
        case .noCommandLine: String(localized: "hostler's command is not known")
        }
    }

    /// The line under it, when there is one.
    public var recovery: String? {
        switch self {
        case .app(let error): error.recoverySuggestion
        case .confirmationMismatch: String(localized: "Nothing was sent to hostler.")
        case .noCommandLine: String(localized:
            "The action runs through hostler's own binary, which the service reports.")
        }
    }

    public var errorDescription: String? { message }
    public var recoverySuggestion: String? { recovery }
}

/// The device actions of the window (plan Task 10). Every one runs against the finished service
/// methods: the direct calls go through `HostlerClient`, and the procedures that need a process
/// identity or the repository's rules (a takeover, `doctor`) run the service's own `hostler`
/// binary through `HostlerCommandRunning`. Pure decisions (the confirmation token) are here, so
/// a test can prove that nothing was sent when a confirmation did not match.
///
/// The window's add, rename, remove and forget-sessions calls carry `person: true` like the
/// CLI's prompt does; that flag records the person's intent and is not proof of a person — any
/// same-user client could set it (spec §8.2, P5-L16).
public struct DeviceActions: Sendable {
    public var client: HostlerClient
    public var runner: any HostlerCommandRunning
    /// The `hostler` binary CLI actions run: `ServiceStatus.exe`, which the store's snapshot
    /// carries. `nil` while the service has not answered yet.
    public var cliExecutable: String?
    /// The app's own process, recorded by `devices.forget_sessions`.
    public var appPid: UInt32
    /// The app's bundle path, recorded with the pid.
    public var appPath: String

    public init(client: HostlerClient, runner: any HostlerCommandRunning,
                cliExecutable: String?, appPid: UInt32 = UInt32(getpid()),
                appPath: String = Bundle.main.bundlePath) {
        self.client = client
        self.runner = runner
        self.cliExecutable = cliExecutable
        self.appPid = appPid
        self.appPath = appPath
    }

    // MARK: Names

    /// The name a request sends for a device (fix F-7): the person's alias when there is one,
    /// otherwise an identity the service resolves for an unregistered device too (its hardware
    /// id, or the resource id when that is empty). `displayName` is for showing, not for
    /// resolving: it may be the model, which `devices.note|rename|remove|forget-sessions` do
    /// not know.
    public static func serviceName(of device: DeviceInfo) -> String {
        if let alias = device.alias, !alias.isEmpty { return alias }
        if !device.hardwareId.isEmpty { return device.hardwareId }
        return device.resource.rawValue
    }

    /// The token a takeover's `--confirmation` must equal (the service's own rule: the alias).
    public static func confirmationToken(of device: DeviceInfo) -> String {
        device.confirmationToken
    }

    /// Whether `typed` is the resource's confirmation token: the app sends nothing otherwise.
    public static func confirmationMatches(_ typed: String, token: String) -> Bool {
        typed.trimmingCharacters(in: .whitespacesAndNewlines) == token
    }

    /// Whether `typed` is the device's confirmation token.
    public static func confirmationMatches(_ typed: String, device: DeviceInfo) -> Bool {
        confirmationMatches(typed, token: device.confirmationToken)
    }

    // MARK: Registry

    /// `devices.add`, with the person's alias (a physical device is registered by a person).
    public func add(_ device: DeviceInfo, alias: String) async -> ActionFailure? {
        let alias = alias.trimmingCharacters(in: .whitespacesAndNewlines)
        do throws(HostlerCallError) {
            _ = try await client.devicesAdd(device: Self.serviceName(of: device),
                                            alias: alias.isEmpty ? nil : alias, person: true)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `devices.rename` (person-only for a physical device).
    public func rename(_ device: DeviceInfo, to alias: String) async -> ActionFailure? {
        let alias = alias.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !alias.isEmpty, alias != device.alias else { return nil }
        do throws(HostlerCallError) {
            _ = try await client.devicesRename(device: Self.serviceName(of: device), alias: alias,
                                               person: true)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `devices.note`: the new note, or `nil` to clear it.
    public func setNote(_ device: DeviceInfo, note: String?) async -> ActionFailure? {
        let note = note?.trimmingCharacters(in: .whitespacesAndNewlines)
        do throws(HostlerCallError) {
            _ = try await client.devicesNote(device: Self.serviceName(of: device),
                                             note: note?.isEmpty == true ? nil : note)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `devices.note` with no note: the CLI's `--clear` (fix F-7). It is the same direct call
    /// `setNote` uses, so Clear reaches the service instead of only emptying the field.
    public func clearNote(_ device: DeviceInfo) async -> ActionFailure? {
        await setNote(device, note: nil)
    }

    /// `devices.remove` (person-only for a physical device; nothing on the device changes).
    public func remove(_ device: DeviceInfo) async -> ActionFailure? {
        do throws(HostlerCallError) {
            _ = try await client.devicesRemove(device: Self.serviceName(of: device), person: true)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `devices.forget_sessions`: release the device from the wait for agent-device's sessions
    /// while agent-device cannot be read (L54). It stops nothing.
    public func forgetSessions(_ device: DeviceInfo) async -> ActionFailure? {
        do throws(HostlerCallError) {
            _ = try await client.devicesForgetSessions(
                device: Self.serviceName(of: device), person: true, byWorkspacePath: appPath,
                byPid: appPid)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    // MARK: Claims

    /// `claims.reserve` for a workspace, by resource id: the screen and accounts reserve too.
    public func reserve(_ resource: ResourceId, for workspaceId: String) async -> ActionFailure? {
        do throws(HostlerCallError) {
            _ = try await client.claimsReserve(resource: resource, workspaceId: workspaceId)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `claims.unreserve` for a workspace, by resource id.
    public func unreserve(_ resource: ResourceId,
                          for workspaceId: String) async -> ActionFailure? {
        do throws(HostlerCallError) {
            try await client.claimsUnreserve(resource: resource, workspaceId: workspaceId)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// `claims.renew`: a TTL hold moves to now + its TTL.
    public func renew(_ claim: Claim) async -> ActionFailure? {
        do throws(HostlerCallError) {
            _ = try await client.claimsRenew(claimId: claim.id,
                                             workspaceId: claim.workspace.id)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }

    /// Ends a claim without stopping what uses it: `hostler release <claim> --force
    /// --confirmation <token>`, run in the claim's workspace (a device, the screen or an
    /// account: `token` is `ClaimRow.confirmationToken`). Nothing runs unless `typed` is it.
    public func release(_ claim: Claim, token: String, typed: String) async -> ActionFailure? {
        guard Self.confirmationMatches(typed, token: token) else {
            return .confirmationMismatch(expected: token)
        }
        return await run(arguments: ["release", claim.id, "--force", "--confirmation", token],
                         cwd: claim.workspace.path)
    }

    /// Takes the device over for a workspace: `hostler hold device <alias> --force
    /// --confirmation <alias> --ttl 30`, run in that workspace. The CLI stops the run that
    /// holds it (the service's own takeover, §9.7). Nothing runs unless `typed` is the token.
    public func takeOver(_ device: DeviceInfo, in workspace: WorkspaceInfo,
                         typed: String) async -> ActionFailure? {
        guard Self.confirmationMatches(typed, device: device) else {
            return .confirmationMismatch(expected: device.confirmationToken)
        }
        return await run(arguments: ["hold", "device", Self.serviceName(of: device), "--force",
                                     "--confirmation", device.confirmationToken, "--ttl", "30"],
                         cwd: workspace.path)
    }

    // MARK: Doctor

    /// `hostler doctor --json` in a workspace (never `--fix`: the app runs no fix by itself).
    ///
    /// `doctor` exits 1 when a check remains a problem while its envelope is still complete; the
    /// runner answers that as `CLIResult.answered`, so the report is read like any other answer
    /// (P5-L15).
    public func runDoctor(in workspace: WorkspaceInfo) async
        -> Result<DoctorReport, ActionFailure>
    {
        guard let executable = cliExecutable else { return .failure(.noCommandLine) }
        let invocation = CLIInvocation(executable: executable, arguments: ["doctor"],
                                       cwd: workspace.path)
        let result = await runner.run(invocation)
        switch result {
        case .ok(let value), .answered(let value, _):
            return doctorReport(value)
        case .failed(let error):
            return .failure(.app(.service(message: error.message, recovery: error.recovery)))
        case .cliFailed(let exit, let stderr):
            return .failure(.app(.cliFailed(exit: exit, stderr: stderr)))
        case .notStarted(let why):
            return .failure(.app(.cliNotStarted(why)))
        case .timedOut(let command):
            return .failure(.app(.cliTimedOut(command: command)))
        case .cancelled(let command):
            return .failure(.app(.cliCancelled(command: command)))
        }
    }

    private func doctorReport(_ value: JSONValue) -> Result<DoctorReport, ActionFailure> {
        do {
            return .success(try value.decoded(DoctorReport.self))
        } catch {
            return .failure(.app(.service(
                message: String(localized: "hostler's doctor answer could not be read"),
                recovery: nil)))
        }
    }

    // MARK: The CLI

    private func run(arguments: [String], cwd: String) async -> ActionFailure? {
        guard let executable = cliExecutable else { return .noCommandLine }
        let invocation = CLIInvocation(executable: executable, arguments: arguments, cwd: cwd,
                                       timeout: CLIInvocation.takeoverTimeout)
        let result = await runner.run(invocation)
        if let error = result.appError { return .app(error) }
        return nil
    }
}

/// The workspace's own action: withdrawing a request that waits for a resource (spec §9.3).
/// There is no CLI form — a waiting process withdraws its request with Ctrl+C — so the app
/// sends `claims.withdraw` itself.
public struct WorkspaceActions: Sendable {
    public var client: HostlerClient

    public init(client: HostlerClient) {
        self.client = client
    }

    public func withdraw(_ request: WaitingRequest) async -> ActionFailure? {
        do throws(HostlerCallError) {
            try await client.claimsWithdraw(requestId: request.requestId, timedOut: false)
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }
}

extension JSONValue {
    /// The value decoded as a contract type: a CLI answer's `data`, as the generated types read
    /// it (plan P5-L15: `CLIResult.answered` carries the envelope's value, not raw bytes).
    public func decoded<T: Decodable>(_ type: T.Type) throws -> T {
        let data = try JSONEncoder().encode(self)
        return try JSONDecoder().decode(T.self, from: data)
    }
}

/// A transport for a window built without a connection (a test, a preview): every call reports
/// that the service is not running, the way `ConnectionController`'s client does.
public struct NotConnectedTransport: HostlerTransport {
    public init() {}

    public func call(_ request: Request, timeout: Duration) async throws(HostlerCallError) -> Data {
        throw .notConnected
    }

    public func callSensitive(_ request: Request, timeout: Duration) async throws(HostlerCallError)
        -> SensitiveData
    {
        throw .notConnected
    }
}
