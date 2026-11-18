import SwiftUI

/// Every SF Symbol the app shows, in one place (plan deviation 3: SF Symbols instead of
/// the reference app's vendored SVGs; the rule "never `Image(systemName:)` in a view" is kept).
/// The status item's template image is the one exception (`StatusItemController`).
public enum Icon: String, CaseIterable, Sendable {
    case window = "macwindow"
    case settings = "gearshape"
    case quit = "power"
    case offline = "bolt.horizontal.circle"
    case attention = "exclamationmark.circle"

    public var image: Image { Image(systemName: rawValue) }
}
