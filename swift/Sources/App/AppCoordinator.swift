// AppCoordinator.swift — owns cross-cutting UI stores and is the seam for
// decomposing the legacy AppState God object in later PRs. PR-022 introduces it
// owning navigation; subsequent PRs migrate conversation / run / editor / git /
// design stores here off AppState.

import SwiftUI

@MainActor
final class AppCoordinator: ObservableObject {
    let navigation = NavigationStore()
}
