import Foundation

/// What the app offers while the service is not running (spec §9.7, R1-10, owner P5-L3).
public enum StartOffer: Sendable, Equatable {
    /// A Start button that runs `<executable> service start --json` on the person's click.
    case start(executable: String)
    /// No button: the command the person types.
    case typeCommand(String)
}

/// The Start policy, a pure function. `.start` only when all hold:
/// - the app's home **is** the default home (no `HOSTLER_HOME`, or one naming the default
///   directory) and its socket is the one the registered service listens on;
/// - the registration exists and sets no environment (launchd serves the default home only);
/// - its binary exists.
///
/// Otherwise the command to type: on a custom home `HOSTLER_HOME=<home> hostler service run`,
/// else `hostler service install`.
public func startOffer(home: SocketLocation, defaultHome: String, registration: Registration?,
                       euid: UInt32, fileExists: (String) -> Bool) -> StartOffer {
    guard isDefaultHome(home, defaultHome: defaultHome, euid: euid) else {
        return .typeCommand("HOSTLER_HOME=\(shellQuoted(home.home)) hostler service run")
    }
    guard let registration, !registration.setsEnvironment,
          fileExists(registration.executable)
    else {
        return .typeCommand(StartPolicy.installCommand)
    }
    return .start(executable: registration.executable)
}

public enum StartPolicy {
    public static let installCommand = "hostler service install"
}

/// `text` as one shell word: unchanged when it is plain, else in single quotes.
func shellQuoted(_ text: String) -> String {
    let plain = text.unicodeScalars.allSatisfy {
        CharacterSet.alphanumerics.contains($0) || "/._-+=:@%,".unicodeScalars.contains($0)
    }
    if plain && !text.isEmpty { return text }
    return "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
}

/// The app serves the default home: no `HOSTLER_HOME`, or one naming the default directory
/// with the default socket.
func isDefaultHome(_ home: SocketLocation, defaultHome: String, euid: UInt32) -> Bool {
    let defaultSocket = SocketLocator.location(home: defaultHome, euid: euid).socketPath
    return SocketLocator.sameDirectory(home.home, defaultHome) && home.socketPath == defaultSocket
}

/// A `hostler …` command as the person types it for the app's home (Task 13 F-2): on a custom
/// home it is prefixed with `HOSTLER_HOME=<home, shell-quoted>`, as the not-running offer is;
/// on the default home, or for a command that is not hostler's, it is left as it is.
public func commandForHome(_ command: String, home: SocketLocation, defaultHome: String,
                           euid: UInt32) -> String {
    guard command.hasPrefix("hostler "),
          !isDefaultHome(home, defaultHome: defaultHome, euid: euid)
    else { return command }
    return "HOSTLER_HOME=\(shellQuoted(home.home)) \(command)"
}
