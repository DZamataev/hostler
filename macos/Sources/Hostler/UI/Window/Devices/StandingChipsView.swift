import HostlerKit
import SwiftUI

/// The standing chips of a device (DeviceDeck's holder chips): a workshop's color is the same
/// as its sidebar dot, so a person reads "who has it" across the window at a glance.
struct StandingChipsView: View {
    var chips: [StandingChip]
    /// Shown in the secondary style when there are no chips (`nil`: nothing at all).
    var emptyText: String?

    var body: some View {
        if chips.isEmpty {
            if let emptyText {
                Text(emptyText).secondaryCaption()
            }
        } else {
            HStack(spacing: 4) {
                ForEach(chips) { chip in
                    StandingChipView(chip: chip)
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Standing")
        }
    }
}

/// One chip.
struct StandingChipView: View {
    var chip: StandingChip

    private var tint: Color { chip.color?.color ?? .secondary }

    var body: some View {
        HStack(spacing: 4) {
            if let symbol = chip.symbol {
                WindowIcon.chip(symbol).image
                    .font(.caption2)
                    .foregroundStyle(chip.isSource ? .secondary : tint)
            } else if chip.color != nil {
                StatusDot(color: tint, diameter: 6)
            }
            Text(chip.text)
                .font(.caption)
                .lineLimit(1)
                .foregroundStyle(chip.isSource ? AnyShapeStyle(.secondary)
                                               : AnyShapeStyle(.primary))
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Capsule().fill(tint.opacity(chip.isSource ? 0.10 : 0.16)))
        .overlay(Capsule().strokeBorder(tint.opacity(chip.isSource ? 0.25 : 0.45)))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(chip.text)
    }
}
