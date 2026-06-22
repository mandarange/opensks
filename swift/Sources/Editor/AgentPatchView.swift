// AgentPatchView.swift — open an agent's unified-diff patch in a read-only diff
// view (PR-033).
//
// An agent (or any external producer) hands the editor a unified-diff patch as a
// string; this view renders it read-only, reusing the same hunk rendering as the
// conflict "Compare" surface. It is observable (the patch is plainly visible)
// and never mutates the workspace — applying a patch is a separate, explicit
// action outside this view.

import SwiftUI

/// A read-only viewer for a unified-diff patch (e.g. produced by an agent).
struct AgentPatchView: View {
    let title: String
    let patch: String

    init(title: String = "Agent patch", patch: String) {
        self.title = title
        self.patch = patch
    }

    var body: some View {
        DiffHunkView(title: title, unifiedPatch: patch)
            .accessibilityIdentifier("editor.agentPatch.view")
    }
}
