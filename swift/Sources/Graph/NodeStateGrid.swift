// NodeStateGrid.swift — a compact, wrapping grid of one small square per node,
// coloured by `NodeProjectionState` (via `graphTint`, the same mapping the mini
// strip and the full canvas use, so no surface ever disagrees on what a colour
// means). Drawn with `Canvas` — one drawing pass regardless of node count, no
// per-node subview — so a run with hundreds of nodes stays cheap to lay out and
// redraw.
//
// This is the "many parallel workers at a glance" view: unlike `MiniGraphStrip`
// (a single row that truncates once it runs past the available width), this
// view wraps to as many rows as its given height allows. If a run has more
// nodes than fit in the given size, drawing stops at the boundary — the same
// bounded-work truncation `MiniGraphStrip` already applies horizontally — while
// the card header's `summaryLine` and `StatusPill` remain the complete, honest
// source of the aggregate counts regardless of how many cells are physically
// drawn.
import SwiftUI

struct NodeStateGrid: View {
    let states: [NodeProjectionState]
    var cellSize: CGFloat = 7
    var gap: CGFloat = 3

    var body: some View {
        Canvas { context, size in
            guard !states.isEmpty, size.width > 0, size.height > 0 else { return }
            let stride = cellSize + gap
            let columns = max(1, Int((size.width + gap) / stride))
            for (index, state) in states.enumerated() {
                let row = index / columns
                let y = CGFloat(row) * stride
                if y + cellSize > size.height { break }
                let column = index % columns
                let x = CGFloat(column) * stride
                let rect = CGRect(x: x, y: y, width: cellSize, height: cellSize)
                context.fill(
                    Path(roundedRect: rect, cornerRadius: 1.5),
                    with: .color(state.graphTint.opacity(state.gridOpacity))
                )
            }
        }
        .accessibilityHidden(true) // The card header already conveys the summary.
    }
}

private extension NodeProjectionState {
    /// Queued/dispatching cells are drawn at reduced opacity so a large grid
    /// reads at a glance as "how much is actually done or active" instead of a
    /// uniform wall of colour, without inventing a separate colour token.
    var gridOpacity: Double {
        switch self {
        case .queued, .dispatching: return 0.35
        default: return 1.0
        }
    }
}
