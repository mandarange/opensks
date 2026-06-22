// ProjectHubView.swift — the `.project` primary route (recovery directive §3.4).
// Groups the secondary destinations (Intelligence, Design, Evidence, Vault,
// Runs, Settings) behind one rail tile so the primary rail stays at five tiles.
// Each card navigates by setting the single NavigationStore route.

import SwiftUI

struct ProjectHubView: View {
    @EnvironmentObject private var nav: NavigationStore

    private struct Destination: Identifiable {
        let route: WorkspaceRoute
        let title: String
        let detail: String
        var id: String { route.rawValue }
    }

    private let destinations: [Destination] = [
        Destination(
            route: .intelligence, title: "Intelligence",
            detail: "Architecture, code graph, and glossary for this project."),
        Destination(
            route: .design, title: "Design System",
            detail: "Token packages, components, audit, and revisions."),
        Destination(
            route: .evidence, title: "Evidence",
            detail: "Acceptance, QA, and security proof state."),
        Destination(
            route: .vault, title: "Vault",
            detail: "Encrypted transcripts and sanitized provenance summaries."),
        Destination(
            route: .runs, title: "Runs",
            detail: "Run history across this project's threads."),
        Destination(
            route: .settings, title: "Settings",
            detail: "Shortcuts, workspace paths, and runtime capabilities."),
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.s16) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Project")
                        .font(Theme.ui(18, .semibold))
                        .foregroundStyle(Theme.text)
                    Text("Everything about this project, grouped in one place.")
                        .font(Theme.ui(12))
                        .foregroundStyle(Theme.muted)
                }
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 240), spacing: 12)],
                    spacing: 12
                ) {
                    ForEach(destinations) { destination in
                        Button { nav.route = destination.route } label: {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack(spacing: 8) {
                                    Image(systemName: destination.route.symbol)
                                        .foregroundStyle(Theme.accent)
                                    Text(destination.title)
                                        .font(Theme.ui(14, .semibold))
                                        .foregroundStyle(Theme.text)
                                }
                                Text(destination.detail)
                                    .font(Theme.ui(11.5))
                                    .foregroundStyle(Theme.muted)
                                    .fixedSize(horizontal: false, vertical: true)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .padding(14)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(
                                RoundedRectangle(cornerRadius: Theme.rLg).fill(Theme.panel)
                            )
                            .overlay(
                                RoundedRectangle(cornerRadius: Theme.rLg)
                                    .strokeBorder(Theme.stroke)
                            )
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("project.hub.\(destination.route.rawValue)")
                        .accessibilityLabel("\(destination.title): \(destination.detail)")
                    }
                }
            }
            .frame(maxWidth: 880, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 40)
            .padding(.vertical, 32)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .accessibilityIdentifier("project.hub")
    }
}
