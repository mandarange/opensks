import XCTest
@testable import OpenSKSStudio

/// PR-029 acceptance for the node-level pipeline projection reducer/store.
///
///  - rebuild == incremental (folding all events == applying one-by-one)
///  - a snapshot does NOT erase terminal/meaningful node or run state
///  - duplicate / older sequence is ignored (dedup)
///  - an unknown event kind never crashes and leaves the projection consistent
@MainActor
final class PipelineProjectionTests: XCTestCase {

    // MARK: - Event builders (snake_case wire shape, decoded like the app)

    private func event(
        id: String,
        runId: String = "run-1",
        sequence: UInt64,
        kind: String,
        payload: [String: JSONValue] = [:],
        sensitivity: String = "public"
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
            sensitivity: EventSensitivity(rawValue: sensitivity),
            evidenceRefs: []
        )
    }

    /// A representative run: start → node queued → running → completed →
    /// verification passes → a snapshot is written last.
    private func sampleEvents(runId: String = "run-1") -> [ExecutionEventEnvelope] {
        [
            event(id: "e1", runId: runId, sequence: 1, kind: "run_started",
                  payload: ["message": .string("started")]),
            event(id: "e2", runId: runId, sequence: 2, kind: "work_item_queued",
                  payload: ["node_id": .string("node-a"), "to": .string("queued"),
                            "provider_ref": .string("anthropic"), "model_ref": .string("opus")]),
            event(id: "e3", runId: runId, sequence: 3, kind: "work_item_running",
                  payload: ["node_id": .string("node-a"), "to": .string("running"),
                            "message": .string("working"),
                            "touched_paths": .array([.string("src/a.swift")])]),
            event(id: "e4", runId: runId, sequence: 4, kind: "work_item_completed",
                  payload: ["node_id": .string("node-a"), "to": .string("succeeded"),
                            "message": .string("done"),
                            "touched_paths": .array([.string("src/b.swift")])]),
            event(id: "e5", runId: runId, sequence: 5, kind: "verification_passed",
                  payload: ["node_id": .string("node-a")]),
            event(id: "e6", runId: runId, sequence: 6, kind: "snapshot_written",
                  payload: ["message": .string("snapshot written")]),
        ]
    }

    // MARK: - rebuild == incremental

    func testRebuildEqualsIncremental() {
        let events = sampleEvents()

        let batch = PipelineProjectionStore()
        batch.rebuild(from: events)

        let live = PipelineProjectionStore()
        for e in events { live.ingest(e) }

        let batchProjection = batch.projection(for: "run-1")
        let liveProjection = live.projection(for: "run-1")

        XCTAssertNotNil(batchProjection)
        XCTAssertEqual(batchProjection, liveProjection,
                       "folding all events at once must equal applying them one-by-one")
    }

    func testRebuildEqualsIncrementalAcrossMultipleRuns() {
        let events = sampleEvents(runId: "run-1") + sampleEvents(runId: "run-2")

        let batch = PipelineProjectionStore()
        batch.rebuild(from: events)

        let live = PipelineProjectionStore()
        for e in events { live.ingest(e) }

        for runId in ["run-1", "run-2"] {
            XCTAssertEqual(batch.projection(for: runId), live.projection(for: runId),
                           "rebuild must equal live for \(runId)")
        }
    }

    func testReducerIsPureAcrossShuffledInOrderGroups() {
        // Same per-run sequence order, interleaved across two runs differently:
        // the projection per run is identical regardless of interleaving.
        let a = sampleEvents(runId: "run-1")
        let b = sampleEvents(runId: "run-2")

        let interleaved = zip(a, b).flatMap { [$0.0, $0.1] }
        let separated = a + b

        let s1 = PipelineProjectionStore()
        for e in interleaved { s1.ingest(e) }
        let s2 = PipelineProjectionStore()
        for e in separated { s2.ingest(e) }

        XCTAssertEqual(s1.projection(for: "run-1"), s2.projection(for: "run-1"))
        XCTAssertEqual(s1.projection(for: "run-2"), s2.projection(for: "run-2"))
    }

    // MARK: - snapshot does not erase terminal

    func testSnapshotDoesNotEraseTerminalNode() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "work_item_running",
                           payload: ["node_id": .string("node-a"), "to": .string("running")]))
        store.ingest(event(id: "e2", sequence: 2, kind: "work_item_completed",
                           payload: ["node_id": .string("node-a"), "to": .string("succeeded"),
                                     "message": .string("done")]))

        // A snapshot with NO node state (lower information) arrives afterwards.
        store.ingest(event(id: "e3", sequence: 3, kind: "snapshot_written",
                           payload: ["message": .string("snapshot written")]))

        let node = store.nodes(for: "run-1").first
        XCTAssertEqual(node?.state, .succeeded, "a succeeded node stays succeeded after a snapshot")
        XCTAssertEqual(node?.lastPublicMessage, "done",
                       "a terminal node keeps its final message, not the generic snapshot text")
    }

    func testSnapshotDoesNotDowngradeTerminalRunOrWriteLiteralSnapshot() {
        // This is the exact bug fix: a snapshot must NOT set run state to the
        // literal "snapshot", and must not downgrade a finished run.
        let store = PipelineProjectionStore()
        for e in sampleEvents() { store.ingest(e) }

        let projection = store.projection(for: "run-1")
        XCTAssertNotNil(projection)
        XCTAssertNotEqual(projection?.state.rawValue, "snapshot",
                          "run state must never be the literal 'snapshot'")
        // run_started + verification_passed advanced the run to running; the
        // trailing snapshot left it untouched.
        XCTAssertEqual(projection?.state, .running)
    }

    func testSnapshotWithLowerStateCannotDowngradeFailedNode() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "verification_failed",
                           payload: ["node_id": .string("node-a")]))
        // A snapshot tries to reassert "running" — must be ignored (downgrade).
        store.ingest(event(id: "e2", sequence: 2, kind: "snapshot_written",
                           payload: ["node_id": .string("node-a"), "state": .string("running")]))

        XCTAssertEqual(store.nodes(for: "run-1").first?.state, .failed,
                       "a failed node cannot be downgraded to running by a snapshot")
    }

    func testTerminalNodePreservesProvenanceAcrossSnapshot() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "work_item_completed",
                           payload: ["node_id": .string("node-a"), "to": .string("succeeded"),
                                     "provider_ref": .string("anthropic"),
                                     "model_ref": .string("opus"),
                                     "touched_paths": .array([.string("x")])]))
        store.ingest(event(id: "e2", sequence: 2, kind: "snapshot_written",
                           payload: ["node_id": .string("node-a")]))

        let node = store.nodes(for: "run-1").first
        XCTAssertEqual(node?.providerRef, "anthropic")
        XCTAssertEqual(node?.modelRef, "opus")
        XCTAssertEqual(node?.touchedPaths, ["x"], "touched paths survive a later snapshot")
    }

    // MARK: - dedup

    func testDuplicateSequenceIsIgnored() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "work_item_running",
                           payload: ["node_id": .string("node-a"), "to": .string("running")]))
        store.ingest(event(id: "e2", sequence: 2, kind: "work_item_completed",
                           payload: ["node_id": .string("node-a"), "to": .string("succeeded")]))

        // Replay an OLD sequence that would (if applied) downgrade the node.
        store.ingest(event(id: "e1-dup", sequence: 1, kind: "work_item_running",
                           payload: ["node_id": .string("node-a"), "to": .string("running")]))

        XCTAssertEqual(store.latestSequence(for: "run-1"), 2)
        XCTAssertEqual(store.nodes(for: "run-1").first?.state, .succeeded,
                       "an older/duplicate sequence is ignored and cannot downgrade the node")
    }

    func testExactDuplicateSequenceIsIgnored() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e2", sequence: 2, kind: "work_item_completed",
                           payload: ["node_id": .string("node-a"), "to": .string("succeeded")]))
        // Same sequence again — ignored.
        store.ingest(event(id: "e2-again", sequence: 2, kind: "work_item_queued",
                           payload: ["node_id": .string("node-b"), "to": .string("queued")]))

        XCTAssertEqual(store.nodes(for: "run-1").count, 1,
                       "an event at an already-seen sequence is ignored entirely")
        XCTAssertEqual(store.nodes(for: "run-1").first?.nodeId, "node-a")
    }

    func testSequenceZeroIsAcceptedAsFirstEvent() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e0", sequence: 0, kind: "run_started",
                           payload: ["message": .string("started")]))
        XCTAssertEqual(store.latestSequence(for: "run-1"), 0)
        XCTAssertEqual(store.projection(for: "run-1")?.state, .running)
    }

    // MARK: - unknown event kind

    func testUnknownEventKindDoesNotCrashAndStaysConsistent() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "work_item_running",
                           payload: ["node_id": .string("node-a"), "to": .string("running")]))
        // A future/unknown kind — must be folded harmlessly.
        store.ingest(event(id: "e2", sequence: 2, kind: "future_scheduler_signal",
                           payload: ["node_id": .string("node-a"), "message": .string("noise")]))
        store.ingest(event(id: "e3", sequence: 3, kind: "work_item_completed",
                           payload: ["node_id": .string("node-a"), "to": .string("succeeded")]))

        let node = store.nodes(for: "run-1").first
        XCTAssertEqual(node?.state, .succeeded,
                       "an unknown kind in the middle does not corrupt the fold")
        XCTAssertEqual(store.latestSequence(for: "run-1"), 3)
    }

    func testUnknownEventWithUnknownStateValueIsLenient() {
        // A snapshot carrying an unrecognized state string must not crash and
        // must not downgrade — unknown decodes to the lowest rank and the
        // monotonic raise rejects it.
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "work_item_completed",
                           payload: ["node_id": .string("node-a"), "to": .string("succeeded")]))
        store.ingest(event(id: "e2", sequence: 2, kind: "snapshot_written",
                           payload: ["node_id": .string("node-a"),
                                     "state": .string("totally_new_state")]))

        XCTAssertEqual(store.nodes(for: "run-1").first?.state, .succeeded)
    }

    func testRebuildWithUnknownKindEqualsIncremental() {
        let events = [
            event(id: "e1", sequence: 1, kind: "run_started",
                  payload: ["message": .string("started")]),
            event(id: "e2", sequence: 2, kind: "future_scheduler_signal",
                  payload: ["node_id": .string("node-a"), "message": .string("noise")]),
            event(id: "e3", sequence: 3, kind: "work_item_completed",
                  payload: ["node_id": .string("node-a"), "to": .string("succeeded")]),
        ]
        let batch = PipelineProjectionStore()
        batch.rebuild(from: events)
        let live = PipelineProjectionStore()
        for e in events { live.ingest(e) }

        XCTAssertEqual(batch.projection(for: "run-1"), live.projection(for: "run-1"))
    }

    // MARK: - metrics

    func testMetricsReflectNodeStates() {
        let store = PipelineProjectionStore()
        store.ingest(event(id: "e1", sequence: 1, kind: "work_item_queued",
                           payload: ["node_id": .string("a"), "to": .string("queued")]))
        store.ingest(event(id: "e2", sequence: 2, kind: "work_item_running",
                           payload: ["node_id": .string("b"), "to": .string("running")]))
        store.ingest(event(id: "e3", sequence: 3, kind: "work_item_completed",
                           payload: ["node_id": .string("c"), "to": .string("succeeded")]))
        store.ingest(event(id: "e4", sequence: 4, kind: "verification_failed",
                           payload: ["node_id": .string("d")]))

        let m = store.metrics(for: "run-1")
        XCTAssertEqual(m.queued, 1)
        XCTAssertEqual(m.active, 1)
        XCTAssertEqual(m.completed, 1)
        XCTAssertEqual(m.failed, 1)
    }

    // MARK: - JSON decoding round-trip (snake_case wire shape)

    func testProjectionDecodesSnakeCaseWireShape() throws {
        let json = """
        {
          "schema": "opensks.pipeline-execution-projection.v1",
          "projection_version": 1,
          "run_id": "run-x",
          "conversation_id": "conv-x",
          "pipeline_id": "pipe-x",
          "state": "running",
          "nodes": [
            {
              "node_id": "node-a",
              "state": "waiting_for_approval",
              "provider_ref": "anthropic",
              "model_ref": "opus",
              "attempt": 2,
              "touched_paths": ["src/a.swift"],
              "last_public_message": "awaiting approval"
            }
          ],
          "metrics": {"completed": 0, "active": 1, "queued": 0, "failed": 0}
        }
        """.data(using: .utf8)!

        let projection = try JSONDecoder.opensks.decode(PipelineExecutionProjection.self, from: json)
        XCTAssertEqual(projection.runId, "run-x")
        XCTAssertEqual(projection.conversationId, "conv-x")
        XCTAssertEqual(projection.state, .running)
        XCTAssertEqual(projection.nodes.first?.state, .waitingForApproval)
        XCTAssertEqual(projection.nodes.first?.providerRef, "anthropic")
        XCTAssertEqual(projection.nodes.first?.attempt, 2)
        XCTAssertEqual(projection.metrics.active, 1)

        // Round-trip back to snake_case.
        let encoded = String(decoding: try JSONEncoder.opensks.encode(projection), as: UTF8.self)
        XCTAssertTrue(encoded.contains("\"waiting_for_approval\""))
        XCTAssertTrue(encoded.contains("\"projection_version\":1"))
        XCTAssertTrue(encoded.contains("\"last_public_message\":\"awaiting approval\""))
    }

    func testUnknownStateStringsDecodeLeniently() throws {
        let json = """
        {
          "schema": "opensks.pipeline-execution-projection.v1",
          "projection_version": 1,
          "run_id": "run-x",
          "conversation_id": null,
          "pipeline_id": null,
          "state": "from_the_future",
          "nodes": [{"node_id": "n", "state": "newish", "provider_ref": null,
                     "model_ref": null, "attempt": 0, "touched_paths": [],
                     "last_public_message": null}],
          "metrics": {"completed": 0, "active": 0, "queued": 0, "failed": 0}
        }
        """.data(using: .utf8)!

        let projection = try JSONDecoder.opensks.decode(PipelineExecutionProjection.self, from: json)
        XCTAssertEqual(projection.state, .queued, "unknown run state falls back to lowest rank")
        XCTAssertEqual(projection.nodes.first?.state, .queued, "unknown node state falls back too")
    }

    // MARK: - boundedness

    func testReducerRetainsNoRawEventPayloads() {
        // Feed many events; the projection node count stays bounded by distinct
        // nodes, not by the number of events (no unbounded raw retention).
        let store = PipelineProjectionStore()
        for i in 1...500 {
            store.ingest(event(id: "e\(i)", sequence: UInt64(i), kind: "work_item_running",
                               payload: ["node_id": .string("node-\(i % 4)"),
                                         "to": .string("running"),
                                         "message": .string("tick \(i)")]))
        }
        XCTAssertEqual(store.nodes(for: "run-1").count, 4,
                       "node count is bounded by distinct nodes, not event count")
        XCTAssertEqual(store.latestSequence(for: "run-1"), 500)
    }
}
