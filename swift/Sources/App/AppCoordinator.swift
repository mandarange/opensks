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

    /// Node-level pipeline projections keyed by run id (PR-029). The `.graph`
    /// route and the conversation thread's `PipelineRunCard`s both read live
    /// projections from here. Multiple concurrent runs coexist (one reducer per
    /// run id), so switching the selected run shows that run's nodes.
    let pipelines = PipelineProjectionStore()

    /// The run whose live graph the `.graph` route renders. Set when an operator
    /// opens a run's graph (e.g. from a `PipelineRunCard`'s "Open live graph").
    @Published var activeGraphRunId: String?

    /// The READ-ONLY Git studio store (PR-034). Starts with a live service rooted
    /// at the process working directory; rebound to the resolved workspace +
    /// bundled CLI via `bindGit(cli:workspace:)` once `AppState` resolves them.
    let git: GitStudioStore

    init() {
        let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
        let cli = cwd.appendingPathComponent("target/debug/opensks")
        conversations = ConversationStore(
            service: LiveConversationService(cli: cli, workspace: cwd)
        )
        git = GitStudioStore(
            service: LiveGitService(cli: cli, workspace: cwd)
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

    /// Rebind the Git studio to the resolved workspace + bundled CLI and refresh.
    func bindGit(cli: URL, workspace: URL) {
        git.rebind(service: LiveGitService(cli: cli, workspace: workspace))
    }

    /// Focus the `.graph` route on a specific run and navigate there. Used by a
    /// `PipelineRunCard`'s "Open live graph" control. Selecting a different run
    /// id swaps the projection the graph renders without disturbing other runs'
    /// state in the store.
    func openGraph(runId: String) {
        activeGraphRunId = runId
        navigation.route = .graph
    }
}
