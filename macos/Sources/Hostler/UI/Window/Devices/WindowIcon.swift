import HostlerKit
import SwiftUI

/// The window's SF Symbols in one table (spec §9.5: SF Symbols, system colors). `Icon` is the
/// popover's and the status item's set; the window needs its own.
enum WindowIcon: String, CaseIterable, Sendable {
    case allDevices = "square.grid.2x2"
    case offline = "bolt.horizontal.circle"
    case attention = "exclamationmark.circle"
    case queue = "tray.full"
    case screen = "display"
    case accounts = "person.crop.circle"
    case secrets = "key.fill"
    case projects = "folder"
    case device = "iphone"
    case tablet = "ipad"
    case emulator = "iphone.gen3"
    case simulator = "rectangle.on.rectangle"
    case bolt = "bolt.fill"
    case chevron = "chevron.right"
    case ellipsis = "ellipsis.circle"
    case refresh = "arrow.clockwise"
    case plus = "plus"
    case handTap = "hand.tap"
    case pin = "pin"
    case pinSlash = "pin.slash"
    case clock = "clock"
    case wifi = "wifi"
    case battery = "battery.75"
    case finder = "folder.badge.gearshape"
    case terminal = "terminal"
    case doctor = "stethoscope"
    case eye = "eye"
    case checkmark = "checkmark.circle.fill"
    case warning = "exclamationmark.triangle.fill"
    case error = "xmark.octagon.fill"
    case skip = "minus.circle"
    case trash = "trash"
    case pencil = "pencil"
    case note = "note.text"
    case stop = "stop.circle"
    case link = "link"

    var image: Image { Image(systemName: rawValue) }
}

extension WindowIcon {
    /// A device's icon by kind (and platform, where a phone and a pad read differently).
    static func device(_ info: DeviceInfo) -> WindowIcon {
        switch info.kind {
        case .physical: info.model.localizedCaseInsensitiveContains("pad") ? .tablet : .device
        case .emulator: .emulator
        case .simulator: .simulator
        }
    }

    /// A claimed resource's icon: a device, the screen or an account.
    static func resource(_ kind: ResourceKind) -> WindowIcon {
        switch kind {
        case .device: .device
        case .screen: .screen
        case .account: .accounts
        }
    }

    /// The chip's small symbol.
    static func chip(_ symbol: StandingChip.Symbol) -> WindowIcon {
        switch symbol {
        case .handTap: .handTap
        case .pin: .pin
        case .clock: .clock
        }
    }
}
