// NavigationStore.swift — the single source of truth for the active workspace
// route. Owned by AppCoordinator and injected so the rail and the central
// PrimaryWorkspaceRouter stay in agreement.

import SwiftUI

@MainActor
final class NavigationStore: ObservableObject {
    // Chat is the main workspace and the default first-launch surface
    // (recovery directive §0.3 / §3.3): a conversation turn is the primary
    // entry point for agent-driven work.
    @Published var route: WorkspaceRoute = .chat
}
