import HostlerKit
import SwiftUI

/// The sheet that sets one secret's value (`secret.set`) or one account field's (`account.set`),
/// spec §9.6: a `SecureField` only, and the value leaves it as a `SecretText` when the sheet
/// sends it. What is typed is never shown again, not even back in the field.
struct SetValueSheet: View {
    var title: String
    /// The line under the title ("Project app · account qa-user").
    var subtitle: String
    /// The fields an account offers: one for the field it was opened for, none (an empty list)
    /// to type the name of a new one; `nil` for a secret's single value.
    var fields: [String]?
    /// The command the action has on the command line, for the sheet's last line.
    var command: String
    /// Sends the field (`nil` for a secret) and the typed value; `nil` closes the sheet.
    var perform: (String?, SecretEntry) async -> ActionFailure?

    @Environment(\.dismiss) private var dismiss
    @State private var entry = SecretEntry()
    @State private var field = ""
    @State private var failure: ActionFailure?
    @State private var saving = false

    /// The field the sheet submits: the one it was opened for, or the name typed for an account
    /// with no known field.
    private var chosenField: String {
        fields?.first ?? field.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSave: Bool {
        guard !saving, !entry.isEmpty else { return false }
        if let fields, fields.isEmpty { return !chosenField.isEmpty }
        return true
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.headline)
            Text(subtitle)
                .font(.callout)
                .foregroundStyle(.secondary)
            if let fields {
                if fields.isEmpty {
                    // An account with no known field: the person names the new one (fix r1-3).
                    TextField(String(localized: "Field"), text: $field)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityLabel("Account field")
                        .accessibilityIdentifier("setvalue.field")
                } else if fields.count == 1 {
                    HStack(spacing: 6) {
                        Text("Field").secondaryCaption()
                        Text(fields[0])
                            .font(.callout)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("Field \(fields[0])")
                } else {
                    Picker(String(localized: "Field"), selection: $field) {
                        ForEach(fields, id: \.self) { name in
                            Text(name).tag(name)
                        }
                    }
                    .accessibilityLabel("Account field")
                }
            }
            SecretEntryField(title: fields == nil || chosenField.isEmpty
                ? String(localized: "Value")
                : String(localized: "Value of \(chosenField)"), entry: entry,
                onSubmit: save)
                .disabled(saving)
            Text(command)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .lineLimit(2)
            HStack {
                if saving {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Saving")
                }
                Spacer()
                Button("Cancel", role: .cancel) { close() }
                    .keyboardShortcut(.cancelAction)
                Button("Save", action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canSave)
                    .accessibilityLabel("Save")
                    .accessibilityIdentifier("setvalue.save")
            }
        }
        .padding(16)
        .frame(width: 420)
        .onAppear { field = fields?.first ?? "" }
        .actionFailureAlert($failure)
    }

    private func save() {
        guard !saving else { return }
        saving = true
        let field = fields.map { _ in chosenField }
        Task {
            let failure = await perform(field, entry)
            saving = false
            if let failure {
                self.failure = failure
            } else {
                close()
            }
        }
    }

    private func close() {
        entry.clear()
        dismiss()
    }
}
