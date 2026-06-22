// GraphLayout.swift — a deterministic, dependency-free layout that assigns a
// position to every node of a `PipelineExecutionProjection`. The same layout is
// consumed by the mini-graph strip on `PipelineRunCard` and by the full
// `PipelineGraphView` canvas, so both surfaces agree on node placement.
//
// The projection (PR-029) carries no explicit edge set — nodes are an ordered
// list rebuilt from the event stream. We therefore lay them out in a stable,
// LAYERED / COLUMNAR grid: column = dependency index / rows-per-column. The
// order is the node array's own order (which the reducer keeps stable: a node
// first appears in the order its first event arrived), so the layout is a pure
// function of the projection and never jitters between frames.
//
// Positions are in an abstract "layout space" (origin top-left, +y down). The
// consumer maps layout space into view space with its own pan/zoom transform;
// the layout itself knows nothing about zoom, so 1 node and 1,000 nodes lay out
// by the exact same arithmetic in O(n).

import CoreGraphics

/// One placed node: its identity plus a center point in layout space.
struct GraphNodePosition: Equatable {
    let nodeId: String
    /// Index into the projection's node array — the deterministic ordinal that
    /// drives column/row assignment and acts as a stable tie-break.
    let index: Int
    let center: CGPoint
}

/// A computed layout: every node placed, plus the bounding box that contains
/// them (used for `fit`). Pure value type; cheap to recompute.
struct GraphLayout: Equatable {
    /// Node placements in projection order (so `first`/`last` are meaningful).
    let positions: [GraphNodePosition]
    /// Fast lookup by node id (e.g. for the inspector / hit-testing).
    let positionsById: [String: GraphNodePosition]
    /// The tight box around all node centers, expanded by one node radius so a
    /// node drawn at the edge is fully inside. Empty layouts get a unit box.
    let contentBounds: CGRect
    /// Per-node visual radius used by both the strip and the canvas.
    let nodeRadius: CGFloat
    /// Number of columns the nodes were spread across.
    let columnCount: Int

    /// Geometry knobs. Defaults are tuned so a few-node run looks airy and a
    /// thousand-node run still fits a reasonable layout-space rectangle.
    struct Metrics: Equatable {
        var nodeRadius: CGFloat = 11
        var horizontalSpacing: CGFloat = 64
        var verticalSpacing: CGFloat = 40
        /// Rows stacked in a column before wrapping to the next column. This is
        /// what makes the layout "layered": each column is one dependency tier.
        var rowsPerColumn: Int = 12
        var padding: CGFloat = 28

        static let `default` = Metrics()
        /// A denser variant for the compact mini-strip.
        static let strip = Metrics(
            nodeRadius: 4,
            horizontalSpacing: 16,
            verticalSpacing: 0,
            rowsPerColumn: 1,
            padding: 8
        )
    }

    /// Build a layout for a projection's nodes. Deterministic: identical input
    /// (same node ids in the same order) yields identical output.
    init(projection: PipelineExecutionProjection, metrics: Metrics = .default) {
        self.init(nodes: projection.nodes, metrics: metrics)
    }

    /// Build a layout from a raw node list (the projection convenience above
    /// forwards here). Kept separate so tests and the strip can lay out a slice.
    init(nodes: [NodeExecutionProjection], metrics: Metrics = .default) {
        let radius = metrics.nodeRadius
        let rowsPerColumn = max(1, metrics.rowsPerColumn)

        var placed: [GraphNodePosition] = []
        placed.reserveCapacity(nodes.count)
        var byId: [String: GraphNodePosition] = [:]
        byId.reserveCapacity(nodes.count)

        for (index, node) in nodes.enumerated() {
            // Layered grid: walk down a column, then wrap to the next column.
            let column = index / rowsPerColumn
            let row = index % rowsPerColumn
            let x = metrics.padding + CGFloat(column) * metrics.horizontalSpacing
            let y = metrics.padding + CGFloat(row) * metrics.verticalSpacing
            let position = GraphNodePosition(
                nodeId: node.nodeId,
                index: index,
                center: CGPoint(x: x, y: y)
            )
            placed.append(position)
            byId[node.nodeId] = position
        }

        self.positions = placed
        self.positionsById = byId
        self.nodeRadius = radius
        self.columnCount = nodes.isEmpty
            ? 0
            : ((nodes.count - 1) / rowsPerColumn) + 1

        if placed.isEmpty {
            self.contentBounds = CGRect(x: 0, y: 0, width: 1, height: 1)
        } else {
            var minX = CGFloat.greatestFiniteMagnitude
            var minY = CGFloat.greatestFiniteMagnitude
            var maxX = -CGFloat.greatestFiniteMagnitude
            var maxY = -CGFloat.greatestFiniteMagnitude
            for p in placed {
                minX = min(minX, p.center.x)
                minY = min(minY, p.center.y)
                maxX = max(maxX, p.center.x)
                maxY = max(maxY, p.center.y)
            }
            // Expand by one radius + padding so edge nodes are fully contained.
            let inset = radius + metrics.padding
            self.contentBounds = CGRect(
                x: minX - inset,
                y: minY - inset,
                width: (maxX - minX) + inset * 2,
                height: (maxY - minY) + inset * 2
            )
        }
    }

    /// The scale + offset that fits `contentBounds` centered inside `viewSize`,
    /// clamped to `[minScale, maxScale]`. Used by `fit` and the initial frame.
    func fitTransform(
        in viewSize: CGSize,
        minScale: CGFloat = 0.05,
        maxScale: CGFloat = 4
    ) -> (scale: CGFloat, offset: CGSize) {
        guard contentBounds.width > 0, contentBounds.height > 0,
              viewSize.width > 0, viewSize.height > 0 else {
            return (1, .zero)
        }
        let rawScale = min(
            viewSize.width / contentBounds.width,
            viewSize.height / contentBounds.height
        )
        let scale = min(max(rawScale, minScale), maxScale)
        // Center the (scaled) content box inside the view.
        let scaledWidth = contentBounds.width * scale
        let scaledHeight = contentBounds.height * scale
        let offsetX = (viewSize.width - scaledWidth) / 2 - contentBounds.minX * scale
        let offsetY = (viewSize.height - scaledHeight) / 2 - contentBounds.minY * scale
        return (scale, CGSize(width: offsetX, height: offsetY))
    }
}
