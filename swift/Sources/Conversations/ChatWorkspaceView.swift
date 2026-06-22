// ChatWorkspaceView.swift — the `.chat` route wrapper (UX-101).
//
// The chat thread gains ONE compact top context bar (recovery directive §15.3):
// branch + uncommitted-change count for the project, alongside the conversation
// title and status — replacing the scattered title/status/proof duplication with a
// single bar. The git context is REAL (sourced from the live `GitStudioStore`), so
// this wrapper observes BOTH the conversation store and the git store and re-renders
// when either changes; it then hands the thread an immutable `ChatGitContext`.

import SwiftUI

/// The real project git context surfaced in the chat top bar. Carries only what the
/// bar shows, computed from the live `GitStatus`/`GitBranches` — never fabricated.
struct ChatGitContext: Equatable {
    var inRepo: Bool
    var branch: String?
    var detached: Bool
    var changedCount: Int
    /// Local branch names (for the informational branch menu).
    var branchNames: [String]

    static let none = ChatGitContext(
        inRepo: false, branch: nil, detached: false, changedCount: 0, branchNames: []
    )

    /// The branch label to display, honest about a detached HEAD.
    var branchLabel: String {
        if let branch, !branch.isEmpty { return branch }
        return detached ? "detached HEAD" : "no branch"
    }
}

struct ChatWorkspaceView: View {
    @ObservedObject var conversations: ConversationStore
    @ObservedObject var git: GitStudioStore
    var pipelines: PipelineProjectionStore?
    var onOpenGraph: (String) -> Void = { _ in }

    var body: some View {
        ConversationThreadView(
            store: conversations,
            pipelines: pipelines,
            onOpenGraph: onOpenGraph,
            gitContext: gitContext
        )
    }

    private var gitContext: ChatGitContext {
        let status = git.status
        return ChatGitContext(
            inRepo: status.inRepo,
            branch: status.branch,
            detached: status.detached,
            changedCount: status.entries.count,
            branchNames: git.branches.branches.map(\.name)
        )
    }
}
