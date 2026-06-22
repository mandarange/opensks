// PrimaryWorkspaceRouter.swift — the central workspace region. Switching the
// rail route re-renders this region: this is the core PR-022 fix (rail selection
// drives the main surface, not just the context sidebar). Surfaces that are not
// built yet show an honest, labelled placeholder naming the PR that delivers
// them — never a fake "live" surface.

import SwiftUI
import AppKit

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
                // UX-101: the chat thread carries a compact top context bar with the
                // project's REAL git context, so the wrapper observes the git store.
                ChatWorkspaceView(
                    conversations: coordinator.conversations,
                    git: coordinator.git,
                    pipelines: coordinator.pipelines,
                    onOpenGraph: { coordinator.openGraph(runId: $0) }
                )
            case .graph:
                PipelineGraphWorkspace(
                    store: coordinator.pipelines,
                    activeRunId: $coordinator.activeGraphRunId
                )
            case .runs:
                RoutePlaceholderView(
                    headline: "Runs",
                    detail: "Run history and the node-level pipeline view are coming soon.",
                    systemImage: "sparkles"
                )
            case .git:
                // PR-034: the READ-ONLY status / branches / diff studio. Commit
                // and approval-gated push arrive in PR-035 → PR-036.
                GitStatusView(store: coordinator.git)
                    .onAppear { coordinator.git.refresh() }
            case .design:
                // PR-040: the Design Studio — a catalog sidebar + detail tabs
                // (Tokens / Components / Audit / Revisions). Activation is ATOMIC: a
                // failing audit blocks it and keeps the previously active package.
                // The PR-039 LOCAL, human-reviewed IMPORT surface remains reachable
                // (quarantine → review → promote feeds the catalog).
                DesignStudioView(store: coordinator.designStudio)
                    .onAppear { Task { await coordinator.designStudio.refreshActiveStatus() } }
            case .intelligence:
                // PR-041: the Project Intelligence surface — architecture records,
                // a PAGED + LOD code-graph explorer, a glossary, and source
                // navigation, each carrying a freshness badge (a STALE section is
                // never drawn as current). Records/results deep-link onto the
                // existing chat / graph / code routes.
                IntelligenceView(
                    store: coordinator.intelligence,
                    onOpen: { coordinator.openIntelTarget($0) }
                )
                .onAppear { Task { await coordinator.intelligence.recheckFreshness() } }
            case .vault:
                // PR-042: the opt-in encrypted-vault + provenance surface. Export a
                // SANITIZED, git-trackable summary (no raw transcript), opt in to
                // encrypt the full transcript into an .age vault for a recipient
                // (public key, configured via the Keychain), or IMPORT a vault with
                // the matching identity. A failure never reveals plaintext.
                VaultView(
                    store: coordinator.vault,
                    activeConversationID: coordinator.conversations.selectedConversationID
                )
                .onAppear { Task { await coordinator.vault.refreshStatus() } }
            case .evidence:
                // PR-045: the proof-state workspace. Reports acceptance results and
                // QA / security posture straight from the decoded app-data, with a
                // truthful empty state + onboarding action before anything is audited.
                EvidenceWorkspaceView()
            case .settings:
                // Providers, permissions and retention land on their own surfaces in
                // later PRs; the keyboard-shortcut reference is available here today.
                SettingsWorkspaceView()
            case .project:
                // The Project hub groups the secondary destinations (Intelligence,
                // Design, Evidence, Vault, Settings, Runs) so the primary rail
                // stays at five tiles (recovery directive §3.4).
                ProjectHubView()
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
                EditorWorkspaceView(store: state.editorStore)
                    .frame(maxWidth: .infinity)
                    .frame(height: state.terminalCollapsed ? h - 30 : editorH)
                if !state.terminalCollapsed {
                    HorizontalDragDivider(fraction: $editorFraction, totalHeight: h)
                }
                TerminalView()
                    .frame(maxHeight: .infinity)
            }
        }
        // Keyboard: Cmd-S save, Opt-Cmd-S save all, Cmd-W close (dirty-protected),
        // Cmd-F find. These are hidden command buttons so the shortcuts work
        // whenever the code workspace is on screen without stealing focus.
        .background(editorShortcuts)
    }

    private var editorShortcuts: some View {
        ZStack {
            Button("") { state.saveActiveFile() }
                .keyboardShortcut("s", modifiers: .command)
            Button("") { state.saveAllFiles() }
                .keyboardShortcut("s", modifiers: [.command, .option])
            Button("") { state.closeActiveFile() }
                .keyboardShortcut("w", modifiers: .command)
            Button("") { performEditorFind() }
                .keyboardShortcut("f", modifiers: .command)
        }
        .buttonStyle(.plain)
        .opacity(0)
        .frame(width: 0, height: 0)
        .accessibilityHidden(true)
    }

    /// Invoke the standard find bar on the first-responder text view by sending
    /// `performTextFinderAction:` with the show-find-interface tag down the
    /// responder chain (the focused NSTextView handles it).
    private func performEditorFind() {
        let item = NSMenuItem(title: "Find", action: nil, keyEquivalent: "")
        item.tag = NSTextFinder.Action.showFindInterface.rawValue
        NSApp.sendAction(
            #selector(NSResponder.performTextFinderAction(_:)),
            to: nil,
            from: item
        )
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
