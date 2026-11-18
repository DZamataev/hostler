import Foundation
import Testing
@testable import HostlerKit

/// The All Devices presentation (plan Task 10): the State texts, the filters, the sort keys, the
/// standing chips, the counts, the telemetry and install rows.
@Suite struct DevicesTablePresentationTests {
    let now: Int64 = Mk.now

    fileprivate func device(
        _ id: String, alias: String? = nil, platform: Platform = .android,
        kind: DeviceKind = .physical, registered: Bool = true, connected: Bool = true,
        ready: Bool = true, transports: [TransportKind] = [], telemetry: Telemetry? = nil,
        sessions: [AgentDeviceSession] = [], shuttingDown: Bool = false
    ) -> DeviceInfo {
        DeviceInfo(
            resource: ResourceId(rawValue: id), hardwareId: "HW-\(id)", platform: platform,
            kind: kind, registered: registered, alias: alias, aliasByPerson: alias != nil,
            model: "Model \(id)", name: "Name \(id)", osVersion: "16",
            note: nil, connected: connected, ready: ready,
            transports: transports.map { Transport(kind: $0, address: "a", state: "connected") },
            coredeviceId: nil, telemetry: telemetry, bootedByHostler: false,
            shuttingDown: shuttingDown, removing: false, sessions: sessions,
            checkingSessions: false, firstSeenMs: now, lastSeenMs: now)
    }

    fileprivate func telemetry(battery: ReadingUInt8 = .known(82),
                               charging: ReadingBool = .known(true),
                               wifi: ReadingBool = .known(true),
                               wifiName: String? = "home",
                               cellular: ReadingBool = .known(false),
                               vpn: ReadingBool = .unknown) -> Telemetry {
        Telemetry(batteryPercent: battery, charging: charging, wifi: wifi, wifiName: wifiName,
                  cellularData: cellular, vpn: vpn, atMs: now - 5000)
    }

    // MARK: State

    @Test func aConnectedDeviceNamesItsTransport() {
        #expect(DeviceState.of(device("d1", transports: [.usb])).text == "connected · usb")
        #expect(DeviceState.of(device("d1", transports: [.wifi])).text == "connected · Wi-Fi")
        #expect(DeviceState.of(device("d1", transports: [.network])).text
            == "connected · network")
        #expect(DeviceState.of(device("d1")).text == "connected")
        // A cable wins over Wi-Fi when a device reports both.
        #expect(DeviceState.of(device("d1", transports: [.wifi, .usb])).text
            == "connected · usb")
        #expect(DeviceState.of(device("d1", transports: [.wifi, .usb])).transport == .usb)
    }

    @Test func aConnectedDeviceThatIsNotReadyIsBooting() {
        let state = DeviceState.of(device("d1", connected: true, ready: false,
                                         transports: [.emulator]))
        #expect(state.text == "booting")
        #expect(state.isReachable)
        #expect(state.phase == .booting)
    }

    @Test func theOtherPhases() {
        // A virtual device hostler knows but that is off is "shut down", not "booting".
        #expect(DeviceState.of(device("s", kind: .simulator, connected: true, ready: false)).text
            == "shut down")
        #expect(DeviceState.of(device("e", kind: .emulator, connected: true, ready: false)).text
            == "shut down")
        #expect(DeviceState.of(device("d1", connected: false)).text == "not connected")
        #expect(DeviceState.of(device("d1", kind: .simulator, connected: false)).text
            == "shut down")
        #expect(DeviceState.of(device("d1", kind: .emulator, connected: false)).text == "shut down")
        #expect(DeviceState.of(device("d1", connected: false, shuttingDown: true)).text
            == "shutting down")
        #expect(!DeviceState.of(device("d1", connected: false)).isReachable)
        #expect(DeviceState.of(device("d1", kind: .simulator, connected: false)).phase == .shutDown)
    }

    // MARK: Filters

    @Test func thePlatformFilterTakesOnePlatform() {
        let ios = device("ios", platform: .ios)
        #expect(PlatformFilter.all.includes(.ios) && PlatformFilter.all.includes(.android))
        #expect(PlatformFilter.ios.includes(ios.platform) && !PlatformFilter.ios.includes(.android))
        #expect(DeviceFilter(platform: .android).apply([device("a"), ios]).map(\.resource.rawValue)
            == ["a"])
    }

    @Test func theKindFilterPutsEmulatorsAndSimulatorsUnderVirtual() {
        let physical = device("p", kind: .physical)
        let emulator = device("e", kind: .emulator)
        let simulator = device("s", kind: .simulator)
        let devices = [physical, emulator, simulator]
        #expect(devices.filter { KindFilter.physical.includes($0.kind) }
            .map(\.resource.rawValue) == ["p"])
        #expect(devices.filter { KindFilter.virtual.includes($0.kind) }
            .map(\.resource.rawValue) == ["e", "s"])
        #expect(devices.filter { KindFilter.all.includes($0.kind) }.count == 3)
    }

    /// Fix F-3: the default view is `hostler devices list` — a device that is registered **or
    /// physical** — and the switch is `--all`, adding the unregistered emulators and simulators.
    @Test func theDefaultViewIsTheCLIsListAndTheSwitchIsAll() {
        let registered = device("r", registered: true)
        let freshPhysical = device("f", registered: false)
        let freshPhysicalOff = device("off", registered: false, connected: false)
        let simulator = device("s", kind: .simulator, registered: false)
        let emulator = device("e", kind: .emulator, registered: false)
        let all = [registered, freshPhysical, freshPhysicalOff, simulator, emulator]
        #expect(DeviceFilter().apply(all).map(\.resource.rawValue) == ["r", "f", "off"])
        #expect(DeviceFilter(showsUnregistered: true).apply(all).map(\.resource.rawValue)
            == ["r", "f", "off", "s", "e"])
        // The platform and kind filters apply either way.
        #expect(DeviceFilter(kind: .physical).apply(all).map(\.resource.rawValue)
            == ["r", "f", "off"])
        #expect(DeviceFilter(kind: .virtual).apply(all).isEmpty)
        #expect(DeviceFilter(kind: .virtual, showsUnregistered: true).apply(all)
            .map(\.resource.rawValue) == ["s", "e"])
    }

    @Test func theFiltersCombine() {
        let devices = [device("a"), device("b", platform: .ios),
                       device("c", platform: .ios, kind: .simulator, connected: false),
                       device("d", platform: .ios, registered: false)]
        let filter = DeviceFilter(platform: .ios, kind: .virtual, showsUnregistered: false)
        #expect(filter.apply(devices).map(\.resource.rawValue) == ["c"])
    }

    // MARK: Sorting

    @Test func theStateColumnPutsReachableDevicesFirst() {
        let rows = deviceRows([
            device("shut", kind: .simulator, connected: false),
            device("gone", connected: false),
            device("booting", connected: true, ready: false),
            device("here", transports: [.usb]),
        ], standingOf: { _ in [] })
        let sorted = DeviceSort(column: .state).sorted(rows)
        #expect(sorted.map(\.id) == ["here", "booting", "gone", "shut"])
        #expect(DeviceSort(column: .state, ascending: false).sorted(rows).map(\.id)
            == ["shut", "gone", "booting", "here"])
    }

    @Test func theDeviceColumnSortsByTheNameItShows() {
        let rows = deviceRows([device("z", alias: "beta"), device("a", alias: "Alpha"),
                               device("m", alias: "gamma")], standingOf: { _ in [] })
        #expect(DeviceSort(column: .device).sorted(rows).map(\.name)
            == ["Alpha", "beta", "gamma"])
        #expect(DeviceSort(column: .device, ascending: false).sorted(rows).map(\.name)
            == ["gamma", "beta", "Alpha"])
    }

    @Test func theBatteryColumnPutsUnknownLast() {
        let rows = deviceRows([
            device("low", telemetry: telemetry(battery: .known(20))),
            device("none"),
            device("high", telemetry: telemetry(battery: .known(90))),
            device("unknown", telemetry: telemetry(battery: .unknown)),
        ], standingOf: { _ in [] })
        #expect(DeviceSort(column: .battery).sorted(rows).map(\.id)
            == ["low", "high", "none", "unknown"])
    }

    /// The window's `Table` sorts by these key paths through its own headers; the pure sort must
    /// agree, so a click on a header and the tested rule are one rule.
    @Test func theTablesSortKeysAgreeWithThePureOrder() {
        let reserved = WorkspaceChip(workspaceId: "w1", path: "/work/app")
        let rows = deviceRows([
            device("zulu", platform: .ios, kind: .simulator, connected: true, ready: false,
                   telemetry: telemetry(battery: .known(15))),
        ], standingOf: { _ in [.waiting(count: 1)] }) + deviceRows([
            device("alpha", platform: .android, kind: .physical, connected: true, ready: true,
                   telemetry: telemetry(battery: .known(88))),
            device("mike", platform: .ios, kind: .physical, connected: true, ready: false),
        ], standingOf: { device in
            device.resource.rawValue == "alpha" ? [.reserved(by: reserved)] : [.free]
        })
        let comparators: [DeviceColumn: [KeyPathComparator<DeviceRow>]] = [
            .device: [KeyPathComparator(\.nameSortKey)],
            .type: [KeyPathComparator(\.typeSortKey)],
            .state: [KeyPathComparator(\.stateSortRank)],
            .standing: [KeyPathComparator(\.standingSortKey)],
            .battery: [KeyPathComparator(\.batterySortKey)],
        ]
        for column in DeviceColumn.allCases {
            let table = rows.sorted(using: comparators[column] ?? [])
            #expect(table.map(\.id) == DeviceSort(column: column).sorted(rows).map(\.id),
                    "\(column.rawValue)")
        }
    }

    @Test func theTypeAndStandingColumnsSortByTheirText() {
        let state = Mk.state("a", claims: [], waiting: 0)
        let rows = deviceRows(
            [device("ios", platform: .ios), device("android"),
             device("held", alias: "zeta")],
            standingOf: { device in
                device.resource.rawValue == "held"
                    ? [.reserved(by: WorkspaceChip(workspaceId: "w1", path: "/work/app"))]
                    : [.free]
            })
        #expect(DeviceSort(column: .type).sorted(rows).map(\.id) == ["android", "held", "ios"])
        // The column sorts by the chips' text: "app reserved" before "free".
        #expect(DeviceSort(column: .standing).sorted(rows).map(\.id) == ["held", "android", "ios"])
        _ = state
    }

    // MARK: Counts and new devices

    @Test func theSubtitleCountsReachableAndClaimed() {
        let devices = [device("a"), device("b", connected: false, ready: false),
                       device("c", kind: .simulator, connected: false, ready: false)]
        let claims = Mk.claims(seq: 1, [
            Mk.state("a", claims: [Mk.claim("c1", on: "a", kind: .lease, workspace: "w1",
                                            path: "/work/app")]),
            Mk.state("b"),
        ])
        let counts = deviceCounts(devices, claims: claims)
        #expect(counts.reachable == 1)
        #expect(counts.claimed == 1)
        #expect(counts.text == "1 reachable · 1 claimed")
    }

    @Test func aForeignClaimCountsAsClaimed() {
        let claims = Mk.claims(seq: 1, [ResourceState(
            resource: Mk.resource("a"), claims: [], foreign: [
                ForeignClaim(resource: ResourceId(rawValue: "a"), source: "rn-looky",
                             description: "a build"),
            ], waiting: [], seizing: false)])
        #expect(deviceCounts([device("a")], claims: claims).claimed == 1)
    }

    @Test func theAddBarListsConnectedUnregisteredPhysicalDevices() {
        let devices = [device("r"), device("f1", registered: false),
                       device("f2", registered: false, connected: false),
                       // A booted simulator hostler has not registered is `--all` territory, not
                       // a new device to add (fix F-3).
                       device("sim", kind: .simulator, registered: false)]
        #expect(newDevices(devices).map(\.resource.rawValue) == ["f1"])
    }

    // MARK: The note

    /// Fix F-7: the field follows the service's note, and a person's unsaved edit stays.
    @Test func theNoteFieldFollowsTheServiceUnlessEdited() {
        #expect(NoteField.text(serviceNote: "service", keepsEdit: false, field: "typed")
            == "service")
        #expect(NoteField.text(serviceNote: "service", keepsEdit: true, field: "typed")
            == "typed")
        #expect(NoteField.text(serviceNote: nil, keepsEdit: false, field: "typed") == "")
        #expect(NoteField.text(serviceNote: nil, keepsEdit: true, field: "typed") == "typed")
        #expect(NoteField.text(serviceNote: "s", keepsEdit: false, field: "") == "s")
    }

    // MARK: Chips

    @Test func freeAndReservedChips() {
        #expect(standingChips([.free]).map(\.text) == ["free"])
        let chip = WorkspaceChip(workspaceId: "w1", path: "/work/app")
        let chips = standingChips([.reserved(by: chip)])
        #expect(chips.map(\.text) == ["app reserved"])
        #expect(chips.first?.color == chip.color)
        #expect(chips.first?.color != nil)
        #expect(chips.first?.symbol == .pin)
    }

    @Test func heldChipsUseTheWorkspaceColorAndTheRunsWord() {
        let app = WorkspaceChip(workspaceId: "w1", path: "/work/app")
        let other = WorkspaceChip(workspaceId: "w2", path: "/work/other")
        let running = standingChips([.held(by: [app, other], kind: .lease)])
        #expect(running.map(\.text) == ["app running", "other running"])
        #expect(running.map(\.color) == [app.color, other.color])
        #expect(standingChips([.held(by: [app], kind: .hold)]).map(\.text) == ["app hold"])
    }

    @Test func theAgentDeviceChipHasItsSessionAndTheWorkspace() {
        let by = WorkspaceChip(workspaceId: "w1", path: "/work/app")
        let chips = standingChips([.testing(session: "s1", by: by)])
        #expect(chips.map(\.text) == ["agent-device (s1)", "app"])
        #expect(chips.first?.symbol == .handTap)
        #expect(chips.first?.isSource == true)
        #expect(chips.last?.color == by.color)
    }

    @Test func foreignAndWaitingChips() {
        let by = WorkspaceChip(workspaceId: "w1", path: "/work/app")
        #expect(standingChips([.foreign(source: "rn-looky", by: by)]).map(\.text)
            == ["rn-looky", "app"])
        #expect(standingChips([.foreign(source: "x", by: nil)]).map(\.text) == ["x"])
        #expect(standingChips([.waiting(count: 1)]).map(\.text) == ["1 waiting"])
        #expect(standingChips([.waiting(count: 3)]).first?.symbol == .clock)
    }

    @Test func theConnectionStateAddsNoChip() {
        #expect(standingChips([.notConnected]).isEmpty)
        #expect(standingChips([.notBooted]).isEmpty)
        let chips = standingChips([.notConnected,
                                   .reserved(by: WorkspaceChip(workspaceId: "w", path: "/w"))])
        #expect(chips.map(\.text) == ["w reserved"])
    }

    // MARK: Telemetry and installs

    @Test func theTelemetrySectionSaysUnknownForAnUnknownReading() {
        let facts = TelemetryFacts.of(nil, nowMs: now)
        #expect(facts.battery == "unknown" && facts.charging == "unknown"
            && facts.wifi == "unknown" && facts.cellular == "unknown" && facts.vpn == "unknown")
        #expect(facts.takenText == nil)

        let read = TelemetryFacts.of(telemetry(), nowMs: now)
        #expect(read.battery == "82%")
        #expect(read.charging == "yes")
        #expect(read.wifi == "yes")
        #expect(read.wifiName == "home")
        #expect(read.cellular == "no")
        #expect(read.vpn == "unknown")
        #expect(read.takenText == "just now")
    }

    @Test func theBatteryColumnText() {
        #expect(batteryText(nil) == nil)
        #expect(batteryText(telemetry(battery: .known(5))) == "5%")
        #expect(batteryText(telemetry(battery: .unknown)) == "unknown")
        #expect(batteryText(telemetry(battery: .emulated)) == "emulated")
        #expect(batteryPercent(telemetry(battery: .known(44))) == 44)
        #expect(batteryPercent(telemetry(battery: .unknown)) == nil)
        let row = deviceRows([device("a", telemetry: telemetry(battery: .known(44)))],
                             standingOf: { _ in [] }).first
        #expect(row?.isCharging == true)
        #expect(row?.batteryText == "44%")
    }

    @Test func installRowsCarryTheOrigin() {
        let id = ResourceId(rawValue: "d1")
        let installs = [
            InstallEvent(id: "i1", device: id, appId: "com.example.app", change: .installed,
                         versionName: "1.2", versionCode: "120", when: .device(atMs: now - 120_000),
                         source: .claim(claimIds: ["c"], workspacePath: "/work/app",
                                        branch: "dev/4.13", commit: "abcdef123456",
                                        command: "make device"), recordedAtMs: now),
            InstallEvent(id: "i2", device: id, appId: "com.other", change: .updated,
                         versionName: nil, versionCode: nil, when: .firstSeen(atMs: now),
                         source: .notThroughHostler, recordedAtMs: now),
            InstallEvent(id: "i3", device: ResourceId(rawValue: "other"), appId: "x",
                         change: .removed, when: .device(atMs: now), source: .notThroughHostler,
                         recordedAtMs: now),
        ]
        let workspaces = [Mk.workspace("w1", project: "p", path: "/work/app")]
        let rows = installRows(of: device("d1"), installs: installs, nowMs: now,
                               workspaces: workspaces)
        #expect(rows.map(\.id) == ["i1", "i2"])
        #expect(rows[0].appId == "com.example.app")
        #expect(rows[0].versionText == "1.2 (120)")
        #expect(rows[0].changeText == "installed")
        #expect(rows[0].timeText == "2 min")
        #expect(rows[0].sourceText == "app · dev/4.13 · abcdef1 · make device")
        #expect(rows[0].workspace?.color != nil)
        #expect(rows[0].isAttributed)
        #expect(rows[1].versionText == nil)
        #expect(rows[1].sourceText == "not through hostler")
        #expect(rows[1].workspace == nil)
        #expect(!rows[1].isAttributed)
        #expect(rows[1].timeText == "first seen just now")
    }

    @Test func aSimulatorsRuntimeReadsAsItsVersion() {
        #expect(DeviceInfo.osText(nil) == nil)
        #expect(DeviceInfo.osText("") == nil)
        #expect(DeviceInfo.osText("17") == "17")
        #expect(DeviceInfo.osText("com.apple.CoreSimulator.SimRuntime.iOS-26-5")
            == "iOS 26.5")
        #expect(DeviceInfo.osText("com.apple.CoreSimulator.SimRuntime.watchOS-11-0")
            == "watchOS 11.0")
    }

    @Test func theNameAndSummaryOfARow() {
        let row = deviceRows([device("d1", alias: "kitchen")], standingOf: { _ in [] }).first
        #expect(row?.name == "kitchen")
        #expect(row?.subtitle == "Android · Model d1, 16")
        #expect(row?.typeText == "Android · physical")
        #expect(row?.isUnregistered == false)
        #expect(row?.batteryText == nil)
    }

    @Test func theClaimRowsOfTheDetail() {
        let claim = Claim(
            id: "c1", resource: Mk.resource("d1"), kind: .lease,
            workspace: WorkspaceRef(id: "w1", path: "/work/app"),
            command: "make device", grantedAtMs: now - 120_000)
        let rows = claimRows([claim], nowMs: now)
        #expect(rows.count == 1)
        #expect(rows[0].workspace.name == "app")
        #expect(rows[0].kindText == "lease")
        #expect(rows[0].sinceText == "2 min")
        #expect(rows[0].command == "make device")
        #expect(rows[0].ttlText == nil)
    }

    @Test func aTTLClaimSaysHowLongIsLeft() {
        let claim = Claim(
            id: "c1", resource: Mk.resource("d1"), kind: .hold,
            workspace: WorkspaceRef(id: "w1", path: "/work/app"),
            binding: .ttl(ttlMs: 1_800_000, expiresAtMs: now + 600_000), grantedAtMs: now)
        let row = claimRows([claim], nowMs: now)[0]
        #expect(row.ttlText == "10 min left")
        #expect(row.expiresAtMs == now + 600_000)
        #expect(row.canRenew)

        let process = Claim(
            id: "c2", resource: Mk.resource("d1"), kind: .hold,
            workspace: WorkspaceRef(id: "w1", path: "/work/app"),
            binding: .process(holder: Mk.pid), grantedAtMs: now)
        #expect(claimRows([process], nowMs: now)[0].ttlText == "while its process lives")
        #expect(!claimRows([process], nowMs: now)[0].canRenew)
    }

    // MARK: Reading state and history

    @Test func unreadIsNotTheSameAsEmpty() {
        let since = Date(timeIntervalSince1970: 1_000)
        #expect(SnapshotState(hasSnapshot: false, syncState: .live) == .loading)
        #expect(SnapshotState(hasSnapshot: false, syncState: .syncing(catchingUp: false))
            == .loading)
        #expect(SnapshotState(hasSnapshot: false, syncState: .syncing(catchingUp: true))
            == .catchingUp)
        #expect(SnapshotState(hasSnapshot: false, syncState: .offline(since: since)) == .offline)
        #expect(SnapshotState(hasSnapshot: false,
                              syncState: .failed(.service(message: "no", recovery: nil)))
            == .failed)
        #expect(SnapshotState(hasSnapshot: true, syncState: .offline(since: since)) == .loaded)
        #expect(SnapshotState.loaded.isLoaded)
        #expect(!SnapshotState.loading.isLoaded)
        #expect(SnapshotState.loading.title == "Reading…")
        #expect(SnapshotState.offline.detail.contains("nothing has been read"))
    }

    @Test func theHistoryOfADeviceMatchesItsStableIDOnly() {
        let history = [
            ClaimHistoryItem(id: "h1", resource: "old-alias",
                             resourceId: ResourceId(rawValue: "device:android:1"),
                             kind: .lease, workspacePath: "/work/app", endedAtMs: 3,
                             reason: .released),
            ClaimHistoryItem(id: "h2", resource: "screen",
                             resourceId: ResourceId(rawValue: "screen"), kind: .hold,
                             workspacePath: "/work/app", endedAtMs: 2, reason: .released),
            // Recorded before P5-L16: no stable id, and the display is not a guess.
            ClaimHistoryItem(id: "h3", resource: "device:android:1", kind: .lease,
                             workspacePath: "/work/app", endedAtMs: 1, reason: .released),
        ]
        let rows = claimHistory(of: ResourceId(rawValue: "device:android:1"), in: history)
        #expect(rows.map(\.id) == ["h1"])
    }

    @Test func theSpans() {
        #expect(Span.since(now - 500, nowMs: now) == "just now")
        #expect(Span.since(now - 120_000, nowMs: now) == "2 min")
        #expect(Span.since(now - 7_200_000, nowMs: now) == "2 h")
        #expect(Span.since(now - 3 * 86_400_000, nowMs: now) == "3 d")
        #expect(Span.since(now + 10_000, nowMs: now) == "just now")
        #expect(Span.remaining(now - 1, nowMs: now) == "ended")
        #expect(Span.remaining(now + 30_000, nowMs: now) == "less than a minute left")
        #expect(Span.remaining(now + 7_200_000, nowMs: now) == "2 h left")
        #expect(Span.remaining(now + 3 * 86_400_000, nowMs: now) == "3 days left")
        #expect(Span.duration(42_000) == "42 s")
        #expect(Span.duration(200_000) == "3 min 20 s")
        #expect(Span.duration(3_600_000) == "1 h")
    }
}
