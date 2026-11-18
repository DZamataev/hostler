import Foundation
import HostlerKit
import Testing
@testable import Hostler

/// Task 13 F-1: every show of the popover is a new session (a fresh root view, the "opened"
/// hook) and every close ends it (the "closed" hook), however often it is shown again.
@Suite @MainActor struct PanelSessionTests {
    @MainActor final class Recorder: PanelLifecycle {
        private(set) var events: [String] = []
        func popoverOpened() { events.append("opened") }
        func popoverClosed() { events.append("closed") }
    }

    @Test func showCloseShowAgainRunsBothHooksEachTime() {
        let recorder = Recorder()
        let session = PanelSession(lifecycle: recorder)
        let first = session.willShow()
        session.didClose()
        let second = session.willShow()
        session.didClose()
        #expect(recorder.events == ["opened", "closed", "opened", "closed"])
        #expect(first != second)
        #expect(!session.isOpen)
    }

    @Test func aCloseWithoutAShowAndASecondCloseDoNothing() {
        let recorder = Recorder()
        let session = PanelSession(lifecycle: recorder)
        session.didClose()
        _ = session.willShow()
        session.didClose()
        session.didClose()
        #expect(recorder.events == ["opened", "closed"])
    }

    @Test func aShowWhileShownIsANewRootWithoutASecondOpen() {
        let recorder = Recorder()
        let session = PanelSession(lifecycle: recorder)
        let first = session.willShow()
        let again = session.willShow()
        #expect(again != first)
        #expect(recorder.events == ["opened"])
    }

    /// The controller builds every show's root through `panel(for:maxHeight:session:)`, keyed
    /// by the session, and calls the session on its show and close paths.
    @Test func theControllerUsesTheSessionOnItsShowAndClosePaths() throws {
        let source = try String(contentsOf: URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "Sources/Hostler/App/StatusItemController.swift"), encoding: .utf8)
        #expect(source.contains("let shown = session.willShow()"))
        #expect(source.contains("Self.panel(for: model, maxHeight: maxPanelHeight(for: button), session: shown)"))
        #expect(source.contains(".id(session)"))
        let didClose = try #require(source.range(of: "func popoverDidClose("))
        #expect(source[didClose.upperBound...].prefix(400).contains("session.didClose()"))
    }
}
