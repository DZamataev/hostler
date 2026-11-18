import Foundation
import Testing
@testable import HostlerKit

/// The sidebar's sections and the workspace detail's model (plan Task 10): the 30-day rule, which
/// claims belong to a workspace, its waiting requests and heavy tasks, "other devices", and the
/// secrets line.
@Suite struct WorkspaceDetailPresentationTests {
    let now: Int64 = Mk.now
    let day: Int64 = 86_400_000

    private func workspace(_ id: String, project: String = "p1", path: String,
                           lastSeenMs: Int64? = nil) -> WorkspaceInfo {
        WorkspaceInfo(id: id, projectId: project, path: path, outcome: .known,
                      projectRepointed: false, lastSeenMs: lastSeenMs ?? now)
    }

    private func project(_ id: String = "p1", name: String = "Looky",
                         workspaces: [WorkspaceInfo]) -> ProjectView {
        ProjectView(info: ProjectInfo(id: id, name: name, commonDir: "/work/\(id)/.git",
                                      approved: true, createdAtMs: now),
                    workspaces: workspaces)
    }

    private func device(_ id: String, registered: Bool = true) -> DeviceInfo {
        DeviceInfo(resource: ResourceId(rawValue: id), hardwareId: "HW-\(id)", platform: .android,
                   kind: .physical, registered: registered, aliasByPerson: false, model: "M",
                   connected: true, ready: true, transports: [], bootedByHostler: false,
                   shuttingDown: false, removing: false, sessions: [], checkingSessions: false,
                   firstSeenMs: now, lastSeenMs: now)
    }

    private func claim(_ id: String, on resource: String, kind: ClaimKind, workspace: String,
                       path: String) -> Claim {
        Claim(id: id, resource: Mk.resource(resource), kind: kind,
              workspace: WorkspaceRef(id: workspace, path: path), grantedAtMs: now - 60_000)
    }

    // MARK: The 30-day rule

    @Test func oldWorkspacesWithoutWorkAreHiddenBehindTheMenu() {
        let recent = workspace("recent", path: "/work/recent")
        let old = workspace("old", path: "/work/old", lastSeenMs: now - 40 * day)
        let holding = workspace("holding", path: "/work/holding", lastSeenMs: now - 40 * day)
        let claims = Mk.claims(seq: 1, [
            Mk.state("d1", claims: [claim("c1", on: "d1", kind: .hold, workspace: "holding",
                                          path: "/work/holding")]),
        ])
        let groups = sidebarGroups(projects: [project(workspaces: [recent, old, holding])],
                                   claims: claims, queue: nil, branches: [:], nowMs: now)
        #expect(groups.count == 1)
        #expect(groups[0].name == "Looky")
        #expect(groups[0].visible.map(\.id) == ["recent", "holding"])
        #expect(groups[0].hidden.map(\.id) == ["old"])
        #expect(!groups[0].showsHidden)
        #expect(groups[0].rows.map(\.id) == ["recent", "holding"])
    }

    @Test func theSectionMenuShowsTheOlderOnes() {
        let recent = workspace("recent", path: "/work/recent")
        let old = workspace("old", path: "/work/old", lastSeenMs: now - 31 * day)
        let groups = sidebarGroups(projects: [project(workspaces: [recent, old])], claims: nil,
                                   queue: nil, branches: [:], nowMs: now,
                                   showsHidden: ["p1"])
        #expect(groups[0].showsHidden)
        #expect(groups[0].rows.map(\.id) == ["recent", "old"])
        #expect(groups[0].hidden.map(\.id) == ["old"])
    }

    @Test func aWaitingWorkspaceIsAtWorkEvenWhenItHoldsNothing() {
        let old = workspace("old", path: "/work/old", lastSeenMs: now - 40 * day)
        let claims = Mk.claims(seq: 1, [
            ResourceState(resource: Mk.resource("d1"), claims: [], foreign: [],
                          waiting: [WaitingRequest(requestId: "r1", ticket: 1,
                                                   workspacePath: "/work/old", kind: .lease,
                                                   sinceMs: now - 1000)],
                          seizing: false),
        ])
        let groups = sidebarGroups(projects: [project(workspaces: [old])], claims: claims,
                                   queue: nil, branches: [:], nowMs: now)
        #expect(groups[0].visible.map(\.id) == ["old"])
        #expect(groups[0].visible[0].isWaiting)
    }

    @Test func aRunningHeavyTaskKeepsAWorkspaceInTheSidebar() {
        let old = workspace("old", path: "/work/old", lastSeenMs: now - 40 * day)
        let queue = Mk.queue(seq: 1, running: [Mk.entry("h1", workspaceRoot: "/work/old/")])
        let groups = sidebarGroups(projects: [project(workspaces: [old])], claims: nil,
                                   queue: queue, branches: ["old": "dev/4.13"], nowMs: now)
        #expect(groups[0].visible.map(\.id) == ["old"])
        #expect(groups[0].visible[0].row.branch == "dev/4.13")
        #expect(groups[0].visible[0].row.heavyRunning)
    }

    // MARK: The detail

    private func detail(_ workspace: WorkspaceInfo, devices: [DeviceInfo] = [],
                        claims: ClaimsSnapshot? = nil, queue: QueueSnapshot? = nil,
                        secrets: SecretList? = nil, read: Bool = false)
        -> WorkspaceDetailModel
    {
        workspaceDetail(of: workspace, projectName: "Looky", devices: devices, claims: claims,
                        queue: queue, branches: [workspace.id: "dev/4.13"], secrets: secrets,
                        secretsRead: read, nowMs: now)
    }

    @Test func theHeaderOfTheDetail() {
        let model = detail(workspace("w1", path: "/work/app/"))
        #expect(model.folderName == "app")
        #expect(model.branch == "dev/4.13")
        #expect(model.color == WorkspaceColor(workspaceId: "w1"))
        #expect(model.projectName == "Looky")
        #expect(model.projectId == "p1")
        #expect(model.workspace.path == "/work/app/")
    }

    @Test func theClaimsOfTheWorkspaceByIDAndByPath() {
        let mine = claim("c1", on: "d1", kind: .hold, workspace: "w1", path: "/work/app")
        let alsoMine = claim("c2", on: "screen", kind: .hold, workspace: "elsewhere",
                             path: "/work/app/")
        let other = claim("c3", on: "account:p:qa", kind: .hold, workspace: "w2",
                          path: "/work/other")
        let claims = Mk.claims(seq: 1, [
            Mk.state("d1", claims: [mine, other]),
            Mk.state("screen", claims: [alsoMine]),
        ])
        let model = detail(workspace("w1", path: "/work/app"), claims: claims)
        #expect(model.held.map(\.id) == ["c1", "c2"])
        #expect(model.held.map(\.workspace.name) == ["app", "app"])
    }

    @Test func theWaitingRequestsOfTheWorkspace() {
        let claims = Mk.claims(seq: 1, [ResourceState(
            resource: Mk.resource("d1"), claims: [], foreign: [], waiting: [
                WaitingRequest(requestId: "r1", ticket: 1, workspacePath: "/work/app",
                               kind: .lease, sinceMs: now - 120_000),
                WaitingRequest(requestId: "r2", ticket: 2, workspacePath: "/work/other",
                               kind: .lease, sinceMs: now - 60_000),
            ], seizing: false)])
        let model = detail(workspace("w1", path: "/work/app"), claims: claims)
        #expect(model.waiting.map(\.id) == ["r1"])
        #expect(model.waiting[0].resourceName == "d1")
        #expect(model.waiting[0].kindText == "lease")
        #expect(model.waiting[0].sinceText == "2 min")
    }

    @Test func theHeavyTasksOfTheWorkspace() {
        let queue = QueueSnapshot(
            capacity: 2,
            running: [Mk.entry("h1", command: "make build", workspaceRoot: "/work/app"),
                      Mk.entry("h2", workspaceRoot: "/work/other"),
                      Mk.entry("h4", command: "make lint", workspaceRoot: "/work/app")],
            waiting: [Mk.entry("h5", workspaceRoot: "/work/other", state: .waiting),
                      Mk.entry("h3", command: "make test", workspaceRoot: "/work/app/",
                               state: .waiting),
                      Mk.entry("h6", workspaceRoot: "/work/app", state: .waiting)],
            foreignRunning: [], asOfSeq: 1)
        let model = detail(workspace("w1", path: "/work/app"), queue: queue)
        #expect(model.heavy.map(\.id) == ["h1", "h4", "h3", "h6"])
        #expect(model.heavy[0].command == "make build")
        #expect(model.heavy[0].stateText == "running")
        #expect(model.heavy[0].position == nil)
        #expect(model.heavy[1].stateText == "running")
        #expect(model.heavy[1].position == nil)
        #expect(model.heavy[2].stateText == "waiting")
        #expect(model.heavy[2].isWaiting)
        // A waiting position counts the workspace's waiting tasks only (fix R1-4): two running
        // tasks and another workspace's waiting task do not make it "fourth".
        #expect(model.heavy[2].position == 1)
        #expect(model.heavy[3].position == 2)
    }

    @Test func theFooterSaysLiveOnlyWhileLive() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let last = now.addingTimeInterval(-120)
        #expect(footerDetailText(lastEventAt: last, now: now, syncState: .live)
            == "Last event 2 min · live from the service")
        #expect(footerDetailText(lastEventAt: last, now: now,
                                 syncState: .offline(since: last))
            == "Last event 2 min · offline, showing the last read")
        #expect(footerDetailText(lastEventAt: nil, now: now, syncState: .offline(since: nil))
            == "hostler is not connected; nothing has been read")
        #expect(footerDetailText(lastEventAt: last, now: now,
                                 syncState: .failed(.service(message: "no", recovery: nil)))
            == "Last event 2 min · not reading; hostler retries")
        #expect(footerDetailText(lastEventAt: last, now: now, syncState: .syncing(catchingUp: true))
            == "Last event 2 min · catching up: re-reading every snapshot")
        #expect(footerDetailText(lastEventAt: last, now: now, syncState: .syncing(catchingUp: false))
            == "Last event 2 min · reading every snapshot")
    }

    @Test func otherDevicesExcludeTheOnesThisWorkspaceHolds() {
        let held = claim("c1", on: "held", kind: .lease, workspace: "w1", path: "/work/app")
        let reserved = claim("c2", on: "reserved", kind: .reservation, workspace: "w1",
                             path: "/work/app")
        let claims = Mk.claims(seq: 1, [
            Mk.state("held", claims: [held]),
            Mk.state("reserved", claims: [reserved]),
            Mk.state("theirs", claims: [claim("c3", on: "theirs", kind: .lease, workspace: "w2",
                                              path: "/work/other")]),
        ])
        let devices = [device("held"), device("reserved"), device("theirs"), device("free"),
                       device("unregistered", registered: false)]
        let model = detail(workspace("w1", path: "/work/app"), devices: devices, claims: claims)
        #expect(model.otherDevices.map(\.id) == ["theirs", "free"])
    }

    @Test func theSecretsLine() {
        #expect(detail(workspace("w1", path: "/a")).secrets.stateText == "Not read yet")
        let list = SecretList(project: ProjectInfo(id: "p1", name: "Looky", commonDir: "/c",
                                                  approved: true, createdAtMs: now),
                              secrets: [
                                  SecretInfo(name: "A", declared: true, state: .set),
                                  SecretInfo(name: "B", declared: true, state: .missing),
                                  SecretInfo(name: "C", declared: true, state: .missing),
                              ])
        let read = detail(workspace("w1", path: "/a"), secrets: list, read: true)
        #expect(read.secrets.stateText == "2 missing of 3")
        #expect(read.secrets.missing == 2)
        #expect(detail(workspace("w1", path: "/a"), read: true).secrets.stateText
            == "Could not be read")
        let failed = workspaceDetail(
            of: workspace("w1", path: "/a"), projectName: "Looky", devices: [], claims: nil,
            queue: nil, branches: [:], secrets: nil, secretsRead: true, nowMs: now,
            secretsError: "the secrets database is missing")
        #expect(failed.secrets.stateText == "the secrets database is missing")
        let none = SecretList(project: list.project, secrets: [])
        #expect(detail(workspace("w1", path: "/a"), secrets: none, read: true)
            .secrets.stateText == "None declared")
    }
}
