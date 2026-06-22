// NavigationStore.swift — the single source of truth for the active workspace
// route. Owned by AppCoordinator and injected so the rail and the central
// PrimaryWorkspaceRouter stay in agreement.

import SwiftUI

@MainActor
final class NavigationStore: ObservableObject {
    @Published var route: WorkspaceRoute = .home
}
