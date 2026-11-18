import HostlerKit
import SwiftUI

/// The unlock form, inline in the popover (spec §9.6; plan Task 9). The popover is key, so the
/// `SecureField` takes typing. With no database it is the create form (the password twice).
struct UnlockView: View {
    let unlock: UnlockController
    /// The database is missing: create it.
    let creates: Bool
    /// Runs the key-file chooser (`AppModel.chooseKeyFile()`: it closes the popover first).
    let chooseKeyFile: () -> Void

    var body: some View {
        @Bindable var unlock = unlock
        ValueBox {
            PopoverCaption(text: creates ? String(localized: "Create the secrets database")
                                       : String(localized: "Unlock the secrets database"))
            SecretEntryField(title: String(localized: "Master password"), entry: unlock.password,
                             onSubmit: submit)
            if creates {
                SecretEntryField(title: String(localized: "The same password again"),
                                 entry: unlock.confirmation, onSubmit: submit)
            }
            keyFileRow
            if !creates, unlock.keyFile == nil {
                Toggle(String(localized: "Forget the remembered key file"), isOn: $unlock.forgetKeyFile)
                    .toggleStyle(.checkbox)
                    .font(.caption)
                    .accessibilityLabel(String(localized: "Forget the remembered key file"))
            }
            ValueErrorLines(unlock.error)
            ProofWaitLine(limit: unlock.limit)
            HStack {
                if unlock.isWorking {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel(String(localized: "Checking the password"))
                }
                Spacer()
                Button(String(localized: "Cancel")) {
                    unlock.reset()
                    unlock.isFormOpen = false
                }
                .accessibilityLabel(String(localized: "Cancel unlocking"))
                Button(creates ? String(localized: "Create") : String(localized: "Unlock"), action: submit)
                    .keyboardShortcut(.defaultAction)
                    .disabled(unlock.isWorking || unlock.password.isEmpty)
                    .accessibilityLabel(creates ? String(localized: "Create the database")
                                                : String(localized: "Unlock the database"))
            }
        }
    }

    private var keyFileRow: some View {
        HStack(spacing: 6) {
            Text(unlock.keyFile.map { String(localized: "Key file: \(($0 as NSString).lastPathComponent)") }
                 ?? String(localized: "No key file chosen"))
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .help(unlock.keyFile ?? "")
            Spacer(minLength: 4)
            Button(String(localized: "Choose…"), action: chooseKeyFile)
                .controlSize(.small)
                .accessibilityLabel(String(localized: "Choose a key file"))
            if unlock.keyFile != nil {
                Button(String(localized: "Clear")) { unlock.clearKeyFile() }
                    .controlSize(.small)
                    .accessibilityLabel(String(localized: "Clear the key file"))
            }
        }
    }

    private func submit() {
        Task { _ = await unlock.unlock(create: creates) }
    }
}
