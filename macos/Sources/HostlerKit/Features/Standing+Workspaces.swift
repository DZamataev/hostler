import Foundation

/// A sidebar row for a workspace (spec §9.3): a dot in its color, the checkout's folder name,
/// its branch, a count of what it holds, a marker while one of its heavy tasks runs.
public struct WorkspaceRow: Identifiable, Hashable, Sendable {
    public var id: String
    public var projectId: String
    public var path: String
    public var folderName: String
    public var branch: String?
    public var color: WorkspaceColor
    /// Distinct resources (devices, the screen, accounts) its live claims hold or reserve.
    public var heldCount: Int
    /// One of its heavy tasks is running (or being stopped).
    public var heavyRunning: Bool
}

/// One row per workspace of `project`, in the snapshot's order (pure).
///
/// - Parameter branches: each workspace's branch by id (the store reads them with
///   `GitHeadReading` off the views' path).
public func workspaceRows(project: ProjectView, claims: ClaimsSnapshot?, queue: QueueSnapshot?,
                          branches: [String: String]) -> [WorkspaceRow] {
    project.workspaces.map { workspace in
        let held = Set(
            (claims?.resources ?? [])
                .filter { state in state.claims.contains { $0.workspace.id == workspace.id } }
                .map(\.resource.id))
        let active = (queue?.running ?? []) + (queue?.foreignRunning ?? [])
        let running = active.contains { entry in
            entry.state != .waiting && entry.workspaceRoot.map { samePath($0, workspace.path) } == true
        }
        return WorkspaceRow(
            id: workspace.id, projectId: workspace.projectId, path: workspace.path,
            folderName: WorkspaceChip.folderName(of: workspace.path), branch: branches[workspace.id],
            color: WorkspaceColor(workspaceId: workspace.id), heldCount: held.count,
            heavyRunning: running)
    }
}
