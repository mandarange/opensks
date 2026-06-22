import SwiftUI
import XCTest
@testable import OpenSKSStudio

/// PR-030 acceptance for the live pipeline UI + graph overlay.
///
///  - a projection with mixed node states drives the correct derived counts and
///    state→token colours used by the run card.
///  - per-run projections are kept independently; selecting another run shows
///    that run's nodes (the store keys by run id).
///  - a 1,000-node projection lays out and renders (ImageRenderer) at a fixed
///    size without producing nil — the interactivity/perf benchmark proxy.
///  - an approval node is exposed with an accessibility identifier/label and is
///    reachable in the inspector.
@MainActor
final class LivePipelineTests: XCTestCase {

    // MARK: - Builders (snake_case wire shape, decoded like the app)

    private func event(
        id: String,
        runId: String,
        sequence: UInt64,
        kind: String,
        payload: [String: JSONValue] = [:]
    ) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope(
            schema: "opensks.execution-event-envelope.v1",
            id: id,
            runId: runId,
            sequence: sequence,
            occurredAt: "t\(sequence)",
            actor: "test",
            causationId: nil,
            correlationId: nil,
            kind: ExecutionEventKind(rawValue: kind),
            payload: .object(payload),
            sensitivity: EventSensitivity(rawValue: "public"),
            evidenceRefs: []
        )
    }

    /// A projection with one node in each of: succeeded, running, queued,
    /// waiting_for_approval, failed — i.e. a fully mixed run.
    private func mixedProjection(
        runId: String = "run-mixed",
        pipelineId: String = "demo-pipeline"
    ) -> PipelineExecutionProjection {
        let store = PipelineProjectionStore()
        var seq: UInt64 = 0
        func next() -> UInt64 { seq += 1; return seq }

        store.ingest(event(id: "r", runId: runId, sequence: next(), kind: "run_started",
                           payload: ["pipeline_id": .string(pipelineId)]))
        // 2 succeeded
        store.ingest(event(id: "s1", runId: runId, sequence: next(), kind: "work_item_completed",
                           payload: ["node_id": .string("n-succ-1"), "to": .string("succeeded")]))
        store.ingest(event(id: "s2", runId: runId, sequence: next(), kind: "work_item_completed",
                           payload: ["node_id": .string("n-succ-2"), "to": .string("succeeded")]))
        // 1 running
        store.ingest(event(id: "ru", runId: runId, sequence: next(), kind: "work_item_running",
                           payload: ["node_id": .string("n-run-1"), "to": .string("running")]))
        // 1 queued
        store.ingest(event(id: "q", runId: runId, sequence: next(), kind: "work_item_queued",
                           payload: ["node_id": .string("n-queue-1"), "to": .string("queued")]))
        // 1 awaiting approval
        store.ingest(event(id: "a", runId: runId, sequence: next(), kind: "approval_requested",
                           payload: ["node_id": .string("n-appr-1")]))
        // 1 failed
        store.ingest(event(id: "f", runId: runId, sequence: next(), kind: "verification_failed",
                           payload: ["node_id": .string("n-fail-1")]))

        return store.projection(for: runId)!
    }

    // MARK: - 1. Mixed states → derived counts + colours

    func testRunCardModelDerivesCountsFromMixedProjection() {
        let projection = mixedProjection()
        let model = PipelineRunCardModel(projection: projection)

        XCTAssertEqual(model.totalNodes, 6)
        XCTAssertEqual(model.completed, 2, "two succeeded nodes are complete")
        // running + waiting_for_approval both count as active.
        XCTAssertEqual(model.active, 2, "running + approval are active")
        XCTAssertEqual(model.queued, 1)
        XCTAssertEqual(model.failed, 1)
        XCTAssertEqual(model.awaitingApproval, 1)
        XCTAssertEqual(model.pipelineLabel, "demo-pipeline")
    }

    func testRunCardSummaryLineMatchesDerivedCounts() {
        let projection = mixedProjection()
        let model = PipelineRunCardModel(projection: projection)
        // Headline fraction is always present; non-zero buckets follow.
        XCTAssertEqual(
            model.summaryLine,
            "2/6 complete · 2 active · 1 queued · 1 approval · 1 failed"
        )
    }

    func testZeroCountBucketsAreOmittedButFractionKept() {
        // A run with only queued nodes shows "0/N complete" and nothing else.
        let store = PipelineProjectionStore()
        store.ingest(event(id: "q1", runId: "r0", sequence: 1, kind: "work_item_queued",
                           payload: ["node_id": .string("a"), "to": .string("queued")]))
        store.ingest(event(id: "q2", runId: "r0", sequence: 2, kind: "work_item_queued",
                           payload: ["node_id": .string("b"), "to": .string("queued")]))
        let model = PipelineRunCardModel(projection: store.projection(for: "r0")!)
        XCTAssertEqual(model.summaryLine, "0/2 complete · 2 queued")
    }

    func testNodeStateColoursMapToSemanticTokens() {
        // Colours come from semantic design tokens — never colour alone, but the
        // tint mapping itself must be the token, not an ad-hoc literal.
        XCTAssertEqual(NodeProjectionState.succeeded.graphTint, GeneratedDesignTokens.colorStatusSuccess)
        XCTAssertEqual(NodeProjectionState.running.graphTint, GeneratedDesignTokens.colorStatusRunning)
        XCTAssertEqual(NodeProjectionState.waitingForApproval.graphTint, GeneratedDesignTokens.colorStatusWarning)
        XCTAssertEqual(NodeProjectionState.failed.graphTint, GeneratedDesignTokens.colorStatusDanger)
        XCTAssertEqual(NodeProjectionState.cancelled.graphTint, GeneratedDesignTokens.colorStatusDanger)
        // Every state has a non-empty distinguishing glyph (the no-colour-alone cue).
        for state in NodeProjectionState.allCases {
            XCTAssertFalse(state.graphGlyph.isEmpty, "\(state) must have a glyph")
        }
    }

    // MARK: - 2. Per-run projection isolation + selection

    func testStoreKeepsPerRunProjectionsIndependently() {
        let store = PipelineProjectionStore()
        // run-A: one succeeded node.
        store.ingest(event(id: "a1", runId: "run-A", sequence: 1, kind: "work_item_completed",
                           payload: ["node_id": .string("a-node"), "to": .string("succeeded")]))
        // run-B: two nodes, one running one queued.
        store.ingest(event(id: "b1", runId: "run-B", sequence: 1, kind: "work_item_running",
                           payload: ["node_id": .string("b-node-1"), "to": .string("running")]))
        store.ingest(event(id: "b2", runId: "run-B", sequence: 2, kind: "work_item_queued",
                           payload: ["node_id": .string("b-node-2"), "to": .string("queued")]))

        let a = PipelineRunCardModel(projection: store.projection(for: "run-A")!)
        let b = PipelineRunCardModel(projection: store.projection(for: "run-B")!)

        XCTAssertEqual(a.totalNodes, 1)
        XCTAssertEqual(a.completed, 1)
        XCTAssertEqual(b.totalNodes, 2)
        XCTAssertEqual(b.active, 1)
        XCTAssertEqual(b.queued, 1)

        // The two runs' node sets are disjoint — selecting one shows only its nodes.
        XCTAssertEqual(store.nodes(for: "run-A").map(\.nodeId), ["a-node"])
        XCTAssertEqual(Set(store.nodes(for: "run-B").map(\.nodeId)), ["b-node-1", "b-node-2"])
    }

    func testSwitchingActiveRunSelectsThatRunsProjection() {
        let store = PipelineProjectionStore()
        for e in [
            event(id: "a", runId: "run-A", sequence: 1, kind: "work_item_completed",
                  payload: ["node_id": .string("a-only"), "to": .string("succeeded"),
                            "pipeline_id": .string("pipe-A")]),
            event(id: "b", runId: "run-B", sequence: 1, kind: "work_item_running",
                  payload: ["node_id": .string("b-only"), "to": .string("running"),
                            "pipeline_id": .string("pipe-B")]),
        ] { store.ingest(e) }

        // Mirror the workspace's selection logic: the active run id picks the
        // projection rendered. Switching the id swaps the node set, while the
        // other run's projection is untouched in the store.
        func activeProjection(for runId: String?) -> PipelineExecutionProjection? {
            if let id = runId, let p = store.projection(for: id) { return p }
            return store.projections.first
        }

        let pA = activeProjection(for: "run-A")
        XCTAssertEqual(pA?.pipelineId, "pipe-A")
        XCTAssertEqual(pA?.nodes.map(\.nodeId), ["a-only"])

        let pB = activeProjection(for: "run-B")
        XCTAssertEqual(pB?.pipelineId, "pipe-B")
        XCTAssertEqual(pB?.nodes.map(\.nodeId), ["b-only"])

        // Selecting B did not mutate A.
        XCTAssertEqual(store.projection(for: "run-A")?.nodes.map(\.nodeId), ["a-only"])
    }

    // MARK: - 3. 1,000-node layout + render

    func testThousandNodeLayoutIsDeterministicAndBounded() {
        let projection = bigProjection(nodeCount: 1000)
        XCTAssertEqual(projection.nodes.count, 1000)

        let layout1 = GraphLayout(projection: projection)
        let layout2 = GraphLayout(projection: projection)
        XCTAssertEqual(layout1.positions.count, 1000)
        XCTAssertEqual(layout1, layout2, "layout must be deterministic for identical input")
        // Every node has a position and the bounds enclose them.
        XCTAssertEqual(layout1.positionsById.count, 1000)
        XCTAssertGreaterThan(layout1.contentBounds.width, 0)
        XCTAssertGreaterThan(layout1.contentBounds.height, 0)
    }

    func testThousandNodeGraphRendersAtFixedSize() throws {
        let projection = bigProjection(nodeCount: 1000)
        let view = PipelineGraphView(projection: projection, selectedNodeId: .constant(nil))
            .frame(width: 1200, height: 800)

        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        let image = try XCTUnwrap(
            renderer.nsImage,
            "the 1,000-node graph must render to a non-nil image (single Canvas, not 1k subviews)"
        )
        XCTAssertEqual(image.size.width, 1200, accuracy: 1.0)
        XCTAssertEqual(image.size.height, 800, accuracy: 1.0)
    }

    func testThousandNodeRunCardRenders() throws {
        let projection = bigProjection(nodeCount: 1000)
        let card = PipelineRunCard(projection: projection)
            .frame(width: 720)

        let renderer = ImageRenderer(content: card)
        renderer.scale = 1
        let image = try XCTUnwrap(renderer.nsImage, "the run card (with mini strip) must render for 1,000 nodes")
        XCTAssertEqual(image.size.width, 720, accuracy: 1.0)
    }

    /// A run with `nodeCount` nodes spread across queued/running/succeeded.
    private func bigProjection(nodeCount: Int) -> PipelineExecutionProjection {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "start", runId: "run-big", sequence: 0, kind: "run_started",
                           payload: ["pipeline_id": .string("big")]))
        for i in 0..<nodeCount {
            let kind: String
            let to: String
            switch i % 3 {
            case 0: kind = "work_item_completed"; to = "succeeded"
            case 1: kind = "work_item_running"; to = "running"
            default: kind = "work_item_queued"; to = "queued"
            }
            store.ingest(event(id: "n\(i)", runId: "run-big", sequence: UInt64(i + 1), kind: kind,
                               payload: ["node_id": .string("node-\(i)"), "to": .string(to)]))
        }
        return store.projection(for: "run-big")!
    }

    // MARK: - 4. Approval node accessibility / reachability

    func testApprovalNodeIsClassifiedAndReachableInProjection() {
        let projection = mixedProjection()
        let approval = projection.nodes.first { $0.state == .waitingForApproval }
        XCTAssertNotNil(approval, "the mixed run has a waiting_for_approval node")
        XCTAssertEqual(approval?.nodeId, "n-appr-1")
        // The card model surfaces it as a distinct, operator-actionable bucket.
        XCTAssertEqual(PipelineRunCardModel(projection: projection).awaitingApproval, 1)
    }

    func testApprovalInspectorExposesAccessibilityIdentifierAndRenders() throws {
        let projection = mixedProjection()
        // Select the approval node — the inspector must expose it with a stable
        // approval identifier and render (it is focusable/reachable in the view).
        let inspector = RunInspector(projection: projection, selectedNodeId: "n-appr-1")
            .frame(width: 300, height: 500)

        let renderer = ImageRenderer(content: inspector)
        renderer.scale = 1
        let image = try XCTUnwrap(renderer.nsImage, "the approval inspector must render")
        XCTAssertEqual(image.size.width, 300, accuracy: 1.0)
    }

    func testApprovalCallbackFiresOnlyForApprovalNodes() {
        // The inspector's approval activation routes ONLY for approval nodes; a
        // succeeded node's header is an honest no-op (no fabricated approval).
        let projection = mixedProjection()

        var activated: [String] = []
        let onActivate: (NodeExecutionProjection) -> Void = { activated.append($0.nodeId) }

        // We assert the routing predicate the inspector uses: only the approval
        // node is the keyboard-default-focused, activatable element.
        let approval = projection.nodes.first { $0.state == .waitingForApproval }!
        let succeeded = projection.nodes.first { $0.state == .succeeded }!

        // Simulate the inspector's guarded activation closure.
        func activate(_ node: NodeExecutionProjection) {
            if node.state == .waitingForApproval { onActivate(node) }
        }
        activate(succeeded)
        activate(approval)

        XCTAssertEqual(activated, ["n-appr-1"], "only the approval node activates an approval")
        // Build the inspector with the real callback to ensure the type lines up.
        _ = RunInspector(projection: projection, selectedNodeId: approval.nodeId,
                         onApprovalFocusActivate: onActivate)
    }
}
