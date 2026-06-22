// RunCard.swift — an inline card surfacing ONE deterministic engine run linked
// to an assistant turn (PR-027). It shows a short run id, the relation
// ("primary"), and the run's final state via a `StatusPill` (running / success /
// danger — glyph + tint, never colour alone). There is no live token streaming
// here: the deterministic run has already completed, so the card honestly shows
// its final state. Live streaming arrives in PR-029 / PR-030.

import SwiftUI

struct RunCard: View {
    let run: ConversationRunRef

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "sparkles")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.violet)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text("Run")
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(Theme.textSoft)
                    Text(shortRunID)
                        .font(Theme.mono(11))
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
                Text(run.relation.capitalized)
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }

            Spacer(minLength: 0)

            StatusPill(kind: run.runState.pillKind, label: run.runState.displayLabel)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .frame(maxWidth: 720, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Run \(shortRunID), \(run.relation), \(run.runState.displayLabel)")
        .accessibilityIdentifier("run.card.\(run.runId)")
    }

    /// First 8 characters of the run id for a compact, honest reference (the
    /// full id is selectable for copy).
    private var shortRunID: String {
        String(run.runId.prefix(8))
    }
}
