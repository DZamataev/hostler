import Foundation
import Testing
@testable import HostlerKit

/// Ported verbatim from the reference app's `Tests/App/StatusItemToggleTests.swift`.
///
/// A repeat click on the status item can dismiss a transient popover on mouse-down and
/// then fire the button's mouse-up action, which would reopen it. The toggle ignores a
/// show that arrives inside that window (spec §9.1, "a show within 0.25 s of a close").
@MainActor
@Suite struct StatusItemToggleTests {
    private let now = Date(timeIntervalSince1970: 1_000_000)

    @Test func aShownPopoverCloses() {
        #expect(StatusItemToggle.decide(isShown: true, lastClosedAt: nil, now: now) == .close)
    }

    @Test func aShownPopoverClosesEvenJustAfterAClose() {
        let decision = StatusItemToggle.decide(
            isShown: true, lastClosedAt: now.addingTimeInterval(-0.1), now: now)
        #expect(decision == .close)
    }

    @Test func aPopoverClosedLongAgoShows() {
        let decision = StatusItemToggle.decide(
            isShown: false, lastClosedAt: now.addingTimeInterval(-1), now: now)
        #expect(decision == .show)
    }

    @Test func aShowWithinTheCloseWindowIsIgnored() {
        let decision = StatusItemToggle.decide(
            isShown: false, lastClosedAt: now.addingTimeInterval(-0.1), now: now)
        #expect(decision == .ignore)
    }

    @Test func aShowAtTheCloseWindowEdgeShows() {
        let decision = StatusItemToggle.decide(
            isShown: false, lastClosedAt: now.addingTimeInterval(-0.25), now: now)
        #expect(decision == .show)
    }

    @Test func aPopoverNeverClosedShows() {
        #expect(StatusItemToggle.decide(isShown: false, lastClosedAt: nil, now: now) == .show)
    }
}
