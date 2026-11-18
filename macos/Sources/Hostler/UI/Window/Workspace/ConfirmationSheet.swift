import HostlerKit
import SwiftUI

/// The sheet a takeover or a forced release asks for: the person types the resource's token
/// (the CLI's `--confirmation`), and nothing runs until it matches.
struct ConfirmationSheet: View {
    var title: String
    var explanation: String
    var token: String
    var command: String
    var actionTitle: String
    var perform: (String) async -> ActionFailure?

    @Environment(\.dismiss) private var dismiss
    @State private var typed = ""
    @State private var failure: ActionFailure?
    @State private var running = false

    private var matches: Bool {
        typed.trimmingCharacters(in: .whitespacesAndNewlines) == token
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title)
                .font(.headline)
            Text(explanation)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text("Type \(token) to confirm:")
                .font(.callout)
            TextField(token, text: $typed)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Confirmation")
                .accessibilityIdentifier("confirmation.field")
            Text(command)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .lineLimit(2)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(actionTitle) { submit() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!matches || running)
                    .accessibilityLabel(actionTitle)
            }
        }
        .padding(16)
        .frame(width: 440)
        .actionFailureAlert($failure)
    }

    private func submit() {
        running = true
        Task {
            if let failure = await perform(typed) {
                self.failure = failure
                running = false
            } else {
                dismiss()
            }
        }
    }
}
