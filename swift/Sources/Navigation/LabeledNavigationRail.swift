// LabeledNavigationRail.swift — the labelled left navigation rail (replaces the
// old 56pt icon-only RailView). Selecting a tile sets the active WorkspaceRoute
// (re-rendering the central workspace) and keeps the context sidebar in sync.
// Each tile is a full-height Button with a visible English label and a matching
// accessibility label; the whole tile is the hit target.

import SwiftUI

struct LabeledNavigationRail: View {
    @EnvironmentObject private var state: AppState
    @EnvironmentObject private var nav: NavigationStore

    var body: some View {
        VStack(spacing: 2) {
            Spacer().frame(height: 8)
            ForEach(WorkspaceRoute.allCases) { route in
                RailTile(route: route, active: nav.route == route) {
                    nav.route = route
                    state.selectedRail = route.legacySection
                }
            }
            Spacer()
        }
        .frame(width: GeneratedDesignTokens.sizeRailWidth)
        .frame(maxHeight: .infinity)
        .background(Theme.sidebar)
    }
}

private struct RailTile: View {
    let route: WorkspaceRoute
    let active: Bool
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            VStack(spacing: 3) {
                Image(systemName: route.symbol)
                    .font(.system(size: 18, weight: .medium))
                Text(route.label)
                    .font(Theme.ui(10, .medium))
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
            .foregroundStyle(active ? Theme.accent : Theme.muted)
            .frame(maxWidth: .infinity, minHeight: 52)
            .background(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                    .fill(active ? Theme.accentTint : (hovering ? Theme.panel : Color.clear))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6)
        .onHover { hovering = $0 }
        .help(shortcutKey.map { "\(route.label) (⌘\($0))" } ?? route.label)
        .accessibilityLabel(route.label)
        .accessibilityIdentifier(route.railTileAccessibilityIdentifier)
        // Primary navigation is keyboard-reachable: the first nine routes bind to
        // ⌘1…⌘9 so an operator can jump between workspaces without the mouse.
        .modifier(NavigationShortcutModifier(key: shortcutKey))
    }

    /// The ⌘-number key for this route, if it is within the first nine routes.
    private var shortcutKey: Character? { KeyboardShortcuts.navigationKey(for: route) }
}

/// Attaches a `.keyboardShortcut` only when the route has a numeric key, so routes
/// beyond the first nine simply carry no shortcut (rather than a bogus one).
private struct NavigationShortcutModifier: ViewModifier {
    let key: Character?

    func body(content: Content) -> some View {
        if let key {
            content.keyboardShortcut(KeyEquivalent(key), modifiers: .command)
        } else {
            content
        }
    }
}
