import Foundation
import Testing
@testable import HostlerKit

@Suite struct FakeClockTests {
    @Test func itMovesOnlyWhenAdvanced() {
        let clock = FakeClock(Date(timeIntervalSinceReferenceDate: 10))
        #expect(clock.now() == Date(timeIntervalSinceReferenceDate: 10))
        clock.advance(by: 2.5)
        #expect(clock.now() == Date(timeIntervalSinceReferenceDate: 12.5))
    }
}
