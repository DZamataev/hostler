import HostlerKit
import SwiftUI

/// The heavy queue (spec §9.3, §11.1; DeviceDeck's Jobs): a list | detail split, the segmented
/// All / Running / Waiting / Finished filter with its counts, and the capacity. The detail has
/// every field of the entry or history item, withdraws a waiting request, and stops a running
/// task behind the person gate after the person types its id.
struct QueueView: View {
    let store: HostlerStore
    let actions: QueueActions

    @State private var filter: QueueFilter = .all
    @State private var selection: String?
    @State private var failure: ActionFailure?

    private var nowMs: Int64 { Int64((Date().timeIntervalSince1970 * 1000).rounded()) }

    /// The queue snapshot this section reads (fix r1-1): no empty state, count or capacity
    /// before it is there.
    private var snapshotState: SnapshotState {
        SnapshotState(hasSnapshot: store.model.queue != nil, syncState: store.syncState)
    }

    private var allRows: [QueueRow] {
        queueRows(queue: store.model.queue, history: store.model.queueHistory, nowMs: nowMs,
                  workspaces: store.model.projects?.projects.flatMap(\.workspaces) ?? [])
    }

    private var rows: [QueueRow] {
        queueRows(queue: store.model.queue, history: store.model.queueHistory, filter: filter,
                  nowMs: nowMs,
                  workspaces: store.model.projects?.projects.flatMap(\.workspaces) ?? [])
    }

    private var counts: QueueCounts {
        queueCounts(queue: store.model.queue, history: store.model.queueHistory)
    }

    private var selectedRow: QueueRow? {
        selection.flatMap { id in allRows.first { $0.id == id } }
    }

    var body: some View {
        Group {
            if snapshotState.isLoaded {
                HSplitView {
                    listPane
                        .frame(minWidth: 300, idealWidth: 420, maxWidth: .infinity,
                               maxHeight: .infinity)
                    QueueDetailPane(row: selectedRow, actions: actions, capacity: capacity,
                                    failure: $failure)
                        .frame(minWidth: 320, idealWidth: 480, maxWidth: .infinity,
                               maxHeight: .infinity)
                }
            } else {
                ReadingView(state: snapshotState)
            }
        }
        .navigationTitle("Queue")
        .navigationSubtitle(snapshotState.isLoaded ? queueSubtitle : "")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Picker("Filter", selection: $filter) {
                    ForEach(QueueFilter.allCases) { entry in
                        // The counts are the service's; before its snapshot they say nothing.
                        Text(snapshotState.isLoaded ? counts.segmentTitle(entry) : entry.title)
                            .tag(entry)
                    }
                }
                .pickerStyle(.segmented)
                .disabled(!snapshotState.isLoaded)
                .help("All, running, waiting or finished heavy tasks")
                .accessibilityLabel("Queue filter")
                .accessibilityIdentifier("queue.filter")

                // The capacity is the service's too: no stepper before its snapshot.
                if snapshotState.isLoaded {
                    capacityStepper
                }
            }
        }
        .onChange(of: filter) {
            if let selection, !rows.contains(where: { $0.id == selection }) {
                self.selection = nil
            }
        }
        .actionFailureAlert($failure)
    }

    // MARK: The list

    private var listPane: some View {
        VStack(spacing: 0) {
            if rows.isEmpty {
                emptyState
            } else {
                List(rows, selection: $selection) { row in
                    QueueListRow(row: row)
                        .tag(row.id)
                        .accessibilityIdentifier("queue.row.\(row.taskId)")
                }
                .listStyle(.inset)
                .accessibilityIdentifier("queue.list")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            WindowIcon.queue.image
                .font(.system(size: 30, weight: .light))
                .foregroundStyle(.tertiary)
                .accessibilityHidden(true)
            Text(emptyText)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
                .accessibilityIdentifier("queue.empty")
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyText: String {
        guard counts.all > 0 else {
            return String(localized: "No heavy tasks yet — `hostler run --heavy -- <cmd>` puts one here.")
        }
        switch filter {
        case .all: return String(localized: "No heavy tasks yet.")
        case .running: return String(localized: "Nothing is running right now.")
        case .waiting: return String(localized: "Nothing is waiting right now.")
        case .finished: return String(localized: "Nothing has finished yet.")
        }
    }

    // MARK: The toolbar

    private var capacityStepper: some View {
        Stepper(value: capacityBinding, in: QueueActions.capacityRange) {
            Text("Capacity \(capacity)")
                .font(.callout)
                .monospacedDigit()
        }
        .help(CommandHelp.text(.setCapacity, capacity: capacity))
        .accessibilityLabel("Heavy queue capacity")
        .accessibilityValue("\(capacity) of 16")
        .accessibilityIdentifier("queue.capacity")
    }

    private var capacity: Int {
        Int(store.model.queue?.capacity ?? 1)
    }

    private var capacityBinding: Binding<Int> {
        Binding(get: { capacity }, set: { value in
            Task {
                if let failure = await actions.setCapacity(value) { self.failure = failure }
            }
        })
    }

    // MARK: Actions

    private var queueSubtitle: String {
        let capacity = store.model.queue?.capacity
        let text = counts.all == 0
            ? String(localized: "No heavy tasks")
            : String(localized: "\(counts.running) running · \(counts.waiting) waiting · \(counts.finished) finished")
        guard let capacity else { return text }
        return String(localized: "\(text) · capacity \(capacity)")
    }
}

// MARK: - A row

/// One line of the queue list: the command in monospaced type, then the kind, the workspace's
/// chip, the position or duration, the outcome, and the time.
struct QueueListRow: View {
    var row: QueueRow

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                Text(row.command.isEmpty ? String(localized: "(no command)") : row.command)
                    .font(.system(.callout, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                HStack(spacing: 6) {
                    Text(row.kind)
                        .secondaryCaption()
                    if let workspace = row.workspace {
                        StandingChipView(chip: StandingChip(workspace: workspace))
                    }
                    if let position = row.positionText {
                        Text(position).secondaryCaption()
                    }
                    Text(row.durationText).secondaryCaption()
                    if row.isForeign {
                        Text("another tool").secondaryCaption()
                    }
                }
            }
            Spacer(minLength: 4)
            VStack(alignment: .trailing, spacing: 3) {
                QueuePhaseBadge(row: row)
                Text(row.timeText)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityText)
    }

    private var accessibilityText: String {
        var parts = [row.command, row.kind]
        if let workspace = row.workspace { parts.append(workspace.name) }
        parts.append(row.phase.title)
        if let position = row.positionText { parts.append(position) }
        if let outcome = row.outcomeText { parts.append(outcome) }
        parts.append(row.timeText)
        return parts.filter { !$0.isEmpty }.joined(separator: ", ")
    }
}

/// The small capsule that says where a task is: its phase, or its outcome once it has finished.
struct QueuePhaseBadge: View {
    var row: QueueRow

    var body: some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().fill(color.opacity(0.16)))
            .overlay(Capsule().strokeBorder(color.opacity(0.4)))
            .foregroundStyle(color)
            .accessibilityHidden(true)
    }

    private var text: String {
        row.outcomeText ?? row.phase.title
    }

    private var color: Color {
        switch row.outcome {
        case .ok?: .green
        case .failed?, .timedOut?: .red
        case .withdrawn?, .stopped?, .reaped?, .forceReleased?: .orange
        case nil:
            switch row.phase {
            case .running: .green
            case .stopping: .orange
            case .waiting: .secondary
            case .finished: .secondary
            }
        }
    }
}

// MARK: - The detail

/// The right half of the queue: the selected task's every field, and its actions.
struct QueueDetailPane: View {
    var row: QueueRow?
    let actions: QueueActions
    var capacity: Int
    var failure: Binding<ActionFailure?>

    var body: some View {
        Group {
            if let row {
                QueueDetailView(row: row, actions: actions, capacity: capacity,
                                failure: failure)
            } else {
                VStack(spacing: 8) {
                    WindowIcon.clock.image
                        .font(.system(size: 30, weight: .light))
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                    Text("Select a task to see what it ran and how it ended.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 320)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }
}

/// One task's detail: its fields, then Withdraw (a waiting request) or Stop (a running task).
struct QueueDetailView: View {
    var row: QueueRow
    let actions: QueueActions
    var capacity: Int
    @Binding var failure: ActionFailure?

    @State private var confirmingStop = false
    @State private var typed = ""
    @State private var stopping = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                if confirmingStop { stopPanel }
                facts
                actionsRow
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier("queue.detail")
        .onChange(of: row.id) {
            confirmingStop = false
            typed = ""
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(row.command)
                    .font(.system(.title3, design: .monospaced))
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                QueuePhaseBadge(row: row)
                Spacer(minLength: 4)
            }
            HStack(spacing: 8) {
                Text(row.kind).secondaryCaption()
                if let workspace = row.workspace { StandingChipView(chip: StandingChip(workspace: workspace)) }
                Text("ticket \(String(row.ticket))").secondaryCaption()
                Text(row.timeText).secondaryCaption()
            }
        }
    }

    private var facts: some View {
        SectionBox {
            VStack(alignment: .leading, spacing: 5) {
                FactRow(label: String(localized: "Task"), value: row.taskId)
                FactRow(label: String(localized: "Ticket"), value: String(row.ticket))
                FactRow(label: String(localized: "Kind"), value: row.kind)
                FactRow(label: String(localized: "State"), value: row.phase.title)
                if let outcome = row.outcomeText {
                    FactRow(label: String(localized: "Outcome"), value: outcome)
                }
                if let exitCode = row.exitCode {
                    FactRow(label: String(localized: "Exit status"), value: String(exitCode))
                }
                FactRow(label: String(localized: "Workspace"), value: row.workspacePath ?? row.cwd)
                FactRow(label: String(localized: "Ran in"), value: row.cwd)
                FactRow(label: String(localized: "Duration"), value: row.durationText)
                if let pid = row.holderPid {
                    FactRow(label: String(localized: "Holder pid"), value: String(pid))
                }
                FactRow(label: String(localized: "Enqueued"), value: Self.time(row.enqueuedAtMs))
                if let granted = row.grantedAtMs {
                    FactRow(label: String(localized: "Started"), value: Self.time(granted))
                }
                if let finished = row.finishedAtMs {
                    FactRow(label: String(localized: "Finished"), value: Self.time(finished))
                }
                if let source = row.source {
                    FactRow(label: String(localized: "Source"),
                            value: source == .hostler ? "hostler" : "rn-looky")
                }
                FactRow(label: String(localized: "Queue capacity"), value: String(capacity))
            }
        }
    }

    @ViewBuilder private var actionsRow: some View {
        HStack(spacing: 8) {
            if row.canWithdraw, !confirmingStop {
                Button {
                    withdraw()
                } label: {
                    Label { Text("Withdraw") } icon: { WindowIcon.stop.image }
                }
                .help(CommandHelp.text(.withdrawTask, task: row.taskId))
                .accessibilityLabel("Withdraw \(row.command)")
                .accessibilityIdentifier("queue.withdraw")
            }
            if row.canStop, !confirmingStop {
                Button(role: .destructive) {
                    typed = ""
                    confirmingStop = true
                } label: {
                    Label { Text("Stop…") } icon: { WindowIcon.stop.image }
                }
                .help(CommandHelp.text(.stopTask, token: row.taskId, task: row.taskId))
                .accessibilityLabel("Stop \(row.command)")
                .accessibilityIdentifier("queue.stop")
            }
            if row.phase == .stopping {
                Text("hostler is stopping it; the slot frees once its processes are gone.")
                    .secondaryCaption()
            }
            if row.isForeign {
                Text("Another tool keeps this task; hostler does not stop it.")
                    .secondaryCaption()
            }
            Spacer(minLength: 0)
        }
    }

    /// The typed confirmation (spec §11.1: the person types the task's id). It is not a sheet,
    /// because the person gate's own sheet is attached to the window: a second sheet cannot
    /// present over it.
    private var stopPanel: some View {
        SectionBox {
            VStack(alignment: .leading, spacing: 8) {
                Text("Stop \(row.taskId)?")
                    .font(.headline)
                Text(String(localized:
                    "hostler stops the task's processes and verifies that they are gone before it records it as stopped and frees the slot. A task that had already ended is recorded as reaped."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text("Type \(row.taskId) to confirm:")
                    .font(.callout)
                TextField(row.taskId, text: $typed)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 320)
                    .accessibilityLabel("Type the task's id to confirm")
                    .accessibilityIdentifier("queue.stop.field")
                Text(CommandHelp.text(.stopTask, token: row.taskId, task: row.taskId))
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                HStack {
                    Button("Cancel", role: .cancel) {
                        confirmingStop = false
                        typed = ""
                    }
                    .keyboardShortcut(.cancelAction)
                    Button("Stop") { stop() }
                        .keyboardShortcut(.defaultAction)
                        .disabled(!QueueActions.confirmationMatches(typed, taskId: row.taskId)
                                  || stopping)
                        .accessibilityLabel("Stop the task")
                        .accessibilityIdentifier("queue.stop.confirm")
                    if stopping {
                        ProgressView().controlSize(.small)
                            .accessibilityLabel("Stopping")
                    }
                }
            }
        }
    }

    private func withdraw() {
        Task {
            if let failure = await actions.withdraw(row) { self.failure = failure }
        }
    }

    private func stop() {
        stopping = true
        let typed = typed
        Task {
            // The panel goes before the gate asks: the gate's sheet is the window's, and no
            // other sheet may be up when it presents.
            confirmingStop = false
            self.typed = ""
            let outcome = await actions.stop(row, typed: typed)
            stopping = false
            switch outcome {
            case .done:
                break
            case .cancelled:
                break
            case .failed(let failure):
                self.failure = failure
            }
        }
    }

    private static func time(_ ms: Int64) -> String {
        Date(timeIntervalSince1970: Double(ms) / 1000)
            .formatted(date: .abbreviated, time: .standard)
    }
}
