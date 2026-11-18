import Foundation

/// Why a call through `HostlerClient` failed. No case carries a frame, a value or a decoder's
/// message (which may quote the input): only the service's own error, a method and fixed texts.
public enum HostlerCallError: Error, Sendable, Equatable {
    /// The service answered with an error; its `message` and `recovery` are the texts to show.
    case service(HostlerError)
    /// No connection to the service (it is not running, or the connection closed).
    case notConnected
    /// The connection failed; a fixed description of what failed (never the frame).
    case transport(String)
    /// No answer within the method's bound (`HostlerMethod.spec.timeout`).
    case timedOut(HostlerMethod)
    /// The answer did not decode as the method's answer type.
    case undecodable(HostlerMethod)
    /// The calling task was cancelled before the answer came (Task 7).
    case cancelled(HostlerMethod)
    /// The method was sent through the wrong path: an answer with values only through
    /// `callSensitive`, any other only through `call`. Nothing was sent (Task 7).
    case wrongPath(HostlerMethod)
    /// The request is longer than `Contract.maxFrameBytes`; nothing was sent (Task 7).
    case tooLarge(HostlerMethod)
}

extension HostlerCallError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .service(let error): error.message
        case .notConnected: String(localized: "hostler's service is not running")
        case .transport(let what): what
        case .timedOut(let method):
            String(localized: "hostler did not answer \(method.rawValue) in time")
        case .undecodable(let method):
            String(localized: "hostler's answer to \(method.rawValue) could not be read")
        case .cancelled(let method): String(localized: "\(method.rawValue) was cancelled")
        case .wrongPath(let method):
            String(localized: "\(method.rawValue) was not sent: it must go through its own path")
        case .tooLarge(let method):
            String(localized: "\(method.rawValue) was not sent: it is larger than a frame may be")
        }
    }

    public var recoverySuggestion: String? {
        switch self {
        case .service(let error): error.recovery
        default: nil
        }
    }
}
