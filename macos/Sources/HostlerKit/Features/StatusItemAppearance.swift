import Foundation

/// The status item's image state (plan Task 2). hostler's item shows an image only, never a
/// title, and a state changes the image, never the item's width.
public enum StatusItemAppearance: Equatable, Sendable, CaseIterable {
    /// Live, nothing waiting for the person.
    case idle
    /// Live, and the person is needed: a pending secret or account request, or a locked
    /// database that secret users are waiting on.
    case attention
    /// The service is not reachable (not running, refused, a contract mismatch, connecting).
    case offline

    /// Offline wins: with no connection nothing else is known to be current.
    public static func decide(isLive: Bool, pendingRequests: Int,
                              lockedWithWaitingSecretUsers: Bool) -> StatusItemAppearance {
        guard isLive else { return .offline }
        if pendingRequests > 0 || lockedWithWaitingSecretUsers { return .attention }
        return .idle
    }
}
