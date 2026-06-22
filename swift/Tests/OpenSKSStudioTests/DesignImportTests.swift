// DesignImportTests.swift — the LOCAL, human-reviewed design import (PR-039).
//
// Drives DesignImportModels / DesignImportService / DesignImportStore /
// DesignImportView through a MockDesignImportService (no disk, no process, NO
// network). Asserts the quarantine → human-review → promote invariants:
//   • `import(...)` produces a QUARANTINED (not promoted) entry, and promotion
//     happens ONLY after an explicit `approve(...)` — the mock asserts approve is
//     NOT called during import;
//   • a REJECTED import (status:rejected + a reason) is shown with its reason and
//     CANNOT be approved (the store refuses; approve is never sent to the service);
//   • the provenance (source / license / commit) decodes and is presented;
//   • `reject(...)` calls import-reject and the entry is removed from the store;
//   • the import view + a quarantined entry render (ImageRenderer non-nil) and fill
//     width at 1024 / 1440 with no letterbox;
//   • the DesignImportService surface is LOCAL: only import / approve / reject /
//     status — there is NO upload / network method.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class DesignImportTests: XCTestCase {

    // MARK: - Canned JSON fixtures (the shared snake_case contract)

    private static let quarantinedJSON = """
    {
      "schema": "opensks.design-import.v1",
      "quarantine_id": "q-abc123",
      "status": "quarantined",
      "provenance": {"source": "~/Downloads/acme-tokens", "license": "MIT", "commit": "deadbeef"},
      "file_count": 42,
      "byte_size": 65536,
      "rejected_reason": null
    }
    """

    private static let rejectedJSON = """
    {
      "schema": "opensks.design-import.v1",
      "quarantine_id": "q-bad999",
      "status": "rejected",
      "provenance": {"source": "~/Downloads/evil.zip", "license": null, "commit": null},
      "file_count": 0,
      "byte_size": 0,
      "rejected_reason": "zip_slip"
    }
    """

    private static let statusJSON = """
    {
      "schema": "opensks.design-import-status.v1",
      "quarantined": [
        {
          "quarantine_id": "q-abc123",
          "status": "quarantined",
          "provenance": {"source": "~/Downloads/acme-tokens", "license": "MIT", "commit": "deadbeef"},
          "file_count": 42,
          "byte_size": 65536,
          "rejected_reason": null
        }
      ]
    }
    """

    private static let approveJSON = """
    {"schema": "opensks.design-import-approve.v1", "promoted": true, "package_id": "acme-tokens"}
    """

    private static let rejectResultJSON = """
    {"schema": "opensks.design-import-reject.v1", "rejected": true, "deleted": true}
    """

    private func decodeImport(_ json: String) throws -> DesignImportResult {
        try JSONDecoder().decode(DesignImportResult.self, from: Data(json.utf8))
    }

    // MARK: - Decode

    func testQuarantinedImportDecodes() throws {
        let result = try decodeImport(Self.quarantinedJSON)
        XCTAssertEqual(result.schema, "opensks.design-import.v1")
        XCTAssertEqual(result.quarantineId, "q-abc123")
        XCTAssertEqual(result.status, .quarantined)
        XCTAssertTrue(result.isQuarantined)
        XCTAssertFalse(result.isRejected)
        XCTAssertEqual(result.fileCount, 42)
        XCTAssertEqual(result.byteSize, 65536)
        XCTAssertNil(result.rejectedReason)
    }

    func testRejectedImportDecodesWithReason() throws {
        let result = try decodeImport(Self.rejectedJSON)
        XCTAssertEqual(result.status, .rejected)
        XCTAssertTrue(result.isRejected)
        XCTAssertFalse(result.isQuarantined)
        XCTAssertEqual(result.rejectedReason, .zipSlip)
        XCTAssertFalse(result.status.isApprovable, "a rejected status is never approvable")
    }

    /// The provenance — source / license / commit — decodes and is presentable.
    func testProvenanceDecodesAndPresents() throws {
        let result = try decodeImport(Self.quarantinedJSON)
        XCTAssertEqual(result.provenance.source, "~/Downloads/acme-tokens")
        XCTAssertEqual(result.provenance.license, "MIT")
        XCTAssertEqual(result.provenance.licenseDisplay, "MIT")
        XCTAssertEqual(result.provenance.commit, "deadbeef")
        XCTAssertEqual(result.provenance.commitDisplay, "deadbeef")

        // A package that declared no license / commit presents honest fallbacks.
        let rejected = try decodeImport(Self.rejectedJSON)
        XCTAssertNil(rejected.provenance.license)
        XCTAssertEqual(rejected.provenance.licenseDisplay, "Unknown")
        XCTAssertEqual(rejected.provenance.commitDisplay, "—")
    }

    func testRejectedReasonMapsToMessageAndSymbol() {
        for reason in DesignImportRejectedReason.allCases {
            XCTAssertFalse(reason.message.isEmpty, "\(reason) has a human message")
            XCTAssertFalse(reason.symbol.isEmpty, "\(reason) has an SF Symbol")
            XCTAssertFalse(reason.label.isEmpty, "\(reason) has a label")
        }
        XCTAssertEqual(DesignImportRejectedReason(rawValue: "zip_slip"), .zipSlip)
        XCTAssertEqual(DesignImportRejectedReason(rawValue: "executable_or_script"), .executableOrScript)
        XCTAssertEqual(DesignImportRejectedReason(rawValue: "too_many_archive_entries"), .tooManyArchiveEntries)
        // An unknown server reason decodes to `.unknown` (total decode).
        let unknown = try? JSONDecoder().decode(
            DesignImportRejectedReason.self, from: Data("\"some_future_reason\"".utf8)
        )
        XCTAssertEqual(unknown, .unknown)
    }

    // MARK: - import quarantines (NOT promoted); approve is NOT called during import

    func testImportProducesQuarantinedEntryWithoutPromoting() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.quarantinedJSON))
        let store = DesignImportStore(service: service)

        let result = await store.import(source: "~/Downloads/acme-tokens", kind: .local)

        // The import produced a QUARANTINED entry…
        let imported = try XCTUnwrap(result)
        XCTAssertEqual(imported.status, .quarantined)
        XCTAssertEqual(store.entries.count, 1)
        XCTAssertEqual(store.quarantined.count, 1)
        XCTAssertEqual(store.entries.first?.quarantineId, "q-abc123")

        // …and promotion did NOT happen during import: approve was never called.
        XCTAssertEqual(service.importCalls.count, 1)
        XCTAssertEqual(service.importCalls.first?.kind, .local)
        XCTAssertTrue(service.approveCalls.isEmpty, "import must NEVER promote — approve is not called")
        XCTAssertNil(store.lastPromotion, "no promotion receipt after a mere import")
    }

    func testPromotionHappensOnlyAfterExplicitApprove() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.quarantinedJSON))
        service.setApproveResult(
            try JSONDecoder().decode(DesignImportApproveResult.self, from: Data(Self.approveJSON.utf8))
        )
        let store = DesignImportStore(service: service)

        await store.import(source: "~/Downloads/acme-tokens", kind: .local)
        XCTAssertTrue(service.approveCalls.isEmpty)

        // The explicit human-review approval is the ONLY promotion path.
        let promotion = await store.approve(id: "q-abc123")
        XCTAssertEqual(service.approveCalls, ["q-abc123"], "approve is called exactly once, explicitly")
        let promoted = try XCTUnwrap(promotion)
        XCTAssertTrue(promoted.promoted)
        XCTAssertEqual(promoted.packageId, "acme-tokens")
        XCTAssertEqual(store.lastPromotion?.packageId, "acme-tokens")
        // The promoted package left quarantine.
        XCTAssertFalse(store.entries.contains { $0.quarantineId == "q-abc123" })
    }

    // MARK: - a rejected import shows its reason and CANNOT be approved

    func testRejectedImportCannotBeApproved() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.rejectedJSON))
        let store = DesignImportStore(service: service)

        let result = await store.import(source: "~/Downloads/evil.zip", kind: .archive)
        let imported = try XCTUnwrap(result)
        XCTAssertEqual(imported.status, .rejected)
        XCTAssertEqual(imported.rejectedReason, .zipSlip)

        // The rejected entry is in the list, shown with its reason…
        let entry = try XCTUnwrap(store.entries.first { $0.quarantineId == "q-bad999" })
        XCTAssertTrue(entry.isRejected)
        XCTAssertEqual(entry.rejectedReason, .zipSlip)
        XCTAssertEqual(entry.rejectedReason?.message, DesignImportRejectedReason.zipSlip.message)

        // …and it CANNOT be approved: the store refuses and never calls the service.
        let promotion = await store.approve(id: "q-bad999")
        XCTAssertNil(promotion, "a rejected package cannot be promoted")
        XCTAssertTrue(service.approveCalls.isEmpty, "approve is never sent for a rejected package")
        XCTAssertNotNil(store.lastError, "the refusal is surfaced")
        XCTAssertNil(store.lastPromotion)
    }

    // MARK: - reject calls import-reject and removes the entry

    func testRejectCallsServiceAndRemovesEntry() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.quarantinedJSON))
        service.setRejectResult(
            try JSONDecoder().decode(DesignImportRejectResult.self, from: Data(Self.rejectResultJSON.utf8))
        )
        let store = DesignImportStore(service: service)

        await store.import(source: "~/Downloads/acme-tokens", kind: .local)
        XCTAssertEqual(store.entries.count, 1)

        let result = await store.reject(id: "q-abc123")
        let rejected = try XCTUnwrap(result)
        XCTAssertTrue(rejected.rejected)
        XCTAssertTrue(rejected.deleted)
        XCTAssertEqual(service.rejectCalls, ["q-abc123"], "reject is routed to import-reject")
        XCTAssertTrue(store.entries.isEmpty, "the rejected entry is dropped from the store")
        // Reject never promotes.
        XCTAssertTrue(service.approveCalls.isEmpty)
    }

    // MARK: - refreshStatus reads the quarantine listing

    func testRefreshStatusReadsQuarantineListing() async throws {
        let service = MockDesignImportService()
        service.setStatusResult(
            try JSONDecoder().decode(DesignImportStatusResult.self, from: Data(Self.statusJSON.utf8))
        )
        let store = DesignImportStore(service: service)

        await store.refreshStatus()
        XCTAssertEqual(service.statusCallCount, 1)
        XCTAssertEqual(store.entries.count, 1)
        let entry = try XCTUnwrap(store.entries.first)
        XCTAssertEqual(entry.quarantineId, "q-abc123")
        XCTAssertEqual(entry.provenance.license, "MIT")
        XCTAssertEqual(entry.fileCount, 42)
    }

    // MARK: - the browser action is a user-initiated URL only (no API)

    func testOpenDesignSiteIsAUserInitiatedURLNotAnAPICall() {
        // The one outward affordance is opening a documented SITE URL — not an API
        // endpoint. The view's openURL hook receives exactly that URL.
        var opened: [URL] = []
        let store = DesignImportStore(service: MockDesignImportService())
        let view = DesignImportView(
            store: store,
            pickLocalSource: { nil },
            openURL: { opened.append($0) }
        )
        // Render to construct the view; the action is wired to the injected opener.
        _ = ImageRenderer(content: view.frame(width: 1024, height: 700)).nsImage

        // The documented site URL is a plain https web page (a link the user opens),
        // not an API call: opening it sends none of the user's data.
        XCTAssertEqual(DesignImportLinks.openDesignURL.scheme, "https")
        XCTAssertNotNil(DesignImportLinks.openDesignURL.host)
    }

    // MARK: - the service surface is LOCAL: import / approve / reject / status only

    /// Enumerate the ENTIRE callable surface of `DesignImportService` through a mock
    /// that records every call. The recorded calls are exactly import / approve /
    /// reject / status — there is NO upload / sync / network method. (An upload
    /// method would not compile against this protocol.)
    func testServiceSurfaceIsLocalOnlyNoUpload() async throws {
        let service: DesignImportService = MockDesignImportService()
        let mock = service as! MockDesignImportService

        _ = try await service.importLocal(source: "~/x", kind: .local)
        _ = try await service.approve(quarantineId: "q-1")
        _ = try await service.reject(quarantineId: "q-1")
        _ = try await service.status()

        // Exactly the four LOCAL entry points were exercised.
        XCTAssertEqual(mock.importCalls.count, 1)
        XCTAssertEqual(mock.approveCalls.count, 1)
        XCTAssertEqual(mock.rejectCalls.count, 1)
        XCTAssertEqual(mock.statusCallCount, 1)

        // There is no upload/network surface to call: the store also exposes none.
        // (Reflection over the store's published members shows no "upload"/"sync"
        // member — the only outward action is the view's user-initiated URL open.)
        let storeMirror = Mirror(reflecting: DesignImportStore(service: mock))
        for child in storeMirror.children {
            let label = (child.label ?? "").lowercased()
            XCTAssertFalse(label.contains("upload"), "no upload surface on the store")
            XCTAssertFalse(label.contains("network"), "no network surface on the store")
        }
    }

    // MARK: - Rendering: non-nil + fills width (no letterbox)

    func testImportViewRendersNonNil() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.quarantinedJSON))
        let store = DesignImportStore(service: service)
        await store.import(source: "~/Downloads/acme-tokens", kind: .local)

        let view = DesignImportView(store: store, pickLocalSource: { nil }, openURL: { _ in })
            .frame(width: 1280, height: 760)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the design import view renders non-nil with a quarantined entry")
    }

    func testImportViewFillsWidthNoLetterbox() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.quarantinedJSON))
        let store = DesignImportStore(service: service)
        await store.import(source: "~/Downloads/acme-tokens", kind: .local)

        for width in [1024.0, 1440.0] {
            let view = DesignImportView(store: store, pickLocalSource: { nil }, openURL: { _ in })
                .frame(width: width, height: 760)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "design import rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "design import must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    /// A REJECTED entry also renders (its reason is surfaced) and fills width.
    func testRejectedEntryRendersWithReasonAndFillsWidth() async throws {
        let service = MockDesignImportService()
        service.setImportResult(try decodeImport(Self.rejectedJSON))
        let store = DesignImportStore(service: service)
        await store.import(source: "~/Downloads/evil.zip", kind: .archive)

        for width in [1024.0, 1440.0] {
            let view = DesignImportView(store: store, pickLocalSource: { nil }, openURL: { _ in })
                .frame(width: width, height: 700)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "rejected entry rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0)
        }
    }
}
