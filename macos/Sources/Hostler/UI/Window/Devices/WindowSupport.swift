import AppKit
import HostlerKit
import SwiftUI

// Small pieces every window view shares (plan Task 10): the failure alert, the state dot, the
// rounded grouped box, and a label/value row.

/// Shows an action's failure as an alert with what the person can do about it.
extension View {
    func actionFailureAlert(_ failure: Binding<ActionFailure?>) -> some View {
        alert(
            "hostler",
            isPresented: Binding(
                get: { failure.wrappedValue != nil },
                set: { if !$0 { failure.wrappedValue = nil } }),
            presenting: failure.wrappedValue
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { failure in
            if let recovery = failure.recovery {
                Text("\(failure.message)\n\(recovery)")
            } else {
                Text(failure.message)
            }
        }
    }
}

/// A small colored dot: the state column, the sidebar footer, a workspace's row.
struct StatusDot: View {
    var color: Color
    var diameter: CGFloat = 7

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: diameter, height: diameter)
            .accessibilityHidden(true)
    }
}

/// The rounded box a grouped section's rows sit in (DeviceDeck's look).
struct SectionBox<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(Color(nsColor: .controlBackgroundColor).opacity(0.65)))
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(Color(nsColor: .separatorColor).opacity(0.5)))
    }
}

/// A label above or beside a value in a section ("Battery", "82%").
struct FactRow: View {
    var label: String
    var value: String
    var symbol: WindowIcon?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 5) {
                if let symbol {
                    symbol.image
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                }
                Text(label)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(width: 116, alignment: .leading)
            Text(value)
                .font(.callout)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label): \(value)")
    }
}

/// A count next to a section's title ("Held by this workspace 3").
struct CountBadge: View {
    var count: Int

    var body: some View {
        Text("\(count)")
            .font(.caption.weight(.medium))
            .monospacedDigit()
            .foregroundStyle(.secondary)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(Capsule().fill(Color.secondary.opacity(0.15)))
            .accessibilityLabel("\(count)")
    }
}

extension View {
    /// A secondary caption line (DeviceDeck's small grey text).
    func secondaryCaption() -> some View {
        font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(1)
    }
}

/// A section whose snapshot has not arrived yet (fix R1-6): the store's own state, never an
/// empty list that reads as "nothing exists".
struct ReadingView: View {
    var state: SnapshotState

    var body: some View {
        VStack(spacing: 8) {
            switch state {
            case .loading, .catchingUp:
                ProgressView().controlSize(.small)
            case .offline:
                WindowIcon.offline.image
                    .font(.system(size: 30, weight: .light))
                    .foregroundStyle(.tertiary)
            case .failed:
                WindowIcon.attention.image
                    .font(.system(size: 30, weight: .light))
                    .foregroundStyle(.secondary)
            case .loaded:
                EmptyView()
            }
            Text(state.title)
                .font(.headline)
            Text(state.detail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(state.title) \(state.detail)")
        .accessibilityIdentifier("window.reading")
    }
}

/// A monospaced one-line command, selectable (a task's or a claim's).
struct CommandText: View {
    var command: String

    var body: some View {
        Text(command)
            .font(.system(.callout, design: .monospaced))
            .lineLimit(1)
            .truncationMode(.middle)
            .textSelection(.enabled)
    }
}
