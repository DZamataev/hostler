import AppKit
import Foundation
import Testing
@testable import HostlerKit

/// Copy and clear (spec §9.6): the pasteboard is cleared after 60 s only if it still holds our
/// copy; the concealed marker is present. A private, uniquely named pasteboard stands in for
/// the general one, so the person's clipboard is never touched.
@Suite(.timeLimit(.minutes(1))) @MainActor struct PasteboardTests {
    struct Board {
        let raw = NSPasteboard(name: NSPasteboard.Name("io.github.dzamataev.hostler.tests.\(UUID().uuidString)"))
        let sleeper = FakeSleeper()
        let writer: SystemPasteboard
        let clipboard: ConcealedClipboard

        @MainActor init() {
            writer = SystemPasteboard(raw)
            clipboard = ConcealedClipboard(pasteboard: writer, sleeper: sleeper)
        }

        func release() { raw.releaseGlobally() }
    }

    @Test func theCopyIsConcealedTextAndClearedAfterSixtySecondsWhenUnchanged() async throws {
        let board = Board()
        defer { board.release() }
        board.clipboard.copy(SecretText("sk-live-123"))
        #expect(board.raw.string(forType: .string) == "sk-live-123")
        #expect(board.raw.types?.contains(SystemPasteboard.concealedType) == true)
        try await storeSoon("the clear is scheduled") { board.sleeper.waitingCount == 1 }
        #expect(board.sleeper.durations == [.seconds(60)])
        #expect(board.raw.string(forType: .string) == "sk-live-123")
        board.sleeper.releaseWaiting()
        try await storeSoon("the pasteboard is cleared") { board.raw.string(forType: .string) == nil }
        #expect(board.raw.types?.isEmpty ?? true)
    }

    @Test func somethingCopiedSinceIsLeftAlone() async throws {
        let board = Board()
        defer { board.release() }
        board.clipboard.copy(SecretText("sk-live-123"))
        try await storeSoon("the clear is scheduled") { board.sleeper.waitingCount == 1 }
        board.raw.clearContents()
        board.raw.setString("the person's own text", forType: .string)
        board.sleeper.releaseWaiting()
        try await Task.sleep(for: .milliseconds(50))
        #expect(board.raw.string(forType: .string) == "the person's own text")
    }

    @Test func aNewerCopyReplacesThePendingClear() async throws {
        let board = Board()
        defer { board.release() }
        board.clipboard.copy(SecretText("first"))
        try await storeSoon("the first clear is scheduled") { board.sleeper.waitingCount == 1 }
        board.clipboard.copy(SecretText("second"))
        try await storeSoon("the second clear is scheduled") { board.sleeper.durations.count == 2 }
        // The first wait was cancelled; the second one clears.
        board.sleeper.releaseWaiting()
        try await storeSoon("the pasteboard is cleared") { board.raw.string(forType: .string) == nil }
    }

    @Test func quittingClearsOurCopyAtOnceButNotTheirs() {
        let board = Board()
        defer { board.release() }
        board.clipboard.copy(SecretText("ours"))
        board.clipboard.clearIfStillOurs()
        #expect(board.raw.string(forType: .string) == nil)

        board.clipboard.copy(SecretText("ours again"))
        board.raw.clearContents()
        board.raw.setString("theirs", forType: .string)
        board.clipboard.clearIfStillOurs()
        #expect(board.raw.string(forType: .string) == "theirs")
    }
}
