// GraphStyle.swift — the single place that maps a `NodeProjectionState` to its
// semantic-token tint and glyph for the graph surfaces (the mini strip, the full
// canvas, and the inspector). Kept in one file so the run card, the canvas, and
// the inspector can never drift on what "running" or "failed" looks like.
//
// IMPORTANT honesty/accessibility note: colour is NEVER the sole signal in the
// product. On the canvas, the tint is paired with a state GLYPH (drawn at higher
// zoom / LOD), and in the card and inspector the same state is shown by a
// `StatusPill` (glyph + tint + label). This extension only centralises the tint
// and glyph; the views are responsible for always showing at least one
// non-colour cue alongside it.

import SwiftUI

extension NodeProjectionState {
    /// Semantic token colour for this state. Drawn into `Canvas`, where a
    /// SwiftUI `StatusPill` cannot be embedded, so the canvas additionally draws
    /// `glyph` at sufficient zoom to satisfy the no-colour-alone rule.
    var graphTint: Color {
        switch self {
        case .queued, .dispatching:
            return Theme.muted
        case .running:
            return GeneratedDesignTokens.colorStatusRunning
        case .waitingForApproval:
            return GeneratedDesignTokens.colorStatusWarning
        case .succeeded:
            return GeneratedDesignTokens.colorStatusSuccess
        case .failed, .cancelled:
            return GeneratedDesignTokens.colorStatusDanger
        case .skipped:
            return Theme.faint
        }
    }

    /// An SF Symbol glyph that, paired with `graphTint`, identifies the state
    /// without relying on colour alone.
    var graphGlyph: String {
        switch self {
        case .queued: return "circle"
        case .dispatching: return "circle.dotted"
        case .running: return "arrow.triangle.2.circlepath"
        case .waitingForApproval: return "exclamationmark.triangle.fill"
        case .succeeded: return "checkmark"
        case .failed: return "xmark"
        case .cancelled: return "minus"
        case .skipped: return "arrow.forward"
        }
    }
}
