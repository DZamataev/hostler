import Foundation
import Testing
@testable import HostlerKit

/// The Projects section (spec §9.3; plan Task 11): the rows with their approval state and
/// workspaces, the approval itself, and the doctor report's presentation from Task 3's fixture.
@Suite(.timeLimit(.minutes(1))) @MainActor struct ProjectsActionsTests {
    private func snapshot() -> ProjectsSnapshot {
        Mk.projects(seq: 1, [
            Mk.projectView("p-1", name: "app", approved: true,
                           workspaces: [Mk.workspace("w-1", project: "p-1", path: "/work/app")],
                           declared: ProjectDecl(
                               secrets: [DeclaredSecret(name: "API_KEY", purpose: "the API")],
                               accounts: [DeclaredAccount(name: "qa-user", purpose: "login",
                                                          fields: ["user"])],
                               apps: DeclaredApps(android: [], ios: []))),
            Mk.projectView("p-2", name: "other", approved: false,
                           commonDir: "/work/other/.git"),
        ])
    }

    @Test func approveSendsTheProjectsOwnReference() async throws {
        let f = WindowFixture()
        let project = projectRows(snapshot())[1]
        #expect(await f.projects.approve(project) == nil)
        guard case .projectApprove(let ref)? = await f.transport.request(.projectApprove) else {
            Issue.record("no project.approve")
            return
        }
        #expect(ref == ProjectRef(commonDir: "/work/other/.git", name: "other"))
    }

    @Test func aRefusedApprovalIsTheAppsError() async {
        let f = WindowFixture()
        let error = HostlerError(code: .badRequest, message: "no such project",
                                 details: .object([:]), recovery: nil)
        await f.transport.fail(.projectApprove, with: .service(error))
        #expect(await f.projects.approve(projectRows(snapshot())[0])
            == .app(.service(message: "no such project", recovery: nil)))
    }

    @Test func theRowsShowApprovalWorkspacesAndDeclarations() {
        let rows = projectRows(snapshot())
        #expect(rows.map(\.name) == ["app", "other"])
        #expect(rows.map(\.approved) == [true, false])
        #expect(rows.map(\.approvalText) == ["approved", "not approved for secrets"])
        #expect(rows[0].path == "/work/app/.git")
        #expect(rows[0].workspaces.map(\.folderName) == ["app"])
        #expect(rows[0].workspaces[0].color == WorkspaceColor(workspaceId: "w-1"))
        #expect(rows[0].declaredSecrets == 1)
        #expect(rows[0].declaredAccounts == 1)
        #expect(rows[1].workspaces.isEmpty)
        #expect(rows[1].declaredSecrets == 0)
    }

    @Test func noSnapshotHasNoRows() {
        #expect(projectRows(nil).isEmpty)
    }

    // MARK: Doctor

    private func report() throws -> DoctorReport {
        try ContractFixtures.decode(DoctorReport.self, "answer.DoctorReport.json")
    }

    @Test func theDoctorReportDecodesFromTheFixtureAndBecomesRows() throws {
        let model = doctorModel(try report())
        #expect(model.rows.map(\.id) == ["service.registered", "secrets.required.API_KEY"])
        #expect(model.rows.map(\.statusText) == ["ok", "problem"])
        #expect(model.rows[0].message == "the service is registered")
        #expect(model.rows[0].fixText == nil)
        #expect(model.rows[0].whoText == nil)
        #expect(model.rows[1].status == .problem)
        #expect(model.rows[1].fixText == "a person sets it")
        #expect(model.rows[1].fixCommand == "hostler secret set API_KEY")
        #expect(model.rows[1].whoText == "a person acts")
        #expect(!model.rows[1].isSafeFix)
        #expect(model.problems == 1)
        #expect(model.warnings == 0)
        #expect(model.summary == "2 checks · 1 problem")
    }

    @Test func theSummaryCountsEachStatus() {
        let checks = [
            Check(id: "a", status: .ok, message: "fine", details: .object([:])),
            Check(id: "b", status: .warn, message: "look", details: .object([:])),
            Check(id: "c", status: .skipped, message: "later", details: .object([:])),
            Check(id: "d", status: .problem, message: "broken",
                  fix: Fix(command: "hostler doctor --fix", text: "run the fix", who: .agent,
                           safe: true), details: .object([:])),
        ]
        let model = doctorModel(DoctorReport(checks: checks, fixed: []))
        #expect(model.summary == "4 checks · 1 problem · 1 warning · 1 skipped")
        #expect(model.rows[3].whoText == "an agent can fix it")
        #expect(model.rows[3].isSafeFix)
        #expect(model.rows[1].statusText == "warning")
    }
}
