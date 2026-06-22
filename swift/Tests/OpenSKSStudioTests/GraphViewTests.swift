import SwiftUI
import XCTest
@testable import OpenSKSStudio

/// PipelineGraphView fixes: GRAPH-102 (run-keyed viewport reset) and A11Y-102 (the
/// accessible node outline). The viewport reset + outline content are pure, so they
/// are unit-tested directly; the views are render-checked for a non-nil tree.
@MainActor
final class GraphViewTests: XCTestCase {
    private func projection(
        runId: String,
        nodes: [(String, NodeProjectionState)]
    ) -> PipelineExecutionProjection {
        var p = PipelineExecutionProjection(runId: runId)
        p.nodes = nodes.map { NodeExecutionProjection(nodeId: $0.0, state: $0.1) }
        return p
    }

    // GRAPH-102: the viewport refits when the shown run changes, but not on a mere
    // resize of the same already-fitted run.
    func testNeedsFitResetsOnRunChange() {
        XCTAssertTrue(
            PipelineGraphView.needsFit(currentRunId: "run-1", fittedRunId: nil),
            "never fitted ⇒ needs fit"
        )
        XCTAssertFalse(
            PipelineGraphView.needsFit(currentRunId: "run-1", fittedRunId: "run-1"),
            "same run already fitted ⇒ no refit (preserves user pan/zoom)"
        )
        XCTAssertTrue(
            PipelineGraphView.needsFit(currentRunId: "run-2", fittedRunId: "run-1"),
            "run changed ⇒ refit the new run (GRAPH-102)"
        )
    }

    // A11Y-102: each outline row names the node AND its status (not colour alone).
    func testOutlineRowLabelIsNodeAndStatus() {
        XCTAssertEqual(
            PipelineOutlineList.rowLabel(NodeExecutionProjection(nodeId: "implement-rust", state: .running)),
            "implement-rust, Running"
        )
        XCTAssertEqual(
            PipelineOutlineList.rowLabel(NodeExecutionProjection(nodeId: "verify", state: .succeeded)),
            "verify, Succeeded"
        )
        XCTAssertEqual(
            PipelineOutlineList.rowLabel(NodeExecutionProjection(nodeId: "deploy", state: .waitingForApproval)),
            "deploy, Awaiting approval"
        )
    }

    func testOutlineRendersForEveryNode() throws {
        let p = projection(
            runId: "run-1",
            nodes: [("plan", .succeeded), ("implement", .running), ("verify", .queued)]
        )
        let outline = PipelineOutlineList(projection: p, selectedNodeId: .constant(nil))
            .frame(width: 320, height: 200)
        XCTAssertNotNil(
            ImageRenderer(content: outline).nsImage,
            "the accessible outline must render for every node"
        )
    }

    func testGraphViewRendersWithOutlineAttached() throws {
        let p = projection(runId: "run-1", nodes: [("a", .running), ("b", .queued)])
        let view = PipelineGraphView(projection: p, selectedNodeId: .constant(nil))
            .frame(width: 600, height: 400)
        XCTAssertNotNil(ImageRenderer(content: view).nsImage)
    }
}
