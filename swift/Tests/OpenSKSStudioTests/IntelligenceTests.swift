// IntelligenceTests.swift — Project Intelligence + Freshness UX (PR-041).
//
// Drives IntelligenceModels / IntelligenceService / IntelligenceStore /
// IntelligenceView through a MockIntelligenceService (no disk, no process) that
// scripts freshness / freshness-check / code-graph pages / glossary / architecture.
// Asserts the PR-041 acceptance:
//   • after the mock reports a DIVERGED current stamp (worktree_changed), the
//     store's badge flips to STALE and the previously-loaded data is NOT presented
//     as current/fresh (stale is never shown as current);
//   • a MATCHING stamp shows FRESH;
//   • the code-graph explorer PAGES: requesting a page asks for limit/offset and
//     shows total (the store never requests the whole graph); a large fixture
//     (total 5000, page 100) renders at a fixed size;
//   • a deep link from a record resolves to the expected conversation / run / file
//     target id;
//   • the Intelligence view + the code-graph explorer render (ImageRenderer
//     non-nil) and fill width at 1024 / 1440 (no letterbox).

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class IntelligenceTests: XCTestCase {

    // MARK: - Fixtures

    private func stamp(head: String? = "head0", worktree: String = "wt0", index: String = "idx0") -> IntelFreshnessStamp {
        IntelFreshnessStamp(headHash: head, worktreeHash: worktree, indexHash: index, inRepo: true)
    }

    private func architecture(stamp: IntelFreshnessStamp, refs: [String] = []) -> IntelArchitecture {
        IntelArchitecture(
            schema: "opensks.intel-architecture.v1",
            records: [
                IntelArchitectureRecord(
                    id: "arch-1",
                    title: "Engine boundary",
                    detail: "The deterministic engine is the only writer of proofs.",
                    refs: refs
                )
            ],
            freshness: stamp
        )
    }

    private func bigCorpus(_ count: Int) -> [IntelCodeGraphRecord] {
        (0..<count).map { i in
            IntelCodeGraphRecord(
                path: "Sources/File\(i % 50).swift",
                symbol: "symbol_\(i)",
                kind: i % 3 == 0 ? "type" : (i % 3 == 1 ? "func" : "var"),
                line: i + 1
            )
        }
    }

    // MARK: - Contract decoding

    /// The freshness-check contract decodes from the shared snake_case JSON, and a
    /// `worktree_changed` reason maps to the typed enum (not "fresh").
    func testFreshnessCheckDecodesFromContractJSON() throws {
        let json = """
        {
          "schema": "opensks.intel-freshness-check.v1",
          "fresh": false,
          "stale_reason": "worktree_changed",
          "current": {"head_hash": "abc", "worktree_hash": "WT2", "index_hash": "idx0", "in_repo": true}
        }
        """
        let check = try JSONDecoder.opensks.decode(IntelFreshnessCheck.self, from: Data(json.utf8))
        XCTAssertFalse(check.fresh)
        XCTAssertEqual(check.staleReason, .worktreeChanged)
        XCTAssertEqual(check.current.worktreeHash, "WT2")
        // The badge derived from a non-fresh check is STALE — never fresh.
        let badge = IntelFreshnessBadge(check: check)
        XCTAssertFalse(badge.isFresh)
        if case .stale(let reason) = badge { XCTAssertEqual(reason, .worktreeChanged) } else { XCTFail("must be stale") }
    }

    /// The paged code-graph contract decodes, including total/limit/offset and the
    /// embedded freshness stamp.
    func testCodeGraphPageDecodesFromContractJSON() throws {
        let json = """
        {
          "schema": "opensks.intel-codegraph.v1",
          "total": 5000,
          "limit": 100,
          "offset": 0,
          "records": [{"path": "a.swift", "symbol": "Foo", "kind": "type", "line": 12}],
          "freshness": {"head_hash": "h", "worktree_hash": "w", "index_hash": "i", "in_repo": true}
        }
        """
        let page = try JSONDecoder.opensks.decode(IntelCodeGraphPage.self, from: Data(json.utf8))
        XCTAssertEqual(page.total, 5000)
        XCTAssertEqual(page.limit, 100)
        XCTAssertEqual(page.records.first?.symbol, "Foo")
        XCTAssertEqual(page.freshness.worktreeHash, "w")
    }

    /// A missing / unknown stale reason is NEVER decoded as fresh — an unknown
    /// reason is treated as stale.
    func testUnknownStaleReasonIsStaleNotFresh() throws {
        let json = """
        {"schema":"opensks.intel-freshness-check.v1","fresh":false,"stale_reason":"meteor_strike",
         "current":{"head_hash":null,"worktree_hash":"w","index_hash":"i","in_repo":false}}
        """
        let check = try JSONDecoder.opensks.decode(IntelFreshnessCheck.self, from: Data(json.utf8))
        XCTAssertEqual(check.staleReason, .unknown)
        XCTAssertFalse(IntelFreshnessBadge(check: check).isFresh, "an unknown reason is never fresh")
    }

    // MARK: - ACCEPTANCE 1: a diverged current flips the badge to STALE

    /// After data is loaded FRESH, moving the current worktree hash and re-checking
    /// flips the architecture badge to STALE(worktree_changed). The previously-loaded
    /// records are still present but are NOT presented as current/fresh.
    func testDivergedCurrentFlipsBadgeToStaleAndNeverShowsAsCurrent() async throws {
        let loadedAt = stamp(worktree: "wt0")
        let service = MockIntelligenceService(current: loadedAt)
        service.setArchitecture(architecture(stamp: loadedAt))
        let store = IntelligenceStore(service: service)

        // Load — at load time the section is FRESH (just read against current).
        await store.loadArchitecture()
        XCTAssertTrue(store.architectureBadge.isFresh, "freshly loaded against current")
        XCTAssertEqual(store.architecture.count, 1)

        // The workspace moves: the working tree changes under the loaded data.
        service.setCurrent(stamp(worktree: "wt-MOVED"))

        // The watcher re-checks the loaded-at stamp against the new current.
        await store.recheckFreshness()

        // The badge flipped to STALE with the right reason…
        XCTAssertFalse(store.architectureBadge.isFresh, "must NOT be fresh after divergence")
        guard case .stale(let reason) = store.architectureBadge else {
            return XCTFail("badge must be stale after divergence")
        }
        XCTAssertEqual(reason, .worktreeChanged)
        XCTAssertEqual(store.architectureBadge.label, "Stale · Working tree changed")
        // …and the previously-loaded data is still there but never labelled current.
        XCTAssertEqual(store.architecture.count, 1, "loaded records remain visible")
        XCTAssertNotEqual(store.architectureBadge.label.lowercased(), "fresh")
        XCTAssertFalse(store.architectureBadge.label.lowercased().contains("current"))
        // A freshness-check WAS performed against the loaded-at stamp.
        XCTAssertEqual(service.freshnessCheckCalls.last?.worktreeHash, "wt0")
    }

    // MARK: - ACCEPTANCE 2: a matching stamp shows FRESH

    /// When current still matches the loaded-at stamp, the re-check keeps the badge
    /// FRESH.
    func testMatchingStampShowsFresh() async throws {
        let loadedAt = stamp(worktree: "wt0", index: "idx0")
        let service = MockIntelligenceService(current: loadedAt)
        service.setArchitecture(architecture(stamp: loadedAt))
        service.setGlossary(IntelGlossary(
            schema: "opensks.intel-glossary.v1",
            terms: [IntelGlossaryTerm(term: "Proof", definition: "A sealed claim", refs: [])],
            freshness: loadedAt
        ))
        let store = IntelligenceStore(service: service)

        await store.loadArchitecture()
        await store.loadGlossary()
        // Current is unchanged → re-check returns fresh.
        await store.recheckFreshness()

        XCTAssertTrue(store.architectureBadge.isFresh)
        XCTAssertEqual(store.architectureBadge.label, "Fresh")
        XCTAssertTrue(store.glossaryBadge.isFresh)
    }

    /// A section that was never loaded (no stamp) re-checks to STALE — it is never
    /// shown as current.
    func testNeverLoadedSectionIsStale() async throws {
        let service = MockIntelligenceService(current: stamp())
        let store = IntelligenceStore(service: service)
        // No load — code graph stamp is nil.
        await store.recheckFreshness()
        XCTAssertFalse(store.codeGraphBadge.isFresh, "an unloaded section is never fresh")
    }

    // MARK: - ACCEPTANCE 3: the code-graph explorer PAGES

    /// Requesting a page asks the service for (limit, offset) and surfaces `total`
    /// without ever requesting the whole graph: a 5000-symbol corpus yields a
    /// 100-record page, total 5000, and the page request used limit 100 / offset 0.
    func testCodeGraphExplorerPagesAndNeverLoadsWholeGraph() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setCodeGraphCorpus(bigCorpus(5000))
        let store = IntelligenceStore(service: service, codeGraphLimit: 100)

        await store.loadCodeGraphPage(offset: 0)

        XCTAssertEqual(store.codeGraphTotal, 5000, "total reflects the whole graph")
        XCTAssertEqual(store.codeGraphRecords.count, 100, "only one page is held in memory")
        XCTAssertEqual(store.codeGraphOffset, 0)
        XCTAssertEqual(store.codeGraphPageCount, 50)
        XCTAssertEqual(store.codeGraphPageIndex, 1)
        // The store asked for a bounded window, NOT the entire graph.
        let request = try XCTUnwrap(service.codeGraphPageRequests.first)
        XCTAssertEqual(request.limit, 100)
        XCTAssertEqual(request.offset, 0)
        XCTAssertLessThan(store.codeGraphRecords.count, store.codeGraphTotal, "never the whole graph")

        // Paging forward asks for the NEXT window (offset 100), still one page.
        await store.nextCodeGraphPage()
        XCTAssertEqual(store.codeGraphOffset, 100)
        XCTAssertEqual(store.codeGraphRecords.count, 100)
        XCTAssertEqual(service.codeGraphPageRequests.last?.offset, 100)
        // Every request stayed bounded to the page limit.
        XCTAssertTrue(service.codeGraphPageRequests.allSatisfy { $0.limit == 100 })
    }

    /// A large fixture renders the explorer at a FIXED size (ImageRenderer non-nil)
    /// — proving the paged page draws regardless of the 5000-symbol corpus size.
    func testCodeGraphExplorerRendersLargeFixtureAtFixedSize() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setCodeGraphCorpus(bigCorpus(5000))
        let store = IntelligenceStore(service: service, codeGraphLimit: 100)
        await store.loadCodeGraphPage(offset: 0)

        let explorer = CodeGraphExplorer(records: store.codeGraphRecords)
            .frame(width: 800, height: 320)
        let renderer = ImageRenderer(content: explorer)
        renderer.scale = 1
        let image = try XCTUnwrap(renderer.nsImage, "code graph explorer renders a large page")
        XCTAssertEqual(image.size.width, 800, accuracy: 1.0)
        XCTAssertEqual(image.size.height, 320, accuracy: 1.0)
    }

    // MARK: - ACCEPTANCE 4: deep links resolve to the expected target id

    /// A record whose first ref is a conversation resolves to a `.conversation`
    /// target with the conversation id.
    func testDeepLinkResolvesConversationTarget() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setArchitecture(architecture(stamp: stamp(), refs: ["conversation:conv-77", "Sources/x.swift"]))
        let store = IntelligenceStore(service: service)
        await store.loadArchitecture()

        let target = try XCTUnwrap(store.deepLinkTarget(forRecord: "arch-1"))
        XCTAssertEqual(target, .conversation(id: "conv-77"))
        XCTAssertEqual(target.targetId, "conv-77")
    }

    /// A record whose first ref is a run resolves to a `.run` target with the run id.
    func testDeepLinkResolvesRunTarget() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setArchitecture(architecture(stamp: stamp(), refs: ["run:run-42"]))
        let store = IntelligenceStore(service: service)
        await store.loadArchitecture()

        let target = try XCTUnwrap(store.deepLinkTarget(forRecord: "arch-1"))
        XCTAssertEqual(target, .run(id: "run-42"))
        XCTAssertEqual(target.targetId, "run-42")
    }

    /// A record whose first ref is a bare path resolves to a `.file` target; a
    /// code-graph result always resolves to its source file.
    func testDeepLinkResolvesFileTargets() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setArchitecture(architecture(stamp: stamp(), refs: ["Sources/Engine.swift"]))
        service.setCodeGraphCorpus([
            IntelCodeGraphRecord(path: "Sources/Graph.swift", symbol: "Foo", kind: "type", line: 9)
        ])
        let store = IntelligenceStore(service: service)
        await store.loadArchitecture()
        await store.loadCodeGraphPage(offset: 0)

        let recordTarget = try XCTUnwrap(store.deepLinkTarget(forRecord: "arch-1"))
        XCTAssertEqual(recordTarget, .file(path: "Sources/Engine.swift", line: nil))
        XCTAssertEqual(recordTarget.targetId, "Sources/Engine.swift")

        let codeRecord = try XCTUnwrap(store.codeGraphRecords.first)
        let codeTarget = store.deepLinkTarget(forCodeGraph: codeRecord)
        XCTAssertEqual(codeTarget, .file(path: "Sources/Graph.swift", line: 9))
        XCTAssertEqual(codeTarget.targetId, "Sources/Graph.swift")
    }

    /// A record with no refs yields no deep link (no fabricated target).
    func testDeepLinkAbsentWhenNoRefs() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setArchitecture(architecture(stamp: stamp(), refs: []))
        let store = IntelligenceStore(service: service)
        await store.loadArchitecture()
        XCTAssertNil(store.deepLinkTarget(forRecord: "arch-1"))
    }

    // MARK: - ACCEPTANCE 5: rendering — non-nil + fills width (no letterbox)

    /// The Intelligence view renders non-nil with architecture, a code-graph page,
    /// and a glossary in view.
    func testIntelligenceViewRendersNonNil() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setArchitecture(architecture(stamp: stamp(), refs: ["run:run-1"]))
        service.setGlossary(IntelGlossary(
            schema: "opensks.intel-glossary.v1",
            terms: [IntelGlossaryTerm(term: "Lease", definition: "A claim on work", refs: ["Sources/Lease.swift"])],
            freshness: stamp()
        ))
        service.setCodeGraphCorpus(bigCorpus(5000))
        let store = IntelligenceStore(service: service, codeGraphLimit: 100)
        await store.loadAll()

        let view = IntelligenceView(store: store).frame(width: 1280, height: 800)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the intelligence view renders non-nil")
    }

    /// The Intelligence view fills the requested width at 1024 and 1440 (no
    /// letterbox: rendered width == requested width).
    func testIntelligenceViewFillsWidthNoLetterbox() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setArchitecture(architecture(stamp: stamp(), refs: ["Sources/x.swift"]))
        service.setCodeGraphCorpus(bigCorpus(5000))
        let store = IntelligenceStore(service: service, codeGraphLimit: 100)
        await store.loadAll()

        for width in [1024.0, 1440.0] {
            let view = IntelligenceView(store: store).frame(width: width, height: 800)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "intelligence rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "intelligence must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    /// The code-graph explorer fills the requested width at 1024 and 1440 (no
    /// letterbox).
    func testCodeGraphExplorerFillsWidthNoLetterbox() async throws {
        let service = MockIntelligenceService(current: stamp())
        service.setCodeGraphCorpus(bigCorpus(5000))
        let store = IntelligenceStore(service: service, codeGraphLimit: 100)
        await store.loadCodeGraphPage(offset: 0)

        for width in [1024.0, 1440.0] {
            let explorer = CodeGraphExplorer(records: store.codeGraphRecords).frame(width: width, height: 320)
            let renderer = ImageRenderer(content: explorer)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "code graph explorer rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "code graph explorer must fill the requested width (no letterbox) at \(width)"
            )
        }
    }
}
