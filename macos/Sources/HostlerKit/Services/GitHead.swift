import Foundation

/// A workspace's branch, read from its git files (plan Task 8). OS-facing (the file system),
/// so it sits behind a protocol; tests use temp directories or `FakeGitHead`.
public protocol GitHeadReading: Sendable {
    /// The checkout's branch (`refs/heads/x` → `x`), the short sha of a detached head, or
    /// `nil` when `checkout` has no readable `.git`.
    func branch(at checkout: URL) -> String?
}

/// Reads `.git/HEAD`, or for a linked worktree the `gitdir:` that its `.git` file names and
/// that directory's `HEAD`. Read-only: it never runs `git`.
public struct FileGitHead: GitHeadReading {
    public init() {}

    public func branch(at checkout: URL) -> String? {
        let dotGit = checkout.appendingPathComponent(".git")
        guard let gitDir = Self.gitDir(of: dotGit, checkout: checkout) else { return nil }
        guard let head = Self.firstLine(of: gitDir.appendingPathComponent("HEAD")) else {
            return nil
        }
        return Self.branch(fromHead: head)
    }

    /// `HEAD`'s first line → the branch name, or the short sha of a detached head.
    static func branch(fromHead head: String) -> String? {
        let refPrefix = "ref:"
        if head.hasPrefix(refPrefix) {
            let ref = head.dropFirst(refPrefix.count).trimmingCharacters(in: .whitespaces)
            let heads = "refs/heads/"
            if ref.hasPrefix(heads) { return String(ref.dropFirst(heads.count)) }
            return ref.isEmpty ? nil : ref
        }
        let sha = head.trimmingCharacters(in: .whitespaces)
        guard sha.count >= 7, sha.allSatisfy(\.isHexDigit) else { return nil }
        return String(sha.prefix(7))
    }

    /// The directory holding `HEAD`: `.git` itself, or the target of a `gitdir:` file
    /// (relative paths are relative to the checkout).
    private static func gitDir(of dotGit: URL, checkout: URL) -> URL? {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: dotGit.path, isDirectory: &isDirectory) else {
            return nil
        }
        if isDirectory.boolValue { return dotGit }
        guard let line = firstLine(of: dotGit), line.hasPrefix("gitdir:") else { return nil }
        let path = line.dropFirst("gitdir:".count).trimmingCharacters(in: .whitespaces)
        guard !path.isEmpty else { return nil }
        if path.hasPrefix("/") { return URL(fileURLWithPath: path, isDirectory: true) }
        return checkout.appendingPathComponent(path, isDirectory: true).standardizedFileURL
    }

    private static func firstLine(of file: URL) -> String? {
        // A HEAD or `.git` file is one short line; anything large is not one.
        guard let data = try? Data(contentsOf: file), data.count <= 4096,
              let text = String(data: data, encoding: .utf8) else { return nil }
        let line = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return line.isEmpty ? nil : line
    }
}
