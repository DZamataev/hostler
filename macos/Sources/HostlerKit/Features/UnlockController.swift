import Foundation
import Observation

/// The unlock form (spec §9.6, plan Task 9): the master password, an optional key file chosen
/// with an open panel, "forget the remembered key file", and the create flow when no database
/// exists (the password twice). It sends `secrets.unlock` as the CLI does; the password goes
/// from the field to the service as a `SecretText` and nowhere else.
///
/// Every outcome clears the fields: the value leaves the field when the call is sent
/// (`SecretEntry.take()`), and a failure leaves nothing typed behind. The service's limit on
/// guessing (`ProofLimit`, shared with the person gate) is honoured: nothing is sent while its
/// wait runs.
@MainActor
@Observable
public final class UnlockController {
    public let password = SecretEntry()
    /// The second entry of the create flow.
    public let confirmation = SecretEntry()
    /// The chosen key file's path; `nil` uses the remembered one, if any.
    public private(set) var keyFile: String?
    public var forgetKeyFile = false
    /// The popover shows the form inline while this is set (it survives the key-file chooser,
    /// which closes the popover).
    public var isFormOpen = false
    /// A call is running (Argon2 takes about 1 s; the bound is the method's 120 s).
    public private(set) var isWorking = false
    public private(set) var error: HostlerAppError?
    public let limit: ProofLimit

    @ObservationIgnored private let client: HostlerClient
    @ObservationIgnored private let openPanel: any OpenPanelPresenting
    @ObservationIgnored private let log: any ValuesLogging

    public init(client: HostlerClient, limit: ProofLimit, openPanel: any OpenPanelPresenting,
                log: any ValuesLogging) {
        self.client = client
        self.limit = limit
        self.openPanel = openPanel
        self.log = log
    }

    /// Asks for a key file; a cancel keeps the earlier choice.
    public func chooseKeyFile() async {
        let prompt = String(localized: "Choose the database's key file")
        if let url = await openPanel.chooseFile(message: prompt) {
            keyFile = url.path
        }
    }

    public func clearKeyFile() {
        keyFile = nil
    }

    /// Unlocks (or, with `create`, creates) the database. `true` on success.
    public func unlock(create: Bool) async -> Bool {
        guard !isWorking else { return false }
        error = nil
        guard !limit.isWaiting else {
            clearFields()
            return false
        }
        guard let typed = password.take() else {
            confirmation.clear()
            error = .emptyValue(field: String(localized: "The master password"))
            return false
        }
        if create {
            let again = confirmation.take()
            guard let again, again == typed else {
                error = .passwordsDiffer
                clearFields()
                return false
            }
        }
        confirmation.clear()
        isWorking = true
        log.event(create ? "unlock: creating the database" : "unlock: unlocking")
        defer { isWorking = false }
        do throws(HostlerCallError) {
            _ = try await client.secretsUnlock(Unlock(password: typed, keyFile: keyFile,
                                                      forgetKeyFile: forgetKeyFile, create: create))
        } catch {
            clearFields()
            log.failure("unlock: failed", code: PersonGate.code(error))
            if !limit.record(error) { self.error = HostlerAppError(call: error) }
            return false
        }
        log.event("unlock: done")
        clearFields()
        keyFile = nil
        forgetKeyFile = false
        isFormOpen = false
        return true
    }

    /// Locks the database (the popover asked for confirmation first). `true` on success.
    public func lock() async -> Bool {
        error = nil
        do throws(HostlerCallError) {
            _ = try await client.secretsLock()
            log.event("lock: done")
            return true
        } catch {
            log.failure("lock: failed", code: PersonGate.code(error))
            self.error = HostlerAppError(call: error)
            return false
        }
    }

    /// The surface closed: nothing typed stays, and the last error goes. The key-file choice
    /// and the open form stay (the chooser closes the popover on its way).
    public func reset() {
        clearFields()
        error = nil
    }

    private func clearFields() {
        password.clear()
        confirmation.clear()
    }
}
