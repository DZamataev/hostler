import Foundation

/// Every user-visible failure of the app (the reference app's invariant 8): a description and a
/// recovery, the service's own `recovery` text when it has one. Later tasks add their cases
/// (Task 7: the connection's and the CLI's; Task 9: the values'; Task 11: the window's two
/// refusals before anything is sent).
public enum HostlerAppError: Error, Equatable, Sendable, LocalizedError {
    /// The service answered with an error; its message and recovery are shown as they are.
    case service(message: String, recovery: String?)
    /// `HOSTLER_HOME` is not an absolute path (the CLI refuses it too, L45).
    case badHome(String)
    /// The `hostler` command failed without a `--json` answer: its exit status and the end of
    /// its error output.
    case cliFailed(exit: Int32, stderr: String)
    /// The `hostler` command could not be started at all.
    case cliNotStarted(String)
    /// The `hostler` command did not finish within its bound.
    case cliTimedOut(command: String)
    /// The `hostler` command was cancelled (its caller went away).
    case cliCancelled(command: String)
    /// A Start was asked for an offer that runs nothing (the command to type instead).
    case nothingToStart(command: String)
    /// Task 9: a value or password field was left empty; nothing was sent.
    case emptyValue(field: String)
    /// Task 9: the two passwords typed to create a database differ; nothing was sent.
    case passwordsDiffer
    /// Task 9: a request's project is not in the projects snapshot yet; nothing was sent.
    case projectUnknown(name: String)
    /// Task 9: another action is already waiting for the master password.
    case gateBusy
    /// Task 11: the heavy queue's capacity is 1–16 (spec §11.1); nothing was sent.
    case capacityOutOfRange(Int)
    /// Task 11: the path is not one of the preserved database files `secrets.status` listed;
    /// nothing was sent.
    case unknownPreservedFile(String)

    public var errorDescription: String? {
        switch self {
        case .service(let message, _): message
        case .badHome(let home):
            String(localized: "HOSTLER_HOME must be an absolute path, not \"\(home)\"")
        case .cliFailed(let exit, let stderr):
            stderr.isEmpty
                ? String(localized: "hostler exited with status \(exit)")
                : String(localized: "hostler exited with status \(exit): \(stderr)")
        case .cliNotStarted(let why):
            String(localized: "hostler could not be started: \(why)")
        case .cliTimedOut(let command):
            String(localized: "\(command) did not finish in time")
        case .cliCancelled(let command): String(localized: "\(command) was cancelled")
        case .nothingToStart(let command):
            String(localized: "hostler's service is not running; run \(command)")
        case .emptyValue(let field): String(localized: "\(field) is empty")
        case .passwordsDiffer: String(localized: "The two passwords differ")
        case .projectUnknown(let name):
            String(localized: "The project \(name) is not known to the app yet")
        case .gateBusy: String(localized: "Another action is waiting for the master password")
        case .capacityOutOfRange(let value):
            String(localized: "The heavy queue takes 1 to 16 tasks at once, not \(value)")
        case .unknownPreservedFile(let path):
            String(localized:
                "\((path as NSString).lastPathComponent) is not one of hostler's preserved files")
        }
    }

    public var recoverySuggestion: String? {
        switch self {
        case .service(_, let recovery): recovery
        case .badHome:
            String(localized: "set HOSTLER_HOME to the full path of the directory, or unset it")
        case .cliFailed, .cliNotStarted, .cliTimedOut, .cliCancelled: nil
        case .nothingToStart(let command): command
        case .emptyValue: String(localized: "type a value, then try again")
        case .passwordsDiffer: String(localized: "type the same password in both fields")
        case .projectUnknown:
            String(localized: "wait until the app has read the service, then try again")
        case .gateBusy: String(localized: "finish or cancel that one first")
        case .capacityOutOfRange: String(localized: "set a number from 1 to 16")
        case .unknownPreservedFile: String(localized: "refresh with ⌘R, then try again")
        }
    }
}
