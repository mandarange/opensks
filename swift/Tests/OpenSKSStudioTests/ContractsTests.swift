import XCTest
@testable import OpenSKSStudio

final class ContractsTests: XCTestCase {
    func testEngineLineBufferDrainsCompleteLinesAcrossChunks() {
        let buffer = EngineLineBuffer()

        buffer.append(Data("{\"schema\":\"partial".utf8))
        XCTAssertEqual(buffer.drainLines(), [])

        buffer.append(Data("\"}\nsecond\nthird".utf8))
        XCTAssertEqual(buffer.drainLines(), ["{\"schema\":\"partial\"}", "second"])

        buffer.append(Data("-continued\n".utf8))
        XCTAssertEqual(buffer.drainLines(), ["third-continued"])
    }

    func testEngineLineBufferKeepsBoundedRecentLines() {
        let buffer = EngineLineBuffer(maxLines: 2)

        buffer.append(Data("first\nsecond\nthird\n".utf8))

        XCTAssertEqual(buffer.drainLines(), ["second", "third"])
    }

    func testEnginePendingResponseRouterKeepsConcurrentRequestStreamsSeparate() {
        let router = EnginePendingResponseRouter()
        let left = EngineRequestEnvelope.health(id: "req-left")
        let right = EngineRequestEnvelope.health(id: "req-right")

        router.register(left)
        router.register(right)
        router.append(Data("""
        {"schema":"opensks.engine-event.v1","event_id":"engine-health-right","request_id":"req-right","event_type":"engine_health","severity":"info","message":"right ok","protocol_version":"opensks.contracts.v1","timestamp_ms":123,"evidence_refs":["daemon:stdio-health"],"redacted":true}
        {"schema":"opensks.engine-event.v1","event_id":"engine-health-left","request_id":"req-left","event_type":"engine_health","severity":"info","message":"left ok","protocol_version":"opensks.contracts.v1","timestamp_ms":124,"evidence_refs":["daemon:stdio-health"],"redacted":true}

        """.utf8))

        let leftSnapshot = router.snapshot(for: "req-left")
        let rightSnapshot = router.snapshot(for: "req-right")
        XCTAssertEqual(leftSnapshot.lines.count, 1)
        XCTAssertEqual(rightSnapshot.lines.count, 1)
        XCTAssertTrue(leftSnapshot.lines.first?.contains("left ok") == true)
        XCTAssertTrue(rightSnapshot.lines.first?.contains("right ok") == true)
        XCTAssertTrue(leftSnapshot.sawRequestEvent)
        XCTAssertTrue(rightSnapshot.sawRequestEvent)

        let leftFinal = router.finish(requestId: "req-left", timedOut: false)
        let rightFinal = router.finish(requestId: "req-right", timedOut: false)
        XCTAssertEqual(leftFinal.lines.count, 1)
        XCTAssertEqual(rightFinal.lines.count, 1)
        XCTAssertFalse(leftFinal.timedOut)
        XCTAssertFalse(rightFinal.timedOut)
    }

    func testEnginePendingResponseRouterCompletesOnExplicitTerminalMarker() {
        // STREAM-001: the router completes a request when its EXPLICIT terminal
        // marker (request_completed) arrives — never on a silence/quiet-window
        // heuristic. The marker is an envelope signal, not a user-facing event, so
        // it is NOT added to the response lines.
        let router = EnginePendingResponseRouter()
        router.register(EngineRequestEnvelope.health(id: "req-term"))

        // A normal correlated event arrives first. The response is NOT complete yet,
        // even though a line is present and time passes — completion is marker-driven,
        // not line-count- or silence-driven.
        router.append(Data("""
        {"schema":"opensks.engine-event.v1","event_id":"engine-health","request_id":"req-term","event_type":"engine_health","severity":"info","message":"health ok","protocol_version":"opensks.contracts.v1","timestamp_ms":1,"evidence_refs":["daemon:stdio-health"],"redacted":true}

        """.utf8))
        let before = router.snapshot(for: "req-term")
        XCTAssertTrue(before.sawRequestEvent)
        XCTAssertFalse(before.isComplete, "no terminal marker yet ⇒ not complete")
        XCTAssertEqual(before.lines.count, 1)

        // The explicit terminal marker arrives back-to-back (zero quiet-window gap).
        router.append(Data("""
        {"schema":"opensks.engine-event.v1","event_id":"engine-request-completed-req-term","request_id":"req-term","event_type":"request_completed","severity":"info","message":"request completed","protocol_version":"opensks.contracts.v1","timestamp_ms":2,"evidence_refs":["daemon:request-completed"],"redacted":true}

        """.utf8))
        let after = router.snapshot(for: "req-term")
        XCTAssertTrue(after.isComplete, "the terminal marker completes the response immediately")
        // The terminal marker is envelope-only — it does NOT pollute the decoded lines.
        XCTAssertEqual(after.lines.count, 1, "terminal marker must not be added to the response lines")
        XCTAssertFalse(after.lines.contains { $0.contains("request_completed") })

        let final = router.finish(requestId: "req-term", timedOut: false)
        XCTAssertEqual(final.lines.count, 1)
        XCTAssertTrue(final.sawRequestEvent)
        XCTAssertFalse(final.timedOut)
    }

    func testEnginePendingResponseRouterRoutesRunEventsByRunId() {
        let router = EnginePendingResponseRouter()
        let request = EngineRequestEnvelope.runStart(
            id: "req-run",
            pipelineId: "single-model-safe",
            objective: "route run events",
            runId: "run-router"
        )

        router.register(request)
        router.append(Data("""
        {"schema":"opensks.engine-event.v1","event_id":"engine-run-start","request_id":"req-run","event_type":"execution_event","severity":"info","message":"run.start accepted","protocol_version":"opensks.contracts.v1","timestamp_ms":123,"evidence_refs":["daemon:run-start"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-router-1","run_id":"run-router","sequence":1,"occurred_at":"t1","actor":"opensks-engine","kind":"run_started","payload":{"message":"started"},"sensitivity":"public","evidence_refs":["daemon:run-start-request"]}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-other-1","run_id":"run-other","sequence":1,"occurred_at":"t1","actor":"opensks-engine","kind":"run_started","payload":{"message":"other"},"sensitivity":"public","evidence_refs":["daemon:run-start-request"]}

        """.utf8))

        let final = router.finish(requestId: "req-run", timedOut: false)
        XCTAssertEqual(final.lines.count, 2)
        XCTAssertTrue(final.sawRequestEvent)
        XCTAssertTrue(final.lines.contains { $0.contains("\"request_id\":\"req-run\"") })
        XCTAssertTrue(final.lines.contains { $0.contains("\"run_id\":\"run-router\"") })
        XCTAssertFalse(final.lines.contains { $0.contains("\"run_id\":\"run-other\"") })
    }

    func testEnginePendingResponseRouterDoesNotCrossWireSameRunStartAndSubscribe() {
        let router = EnginePendingResponseRouter()
        let start = EngineRequestEnvelope.runStart(
            id: "req-run",
            pipelineId: "single-model-safe",
            objective: "same run routing",
            runId: "run-shared"
        )
        let subscribe = EngineRequestEnvelope.subscribeEvents(
            id: "req-subscribe-run-shared",
            runId: "run-shared",
            sinceSequence: 0
        )

        router.register(start)
        router.register(subscribe)
        router.append(Data("""
        {"schema":"opensks.engine-event.v1","event_id":"engine-run-start","request_id":"req-run","event_type":"execution_event","severity":"info","message":"run.start accepted","protocol_version":"opensks.contracts.v1","timestamp_ms":1,"evidence_refs":["daemon:run-start"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-from-start","run_id":"run-shared","sequence":1,"occurred_at":"t1","actor":"opensks-engine","kind":"run_started","payload":{"message":"started"},"sensitivity":"public","evidence_refs":["daemon:run-start-request"]}
        {"schema":"opensks.engine-event.v1","event_id":"engine-subscribe-events-run-shared","request_id":"req-subscribe-run-shared","event_type":"execution_event","severity":"info","message":"event stream replayed 1 events since sequence 0","protocol_version":"opensks.contracts.v1","timestamp_ms":2,"evidence_refs":["daemon:subscription-accepted","event-store:replay-since"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-from-subscribe","run_id":"run-shared","sequence":2,"occurred_at":"t2","actor":"opensks-engine","kind":"snapshot_written","payload":{"message":"snapshot"},"sensitivity":"public","evidence_refs":["event-store:replay-since"]}

        """.utf8))

        let startFinal = router.finish(requestId: "req-run", timedOut: false)
        let subscribeFinal = router.finish(requestId: "req-subscribe-run-shared", timedOut: false)

        XCTAssertTrue(startFinal.sawRequestEvent)
        XCTAssertTrue(subscribeFinal.sawRequestEvent)
        XCTAssertTrue(startFinal.lines.contains { $0.contains("\"request_id\":\"req-run\"") })
        XCTAssertFalse(startFinal.lines.contains { $0.contains("\"request_id\":\"req-subscribe-run-shared\"") })
        XCTAssertTrue(startFinal.lines.contains { $0.contains("\"id\":\"evt-from-start\"") })
        XCTAssertFalse(startFinal.lines.contains { $0.contains("\"id\":\"evt-from-subscribe\"") })
        XCTAssertTrue(subscribeFinal.lines.contains { $0.contains("\"request_id\":\"req-subscribe-run-shared\"") })
        XCTAssertFalse(subscribeFinal.lines.contains { $0.contains("\"request_id\":\"req-run\"") })
        XCTAssertTrue(subscribeFinal.lines.contains { $0.contains("\"id\":\"evt-from-subscribe\"") })
        XCTAssertFalse(subscribeFinal.lines.contains { $0.contains("\"id\":\"evt-from-start\"") })
    }

    func testEnginePendingResponseRouterUsesLatestSameRunRequestOwner() {
        let router = EnginePendingResponseRouter()
        let start = EngineRequestEnvelope.runStart(
            id: "req-run",
            pipelineId: "single-model-safe",
            objective: "reverse same run routing",
            runId: "run-shared"
        )
        let subscribe = EngineRequestEnvelope.subscribeEvents(
            id: "req-subscribe-run-shared",
            runId: "run-shared",
            sinceSequence: 0
        )

        router.register(start)
        router.register(subscribe)
        router.append(Data("""
        {"schema":"opensks.engine-event.v1","event_id":"engine-subscribe-events-run-shared","request_id":"req-subscribe-run-shared","event_type":"execution_event","severity":"info","message":"event stream replayed 1 events since sequence 0","protocol_version":"opensks.contracts.v1","timestamp_ms":1,"evidence_refs":["daemon:subscription-accepted","event-store:replay-since"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-from-subscribe","run_id":"run-shared","sequence":1,"occurred_at":"t1","actor":"opensks-engine","kind":"snapshot_written","payload":{"message":"snapshot"},"sensitivity":"public","evidence_refs":["event-store:replay-since"]}
        {"schema":"opensks.engine-event.v1","event_id":"engine-run-start","request_id":"req-run","event_type":"execution_event","severity":"info","message":"run.start accepted","protocol_version":"opensks.contracts.v1","timestamp_ms":2,"evidence_refs":["daemon:run-start"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-from-start","run_id":"run-shared","sequence":2,"occurred_at":"t2","actor":"opensks-engine","kind":"run_started","payload":{"message":"started"},"sensitivity":"public","evidence_refs":["daemon:run-start-request"]}

        """.utf8))

        let startFinal = router.finish(requestId: "req-run", timedOut: false)
        let subscribeFinal = router.finish(requestId: "req-subscribe-run-shared", timedOut: false)

        XCTAssertTrue(startFinal.lines.contains { $0.contains("\"request_id\":\"req-run\"") })
        XCTAssertTrue(startFinal.lines.contains { $0.contains("\"id\":\"evt-from-start\"") })
        XCTAssertFalse(startFinal.lines.contains { $0.contains("\"id\":\"evt-from-subscribe\"") })
        XCTAssertTrue(subscribeFinal.lines.contains { $0.contains("\"request_id\":\"req-subscribe-run-shared\"") })
        XCTAssertTrue(subscribeFinal.lines.contains { $0.contains("\"id\":\"evt-from-subscribe\"") })
        XCTAssertFalse(subscribeFinal.lines.contains { $0.contains("\"id\":\"evt-from-start\"") })
    }

    func testEngineEventDecodesSnakeCaseEnvelope() throws {
        let json = """
        {
          "schema": "opensks.engine-event.v1",
          "event_id": "engine-health",
          "request_id": "req-1",
          "event_type": "engine_health",
          "severity": "info",
          "message": "health ok",
          "protocol_version": "opensks.contracts.v1",
          "timestamp_ms": 123,
          "evidence_refs": ["daemon:stdio-health"],
          "redacted": true
        }
        """.data(using: .utf8)!

        let event = try JSONDecoder.opensks.decode(EngineEvent.self, from: json)
        XCTAssertEqual(event.id, "engine-health")
        XCTAssertEqual(event.requestId, "req-1")
        XCTAssertEqual(event.eventType, .engineHealth)
        XCTAssertEqual(event.eventType.rawValue, "engine_health")
        XCTAssertEqual(event.severity, .info)
        XCTAssertTrue(event.redacted)
    }

    func testEngineEventPreservesUnknownTypedValues() throws {
        let json = """
        {
          "schema": "opensks.engine-event.v1",
          "event_id": "future-engine-event",
          "request_id": null,
          "event_type": "future_engine_signal",
          "severity": "notice",
          "message": "future ok",
          "protocol_version": "opensks.contracts.v1",
          "timestamp_ms": 456,
          "evidence_refs": [],
          "redacted": true
        }
        """.data(using: .utf8)!

        let event = try JSONDecoder.opensks.decode(EngineEvent.self, from: json)
        XCTAssertEqual(event.eventType, .unrecognized("future_engine_signal"))
        XCTAssertEqual(event.eventType.rawValue, "future_engine_signal")
        XCTAssertEqual(event.severity, .unrecognized("notice"))
        XCTAssertFalse(event.severity.isError)

        let encoded = String(decoding: try JSONEncoder.opensks.encode(event), as: UTF8.self)
        XCTAssertTrue(encoded.contains("\"event_type\":\"future_engine_signal\""))
        XCTAssertTrue(encoded.contains("\"severity\":\"notice\""))
    }

    func testExecutionEventEnvelopeDecodesTypedKindAndSensitivity() throws {
        let json = """
        {
          "schema": "opensks.execution-event-envelope.v1",
          "id": "evt-typed",
          "run_id": "run-typed",
          "sequence": 1,
          "occurred_at": "t1",
          "actor": "opensks-engine",
          "kind": "snapshot_written",
          "payload": {"message": "snapshot written"},
          "sensitivity": "public",
          "evidence_refs": ["event-store:snapshot-written"]
        }
        """.data(using: .utf8)!

        let event = try JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: json)
        XCTAssertEqual(event.kind, .snapshotWritten)
        XCTAssertEqual(event.kind.rawValue, "snapshot_written")
        XCTAssertEqual(event.sensitivity, .public)
    }

    func testExecutionEventEnvelopePreservesUnknownTypedValues() throws {
        let json = """
        {
          "schema": "opensks.execution-event-envelope.v1",
          "id": "evt-future",
          "run_id": "run-future",
          "sequence": 7,
          "occurred_at": "t7",
          "actor": "opensks-engine",
          "kind": "future_scheduler_signal",
          "payload": {"message": "future signal"},
          "sensitivity": "confidential-preview",
          "evidence_refs": ["future:evidence"]
        }
        """.data(using: .utf8)!

        let event = try JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: json)
        XCTAssertEqual(event.kind, .unrecognized("future_scheduler_signal"))
        XCTAssertEqual(event.kind.rawValue, "future_scheduler_signal")
        XCTAssertEqual(event.sensitivity, .unrecognized("confidential-preview"))
        XCTAssertEqual(event.sensitivity.rawValue, "confidential-preview")

        let encoded = String(decoding: try JSONEncoder.opensks.encode(event), as: UTF8.self)
        XCTAssertTrue(encoded.contains("\"kind\":\"future_scheduler_signal\""))
        XCTAssertTrue(encoded.contains("\"sensitivity\":\"confidential-preview\""))
    }

    func testHealthRequestEncodesSnakeCaseKind() throws {
        let request = EngineRequestEnvelope.health(id: "req-health")
        let json = String(decoding: try JSONEncoder.opensks.encode(request), as: UTF8.self)
        XCTAssertTrue(json.contains("\"kind\":\"health\""))
        XCTAssertTrue(json.contains("\"id\":\"req-health\""))
        XCTAssertTrue(json.contains("\"params\":{"))
    }

    @MainActor
    func testEngineRunStreamDecodesAndFeedsExecutionStore() throws {
        let ndjson = """
        {"schema":"opensks.engine-event.v1","event_id":"engine-run-start","request_id":"req-run","event_type":"execution_event","severity":"info","message":"run.start accepted","protocol_version":"opensks.contracts.v1","timestamp_ms":123,"evidence_refs":["daemon:run-start"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-1","run_id":"run-swift","sequence":1,"occurred_at":"t1","actor":"opensks-engine","kind":"run_started","payload":{"message":"started"},"sensitivity":"public","evidence_refs":["daemon:run-start-request"]}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-2","run_id":"run-swift","sequence":2,"occurred_at":"t2","actor":"opensks-scheduler","kind":"work_item_running","payload":{"work_item_id":"wi-swift","to":"running"},"sensitivity":"public","evidence_refs":[]}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-3","run_id":"run-swift","sequence":3,"occurred_at":"t3","actor":"opensks-engine","kind":"snapshot_written","payload":{"message":"snapshot written"},"sensitivity":"public","evidence_refs":["event-store:snapshot-written"]}
        """.data(using: .utf8)!

        let stream = EngineProcess.decodeRunStream(ndjson)
        XCTAssertEqual(stream.engineEvents.count, 1)
        XCTAssertEqual(stream.executionEvents.count, 3)
        XCTAssertEqual(stream.executionEvents.last?.evidenceRefs, ["event-store:snapshot-written"])

        let store = ExecutionStore()
        store.rebuild(from: stream.executionEvents)
        XCTAssertEqual(store.runs.first?.id, "run-swift")
        XCTAssertEqual(store.runs.first?.state, "snapshot")
        XCTAssertEqual(store.queueItems.first?.state, "running")
    }

    func testEngineControlRequestEncodesSnakeCaseParams() throws {
        let request = EngineRequestEnvelope.runControl(
            id: "req-cancel",
            kind: "run_cancel",
            runId: "run-swift",
            targetId: nil,
            message: "cancel requested",
            reasonCode: "cancelled_by_user"
        )
        let json = String(decoding: try JSONEncoder.opensks.encode(request), as: UTF8.self)
        XCTAssertTrue(json.contains("\"kind\":\"run_cancel\""))
        XCTAssertTrue(json.contains("\"run_id\":\"run-swift\""))
        XCTAssertTrue(json.contains("\"reason_code\":\"cancelled_by_user\""))
    }

    func testApprovalRequestEncodesSnakeCaseParams() throws {
        let request = EngineRequestEnvelope.approval(
            id: "req-approval",
            kind: "approval_request",
            runId: "run-swift",
            approvalId: "approval-1",
            scope: "git_push",
            message: "Approve push",
            reasonCode: "approval_required"
        )
        let json = String(decoding: try JSONEncoder.opensks.encode(request), as: UTF8.self)
        XCTAssertTrue(json.contains("\"kind\":\"approval_request\""))
        XCTAssertTrue(json.contains("\"approval_id\":\"approval-1\""))
        XCTAssertTrue(json.contains("\"scope\":\"git_push\""))
    }

    func testSubscribeEventsRequestEncodesReplayCursor() throws {
        let request = EngineRequestEnvelope.subscribeEvents(
            id: "req-subscribe",
            runId: "run-swift",
            sinceSequence: 4,
            tailMs: 1_500,
            pollIntervalMs: 100
        )
        let json = String(decoding: try JSONEncoder.opensks.encode(request), as: UTF8.self)
        XCTAssertTrue(json.contains("\"kind\":\"subscribe_events\""))
        XCTAssertTrue(json.contains("\"run_id\":\"run-swift\""))
        XCTAssertTrue(json.contains("\"since_sequence\":4"))
        XCTAssertTrue(json.contains("\"tail_ms\":1500"))
        XCTAssertTrue(json.contains("\"poll_interval_ms\":100"))
    }

    func testRunStartRequestEncodesGraphPath() throws {
        let request = EngineRequestEnvelope.runStart(
            id: "req-run-graph",
            pipelineId: "editor-draft",
            objective: "Run saved graph",
            runId: "run-swift-graph",
            graphPath: ".opensks/pipelines/editor/current.graph.json"
        )
        let data = try JSONEncoder.opensks.encode(request)
        let json = String(decoding: data, as: UTF8.self)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let params = try XCTUnwrap(object["params"] as? [String: Any])
        XCTAssertTrue(json.contains("\"kind\":\"run_start\""))
        XCTAssertTrue(json.contains("\"pipeline_id\":\"editor-draft\""))
        XCTAssertEqual(params["graph_path"] as? String, ".opensks/pipelines/editor/current.graph.json")
    }

    func testConversationTurnStartRequestEncodesTypedPayload() throws {
        let turnRequest = ConversationTurnStartRequest(
            schema: "opensks.conversation-turn-start-request.v1",
            requestId: "req-conversation-turn",
            projectId: "project-1",
            conversationId: "conversation-1",
            clientTurnId: "client-turn-1",
            message: UserMessageInput(text: "start this turn", attachmentRefs: []),
            settings: .defaultForTurn(),
            context: .empty,
            idempotencyKey: "idem-1"
        )
        let request = EngineRequestEnvelope.conversationTurnStart(turnRequest)
        let data = try JSONEncoder.opensks.encode(request)
        let json = String(decoding: data, as: UTF8.self)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let params = try XCTUnwrap(object["params"] as? [String: Any])
        let nested = try XCTUnwrap(params["conversation_turn_start"] as? [String: Any])
        let settings = try XCTUnwrap(nested["settings"] as? [String: Any])
        let model = try XCTUnwrap(settings["model"] as? [String: Any])

        XCTAssertTrue(json.contains("\"kind\":\"conversation_turn_start\""))
        XCTAssertEqual(object["id"] as? String, "req-conversation-turn")
        XCTAssertEqual(nested["request_id"] as? String, "req-conversation-turn")
        XCTAssertEqual(nested["project_id"] as? String, "project-1")
        XCTAssertEqual(nested["conversation_id"] as? String, "conversation-1")
        XCTAssertEqual(nested["idempotency_key"] as? String, "idem-1")
        XCTAssertEqual(settings["execution_mode"] as? String, "worktree")
        XCTAssertEqual(settings["reasoning_effort"] as? String, "standard")
        XCTAssertEqual(settings["pipeline_id"] as? String, "auto")
        XCTAssertEqual(model["mode"] as? String, "auto")
    }

    func testConversationThreadSettingsRoundTripsSnakeCaseWireContract() throws {
        let settings = ConversationThreadSettings(
            schema: "opensks.thread-settings.v1",
            conversationId: "conversation-1",
            modelSelection: ModelSelection(
                mode: .pinned,
                modelId: "openai/gpt-4o-mini",
                fallbackModelIds: []
            ),
            reasoningEffort: .deep,
            executionMode: .readOnly,
            pipelineId: "parallel-build",
            maxParallelism: 8,
            verifierCount: 2,
            toolPolicyId: "read-only",
            approvalPolicyId: "safe-interactive",
            imageModelId: nil,
            updatedAtMs: 42
        )
        let data = try JSONEncoder.opensks.encode(settings)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let model = try XCTUnwrap(object["model_selection"] as? [String: Any])

        XCTAssertEqual(object["schema"] as? String, "opensks.thread-settings.v1")
        XCTAssertEqual(object["conversation_id"] as? String, "conversation-1")
        XCTAssertEqual(object["execution_mode"] as? String, "read_only")
        XCTAssertEqual(object["reasoning_effort"] as? String, "deep")
        XCTAssertEqual(object["pipeline_id"] as? String, "parallel-build")
        XCTAssertEqual(object["tool_policy_id"] as? String, "read-only")
        XCTAssertEqual(model["mode"] as? String, "pinned")
        XCTAssertEqual(model["model_id"] as? String, "openai/gpt-4o-mini")

        let decoded = try JSONDecoder.opensks.decode(ConversationThreadSettings.self, from: data)
        XCTAssertEqual(decoded, settings)
    }

    func testConversationTurnAcceptedDecodesFromDaemonResponseLine() throws {
        let lines = [
            """
            {"schema":"opensks.conversation-turn-accepted.v1","request_id":"req-conversation-turn","turn_id":"turn-1","run_id":"turn-turn-1","user_message_id":"user-1","assistant_message_id":"assistant-1","stream_id":"stream-turn-1","state":"queued"}
            """
        ]
        let accepted = try XCTUnwrap(EngineProcess.decodeConversationTurnAccepted(lines))
        XCTAssertEqual(accepted.requestId, "req-conversation-turn")
        XCTAssertEqual(accepted.turnId, "turn-1")
        XCTAssertEqual(accepted.runId, "turn-turn-1")
        XCTAssertEqual(accepted.userMessageId, "user-1")
        XCTAssertEqual(accepted.assistantMessageId, "assistant-1")
        XCTAssertEqual(accepted.streamId, "stream-turn-1")
        XCTAssertEqual(accepted.state, .queued)
    }

    func testConversationTurnAcceptedRoutesThroughPendingResponseRouter() throws {
        let router = EnginePendingResponseRouter()
        let turnRequest = ConversationTurnStartRequest(
            schema: "opensks.conversation-turn-start-request.v1",
            requestId: "req-conversation-turn",
            projectId: "project-1",
            conversationId: "conversation-1",
            clientTurnId: "client-turn-1",
            message: UserMessageInput(text: "start this turn", attachmentRefs: []),
            settings: .defaultForTurn(),
            context: .empty,
            idempotencyKey: "idem-1"
        )
        router.register(.conversationTurnStart(turnRequest))
        router.append(Data("""
        {"schema":"opensks.conversation-turn-accepted.v1","request_id":"req-conversation-turn","turn_id":"turn-1","run_id":"turn-turn-1","user_message_id":"user-1","assistant_message_id":"assistant-1","stream_id":"stream-turn-1","state":"queued"}
        {"schema":"opensks.engine-event.v1","event_id":"engine-request-completed-req-conversation-turn","request_id":"req-conversation-turn","event_type":"request_completed","severity":"info","message":"request completed","protocol_version":"opensks.contracts.v1","timestamp_ms":2,"evidence_refs":["daemon:request-completed"],"redacted":true}

        """.utf8))

        let snapshot = router.snapshot(for: "req-conversation-turn")
        XCTAssertTrue(snapshot.sawRequestEvent)
        XCTAssertTrue(snapshot.isComplete)
        XCTAssertEqual(snapshot.lines.count, 1)
        XCTAssertTrue(snapshot.lines[0].contains("opensks.conversation-turn-accepted.v1"))
        let accepted = try XCTUnwrap(EngineProcess.decodeConversationTurnAccepted(snapshot.lines))
        XCTAssertEqual(accepted.runId, "turn-turn-1")
    }

    func testConversationSupervisorTickRequestEncodesTypedParams() throws {
        let request = EngineRequestEnvelope.conversationSupervisorTick(
            id: "req-supervisor",
            supervisorId: "swift-chat-supervisor",
            leaseTtlMs: 30_000
        )
        let data = try JSONEncoder.opensks.encode(request)
        let json = String(decoding: data, as: UTF8.self)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let params = try XCTUnwrap(object["params"] as? [String: Any])

        XCTAssertTrue(json.contains("\"kind\":\"conversation_supervisor_tick\""))
        XCTAssertEqual(object["id"] as? String, "req-supervisor")
        XCTAssertEqual(params["supervisor_id"] as? String, "swift-chat-supervisor")
        XCTAssertEqual(params["lease_ttl_ms"] as? Int, 30_000)
        XCTAssertEqual(params["reason_code"] as? String, "conversation_supervisor_tick_requested")
    }

    func testConversationSupervisorTickResultDecodesFromDaemonResponseLine() throws {
        let lines = [
            """
            {"schema":"opensks.turn-supervisor-tick.v1","request_id":"req-supervisor","supervisor_id":"swift-chat-supervisor","recovered_expired_leases":1,"claimed":{"turn_id":"turn-1","run_id":"turn-turn-1","project_id":"project-1","conversation_id":"conversation-1","assistant_message_id":"assistant-1","lease_owner":"swift-chat-supervisor","lease_expires_at_ms":12345,"has_model_routing_decision":true},"executed":{"status":"executed","run_state":"completed","assistant_message_id":"assistant-1","last_event_sequence":6,"patch_count":1,"apply_result_count":1}}
            """
        ]
        let tick = try XCTUnwrap(EngineProcess.decodeTurnSupervisorTickResult(lines))
        XCTAssertEqual(tick.requestId, "req-supervisor")
        XCTAssertEqual(tick.supervisorId, "swift-chat-supervisor")
        XCTAssertEqual(tick.recoveredExpiredLeases, 1)
        XCTAssertEqual(tick.claimed?.runId, "turn-turn-1")
        XCTAssertEqual(tick.claimed?.hasModelRoutingDecision, true)
        XCTAssertEqual(tick.executed?.status, "executed")
        XCTAssertEqual(tick.executed?.runState, .completed)
        XCTAssertEqual(tick.executed?.lastEventSequence, 6)
        XCTAssertEqual(tick.executed?.patchCount, 1)
    }

    func testConversationSupervisorTickRoutesThroughPendingResponseRouter() throws {
        let router = EnginePendingResponseRouter()
        router.register(.conversationSupervisorTick(
            id: "req-supervisor",
            supervisorId: "swift-chat-supervisor",
            leaseTtlMs: 30_000
        ))
        router.append(Data("""
        {"schema":"opensks.turn-supervisor-tick.v1","request_id":"req-supervisor","supervisor_id":"swift-chat-supervisor","recovered_expired_leases":0,"claimed":null,"executed":null}
        {"schema":"opensks.engine-event.v1","event_id":"engine-request-completed-req-supervisor","request_id":"req-supervisor","event_type":"request_completed","severity":"info","message":"request completed","protocol_version":"opensks.contracts.v1","timestamp_ms":2,"evidence_refs":["daemon:request-completed"],"redacted":true}

        """.utf8))

        let snapshot = router.snapshot(for: "req-supervisor")
        XCTAssertTrue(snapshot.sawRequestEvent)
        XCTAssertTrue(snapshot.isComplete)
        XCTAssertEqual(snapshot.lines.count, 1)
        let tick = try XCTUnwrap(EngineProcess.decodeTurnSupervisorTickResult(snapshot.lines))
        XCTAssertNil(tick.claimed)
    }

    @MainActor
    func testSubscribeReplayStreamRebuildsExecutionStore() throws {
        let ndjson = """
        {"schema":"opensks.engine-event.v1","event_id":"engine-subscribe-events-run-swift","request_id":"req-subscribe","event_type":"execution_event","severity":"info","message":"event stream replayed 2 events since sequence 0","protocol_version":"opensks.contracts.v1","timestamp_ms":123,"evidence_refs":["daemon:subscription-accepted","event-store:replay-since"],"redacted":true}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-1","run_id":"run-swift","sequence":1,"occurred_at":"t1","actor":"opensks-engine","kind":"run_started","payload":{"message":"started"},"sensitivity":"public","evidence_refs":["daemon:run-start-request"]}
        {"schema":"opensks.execution-event-envelope.v1","id":"evt-2","run_id":"run-swift","sequence":2,"occurred_at":"t2","actor":"opensks-engine","kind":"snapshot_written","payload":{"message":"snapshot written"},"sensitivity":"public","evidence_refs":["event-store:snapshot-written"]}
        """.data(using: .utf8)!

        let stream = EngineProcess.decodeRunStream(ndjson)
        XCTAssertEqual(stream.engineEvents.first?.evidenceRefs, ["daemon:subscription-accepted", "event-store:replay-since"])
        XCTAssertEqual(stream.executionEvents.count, 2)
        let store = ExecutionStore()
        store.rebuild(from: stream.executionEvents)
        XCTAssertEqual(store.runs.first?.id, "run-swift")
        XCTAssertEqual(store.runs.first?.state, "snapshot")
    }

    @MainActor
    func testExecutionStoreAppliesRunControlAndSteeringEvents() throws {
        let data = """
        [
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-1",
            "run_id": "run-control",
            "sequence": 1,
            "occurred_at": "t1",
            "actor": "test",
            "kind": "run_cancelled",
            "payload": {"message": "cancel requested", "reason_code": "cancelled_by_user"},
            "sensitivity": "public",
            "evidence_refs": ["daemon:run-control-request"]
          },
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-2",
            "run_id": "run-control",
            "sequence": 2,
            "occurred_at": "t2",
            "actor": "test",
            "kind": "steering_requested",
            "payload": {"steering_id": "steer-1", "target_id": "work-1", "message": "focus tests"},
            "sensitivity": "public",
            "evidence_refs": ["daemon:run-control-request"]
          }
        ]
        """.data(using: .utf8)!
        let events = try JSONDecoder.opensks.decode([ExecutionEventEnvelope].self, from: data)
        let store = ExecutionStore()
        store.rebuild(from: events)
        XCTAssertEqual(store.runs.first?.state, "cancelled")
        XCTAssertEqual(store.steering.first?.targetId, "work-1")
        XCTAssertEqual(store.steering.first?.message, "focus tests")
    }

    @MainActor
    func testExecutionStoreAppliesApprovalEvents() throws {
        let data = """
        [
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-1",
            "run_id": "run-approval",
            "sequence": 1,
            "occurred_at": "t1",
            "actor": "test",
            "kind": "approval_requested",
            "payload": {"approval_id": "approval-1", "scope": "git_push", "state": "pending", "message": "approve push"},
            "sensitivity": "public",
            "evidence_refs": ["daemon:approval-request"]
          },
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-2",
            "run_id": "run-approval",
            "sequence": 2,
            "occurred_at": "t2",
            "actor": "test",
            "kind": "approval_approved",
            "payload": {"approval_id": "approval-1", "scope": "git_push", "state": "approved", "message": "approved"},
            "sensitivity": "public",
            "evidence_refs": ["daemon:approval-request"]
          }
        ]
        """.data(using: .utf8)!
        let events = try JSONDecoder.opensks.decode([ExecutionEventEnvelope].self, from: data)
        let store = ExecutionStore()
        store.rebuild(from: events)
        XCTAssertEqual(store.approvals.first?.id, "approval-1")
        XCTAssertEqual(store.approvals.first?.scope, "git_push")
        XCTAssertEqual(store.approvals.first?.state, "approved")
    }

    func testHonestTextNeverSaysComplete() {
        let acceptance = Acceptance(total: 2, passed: 2, partial: 0, failed: 0, goalComplete: true)
        XCTAssertEqual(HonestText.goalState(acceptance), "Verifying")
        XCTAssertFalse(HonestText.statusLine(acceptance).lowercased().contains("complete"))
    }

    @MainActor
    func testExecutionStoreRebuildsRunAndQueueFromEventStream() throws {
        let data = """
        [
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-1",
            "run_id": "run-1",
            "sequence": 1,
            "occurred_at": "t1",
            "actor": "test",
            "kind": "run_started",
            "payload": {"message": "started"},
            "sensitivity": "public",
            "evidence_refs": []
          },
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-2",
            "run_id": "run-1",
            "sequence": 2,
            "occurred_at": "t2",
            "actor": "test",
            "kind": "work_item_queued",
            "payload": {"work_item_id": "wi-1", "to": "queued", "priority": 4},
            "sensitivity": "public",
            "evidence_refs": []
          },
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-3",
            "run_id": "run-1",
            "sequence": 3,
            "occurred_at": "t3",
            "actor": "test",
            "kind": "work_item_running",
            "payload": {"work_item_id": "wi-1", "to": "running", "message": "running"},
            "sensitivity": "public",
            "evidence_refs": []
          },
          {
            "schema": "opensks.execution-event-envelope.v1",
            "id": "evt-4",
            "run_id": "run-1",
            "sequence": 4,
            "occurred_at": "t3",
            "actor": "test",
            "kind": "work_item_completed",
            "payload": {"work_item_id": "wi-1", "to": "completed", "message": "done"},
            "sensitivity": "public",
            "evidence_refs": ["proof"]
          }
        ]
        """.data(using: .utf8)!
        let events = try JSONDecoder.opensks.decode([ExecutionEventEnvelope].self, from: data)

        let store = ExecutionStore()
        store.rebuild(from: events)

        XCTAssertEqual(store.runs.first?.id, "run-1")
        XCTAssertEqual(store.runs.first?.state, "verifying")
        XCTAssertEqual(store.queueItems.first?.id, "wi-1")
        XCTAssertEqual(store.queueItems.first?.state, "completed")
        XCTAssertEqual(store.queueItems.first?.lastSequence, 4)
    }

    @MainActor
    func testProjectIntelligenceUsesLodAndClickToSourcePath() {
        let store = ProjectIntelligenceStore()
        let records = (0..<600).map { index in
            IntelligenceRecord(
                id: "record-\(index)",
                kind: index % 2 == 0 ? "symbol" : "glossary",
                title: "Record \(index)",
                path: "/tmp/source-\(index).swift",
                summary: "summary"
            )
        }
        store.load(records: records, freshness: "stale")
        XCTAssertEqual(store.visibleRecords(limit: 40).count, 40)
        XCTAssertEqual(store.freshnessLabel, "Stale")
        XCTAssertEqual(store.sourcePath(for: "record-42"), "/tmp/source-42.swift")
    }

    @MainActor
    func testGraphEditorUndoRedoAndTypedPortProblems() {
        let store = GraphEditorStore()
        store.reset(nodes: [
            GraphEditorNode(id: "goal", kind: "goal_input", title: "Goal", inputType: nil, outputType: "string"),
            GraphEditorNode(id: "seal", kind: "final_seal", title: "FinalSeal", inputType: "proof", outputType: nil)
        ])
        store.connect(GraphEditorEdge(id: "edge-1", fromNodeId: "goal", toNodeId: "seal", portType: "string"))
        XCTAssertTrue(store.problems.contains { $0.message == "Typed port mismatch" })
        store.undo()
        XCTAssertTrue(store.edges.isEmpty)
        store.redo()
        XCTAssertEqual(store.edges.count, 1)
    }

    @MainActor
    func testGraphEditorBlocksUnsupportedAndApprovalRequiredNodeKinds() {
        let store = GraphEditorStore()
        store.reset(nodes: [
            GraphEditorNode(id: "goal", kind: "goal_input", title: "Goal", inputType: nil, outputType: "control"),
            GraphEditorNode(id: "push", kind: "git_push", title: "Push", inputType: "control", outputType: "control"),
            GraphEditorNode(id: "mystery", kind: "not_a_contract_node", title: "Mystery", inputType: "control", outputType: "control"),
            GraphEditorNode(id: "seal", kind: "final_seal", title: "FinalSeal", inputType: "control", outputType: nil)
        ], edges: [
            GraphEditorEdge(id: "edge-goal-push", fromNodeId: "goal", toNodeId: "push", portType: "control"),
            GraphEditorEdge(id: "edge-push-seal", fromNodeId: "push", toNodeId: "seal", portType: "control")
        ])
        XCTAssertTrue(store.problems.contains { $0.message == "Side-effect node requires approval policy" })
        XCTAssertTrue(store.problems.contains { $0.message == "Unsupported graph node kind" })
    }

    @MainActor
    func testGraphEditorSavesLoadsAndMarksRunnableTemplate() throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent("opensks-graph-editor-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: workspace) }

        let store = GraphEditorStore()
        store.loadSingleModelSafeTemplate()
        XCTAssertTrue(store.problems.isEmpty)
        XCTAssertTrue(store.canRunDaemonTemplate)
        XCTAssertEqual(store.visibleNodes(limit: 2).count, 2)

        let saved = try store.saveCurrentDocument(workspace: workspace)
        let raw = try String(contentsOf: saved, encoding: .utf8)
        XCTAssertTrue(raw.contains("\"schema\":\"opensks.graph-editor-document.v1\""))
        XCTAssertTrue(raw.contains("\"run_template_id\":\"single-model-safe\""))
        XCTAssertNotNil(store.lastExportedGraphPath)
        let exported = workspace
            .appendingPathComponent(".opensks", isDirectory: true)
            .appendingPathComponent("pipelines", isDirectory: true)
            .appendingPathComponent("editor", isDirectory: true)
            .appendingPathComponent("current.graph.json")
        let graphRaw = try String(contentsOf: exported, encoding: .utf8)
        let graphData = try Data(contentsOf: exported)
        let graph = try XCTUnwrap(JSONSerialization.jsonObject(with: graphData) as? [String: Any])
        let graphNodes = try XCTUnwrap(graph["nodes"] as? [String: Any])
        let goalNode = try XCTUnwrap(graphNodes["goal"] as? [String: Any])
        let graphEdges = try XCTUnwrap(graph["edges"] as? [[String: Any]])
        let firstEdge = try XCTUnwrap(graphEdges.first)
        let firstEdgeFrom = try XCTUnwrap(firstEdge["from"] as? [String: Any])
        XCTAssertTrue(graphRaw.contains("\"schema\":\"opensks.pipeline-graph.v1\""))
        XCTAssertTrue(graphRaw.contains("\"entry_nodes\":[\"goal\"]"))
        XCTAssertTrue(graphRaw.contains("\"final_seal_required\":true"))
        XCTAssertTrue(graphRaw.contains("\"studio:graph-editor-export\""))
        XCTAssertEqual(goalNode["display_name"] as? String, "Goal input")
        XCTAssertEqual(firstEdgeFrom["node_id"] as? String, "goal")

        store.reset()
        XCTAssertFalse(store.canRunDaemonTemplate)
        let loaded = try store.loadSavedDocument(workspace: workspace)
        XCTAssertEqual(loaded.id, "single-model-safe")
        XCTAssertEqual(store.nodes.count, 3)
        XCTAssertEqual(store.edges.count, 2)
        XCTAssertTrue(store.canRunDaemonTemplate)
    }
}
