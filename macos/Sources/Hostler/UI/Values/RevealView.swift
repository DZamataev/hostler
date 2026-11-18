import AppKit
import HostlerKit
import SwiftUI

/// Reveal for one secret or account (spec §9.6; Task 11's Secrets and Accounts rows use it).
/// Reveal asks through the person gate (attach `.personGateSheet(model.gate)` to the window);
/// the value is shown monospaced for at most 30 s:
/// - hidden from the accessibility tree (`.accessibilityHidden(true)`) and marked
///   `.privacySensitive()`;
/// - the window's `sharingType` is `.none` while it is shown (best effort), restored after;
/// - it hides when this view goes away (the window closes or the row scrolls out) and on a lock.
/// Copy writes it concealed; the pasteboard is cleared after 60 s if it still holds it.
struct RevealView: View {
    let reveal: RevealController
    let subject: RevealSubject

    private var shown: RevealedValue? {
        reveal.shown.flatMap { $0.subject == subject ? $0 : nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let shown {
                ForEach(shown.fields) { field in
                    fieldRow(field)
                }
                HStack {
                    Text(String(localized: "Hidden again after 30 s"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button(String(localized: "Hide")) { reveal.hide() }
                        .controlSize(.small)
                        .accessibilityLabel(String(localized: "Hide \(subject.name)"))
                }
            } else {
                HStack {
                    Button(String(localized: "Reveal")) {
                        Task { await reveal.reveal(subject) }
                    }
                    .controlSize(.small)
                    .disabled(reveal.revealing != nil)
                    .accessibilityLabel(String(localized: "Reveal \(subject.name)"))
                    if reveal.revealing == subject {
                        ProgressView().controlSize(.small)
                            .accessibilityLabel(String(localized: "Revealing"))
                    }
                }
                if let failure = reveal.error, failure.subject == subject {
                    ValueErrorLines(failure.error)
                }
            }
        }
        // Always present, active only while a value is shown: the window is captured
        // normally again the moment the value hides (Task 13 F-6).
        .background(WindowSharingGuard(active: shown != nil))
        .onDisappear {
            // Also while a reveal is on its way: its answer is then dropped (clean read C-B2).
            reveal.dismissed(subject)
        }
    }

    private func fieldRow(_ field: RevealedField) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if case .account = subject {
                Text(field.name)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The one place a value becomes a `String`: for drawing only (SecretText's honest
            // limit). Never in the accessibility tree.
            Text(verbatim: field.value.reveal())
                .font(.system(.body, design: .monospaced))
                .textSelection(.disabled)
                .lineLimit(3)
                .privacySensitive()
                .accessibilityHidden(true)
            Spacer(minLength: 4)
            Button(reveal.copied == field.name ? String(localized: "Copied") : String(localized: "Copy")) {
                reveal.copy(field)
            }
            .controlSize(.small)
            .help(String(localized: "Copies the value; the clipboard is cleared after 60 s if it still holds it"))
            .accessibilityLabel(String(localized: "Copy \(field.name)"))
        }
    }
}

/// Keeps its window out of screen capture and sharing (`sharingType = .none`) while `active`,
/// and gives the window back the value it had as soon as it is not: when `active` turns false
/// (the auto-hide, Hide, a lock), when SwiftUI dismantles the view, or when it leaves the
/// window (best effort, spec §9.6; Task 13 F-6).
struct WindowSharingGuard: NSViewRepresentable {
    let active: Bool

    func makeNSView(context: Context) -> GuardView { GuardView() }

    func updateNSView(_ nsView: GuardView, context: Context) {
        nsView.setActive(active)
    }

    static func dismantleNSView(_ nsView: GuardView, coordinator: ()) {
        nsView.setActive(false)
    }

    /// One count per window of the values shown in it (Task 13 re-run G-1).
    ///
    /// A window's own setting is remembered **once**, the first time the app excludes that
    /// window, and given back whenever its last shown value goes. It is never read again: on
    /// macOS 26 the getter of an on-screen window keeps answering `.none` once it was set to
    /// `.none`, although WindowServer does honour a later `.readOnly` (checked with a probe
    /// window), so a value read at the second reveal would be `.none` and capture would stay
    /// off for the rest of the run. Counting also keeps two rows that overlap while SwiftUI
    /// updates them, in either order, from restoring too early.
    @MainActor
    enum Registry {
        private struct Original {
            weak var window: NSWindow?
            var sharing: NSWindow.SharingType
        }

        private static var originals: [ObjectIdentifier: Original] = [:]
        private static var counts: [ObjectIdentifier: Int] = [:]

        static func acquire(_ window: NSWindow) {
            prune()
            let key = ObjectIdentifier(window)
            if originals[key] == nil {
                originals[key] = Original(window: window, sharing: window.sharingType)
            }
            counts[key, default: 0] += 1
            window.sharingType = .none
        }

        static func release(_ window: NSWindow) {
            let key = ObjectIdentifier(window)
            guard let count = counts[key] else { return }
            if count > 1 {
                counts[key] = count - 1
            } else {
                counts[key] = nil
                window.sharingType = originals[key]?.sharing ?? .readOnly
            }
        }

        /// The values counted as shown in `window` (tests).
        static func count(for window: NSWindow) -> Int {
            counts[ObjectIdentifier(window)] ?? 0
        }

        /// Drops what is kept for windows that are gone (their identifiers may be reused).
        private static func prune() {
            for (key, original) in originals where original.window == nil {
                originals[key] = nil
                counts[key] = nil
            }
        }
    }

    final class GuardView: NSView {
        private var active = false
        /// The window this view counts in, while it does.
        private weak var counted: NSWindow?

        func setActive(_ on: Bool) {
            active = on
            apply()
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            apply()
        }

        override func removeFromSuperview() {
            release()
            super.removeFromSuperview()
        }

        private func apply() {
            guard active, let window else {
                release()
                return
            }
            guard counted !== window else { return }
            release()
            counted = window
            Registry.acquire(window)
        }

        private func release() {
            if let counted { Registry.release(counted) }
            counted = nil
        }
    }
}
