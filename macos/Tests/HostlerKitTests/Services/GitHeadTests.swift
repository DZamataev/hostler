import Foundation
import Testing
@testable import HostlerKit

@Suite struct GitHeadTests {
    /// A fresh temp directory, removed after `body`.
    private func withTemp(_ body: (URL) throws -> Void) throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("hostler-githead-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try body(dir)
    }

    private func write(_ text: String, to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(text.utf8).write(to: url)
    }

    @Test func aMainCheckout() throws {
        try withTemp { dir in
            try write("ref: refs/heads/feature/login\n", to: dir.appendingPathComponent(".git/HEAD"))
            #expect(FileGitHead().branch(at: dir) == "feature/login")
        }
    }

    @Test func aLinkedWorktreeWithAnAbsoluteAndARelativeGitdir() throws {
        try withTemp { dir in
            let main = dir.appendingPathComponent("main")
            let gitdir = main.appendingPathComponent(".git/worktrees/wt")
            try write("ref: refs/heads/p5-t8\n", to: gitdir.appendingPathComponent("HEAD"))
            let worktree = dir.appendingPathComponent("wt")
            try write("gitdir: \(gitdir.path)\n", to: worktree.appendingPathComponent(".git"))
            #expect(FileGitHead().branch(at: worktree) == "p5-t8")

            let relative = dir.appendingPathComponent("rel")
            try write("gitdir: ../main/.git/worktrees/wt\n", to: relative.appendingPathComponent(".git"))
            #expect(FileGitHead().branch(at: relative) == "p5-t8")
        }
    }

    @Test func aDetachedHeadIsTheShortSha() throws {
        try withTemp { dir in
            try write("c520ff7d5a1b2c3d4e5f60718293a4b5c6d7e8f9\n", to: dir.appendingPathComponent(".git/HEAD"))
            #expect(FileGitHead().branch(at: dir) == "c520ff7")
        }
    }

    @Test func aMissingOrUnreadableGitIsNil() throws {
        try withTemp { dir in
            #expect(FileGitHead().branch(at: dir) == nil)
            #expect(FileGitHead().branch(at: dir.appendingPathComponent("absent")) == nil)
            try write("not a gitdir line\n", to: dir.appendingPathComponent("odd/.git"))
            #expect(FileGitHead().branch(at: dir.appendingPathComponent("odd")) == nil)
            try write("garbage\n", to: dir.appendingPathComponent("bad/.git/HEAD"))
            #expect(FileGitHead().branch(at: dir.appendingPathComponent("bad")) == nil)
        }
    }

    @Test func headLines() {
        #expect(FileGitHead.branch(fromHead: "ref: refs/heads/main") == "main")
        #expect(FileGitHead.branch(fromHead: "ref: refs/remotes/origin/x") == "refs/remotes/origin/x")
        #expect(FileGitHead.branch(fromHead: "abc") == nil)
    }
}
