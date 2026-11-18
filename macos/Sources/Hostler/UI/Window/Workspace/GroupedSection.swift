import HostlerKit
import SwiftUI

/// A collapsible grouped section with a count (spec §9.3, DeviceDeck's look): a chevron, the
/// title, the count, and the rows in a rounded box. Used by the device and workspace details.
struct GroupedSection<Content: View>: View {
    var title: String
    var count: Int?
    @Binding var isExpanded: Bool
    /// Anything shown at the section's trailing edge (a button, a status line).
    var trailing: AnyView?
    @ViewBuilder var content: Content

    init(title: String, count: Int? = nil, isExpanded: Binding<Bool>,
         trailing: AnyView? = nil, @ViewBuilder content: () -> Content) {
        self.title = title
        self.count = count
        self._isExpanded = isExpanded
        self.trailing = trailing
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Button {
                    withAnimation(.easeInOut(duration: 0.15)) { isExpanded.toggle() }
                } label: {
                    HStack(spacing: 6) {
                        WindowIcon.chevron.image
                            .font(.caption.weight(.semibold))
                            .rotationEffect(.degrees(isExpanded ? 90 : 0))
                            .foregroundStyle(.secondary)
                        Text(title)
                            .font(.headline)
                        if let count { CountBadge(count: count) }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(title)
                .accessibilityValue(isExpanded ? String(localized: "expanded")
                                   : String(localized: "collapsed"))
                .accessibilityHint("Show or hide this section")

                Spacer(minLength: 4)
                if let trailing { trailing }
            }
            if isExpanded {
                SectionBox { content }
            }
        }
    }
}

/// An empty section's line ("Nothing held").
struct EmptyLine: View {
    var text: String

    var body: some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
