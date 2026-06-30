import SwiftUI
import Combine
import XCTest
@testable import OpenSKSStudio

@MainActor
final class ConversationUITests: XCTestCase {
    // MARK: - Fixtures

    private func summary(
        id: String,
        title: String,
        status: ConversationStatus = .idle,
        pinned: Bool = false,
        archived: Bool = false,
        messageCount: Int = 0,
        activityMs: Int64 = 1_000
    ) -> ConversationSummary {
        ConversationSummary(
            schema: "opensks.conversation-summary.v1",
            id: id,
            projectId: "mock-project",
            title: title,
            titleSource: .generated,
            status: status,
            pinned: pinned,
            archived: archived,
            messageCount: messageCount,
            createdAtMs: activityMs,
            updatedAtMs: activityMs,
            lastMessageAtMs: messageCount > 0 ? activityMs : nil
        )
    }

    private func message(
        id: String,
        conversation: String,
        role: MessageRole,
        text: String,
        sequence: Int64
    ) -> ConversationMessage {
        ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: id,
            projectId: "mock-project",
            conversationId: conversation,
            turnId: nil,
            role: role,
            state: .complete,
            contentRedacted: text,
            sequence: sequence,
            createdAtMs: 1_000 + sequence,
            updatedAtMs: 1_000 + sequence
        )
    }

    private func makeStore(
        summaries: [ConversationSummary] = [],
        messages: [String: [ConversationMessage]] = [:],
        pageSize: Int = 50
    ) -> ConversationStore {
        let mock = MockConversationService(summaries: summaries, messages: messages)
        return ConversationStore(service: mock, messagePageSize: pageSize)
    }

    // MARK: - JSON decode parity (wire contract)

    func testSummaryDecodesSnakeCaseWireContract() throws {
        let json = """
        {"schema":"opensks.conversation-summary.v1","id":"c1","project_id":"p1","title":"Hello","title_source":"generated","status":"running","pinned":true,"archived":false,"message_count":3,"created_at_ms":10,"updated_at_ms":20,"last_message_at_ms":25}
        """
        let decoded = try JSONDecoder.opensks.decode(ConversationSummary.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.id, "c1")
        XCTAssertEqual(decoded.projectId, "p1")
        XCTAssertEqual(decoded.titleSource, .generated)
        XCTAssertEqual(decoded.status, .running)
        XCTAssertTrue(decoded.pinned)
        XCTAssertEqual(decoded.messageCount, 3)
        XCTAssertEqual(decoded.lastMessageAtMs, 25)
        XCTAssertEqual(decoded.activityMs, 25)
    }

    func testConversationSummaryDecodesAllRustWireStatuses() throws {
        let statuses: [(String, ConversationStatus, String)] = [
            ("idle", .idle, "Idle"),
            ("queued", .queued, "Queued"),
            ("running", .running, "Running"),
            ("waiting_for_input", .waitingForInput, "Waiting for input"),
            ("waiting_for_approval", .waitingForApproval, "Waiting for approval"),
            ("failed", .failed, "Failed"),
            ("completed", .completed, "Done")
        ]
        for (raw, expected, label) in statuses {
            let json = """
            {"schema":"opensks.conversation-summary.v1","id":"c-\(raw)","project_id":"p1","title":"\(raw)","title_source":"generated","status":"\(raw)","pinned":false,"archived":false,"message_count":0,"created_at_ms":1,"updated_at_ms":1,"last_message_at_ms":null}
            """
            let decoded = try JSONDecoder.opensks.decode(ConversationSummary.self, from: Data(json.utf8))
            XCTAssertEqual(decoded.status, expected)
            XCTAssertEqual(decoded.status.displayLabel, label)

            let encoded = try JSONEncoder.opensks.encode(decoded)
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
            XCTAssertEqual(object["status"] as? String, raw)
        }
    }

    func testHeaderFailureDetailsAreExposedOnlyForFailedSummaryStatus() {
        XCTAssertTrue(shouldExposeConversationHeaderFailureDetails(status: .failed))
        for status in ConversationStatus.allCases where status != .failed {
            XCTAssertFalse(
                shouldExposeConversationHeaderFailureDetails(status: status),
                "\(status.rawValue) should keep its normal header status semantics"
            )
        }
    }

    func testConversationSummaryDecodesAllRustTitleSources() throws {
        let titleSources: [(String, ConversationTitleSource)] = [
            ("generated", .generated),
            ("user", .user),
            ("agent", .agent),
            ("imported", .imported)
        ]
        for (raw, expected) in titleSources {
            let json = """
            {"schema":"opensks.conversation-summary.v1","id":"c-\(raw)","project_id":"p1","title":"\(raw)","title_source":"\(raw)","status":"idle","pinned":false,"archived":false,"message_count":0,"created_at_ms":1,"updated_at_ms":1,"last_message_at_ms":null}
            """
            let decoded = try JSONDecoder.opensks.decode(ConversationSummary.self, from: Data(json.utf8))
            XCTAssertEqual(decoded.titleSource, expected)

            let encoded = try JSONEncoder.opensks.encode(decoded)
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
            XCTAssertEqual(object["title_source"] as? String, raw)
        }
    }

    func testUnknownEnumValuesFallBackInsteadOfThrowing() throws {
        let json = """
        {"schema":"s","id":"m1","project_id":"p1","conversation_id":"c1","turn_id":null,"role":"future_role","state":"buffering","content_redacted":"hi","sequence":1,"created_at_ms":1,"updated_at_ms":1}
        """
        let decoded = try JSONDecoder.opensks.decode(ConversationMessage.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.role, .unknown)
        XCTAssertEqual(decoded.state, .unknown)
    }

    func testConversationMessageDecodesAllRustWireStates() throws {
        let states: [(String, MessageState)] = [
            ("draft", .draft),
            ("queued", .queued),
            ("streaming", .streaming),
            ("complete", .complete),
            ("failed", .failed),
            ("cancelled", .cancelled)
        ]
        for (raw, expected) in states {
            let json = """
            {"schema":"opensks.conversation-message.v1","id":"m-\(raw)","project_id":"p1","conversation_id":"c1","turn_id":"t1","role":"assistant","state":"\(raw)","content_redacted":"\(raw)","sequence":1,"created_at_ms":1,"updated_at_ms":1}
            """
            let decoded = try JSONDecoder.opensks.decode(ConversationMessage.self, from: Data(json.utf8))
            XCTAssertEqual(decoded.state, expected)

            let encoded = try JSONEncoder.opensks.encode(decoded)
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
            XCTAssertEqual(object["state"] as? String, raw)
        }
    }

    func testTimelineDecodesSnakeCaseWireContract() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-m1","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2000000,"kind":"assistant_message","state":"completed","payload":{"message_id":"m1","role":"assistant","message_state":"complete","content_redacted":"done","run_relation":"primary"},"created_at_ms":10,"updated_at_ms":20},{"schema":"opensks.timeline-item.v1","id":"timeline-event-e1","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2000002,"kind":"error","state":"verification_failed","payload":{"event_id":"e1","event_kind":"verification_failed","event_sequence":2,"content_redacted":"Needs setup","payload_redacted":{"code":"setup_required"},"projection":"event_journal_replay"},"created_at_ms":11,"updated_at_ms":11}]}
        """
        let timeline = try JSONDecoder.opensks.decode(ConversationTimeline.self, from: Data(json.utf8))
        XCTAssertEqual(timeline.schema, "opensks.conversation-timeline.v1")
        XCTAssertEqual(timeline.conversationId, "c1")
        let item = try XCTUnwrap(timeline.items.first)
        XCTAssertEqual(item.kind, .assistantMessage)
        XCTAssertEqual(item.runId, "r1")
        XCTAssertEqual(item.payload.messageId, "m1")
        XCTAssertEqual(item.payload.contentRedacted, "done")
        XCTAssertEqual(item.message?.role, .assistant)
        let event = try XCTUnwrap(timeline.items.last)
        XCTAssertEqual(event.kind, .error)
        XCTAssertNil(event.message)
        XCTAssertEqual(event.payload.contentRedacted, "Needs setup")
    }

    func testPendingSendBubblesAppendAfterExistingDurableTimeline() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-old-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"old prompt"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-old-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"old answer"},"created_at_ms":11,"updated_at_ms":11}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items
        let pending = ConversationPendingSend(
            projectId: "mock-project",
            text: "new prompt",
            createdAtMs: 99,
            token: "pending-token"
        )

        let pendingMessages = localPendingConversationMessages(pending, conversationID: "c1")
        XCTAssertEqual(pendingMessages.map(\.contentRedacted), ["new prompt", "Thinking..."])
        XCTAssertEqual(pendingMessages.map(\.role), [.user, .assistant])

        let appended = appendedLocalPendingMessagesForRenderedTimeline(
            mainThreadTimelineItems(timeline),
            pendingMessages: pendingMessages
        )
        XCTAssertEqual(appended.map(\.contentRedacted), ["new prompt", "Thinking..."])
    }

    func testMainThreadTimelineOrdersOldestAtTopEvenWhenServiceReturnsNewestFirst() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-new-user","project_id":"p1","conversation_id":"c1","turn_id":"t2","run_id":"r2","sequence":3,"kind":"user_message","state":"complete","payload":{"message_id":"u2","role":"user","message_state":"complete","content_redacted":"new prompt"},"created_at_ms":30,"updated_at_ms":30},{"schema":"opensks.timeline-item.v1","id":"timeline-old-error","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2000001,"kind":"error","state":"verification_failed","payload":{"content_redacted":"old run failed"},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-old-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"old prompt"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-old-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"assistant_message","state":"failed","payload":{"message_id":"a1","role":"assistant","message_state":"failed","content_redacted":"old answer"},"created_at_ms":11,"updated_at_ms":11}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-old-user", "timeline-old-assistant", "timeline-old-error", "timeline-new-user"]
        )
    }

    func testBudgetExhaustionTextIsDisplayedAsFailureSummaryNotRawSentinel() {
        let message = ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: "a1",
            projectId: "p1",
            conversationId: "c1",
            turnId: "t1",
            role: .assistant,
            state: .failed,
            contentRedacted: "Stopped after the 16-step budget without a final answer.",
            sequence: 2,
            createdAtMs: 20,
            updatedAtMs: 20
        )

        XCTAssertEqual(
            messageCellDisplayText(message),
            "The agent exhausted its step budget before producing a final answer."
        )
        XCTAssertEqual(
            normalizedRunFailureText(message.contentRedacted),
            "The agent exhausted its step budget before producing a final answer."
        )
    }

    func testWorkerTimelineItemsMoveOutOfMainConversationRail() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"fix the layout"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-worker","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"worker","state":"running","payload":{"worker_id":"worker-1","role_label":"layout reviewer","content_redacted":"checking the sidebar layout"},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-tool","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"tool_call","state":"completed","payload":{"worker_id":"worker-1","tool":"workspace.search_text","content_redacted":"searched layout files"},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"I fixed the layout."},"created_at_ms":13,"updated_at_ms":13},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant-event","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":5,"kind":"assistant_message","state":"completed","payload":{"assistant_message_id":"a1","agent_event_kind":"assistant_text_completed","worker_id":"worker-1","assistant_text":"I fixed the layout.","content_redacted":"I fixed the layout."},"created_at_ms":14,"updated_at_ms":14},{"schema":"opensks.timeline-item.v1","id":"timeline-error","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":6,"kind":"error","state":"failed","payload":{"content_redacted":"A visible failure reason."},"created_at_ms":15,"updated_at_ms":15}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-assistant", "timeline-error"]
        )
        XCTAssertEqual(
            workerRailTimelineItems(timeline).map(\.id),
            ["timeline-assistant-event"]
        )
        let rail = WorkerRailView(items: workerRailTimelineItems(timeline))
            .frame(width: 304, height: 360)
        XCTAssertNotNil(ImageRenderer(content: rail).nsImage, "worker rail should render collapsed worker summaries")
        XCTAssertTrue(isActiveWorkerItem(timeline[1]))
        XCTAssertFalse(isActiveWorkerItem(timeline[2]))
    }

    func testWorkerScopedWarningsMoveOutOfMainConversationRail() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"fix the UI"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-worker-warning","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"warning","state":"work_item_running","payload":{"event_kind":"work_item_running","worker_id":"agentic","tool":"workspace.list_directory","content_redacted":"warning"},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-user-warning","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"warning","state":"warning","payload":{"content_redacted":"A main-thread warning."},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"Done."},"created_at_ms":13,"updated_at_ms":13}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-user-warning", "timeline-assistant"]
        )
        XCTAssertEqual(workerRailTimelineItems(timeline).map(\.id), ["timeline-worker-warning"])
    }

    func testWorkerScopedErrorsMoveOutOfMainConversationRail() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"hi"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"안녕하세요. 무엇을 도와드릴까요?"},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-worker-error","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"error","state":"verification_failed","payload":{"worker_id":"turn-supervisor","role_label":"verification","content_redacted":"role worker model call failed","code":"role_worker_model_call_failed"},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-main-error","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"error","state":"verification_failed","payload":{"content_redacted":"A main run failure."},"created_at_ms":13,"updated_at_ms":13}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-assistant", "timeline-main-error"]
        )
        XCTAssertEqual(workerRailTimelineItems(timeline).map(\.id), ["timeline-worker-error"])
        XCTAssertEqual(
            latestAssistantReplyMessage(in: mainThreadTimelineItems(timeline))?.contentRedacted,
            "안녕하세요. 무엇을 도와드릴까요?"
        )
    }

    func testLatestErrorTextByRunIDUsesRunScopedErrorText() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"assistant_message","state":"failed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"What would you like me to do?"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-error-old","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"error","state":"verification_failed","payload":{"content_redacted":"old provider error"},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-error-new","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"error","state":"verification_failed","payload":{"content_redacted":"The model call failed: provider response had no message content"},"created_at_ms":12,"updated_at_ms":12}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            latestErrorTextByRunID(in: timeline)["r1"],
            "The model call failed: provider response had no message content"
        )
    }

    func testRunFailureSummaryPrefersRunErrorOverAssistantText() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"assistant_message","state":"failed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"What would you like me to do with these lines?"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-error","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"error","state":"verification_failed","payload":{"content_redacted":"The model call failed: provider call failed: provider response had no message content"},"created_at_ms":11,"updated_at_ms":11}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            runFailureSummary(
                messageContent: "What would you like me to do with these lines?",
                evidence: timeline
            ),
            "The model call failed: provider call failed: provider response had no message content"
        )
    }

    func testRunFailureDiagnosticsPopoverRendersLongScrollableDiagnostics() {
        let run = ConversationRunRef(
            turnId: "turn-long-diagnostics",
            runId: "run-long-diagnostics",
            messageId: "assistant-long-diagnostics",
            relation: "primary",
            runState: .failed
        )
        let diagnostics = RunFailureDiagnostics(
            run: run,
            summary: "The model call failed: provider call failed: provider response had no message content",
            details: (0..<36).map { index in
                RunFailureDetail(
                    label: "Signal \(index)",
                    value: "Long diagnostic row \(index) with enough text to wrap inside the scrollable failure details panel."
                )
            },
            recoveryHints: [
                "Retry on the current app build; if this repeats, inspect the provider response shape and model selection first.",
                "Use the reason/code signal above as the first fix target."
            ]
        )

        let rendered = RunFailureDiagnosticsPopover(
            diagnostics: diagnostics,
            shortRunID: "run-long"
        )
        .frame(width: 560, height: 620)

        XCTAssertNotNil(
            ImageRenderer(content: rendered).nsImage,
            "long failure diagnostics should render in a bounded scrollable panel"
        )
    }

    func testMainWorkspacePatchRendersAsCodeChangeCardInsteadOfWorkerLog() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"make two tiny edits"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-isolated-patch","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"patch","state":"completed","payload":{"agent_event_kind":"file_patch_applied","worker_id":"worker-a","content_redacted":"isolated worker patch","applied_files":["scratch-a.md"],"patch_count":1,"apply_result_count":1,"main_workspace_modified":false},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-main-patch","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"patch","state":"completed","payload":{"agent_event_kind":"file_patch_applied","content_redacted":"Applied integrated candidate to the main workspace.","applied_files":["src/lib.rs","README.md"],"patch_count":2,"apply_result_count":2,"main_workspace_modified":true,"integration_final_diff_hash":"fnv1a64:abcdef1234567890","integration_final_diff_ref":"artifact://.opensks/runtime/integration-candidates/r1/final.diff","receipt_ref":"artifact://receipt.json"},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"Done."},"created_at_ms":13,"updated_at_ms":13}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-main-patch", "timeline-assistant"]
        )
        XCTAssertEqual(
            workerRailTimelineItems(timeline).map(\.id),
            ["timeline-isolated-patch"]
        )
        let mainPatch = try XCTUnwrap(timeline.first { $0.id == "timeline-main-patch" })
        let card = try XCTUnwrap(CodeChangeSummaryCardModel(item: mainPatch))
        XCTAssertEqual(card.title, "Edited 2 files")
        XCTAssertEqual(card.files, ["src/lib.rs", "README.md"])
        XCTAssertTrue(card.diffEvidence?.contains("abcdef123456") ?? false)
        let rendered = CodeChangeSummaryCard(card: card).frame(width: 720, height: 240)
        XCTAssertNotNil(ImageRenderer(content: rendered).nsImage, "main workspace patch should render a code change card")
    }

    func testPendingIntegrationCandidateRendersMainApplyCard() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"create the smoke file"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-pending-integration","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"worker","state":"completed","payload":{"code":"integration_candidate_ready","reason_code":"aggregate_isolated_patch_candidate_ready","main_workspace_modified":false,"approval_required":true,"receipt_ref":"artifact://.opensks/runtime/integration-candidates/r1/candidate.json","patch_ref":"artifact://.opensks/runtime/integration-candidates/r1/candidate.patch","target_paths":["SMOKE.txt"]},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"Integration is pending approval."},"created_at_ms":13,"updated_at_ms":13}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-pending-integration", "timeline-assistant"]
        )
        XCTAssertTrue(workerRailTimelineItems(timeline).isEmpty)
        let pending = try XCTUnwrap(timeline.first { $0.id == "timeline-pending-integration" })
        let card = try XCTUnwrap(CodeChangeSummaryCardModel(item: pending))
        XCTAssertEqual(card.title, "Code changes ready")
        XCTAssertTrue(card.canApplyIntegration)
        XCTAssertEqual(card.runID, "r1")
        XCTAssertEqual(card.files, ["SMOKE.txt"])
        let rendered = CodeChangeSummaryCard(card: card) { _ in }
            .frame(width: 720, height: 260)
        XCTAssertNotNil(ImageRenderer(content: rendered).nsImage, "pending candidate should render with apply affordance")
    }

    func testIntegratedApplySupersedesPendingIntegrationFailure() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"create the smoke file"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-pending-integration","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"worker","state":"completed","payload":{"code":"integration_candidate_ready","reason_code":"aggregate_isolated_patch_candidate_ready","main_workspace_modified":false,"approval_required":true,"receipt_ref":"artifact://.opensks/runtime/integration-candidates/r1/candidate.json","patch_ref":"artifact://.opensks/runtime/integration-candidates/r1/candidate.patch","target_paths":["SMOKE.txt"]},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-failed-integration","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"error","state":"verification_failed","payload":{"event_kind":"verification_failed","reason_code":"git_apply_failed","content_redacted":"verification failed","repair_ref":"artifact://.opensks/runtime/integration-candidates/r1/repair.json"},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-integrated-apply-old","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":4,"kind":"worker","state":"completed","payload":{"event_kind":"work_item_completed","reason_code":"candidate_applied_to_main_workspace","main_workspace_modified":true,"verifier_passed":true,"target_paths":["SMOKE.txt"],"final_diff_ref":"artifact://.opensks/runtime/integration-candidates/r1/final.diff","receipt_ref":"artifact://.opensks/runtime/integration-candidates/r1/integration.json"},"created_at_ms":13,"updated_at_ms":13},{"schema":"opensks.timeline-item.v1","id":"timeline-integrated-apply","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":5,"kind":"worker","state":"completed","payload":{"event_kind":"work_item_completed","reason_code":"candidate_already_applied_to_main_workspace","main_workspace_modified":true,"verifier_passed":true,"target_paths":["SMOKE.txt"],"final_diff_ref":"artifact://.opensks/runtime/integration-candidates/r1/final.diff","receipt_ref":"artifact://.opensks/runtime/integration-candidates/r1/integration.json"},"created_at_ms":14,"updated_at_ms":14},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":6,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"Applied."},"created_at_ms":15,"updated_at_ms":15}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-integrated-apply", "timeline-assistant"]
        )
        XCTAssertTrue(workerRailTimelineItems(timeline).isEmpty)
        let integrated = try XCTUnwrap(timeline.first { $0.id == "timeline-integrated-apply" })
        let card = try XCTUnwrap(CodeChangeSummaryCardModel(item: integrated))
        XCTAssertEqual(card.title, "Applied 1 file")
        XCTAssertEqual(card.files, ["SMOKE.txt"])
        XCTAssertFalse(card.canApplyIntegration)
    }

    func testStoreApplyIntegrationCandidateRoutesThroughService() async {
        let service = MockConversationService(
            summaries: [
                summary(id: "c1", title: "Pending integration", status: .completed, messageCount: 1)
            ]
        )
        let store = ConversationStore(service: service)

        await store.load()
        await store.applyIntegrationCandidate(runID: "r1", conversationID: "c1")

        XCTAssertEqual(service.applyIntegrationCandidateRequests, ["r1"])
        XCTAssertFalse(store.isApplyingIntegration(runID: "r1"))
        XCTAssertNil(store.errorMessage)
    }

    func testAgenticPatchApplyWithoutMainWorkspaceFlagStaysInWorkerRail() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"create the two smoke files"},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-agentic-apply","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":8000008,"kind":"patch","state":"work_item_completed","payload":{"agent_event_kind":"file_patch_applied","worker_id":"agentic","content_redacted":"file_patch_applied","applied_files":["PARALLEL_SMOKE_A.txt","PARALLEL_SMOKE_B.txt"],"reason_code":"applied"},"created_at_ms":12,"updated_at_ms":12},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":8000016,"kind":"assistant_message","state":"completed","payload":{"message_id":"a1","role":"assistant","message_state":"complete","content_redacted":"Verified both files contain the requested content."},"created_at_ms":13,"updated_at_ms":13}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-assistant"]
        )
        XCTAssertEqual(workerRailTimelineItems(timeline).map(\.id), ["timeline-agentic-apply"])
        let patch = try XCTUnwrap(timeline.first { $0.id == "timeline-agentic-apply" })
        XCTAssertNil(CodeChangeSummaryCardModel(item: patch))
    }

    func testMainThreadPrefersDurableAssistantMessageOverSupersededRawAssistantEvent() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"Title smoke: provider routing works."},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-assistant","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"assistant_message","state":"failed","payload":{"message_id":"a1","role":"assistant","message_state":"failed","content_redacted":"Prepared isolated change candidate for src/tests.rs from 1 source candidate(s)."},"created_at_ms":11,"updated_at_ms":30},{"schema":"opensks.timeline-item.v1","id":"timeline-agentic-raw-final","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":3,"kind":"assistant_message","state":"completed","payload":{"assistant_message_id":"a1","agent_event_kind":"assistant_text_completed","worker_id":"agentic","assistant_text":"Stopped after the 16-step budget without a final answer.","content_redacted":"Stopped after the 16-step budget without a final answer."},"created_at_ms":12,"updated_at_ms":12}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-assistant"]
        )
        XCTAssertEqual(
            latestAssistantReplyMessage(in: mainThreadTimelineItems(timeline))?.contentRedacted,
            "Prepared isolated change candidate for src/tests.rs from 1 source candidate(s)."
        )
        let reply = try XCTUnwrap(latestAssistantReplyMessage(in: mainThreadTimelineItems(timeline)))
        XCTAssertFalse(shouldRenderPinnedAssistantReply(reply, in: mainThreadTimelineItems(timeline)))
    }

    func testMainThreadUsesLoadedDurableAssistantMessageWhenTimelineOnlyHasRawEvent() throws {
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[{"schema":"opensks.timeline-item.v1","id":"timeline-user","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":1,"kind":"user_message","state":"complete","payload":{"message_id":"u1","role":"user","message_state":"complete","content_redacted":"Title smoke: provider routing works."},"created_at_ms":10,"updated_at_ms":10},{"schema":"opensks.timeline-item.v1","id":"timeline-error","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":2,"kind":"error","state":"verification_failed","payload":{"content_redacted":"TurnSupervisor failed.","code":"turn_supervisor_failed"},"created_at_ms":11,"updated_at_ms":11},{"schema":"opensks.timeline-item.v1","id":"timeline-agentic-raw-final","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":8000079,"kind":"assistant_message","state":"completed","payload":{"assistant_message_id":"a1","agent_event_kind":"assistant_text_completed","worker_id":"agentic","assistant_text":"Stopped after the 16-step budget without a final answer.","content_redacted":"Stopped after the 16-step budget without a final answer.","projection":"assistant_execution_event"},"created_at_ms":999999,"updated_at_ms":999999}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items
        let durable = message(
            id: "a1",
            conversation: "c1",
            role: .assistant,
            text: "Prepared isolated change candidate for src/tests.rs from 1 source candidate(s).",
            sequence: 4
        )

        XCTAssertEqual(
            mainThreadTimelineItems(timeline, additionalDurableAssistantMessageIDs: ["a1"]).map(\.id),
            ["timeline-user", "timeline-error"]
        )
        XCTAssertEqual(
            mainThreadTimelineItems(timeline).map(\.id),
            ["timeline-user", "timeline-error"]
        )
        XCTAssertEqual(
            latestAssistantReplyMessage(in: timeline, durableMessages: [durable])?.contentRedacted,
            "Prepared isolated change candidate for src/tests.rs from 1 source candidate(s)."
        )
        let reply = try XCTUnwrap(latestAssistantReplyMessage(in: timeline, durableMessages: [durable]))
        XCTAssertTrue(shouldRenderPinnedAssistantReply(reply, in: mainThreadTimelineItems(timeline)))
    }

    func testWorkerRailCompressesRepeatedToolLogsIntoOneDialogueLane() throws {
        let repeatedToolEvents = (1...20)
            .map { index in
                """
                {"schema":"opensks.timeline-item.v1","id":"tool-\(index)","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":\(index),"kind":"tool_call","state":"tool_call_completed","payload":{"worker_id":"agentic","role_label":"Agentic","tool":"workspace.read_file_range","content_redacted":"read chunk \(index)"},"created_at_ms":\(10 + index),"updated_at_ms":\(10 + index)}
                """
            }
            .joined(separator: ",")
        let json = """
        {"schema":"opensks.conversation-timeline.v1","conversation_id":"c1","items":[\(repeatedToolEvents),{"schema":"opensks.timeline-item.v1","id":"worker-dialogue","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":21,"kind":"assistant_message","state":"completed","payload":{"assistant_message_id":"a1","agent_event_kind":"assistant_text_completed","worker_id":"agentic","role_label":"Agentic","assistant_text":"I found the UI loop and reduced the worker rail noise.","content_redacted":"I found the UI loop and reduced the worker rail noise."},"created_at_ms":31,"updated_at_ms":31},{"schema":"opensks.timeline-item.v1","id":"tool-after-dialogue","project_id":"p1","conversation_id":"c1","turn_id":"t1","run_id":"r1","sequence":22,"kind":"tool_call","state":"tool_call_completed","payload":{"worker_id":"agentic","role_label":"Agentic","tool":"git.status","content_redacted":"status checked"},"created_at_ms":32,"updated_at_ms":32}]}
        """
        let timeline = try JSONDecoder.opensks
            .decode(ConversationTimeline.self, from: Data(json.utf8))
            .items

        XCTAssertEqual(workerRailTimelineItems(timeline).map(\.id), ["worker-dialogue"])
    }

    func testThreadSettingsDecodesCliModelSelectionWithoutFallbackIds() throws {
        let json = """
        {"schema":"opensks.thread-settings.v1","conversation_id":"c1","model_selection":{"mode":"pinned","model_id":"provider-1/code-model"},"reasoning_effort":"standard","execution_mode":"worktree","pipeline_id":"auto","max_parallelism":4,"verifier_count":1,"tool_policy_id":"project-default","approval_policy_id":"safe-interactive","updated_at_ms":10}
        """
        let decoded = try JSONDecoder.opensks.decode(ConversationThreadSettings.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.modelSelection.mode, .pinned)
        XCTAssertEqual(decoded.modelSelection.modelId, "provider-1/code-model")
        XCTAssertEqual(decoded.modelSelection.fallbackModelIds, [])
    }

    func testMessagePageDecodesHasMore() throws {
        let json = """
        {"conversation_id":"c1","messages":[],"has_more":true}
        """
        let decoded = try JSONDecoder.opensks.decode(MessagePage.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.conversationId, "c1")
        XCTAssertTrue(decoded.hasMore)
    }

    // MARK: - Store lifecycle

    func testCreateSelectsNewConversationAndAppendsToList() async {
        let store = makeStore()
        await store.load()
        XCTAssertTrue(store.summaries.isEmpty)

        await store.create(title: "First thread")

        XCTAssertEqual(store.summaries.count, 1)
        XCTAssertEqual(store.selectedConversationID, store.summaries.first?.id)
        XCTAssertEqual(store.selectedSummary?.title, "First thread")
    }

    func testLoadAutoSelectsFirstConversation() async {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Alpha", activityMs: 2_000),
            summary(id: "b", title: "Beta", activityMs: 1_000)
        ])
        await store.load()
        // Newest activity sorts first.
        XCTAssertEqual(store.selectedConversationID, "a")
    }

    func testRenameUpdatesTitle() async {
        let store = makeStore(summaries: [summary(id: "a", title: "Old")])
        await store.load()

        await store.rename("a", to: "Renamed")

        XCTAssertEqual(store.summaries.first(where: { $0.id == "a" })?.title, "Renamed")
    }

    func testTogglePinnedReordersList() async {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Alpha", activityMs: 2_000),
            summary(id: "b", title: "Beta", activityMs: 1_000)
        ])
        await store.load()

        await store.togglePinned("b")

        XCTAssertTrue(store.summaries.first(where: { $0.id == "b" })?.pinned == true)
        // Pinned sorts to the top.
        XCTAssertEqual(store.summaries.first?.id, "b")
    }

    func testArchiveRemovesFromAllFilterButShowsInArchivedFilter() async {
        let store = makeStore(summaries: [summary(id: "a", title: "Alpha")])
        await store.load()
        XCTAssertEqual(store.summaries.count, 1)

        await store.archive("a")
        XCTAssertTrue(store.summaries.isEmpty, "archived conversation leaves the default 'all' filter")

        await store.applyFilter(.archived)
        XCTAssertEqual(store.summaries.map(\.id), ["a"])
    }

    func testDeleteRemovesConversationAndClearsSelection() async {
        let store = makeStore(summaries: [summary(id: "a", title: "Alpha")])
        await store.load()
        XCTAssertEqual(store.selectedConversationID, "a")

        await store.delete("a")

        XCTAssertTrue(store.summaries.isEmpty)
        XCTAssertNil(store.selectedConversationID)
        XCTAssertTrue(store.messages.isEmpty)
    }

    // MARK: - Filtering

    func testRunningFilterOnlyKeepsRunningConversations() async {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Idle one", status: .idle),
            summary(id: "b", title: "Running one", status: .running)
        ])
        await store.applyFilter(.running)
        XCTAssertEqual(store.summaries.map(\.id), ["b"])
    }

    func testSearchTextFiltersVisibleSummaries() async {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Refactor parser"),
            summary(id: "b", title: "Design tokens")
        ])
        await store.load()

        store.searchText = "design"
        XCTAssertEqual(store.visibleSummaries.map(\.id), ["b"])

        store.searchText = ""
        XCTAssertEqual(Set(store.visibleSummaries.map(\.id)), ["a", "b"])
    }

    // MARK: - Drafts

    func testDraftsPersistPerConversationIndependently() async {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Alpha"),
            summary(id: "b", title: "Beta")
        ])
        await store.load()

        store.setDraft("draft for A", for: "a")
        store.setDraft("draft for B", for: "b")

        XCTAssertEqual(store.draft(for: "a"), "draft for A")
        XCTAssertEqual(store.draft(for: "b"), "draft for B")
        XCTAssertEqual(store.draft(for: "missing"), "")
    }

    func testDeletingConversationClearsItsDraft() async {
        let store = makeStore(summaries: [summary(id: "a", title: "Alpha")])
        await store.load()
        store.setDraft("typed text", for: "a")

        await store.delete("a")

        XCTAssertEqual(store.draft(for: "a"), "")
    }

    // MARK: - Thread settings

    func testThreadSettingsPersistAndReloadThroughStore() async {
        let mock = MockConversationService(summaries: [summary(id: "a", title: "Alpha")])
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()

        XCTAssertEqual(store.threadSettings(for: "a").executionMode, .worktree)
        XCTAssertEqual(store.threadSettings(for: "a").reasoningEffort, .standard)

        await store.updateThreadSettings(for: "a") { settings in
            settings.executionMode = .readOnly
            settings.reasoningEffort = .deep
            settings.pipelineId = "parallel-build"
            settings.maxParallelism = 8
            settings.toolPolicyId = "read-only"
        }

        let relaunched = ConversationStore(service: mock, messagePageSize: 50)
        await relaunched.load()
        let persisted = relaunched.threadSettings(for: "a")
        XCTAssertEqual(persisted.executionMode, .readOnly)
        XCTAssertEqual(persisted.reasoningEffort, .deep)
        XCTAssertEqual(persisted.pipelineId, "parallel-build")
        XCTAssertEqual(persisted.maxParallelism, 8)
        XCTAssertEqual(persisted.toolPolicyId, "read-only")
    }

    func testLoadHydratesThreadSettingsForExistingSelection() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a", title: "Alpha")])
        var pinned = ConversationThreadSettings.defaultFor(conversationID: "a")
        pinned.modelSelection = ModelSelection(
            mode: .pinned,
            modelId: "provider-1/code-model",
            fallbackModelIds: []
        )
        _ = try await mock.setThreadSettings(pinned, conversationID: "a")

        let store = ConversationStore(service: mock, messagePageSize: 50)
        store.selectedConversationID = "a"

        await store.load()

        XCTAssertEqual(store.threadSettings(for: "a").modelSelection.mode, .pinned)
        XCTAssertEqual(store.threadSettings(for: "a").modelSelection.modelId, "provider-1/code-model")
    }

    func testThreadSettingsModelSelectionPublishesComposerRefresh() async {
        let mock = MockConversationService(summaries: [summary(id: "a", title: "Alpha")])
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()

        var publishCount = 0
        let cancellable = store.objectWillChange.sink {
            publishCount += 1
        }

        await store.updateThreadSettings(for: "a") { settings in
            settings.modelSelection = ModelSelection(
                mode: .pinned,
                modelId: "provider-1/code-model",
                fallbackModelIds: []
            )
        }

        withExtendedLifetime(cancellable) {
            XCTAssertGreaterThan(publishCount, 0)
            XCTAssertEqual(store.threadSettings(for: "a").modelSelection.mode, .pinned)
            XCTAssertEqual(store.threadSettings(for: "a").modelSelection.modelId, "provider-1/code-model")
        }
    }

    func testComposerModelSelectionNormalizesProviderAliasBeforePersisting() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a", title: "Alpha")])
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()

        await store.updateThreadSettings(for: "a") { settings in
            settings.modelSelection = ModelSelection(
                mode: .pinned,
                modelId: "codex-lb",
                fallbackModelIds: []
            )
        }

        let providers = ProviderStore(secretStore: InMemoryProviderSecretStore())
        var draft = ProviderDraft(kind: .codexLB)
        draft.displayName = "codex-lb"
        draft.codexLbDomain = "codex.hyper-lab.xyz"
        let providerID = try await providers.connect(draft, credential: SecureCredential(value: "test-secret"))

        let current = store.threadSettings(for: "a").modelSelection
        let normalized = providers.normalizedTextModelSelection(from: current)
        await store.updateThreadSettings(for: "a") { settings in
            settings.modelSelection = normalized
        }

        let persisted = store.threadSettings(for: "a").modelSelection
        XCTAssertEqual(persisted.mode, .pinned)
        XCTAssertEqual(persisted.modelId, "\(providerID)/auto-code")
        XCTAssertEqual(
            persisted.fallbackModelIds,
            [
                "\(providerID)/gpt-5.4-mini",
                "\(providerID)/gpt-5.4-nano",
                "\(providerID)/gpt-5.5"
            ]
        )
        XCTAssertEqual(providers.modelDisplayLabel(for: persisted.modelId ?? ""), "codex-lb / Default code model")
    }

    // MARK: - Pagination

    func testMessagePaginationAppendsOlderPages() async {
        // 3 messages, page size 2: first page is the newest 2 (seq 2,3),
        // loading older prepends seq 1.
        let msgs = [
            message(id: "m1", conversation: "a", role: .user, text: "one", sequence: 1),
            message(id: "m2", conversation: "a", role: .assistant, text: "two", sequence: 2),
            message(id: "m3", conversation: "a", role: .user, text: "three", sequence: 3)
        ]
        let store = makeStore(
            summaries: [summary(id: "a", title: "Alpha", messageCount: 3)],
            messages: ["a": msgs],
            pageSize: 2
        )
        await store.load()

        XCTAssertEqual(store.messages.map(\.sequence), [2, 3])
        XCTAssertEqual(store.timelineItems(for: "a").map(\.sequence), [2, 3])
        XCTAssertTrue(store.hasMoreMessages)

        await store.loadOlderMessages()

        XCTAssertEqual(store.messages.map(\.sequence), [1, 2, 3], "older page prepends oldest->newest")
        XCTAssertEqual(store.timelineItems(for: "a").map(\.sequence), [1, 2, 3])
        XCTAssertFalse(store.hasMoreMessages)
    }

    func testAppendThroughServiceShowsAfterReload() async {
        let mock = MockConversationService(summaries: [summary(id: "a", title: "Alpha")])
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()
        XCTAssertTrue(store.messages.isEmpty)

        _ = try? await mock.append(id: "a", role: .user, text: "seeded message")
        await store.select("a")

        XCTAssertEqual(store.messages.map(\.contentRedacted), ["seeded message"])
        XCTAssertEqual(store.timelineItems(for: "a").map { $0.payload.contentRedacted ?? "" }, ["seeded message"])
    }

    // MARK: - Render smoke tests

    func testConversationSidebarRendersAtFixedSize() throws {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Alpha", status: .running, messageCount: 4, activityMs: 2_000),
            summary(id: "b", title: "Beta", pinned: true, activityMs: 1_000)
        ])
        let sidebar = ConversationSidebar(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore()),
            projectName: "OpenSKS"
        )
            .frame(width: 280, height: 600)
        let renderer = ImageRenderer(content: sidebar)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation sidebar must render non-nil")
    }

    func testConversationSidebarRendersProviderSettingsEntry() async throws {
        let store = makeStore(summaries: [
            summary(id: "a", title: "Alpha", status: .running, messageCount: 4, activityMs: 2_000)
        ])
        let providers = ProviderStore(secretStore: InMemoryProviderSecretStore())
        var draft = ProviderDraft(kind: .codexLB)
        draft.displayName = "codex-lb"
        draft.codexLbDomain = "codex.hyper-lab.xyz"
        _ = try await providers.connect(draft, credential: SecureCredential(value: "test-secret"))

        let sidebar = ConversationSidebar(
            store: store,
            providers: providers,
            projectName: "OpenSKS",
            onOpenProviderSettings: {}
        )
            .frame(width: 280, height: 600)
        let renderer = ImageRenderer(content: sidebar)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation sidebar should render the compact provider settings entry")
    }

    func testConversationRowRendersInlinePinControl() throws {
        let row = ConversationRow(
            summary: summary(id: "pinned", title: "Pinned design review", pinned: true, messageCount: 2),
            isSelected: true,
            onTogglePinned: {},
            onSelect: {}
        )
        .frame(width: 280, height: 64)

        let renderer = ImageRenderer(content: row)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation rows should render the inline pin control")
    }

    func testConversationSidebarRendersPinLimitErrorWithExistingRows() async {
        let pinned = (1...5).map { index in
            summary(id: "p\(index)", title: "Pinned \(index)", pinned: true, activityMs: Int64(2_000 - index))
        }
        let store = makeStore(summaries: pinned + [
            summary(id: "target", title: "Target", activityMs: 500)
        ])
        await store.load()

        await store.togglePinned("target")

        XCTAssertEqual(
            store.errorMessage,
            "You can pin up to 5 conversations. Unpin one before pinning another."
        )
        let sidebar = ConversationSidebar(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore()),
            projectName: "OpenSKS"
        )
            .frame(width: 280, height: 600)
        let renderer = ImageRenderer(content: sidebar)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation sidebar should render cap error above populated rows")
    }

    func testConversationThreadViewRendersAtFixedSize() throws {
        let msgs = [
            message(id: "m1", conversation: "a", role: .user, text: "Hello there", sequence: 1),
            message(id: "m2", conversation: "a", role: .assistant, text: "Hi! How can I help?", sequence: 2)
        ]
        let store = makeStore(
            summaries: [summary(id: "a", title: "Alpha", messageCount: 2)],
            messages: ["a": msgs]
        )
        store.selectedConversationID = "a"
        let thread = ConversationThreadView(store: store, providers: ProviderStore(secretStore: InMemoryProviderSecretStore()))
            .frame(width: 720, height: 600)
        let renderer = ImageRenderer(content: thread)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation thread view must render non-nil")
    }

    func testConversationThreadShowsPendingThinkingBubbleWhileTurnStartIsDelayed() async throws {
        let mock = MockConversationService(
            summaries: [summary(id: "a", title: "Pending send", messageCount: 0)],
            turnStartDelayNanoseconds: 300_000_000
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()
        await store.select("a")

        let sendTask = Task {
            await store.send(conversationID: "a", text: "create a tiny file")
        }
        try await Task.sleep(nanoseconds: 60_000_000)

        let pending = try XCTUnwrap(store.localPendingSend(for: "a"))
        XCTAssertEqual(pending.text, "create a tiny file")
        XCTAssertTrue(store.timelineItems(for: "a").isEmpty)

        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore())
        )
            .frame(width: 720, height: 600)
        let renderer = ImageRenderer(content: thread)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "pending send should render user + Thinking... bubbles")

        await sendTask.value
        XCTAssertNil(store.localPendingSend(for: "a"))
    }

    func testConversationThreadShowsPendingThinkingBubbleAfterExistingTimeline() async throws {
        let existing = [
            message(id: "m1", conversation: "a", role: .user, text: "old prompt", sequence: 1),
            message(id: "m2", conversation: "a", role: .assistant, text: "old answer", sequence: 2)
        ]
        let mock = MockConversationService(
            summaries: [summary(id: "a", title: "Pending after history", messageCount: existing.count)],
            messages: ["a": existing],
            turnStartDelayNanoseconds: 300_000_000
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()
        await store.select("a")

        XCTAssertFalse(store.timelineItems(for: "a").isEmpty)

        let sendTask = Task {
            await store.send(conversationID: "a", text: "new prompt")
        }
        try await Task.sleep(nanoseconds: 60_000_000)

        let pending = try XCTUnwrap(store.localPendingSend(for: "a"))
        XCTAssertEqual(pending.text, "new prompt")
        XCTAssertFalse(store.timelineItems(for: "a").isEmpty)

        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore())
        )
            .frame(width: 720, height: 600)
        let renderer = ImageRenderer(content: thread)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "pending send should render after existing history")

        await sendTask.value
        XCTAssertNil(store.localPendingSend(for: "a"))
    }

    func testStreamingAssistantPlaceholderDisplaysThinkingInsteadOfEllipsis() {
        let streaming = ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: "assistant-thinking",
            projectId: "mock-project",
            conversationId: "a",
            turnId: nil,
            role: .assistant,
            state: .streaming,
            contentRedacted: "...",
            sequence: 1,
            createdAtMs: 1,
            updatedAtMs: 1
        )
        let completed = ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: "assistant-complete",
            projectId: "mock-project",
            conversationId: "a",
            turnId: nil,
            role: .assistant,
            state: .complete,
            contentRedacted: "...",
            sequence: 2,
            createdAtMs: 2,
            updatedAtMs: 2
        )

        XCTAssertEqual(messageCellDisplayText(streaming), "Thinking...")
        XCTAssertEqual(messageCellDisplayText(completed), "...")
    }

    func testFailedConversationHeaderRendersFailureDiagnosticsEntryPoint() async throws {
        let mock = MockConversationService(
            summaries: [summary(id: "a", title: "Failure details", status: .idle)],
            runStateOnTurn: .failed
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()

        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "trigger a failed run",
            settings: nil,
            threadSettingsUpdatedAtMs: nil,
            context: .empty,
            idempotencyKey: "failed-header-render"
        )
        _ = try await mock.supervisorTick()
        await store.load()
        await store.select("a")

        XCTAssertEqual(store.selectedSummary?.status, .failed)
        XCTAssertEqual(store.runs(for: "a").first?.runState, .failed)

        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore())
        )
            .frame(width: 860, height: 600)
        let renderer = ImageRenderer(content: thread)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "failed header should render its diagnostics entry point")
    }

    func testFailedConversationHeaderRendersTimelineOnlyFailureDiagnosticsEntryPoint() async throws {
        let mock = MockConversationService(
            summaries: [summary(id: "a", title: "Timeline-only failure", status: .failed)]
        )
        _ = try await mock.appendTimelineItem(
            conversationID: "a",
            kind: .error,
            state: "verification_failed",
            payloadJSON: """
            {"content_redacted":"TurnSupervisor failed.","code":"turn_supervisor_failed"}
            """
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)
        await store.load()
        await store.select("a")

        XCTAssertEqual(store.selectedSummary?.status, .failed)
        XCTAssertTrue(store.runs(for: "a").isEmpty)

        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore())
        )
            .frame(width: 860, height: 600)
        let renderer = ImageRenderer(content: thread)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "failed header should render details even without a run-list ref")
    }

    func testEmptyThreadRendersEmptyState() throws {
        let store = makeStore()
        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore())
        )
            .frame(width: 720, height: 600)
        XCTAssertNotNil(ImageRenderer(content: thread).nsImage)
    }

    // MARK: - UX-101: compact top context bar

    /// The git context bar label is honest: a real branch name, an explicit
    /// "detached HEAD", or "no branch" — never a fabricated value.
    func testChatGitContextBranchLabelIsHonest() {
        XCTAssertEqual(
            ChatGitContext(inRepo: true, branch: "main", detached: false, changedCount: 3, branchNames: ["main"]).branchLabel,
            "main"
        )
        XCTAssertEqual(
            ChatGitContext(inRepo: true, branch: nil, detached: true, changedCount: 0, branchNames: []).branchLabel,
            "detached HEAD"
        )
        XCTAssertEqual(
            ChatGitContext(inRepo: true, branch: nil, detached: false, changedCount: 0, branchNames: []).branchLabel,
            "no branch"
        )
        XCTAssertEqual(ChatGitContext.none.inRepo, false)
    }

    /// The thread renders its compact context bar with real git context (the bar is
    /// shown only inside a repo) without a letterbox at a supported width.
    func testThreadRendersWithGitContextBar() throws {
        let store = makeStore(
            summaries: [summary(id: "a", title: "Alpha", status: .running, messageCount: 1)],
            messages: ["a": [message(id: "m1", conversation: "a", role: .user, text: "hi", sequence: 1)]]
        )
        store.selectedConversationID = "a"
        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore()),
            gitContext: ChatGitContext(
                inRepo: true,
                branch: "main",
                detached: false,
                changedCount: 3,
                branchNames: ["main", "feature/x"]
            )
        )
        .frame(width: 900, height: 600)
        let renderer = ImageRenderer(content: thread)
        renderer.scale = 1
        let image = try XCTUnwrap(renderer.nsImage, "thread with a git context bar must render")
        XCTAssertEqual(image.size.width, 900, accuracy: 1.0, "no letterbox")
    }
}
