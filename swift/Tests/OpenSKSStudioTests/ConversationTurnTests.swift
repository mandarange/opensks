// ConversationTurnTests.swift — PR-027. Exercises the conversation send path
// through `MockConversationService`: one Send persists the user message + an
// assistant turn and records ONE run; the store-level send is idempotent in
// effect (the mock de-dups a replayed idempotency key, so the run list never
// grows on a reused turn); and the new UI (RunCard + ConversationComposer)
// renders non-nil.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class ConversationTurnTests: XCTestCase {
    // MARK: - Fixtures

    private func summary(
        id: String,
        title: String = "Thread",
        messageCount: Int = 0,
        activityMs: Int64 = 1_000
    ) -> ConversationSummary {
        ConversationSummary(
            schema: "opensks.conversation-summary.v1",
            id: id,
            projectId: "mock-project",
            title: title,
            titleSource: "manual",
            status: .idle,
            pinned: false,
            archived: false,
            messageCount: messageCount,
            createdAtMs: activityMs,
            updatedAtMs: activityMs,
            lastMessageAtMs: messageCount > 0 ? activityMs : nil
        )
    }

    private func runRef(
        turn: String = "turn-1",
        run: String = "run-1",
        message: String = "msg-1",
        relation: String = "primary",
        state: RunState = .completed
    ) -> ConversationRunRef {
        ConversationRunRef(
            turnId: turn,
            runId: run,
            messageId: message,
            relation: relation,
            runState: state
        )
    }

    private func makeStore(
        summaries: [ConversationSummary],
        runStateOnTurn: RunState = .completed
    ) -> (ConversationStore, MockConversationService) {
        let mock = MockConversationService(summaries: summaries, runStateOnTurn: runStateOnTurn)
        return (ConversationStore(service: mock, messagePageSize: 50), mock)
    }

    // MARK: - Wire contract decode parity

    func testTurnDecodesSnakeCaseWireContract() throws {
        let json = """
        {"schema":"opensks.conversation-turn.v1","turn_id":"t1","user_message_id":"u1","assistant_message_id":"a1","run_id":"r1","run_state":"completed","reused":false}
        """
        let turn = try JSONDecoder.opensks.decode(ConversationTurn.self, from: Data(json.utf8))
        XCTAssertEqual(turn.turnId, "t1")
        XCTAssertEqual(turn.userMessageId, "u1")
        XCTAssertEqual(turn.assistantMessageId, "a1")
        XCTAssertEqual(turn.runId, "r1")
        XCTAssertEqual(turn.runState, .completed)
        XCTAssertFalse(turn.reused)
    }

    func testReusedTurnDecodes() throws {
        let json = """
        {"schema":"opensks.conversation-turn.v1","turn_id":"t1","user_message_id":"u1","assistant_message_id":"a1","run_id":"r1","run_state":"failed","reused":true}
        """
        let turn = try JSONDecoder.opensks.decode(ConversationTurn.self, from: Data(json.utf8))
        XCTAssertTrue(turn.reused)
        XCTAssertEqual(turn.runState, .failed)
    }

    func testRunListDecodesSnakeCaseWireContract() throws {
        let json = """
        {"schema":"opensks.conversation-run-list.v1","conversation_id":"c1","runs":[{"turn_id":"t1","run_id":"r1","message_id":"m1","relation":"primary","run_state":"completed"}]}
        """
        let list = try JSONDecoder.opensks.decode(ConversationRunList.self, from: Data(json.utf8))
        XCTAssertEqual(list.conversationId, "c1")
        XCTAssertEqual(list.runs.count, 1)
        let run = try XCTUnwrap(list.runs.first)
        XCTAssertEqual(run.turnId, "t1")
        XCTAssertEqual(run.runId, "r1")
        XCTAssertEqual(run.messageId, "m1")
        XCTAssertEqual(run.relation, "primary")
        XCTAssertEqual(run.runState, .completed)
        XCTAssertEqual(run.id, "r1", "run id is the identity for ForEach")
    }

    func testUnknownRunStateFallsBackInsteadOfThrowing() throws {
        let json = """
        {"schema":"opensks.conversation-turn.v1","turn_id":"t1","user_message_id":"u1","assistant_message_id":"a1","run_id":"r1","run_state":"materializing","reused":false}
        """
        let turn = try JSONDecoder.opensks.decode(ConversationTurn.self, from: Data(json.utf8))
        XCTAssertEqual(turn.runState, .unknown)
    }

    // MARK: - Send appends user + assistant and records the run

    func testSendAppendsUserAndAssistantTurnAndRecordsRun() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        XCTAssertTrue(store.messages.isEmpty)
        XCTAssertTrue(store.runs(for: "a").isEmpty)

        await store.send(conversationID: "a", text: "explain the parser")

        // User message + assistant turn are both persisted and reloaded.
        XCTAssertEqual(store.messages.count, 2)
        XCTAssertEqual(store.messages.first?.role, .user)
        XCTAssertEqual(store.messages.first?.contentRedacted, "explain the parser")
        XCTAssertEqual(store.messages.last?.role, .assistant)
        XCTAssertFalse(store.messages.last?.contentRedacted.isEmpty ?? true,
                       "assistant content is set from the run result")

        // Exactly one run is recorded and linked to the assistant message.
        let runs = store.runs(for: "a")
        XCTAssertEqual(runs.count, 1)
        let run = runs[0]
        XCTAssertEqual(run.relation, "primary")
        XCTAssertEqual(run.runState, .completed)
        XCTAssertEqual(run.messageId, store.messages.last?.id)
        XCTAssertNotNil(store.run(forMessageID: store.messages.last!.id))
    }

    func testSendClearsDraftForThatConversation() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        store.setDraft("typed text", for: "a")

        await store.send(conversationID: "a", text: store.draft(for: "a"))

        XCTAssertEqual(store.draft(for: "a"), "", "draft is cleared on a successful send")
    }

    func testEmptySendStartsNoRun() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()

        await store.send(conversationID: "a", text: "   \n  ")

        XCTAssertTrue(store.messages.isEmpty)
        XCTAssertTrue(store.runs(for: "a").isEmpty)
    }

    // MARK: - Idempotency at the store / service level

    func testReusedTurnDoesNotDuplicateRunList() async {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()

        // First send starts a real turn + run.
        let first = try? await mock.turnStart(conversationID: "a", text: "hi", idempotencyKey: "key-1")
        XCTAssertEqual(first?.reused, false)

        // Replaying the SAME key returns the same ids, reused, and does NOT add
        // a second run to the list.
        let replay = try? await mock.turnStart(conversationID: "a", text: "hi", idempotencyKey: "key-1")
        XCTAssertEqual(replay?.reused, true)
        XCTAssertEqual(replay?.runId, first?.runId)
        XCTAssertEqual(replay?.turnId, first?.turnId)

        await store.loadRuns(for: "a")
        XCTAssertEqual(store.runs(for: "a").count, 1, "a reused turn does not duplicate the run list")
    }

    func testTwoDistinctSendsStartTwoRuns() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()

        await store.send(conversationID: "a", text: "first")
        await store.send(conversationID: "a", text: "second")

        // Two distinct sends (distinct generated keys) => two runs, four messages.
        XCTAssertEqual(store.runs(for: "a").count, 2)
        XCTAssertEqual(store.messages.count, 4)
    }

    func testFailedRunSurfacesDangerPillKind() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")], runStateOnTurn: .failed)
        await store.load()

        await store.send(conversationID: "a", text: "trigger failure")

        let run = try? XCTUnwrap(store.runs(for: "a").first)
        XCTAssertEqual(run?.runState, .failed)
        XCTAssertEqual(run?.runState.pillKind, .danger)
    }

    // MARK: - Render smoke tests

    func testRunCardRendersNonNil() throws {
        let card = RunCard(run: runRef())
            .frame(width: 480, height: 80)
        let renderer = ImageRenderer(content: card)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "run card must render non-nil")
    }

    func testRunCardRendersFailedStateNonNil() throws {
        let card = RunCard(run: runRef(state: .failed))
            .frame(width: 480, height: 80)
        XCTAssertNotNil(ImageRenderer(content: card).nsImage)
    }

    func testConversationComposerRendersNonNil() async throws {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let composer = ConversationComposer(store: store, conversationID: "a")
            .frame(width: 720, height: 90)
        let renderer = ImageRenderer(content: composer)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation composer must render non-nil")
    }

    func testThreadWithRunCardRendersNonNil() async throws {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        await store.send(conversationID: "a", text: "render me")
        let thread = ConversationThreadView(store: store)
            .frame(width: 720, height: 600)
        XCTAssertNotNil(ImageRenderer(content: thread).nsImage,
                        "thread with composer + run card must render non-nil")
    }
}
