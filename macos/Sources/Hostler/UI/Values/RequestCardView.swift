import HostlerKit
import SwiftUI

/// One pending request as a card (spec §9.2 item 4): a secret's (project, name, declared
/// purpose, reason, requester, age; one `SecureField`) or an account's (one per requested
/// field). Store and Decline; an account's both pass the person gate. Used by the popover and
/// by the window's Secrets and Accounts sections (Task 11).
struct RequestCardView: View {
    let card: RequestCard
    let requests: RequestsController
    /// The projects snapshot, for the secret's project (`secret.set` needs its directory).
    let projects: ProjectsSnapshot?
    var presenter: PersonGate.Presenter = .popover

    var body: some View {
        ValueBox {
            HStack(alignment: .firstTextBaseline) {
                Text(PopoverPresentation.cardTitle(card))
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer()
            }
            if let purpose = card.purpose {
                Text(purpose).font(.caption).foregroundStyle(.secondary)
            }
            Text("“\(card.reason)”")
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            TimelineView(.periodic(from: .now, by: 30)) { context in
                Text(PopoverPresentation.cardSubtitle(card, nowMs: Self.ms(context.date)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ForEach(card.fields, id: \.self) { field in
                SecretEntryField(title: fieldTitle(field), entry: requests.entry(card, field: field),
                                 onSubmit: store)
            }
            ValueErrorLines(requests.errors[card.id])
            HStack {
                if requests.working.contains(card.id) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel(String(localized: "Sending"))
                }
                Spacer()
                Button(String(localized: "Decline")) {
                    Task { _ = await requests.decline(card, from: presenter) }
                }
                .disabled(requests.working.contains(card.id))
                .accessibilityLabel(String(localized: "Decline the request for \(card.name)"))
                Button(String(localized: "Store"), action: store)
                    .disabled(requests.working.contains(card.id) || !requests.canStore(card))
                    .accessibilityLabel(String(localized: "Store \(card.name)"))
            }
        }
    }

    private func fieldTitle(_ field: String) -> String {
        card.isAccount ? field : String(localized: "Value of \(card.name)")
    }

    private func store() {
        Task { _ = await requests.store(card, projects: projects, from: presenter) }
    }

    static func ms(_ date: Date) -> Int64 {
        Int64((date.timeIntervalSince1970 * 1000).rounded())
    }
}
