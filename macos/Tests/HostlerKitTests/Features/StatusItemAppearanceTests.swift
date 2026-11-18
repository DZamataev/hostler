import Testing
@testable import HostlerKit

/// The status image's three states (plan Task 2): offline wins, then attention, then idle.
@Suite struct StatusItemAppearanceTests {
    @Test func notLiveIsOfflineWhateverElseIsPending() {
        #expect(StatusItemAppearance.decide(isLive: false, pendingRequests: 0,
                                            lockedWithWaitingSecretUsers: false) == .offline)
        #expect(StatusItemAppearance.decide(isLive: false, pendingRequests: 3,
                                            lockedWithWaitingSecretUsers: true) == .offline)
    }

    @Test func aPendingRequestNeedsAttention() {
        #expect(StatusItemAppearance.decide(isLive: true, pendingRequests: 1,
                                            lockedWithWaitingSecretUsers: false) == .attention)
    }

    @Test func aLockedDatabaseWithWaitingSecretUsersNeedsAttention() {
        #expect(StatusItemAppearance.decide(isLive: true, pendingRequests: 0,
                                            lockedWithWaitingSecretUsers: true) == .attention)
    }

    @Test func liveWithNothingPendingIsIdle() {
        #expect(StatusItemAppearance.decide(isLive: true, pendingRequests: 0,
                                            lockedWithWaitingSecretUsers: false) == .idle)
    }
}
