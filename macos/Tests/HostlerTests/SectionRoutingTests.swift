import HostlerKit
import Testing
@testable import Hostler

/// The window's routing of a notification's click (plan Task 11; spec §9.4: "a notification
/// opens the popover or the matching window section"). `AppModel.open` sets
/// `requestedSection`; the sidebar follows it.
@Suite struct SectionRoutingTests {
    @Test func aQueueClickSelectsTheQueue() {
        var section: WindowSection? = .allDevices
        var device: String? = "device:android:1"
        SectionRouting.apply(.queue, section: &section, device: &device)
        #expect(section == .queue)
        // The device the person had open is left as it was.
        #expect(device == "device:android:1")
    }

    @Test func aDeviceClickSelectsAllDevicesAndThatDevice() {
        var section: WindowSection? = .queue
        var device: String?
        SectionRouting.apply(.device(ResourceId("device:android:1")), section: &section,
                             device: &device)
        #expect(section == .allDevices)
        #expect(device == "device:android:1")
    }

    @Test func aRequestClickChangesNothingHere() {
        var section: WindowSection? = .projects
        var device: String? = "kept"
        SectionRouting.apply(.popover, section: &section, device: &device)
        #expect(section == .projects)
        #expect(device == "kept")
    }
}
