import XCTest
import SwiftUI

@testable import OpenSKSStudio

/// The Swift capability models must decode exactly what `opensks capability
/// report` emits — including capabilities whose empty `evidence_refs`/`actions`
/// are omitted from the JSON (recovery directive §18).
final class CapabilityTests: XCTestCase {
    @MainActor
    func testDecodesReportWithAndWithoutEvidence() throws {
        let json = """
        {
          "schema": "opensks.runtime-capability-report.v1",
          "tool_registry": {
            "schema": "opensks.tool-registry.v1",
            "registry_id": "opensks-runtime-tools",
            "revision": 1,
            "tools": [
              {
                "schema": "opensks.tool-descriptor.v1",
                "name": "skill.invoke",
                "display_name": "Invoke Skill",
                "description": "Load an allowlisted local skill route as bounded context.",
                "permission": "ask",
                "availability": "available",
                "reason_code": "local_skill_registry_executable",
                "input_schema": {"type": "object"}
              },
              {
                "schema": "opensks.tool-descriptor.v1",
                "name": "image.generate",
                "display_name": "Generate Image",
                "description": "Generate an image through a provider-backed image lane.",
                "permission": "ask",
                "availability": "unavailable",
                "reason_code": "executor_pending",
                "input_schema": {"type": "object"},
                "evidence_refs": ["tool-registry:canonical-catalog"]
              }
            ]
          },
          "capabilities": [
            {"schema":"opensks.runtime-capability.v1","id":"git.commit","title":"Git commit","maturity":"live","available":true,"reason_code":"reviewed_index_hash_commit_path","evidence_refs":["crate:opensks-git-service"]},
            {"schema":"opensks.runtime-capability.v1","id":"web.research","title":"Web research tool","maturity":"unavailable","available":false,"reason_code":"no_web_tool_implementation"}
          ]
        }
        """
        let report = try XCTUnwrap(RuntimeCapabilityReport.decode(from: Data(json.utf8)))
        XCTAssertEqual(report.schema, "opensks.runtime-capability-report.v1")
        XCTAssertEqual(report.capabilities.count, 2)
        XCTAssertEqual(report.toolRegistry?.registryId, "opensks-runtime-tools")
        XCTAssertEqual(report.toolRegistry?.tools.count, 2)

        let skill = try XCTUnwrap(report.toolRegistry?.descriptor(named: "skill.invoke"))
        XCTAssertEqual(skill.permission, .ask)
        XCTAssertEqual(skill.permission.displayLabel, "Ask")
        XCTAssertEqual(skill.availability, .available)
        XCTAssertTrue(skill.availability.isAvailable)
        XCTAssertEqual(skill.availability.displayLabel, "Available")
        XCTAssertEqual(skill.reasonCode, "local_skill_registry_executable")
        // Omitted evidence_refs decode to empty, not a failure.
        XCTAssertTrue(skill.evidenceRefs.isEmpty)

        let image = try XCTUnwrap(report.toolRegistry?.descriptor(named: "image.generate"))
        XCTAssertEqual(image.availability, .unavailable)
        XCTAssertFalse(image.availability.isAvailable)
        XCTAssertEqual(image.availability.displayLabel, "Disabled")
        XCTAssertEqual(image.evidenceRefs, ["tool-registry:canonical-catalog"])
        XCTAssertEqual(report.toolRegistry?.availableToolCount, 1)
        XCTAssertEqual(report.toolRegistry?.unavailableToolCount, 1)
        XCTAssertEqual(report.toolRegistry?.sortedTools.map(\.name), ["skill.invoke", "image.generate"])
        XCTAssertNotNil(
            ImageRenderer(content: ToolRegistryStatusView(registry: try XCTUnwrap(report.toolRegistry))).nsImage,
            "tool registry status view must render the available and disabled rows"
        )

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

    func testOlderReportWithoutToolRegistryStillDecodes() throws {
        let json = """
        {
          "schema": "opensks.runtime-capability-report.v1",
          "capabilities": [
            {"schema":"opensks.runtime-capability.v1","id":"git.status","title":"Git status","maturity":"live","available":true,"reason_code":"git_service_read_only"}
          ]
        }
        """
        let report = try XCTUnwrap(RuntimeCapabilityReport.decode(from: Data(json.utf8)))
        XCTAssertNil(report.toolRegistry)
        XCTAssertEqual(report.capabilities.count, 1)
    }

    func testMaturityLabelsCoverAllCases() {
        XCTAssertEqual(CapabilityMaturity.foundation.displayLabel, "Needs setup")
        XCTAssertEqual(CapabilityMaturity.simulation.displayLabel, "Simulation")
        XCTAssertEqual(CapabilityMaturity.degraded.displayLabel, "Limited")
    }
}
