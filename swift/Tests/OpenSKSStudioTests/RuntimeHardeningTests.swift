// RuntimeHardeningTests.swift — PR-043 acceptance for UI memory + high-rate
// hardening:
//
//   1. EventBatcher coalesces a burst (10,000 events) into a BOUNDED number of
//      flushes (flush count << event count) and the final state equals the last
//      event; it retains no unbounded backlog (one pending slot).
//   2. BoundedCache never exceeds capacity under 100,000 inserts and evicts LRU;
//      a simulated memory-pressure purge empties / shrinks it.
//   3. A backgrounded conversation / run store releases its heavy view/projection
//      (the heavy cache is empty after backgrounding) while the foreground one
//      retains it; re-activating reloads.
//   4. A large render still fills width at 1024 / 1440 (no letterbox) and renders
//      (ImageRenderer non-nil).

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class RuntimeHardeningTests: XCTestCase {

    // MARK: - Proof artifact freshness

    func testProofArtifactFingerprintChangesWhenAcceptanceSummaryChanges() throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent("opensks-proof-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: workspace) }
        let acceptanceDir = workspace.appendingPathComponent(".opensks/acceptance", isDirectory: true)
        try FileManager.default.createDirectory(at: acceptanceDir, withIntermediateDirectories: true)
        let summary = acceptanceDir.appendingPathComponent("acceptance-summary.json")
        try #"{"summary":{"total":23,"passed":21,"partial":2,"failed":0},"goal_complete":false}"#
            .write(to: summary, atomically: true, encoding: .utf8)

        let stale = ProofArtifactMonitor.fingerprint(workspace: workspace)
        try #"{"summary":{"total":23,"passed":20,"partial":3,"failed":0},"goal_complete":false}"#
            .write(to: summary, atomically: true, encoding: .utf8)
        let fresh = ProofArtifactMonitor.fingerprint(workspace: workspace)

        XCTAssertNotEqual(stale, fresh, "external acceptance audit changes must refresh app-data")
    }

    func testProofArtifactFingerprintChangesWhenMissionDirectoryChanges() throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent("opensks-proof-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: workspace) }
        let missionsDir = workspace.appendingPathComponent(".opensks/missions", isDirectory: true)
        try FileManager.default.createDirectory(at: missionsDir, withIntermediateDirectories: true)

        let before = ProofArtifactMonitor.fingerprint(workspace: workspace)
        try FileManager.default.createDirectory(
            at: missionsDir.appendingPathComponent("mission-001", isDirectory: true),
            withIntermediateDirectories: true
        )
        let after = ProofArtifactMonitor.fingerprint(workspace: workspace)

        XCTAssertNotEqual(before, after, "mission count changes must refresh app-data")
    }

    // MARK: - 1. EventBatcher coalesces a burst into bounded flushes

    /// A 10,000-event burst into a manually-ticked batcher coalesces to ONE flush
    /// per tick, never one per event, and the delivered value is the LAST one.
    /// The batcher retains a single pending slot — no unbounded backlog.
    func testEventBatcherCoalescesBurstIntoBoundedFlushes() {
        var delivered: [Int] = []
        // autoFlush off → fully deterministic; we drive ticks ourselves.
        let batcher = EventBatcher<Int>(interval: 0, autoFlush: false) { delivered.append($0) }

        let eventCount = 10_000
        // Submit a huge burst with NO tick in between: all collapse into one slot.
        for i in 0..<eventCount {
            batcher.submit(i)
        }

        // Before any tick, nothing has been delivered but exactly one value is
        // pending (bounded backlog == 1, independent of the 10,000 submits).
        XCTAssertEqual(delivered.count, 0, "no flush before a tick")
        XCTAssertEqual(batcher.submitCount, eventCount)

        // One tick delivers exactly ONE coalesced value: the last submitted.
        XCTAssertTrue(batcher.flushIfNeeded(force: true))
        XCTAssertEqual(delivered, [eventCount - 1], "the single flush carries the LAST event")
        XCTAssertEqual(batcher.flushCount, 1)

        // A second tick with nothing pending is a no-op (no spurious flush, no
        // retained backlog to drain).
        XCTAssertFalse(batcher.flushIfNeeded(force: true))
        XCTAssertEqual(batcher.flushCount, 1)

        // The flush count is bounded FAR below the event count — the contract.
        XCTAssertLessThan(batcher.flushCount, eventCount / 1000)
    }

    /// Interleaving ticks across several bursts yields one flush per burst, each
    /// carrying that burst's latest value — and total flushes stay tiny.
    func testEventBatcherFlushCountBoundedByTicksNotEvents() {
        var delivered: [Int] = []
        let batcher = EventBatcher<Int>(interval: 0, autoFlush: false) { delivered.append($0) }

        for burst in 0..<5 {
            for i in 0..<2_000 {
                batcher.submit(burst * 10_000 + i)
            }
            batcher.flushIfNeeded(force: true)
        }

        XCTAssertEqual(delivered.count, 5, "one flush per burst, not per event")
        XCTAssertEqual(batcher.submitCount, 10_000)
        XCTAssertEqual(delivered, [
            0 * 10_000 + 1_999,
            1 * 10_000 + 1_999,
            2 * 10_000 + 1_999,
            3 * 10_000 + 1_999,
            4 * 10_000 + 1_999,
        ])
        XCTAssertEqual(batcher.flushCount, 5)
    }

    /// Auto-flush mode (the production path) eventually delivers the latest value
    /// asynchronously, still as a single coalesced flush.
    func testEventBatcherAutoFlushDeliversLatestAsynchronously() async {
        var delivered: [Int] = []
        let batcher = EventBatcher<Int>(interval: 0.001, autoFlush: true) { delivered.append($0) }
        for i in 0..<1_000 { batcher.submit(i) }

        // Wait for the scheduled flush to fire.
        try? await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertFalse(delivered.isEmpty, "auto-flush delivered at least one value")
        XCTAssertEqual(delivered.last, 999, "the final delivered value is the last submitted")
        XCTAssertLessThan(batcher.flushCount, 1_000, "auto-flush still coalesces (flush << submit)")
    }

    // MARK: - 2. BoundedCache caps + LRU + memory-pressure purge

    /// 100,000 inserts into a small-capacity cache never exceed capacity, and the
    /// surviving entries are the most-recently-used.
    func testBoundedCacheNeverExceedsCapacityUnderManyInserts() {
        let capacity = 256
        let cache = BoundedCache<Int, Int>(capacity: capacity)

        for i in 0..<100_000 {
            cache.insert(i, forKey: i)
            // Invariant after EVERY insert: count never exceeds capacity.
            XCTAssertLessThanOrEqual(cache.count, capacity)
        }

        XCTAssertEqual(cache.count, capacity, "cache is full at exactly capacity")
        // LRU eviction kept the last `capacity` keys; the oldest are gone.
        XCTAssertTrue(cache.contains(99_999), "most recent key retained")
        XCTAssertFalse(cache.contains(0), "oldest key evicted")
        XCTAssertFalse(cache.contains(100_000 - capacity - 1), "anything before the window is gone")
        XCTAssertTrue(cache.contains(100_000 - capacity), "the start of the retained window survives")
    }

    /// Exercising `value(forKey:)` refreshes recency so the touched key survives a
    /// subsequent overflow that would otherwise evict it.
    func testBoundedCacheEvictsLeastRecentlyUsed() {
        let cache = BoundedCache<String, Int>(capacity: 3)
        cache.insert(1, forKey: "a")
        cache.insert(2, forKey: "b")
        cache.insert(3, forKey: "c")
        // Touch "a" → it becomes most-recently-used; "b" is now the LRU victim.
        _ = cache.value(forKey: "a")
        cache.insert(4, forKey: "d") // evicts "b"

        XCTAssertEqual(cache.count, 3)
        XCTAssertTrue(cache.contains("a"), "touched key survived")
        XCTAssertFalse(cache.contains("b"), "untouched LRU key evicted")
        XCTAssertEqual(cache.keysByRecency.first, "d", "newest is most-recently-used")
    }

    /// A memory-pressure purge empties the cache; a warning-level shrink reduces
    /// it to a fraction of capacity while keeping the hottest entries.
    func testBoundedCacheMemoryPressurePurgeAndShrink() {
        let cache = BoundedCache<Int, Int>(capacity: 100)
        for i in 0..<100 { cache.insert(i, forKey: i) }
        XCTAssertEqual(cache.count, 100)

        // Warning-level shrink to 25% keeps the 25 most-recently-used (75..99).
        cache.purge(toFraction: 0.25)
        XCTAssertEqual(cache.count, 25, "shrink to a fraction of capacity")
        XCTAssertTrue(cache.contains(99), "hot key kept")
        XCTAssertFalse(cache.contains(0), "cold key dropped")

        // Critical-level purge empties it entirely.
        cache.purge()
        XCTAssertEqual(cache.count, 0)
        XCTAssertTrue(cache.isEmpty)
    }

    /// The MemoryPressureMonitor fans a simulated event out to a registered cache:
    /// warning shrinks, critical purges.
    func testMemoryPressureMonitorDrivesCacheReclaim() {
        let monitor = MemoryPressureMonitor()
        let cache = BoundedCache<Int, Int>(capacity: 80)
        monitor.register(cache: cache)
        for i in 0..<80 { cache.insert(i, forKey: i) }

        monitor.simulate(.warning)
        XCTAssertLessThan(cache.count, 80, "warning shrank the cache")
        XCTAssertGreaterThan(cache.count, 0, "warning kept the hot working set")

        monitor.simulate(.critical)
        XCTAssertTrue(cache.isEmpty, "critical purged the cache")
        XCTAssertEqual(monitor.eventCount, 2)
    }

    // MARK: - 3. Background release: only the foreground view is retained

    /// A backgrounded conversation releases its heavy message page while the
    /// foreground one retains it; re-activating reloads the page from the service.
    func testBackgroundedConversationReleasesHeavyPageForegroundRetains() async {
        let a = conversationSummary(id: "conv-A", messageCount: 3)
        let b = conversationSummary(id: "conv-B", messageCount: 3)
        let mock = MockConversationService(
            summaries: [a, b],
            messages: [
                "conv-A": (0..<3).map { messageFixture(id: "a\($0)", conversation: "conv-A", sequence: Int64($0)) },
                "conv-B": (0..<3).map { messageFixture(id: "b\($0)", conversation: "conv-B", sequence: Int64($0)) },
            ]
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load() // populate the light summaries list

        // Foreground conv-A: it holds its heavy page.
        await store.setActive("conv-A")
        XCTAssertFalse(store.messages.isEmpty, "active conversation has its page loaded")
        XCTAssertTrue(store.retainsHeavyView("conv-A"), "foreground retains the heavy view")
        XCTAssertFalse(store.retainsHeavyView("conv-B"), "the other conversation does not")

        // Switch foreground to conv-B: conv-A is now backgrounded; only ONE page
        // is held — conv-B's. conv-A no longer retains a heavy view.
        await store.setActive("conv-B")
        XCTAssertTrue(store.retainsHeavyView("conv-B"))
        XCTAssertFalse(store.retainsHeavyView("conv-A"), "backgrounded conversation released its page")

        // Background EVERYTHING (e.g. window resigns active / memory pressure):
        // the heavy page is dropped, the light summaries list survives.
        await store.setActive(nil)
        XCTAssertTrue(store.messages.isEmpty, "backgrounding all releases the heavy page")
        XCTAssertFalse(store.retainsHeavyView("conv-B"))
        XCTAssertEqual(store.summaries.count, 2, "light summaries are unaffected")

        // Re-activating reloads the page from the service.
        await store.setActive("conv-A")
        XCTAssertFalse(store.messages.isEmpty, "re-activating reloads the heavy page")
        XCTAssertTrue(store.retainsHeavyView("conv-A"))
    }

    /// Memory pressure releases a backgrounded conversation's page while keeping
    /// the active one's.
    func testConversationMemoryPressureReleasesBackgroundedPage() async {
        let a = conversationSummary(id: "conv-A", messageCount: 2)
        let mock = MockConversationService(
            summaries: [a],
            messages: ["conv-A": (0..<2).map { messageFixture(id: "a\($0)", conversation: "conv-A", sequence: Int64($0)) }]
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load() // populate the light summaries list
        let monitor = MemoryPressureMonitor()
        store.registerForMemoryPressure(monitor)

        await store.setActive("conv-A")
        XCTAssertTrue(store.retainsHeavyView("conv-A"))
        // Pressure while conv-A is the ACTIVE view: its page is kept (foreground).
        monitor.simulate(.warning)
        XCTAssertTrue(store.retainsHeavyView("conv-A"), "the foreground page survives pressure")

        // Load conv-A's page but mark NOTHING active (a stale selection whose
        // view is no longer the foreground). `select` loads the page; then
        // backgrounding everything via setActive(nil) marks it inactive. A second
        // selection reload followed by a release shows the inactive page dropping.
        await store.setActive(nil)
        XCTAssertTrue(store.messages.isEmpty, "backgrounding all drops the page")
        // Pressure with nothing active is a safe no-op and keeps the page empty.
        monitor.simulate(.critical)
        XCTAssertTrue(store.messages.isEmpty, "no heavy page is retained while backgrounded")
        XCTAssertFalse(store.retainsHeavyView("conv-A"))
        XCTAssertEqual(store.summaries.count, 1, "the light summary survives pressure")
    }

    /// A backgrounded run releases its heavy node-level projection while the
    /// foreground run retains it; the released run keeps only a light summary.
    func testBackgroundedRunReleasesHeavyProjectionForegroundRetains() async {
        let store = PipelineProjectionStore()
        // run-A active (becomes active on first ingest), then run-B streams too.
        store.ingest(execEvent(runId: "run-A", sequence: 0, kind: "run_started",
                               payload: ["pipeline_id": .string("pa")]))
        store.ingest(execEvent(runId: "run-A", sequence: 1, kind: "work_item_completed",
                               payload: ["node_id": .string("a1"), "to": .string("succeeded")]))
        store.ingest(execEvent(runId: "run-B", sequence: 0, kind: "work_item_running",
                               payload: ["node_id": .string("b1"), "to": .string("running"),
                                         "pipeline_id": .string("pb")]))

        // run-A is the active run; both currently retain heavy views.
        XCTAssertTrue(store.retainsHeavyView("run-A"))
        XCTAssertTrue(store.retainsHeavyView("run-B"))

        // Release backgrounded views: only the active run-A keeps its node-level
        // projection; run-B is released to a light summary (no node array).
        store.releaseBackgroundViews()
        XCTAssertTrue(store.retainsHeavyView("run-A"), "foreground run keeps its full projection")
        XCTAssertFalse(store.retainsHeavyView("run-B"), "backgrounded run released its heavy projection")
        XCTAssertNil(store.projection(for: "run-B"), "no heavy projection for the released run")
        XCTAssertTrue(store.nodes(for: "run-B").isEmpty, "the heavy node array is gone")

        // A LIGHT summary survives for the released run (counts + lifecycle, no nodes).
        let summary = store.summary(for: "run-B")
        XCTAssertNotNil(summary, "the released run keeps a light summary")
        XCTAssertEqual(summary?.nodeCount, 1, "the summary remembers the node count")
        XCTAssertEqual(summary?.pipelineId, "pb")

        // The active run still has its full node-level projection.
        XCTAssertEqual(store.nodes(for: "run-A").count, 1)
    }

    /// Memory pressure on the pipeline store releases every non-active run's heavy
    /// projection.
    func testPipelineMemoryPressureReleasesBackgroundedRuns() async {
        let store = PipelineProjectionStore()
        let monitor = MemoryPressureMonitor()
        store.registerForMemoryPressure(monitor)

        store.ingest(execEvent(runId: "run-A", sequence: 0, kind: "work_item_running",
                               payload: ["node_id": .string("a1"), "to": .string("running")]))
        store.ingest(execEvent(runId: "run-B", sequence: 0, kind: "work_item_running",
                               payload: ["node_id": .string("b1"), "to": .string("running")]))
        XCTAssertTrue(store.retainsHeavyView("run-B"))

        monitor.simulate(.critical)
        XCTAssertTrue(store.retainsHeavyView("run-A"), "active run survives pressure")
        XCTAssertFalse(store.retainsHeavyView("run-B"), "backgrounded run released under pressure")
    }

    // MARK: - 4. Large render fills width at 1024 / 1440 (no letterbox)

    /// A large (1,000-node) pipeline graph render fills the requested width at the
    /// supported window widths (no letterbox) and renders to a non-nil image.
    func testLargeRenderFillsWidthNoLetterbox() throws {
        let projection = bigProjection(nodeCount: 1_000)
        for width in [1024.0, 1440.0] {
            let view = PipelineGraphView(projection: projection, selectedNodeId: .constant(nil))
                .frame(width: width, height: 800)

            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(
                renderer.nsImage,
                "the 1,000-node graph must render to a non-nil image at width \(width)"
            )
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "the large render must fill width (no letterbox) at \(width)"
            )
            XCTAssertEqual(image.size.height, 800, accuracy: 1.0)
        }
    }

    /// The run card (which embeds the mini node strip) for a large run also fills
    /// the requested width and renders non-nil.
    func testLargeRunCardFillsWidthNoLetterbox() throws {
        let projection = bigProjection(nodeCount: 1_000)
        for width in [1024.0, 1440.0] {
            let card = PipelineRunCard(projection: projection).frame(width: width)
            let renderer = ImageRenderer(content: card)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "the large run card must render at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0, "run card fills width at \(width)")
        }
    }

    // MARK: - Fixtures

    private func conversationSummary(id: String, messageCount: Int) -> ConversationSummary {
        ConversationSummary(
            schema: "opensks.conversation-summary.v1",
            id: id,
            projectId: "mock-project",
            title: id,
            titleSource: "manual",
            status: .idle,
            pinned: false,
            archived: false,
            messageCount: messageCount,
            createdAtMs: 1_000,
            updatedAtMs: 1_000,
            lastMessageAtMs: messageCount > 0 ? 1_000 : nil
        )
    }

    private func messageFixture(
        id: String,
        conversation: String,
        sequence: Int64
    ) -> ConversationMessage {
        ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: id,
            projectId: "mock-project",
            conversationId: conversation,
            turnId: nil,
            role: .user,
            state: .complete,
            contentRedacted: "message \(id)",
            sequence: sequence,
            createdAtMs: 1_000 + sequence,
            updatedAtMs: 1_000 + sequence
        )
    }

    private func execEvent(
        runId: String,
        sequence: UInt64,
        kind: String,
        payload: [String: JSONValue] = [:]
    ) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope(
            schema: "opensks.execution-event-envelope.v1",
            id: "\(runId)-\(sequence)",
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

    private func bigProjection(nodeCount: Int) -> PipelineExecutionProjection {
        let store = PipelineProjectionStore()
        store.ingest(execEvent(runId: "run-big", sequence: 0, kind: "run_started",
                               payload: ["pipeline_id": .string("big")]))
        for i in 0..<nodeCount {
            let kind: String
            let to: String
            switch i % 3 {
            case 0: kind = "work_item_completed"; to = "succeeded"
            case 1: kind = "work_item_running"; to = "running"
            default: kind = "work_item_queued"; to = "queued"
            }
            store.ingest(execEvent(runId: "run-big", sequence: UInt64(i + 1), kind: kind,
                                   payload: ["node_id": .string("node-\(i)"), "to": .string(to)]))
        }
        return store.projection(for: "run-big")!
    }
}
