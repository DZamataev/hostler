import HostlerKit
import SwiftUI

/// The Settings window (plan Task 9, P5-L6): the notification kinds (spec §9.4, kept in
/// `UserDefaults`) with macOS's permission state, and open at login.
struct SettingsView: View {
    let model: AppModel

    var body: some View {
        @Bindable var notifier = model.notifier
        Form {
            Section(String(localized: "Notifications")) {
                Toggle(String(localized: "A new device connected"), isOn: $notifier.settings.newDevice)
                    .accessibilityLabel(String(localized: "Notify when a new device connects"))
                Toggle(String(localized: "A secret or account requested"), isOn: $notifier.settings.request)
                    .accessibilityLabel(String(localized: "Notify when a secret or account is requested"))
                Toggle(String(localized: "A heavy task looks stuck"), isOn: $notifier.settings.stuckTask)
                    .accessibilityLabel(String(localized: "Notify when a heavy task looks stuck"))
                Toggle(String(localized: "A device was taken over"), isOn: $notifier.settings.seizure)
                    .accessibilityLabel(String(localized: "Notify when a device is taken over"))
                if let text = model.notifier.permission.settingsText {
                    Text(text)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Section(String(localized: "Login")) {
                Toggle(String(localized: "Open at login"),
                       isOn: Binding(get: { model.loginItem.isOn }, set: { model.loginItem.set($0) }))
                    .disabled(!model.loginItem.isAvailable)
                    .accessibilityLabel(String(localized: "Open hostler at login"))
                if let hint = model.loginItem.hint {
                    Text(hint).font(.caption).foregroundStyle(.secondary)
                }
                if let error = model.loginItem.error {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 440)
        .onAppear {
            model.loginItem.refresh()
            model.notifier.refresh()
        }
    }
}
