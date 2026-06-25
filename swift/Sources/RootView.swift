// RootView.swift — top-level composition: titlebar band, the resizable body
// (rail | explorer | chat-first center), and the persistent honest status bar.
// Owns AppState and loads domain data on appear. There is no permanent
// right-hand composer: the conversation composer in Chat is the primary
// execution control (recovery directive §0.3 / §3.2).

import SwiftUI
import AppKit

struct RootView: View {
    @StateObject private var state = AppState()
    @StateObject private var coordinator = AppCoordinator()
    @State private var explorerWidth: CGFloat = 240

    var body: some View {
        ZStack {
            VibrantBackground(material: .underWindowBackground)
                .ignoresSafeArea()
            Theme.bg.opacity(0.55).ignoresSafeArea()

            VStack(spacing: 0) {
                TitleBarView(providers: coordinator.providers)
                Divider().overlay(Theme.stroke)
                mainBody
                Divider().overlay(Theme.stroke)
                StatusBarView()
            }
        }
        .environmentObject(state)
        .environmentObject(coordinator)
        .environmentObject(coordinator.navigation)
        .onAppear {
            state.loadData()
            state.startProofArtifactRefresh()
            state.connectEngine()
            // Bind the conversation store to the SAME bundled CLI + workspace
            // path AppState resolved, then load this project's conversations.
            // Feed streamed execution events into the pipeline projection that
            // drives the graph (PIPE-001 — it previously never received any).
            state.pipelines = coordinator.pipelines
            coordinator.bindConversations(cli: state.cli, workspace: state.workspace)
            // Bind the Git studio to the same resolved CLI + workspace, then wire
            // it to the editor (dirty-switch preflight) + conversations (commit
            // card sink) for the PR-035 local mutations.
            coordinator.bindGit(cli: state.cli, workspace: state.workspace)
            coordinator.wireGit(editorStore: state.editorStore)
            coordinator.bindProviders(cli: state.cli, workspace: state.workspace)
            // Bind the LOCAL design-import store (PR-039) to the same resolved CLI +
            // workspace, then read this project's quarantine listing.
            coordinator.bindDesignImport(cli: state.cli, workspace: state.workspace)
            // Bind the Project Intelligence store (PR-041) to the same resolved CLI +
            // workspace, then load architecture / code graph / glossary.
            coordinator.bindIntelligence(cli: state.cli, workspace: state.workspace)
            // Bind the Vault store (PR-042) to the same resolved CLI + workspace,
            // then read this workspace's vault inventory (summaries + redacted vaults).
            coordinator.bindVault(cli: state.cli, workspace: state.workspace)
            // Bind the Design Studio to the resolved CLI + workspace. Without this
            // the studio kept its init-time process-cwd CLI path, so a packaged
            // .app drove the wrong binary/cwd (DESIGN-001).
            coordinator.bindDesignStudio(cli: state.cli, workspace: state.workspace)
        }
        .background(globalShortcuts)
        .sheet(isPresented: $state.showPalette) { CommandPalette() }
        .sheet(isPresented: $state.showHelp) { KeyboardShortcutsHelpView() }
    }

    /// App-wide keyboard shortcuts surfaced as hidden command buttons so they work
    /// whenever the window is key, without stealing focus. Discoverable via the
    /// ⌘/ shortcuts reference and the titlebar affordances.
    private var globalShortcuts: some View {
        ZStack {
            Button("") { state.showPalette = true }
                .keyboardShortcut("k", modifiers: .command)
            Button("") { state.showHelp = true }
                .keyboardShortcut("/", modifiers: .command)
            Button("") { state.runAcceptance() }
                .keyboardShortcut("r", modifiers: .command)
            Button("") { coordinator.navigation.route = .chat }
                .keyboardShortcut("l", modifiers: .command)
        }
        .buttonStyle(.plain)
        .opacity(0)
        .frame(width: 0, height: 0)
        .accessibilityHidden(true)
    }

    private var mainBody: some View {
        HStack(spacing: 0) {
            LabeledNavigationRail()
            Divider().overlay(Theme.stroke)

            ExplorerView()
                .frame(width: explorerWidth)
            DragDivider(width: $explorerWidth, range: 200...340)

            // The central workspace is route-driven and chat-first: it fills the
            // available space (highest layout priority) so the shell never
            // letterboxes the center and Chat stays readable down to 1040pt.
            PrimaryWorkspaceRouter()
                .frame(minWidth: 520, maxWidth: .infinity)
                .layoutPriority(1)
        }
    }
}

/// Horizontal resize handle between the editor and terminal drawer.
struct HorizontalDragDivider: View {
    @Binding var fraction: CGFloat
    var totalHeight: CGFloat
    @State private var base: CGFloat?

    var body: some View {
        Rectangle()
            .fill(Theme.stroke)
            .frame(height: 1)
            .overlay(
                Color.clear
                    .frame(height: 9)
                    .contentShape(Rectangle())
                    .onHover { inside in
                        if inside { NSCursor.resizeUpDown.push() } else { NSCursor.pop() }
                    }
                    .gesture(
                        DragGesture()
                            .onChanged { value in
                                let start = base ?? fraction
                                if base == nil { base = start }
                                let delta = value.translation.height / max(1, totalHeight)
                                fraction = min(max(start + delta, 0.3), 0.85)
                            }
                            .onEnded { _ in base = nil }
                    )
            )
    }
}
