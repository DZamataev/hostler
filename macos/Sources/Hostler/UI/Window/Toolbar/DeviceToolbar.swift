import HostlerKit
import SwiftUI

/// The device views' toolbar (spec §9.3, DeviceDeck's segmented filters): All / iOS / Android,
/// All / Physical / Virtual, "show unregistered", and Refresh (⌘R).
struct DeviceToolbar: ToolbarContent {
    @Binding var platform: PlatformFilter
    @Binding var kind: KindFilter
    @Binding var showsUnregistered: Bool
    var refresh: () -> Void

    var body: some ToolbarContent {
        // A toolbar group, not one item holding an HStack: the group is laid out with the rest
        // of the toolbar, so its controls never draw over the sidebar toggle or Refresh. The
        // pickers keep their ideal width — a fixed width clipped and overlapped the longer
        // Russian segments ("Физические", "Виртуальные") — and the window's minimum width below
        // leaves the group its room in both languages (review of t12-window-ru.png).
        ToolbarItemGroup(placement: .principal) {
            Picker("Platform", selection: $platform) {
                ForEach(PlatformFilter.allCases, id: \.self) { filter in
                    Text(filter.title).tag(filter)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
            .help("All devices, only iOS or only Android")
            .accessibilityLabel("Platform filter")
            .accessibilityIdentifier("filter.platform")

            Picker("Device kind", selection: $kind) {
                ForEach(KindFilter.allCases, id: \.self) { filter in
                    Text(filter.title).tag(filter)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
            .help("All devices, only physical ones or only emulators and simulators")
            .accessibilityLabel("Device kind filter")
            .accessibilityIdentifier("filter.kind")

            // The eye carries the meaning and its state draws it pressed; the visible label
            // would push the two pickers out of the toolbar at the window's default width in
            // Russian ("Незарегистрированные"), so the label is the tooltip and the
            // accessibility name instead.
            Toggle(isOn: $showsUnregistered) {
                WindowIcon.eye.image
                    .accessibilityHidden(true)
            }
            .toggleStyle(.button)
            .controlSize(.small)
            .fixedSize()
            .help(String(localized:
                "Show unregistered devices — \(CommandHelp.text(.listDevices))"))
            .accessibilityLabel("Show unregistered devices")
            .accessibilityIdentifier("filter.unregistered")
        }

        ToolbarItem(placement: .primaryAction) {
            Button(action: refresh) {
                WindowIcon.refresh.image
            }
            .help("Refresh — re-read every snapshot (⌘R)")
            .accessibilityLabel("Refresh")
        }
    }
}
