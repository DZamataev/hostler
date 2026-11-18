import Foundation

// The window's Projects section (plan Task 11; spec §9.3): the rows it draws, the approval it
// offers, and the doctor report's presentation. `hostler doctor --json` itself stays Task 10's
// `DeviceActions.runDoctor` (which reads the report at exit status 1); this file turns the
// report into rows a view lays out.

// MARK: - Projects

/// One workspace of a project, as the Projects section lists it.
public struct ProjectWorkspaceRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var workspace: WorkspaceInfo
    public var folderName: String
    public var color: WorkspaceColor
    public var path: String

    public init(workspace: WorkspaceInfo) {
        self.id = workspace.id
        self.workspace = workspace
        self.folderName = WorkspaceChip.folderName(of: workspace.path)
        self.color = WorkspaceColor(workspaceId: workspace.id)
        self.path = workspace.path
    }
}

/// One project of the Projects section: its approval state, its path, its workspaces, and what
/// `hostler.toml` declares (the declarations the Secrets and Accounts sections show).
public struct ProjectRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var info: ProjectInfo
    public var workspaces: [ProjectWorkspaceRow]
    public var declaredSecrets: Int
    public var declaredAccounts: Int

    public var name: String { info.name }
    public var path: String { info.commonDir }
    public var approved: Bool { info.approved }

    /// "approved" / "not approved for secrets".
    public var approvalText: String {
        approved ? String(localized: "approved") : String(localized: "not approved for secrets")
    }
}

/// Every registered project, in the snapshot's order (pure).
public func projectRows(_ snapshot: ProjectsSnapshot?) -> [ProjectRow] {
    (snapshot?.projects ?? []).map { project in
        ProjectRow(id: project.info.id, info: project.info,
                   workspaces: project.workspaces.map(ProjectWorkspaceRow.init(workspace:)),
                   declaredSecrets: project.declared?.secrets.count ?? 0,
                   declaredAccounts: project.declared?.accounts.count ?? 0)
    }
}

// MARK: - The actions

/// The Projects section's action (plan Task 11): approve a project, so that it may ask for
/// secrets (`project.approve`). The confirmation is the view's; the service records the time.
@MainActor
public struct ProjectsActions {
    public var client: HostlerClient

    public init(client: HostlerClient) {
        self.client = client
    }

    /// `project.approve`. The project's own reference, from the projects snapshot.
    public func approve(_ project: ProjectRow) async -> ActionFailure? {
        do throws(HostlerCallError) {
            _ = try await client.projectApprove(ProjectRef(commonDir: project.info.commonDir,
                                                           name: project.info.name))
            return nil
        } catch {
            return .app(HostlerAppError(call: error))
        }
    }
}

// MARK: - The doctor's report

/// One check of `hostler doctor --json` as a row: its status, its message, the fix (the command
/// a person copies, or the text that applies), and who acts.
public struct DoctorCheckRow: Identifiable, Equatable, Sendable {
    public var id: String
    public var status: CheckStatus
    public var statusText: String
    public var message: String
    /// The line under the message: the fix's text (always) — or the command when the text is
    /// the command's description.
    public var fixText: String?
    /// A runnable command, which the view offers to copy.
    public var fixCommand: String?
    /// "a person acts" / "an agent can fix it".
    public var whoText: String?
    /// `--fix` would run it (the report's `safe`).
    public var isSafeFix: Bool

    public var isProblem: Bool { status == .problem }
}

/// The whole report as the Projects section shows it.
public struct DoctorReportModel: Equatable, Sendable {
    public var rows: [DoctorCheckRow]
    public var problems: Int
    public var warnings: Int
    public var skipped: Int

    /// "4 checks · 1 problem · 2 warnings"; a summary with the counts that matter.
    public var summary: String {
        var parts = [rows.count == 1 ? String(localized: "1 check")
            : String(localized: "\(rows.count) checks")]
        if problems > 0 {
            parts.append(problems == 1 ? String(localized: "1 problem")
                : String(localized: "\(problems) problems"))
        }
        if warnings > 0 {
            parts.append(warnings == 1 ? String(localized: "1 warning")
                : String(localized: "\(warnings) warnings"))
        }
        if skipped > 0 { parts.append(String(localized: "\(skipped) skipped")) }
        return parts.joined(separator: " · ")
    }
}

/// The report's rows in the service's order, with the counts (pure).
public func doctorModel(_ report: DoctorReport) -> DoctorReportModel {
    let rows = report.checks.map { check in
        DoctorCheckRow(
            id: check.id, status: check.status,
            statusText: DoctorCheckRow.statusText(check.status), message: check.message,
            fixText: check.fix?.text, fixCommand: check.fix?.command,
            whoText: check.fix.map { DoctorCheckRow.whoText($0.who) }, isSafeFix: check.fix?.safe ?? false)
    }
    return DoctorReportModel(
        rows: rows,
        problems: rows.filter { $0.status == .problem }.count,
        warnings: rows.filter { $0.status == .warn }.count,
        skipped: rows.filter { $0.status == .skipped }.count)
}

extension DoctorCheckRow {
    public static func statusText(_ status: CheckStatus) -> String {
        switch status {
        case .ok: String(localized: "ok")
        case .warn: String(localized: "warning")
        case .problem: String(localized: "problem")
        case .skipped: String(localized: "skipped")
        }
    }

    public static func whoText(_ who: Who) -> String {
        switch who {
        case .person: String(localized: "a person acts")
        case .agent: String(localized: "an agent can fix it")
        }
    }
}
