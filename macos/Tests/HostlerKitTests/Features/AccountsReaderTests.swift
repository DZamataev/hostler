import Foundation
import Testing
@testable import HostlerKit

/// The Accounts section's per-project reads (fix r1-2): each read is keyed by its project id and
/// discarded when the person switched away from that project or started a newer read of it, so
/// one project's rows and actions never show under another's.
@Suite(.timeLimit(.minutes(1))) @MainActor struct AccountsReaderTests {
    private func project(_ id: String, name: String) -> ProjectView {
        Mk.projectView(id, name: name, commonDir: "/work/\(name)/.git")
    }

    /// The scope the app would report: connection 1, instance "A", each project as built here.
    final class ScopeBox {
        var connection: ConnectionToken? = 1
        var instance: String? = "A"
        var projects: [String: (commonDir: String, name: String)] = [
            "p-1": ("/work/app/.git", "app"), "p-2": ("/work/other/.git", "other"),
        ]

        func scope(_ projectId: String) -> AccountReadScope? {
            guard let connection, let project = projects[projectId] else { return nil }
            return AccountReadScope(connection: connection, instance: instance, projectId: projectId,
                                    commonDir: project.commonDir, name: project.name)
        }
    }

    private func reader(_ f: WindowFixture, _ box: ScopeBox = ScopeBox()) -> AccountsReader {
        AccountsReader(client: f.client, scope: { box.scope($0) })
    }

    private func gated(_ f: WindowFixture) async {
        await f.transport.set { $0.gateAccounts = true }
    }

    @Test func aSelectionWaitsForItsOwnRead() {
        let f = WindowFixture()
        let reader = reader(f)
        #expect(reader.state(for: nil) == nil)
        reader.select("p-1")
        #expect(reader.selectedProjectId == "p-1")
        #expect(reader.state(for: "p-1") == .reading)
        #expect(!reader.isLoaded("p-1"))
    }

    @Test func theReadOfTheProjectThePersonSwitchedAwayFromIsDiscarded() async throws {
        let f = WindowFixture()
        await gated(f)
        let a = project("p-1", name: "app")
        let b = project("p-2", name: "other")
        let reader = reader(f)

        reader.select("p-1")
        async let readA: Void = reader.load(a)
        try await storeSoon("the first read starts") { await f.transport.waitingAccountReads == 1 }

        reader.select("p-2")
        async let readB: Void = reader.load(b)
        try await storeSoon("the second read starts") { await f.transport.waitingAccountReads == 2 }

        // B's answer first: the rows the person now sees.
        await f.transport.answerAccountRead(1, with: .success([Mk.account("b-user",
                                                                        fields: ["session"])]))
        _ = await readB
        #expect(reader.state(for: "p-2") == .loaded([Mk.account("b-user", fields: ["session"])]))
        #expect(reader.isLoaded("p-2"))

        // A's answer arrives late, for a project that is no longer selected: discarded.
        await f.transport.answerAccountRead(0, with: .success([Mk.account("a-user",
                                                                        fields: ["user"])]))
        _ = await readA
        #expect(reader.state(for: "p-1") != .loaded([Mk.account("a-user", fields: ["user"])]))
        #expect(reader.state(for: "p-1") == .reading)
        #expect(reader.state(for: "p-2") == .loaded([Mk.account("b-user", fields: ["session"])]))
        #expect(reader.selectedProjectId == "p-2")
    }

    @Test func aNewerReadOfTheSameProjectWins() async throws {
        let f = WindowFixture()
        await gated(f)
        let a = project("p-1", name: "app")
        let reader = reader(f)
        reader.select("p-1")

        async let first: Void = reader.load(a)
        try await storeSoon("the first read starts") { await f.transport.waitingAccountReads == 1 }
        async let second: Void = reader.load(a)
        try await storeSoon("the second read starts") { await f.transport.waitingAccountReads == 2 }

        await f.transport.answerAccountRead(1, with: .success([Mk.account("new", fields: ["x"])]))
        _ = await second
        #expect(reader.state(for: "p-1") == .loaded([Mk.account("new", fields: ["x"])]))

        // The older read of the same project must not overwrite the newer one.
        await f.transport.answerAccountRead(0, with: .success([Mk.account("old", fields: ["y"])]))
        _ = await first
        #expect(reader.state(for: "p-1") == .loaded([Mk.account("new", fields: ["x"])]))
    }

    @Test func aFailedReadIsFailedAndEnablesNoAction() async throws {
        let f = WindowFixture()
        let error = HostlerError(code: .projectNotApproved, message: "not approved for secrets",
                                 details: .object([:]), recovery: nil)
        await f.transport.fail(.accountList, with: .service(error))
        let reader = reader(f)
        reader.select("p-1")
        await reader.load(project("p-1", name: "app"))
        #expect(reader.state(for: "p-1")
            == .failed(.service(message: "not approved for secrets", recovery: nil)))
        #expect(!reader.isLoaded("p-1"))
    }

    @Test func aLoadedReadCarriesTheStoredAccounts() async throws {
        let f = WindowFixture()
        await f.transport.set {
            $0.accounts = ["app": [Mk.account("qa-user", fields: ["user"])]]
        }
        let reader = reader(f)
        reader.select("p-1")
        await reader.load(project("p-1", name: "app"))
        #expect(reader.state(for: "p-1") == .loaded([Mk.account("qa-user", fields: ["user"])]))
        #expect(reader.state(for: "p-1")?.accounts == [Mk.account("qa-user", fields: ["user"])])
        // Another project is still unread: its rows wait.
        #expect(reader.state(for: "p-2") == nil)
        #expect(!reader.isLoaded("p-2"))
        reader.forget("p-1")
        #expect(reader.state(for: "p-1") == nil)
    }

    // MARK: Clean read C-B1: a read is bound to its connection, instance and project identity

    @Test func anAnswerFromBeforeAReconnectIsDiscarded() async throws {
        let f = WindowFixture()
        await gated(f)
        let box = ScopeBox()
        let reader = reader(f, box)
        reader.select("p-1")
        async let old: Void = reader.load(project("p-1", name: "app"))
        try await storeSoon("the read starts") { await f.transport.waitingAccountReads == 1 }

        // The service restarted: a new connection and a new instance.
        box.connection = 2
        box.instance = "B"
        await f.transport.answerAccountRead(0, with: .success([Mk.account("old-service", fields: ["x"])]))
        _ = await old
        #expect(reader.state(for: "p-1") == .reading)
        #expect(!reader.isLoaded("p-1"))

        // The read the new scope starts is committed.
        await f.transport.set { $0.gateAccounts = false }
        await f.transport.set { $0.accounts = ["app": [Mk.account("new-service", fields: ["y"])]] }
        await reader.load(project("p-1", name: "app"))
        #expect(reader.state(for: "p-1") == .loaded([Mk.account("new-service", fields: ["y"])]))
    }

    @Test func anAnswerFromBeforeAnInstanceChangeOnTheSameConnectionIsDiscarded() async throws {
        let f = WindowFixture()
        await gated(f)
        let box = ScopeBox()
        let reader = reader(f, box)
        reader.select("p-1")
        async let old: Void = reader.load(project("p-1", name: "app"))
        try await storeSoon("the read starts") { await f.transport.waitingAccountReads == 1 }
        box.instance = "B"
        await f.transport.answerAccountRead(0, with: .success([Mk.account("stale", fields: ["x"])]))
        _ = await old
        #expect(reader.state(for: "p-1") == .reading)
    }

    @Test func anAnswerForAReplacedProjectWithTheSameIdIsDiscarded() async throws {
        let f = WindowFixture()
        await gated(f)
        let box = ScopeBox()
        let reader = reader(f, box)
        reader.select("p-1")
        async let old: Void = reader.load(project("p-1", name: "app"))
        try await storeSoon("the read starts") { await f.transport.waitingAccountReads == 1 }

        // The snapshot now holds another project under the same id.
        box.projects["p-1"] = ("/work/elsewhere/.git", "elsewhere")
        await f.transport.answerAccountRead(0, with: .success([Mk.account("old-project", fields: ["x"])]))
        _ = await old
        #expect(reader.state(for: "p-1") == .reading)
    }

    @Test func noReadIsSentWithoutALiveScopeOrForAProjectTheSnapshotNoLongerHolds() async throws {
        let f = WindowFixture()
        let box = ScopeBox()
        box.connection = nil
        let reader = reader(f, box)
        reader.select("p-1")
        await reader.load(project("p-1", name: "app"))
        #expect(reader.state(for: "p-1") == .reading)
        box.connection = 1
        // The view passed a project whose identity is not the snapshot's any more.
        await reader.load(project("p-1", name: "renamed"))
        #expect(reader.state(for: "p-1") == .reading)
        #expect(await f.transport.count(.accountList) == 0)
    }
}

/// The set-field sheet's arguments (fix r1-3): an existing field is locked to itself; an account
/// with no known field asks the person for the name, and `account.set` carries what they typed.
@Suite(.timeLimit(.minutes(1))) @MainActor struct AccountFieldSheetTests {
    private func project() -> ProjectView {
        Mk.projectView("p-1", name: "app", commonDir: "/work/app/.git")
    }

    private func typed(_ text: String) -> SecretEntry {
        let entry = SecretEntry()
        entry.text = text
        return entry
    }

    @Test func anExistingFieldIsLockedToItself() {
        let sheet = AccountFieldSheet(account: "qa-user", existingField: "session")
        #expect(sheet.fields == ["session"])
        #expect(!sheet.needsTypedName)
        #expect(sheet.resolvedField(submitted: "session") == "session")
        // Whatever the sheet reports, the locked field is what is sent.
        #expect(sheet.resolvedField(submitted: "another") == "session")
    }

    @Test func anAccountWithoutFieldsAsksForTheName() {
        let sheet = AccountFieldSheet(account: "legacy", existingField: nil)
        #expect(sheet.fields.isEmpty)
        #expect(sheet.needsTypedName)
        #expect(sheet.resolvedField(submitted: " session ") == "session")
        #expect(sheet.resolvedField(submitted: "  ") == nil)
        #expect(sheet.resolvedField(submitted: nil) == nil)
    }

    @Test func theTypedFieldReachesAccountSet() async throws {
        let f = WindowFixture()
        let sheet = AccountFieldSheet(account: "legacy", existingField: nil)
        let entry = typed("session-cookie")
        #expect(await f.secrets().setAccountField(project(), account: "legacy", sheet: sheet,
                                                  submitted: "session", entry: entry) == nil)
        guard case .accountSet(let set)? = await f.transport.request(.accountSet) else {
            Issue.record("no account.set")
            return
        }
        #expect(set.name == "legacy")
        #expect(set.field == "session")
        #expect(set.value == SecretText("session-cookie"))
        #expect(entry.isEmpty)
    }

    @Test func anEmptyTypedFieldSendsNothing() async {
        let f = WindowFixture()
        let sheet = AccountFieldSheet(account: "legacy", existingField: nil)
        let entry = typed("v")
        #expect(await f.secrets().setAccountField(project(), account: "legacy", sheet: sheet,
                                                  submitted: "   ", entry: entry)
            == .app(.emptyValue(field: "the field")))
        #expect(entry.isEmpty)
        #expect(await f.transport.calls.isEmpty)
    }

    @Test func aLockedFieldSendsItsOwnNameEvenWhenTheSheetReportsAnother() async throws {
        let f = WindowFixture()
        let sheet = AccountFieldSheet(account: "qa-user", existingField: "session")
        #expect(await f.secrets().setAccountField(project(), account: "qa-user", sheet: sheet,
                                                  submitted: "other", entry: typed("v")) == nil)
        guard case .accountSet(let set)? = await f.transport.request(.accountSet) else {
            Issue.record("no account.set")
            return
        }
        #expect(set.field == "session")
    }
}
