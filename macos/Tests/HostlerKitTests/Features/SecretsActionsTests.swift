import Foundation
import Testing
@testable import HostlerKit

/// The Secrets and Accounts sections' actions (spec §9.3, §9.6; plan Task 11): what each sends,
/// that a value only ever travels as a `SecretText` taken from a `SecretEntry`, and that the
/// preserved files are deleted only behind the gate and only for a path the service listed.
@Suite(.timeLimit(.minutes(1))) @MainActor struct SecretsActionsTests {
    private func project(declared: ProjectDecl? = nil) -> ProjectView {
        Mk.projectView("p-1", name: "app", commonDir: "/work/app/.git", declared: declared)
    }

    private func ref() -> ProjectRef {
        ProjectRef(commonDir: "/work/app/.git", name: "app")
    }

    private func typed(_ text: String) -> SecretEntry {
        let entry = SecretEntry()
        entry.text = text
        return entry
    }

    // MARK: Secrets

    @Test func setSendsTheProjectTheNameAndTheTextAndClearsTheField() async throws {
        let f = WindowFixture()
        let entry = typed("sk-test-123")
        #expect(await f.secrets().setSecret(project(), name: "API_KEY", entry: entry) == nil)
        guard case .secretSet(let set)? = await f.transport.request(.secretSet) else {
            Issue.record("no secret.set")
            return
        }
        #expect(set.project == ref())
        #expect(set.name == "API_KEY")
        // The value is a `SecretText` on the wire, and the field is empty again.
        #expect(set.value == SecretText("sk-test-123"))
        #expect(entry.isEmpty)
    }

    @Test func anEmptyValueSendsNothing() async {
        let f = WindowFixture()
        let entry = typed("")
        #expect(await f.secrets().setSecret(project(), name: "API_KEY", entry: entry)
            == .app(.emptyValue(field: "API_KEY")))
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func removeSendsTheProjectAndTheName() async throws {
        let f = WindowFixture()
        #expect(await f.secrets().removeSecret(project(), name: "OLD_KEY") == nil)
        guard case .secretRemove(let project, let name)? = await f.transport.request(.secretRemove)
        else { Issue.record("no secret.remove"); return }
        #expect(project == ref())
        #expect(name == "OLD_KEY")
    }

    @Test func shareSendsTheTargetProjectId() async throws {
        let f = WindowFixture()
        #expect(await f.secrets().share(project(), name: "API_KEY", toProjectId: "p-2") == nil)
        guard case .secretShare(let share)? = await f.transport.request(.secretShare) else {
            Issue.record("no secret.share")
            return
        }
        #expect(share.from == ref())
        #expect(share.name == "API_KEY")
        #expect(share.toProjectId == "p-2")
    }

    @Test func unshareNamesTheProjectTheSecretComesFrom() async throws {
        let f = WindowFixture()
        let source = Mk.projectView("p-2", name: "shared", commonDir: "/work/shared/.git")
        #expect(await f.secrets().unshare(from: source, name: "API_KEY", toProjectId: "p-1")
            == nil)
        guard case .secretUnshare(let share)? = await f.transport.request(.secretUnshare) else {
            Issue.record("no secret.unshare")
            return
        }
        #expect(share.from == ProjectRef(commonDir: "/work/shared/.git", name: "shared"))
        #expect(share.toProjectId == "p-1")
    }

    @Test func aServiceFailureIsTheAppsError() async {
        let f = WindowFixture()
        let error = HostlerError(code: .secretMissing, message: "no such secret",
                                 details: .object([:]), recovery: nil)
        await f.transport.fail(.secretRemove, with: .service(error))
        #expect(await f.secrets().removeSecret(project(), name: "API_KEY")
            == .app(.service(message: "no such secret", recovery: nil)))
    }

    // MARK: Preserved files

    @Test func aListedPreservedFileIsDeletedBehindTheGate() async throws {
        let f = WindowFixture()
        let path = "/db/hostler.kdbx.prev-2026-11-01"
        await f.transport.set { $0.preserved = [path] }
        async let deleting = f.secrets(preservedFiles: [path]).removePreserved(path)
        try await f.answer("correct horse")
        guard case .done(let status) = await deleting else { Issue.record("not deleted"); return }
        #expect(status.preservedFiles.isEmpty)
        #expect(await f.transport.methods == [.secretsProve, .secretsRemovePreserved])
        guard case .secretsRemovePreserved(let sent)? =
            await f.transport.request(.secretsRemovePreserved)
        else { Issue.record("no secrets.remove_preserved"); return }
        #expect(sent == path)
    }

    @Test func aPathThatWasNotListedSendsNothing() async {
        let f = WindowFixture()
        let outcome = await f.secrets(preservedFiles: ["/db/hostler.kdbx.prev-2026-11-01"])
            .removePreserved("/db/another.kdbx.prev-2026-11-01")
        guard case .failed(let failure) = outcome else { Issue.record("\(outcome)"); return }
        #expect(failure == .app(.unknownPreservedFile("/db/another.kdbx.prev-2026-11-01")))
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func cancellingTheGateDeletesNothing() async throws {
        let f = WindowFixture()
        let path = "/db/hostler.kdbx.prev-2026-11-01"
        async let deleting = f.secrets(preservedFiles: [path]).removePreserved(path)
        try await storeSoon("the gate asks") { f.gate.phase == .asking }
        f.gate.cancel()
        guard case .cancelled = await deleting else { Issue.record("not cancelled"); return }
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func aLiveGrantDeletesWithoutAsking() async {
        let f = WindowFixture()
        f.grantLive()
        let path = "/db/hostler.kdbx.prev-2026-11-01"
        await f.transport.set { $0.preserved = [path] }
        let outcome = await f.secrets(preservedFiles: [path]).removePreserved(path)
        guard case .done = outcome else { Issue.record("\(outcome)"); return }
        #expect(await f.transport.methods == [.secretsRemovePreserved])
    }

    // MARK: Accounts

    @Test func accountsReadTheProjectsStoredOnes() async {
        let f = WindowFixture()
        await f.transport.set {
            $0.accounts = ["app": [Mk.account("qa-user", fields: ["session"])]]
        }
        let result = await f.secrets().accounts(of: project())
        guard case .success(let list) = result else { Issue.record("\(result)"); return }
        #expect(list.map(\.name) == ["qa-user"])
        guard case .accountList(let sent)? = await f.transport.request(.accountList) else {
            Issue.record("no account.list"); return
        }
        #expect(sent == ref())
    }

    @Test func aFieldIsSetAsASecretTextAndTheFieldIsCleared() async throws {
        let f = WindowFixture()
        let entry = typed("session-cookie")
        #expect(await f.secrets().setAccountField(project(), account: "qa-user",
                                                  field: "session", entry: entry) == nil)
        guard case .accountSet(let set)? = await f.transport.request(.accountSet) else {
            Issue.record("no account.set")
            return
        }
        #expect(set.project == ref())
        #expect(set.name == "qa-user")
        #expect(set.field == "session")
        #expect(set.value == SecretText("session-cookie"))
        #expect(entry.isEmpty)
    }

    @Test func anEmptyFieldSendsNothing() async {
        let f = WindowFixture()
        #expect(await f.secrets().setAccountField(project(), account: "qa-user", field: "session",
                                                  entry: typed(""))
            == .app(.emptyValue(field: "session")))
        #expect(await f.secrets().setAccountField(project(), account: "qa-user", field: "  ",
                                                  entry: typed("x"))
            == .app(.emptyValue(field: "the field")))
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func removingAFieldIsAnAccountRemoveWithTheField() async throws {
        let f = WindowFixture()
        #expect(await f.secrets().removeAccount(project(), name: "qa-user", field: "session")
            == nil)
        guard case .accountRemove(let remove)? = await f.transport.request(.accountRemove) else {
            Issue.record("no account.remove")
            return
        }
        #expect(remove.field == "session")
    }

    @Test func removingTheWholeAccountLeavesTheFieldNil() async throws {
        let f = WindowFixture()
        #expect(await f.secrets().removeAccount(project(), name: "qa-user") == nil)
        guard case .accountRemove(let remove)? = await f.transport.request(.accountRemove) else {
            Issue.record("no account.remove")
            return
        }
        #expect(remove.field == nil)
    }
}

/// The Secrets section's rows (spec §9.3): the declared and stored secrets merged, set or
/// missing, and where a shared one comes from.
@Suite struct SecretRowsTests {
    private func list(_ secrets: [SecretInfo]) -> SecretList {
        SecretList(project: Mk.projectView("p-1").info, secrets: secrets)
    }

    @Test func theRowsKeepTheServicesOrderAndSaySetOrMissing() {
        let rows = secretRows(list([
            Mk.secretInfo("API_KEY", purpose: "the API", declared: true, state: .set),
            Mk.secretInfo("DB_URL", purpose: "the database", declared: true, state: .missing),
            Mk.secretInfo("OLD_TOKEN", declared: false, state: .set),
        ]))
        #expect(rows.map(\.name) == ["API_KEY", "DB_URL", "OLD_TOKEN"])
        #expect(rows.map(\.stateText) == ["set", "missing", "set"])
        #expect(rows.map(\.declared) == [true, true, false])
        #expect(rows[0].purpose == "the API")
    }

    @Test func aSharedOneSaysWhereItComesFrom() {
        let rows = secretRows(list([
            Mk.secretInfo("SHARED_KEY", declared: true, state: .set,
                          source: .sharedFrom(projectId: "p-2", project: "other")),
        ]))
        #expect(rows.first?.sharedFrom == "other")
        #expect(rows.first?.sharedFromProjectId == "p-2")
    }

    @Test func anUnreadListHasNoRows() {
        #expect(secretRows(nil).isEmpty)
    }
}

/// The Accounts section's rows (spec §9.3): the declared accounts and the stored ones merged,
/// with each field's state and the holder.
@Suite struct AccountRowsTests {
    private func declared() -> [DeclaredAccount] {
        [DeclaredAccount(name: "qa-user", purpose: "the test login",
                         fields: ["user", "password"]),
         DeclaredAccount(name: "admin", purpose: "the admin console", fields: ["session"])]
    }

    @Test func theDeclaredAccountsComeFirstThenTheStoredOnes() {
        let rows = accountRows(declared: declared(),
                               stored: [Mk.account("qa-user", fields: ["user"]),
                                        Mk.account("legacy", fields: ["cookie"])])
        #expect(rows.map(\.name) == ["qa-user", "admin", "legacy"])
        #expect(rows.map(\.declared) == [true, true, false])
        #expect(rows[0].purpose == "the test login")
        #expect(rows[2].purpose == nil)
    }

    @Test func theFieldsAreDeclaredFirstThenStoredAndSayWhichAreSet() {
        let rows = accountRows(declared: declared(),
                               stored: [Mk.account("qa-user", fields: ["user", "extra"])])
        let qa = rows[0]
        #expect(qa.fields.map(\.name) == ["user", "password", "extra"])
        #expect(qa.fields.map(\.isSet) == [true, false, true])
        #expect(qa.missing == ["password"])
        #expect(qa.stateText == "1 of 3 fields missing")
        #expect(rows[1].missing == ["session"])
        #expect(rows[1].stateText == "1 of 1 fields missing")
    }

    @Test func aCompleteAccountAndOneWithoutFields() {
        let rows = accountRows(declared: [DeclaredAccount(name: "a", purpose: "p",
                                                          fields: ["x"])],
                               stored: [Mk.account("a", fields: ["x"]),
                                        Mk.account("b", fields: [])])
        #expect(rows[0].stateText == "1 field set")
        #expect(rows[0].missing.isEmpty)
        #expect(rows[1].stateText == "no fields")
    }

    @Test func theHolderIsAChipInItsWorkspacesColor() throws {
        let workspaces = [Mk.workspace("w-1", project: "p-1", path: "/work/app")]
        let rows = accountRows(declared: [], stored: [
            Mk.account("qa-user", fields: ["user"], holder: Mk.claimSummary("account:p-1/qa-user")),
        ], workspaces: workspaces)
        let holder = try #require(rows.first?.holderChip)
        #expect(holder.name == "app")
        #expect(holder.color == WorkspaceColor(workspaceId: "w-1"))
        #expect(rows.first?.holder?.kind == .hold)
    }

    @Test func anEmptySnapshotHasNoRows() {
        #expect(accountRows(declared: [], stored: []).isEmpty)
    }
}
