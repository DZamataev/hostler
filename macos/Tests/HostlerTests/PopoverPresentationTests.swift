import Foundation
import HostlerKit
import Testing
@testable import Hostler

/// The popover's pure texts (plan Task 9, as the reference app's `MenuPanelPresentation`): the
/// banner per connection state (spec §9.7), "N more", a request card's subtitle.
@Suite struct PopoverPresentationTests {
    typealias P = PopoverPresentation

    @Test func liveHasNoBannerAndAGreenDot() {
        #expect(P.banner(.live(serviceVersion: "0.0.4")) == nil)
        #expect(P.header(.live(serviceVersion: "0.0.4")).dot == .live)
        #expect(P.header(.live(serviceVersion: "0.0.4")).text == "Live")
    }

    @Test func notRunningOnTheDefaultHomeOffersStartAndRetry() {
        let banner = P.banner(.notRunning(offer: .start(executable: "/opt/homebrew/bin/hostler")))
        #expect(banner?.title == "hostler's service is not running.")
        #expect(banner?.actions == [.start, .retry])
        #expect(P.header(.notRunning(offer: .start(executable: "x"))).dot == .offline)
    }

    @Test func notRunningOtherwiseOffersTheCommandToTypeAndNoStart() {
        let command = "HOSTLER_HOME=/tmp/h hostler service run"
        let banner = P.banner(.notRunning(offer: .typeCommand(command)))
        #expect(banner?.detail == "Start it in a terminal: \(command)")
        #expect(banner?.actions == [.copy(command), .retry])
        #expect(banner?.actions.contains(.start) == false)
        let install = P.banner(.notRunning(offer: .typeCommand("hostler service install")))
        #expect(install?.actions == [.copy("hostler service install"), .retry])
    }

    @Test func refusedStatesGiveTheReasonAndNoStart() {
        let denied = P.banner(.denied(reason: "permission denied: /x/hostler.sock"))
        #expect(denied == .init(title: "This app may not reach hostler's socket.",
                                detail: "permission denied: /x/hostler.sock", actions: [.retry]))
        #expect(P.header(.denied(reason: "x")).dot == .refused)
        let foreign = P.banner(.foreignOwner(path: "/x/hostler.sock"))
        #expect(foreign?.title == "hostler's socket belongs to another user.")
        #expect(foreign?.actions == [.retry])
        #expect(P.header(.foreignOwner(path: "x")).text == "Refused")
    }

    @Test func aMismatchShowsBothVersionsAndTheRecovery() {
        let banner = P.banner(.mismatch(client: 5, service: 4, serviceVersion: "0.0.4",
                                        recovery: "brew upgrade hostler"))
        #expect(banner?.title == "The app and the service do not match.")
        #expect(banner?.detail
            == "This app speaks contract 5; the service (hostler 0.0.4) speaks 4. brew upgrade hostler")
        #expect(banner?.actions == [.copy("brew upgrade hostler"), .retry])
        let bare = P.banner(.mismatch(client: 5, service: 6, serviceVersion: "0.0.6", recovery: nil))
        #expect(bare?.actions == [.retry])
    }

    @Test func connectingProtocolErrorAndBadHome() {
        #expect(P.banner(.connecting) == .init(title: "Connecting to hostler…", detail: nil, actions: []))
        #expect(P.banner(.protocolError("connecting timed out"))?.detail == "connecting timed out")
        #expect(P.banner(.badHome("rel/home"))?.actions == [])
        #expect(P.header(.badHome("x")).dot == .refused)
    }

    @Test func nMore() {
        #expect(P.moreText(3) == "3 more")
        let many = (0..<11).map { Self.device("d\($0)", connected: $0 % 2 == 0) }
        let (shown, more) = P.devices(many, standing: { _ in [] })
        #expect(shown.count == 8)
        #expect(more == "3 more")
        // Connected devices come first.
        #expect(shown.prefix(6).allSatisfy { $0.connected })
        let (few, none) = P.devices(Array(many.prefix(3)), standing: { _ in [] })
        #expect(few.count == 3)
        #expect(none == nil)
    }

    @Test func aRequestCardsSubtitle() {
        let card = RequestCard(id: "s-1", kind: .secret, projectId: "p", projectName: "app",
                               name: "API_KEY", purpose: nil, reason: "r", requesterPid: 4242,
                               createdAtMs: 1_000_000, fields: ["value"])
        #expect(P.cardSubtitle(card, nowMs: 1_000_000 + 20_000) == "app · pid 4242 · just now")
        #expect(P.cardSubtitle(card, nowMs: 1_000_000 + 180_000) == "app · pid 4242 · 3 min ago")
        #expect(P.cardSubtitle(card, nowMs: 1_000_000 + 7_200_000) == "app · pid 4242 · 2 h ago")
        #expect(P.cardTitle(card) == "API_KEY")
        var account = card
        account.kind = .account
        account.name = "login"
        #expect(P.cardTitle(account) == "login (account)")
    }

    @Test func theLockRowAndTheQueue() {
        #expect(P.lockText(.unlocked) == "Secrets unlocked")
        #expect(P.lockText(.locked) == "Secrets locked")
        #expect(P.lockText(.missing) == "No secrets database yet")
        #expect(P.lockWarning == "Agents' runs that need secrets will be refused.")
        #expect(P.queueSummary(QueueSnapshot(capacity: 2, running: [], waiting: [], foreignRunning: [],
                                             asOfSeq: 1)) == "No heavy tasks")
        #expect(P.elapsed(sinceMs: 0, nowMs: 125_000) == "2 min")
        #expect(P.elapsed(sinceMs: 0, nowMs: 3_720_000) == "1 h 2 min")
    }

    /// Task 13 F-4: registered devices only; the held, reserved or tested ones first, then the
    /// ready, then the connected; by name without regard to case; the holder chip with them.
    @Test func devicesInShortShowWhatMatters() {
        let chip = WorkspaceChip(workspaceId: "w1", path: "/work/proj13")
        let devices = [
            Self.device("iPad Air", connected: true, kind: .simulator),
            Self.device("iPhone 17", connected: true, kind: .simulator),
            Self.device("Zed tablet", connected: true),
            Self.device("unregistered", connected: true, registered: false),
            Self.device("shut-down-sim", connected: false, kind: .simulator, registered: false),
            Self.device("apple tv", connected: false),
            Self.device("pixel9-v13", connected: true),
        ]
        let standing: (DeviceInfo) -> [DeviceStanding] = { device in
            device.alias == "pixel9-v13" ? [.held(by: [chip], kind: .hold)] : [.free]
        }
        let (shown, more) = P.devices(devices, standing: standing)
        #expect(shown.map { P.deviceName($0) }
            == ["pixel9-v13", "iPad Air", "iPhone 17", "Zed tablet", "apple tv"])
        #expect(more == nil)
        #expect(P.holders(standing(shown[0])) == [chip])
        // Many free devices: the held one is still among the eight.
        let crowd = (0..<12).map { Self.device("a-\($0)", connected: true) } + [devices[6]]
        let (eight, rest) = P.devices(crowd, standing: standing)
        #expect(eight.count == 8)
        #expect(eight.first.map { P.deviceName($0) } == "pixel9-v13")
        #expect(rest == "5 more")
    }

    static func device(_ id: String, connected: Bool, kind: DeviceKind = .physical,
                       registered: Bool = true) -> DeviceInfo {
        DeviceInfo(resource: ResourceId(rawValue: id), hardwareId: id, platform: .android,
                   kind: kind, registered: registered, alias: id, aliasByPerson: true,
                   model: "Pixel", connected: connected, ready: connected, transports: [],
                   bootedByHostler: false, shuttingDown: false, removing: false, sessions: [],
                   checkingSessions: false, firstSeenMs: 0, lastSeenMs: 0)
    }
}
