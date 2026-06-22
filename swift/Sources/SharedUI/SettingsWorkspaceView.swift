// SettingsWorkspaceView.swift — the `.settings` route (PR-045). A truthful
// settings surface: it exposes what exists today (the keyboard-shortcut
// reference and the read-only resolved workspace paths) and states plainly that
// provider, permission and retention settings have their own surfaces in later
// PRs — without claiming they are present here.

import SwiftUI

struct SettingsWorkspaceView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.s16) {
                header
                shortcutsCard
                workspaceCard
                upcomingCard
            }
            .frame(maxWidth: 720, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 40)
            .padding(.vertical, 32)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .accessibilityIdentifier("settings.workspace")
    }

    private var header: some View {
        HStack(spacing: Theme.s12) {
            Image(systemName: "gearshape")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 2) {
                Text("Settings")
                    .font(Theme.ui(18, .semibold))
                    .foregroundStyle(Theme.text)
                Text("What you can configure today, and what is on its way.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 0)
        }
    }

    private var shortcutsCard: some View {
        card {
            HStack(alignment: .center, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Keyboard shortcuts")
                        .font(Theme.ui(15, .semibold))
                        .foregroundStyle(Theme.text)
                    Text("Every workspace, the command palette, and the primary actions are reachable from the keyboard. Press ⌘/ anytime to open this reference.")
                        .font(Theme.ui(12.5))
                        .foregroundStyle(Theme.muted)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
                Button {
                    state.showHelp = true
                } label: {
                    Label("View shortcuts", systemImage: "keyboard")
                }
                .buttonStyle(.secondaryAction)
                .frame(width: 180)
                .accessibilityIdentifier("settings.shortcuts.open")
                .help("Open the keyboard-shortcuts reference (⌘/).")
            }
        }
    }

    private var workspaceCard: some View {
        card {
            VStack(alignment: .leading, spacing: Theme.s10) {
                Text("Workspace")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                infoRow(label: "Folder", value: state.data?.workspaceLabel ?? state.workspace.lastPathComponent, systemImage: "folder")
                infoRow(label: "Path", value: state.workspace.path, systemImage: "externaldrive")
                infoRow(label: "Engine CLI", value: state.cli.lastPathComponent, systemImage: "terminal")
            }
        }
        .accessibilityIdentifier("settings.workspace.card")
    }

    private var upcomingCard: some View {
        card {
            VStack(alignment: .leading, spacing: Theme.s8) {
                HStack(spacing: Theme.s8) {
                    StatusPill(kind: .neutral, label: "Not here yet")
                    Spacer()
                }
                Text("Provider configuration, permission scopes and retention policy get their own surfaces in later PRs. They are intentionally not shown here so nothing implies a setting exists before it does.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityIdentifier("settings.upcoming.card")
    }

    private func infoRow(label: String, value: String, systemImage: String) -> some View {
        HStack(spacing: Theme.s10) {
            Label {
                Text(label).font(Theme.ui(12, .medium)).foregroundStyle(Theme.textSoft)
            } icon: {
                Image(systemName: systemImage).font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            .frame(width: 110, alignment: .leading)
            Text(value)
                .font(Theme.mono(11.5))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label): \(value)")
    }

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .padding(Theme.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: Theme.rLg).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: Theme.rLg).strokeBorder(Theme.stroke, lineWidth: 1))
    }
}
