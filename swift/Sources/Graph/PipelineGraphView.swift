// PipelineGraphView.swift — the FULL pannable + zoomable pipeline canvas (PR-030).
//
// Performance contract: the entire node set is drawn into a SINGLE SwiftUI
// `Canvas` (one drawing surface), NOT one subview per node. This is what keeps
// 1,000 nodes interactive — pan/zoom mutate two scalars (scale + offset) and
// trigger one redraw, with no view-tree churn. Node positions come from the
// deterministic `GraphLayout`; node colours come from the live projection's node
// states via `graphTint` (semantic tokens).
//
// Interaction:
//   * Pan  — drag anywhere on the canvas.
//   * Zoom — pinch / trackpad magnify (`MagnificationGesture`) plus the on-canvas
//            Zoom in / Zoom out buttons (GRAPH-101: there is no mouse scroll-wheel
//            zoom handler, so this comment no longer claims one — the controls above
//            are the actual zoom surface).
//   * Fit  — a button recentres + rescales the whole graph into view; the viewport
//            also refits automatically when the shown run changes (GRAPH-102).
//   * LOD  — labels and per-node glyphs are hidden below a zoom threshold so a
//            zoomed-out thousand-node graph stays legible and cheap to draw.
//   * Select — a tap maps the point back through the transform to the nearest
//            node, which drives `RunInspector`.
//   * Accessibility — the canvas exposes an accessible "Pipeline outline" (one
//            selectable row per node, A11Y-102) so it is not a single opaque image.
//   * Reduced motion — when Accessibility "reduce motion" is on, the active-node
//            pulse is disabled (states are still distinguishable by tint+glyph).

import SwiftUI

struct PipelineGraphView: View {
    let projection: PipelineExecutionProjection
    /// Two-way selection so the host can show a `RunInspector` for the node.
    @Binding var selectedNodeId: String?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    // Viewport transform. `scale` is layout→view zoom; `offset` is the pan in
    // view points. `gestureScale` / `gestureOffset` are the in-flight deltas.
    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @GestureState private var gestureScale: CGFloat = 1
    @GestureState private var gestureOffset: CGSize = .zero
    /// The run id the viewport was last fitted for (GRAPH-102). When the shown run
    /// changes, the viewport refits the new run instead of stranding it at the
    /// previous run's pan/zoom. `nil` = never fitted.
    @State private var fittedRunId: String?

    // A slow phase used only for the active-node pulse; frozen when reduce-motion.
    @State private var pulsePhase: CGFloat = 0

    private let minScale: CGFloat = 0.05
    private let maxScale: CGFloat = 4
    /// Below this effective zoom, labels/glyphs are dropped (LOD).
    private let labelLODThreshold: CGFloat = 0.6
    private let glyphLODThreshold: CGFloat = 0.35

    private var layout: GraphLayout { GraphLayout(projection: projection) }
    private var effectiveScale: CGFloat {
        min(max(scale * gestureScale, minScale), maxScale)
    }

    init(projection: PipelineExecutionProjection, selectedNodeId: Binding<String?>) {
        self.projection = projection
        self._selectedNodeId = selectedNodeId
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topTrailing) {
                canvas(in: geo.size)
                    // A11Y-102 / §13.6: replace the opaque canvas's accessibility
                    // with an equivalent, keyboard-navigable node outline.
                    .accessibilityRepresentation {
                        PipelineOutlineList(projection: projection, selectedNodeId: $selectedNodeId)
                    }
                overlayControls
            }
            .background(Theme.panelDeep)
            .contentShape(Rectangle())
            .gesture(panGesture)
            .simultaneousGesture(zoomGesture)
            .onTapGesture { location in
                // NOTE: this tap is canvas hit-testing for node SELECTION, not a
                // button-like control — there is no Button here to attach to.
                selectNode(at: location, viewSize: geo.size)
            }
            .onAppear { fitIfNeeded(viewSize: geo.size) }
            .onChange(of: geo.size) { _ in fitIfNeeded(viewSize: geo.size, force: fittedRunId == nil) }
            // GRAPH-102: a run/revision change refits the viewport (needsFit is true
            // because the fitted run no longer matches the shown run).
            .onChange(of: projection.runId) { _ in fitIfNeeded(viewSize: lastViewSize) }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("pipeline.graph.canvas")
        }
        .onAppear(perform: startPulseIfAllowed)
    }

    // MARK: - Canvas

    private func canvas(in viewSize: CGSize) -> some View {
        let currentScale = effectiveScale
        let currentOffset = CGSize(
            width: offset.width + gestureOffset.width,
            height: offset.height + gestureOffset.height
        )
        let showGlyphs = currentScale >= glyphLODThreshold
        let showLabels = currentScale >= labelLODThreshold

        return Canvas { context, size in
            let nodes = projection.nodes
            guard !nodes.isEmpty else { return }
            let layout = self.layout

            // Edges first (so nodes draw on top — directive §13.4 draw order):
            // connect consecutive nodes to show the execution flow instead of
            // isolated dots (PIPE-003). This sequential baseline is replaced by a
            // real dependency topology once the topology snapshot is wired.
            func viewPoint(_ p: CGPoint) -> CGPoint {
                CGPoint(
                    x: p.x * currentScale + currentOffset.width,
                    y: p.y * currentScale + currentOffset.height
                )
            }
            if layout.positions.count > 1 {
                var edges = Path()
                for index in 0..<(layout.positions.count - 1) {
                    edges.move(to: viewPoint(layout.positions[index].center))
                    edges.addLine(to: viewPoint(layout.positions[index + 1].center))
                }
                context.stroke(
                    edges,
                    with: .color(Theme.stroke),
                    lineWidth: max(1, layout.nodeRadius * currentScale * 0.18)
                )
            }

            for position in layout.positions {
                let node = nodes[position.index]
                // Map layout space → view space through pan/zoom.
                let center = CGPoint(
                    x: position.center.x * currentScale + currentOffset.width,
                    y: position.center.y * currentScale + currentOffset.height
                )
                let radius = layout.nodeRadius * currentScale

                // Cull anything fully outside the viewport (bounded draw cost).
                if center.x + radius < 0 || center.x - radius > size.width
                    || center.y + radius < 0 || center.y - radius > size.height {
                    continue
                }

                drawNode(
                    node: node,
                    center: center,
                    radius: radius,
                    isSelected: node.nodeId == selectedNodeId,
                    showGlyph: showGlyphs,
                    showLabel: showLabels,
                    context: &context
                )
            }
        }
        .drawingGroup() // Rasterise the canvas for smooth pan/zoom of many nodes.
    }

    private func drawNode(
        node: NodeExecutionProjection,
        center: CGPoint,
        radius: CGFloat,
        isSelected: Bool,
        showGlyph: Bool,
        showLabel: Bool,
        context: inout GraphicsContext
    ) {
        let tint = node.state.graphTint

        // Active-node pulse ring (motion only when reduce-motion is OFF).
        if !reduceMotion, node.state == .running || node.state == .dispatching {
            let pulseRadius = radius + 4 + pulsePhase * 4
            let ring = Path(ellipseIn: CGRect(
                x: center.x - pulseRadius, y: center.y - pulseRadius,
                width: pulseRadius * 2, height: pulseRadius * 2
            ))
            context.stroke(ring, with: .color(tint.opacity(0.35 * (1 - pulsePhase))), lineWidth: 2)
        }

        let nodeRect = CGRect(
            x: center.x - radius, y: center.y - radius,
            width: radius * 2, height: radius * 2
        )
        let circle = Path(ellipseIn: nodeRect)
        context.fill(circle, with: .color(tint.opacity(0.9)))
        // Selection / state border (a second non-colour cue: selected nodes get
        // a thicker, brighter ring).
        context.stroke(
            circle,
            with: .color(isSelected ? Theme.text : tint),
            lineWidth: isSelected ? 2.5 : 1
        )

        // LOD: only draw the state glyph when zoomed in enough to read it. This
        // is the non-colour-alone cue at the canvas level.
        if showGlyph, radius >= 6 {
            let glyph = Image(systemName: node.state.graphGlyph)
            var resolved = context.resolve(glyph)
            resolved.shading = .color(Theme.accentInk)
            let glyphSize = min(radius * 1.1, 14)
            context.draw(
                resolved,
                in: CGRect(
                    x: center.x - glyphSize / 2,
                    y: center.y - glyphSize / 2,
                    width: glyphSize,
                    height: glyphSize
                )
            )
        }

        // LOD: node label only at high zoom, to the right of the node.
        if showLabel {
            var resolvedText = context.resolve(Text(node.nodeId).font(Theme.mono(9)))
            resolvedText.shading = .color(Theme.muted)
            context.draw(
                resolvedText,
                at: CGPoint(x: center.x + radius + 4, y: center.y),
                anchor: .leading
            )
        }
    }

    // MARK: - Overlay controls

    private var overlayControls: some View {
        VStack(spacing: 6) {
            graphButton("Fit", systemImage: "arrow.up.left.and.arrow.down.right") {
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.2)) {
                    let transform = layout.fitTransform(in: lastViewSize, minScale: minScale, maxScale: maxScale)
                    scale = transform.scale
                    offset = transform.offset
                }
            }
            graphButton("Zoom in", systemImage: "plus.magnifyingglass") {
                zoomBy(1.25)
            }
            graphButton("Zoom out", systemImage: "minus.magnifyingglass") {
                zoomBy(0.8)
            }
        }
        .padding(10)
    }

    private func graphButton(_ label: String, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 13, weight: .semibold))
        }
        .buttonStyle(IconTileButtonStyle(size: 34))
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .fill(Theme.panel.opacity(0.9))
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityLabel(label)
        .accessibilityIdentifier("pipeline.graph.control.\(label.replacingOccurrences(of: " ", with: "").lowercased())")
    }

    // MARK: - Gestures

    private var panGesture: some Gesture {
        DragGesture()
            .updating($gestureOffset) { value, state, _ in
                state = value.translation
            }
            .onEnded { value in
                offset.width += value.translation.width
                offset.height += value.translation.height
            }
    }

    private var zoomGesture: some Gesture {
        MagnificationGesture()
            .updating($gestureScale) { value, state, _ in
                state = value
            }
            .onEnded { value in
                scale = min(max(scale * value, minScale), maxScale)
            }
    }

    private func zoomBy(_ factor: CGFloat) {
        withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.15)) {
            scale = min(max(scale * factor, minScale), maxScale)
        }
    }

    // MARK: - Selection (canvas hit-testing)

    private func selectNode(at point: CGPoint, viewSize: CGSize) {
        let s = effectiveScale
        let o = CGSize(
            width: offset.width + gestureOffset.width,
            height: offset.height + gestureOffset.height
        )
        // Inverse-map the tap into layout space, then nearest-node within radius.
        let layoutPoint = CGPoint(
            x: (point.x - o.width) / s,
            y: (point.y - o.height) / s
        )
        let hitRadius = max(layout.nodeRadius, 14 / s)
        var best: (id: String, dist: CGFloat)?
        for position in layout.positions {
            let dx = position.center.x - layoutPoint.x
            let dy = position.center.y - layoutPoint.y
            let dist = (dx * dx + dy * dy).squareRoot()
            if dist <= hitRadius, best == nil || dist < best!.dist {
                best = (position.nodeId, dist)
            }
        }
        selectedNodeId = best?.id
    }

    // MARK: - Fit + pulse lifecycle

    // Remembered so the Fit button (outside GeometryReader's closure scope) can
    // recompute against the live viewport.
    @State private var lastViewSize: CGSize = CGSize(width: 1, height: 1)

    /// Whether the viewport needs a (re)fit for `currentRunId`: either it was never
    /// fitted, or the shown run changed since the last fit (GRAPH-102). Pure so the
    /// run-keyed reset is unit-testable.
    static func needsFit(currentRunId: String, fittedRunId: String?) -> Bool {
        fittedRunId != currentRunId
    }

    private func fitIfNeeded(viewSize: CGSize, force: Bool = false) {
        lastViewSize = viewSize
        guard force || Self.needsFit(currentRunId: projection.runId, fittedRunId: fittedRunId)
        else { return }
        let transform = layout.fitTransform(in: viewSize, minScale: minScale, maxScale: maxScale)
        scale = transform.scale
        offset = transform.offset
        fittedRunId = projection.runId
    }

    private func startPulseIfAllowed() {
        guard !reduceMotion else { return }
        withAnimation(.easeInOut(duration: 1.1).repeatForever(autoreverses: true)) {
            pulsePhase = 1
        }
    }
}

// MARK: - Accessible outline (A11Y-102 / §13.6)

/// An accessible outline EQUIVALENT to the canvas: one selectable, labelled row per
/// node, navigable by keyboard / VoiceOver. It is attached to the canvas via
/// `accessibilityRepresentation`, so it provides the accessibility tree without
/// changing the visual drawing. Activating a row selects that node (driving the
/// inspector), exactly like a canvas tap.
struct PipelineOutlineList: View {
    let projection: PipelineExecutionProjection
    @Binding var selectedNodeId: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Pipeline outline, \(projection.nodes.count) node\(projection.nodes.count == 1 ? "" : "s")")
                .accessibilityAddTraits(.isHeader)
            ForEach(projection.nodes) { node in
                Button {
                    selectedNodeId = node.nodeId
                } label: {
                    Text(Self.rowLabel(node))
                }
                .accessibilityLabel(Self.rowLabel(node))
                .accessibilityAddTraits(node.nodeId == selectedNodeId ? .isSelected : [])
                .accessibilityIdentifier("pipeline.graph.outline.\(node.nodeId)")
            }
        }
        .accessibilityIdentifier("pipeline.graph.outline")
    }

    /// "<node>, <status>" — the spoken / listed label for one node. Pure so the
    /// outline content is unit-testable.
    static func rowLabel(_ node: NodeExecutionProjection) -> String {
        "\(node.nodeId), \(node.state.displayLabel)"
    }
}
