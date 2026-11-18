import Foundation
import Testing
@testable import HostlerKit

@Suite struct HostlerAppErrorTests {
    @Test func aServiceErrorShowsTheServicesOwnMessageAndRecovery() {
        let error = HostlerAppError.service(message: "the database is locked",
                                            recovery: "run `hostler unlock`")
        #expect(error.errorDescription == "the database is locked")
        #expect(error.recoverySuggestion == "run `hostler unlock`")
        #expect(error.localizedDescription == "the database is locked")
    }
}

/// Task 13 F-5: inside the app, a service error whose recovery is a CLI step the app itself
/// performs (`hostler unlock`) is shown in the app's words, with no CLI line.
@Suite struct InAppWordingTests {
    static func service(_ code: ErrorCode, _ message: String, details: JSONValue = .object([:]),
                        recovery: String? = "hostler unlock") -> HostlerAppError {
        HostlerAppError(call: .service(HostlerError(code: code, message: message, details: details,
                                                    recovery: recovery)))
    }

    @Test func aLockedDatabaseSaysToUnlockInTheMenuBarPanel() {
        let error = Self.service(.secretsLocked,
            "the hostler secrets database is locked; a person runs `hostler unlock` in a terminal (agents ask the operator)",
            details: .object(["state": .string("locked")]))
        #expect(error == .service(
            message: "The secrets database is locked. Unlock it in hostler's menu bar panel.",
            recovery: nil))
    }

    @Test func aMissingDatabaseSaysToCreateOneInTheMenuBarPanel() {
        let error = Self.service(.secretsLocked, "there is no hostler secrets database yet; …",
                                 details: .object(["state": .string("missing")]))
        #expect(error == .service(
            message: "No secrets database exists yet. Create one in hostler's menu bar panel.",
            recovery: nil))
    }

    @Test func aWrongPasswordHasNoCLILine() {
        let error = Self.service(.unlockFailed, "wrong master password or key file")
        #expect(error == .service(message: "Wrong master password or key file.",
                                  recovery: "Check the password and the key file, then try again."))
    }

    @Test func anyOtherUnlockRecoveryIsTheMenuBarPanel() {
        let error = Self.service(.secretsConflict, "hostler.kdbx was changed outside hostler …")
        #expect(error == .service(message: "hostler.kdbx was changed outside hostler …",
                                  recovery: "Unlock it in hostler's menu bar panel."))
    }

    @Test func otherRecoveriesStayAsTheServiceWroteThem() {
        let error = Self.service(.projectNotApproved, "not approved", recovery: "hostler project approve")
        #expect(error == .service(message: "not approved", recovery: "hostler project approve"))
    }

    @Test func noTextTheAppShowsNamesTheCLIsUnlock() {
        for error in [
            Self.service(.secretsLocked, "locked; a person runs `hostler unlock`"),
            Self.service(.unlockFailed, "wrong master password or key file"),
            Self.service(.secretsConflict, "changed"),
        ] {
            #expect(!(error.errorDescription ?? "").contains("hostler unlock"))
            #expect(!(error.recoverySuggestion ?? "").contains("hostler unlock"))
        }
    }
}
