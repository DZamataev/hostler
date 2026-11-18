import Foundation
import Testing
@testable import HostlerKit

@Suite struct StandingTests {
    let id = "device:android:1"
    let app = WorkspaceChip(workspaceId: "w1", path: "/work/app")
    let other = WorkspaceChip(workspaceId: "w2", path: "/work/other")
    let workspaces = [Mk.workspace("w1", project: "p1", path: "/work/app"),
                      Mk.workspace("w2", project: "p1", path: "/work/other")]

    private func claim(_ claimId: String, _ kind: ClaimKind, _ chip: WorkspaceChip) -> Claim {
        Mk.claim(claimId, on: id, kind: kind, workspace: chip.workspaceId!, path: chip.path)
    }

    @Test func free() {
        #expect(standing(device: Mk.device(id), claims: nil, workspaces: []) == [.free])
        #expect(standing(device: Mk.device(id), claims: Mk.state(id), workspaces: []) == [.free])
    }

    @Test func reserved() {
        let state = Mk.state(id, claims: [claim("c1", .reservation, app)])
        #expect(standing(device: Mk.device(id), claims: state, workspaces: []) == [.reserved(by: app)])
    }

    @Test func heldByEveryDistinctWorkspaceOfAKind() {
        let state = Mk.state(id, claims: [
            claim("c1", .lease, app), claim("c2", .lease, other), claim("c3", .lease, app),
        ])
        #expect(standing(device: Mk.device(id), claims: state, workspaces: []) == [.held(by: [app, other], kind: .lease)])
    }

    @Test func aReservationPlusAnotherWorkspacesLiveHoldShowsBothChips() {
        let state = Mk.state(id, claims: [claim("c1", .reservation, app), claim("c2", .hold, other)])
        #expect(standing(device: Mk.device(id), claims: state, workspaces: []) == [
            .reserved(by: app), .held(by: [other], kind: .hold),
        ])
    }

    @Test func agentDeviceTestingOverAReservation() {
        let device = Mk.device(id, sessions: [Mk.session("s1", path: "/work/other")])
        let state = Mk.state(id, claims: [claim("c1", .reservation, app)], foreign: [
            // agent-device's own claim for the same session is the session, not a second chip.
            ForeignClaim(resource: ResourceId(rawValue: id), source: "agent-device",
                         description: "agent-device session s1", workspacePath: "/work/other",
                         stop: .object(["session": .string("s1")])),
        ])
        #expect(standing(device: device, claims: state, workspaces: workspaces) == [
            .reserved(by: app), .testing(session: "s1", by: other),
        ])
    }

    @Test func testingWithoutARegisteredWorkspaceHasNoColor() {
        let device = Mk.device(id, sessions: [Mk.session("s1", path: "/elsewhere/tool")])
        let chip = WorkspaceChip(workspaceId: nil, path: "/elsewhere/tool")
        #expect(standing(device: device, claims: nil, workspaces: workspaces) == [.testing(session: "s1", by: chip)])
        #expect(chip.color == nil)
        #expect(chip.name == "tool")
    }

    @Test func foreign() {
        let state = Mk.state(id, foreign: [
            ForeignClaim(resource: ResourceId(rawValue: id), source: "rn-looky", description: "a build",
                         workspacePath: "/work/app"),
            ForeignClaim(resource: ResourceId(rawValue: id), source: "agent-device",
                         description: "agent-device session gone", stop: .object(["session": .string("gone")])),
        ])
        #expect(standing(device: Mk.device(id), claims: state, workspaces: workspaces) == [
            .foreign(source: "rn-looky", by: app), .foreign(source: "agent-device", by: nil),
        ])
    }

    @Test func waiting() {
        let state = Mk.state(id, claims: [claim("c1", .lease, app)], waiting: 2)
        #expect(standing(device: Mk.device(id), claims: state, workspaces: []) == [
            .held(by: [app], kind: .lease), .waiting(count: 2),
        ])
    }

    @Test func notConnectedAndNotBooted() {
        #expect(standing(device: Mk.device(id, connected: false), claims: nil, workspaces: []) == [.notConnected])
        #expect(standing(device: Mk.device(id, kind: .emulator, connected: false), claims: nil, workspaces: []) == [.notBooted])
        #expect(standing(device: Mk.device(id, kind: .simulator, connected: false), claims: nil, workspaces: []) == [.notBooted])
        // A reservation outlives the connection.
        let state = Mk.state(id, claims: [claim("c1", .reservation, app)])
        #expect(standing(device: Mk.device(id, connected: false), claims: state, workspaces: []) == [
            .notConnected, .reserved(by: app),
        ])
    }
}

@Suite struct WorkspaceRowsTests {
    @Test func rowsCountDistinctHeldResourcesAndMarkRunningHeavyTasks() {
        let project = Mk.project("p1", workspaces: [
            Mk.workspace("w1", project: "p1", path: "/work/app"),
            Mk.workspace("w2", project: "p1", path: "/work/app-2/"),
        ])
        let claims = Mk.claims(seq: 1, [
            Mk.state("screen", claims: [
                Mk.claim("c1", on: "screen", kind: .hold, workspace: "w1", path: "/work/app"),
                Mk.claim("c2", on: "screen", kind: .lease, workspace: "w1", path: "/work/app"),
            ]),
            Mk.state("device:android:1", claims: [
                Mk.claim("c3", on: "device:android:1", kind: .reservation, workspace: "w1", path: "/work/app"),
            ]),
            Mk.state("device:android:2", claims: [
                Mk.claim("c4", on: "device:android:2", kind: .lease, workspace: "w9", path: "/work/x"),
            ]),
        ])
        let queue = Mk.queue(seq: 1, running: [Mk.entry("h1", workspaceRoot: "/work/app-2")],
                             waiting: [Mk.entry("h2", workspaceRoot: "/work/app", state: .waiting)])
        let rows = workspaceRows(project: project, claims: claims, queue: queue, branches: ["w1": "main"])
        #expect(rows == [
            WorkspaceRow(id: "w1", projectId: "p1", path: "/work/app", folderName: "app", branch: "main",
                         color: WorkspaceColor(workspaceId: "w1"), heldCount: 2, heavyRunning: false),
            WorkspaceRow(id: "w2", projectId: "p1", path: "/work/app-2/", folderName: "app-2", branch: nil,
                         color: WorkspaceColor(workspaceId: "w2"), heldCount: 0, heavyRunning: true),
        ])
    }
}

@Suite struct WorkspaceColorsTests {
    @Test func theSameIdAlwaysGetsTheSameColor() {
        // Fixed values: FNV-1a is the same on every run and machine.
        #expect(WorkspaceColor.fnv1a("") == 0xcbf2_9ce4_8422_2325)
        #expect(WorkspaceColor.fnv1a("a") == 0xaf63_dc4c_8601_ec8c)
        #expect(WorkspaceColor(workspaceId: "w1") == WorkspaceColor(workspaceId: "w1"))
        let ids = (0..<50).map { "ws-\($0)" }
        #expect(ids.map(WorkspaceColor.init(workspaceId:)) == ids.map(WorkspaceColor.init(workspaceId:)))
    }

    @Test(arguments: ["uuid", "sequential"])
    func anEvenSpreadOverAThousandIds(kind: String) {
        var generator = SplitMix(seed: 7)
        let ids: [String] = (0..<1000).map { index in
            kind == "uuid" ? generator.uuid() : "ws-\(index)"
        }
        var counts: [WorkspaceColor: Int] = [:]
        for id in ids { counts[WorkspaceColor(workspaceId: id), default: 0] += 1 }
        #expect(counts.count == WorkspaceColor.allCases.count)
        #expect(counts.values.allSatisfy { $0 <= 150 }, "\(counts)")
    }

    @Test func chipsTakeTheFolderName() {
        #expect(WorkspaceChip(workspaceId: "w1", path: "/work/app/").name == "app")
        #expect(WorkspaceChip(workspaceId: "w1", path: "/").name == "/")
        #expect(WorkspaceChip(workspaceId: "w1", path: "/work/app").color == WorkspaceColor(workspaceId: "w1"))
    }
}

/// A deterministic generator for test ids.
struct SplitMix {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
    mutating func uuid() -> String {
        let a = next(), b = next()
        let bytes = (0..<8).map { UInt8(truncatingIfNeeded: a >> ($0 * 8)) }
            + (0..<8).map { UInt8(truncatingIfNeeded: b >> ($0 * 8)) }
        return UUID(uuid: (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                           bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
            .uuidString.lowercased()
    }
}
