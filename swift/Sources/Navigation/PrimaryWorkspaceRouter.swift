// PrimaryWorkspaceRouter.swift — the central workspace region. Switching the
// rail route re-renders this region: this is the core PR-022 fix (rail selection
// drives the main surface, not just the context sidebar). Surfaces that are not
// built yet show an honest, labelled placeholder naming the PR that delivers
// them — never a fake "live" surface.

import SwiftUI

struct PrimaryWorkspaceRouter: View {
    @EnvironmentObject private var nav: NavigationStore
    @EnvironmentObject private var coordinator: AppCoordinator

    var body: some View {
        Group {
            switch nav.route {
            case .home:
                HomeView()
            case .code:
                CodeWorkspaceView()
            case .chat:
                ConversationThreadView(store: coordinator.conversations)
            case .graph:
                RoutePlaceholderView(
                    headline: "Pipeline Graph",
                    detail: "The live visual graph canvas arrives in PR-030.",
                    systemImage: "point.3.connected.trianglepath.dotted"
                )
            case .runs:
                RoutePlaceholderView(
                    headline: "Runs",
                    detail: "Run history and the node-level pipeline view arrive in PR-029 / PR-030.",
                    systemImage: "sparkles"
                )
            case .git:
                RoutePlaceholderView(
                    headline: "Git Studio",
                    detail: "Branch, status, commit and approval-gated push arrive in PR-034 → PR-036.",
                    systemImage: "arrow.triangle.branch"
                )
            case .design:
                RoutePlaceholderView(
                    headline: "Design Systems",
                    detail: "The Design Studio (tokens, components, audit) arrives in PR-037 → PR-040.",
                    systemImage: "paintpalette"
                )
            case .intelligence:
                RoutePlaceholderView(
                    headline: "Project Intelligence",
                    detail: "Architecture, code graph and freshness arrive in PR-041.",
                    systemImage: "brain.head.profile"
                )
            case .evidence:
                RoutePlaceholderView(
                    headline: "Evidence",
                    detail: "The proof chain and approvals workspace arrives in PR-045.",
                    systemImage: "checkmark.seal"
                )
            case .settings:
                RoutePlaceholderView(
                    headline: "Settings",
                    detail: "Providers, permissions, retention and shortcuts arrive in later PRs.",
                    systemImage: "gearshape"
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .accessibilityIdentifier(nav.route.centralAccessibilityIdentifier)
    }
}

/// The editable code surface plus the terminal drawer (extracted from the old
/// fixed-center RootView so the center is now route-driven).
struct CodeWorkspaceView: View {
    @EnvironmentObject private var state: AppState
    @State private var editorFraction: CGFloat = 0.62

    var body: some View {
        GeometryReader { geo in
            let h = geo.size.height
            let editorH = max(160, h * editorFraction)
            VStack(spacing: 0) {
                EditorView()
                    .frame(height: state.terminalCollapsed ? h - 30 : editorH)
                if !state.terminalCollapsed {
                    HorizontalDragDivider(fraction: $editorFraction, totalHeight: h)
                }
                TerminalView()
                    .frame(maxHeight: .infinity)
            }
        }
    }
}

/// Honest, labelled empty state for a routed surface that is not built yet.
/// Delegates to the shared `EmptyStateView` (SharedUI).
struct RoutePlaceholderView: View {
    let headline: String
    let detail: String
    let systemImage: String

    var body: some View {
        EmptyStateView(headline: headline, detail: detail, systemImage: systemImage)
    }
}
