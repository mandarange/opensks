import XCTest

@testable import OpenSKSStudio

/// The Swift capability models must decode exactly what `opensks capability
/// report` emits — including capabilities whose empty `evidence_refs`/`actions`
/// are omitted from the JSON (recovery directive §18).
final class CapabilityTests: XCTestCase {
    func testDecodesReportWithAndWithoutEvidence() throws {
        let json = """
        {
          "schema": "opensks.runtime-capability-report.v1",
          "capabilities": [
            {"schema":"opensks.runtime-capability.v1","id":"git.commit","title":"Git commit","maturity":"live","available":true,"reason_code":"reviewed_index_hash_commit_path","evidence_refs":["crate:opensks-git-service"]},
            {"schema":"opensks.runtime-capability.v1","id":"web.research","title":"Web research tool","maturity":"unavailable","available":false,"reason_code":"no_web_tool_implementation"}
          ]
        }
        """
        let report = try XCTUnwrap(RuntimeCapabilityReport.decode(from: Data(json.utf8)))
        XCTAssertEqual(report.schema, "opensks.runtime-capability-report.v1")
        XCTAssertEqual(report.capabilities.count, 2)

        let git = report.capabilities[0]
        XCTAssertEqual(git.maturity, .live)
        XCTAssertEqual(git.maturity.displayLabel, "Available")
        XCTAssertTrue(git.available)
        XCTAssertEqual(git.evidenceRefs, ["crate:opensks-git-service"])

        let web = report.capabilities[1]
        XCTAssertEqual(web.maturity, .unavailable)
        XCTAssertEqual(web.maturity.displayLabel, "Unavailable")
        XCTAssertFalse(web.available)
        // Omitted arrays decode to empty, not a failure.
        XCTAssertTrue(web.evidenceRefs.isEmpty)
        XCTAssertTrue(web.actions.isEmpty)
    }

    func testMaturityLabelsCoverAllCases() {
        XCTAssertEqual(CapabilityMaturity.foundation.displayLabel, "Needs setup")
        XCTAssertEqual(CapabilityMaturity.simulation.displayLabel, "Simulation")
        XCTAssertEqual(CapabilityMaturity.degraded.displayLabel, "Limited")
    }
}
