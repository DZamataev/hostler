import Foundation
import SwiftUI

/// A workspace's color (spec §9.3, §9.5): derived from its id, so the same workspace has the
/// same color in the sidebar dot and in every chip, on every run.
public enum WorkspaceColor: Int, CaseIterable, Sendable, Hashable {
    case blue, purple, pink, red, orange, yellow, green, mint, teal, indigo

    /// The color of the workspace with this id: FNV-1a (64-bit) of the id's UTF-8 bytes,
    /// modulo the palette.
    public init(workspaceId: String) {
        let index = Int(WorkspaceColor.fnv1a(workspaceId) % UInt64(WorkspaceColor.allCases.count))
        self = WorkspaceColor.allCases[index]
    }

    /// FNV-1a, 64-bit. Stable across runs and machines (Swift's `hashValue` is not).
    static func fnv1a(_ text: String) -> UInt64 {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in text.utf8 {
            hash ^= UInt64(byte)
            hash &*= 0x0000_0100_0000_01b3
        }
        return hash
    }

    /// The system color (light and dark adapt by themselves).
    public var color: Color {
        switch self {
        case .blue: .blue
        case .purple: .purple
        case .pink: .pink
        case .red: .red
        case .orange: .orange
        case .yellow: .yellow
        case .green: .green
        case .mint: .mint
        case .teal: .teal
        case .indigo: .indigo
        }
    }
}

/// A workspace shown as a chip (DeviceDeck's holder chip): its folder name in its color.
/// `color` is `nil` for a path hostler has no registered workspace for.
public struct WorkspaceChip: Hashable, Sendable {
    public var workspaceId: String?
    public var name: String
    public var path: String
    public var color: WorkspaceColor?

    public init(workspaceId: String?, path: String) {
        self.workspaceId = workspaceId
        self.path = path
        self.name = WorkspaceChip.folderName(of: path)
        self.color = workspaceId.map(WorkspaceColor.init(workspaceId:))
    }

    /// A path's last component (`/work/app/` → `app`); the path itself when it has none.
    public static func folderName(of path: String) -> String {
        let name = (path as NSString).lastPathComponent
        return name.isEmpty || name == "/" ? path : name
    }

    /// The chip of a workspace path, with the id of the registered workspace at that path
    /// when the source did not give one.
    static func of(path: String, workspaceId: String?, workspaces: [WorkspaceInfo]) -> WorkspaceChip {
        let id = workspaceId ?? workspaces.first(where: { samePath($0.path, path) })?.id
        return WorkspaceChip(workspaceId: id, path: path)
    }
}

/// Two paths name the same folder (a trailing slash and `.`/`..` do not matter).
func samePath(_ a: String, _ b: String) -> Bool {
    (a as NSString).standardizingPath == (b as NSString).standardizingPath
}
