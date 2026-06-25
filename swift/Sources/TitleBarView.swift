// TitleBarView.swift — 44pt unified vibrancy titlebar with the brand mark,
// workspace label, a centered run-state capsule, and provider readiness shown
// as a count only (never a key).

import SwiftUI

struct TitleBarView: View {
    @EnvironmentObject private var state: AppState
    @EnvironmentObject private var coordinator: AppCoordinator

    var body: some View {
        ZStack {
            HStack(spacing: 10) {
                Spacer().frame(width: 70) // traffic-light safe gutter
                AgentMark(size: 22)
                Text("OpenSKS Studio")
                    .font(Theme.ui(13.5, .semibold))
                    .foregroundStyle(Theme.text)
                Text("Studio").hidden().frame(width: 0)
                if let label = state.data?.workspaceLabel {
                    Text(label)
                        .font(Theme.ui(11.5))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(1)
                }
                Spacer()
                providerReadiness
                helpChip
                paletteChip
                Spacer().frame(width: 14)
            }

            StatePill(
                label: state.isRunning ? "Running" : "Idle · agent ready",
                color: state.isRunning ? Theme.accent : Theme.muted,
                pulse: state.isRunning
            )
        }
        .frame(height: 44)
        .background(
            LinearGradient(colors: [Theme.titlebarTop, Theme.titlebarBottom],
                           startPoint: .top, endPoint: .bottom)
                .opacity(0.72)
        )
        .background(.ultraThinMaterial)
    }

    private var providerReadiness: some View {
        let count = coordinator.providers.connections.count
        return HStack(spacing: 6) {
            Image(systemName: "dot.radiowaves.left.and.right")
                .font(.system(size: 11))
                .foregroundStyle(count > 0 ? Theme.accent : Theme.muted)
            Text(Pluralize.count(count, "provider"))
                .font(Theme.ui(11, .medium))
                .foregroundStyle(Theme.textSoft)
        }
        .help("Configured provider registry connections — secrets are never shown")
    }

    private var helpChip: some View {
        Button { state.showHelp = true } label: {
            Image(systemName: "questionmark.circle")
                .font(.system(size: 13))
                .foregroundStyle(Theme.muted)
                .frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Keyboard shortcuts (⌘/)")
        .accessibilityLabel("Keyboard shortcuts")
        .accessibilityIdentifier("titlebar.help")
    }

    private var paletteChip: some View {
        Button { state.showPalette = true } label: {
            HStack(spacing: 4) {
                Image(systemName: "command").font(.system(size: 9, weight: .semibold))
                Text("K").font(Theme.ui(10.5, .semibold))
            }
            .foregroundStyle(Theme.muted)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Capsule().fill(Theme.input))
            .overlay(Capsule().strokeBorder(Theme.stroke, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .help("Command palette (⌘K)")
        .accessibilityLabel("Command palette")
        .accessibilityIdentifier("titlebar.palette")
    }
}
