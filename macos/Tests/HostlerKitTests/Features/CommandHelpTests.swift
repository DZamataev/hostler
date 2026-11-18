import Foundation
import Testing
@testable import HostlerKit

@Suite struct CommandHelpTests {
    /// The plan's rule: every action has an entry and each text starts with `hostler `.
    @Test func everyActionNamesACommand() {
        #expect(!CommandAction.allCases.isEmpty)
        for action in CommandAction.allCases {
            #expect(action.template.hasPrefix("hostler "), "\(action.rawValue): \(action.template)")
        }
    }

    /// The spellings the Rust CLI parses (`crates/hostler/src/cli`).
    @Test func theTableHasTheCLISSpellings() {
        #expect(CommandAction.reserveDevice.template == "hostler devices reserve <alias>")
        #expect(CommandAction.unreserveDevice.template == "hostler devices unreserve <alias>")
        #expect(CommandAction.renameDevice.template == "hostler devices rename <alias> <new>")
        #expect(CommandAction.removeDevice.template == "hostler devices remove <alias>")
        // The note is a positional argument; Clear is the `--clear` flag (fix R1-7).
        #expect(CommandAction.noteDevice.template
            == "hostler devices note <alias> \"<note>\"")
        #expect(CommandAction.clearNote.template == "hostler devices note <alias> --clear")
        #expect(CommandAction.takeOverDevice.template
            == "hostler hold device <alias> --force --confirmation <token> --ttl 30")
        #expect(CommandAction.releaseClaim.template
            == "hostler release <claim> --force --confirmation <token>")
        #expect(CommandAction.renewHold.template == "hostler hold renew <claim>")
        #expect(CommandAction.forgetSessions.template
            == "hostler devices forget-sessions <alias>")
        #expect(CommandAction.runDoctor.template == "hostler doctor")
        #expect(CommandAction.listDevices.template == "hostler devices list --all")
        #expect(CommandAction.addDevice.template == "hostler devices add <device> --alias <new>")
    }

    @Test func thePlaceholdersAreFilledIn() {
        #expect(CommandAction.reserveDevice.text(device: "pixel")
            == "hostler devices reserve pixel")
        #expect(CommandAction.renameDevice.text(device: "pixel", newName: "phone")
            == "hostler devices rename pixel phone")
        #expect(CommandAction.takeOverDevice.text(device: "pixel", token: "pixel")
            == "hostler hold device pixel --force --confirmation pixel --ttl 30")
        #expect(CommandAction.releaseClaim.text(claim: "c1", token: "pixel")
            == "hostler release c1 --force --confirmation pixel")
        #expect(CommandAction.noteDevice.text(device: "pixel", note: "kitchen")
            == "hostler devices note pixel \"kitchen\"")
        #expect(CommandAction.clearNote.text(device: "pixel")
            == "hostler devices note pixel --clear")
        #expect(CommandAction.runDoctor.text() == "hostler doctor")
        #expect(CommandAction.listDevices.text() == "hostler devices list --all")
    }

    @Test func theWorkspaceCommandsSayWhereTheyRun() {
        #expect(CommandAction.reserveDevice.text(device: "pixel", workspace: "app")
            == "hostler devices reserve pixel (in app)")
        #expect(CommandAction.takeOverDevice.text(device: "pixel", token: "pixel",
                                                 workspace: "app")
            == "hostler hold device pixel --force --confirmation pixel --ttl 30 (in app)")
        #expect(CommandAction.runDoctor.text(workspace: "app") == "hostler doctor (in app)")
        // The ones that name their resource themselves get no note.
        #expect(CommandAction.renewHold.text(claim: "c1", workspace: "app")
            == "hostler hold renew c1")
        #expect(CommandAction.removeDevice.text(device: "pixel", workspace: "app")
            == "hostler devices remove pixel")
    }

    @Test func theActionsThatNeedAWorkspaceAreMarked() {
        let inWorkspace = CommandAction.allCases.filter(\.runsInTheWorkspace)
        #expect(Set(inWorkspace) == [.reserveDevice, .unreserveDevice, .takeOverDevice, .runDoctor,
                                     .takeOverScreen, .approveProject])
    }

    @Test func theHelpEntryPointIsTheSameTable() {
        #expect(CommandHelp.text(.reserveDevice, device: "pixel", workspace: "app")
            == CommandAction.reserveDevice.text(device: "pixel", workspace: "app"))
    }

    /// Fix R1-7: one fixture (`crates/hostler/tests/fixtures/command-help.json`) lists every
    /// tooltip; this test compares the Swift table to it, and `crates/hostler/tests/
    /// command_help.rs` parses each entry's argv with the real clap definitions.
    @Test func theFixtureMatchesTheTableAndTheCLIParsesIt() throws {
        let fixture = try Self.fixture()
        #expect(fixture.count == CommandAction.allCases.count)
        var seen: Set<String> = []
        for entry in fixture {
            let action = try #require(CommandAction(rawValue: entry.action),
                                      "unknown action \(entry.action)")
            #expect(seen.insert(action.rawValue).inserted, "\(entry.action) is listed twice")
            #expect(action.template == entry.command, "\(entry.action)")
        }
        #expect(seen.count == CommandAction.allCases.count, "every action is in the fixture")
    }

    // MARK: The fixture

    private struct Entry {
        var action: String
        var command: String
        var argv: [String]
    }

    private static func fixture() throws -> [Entry] {
        // The fixture is shared with the Rust test, so it lives in the crate's tests.
        var directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        for _ in 0..<8 {
            let candidate = directory.appendingPathComponent(
                "crates/hostler/tests/fixtures/command-help.json")
            if FileManager.default.fileExists(atPath: candidate.path) {
                let data = try Data(contentsOf: candidate)
                let raw = try JSONSerialization.jsonObject(with: data) as? [[String: Any]]
                return try (raw ?? []).map { item in
                    Entry(action: try #require(item["action"] as? String),
                          command: try #require(item["command"] as? String),
                          argv: try #require(item["argv"] as? [String]))
                }
            }
            directory.deleteLastPathComponent()
        }
        throw FixtureNotFound()
    }

    private struct FixtureNotFound: Error {}
}
