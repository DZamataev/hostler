import Foundation

/// An action of the window that has an equivalent `hostler` command line (plan Task 10). Every
/// tooltip of such an action names the command from this one table (spec §9.3), so the app never
/// drifts from the CLI's spelling: the text is the command a person would type, with the
/// placeholders `<device>`, `<alias>`, `<new>`, `<note>`, `<claim>`, `<token>`, `<task>`,
/// `<name>`, `<field>`, `<project>` and `<capacity>`.
///
/// `<token>` is `--confirmation`'s value: a device's alias, `screen`, or an account's name
/// (`ClaimRow.confirmationToken`). Every template is also parsed by the real CLI in
/// `crates/hostler/tests/command_help.rs`, against the same fixture the Swift test reads
/// (`crates/hostler/tests/fixtures/command-help.json`), so a tooltip cannot promise a command the
/// parser rejects (P5-L16).
///
/// Actions without a command line are not in this table: a claim's waiting request is withdrawn
/// from the app (its waiting process withdraws it with Ctrl+C; a queued heavy task does have
/// `hostler queue withdraw`), a preserved database file is deleted only from the app (the person
/// gate, spec §8.5), and Refresh re-reads the service's snapshots (`HostlerStore.refresh`).
public enum CommandAction: String, CaseIterable, Sendable {
    /// The device list with unregistered devices (`Toolbar`'s "show unregistered").
    case listDevices
    case addDevice
    case renameDevice
    case noteDevice
    /// The note's Clear button: the CLI's `--clear` form, not an empty text.
    case clearNote
    case removeDevice
    case reserveDevice
    case unreserveDevice
    case renewHold
    case releaseClaim
    case takeOverDevice
    case forgetSessions
    case runDoctor
    // Task 11: the queue, the screen, the secrets, the accounts and a project's approval.
    case withdrawTask
    case stopTask
    case setCapacity
    case releaseScreen
    case takeOverScreen
    case setSecret
    case removeSecret
    case shareSecret
    case unshareSecret
    case setAccountField
    case removeAccount
    case approveProject

    /// The command with placeholders. Every text starts with `hostler ` (a test checks it).
    public var template: String {
        switch self {
        case .listDevices: "hostler devices list --all"
        case .addDevice: "hostler devices add <device> --alias <new>"
        case .renameDevice: "hostler devices rename <alias> <new>"
        case .noteDevice: "hostler devices note <alias> \"<note>\""
        case .clearNote: "hostler devices note <alias> --clear"
        case .removeDevice: "hostler devices remove <alias>"
        case .reserveDevice: "hostler devices reserve <alias>"
        case .unreserveDevice: "hostler devices unreserve <alias>"
        case .renewHold: "hostler hold renew <claim>"
        case .releaseClaim: "hostler release <claim> --force --confirmation <token>"
        case .takeOverDevice:
            "hostler hold device <alias> --force --confirmation <token> --ttl 30"
        case .forgetSessions: "hostler devices forget-sessions <alias>"
        case .runDoctor: "hostler doctor"
        case .withdrawTask: "hostler queue withdraw --id <task>"
        case .stopTask: "hostler queue stop <task> --force --confirmation <token>"
        case .setCapacity: "hostler queue capacity set <capacity>"
        case .releaseScreen: "hostler release <claim> --force --confirmation <token>"
        case .takeOverScreen:
            "hostler hold screen --force --confirmation <token> --ttl 30"
        case .setSecret: "hostler secret set <name>"
        case .removeSecret: "hostler secret remove <name>"
        case .shareSecret: "hostler secret share <name> --to <project>"
        case .unshareSecret: "hostler secret unshare <name> --to <project>"
        case .setAccountField: "hostler account set <name> --field <field>"
        case .removeAccount: "hostler account remove <name>"
        case .approveProject: "hostler project approve"
        }
    }

    /// Whether the command resolves a workspace, so a person runs it in that checkout.
    /// `hostler devices reserve|unreserve`, `hold device`, `hold screen` and `project approve`
    /// default to the current workspace, and `doctor` reads it.
    public var runsInTheWorkspace: Bool {
        switch self {
        case .reserveDevice, .unreserveDevice, .takeOverDevice, .runDoctor, .takeOverScreen,
             .approveProject: true
        case .listDevices, .addDevice, .renameDevice, .noteDevice, .clearNote, .removeDevice,
             .renewHold, .releaseClaim, .forgetSessions, .withdrawTask, .stopTask, .setCapacity,
             .releaseScreen, .setSecret, .removeSecret, .shareSecret, .unshareSecret,
             .setAccountField, .removeAccount: false
        }
    }

    /// The command for a tooltip: the placeholders are filled in where the caller knows the
    /// value, and the workspace note is added when the command runs in one.
    ///
    /// - Parameters:
    ///   - device: the device's name as `devices list` shows it (`<device>` and, for a registered
    ///     device, `<alias>`).
    ///   - newName: the new alias (`<new>`).
    ///   - note: the note's text (`<note>`).
    ///   - claim: the claim's id (`<claim>`).
    ///   - token: `--confirmation`'s value (`<token>`).
    ///   - workspace: the checkout's folder name, for the note.
    ///   - task: a heavy task's id (`<task>`).
    ///   - name: a secret's, an account's or a project's name (`<name>`).
    ///   - field: an account field (`<field>`).
    ///   - project: the project a secret is shared with (`<project>`).
    ///   - capacity: the heavy queue's capacity (`<capacity>`).
    public func text(device: String? = nil, newName: String? = nil, note: String? = nil,
                     claim: String? = nil, token: String? = nil, workspace: String? = nil,
                     task: String? = nil, name: String? = nil, field: String? = nil,
                     project: String? = nil, capacity: Int? = nil) -> String {
        var text = template
        if let device {
            text = text.replacingOccurrences(of: "<device>", with: device)
            text = text.replacingOccurrences(of: "<alias>", with: device)
        }
        if let newName { text = text.replacingOccurrences(of: "<new>", with: newName) }
        if let note { text = text.replacingOccurrences(of: "<note>", with: note) }
        if let claim { text = text.replacingOccurrences(of: "<claim>", with: claim) }
        if let token { text = text.replacingOccurrences(of: "<token>", with: token) }
        if let task { text = text.replacingOccurrences(of: "<task>", with: task) }
        if let name { text = text.replacingOccurrences(of: "<name>", with: name) }
        if let field { text = text.replacingOccurrences(of: "<field>", with: field) }
        if let project { text = text.replacingOccurrences(of: "<project>", with: project) }
        if let capacity { text = text.replacingOccurrences(of: "<capacity>", with: "\(capacity)") }
        if runsInTheWorkspace, let workspace {
            text += " (in \(workspace))"
        }
        return text
    }
}

/// The table itself. Views use `CommandHelp.text(_:)` for every `.help(…)` that names a command.
public enum CommandHelp {
    public static func text(_ action: CommandAction, device: String? = nil, newName: String? = nil,
                            note: String? = nil, claim: String? = nil, token: String? = nil,
                            workspace: String? = nil, task: String? = nil, name: String? = nil,
                            field: String? = nil, project: String? = nil,
                            capacity: Int? = nil) -> String {
        action.text(device: device, newName: newName, note: note, claim: claim, token: token,
                    workspace: workspace, task: task, name: name, field: field,
                    project: project, capacity: capacity)
    }
}
