// RootView.swift — top-level composition: titlebar band, the resizable body
// (rail | explorer | center(editor / terminal) | composer), and the persistent
// honest status bar. Owns AppState and loads domain data on appear.

import SwiftUI
import AppKit

struct RootView: View {
    @StateObject private var state = AppState()
    @State private var explorerWidth: CGFloat = 240
    @State private var composerWidth: CGFloat = 352
    @State private var editorFraction: CGFloat = 0.62

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
        .onAppear {
            state.loadData()
            state.connectEngine()
        }
        .sheet(isPresented: $state.showPalette) { CommandPalette() }
    }

    private var mainBody: some View {
        HStack(spacing: 0) {
            RailView()
                .frame(width: 56)
            Divider().overlay(Theme.stroke)

            ExplorerView()
                .frame(width: explorerWidth)
            DragDivider(width: $explorerWidth, range: 200...340)

            VStack(spacing: 0) {
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
            .frame(maxWidth: .infinity)

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
