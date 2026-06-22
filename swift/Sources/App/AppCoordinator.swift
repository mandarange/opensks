// AppCoordinator.swift — owns cross-cutting UI stores and is the seam for
// decomposing the legacy AppState God object in later PRs. PR-022 introduces it
// owning navigation; subsequent PRs migrate conversation / run / editor / git /
// design stores here off AppState.

import SwiftUI

@MainActor
final class AppCoordinator: ObservableObject {
    let navigation = NavigationStore()

    /// Conversation sidebar + thread store (PR-025). It starts with a live
    /// service rooted at the process working directory; once the real workspace
    /// path + bundled CLI are resolved (RootView.onAppear reads `AppState`), the
    /// service is rebound via `bindConversations(cli:workspace:)`.
    let conversations: ConversationStore

    init() {
        let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
        let cli = cwd.appendingPathComponent("target/debug/opensks")
        conversations = ConversationStore(
            service: LiveConversationService(cli: cli, workspace: cwd)
        )
    }

    /// Rebind the conversation store's live service to the resolved workspace and
    /// bundled CLI (same values `AppState` uses), then reload.
    func bindConversations(cli: URL, workspace: URL) {
        conversations.updateService(
            LiveConversationService(cli: cli, workspace: workspace)
        )
        Task { await conversations.load() }
    }
}
