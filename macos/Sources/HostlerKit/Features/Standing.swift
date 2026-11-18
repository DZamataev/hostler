import Foundation

/// One thing a device's standing shows as a chip (spec §9.3: free, reserved by a workspace,
/// held, waiting requests, agent-device testing, foreign claim; plus the device's own state).
public enum DeviceStanding: Hashable, Sendable {
    /// Connected, and nothing holds, reserves, tests or waits for it.
    case free
    /// A workspace's reservation.
    case reserved(by: WorkspaceChip)
    /// Live leases or holds, one entry per kind, with every distinct workspace holding it.
    case held(by: [WorkspaceChip], kind: ClaimKind)
    /// An agent-device session runs on it (`by` is the session's workspace).
    case testing(session: String, by: WorkspaceChip)
    /// A claim of another tool (`source`), not an agent-device session shown as `testing`.
    case foreign(source: String, by: WorkspaceChip?)
    /// Requests queued for it.
    case waiting(count: Int)
    /// A physical device that is not connected.
    case notConnected
    /// An emulator or simulator that is not running.
    case notBooted
}

/// A device's standing, every chip that applies, in display order (pure; plan Task 8):
/// connection state, reservations, live holds and leases, agent-device sessions, foreign
/// claims, waiting requests. `[.free]` when connected and nothing applies.
///
/// - Parameters:
///   - device: the device (its connection and agent-device sessions).
///   - claims: the device's entry of the claims snapshot, or `nil` when it has none.
///   - workspaces: the registered workspaces, to color a path that came without an id.
public func standing(device: DeviceInfo, claims: ResourceState?,
                     workspaces: [WorkspaceInfo]) -> [DeviceStanding] {
    var result: [DeviceStanding] = []
    if !device.connected {
        result.append(device.kind == .physical ? .notConnected : .notBooted)
    }

    let own = claims?.claims ?? []
    for claim in own where claim.kind == .reservation {
        let chip = WorkspaceChip(workspaceId: claim.workspace.id, path: claim.workspace.path)
        if !result.contains(.reserved(by: chip)) { result.append(.reserved(by: chip)) }
    }
    for kind in [ClaimKind.hold, .lease] {
        var chips: [WorkspaceChip] = []
        for claim in own where claim.kind == kind {
            let chip = WorkspaceChip(workspaceId: claim.workspace.id, path: claim.workspace.path)
            if !chips.contains(chip) { chips.append(chip) }
        }
        if !chips.isEmpty { result.append(.held(by: chips, kind: kind)) }
    }

    let sessionNames = Set(device.sessions.map(\.session))
    for session in device.sessions {
        let chip = WorkspaceChip.of(path: session.workspacePath, workspaceId: session.workspaceId,
                                    workspaces: workspaces)
        result.append(.testing(session: session.session, by: chip))
    }
    for foreign in claims?.foreign ?? [] {
        // agent-device's own claim for a session the device already lists is that session.
        if foreign.source == agentDeviceSource, let name = foreign.stop.sessionName,
           sessionNames.contains(name) {
            continue
        }
        let chip = foreign.workspacePath.map {
            WorkspaceChip.of(path: $0, workspaceId: foreign.workspaceId, workspaces: workspaces)
        }
        result.append(.foreign(source: foreign.source, by: chip))
    }

    let waiting = claims?.waiting.count ?? 0
    if waiting > 0 { result.append(.waiting(count: waiting)) }

    if result.isEmpty { result.append(.free) }
    return result
}

/// The foreign-claim source that reports agent-device sessions.
let agentDeviceSource = "agent-device"

private extension JSONValue {
    /// `{"session": "<name>"}`: agent-device's stop target.
    var sessionName: String? {
        guard case .object(let fields) = self, case .string(let name)? = fields["session"] else {
            return nil
        }
        return name
    }
}
