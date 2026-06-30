// DesignStudioTests.swift — the Design Studio (PR-040).
//
// Drives DesignStudioModels / DesignStudioService / DesignStudioStore /
// DesignStudioView through a FakeDesignStudioService (no disk, no process) that
// scripts audit / activate / active-status / revision results. Asserts the PR-040
// acceptance:
//   • a FAILING audit (passed:false / blocks_activation:true) BLOCKS activation:
//     activate() does NOT change the shown active package, surfaces the
//     `activationBlock`, and leaves the previously active package in place;
//   • a PASSING audit allows activation and the active status updates predictably;
//   • a revision: propose returns a revision exposing its proof_ref; accept / reject
//     / rollback transition the shown revision state;
//   • the Tokens editor lists token paths/values (selectedTokens /
//     tokenDraftsByPackage);
//   • the Design Studio view + the native component STATE MATRIX render
//     (ImageRenderer non-nil) and fill width at 1024 / 1440 (no letterbox).
//
// Audit reports are decoded from JSON strings that match the shared CLI contract
// (`opensks.design-audit.v1`, snake_case) so the tests exercise the real decoding
// path, not hand-built values.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

/// `XCTUnwrap` takes a non-async autoclosure, so `try unwrapAsync(await …)` does not
/// compile. This by-value wrapper evaluates the awaited value first, then unwraps.
private func unwrapAsync<T>(
    _ value: T?,
    _ message: @autoclosure () -> String = "",
    file: StaticString = #filePath,
    line: UInt = #line
) throws -> T {
    try XCTUnwrap(value, message(), file: file, line: line)
}

// MARK: - Fake service (test-local; never touches disk or spawns a process)

/// An in-memory `DesignStudioService` for these tests. It scripts audit reports per
/// package and models the CLI's ATOMIC activation: a FAILING/blocking audit throws
/// `.auditFailed` (so the store keeps the previous active package and surfaces the
/// block), a PASSING audit activates and updates the active status. Revision steps
/// transition state and stay proof-linked. Call counts make the invariants
/// observable.
final class FakeDesignStudioService: DesignStudioService, @unchecked Sendable {
    private let lock = NSLock()

    private var auditByPackage: [String: DesignAuditReport] = [:]
    private var active = DesignActiveStatus.none
    private var cannedRevisions: [String: DesignRevision] = [:]

    private(set) var auditCalls: [String] = []
    private(set) var activateCalls: [String] = []
    private(set) var activeStatusCallCount = 0
    private(set) var proposeCalls: [String] = []
    private(set) var acceptCalls: [String] = []
    private(set) var rejectCalls: [String] = []
    private(set) var rollbackCalls: [String] = []
    private(set) var saveCalls: [(packageId: String, tokens: [DesignTokenEntry])] = []
    private(set) var compileCalls: [String] = []
    private(set) var listCallCount = 0
    private var registryPackages: [DesignPackageListEntry] = []

    init() {}

    private func withServiceLock<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try body()
    }

    // Test setup

    func setAudit(_ report: DesignAuditReport, forPackage packageId: String) {
        withServiceLock {
            auditByPackage[packageId] = report
        }
    }

    func setActive(_ status: DesignActiveStatus) {
        withServiceLock {
            active = status
        }
    }

    func setProposedRevision(_ revision: DesignRevision, forPackage packageId: String) {
        withServiceLock {
            cannedRevisions["propose|\(packageId)"] = revision
        }
    }

    // DesignStudioService

    func audit(packageId: String) async throws -> DesignAuditReport {
        withServiceLock {
            auditCalls.append(packageId)
            return scriptedAudit(packageId)
        }
    }

    func activate(packageId: String) async throws -> DesignActivateResult {
        try withServiceLock {
            activateCalls.append(packageId)
            // ATOMIC: audit gates the activation. A failing/blocking audit throws and
            // does NOT change the active package — exactly the CLI's behaviour.
            let report = scriptedAudit(packageId)
            if !report.passed && report.blocksActivation {
                throw DesignStudioServiceError.auditFailed(findings: report.findings)
            }
            let previous = active.activePackage
            active = DesignActiveStatus(activePackage: packageId, activatedRevision: active.activatedRevision)
            return DesignActivateResult(
                activated: true,
                packageId: packageId,
                previousActive: previous,
                auditPassed: true
            )
        }
    }

    func activeStatus() async throws -> DesignActiveStatus {
        withServiceLock {
            activeStatusCallCount += 1
            return active
        }
    }

    func proposeRevision(packageId: String) async throws -> DesignRevision {
        withServiceLock {
            proposeCalls.append(packageId)
            if let canned = cannedRevisions["propose|\(packageId)"] { return canned }
            return DesignRevision(
                revisionId: "rev-\(proposeCalls.count)",
                packageId: packageId,
                state: .proposed,
                proofRef: "proof://\(packageId)/rev-\(proposeCalls.count)"
            )
        }
    }

    func acceptRevision(revisionId: String) async throws -> DesignRevision {
        transition(revisionId: revisionId, to: .accepted) { self.acceptCalls.append($0) }
    }

    func rejectRevision(revisionId: String) async throws -> DesignRevision {
        transition(revisionId: revisionId, to: .rejected) { self.rejectCalls.append($0) }
    }

    func rollbackRevision(revisionId: String) async throws -> DesignRevision {
        transition(revisionId: revisionId, to: .rolledBack) { self.rollbackCalls.append($0) }
    }

    func setRegistryPackages(_ packages: [DesignPackageListEntry]) {
        withServiceLock {
            registryPackages = packages
        }
    }

    func saveTokens(packageId: String, tokens: [DesignTokenEntry]) async throws -> DesignSaveResult {
        withServiceLock {
            saveCalls.append((packageId: packageId, tokens: tokens))
            return DesignSaveResult(
                packageId: packageId,
                updated: tokens.count,
                unknownPaths: [],
                total: tokens.count,
                contentHash: "fake"
            )
        }
    }

    func compile(packageId: String) async throws -> DesignCompileResult {
        withServiceLock {
            compileCalls.append(packageId)
            return DesignCompileResult(packageId: packageId, ok: true, swiftBytes: 256, error: nil)
        }
    }

    func listPackages() async throws -> [DesignPackageListEntry] {
        withServiceLock {
            listCallCount += 1
            return registryPackages
        }
    }

    private func transition(
        revisionId: String,
        to state: DesignRevisionState,
        record: (String) -> Void
    ) -> DesignRevision {
        withServiceLock {
            record(revisionId)
            return DesignRevision(
                revisionId: revisionId,
                packageId: "",
                state: state,
                proofRef: "proof://\(revisionId)/\(state.rawValue)"
            )
        }
    }

    private func scriptedAudit(_ packageId: String) -> DesignAuditReport {
        auditByPackage[packageId] ?? DesignAuditReport(
            packageId: packageId,
            passed: true,
            blocksActivation: false,
            findings: []
        )
    }
}

@MainActor
final class DesignStudioTests: XCTestCase {

    // MARK: - Fixtures

    /// A FAILING audit that BLOCKS activation (an `error` finding), decoded from the
    /// shared CLI contract JSON so the decode path is exercised.
    private static let failingAuditJSON = """
    {
      "schema": "opensks.design-audit.v1",
      "package_id": "tokens-bad",
      "passed": false,
      "blocks_activation": true,
      "findings": [
        {"kind": "contrast", "severity": "error", "detail": "Text on surface fails AA contrast", "ref": "color.text.muted"},
        {"kind": "hit_target", "severity": "warning", "detail": "Control smaller than 44pt", "ref": "size.control.min"}
      ]
    }
    """

    /// A PASSING audit (no blocking findings).
    private static let passingAuditJSON = """
    {
      "schema": "opensks.design-audit.v1",
      "package_id": "tokens-good",
      "passed": true,
      "blocks_activation": false,
      "findings": []
    }
    """

    private func decodeAudit(_ json: String) throws -> DesignAuditReport {
        try JSONDecoder().decode(DesignAuditReport.self, from: Data(json.utf8))
    }

    private func makeCatalog() -> [DesignPackage] {
        [
            DesignPackage(
                packageId: "tokens-good",
                title: "Good Tokens",
                tokens: [
                    DesignTokenEntry(path: "color.text.default", value: "#E6E6E6"),
                    DesignTokenEntry(path: "color.accent", value: "#7AA2F7"),
                    DesignTokenEntry(path: "radius.control", value: "8")
                ]
            ),
            DesignPackage(
                packageId: "tokens-bad",
                title: "Bad Tokens",
                tokens: [
                    DesignTokenEntry(path: "color.text.muted", value: "#3A3A3A")
                ]
            )
        ]
    }

    private func makeStore(
        active: DesignActiveStatus = .none,
        configure: (FakeDesignStudioService) -> Void = { _ in }
    ) -> (DesignStudioStore, FakeDesignStudioService) {
        let service = FakeDesignStudioService()
        service.setActive(active)
        configure(service)
        let store = DesignStudioStore(service: service, catalog: makeCatalog())
        return (store, service)
    }

    // MARK: - Contract decoding (audit report)

    func testFailingAuditReportDecodesFromContractJSON() throws {
        let report = try decodeAudit(Self.failingAuditJSON)
        XCTAssertEqual(report.schema, "opensks.design-audit.v1")
        XCTAssertEqual(report.packageId, "tokens-bad")
        XCTAssertFalse(report.passed)
        XCTAssertTrue(report.blocksActivation)
        XCTAssertEqual(report.findings.count, 2)
        XCTAssertEqual(report.errors.count, 1)
        XCTAssertEqual(report.warnings.count, 1)
        let error = try XCTUnwrap(report.errors.first)
        XCTAssertEqual(error.kind, .contrast)
        XCTAssertEqual(error.severity, .error)
        XCTAssertEqual(error.ref, "color.text.muted")
    }

    // MARK: - ACCEPTANCE 1: a FAILING audit BLOCKS activation

    /// A failing/blocking audit BLOCKS activation: `activate` returns nil, the SHOWN
    /// active package is unchanged (the previously active package remains), and the
    /// blocked state is surfaced via `activationBlock`.
    func testFailingAuditBlocksActivationAndKeepsPreviousActive() async throws {
        let previouslyActive = DesignActiveStatus(activePackage: "tokens-good", activatedRevision: "rev-prev")
        let (store, service) = makeStore(active: previouslyActive) { service in
            service.setAudit(try! self.decodeAudit(Self.failingAuditJSON), forPackage: "tokens-bad")
        }
        // Confirm the starting active package the store will be defending.
        await store.refreshActiveStatus()
        XCTAssertTrue(store.isActive("tokens-good"))

        let result = await store.activate(package: "tokens-bad")

        // The activation did NOT happen.
        XCTAssertNil(result, "a blocked activation returns no success result")
        XCTAssertTrue(store.isActive("tokens-good"), "the previously active package remains active")
        XCTAssertFalse(store.isActive("tokens-bad"), "the failing package did NOT become active")
        XCTAssertEqual(store.active.activePackage, "tokens-good")

        // The blocked state is surfaced, naming the kept active package + findings.
        let block = try XCTUnwrap(store.activationBlock, "the blocked activation is surfaced")
        XCTAssertEqual(block.blockedPackageId, "tokens-bad")
        XCTAssertEqual(block.keptActivePackageId, "tokens-good")
        XCTAssertEqual(block.errors.count, 1, "the blocking error finding is carried")

        // The failing audit is recorded so the Audit tab can show it.
        let recorded = try XCTUnwrap(store.auditByPackage["tokens-bad"])
        XCTAssertFalse(recorded.passed)
        XCTAssertTrue(recorded.blocksActivation)

        // The service WAS asked to activate (the gate ran on the CLI side).
        XCTAssertEqual(service.activateCalls, ["tokens-bad"])
    }

    /// With NOTHING active, a blocked activation still leaves the active package as
    /// none — it never fabricates an activation.
    func testFailingAuditBlocksActivationWhenNothingActive() async throws {
        let (store, _) = makeStore { service in
            service.setAudit(try! self.decodeAudit(Self.failingAuditJSON), forPackage: "tokens-bad")
        }
        let result = await store.activate(package: "tokens-bad")
        XCTAssertNil(result)
        XCTAssertNil(store.active.activePackage, "no package becomes active on a block")
        let block = try XCTUnwrap(store.activationBlock)
        XCTAssertEqual(block.keptActivePackageId, nil)
        XCTAssertEqual(block.keptActiveDisplay, "None")
    }

    // MARK: - ACCEPTANCE 2: a PASSING audit ALLOWS activation

    /// A passing audit allows activation and the active status updates predictably:
    /// the activated package becomes the shown active one and no block is surfaced.
    func testPassingAuditAllowsActivationAndUpdatesActiveStatus() async throws {
        let (store, service) = makeStore { service in
            service.setAudit(try! self.decodeAudit(Self.passingAuditJSON), forPackage: "tokens-good")
        }
        XCTAssertNil(store.active.activePackage, "nothing active to start")

        let result = await store.activate(package: "tokens-good")

        let activated = try XCTUnwrap(result, "a passing audit yields a success result")
        XCTAssertTrue(activated.activated)
        XCTAssertEqual(activated.packageId, "tokens-good")
        XCTAssertTrue(activated.auditPassed)

        XCTAssertTrue(store.isActive("tokens-good"), "the package is now the shown active one")
        XCTAssertEqual(store.active.activePackage, "tokens-good")
        XCTAssertNil(store.activationBlock, "a successful activation surfaces no block")
        XCTAssertEqual(service.activateCalls, ["tokens-good"])
        // active-status is re-read to confirm the new active package.
        XCTAssertGreaterThanOrEqual(service.activeStatusCallCount, 1)
    }

    /// A passing activation reports the package that was previously active as
    /// `previousActive`, then the new one becomes active.
    func testPassingActivationReportsPreviousActive() async throws {
        let (store, _) = makeStore(active: DesignActiveStatus(activePackage: "tokens-good")) { service in
            service.setAudit(try! self.decodeAudit(Self.passingAuditJSON), forPackage: "tokens-good")
        }
        await store.refreshActiveStatus()
        // Activating the (already clean) "tokens-bad" with no scripted audit defaults
        // to a clean PASS, so it activates and supersedes "tokens-good".
        let result = await store.activate(package: "tokens-bad")
        let activated = try XCTUnwrap(result)
        XCTAssertEqual(activated.previousActive, "tokens-good", "the prior active package is reported")
        XCTAssertTrue(store.isActive("tokens-bad"))
        XCTAssertFalse(store.isActive("tokens-good"))
    }

    // MARK: - ACCEPTANCE 3: revision lifecycle (proof-linked)

    /// `proposeRevision` returns a `proposed` revision LINKED TO A PROOF; it lands at
    /// the front of the package's revision list with its proof_ref exposed.
    func testProposeRevisionReturnsProofLinkedRevision() async throws {
        let (store, _) = makeStore { service in
            service.setProposedRevision(
                DesignRevision(
                    revisionId: "rev-100",
                    packageId: "tokens-good",
                    state: .proposed,
                    proofRef: "proof://tokens-good/rev-100"
                ),
                forPackage: "tokens-good"
            )
        }
        let revision = try unwrapAsync(await store.proposeRevision(package: "tokens-good"))
        XCTAssertEqual(revision.revisionId, "rev-100")
        XCTAssertEqual(revision.state, .proposed)
        XCTAssertTrue(revision.state.isPending)
        XCTAssertEqual(revision.proofRef, "proof://tokens-good/rev-100")
        XCTAssertEqual(revision.proofRefDisplay, "proof://tokens-good/rev-100")

        let listed = store.revisionsByPackage["tokens-good"] ?? []
        XCTAssertEqual(listed.first?.revisionId, "rev-100", "the proposed revision is listed")
    }

    /// accept transitions the shown revision state proposed → accepted (and keeps the
    /// proof link); the revision is updated in place, not duplicated.
    func testAcceptRevisionTransitionsShownState() async throws {
        let (store, _) = makeStore()
        let proposed = try unwrapAsync(await store.proposeRevision(package: "tokens-good"))
        XCTAssertEqual(proposed.state, .proposed)

        let accepted = try unwrapAsync(await store.acceptRevision(proposed.revisionId, package: "tokens-good"))
        XCTAssertEqual(accepted.state, .accepted)
        XCTAssertEqual(accepted.revisionId, proposed.revisionId, "the same revision transitioned")
        XCTAssertNotNil(accepted.proofRef, "an accepted revision is still proof-linked")

        let listed = store.revisionsByPackage["tokens-good"] ?? []
        XCTAssertEqual(listed.count, 1, "the revision is updated in place, not duplicated")
        XCTAssertEqual(listed.first?.state, .accepted)
        XCTAssertEqual(listed.first?.packageId, "tokens-good", "stays grouped under its package")
    }

    /// reject transitions the shown revision state proposed → rejected.
    func testRejectRevisionTransitionsShownState() async throws {
        let (store, _) = makeStore()
        let proposed = try unwrapAsync(await store.proposeRevision(package: "tokens-good"))
        let rejected = try unwrapAsync(await store.rejectRevision(proposed.revisionId, package: "tokens-good"))
        XCTAssertEqual(rejected.state, .rejected)
        XCTAssertEqual(store.revisionsByPackage["tokens-good"]?.first?.state, .rejected)
    }

    /// rollback transitions an accepted revision → rolled_back.
    func testRollbackRevisionTransitionsShownState() async throws {
        let (store, _) = makeStore()
        let proposed = try unwrapAsync(await store.proposeRevision(package: "tokens-good"))
        _ = try unwrapAsync(await store.acceptRevision(proposed.revisionId, package: "tokens-good"))
        let rolledBack = try unwrapAsync(await store.rollbackRevision(proposed.revisionId, package: "tokens-good"))
        XCTAssertEqual(rolledBack.state, .rolledBack)
        XCTAssertEqual(store.revisionsByPackage["tokens-good"]?.first?.state, .rolledBack)
    }

    // MARK: - ACCEPTANCE 4: the token editor surface lists token paths/values

    /// The Tokens tab surface lists the selected package's token paths/values, seeded
    /// from the catalog into `tokenDraftsByPackage` and surfaced via `selectedTokens`.
    func testTokenEditorListsPathsAndValues() throws {
        let (store, _) = makeStore()
        store.select("tokens-good")

        let tokens = store.selectedTokens
        XCTAssertEqual(tokens.map(\.path), ["color.text.default", "color.accent", "radius.control"])
        XCTAssertEqual(tokens.first?.value, "#E6E6E6")

        // The same list is available per-package via the draft map.
        let drafts = try XCTUnwrap(store.tokenDraftsByPackage["tokens-good"])
        XCTAssertEqual(drafts.count, 3)
        // A colour-valued token is detected so the editor can show a swatch.
        XCTAssertTrue(try XCTUnwrap(drafts.first { $0.path == "color.accent" }).isColor)
        XCTAssertFalse(try XCTUnwrap(drafts.first { $0.path == "radius.control" }).isColor)
    }

    /// Editing a token value updates the draft surface (the editor is live), without
    /// disturbing other tokens.
    func testEditingTokenValueUpdatesDraft() throws {
        let (store, _) = makeStore()
        store.select("tokens-good")
        store.setTokenValue("#000000", forPath: "color.accent", package: "tokens-good")

        let edited = try XCTUnwrap(store.selectedTokens.first { $0.path == "color.accent" })
        XCTAssertEqual(edited.value, "#000000")
        // Untouched token is unchanged.
        XCTAssertEqual(store.selectedTokens.first { $0.path == "color.text.default" }?.value, "#E6E6E6")
    }

    // MARK: - ACCEPTANCE 5: rendering — non-nil + fills width (no letterbox)

    /// The Design Studio view renders non-nil with a selected package, a recorded
    /// audit, and a proposed revision in view.
    func testDesignStudioViewRendersNonNil() async throws {
        let (store, _) = makeStore { service in
            service.setAudit(try! self.decodeAudit(Self.failingAuditJSON), forPackage: "tokens-bad")
        }
        store.select("tokens-bad")
        await store.audit(package: "tokens-bad")
        _ = await store.proposeRevision(package: "tokens-bad")

        let view = DesignStudioView(store: store).frame(width: 1280, height: 760)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the design studio renders non-nil")
    }

    /// The Design Studio view fills the requested width at 1024 and 1440 (no
    /// letterbox: rendered width == requested width).
    func testDesignStudioViewFillsWidthNoLetterbox() async throws {
        let (store, _) = makeStore { service in
            service.setAudit(try! self.decodeAudit(Self.failingAuditJSON), forPackage: "tokens-bad")
        }
        store.select("tokens-bad")
        await store.audit(package: "tokens-bad")

        for width in [1024.0, 1440.0] {
            let view = DesignStudioView(store: store).frame(width: width, height: 760)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "design studio rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "design studio must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    /// The native component STATE MATRIX renders non-nil and fills width at 1024 and
    /// 1440 (no letterbox) — the matrix is the PR-040 native component preview.
    func testComponentStateMatrixRendersAndFillsWidth() throws {
        for width in [1024.0, 1440.0] {
            let view = ComponentStateMatrixView().frame(width: width, height: 600)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "component state matrix rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "component state matrix must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    /// The component state matrix covers every interaction state (default / hover /
    /// pressed / disabled / focused) so the matrix is complete.
    func testComponentStateMatrixCoversAllStates() {
        XCTAssertEqual(
            DesignControlState.allCases.map(\.rawValue),
            ["defaultState", "hover", "pressed", "disabled", "focused"]
        )
        XCTAssertEqual(DesignControlState.allCases.map(\.label),
                       ["Default", "Hover", "Pressed", "Disabled", "Focused"])
    }

    // MARK: - PR-056: token draft persistence (DESIGN-002) + registry catalog (DESIGN-101)

    /// Editing a token marks the package's draft dirty (drives the Save button) and
    /// invalidates any prior compile status.
    func testEditingTokenMarksDraftDirty() {
        let (store, _) = makeStore()
        store.select("tokens-good")
        XCTAssertFalse(store.isSelectedDirty)
        store.setTokenValue("#000000", forPath: "color.accent", package: "tokens-good")
        XCTAssertTrue(store.isSelectedDirty)
        XCTAssertTrue(store.dirtyPackages.contains("tokens-good"))
        // A no-op edit (same value) does NOT re-dirty after a save would clear it.
    }

    /// Saving a draft calls `save-tokens` with the edited tokens, records the receipt
    /// in `lastSave`, and clears the dirty flag (DESIGN-002).
    func testSaveDraftPersistsViaServiceAndClearsDirty() async throws {
        let (store, service) = makeStore()
        store.select("tokens-good")
        store.setTokenValue("#123456", forPath: "color.accent", package: "tokens-good")
        XCTAssertTrue(store.isSelectedDirty)

        let result = await store.saveDraft(package: "tokens-good")

        XCTAssertEqual(service.saveCalls.count, 1)
        XCTAssertEqual(service.saveCalls.first?.packageId, "tokens-good")
        // The edited value was sent to the service.
        XCTAssertEqual(
            service.saveCalls.first?.tokens.first { $0.path == "color.accent" }?.value,
            "#123456"
        )
        XCTAssertEqual(result?.packageId, "tokens-good")
        XCTAssertEqual(store.lastSave?.packageId, "tokens-good")
        XCTAssertFalse(store.isSelectedDirty, "a successful save clears the dirty flag")
    }

    /// Compiling stores the result for the selected package so the editor can surface
    /// compile success/errors without activating.
    func testCompileStoresResultForSelectedPackage() async throws {
        let (store, service) = makeStore()
        store.select("tokens-good")

        let result = await store.compile(package: "tokens-good")

        XCTAssertEqual(service.compileCalls, ["tokens-good"])
        XCTAssertEqual(result?.ok, true)
        XCTAssertEqual(store.selectedCompile?.ok, true)
        XCTAssertEqual(store.compileByPackage["tokens-good"]?.swiftBytes, 256)
    }

    /// Loading the registry-driven catalog (DESIGN-101) replaces the package set from
    /// `design list`, marks the active package, and preserves tokens already known for
    /// a package so the Tokens tab keeps content.
    func testLoadRegistryCatalogReplacesCatalogFromRegistry() async throws {
        let (store, service) = makeStore()
        service.setRegistryPackages([
            DesignPackageListEntry(packageId: "tokens-good", title: "Good (registry)", active: true),
            DesignPackageListEntry(packageId: "on-disk-only", title: "On Disk Only", active: false),
        ])

        await store.loadRegistryCatalog()

        XCTAssertEqual(service.listCallCount, 1)
        XCTAssertEqual(store.catalog.map(\.packageId), ["tokens-good", "on-disk-only"])
        // Registry title wins; tokens already known for tokens-good are preserved.
        let good = try XCTUnwrap(store.catalog.first { $0.packageId == "tokens-good" })
        XCTAssertEqual(good.title, "Good (registry)")
        XCTAssertFalse(good.tokens.isEmpty, "seeded tokens are preserved across a registry refresh")
        XCTAssertEqual(store.active.activePackage, "tokens-good")
    }
}
