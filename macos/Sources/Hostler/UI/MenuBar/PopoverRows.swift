import AppKit
import HostlerKit
import SwiftUI

/// The popover's command row (the reference app's `PopoverCommandRow`): a fixed icon column, the
/// label, an optional shortcut; hover paints the system's selected-content background.
struct PopoverCommandRow: View {
    let icon: Icon
    let title: String
    var shortcut: String?
    var isEnabled = true
    let action: () -> Void

    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 7) {
                icon.image
                    .frame(width: 16)
                    .foregroundStyle(foreground)
                Text(title)
                    .lineLimit(1)
                    .foregroundStyle(foreground)
                Spacer(minLength: 6)
                if let shortcut {
                    Text(shortcut)
                        .font(.caption)
                        .foregroundStyle(isHovering
                                         ? Color(nsColor: .selectedMenuItemTextColor).opacity(0.8)
                                         : .secondary)
                }
            }
            .padding(.horizontal, 6)
            .frame(minHeight: 24)
            .opacity(isEnabled ? 1 : 0.45)
            .contentShape(Rectangle())
        }
        .buttonStyle(PopoverRowButtonStyle(isHovering: isHovering))
        .onHover { isHovering = isEnabled && $0 }
        .disabled(!isEnabled)
        .accessibilityLabel(title)
    }

    private var foreground: Color {
        isHovering ? Color(nsColor: .selectedMenuItemTextColor) : .primary
    }
}

private struct PopoverRowButtonStyle: ButtonStyle {
    let isHovering: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(RoundedRectangle(cornerRadius: 5)
                .fill(isHovering ? Color(nsColor: .selectedContentBackgroundColor) : .clear))
    }
}

/// A section's small caption (the reference app's `PopoverCaption`).
struct PopoverCaption: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
    }
}

/// A rounded box for a card or a form inside the popover.
struct ValueBox<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 6) { content }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .quaternaryLabelColor).opacity(0.35)))
    }
}

/// A failure line: the message, and the recovery under it.
struct ValueErrorLines: View {
    let message: String?
    let recovery: String?

    init(_ error: HostlerAppError?) {
        message = error?.errorDescription
        recovery = error?.recoverySuggestion
    }

    init(message: String?, recovery: String?) {
        self.message = message
        self.recovery = recovery
    }

    var body: some View {
        if let message {
            VStack(alignment: .leading, spacing: 2) {
                Text(message).foregroundStyle(.red)
                if let recovery { Text(recovery).foregroundStyle(.secondary) }
            }
            .font(.caption)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityElement(children: .combine)
        }
    }
}

/// The limit's countdown (spec §8.5's text), refreshed every second while it runs.
struct ProofWaitLine: View {
    let limit: ProofLimit

    var body: some View {
        if limit.retryAt != nil {
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                if let text = limit.waitText {
                    Text(text)
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

/// A `SecureField` for a `SecretEntry`, labelled for accessibility (its value reads as "").
struct SecretEntryField: View {
    let title: String
    let entry: SecretEntry
    var onSubmit: () -> Void = {}

    var body: some View {
        @Bindable var entry = entry
        SecureField(title, text: $entry.text)
            .textFieldStyle(.roundedBorder)
            .onSubmit(onSubmit)
            .accessibilityLabel(title)
    }
}
