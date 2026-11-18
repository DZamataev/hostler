import Foundation
import HostlerKit

@MainActor
final class FakeWindowRaiser: WindowRaising {
    var hasEligibleWindows = false
    private(set) var raiseAllCount = 0

    func raiseAll() { raiseAllCount += 1 }
}
