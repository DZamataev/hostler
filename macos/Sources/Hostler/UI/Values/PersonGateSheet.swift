import HostlerKit
import SwiftUI

/// The person gate's form (spec §8.5, §9.6): what it is for, the explanation, the master
/// password, the service's message or the limit's countdown, Cancel and Confirm. Inline in the
/// popover (a sheet there would be another window, and a click in it closes the popover); a
/// sheet in the window (`.personGateSheet(_:)`).
struct PersonGateForm: View {
    let gate: PersonGate

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(gate.purpose ?? String(localized: "Confirm it is you"))
                .font(.headline)
            Text(PersonGate.explanation)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecretEntryField(title: String(localized: "Master password"), entry: gate.password,
                             onSubmit: gate.submit)
                .disabled(gate.phase == .proving)
            ValueErrorLines(message: gate.message, recovery: gate.recovery)
            ProofWaitLine(limit: gate.limit)
            HStack {
                if gate.phase == .proving {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel(String(localized: "Checking the password"))
                }
                Spacer()
                Button(String(localized: "Cancel"), action: gate.cancel)
                    .keyboardShortcut(.cancelAction)
                    .accessibilityLabel(String(localized: "Cancel"))
                TimelineView(.periodic(from: .now, by: 1)) { _ in
                    Button(String(localized: "Confirm"), action: gate.submit)
                        .keyboardShortcut(.defaultAction)
                        .disabled(gate.phase != .asking || gate.password.isEmpty || gate.limit.isWaiting)
                        .accessibilityLabel(String(localized: "Confirm with the master password"))
                }
            }
        }
    }
}

extension View {
    /// The person gate as a sheet of this window, while `gate` asks for the window.
    func personGateSheet(_ gate: PersonGate) -> some View {
        sheet(isPresented: Binding(
            get: { gate.isAsking && gate.presenter == .window },
            set: { shown in if !shown { gate.cancel() } })
        ) {
            PersonGateForm(gate: gate)
                .padding(16)
                .frame(width: 360)
        }
    }
}
