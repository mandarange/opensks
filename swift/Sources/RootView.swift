// RootView.swift — top-level composition: titlebar band, the resizable body
// (rail | explorer | center(editor / terminal) | composer), and the persistent
// honest status bar. Owns AppState and loads domain data on appear.

import SwiftUI
import AppKit

struct RootView: View {
    @StateObject private var state = AppState()
    @StateObject private var coordinator = AppCoordinator()
    @State private var explorerWidth: CGFloat = 240
    @State private var composerWidth: CGFloat = 352

    var body: some View {
        ZStack {
            VibrantBackground(material: .underWindowBackground)
                .ignoresSafeArea()
            Theme.bg.opacity(0.55).ignoresSafeArea()

            VStack(spacing: 0) {
                TitleBarView()
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
            state.connectEngine()
            // Bind the conversation store to the SAME bundled CLI + workspace
            // path AppState resolved, then load this project's conversations.
            coordinator.bindConversations(cli: state.cli, workspace: state.workspace)
            // Bind the Git studio to the same resolved CLI + workspace, then wire
            // it to the editor (dirty-switch preflight) + conversations (commit
            // card sink) for the PR-035 local mutations.
            coordinator.bindGit(cli: state.cli, workspace: state.workspace)
            coordinator.wireGit(editorStore: state.editorStore)
            // Bind the LOCAL design-import store (PR-039) to the same resolved CLI +
            // workspace, then read this project's quarantine listing.
            coordinator.bindDesignImport(cli: state.cli, workspace: state.workspace)
            // Bind the Project Intelligence store (PR-041) to the same resolved CLI +
            // workspace, then load architecture / code graph / glossary.
            coordinator.bindIntelligence(cli: state.cli, workspace: state.workspace)
        }
        .sheet(isPresented: $state.showPalette) { CommandPalette() }
    }

    private var mainBody: some View {
        HStack(spacing: 0) {
            LabeledNavigationRail()
            Divider().overlay(Theme.stroke)

            ExplorerView()
                .frame(width: explorerWidth)
            DragDivider(width: $explorerWidth, range: 200...340)

            // The central workspace is route-driven (PR-022): selecting a rail
            // tile re-renders this region. No fixed max width — it fills the
            // available space so the shell never letterboxes the center.
            PrimaryWorkspaceRouter()
                .frame(maxWidth: .infinity)
                .layoutPriority(1)

            DragDivider(width: $composerWidth, range: 320...440, invert: true)
            ComposerView()
                .frame(width: composerWidth)
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
