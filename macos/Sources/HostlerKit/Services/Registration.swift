import Foundation

/// The service's launchd registration, as far as the Start policy needs it.
public struct Registration: Sendable, Equatable {
    /// `ProgramArguments[0]`: the binary launchd starts.
    public let executable: String
    /// The plist sets an environment (`EnvironmentVariables`). hostler's own plist never
    /// does, so a registration that does is not one this app may start (spec §9.7).
    public let setsEnvironment: Bool

    public init(executable: String, setsEnvironment: Bool) {
        self.executable = executable
        self.setsEnvironment = setsEnvironment
    }
}

/// Reads the service's registration; `nil` when there is none.
public protocol RegistrationReading: Sendable {
    func registration() -> Registration?
}

/// The LaunchAgent plist `~/Library/LaunchAgents/io.github.dzamataev.hostler.plist`
/// (`service/manager/launchd.rs`). Read only; the app never writes or loads it.
public struct LaunchAgentRegistration: RegistrationReading {
    public static let label = "io.github.dzamataev.hostler"

    public let plistPath: String

    public init(homeDirectory: String) {
        plistPath = SocketLocator.join(
            SocketLocator.join(SocketLocator.join(homeDirectory, "Library"), "LaunchAgents"),
            "\(Self.label).plist")
    }

    public init(plistPath: String) {
        self.plistPath = plistPath
    }

    /// `nil` when the file is absent, unreadable, or has no `ProgramArguments[0]`: any of
    /// these is fixed by `hostler service install`.
    public func registration() -> Registration? {
        guard let data = FileManager.default.contents(atPath: plistPath) else { return nil }
        return Self.parse(data)
    }

    static func parse(_ data: Data) -> Registration? {
        guard
            let plist = try? PropertyListSerialization.propertyList(from: data, format: nil),
            let dictionary = plist as? [String: Any],
            let arguments = dictionary["ProgramArguments"] as? [Any],
            let executable = arguments.first as? String, !executable.isEmpty
        else { return nil }
        let environment = dictionary["EnvironmentVariables"]
        let setsEnvironment: Bool
        if let variables = environment as? [String: Any] {
            setsEnvironment = !variables.isEmpty
        } else {
            setsEnvironment = environment != nil
        }
        return Registration(executable: executable, setsEnvironment: setsEnvironment)
    }
}
