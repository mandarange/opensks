// ConversationTurnTests.swift — PR-027. Exercises the conversation send path
// through `MockConversationService`: one Send persists the user message + an
// accepted assistant placeholder, starts live event observation, kicks the
// foreground supervisor drain, and records ONE run; the store-level send is idempotent in effect (the mock
// de-dups a replayed idempotency key, so the run list never grows on a reused
// turn); and the UI (RunCard + ConversationComposer) renders non-nil.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class ConversationTurnTests: XCTestCase {
    // MARK: - Fixtures

    private func summary(
        id: String,
        title: String = "Thread",
        status: ConversationStatus = .idle,
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
        runStateOnTurn: RunState = .queued
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
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        XCTAssertTrue(store.messages.isEmpty)
        XCTAssertTrue(store.runs(for: "a").isEmpty)

        await store.send(conversationID: "a", text: "explain the parser")

        // User message + assistant turn are both persisted and reloaded.
        XCTAssertEqual(store.messages.count, 2)
        XCTAssertEqual(store.messages.first?.role, .user)
        XCTAssertEqual(store.messages.first?.contentRedacted, "explain the parser")
        XCTAssertEqual(store.messages.last?.role, .assistant)
        await waitForSubscribeCalls(mock)
        await waitForSupervisorTicks(mock, atLeast: 1)
        await waitForAssistantState(store, conversationID: "a", state: .complete)
        XCTAssertGreaterThanOrEqual(mock.subscribeRunEventsCallCount, 1)
        XCTAssertEqual(mock.subscribeRunEventsRequests.first?.runID, store.runs(for: "a").first?.runId)
        XCTAssertEqual(mock.subscribeRunEventsRequests.first?.sinceSequence, 0)
        XCTAssertNotNil(mock.subscribeRunEventsRequests.first?.tailMs)
        XCTAssertLessThanOrEqual(mock.subscribeRunEventsRequests.first?.tailMs ?? .max, 1_000)
        let timeline = store.timelineItems(for: "a")
        let messageTimeline = timeline.filter { $0.kind == .userMessage || $0.kind == .assistantMessage }
        XCTAssertEqual(messageTimeline.map(\.kind), [.userMessage, .assistantMessage])
        XCTAssertEqual(messageTimeline.last?.payload.messageId, store.messages.last?.id)
        XCTAssertEqual(messageTimeline.last?.runId, store.runs(for: "a").first?.runId)
        XCTAssertEqual(messageTimeline.last?.state, "completed")

        // Exactly one run is recorded and linked to the assistant message.
        let runs = store.runs(for: "a")
        XCTAssertEqual(runs.count, 1)
        let run = runs[0]
        XCTAssertEqual(run.relation, "primary")
        XCTAssertEqual(run.runState, .completed)
        XCTAssertEqual(run.messageId, store.messages.last?.id)
        XCTAssertNotNil(store.run(forMessageID: store.messages.last!.id))
        XCTAssertEqual(store.selectedSummary?.status, .completed)
        XCTAssertEqual(mock.supervisorTickCount, 1, "foreground send claims the just-accepted run without draining stale backlog")
        XCTAssertEqual(store.messages.last?.state, .complete)
        XCTAssertEqual(store.messages.last?.contentRedacted, "Mock supervisor completed.")
    }

    func testForegroundSendClaimsJustAcceptedRunBeforeOlderQueuedTurns() async throws {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        let oldTurn = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "old queued turn",
            idempotencyKey: "old-queued-turn"
        )
        await store.load()

        await store.send(conversationID: "a", text: "fresh foreground turn")

        await waitForSupervisorTicks(mock, atLeast: 1)
        await waitForAssistantState(store, conversationID: "a", state: .complete)
        let runs = store.runs(for: "a")
        XCTAssertEqual(runs.first { $0.runId == oldTurn.runId }?.runState, .queued)
        let freshRun = try XCTUnwrap(runs.last)
        XCTAssertNotEqual(freshRun.runId, oldTurn.runId)
        XCTAssertEqual(freshRun.runState, .completed)
        XCTAssertEqual(mock.supervisorTickCount, 1)
        XCTAssertEqual(store.messages.last?.contentRedacted, "Mock supervisor completed.")
    }

    func testSendRecoversCommittedTurnWhenTurnStartResponseFails() async {
        let base = MockConversationService(summaries: [summary(id: "a")])
        let service = GatedTurnStartConversationService(
            base: base,
            turnStartErrorAfterCommit: NSError(
                domain: "turn-start",
                code: 42,
                userInfo: [NSLocalizedDescriptionKey: "accepted response timed out"]
            )
        )
        let store = ConversationStore(service: service, messagePageSize: 50)
        await store.load()
        store.setDraft("recover the committed turn", for: "a")

        await store.send(conversationID: "a", text: store.draft(for: "a"))

        await waitForSupervisorTicks(base, atLeast: 1)
        await waitForAssistantState(store, conversationID: "a", state: .complete)
        XCTAssertEqual(store.draft(for: "a"), "")
        XCTAssertEqual(store.messages.filter { $0.role == .user }.count, 1)
        XCTAssertEqual(store.runs(for: "a").first?.runState, .completed)
        XCTAssertNil(store.errorMessage)
    }

    func testSendSurfacesResumableRunEventCursorGapInTimeline() async throws {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        var error = PublicEngineError(
            code: "subscription_cursor_gap",
            message: "Requested event sequence 999 is beyond durable sequence 2",
            retryable: true
        )
        error.remediation = "Reconnect from sequence 2"
        error.evidenceRefs = ["daemon:subscription-cursor-gap", "event-store:last-sequence"]
        mock.setDefaultSubscribeRunFailure(
            EngineStreamFailure(
                streamID: "event-stream-run-gap",
                cursor: 1,
                error: error,
                resumable: true
            )
        )

        await store.load()
        await store.send(conversationID: "a", text: "watch the stream")
        await waitForSubscribeCalls(mock)

        let runID = try XCTUnwrap(store.runs(for: "a").first?.runId)
        let itemID = "timeline-event-stream-failure-\(runID)-subscription_cursor_gap-1"
        let failureItem = try XCTUnwrap(
            store.timelineItems(for: "a").first { timelineItem in
                timelineItem.id == itemID
            }
        )
        XCTAssertEqual(failureItem.kind, .error)
        XCTAssertEqual(failureItem.state, "stream_failed")
        XCTAssertEqual(failureItem.payload.projection, "live_stream_failure")
        XCTAssertEqual(failureItem.payload.sourceSchema, "opensks.public-engine-error.v1")
        XCTAssertEqual(failureItem.payload.message, "Requested event sequence 999 is beyond durable sequence 2")
        XCTAssertTrue(failureItem.payload.contentRedacted?.contains("Reconnect from sequence 2") == true)
        XCTAssertTrue(store.errorMessage?.contains("Run event stream failed") == true)
        XCTAssertTrue(store.errorMessage?.contains("Reconnect from sequence 2") == true)
        XCTAssertEqual(mock.subscribeRunEventsRequests.first?.sinceSequence, 0)
    }

    func testExplicitSupervisorDrainRecoversQueuedTurnsAfterRelaunch() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a")])
        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "first queued turn",
            idempotencyKey: "queued-1"
        )
        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "second queued turn",
            idempotencyKey: "queued-2"
        )

        let relaunchedStore = ConversationStore(service: mock, messagePageSize: 50)
        await relaunchedStore.load()

        XCTAssertNil(relaunchedStore.errorMessage)
        XCTAssertEqual(mock.supervisorTickCount, 0, "default load observes queued turns without executing supervisor ticks")
        XCTAssertEqual(relaunchedStore.messages.count, 4)
        XCTAssertEqual(relaunchedStore.runs(for: "a").map(\.runState), [.queued, .queued])
        XCTAssertTrue(relaunchedStore.messages.filter { $0.role == .assistant }.allSatisfy { $0.state == .streaming })

        let drained = try await relaunchedStore.drainSupervisorQueue()

        XCTAssertEqual(drained, 2)
        XCTAssertEqual(mock.supervisorTickCount, 3, "explicit drain recovers both queued turns and stops on an empty tick")
        XCTAssertEqual(relaunchedStore.messages.count, 4)
        XCTAssertEqual(relaunchedStore.timelineItems(for: "a").count, 4)
        XCTAssertEqual(relaunchedStore.timelineItems(for: "a").filter { $0.kind == .assistantMessage }.count, 2)
        XCTAssertTrue(relaunchedStore.messages.filter { $0.role == .assistant }.allSatisfy { $0.state == .complete })
        XCTAssertEqual(relaunchedStore.runs(for: "a").map(\.runState), [.completed, .completed])
        XCTAssertEqual(relaunchedStore.selectedSummary?.status, .completed)
    }

    func testSendClearsDraftForThatConversation() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        store.setDraft("typed text", for: "a")

        await store.send(conversationID: "a", text: store.draft(for: "a"))

        XCTAssertEqual(store.draft(for: "a"), "", "draft is cleared on a successful send")
        XCTAssertFalse(store.localSendToken(for: "a").isEmpty, "send publishes a latest-scroll token")
    }

    func testEmptySendStartsNoRun() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()

        await store.send(conversationID: "a", text: "   \n  ")

        XCTAssertTrue(store.messages.isEmpty)
        XCTAssertTrue(store.runs(for: "a").isEmpty)
    }

    func testSendPassesThreadSettingsAndContextRefs() async throws {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let source = "alpha\nselected context\nomega\n"
        let ref = try XCTUnwrap(EditorContextRef.capture(
            workspaceRelativePath: "src/lib.rs",
            displayName: "lib.rs",
            fullText: source,
            lineRange: EditorLineRange(start: 2, end: 2)
        ))
        store.attachEditorContext(ref, to: "a", currentText: source)
        await store.updateThreadSettings(for: "a") { settings in
            settings.modelSelection = ModelSelection(
                mode: .pinned,
                modelId: "provider/text-large",
                fallbackModelIds: ["provider/text-small"]
            )
            settings.reasoningEffort = .maximum
            settings.executionMode = .worktree
            settings.pipelineId = "parallel-build"
            settings.maxParallelism = 8
            settings.verifierCount = 3
            settings.toolPolicyId = "read-only"
            settings.approvalPolicyId = "manual-review"
            settings.tokenBudget = 120_000
            settings.costBudgetUsd = 2.75
            settings.timeoutMs = 600_000
            settings.imageModelId = "provider/image"
        }

        await store.send(conversationID: "a", text: "use the selected lines")

        let request = try XCTUnwrap(mock.turnStartRequests.last)
        let storedSettings = try await mock.threadSettings(conversationID: "a")
        XCTAssertEqual(request.text, "use the selected lines")
        XCTAssertEqual(request.threadSettingsUpdatedAtMs, storedSettings.updatedAtMs)
        XCTAssertEqual(request.context.refs, [ref.wireReference])
        XCTAssertNil(request.settings)
        XCTAssertEqual(storedSettings.modelSelection, ModelSelection(
            mode: .pinned,
            modelId: "provider/text-large",
            fallbackModelIds: ["provider/text-small"]
        ))
        XCTAssertEqual(storedSettings.reasoningEffort, .maximum)
        XCTAssertEqual(storedSettings.pipelineId, "parallel-build")
        XCTAssertEqual(storedSettings.maxParallelism, 8)
        XCTAssertEqual(storedSettings.verifierCount, 3)
        XCTAssertEqual(storedSettings.toolPolicyId, "read-only")
        XCTAssertEqual(storedSettings.approvalPolicyId, "manual-review")
        XCTAssertEqual(storedSettings.tokenBudget, 120_000)
        XCTAssertEqual(storedSettings.costBudgetUsd, 2.75)
        XCTAssertEqual(storedSettings.timeoutMs, 600_000)
        XCTAssertEqual(storedSettings.imageModelId, "provider/image")
    }

    func testContextAttachmentMarksStaleAfterBackgroundRefreshAndComposerRenders() async throws {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let original = "alpha\nselected context\nomega\n"
        let ref = try XCTUnwrap(EditorContextRef.capture(
            workspaceRelativePath: "src/lib.rs",
            displayName: "lib.rs",
            fullText: original,
            lineRange: EditorLineRange(start: 2, end: 2)
        ))
        store.attachEditorContext(ref, to: "a", currentText: original)
        XCTAssertEqual(store.contextAttachments(for: "a").first?.isStale, false)

        store.refreshEditorContexts(
            workspaceRelativePath: "src/lib.rs",
            fullText: "alpha\nchanged context\nomega\n"
        )

        let attachment = try XCTUnwrap(store.contextAttachments(for: "a").first)
        XCTAssertTrue(attachment.isStale)
        XCTAssertNotEqual(attachment.currentHash, ref.contentHash)

        let composer = ConversationComposer(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore()),
            conversationID: "a"
        )
            .frame(width: 720, height: 180)
        let renderer = ImageRenderer(content: composer)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "composer should render stale context attachments")
    }

    // MARK: - Idempotency at the store / service level

    func testReusedTurnDoesNotDuplicateRunList() async {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()

        // First send starts a real turn + run.
        let first = try? await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "hi",
            idempotencyKey: "key-1"
        )
        XCTAssertEqual(first?.reused, false)

        // Replaying the SAME key returns the same ids, reused, and does NOT add
        // a second run to the list.
        let replay = try? await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "hi",
            idempotencyKey: "key-1"
        )
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
        let messageTimeline = store.timelineItems(for: "a")
            .filter { $0.kind == .userMessage || $0.kind == .assistantMessage }
        XCTAssertEqual(messageTimeline.count, 4)
    }

    func testSendInFlightIsScopedPerConversation() async {
        let base = MockConversationService(summaries: [summary(id: "a"), summary(id: "b")])
        let service = GatedTurnStartConversationService(base: base, gatedConversationID: "a")
        let store = ConversationStore(service: service, messagePageSize: 50)
        await store.load()

        let firstSend = Task { await store.send(conversationID: "a", text: "first") }
        await service.waitUntilGatedTurnStartEntered()

        XCTAssertTrue(store.isSending(conversationID: "a"))
        XCTAssertFalse(store.isSending(conversationID: "b"))

        await store.send(conversationID: "b", text: "second")
        XCTAssertEqual(base.turnStartRequests.map(\.conversationID), ["b"])
        XCTAssertEqual(store.runs(for: "b").count, 1)

        service.releaseGate()
        await firstSend.value

        XCTAssertEqual(Set(base.turnStartRequests.map(\.conversationID)), Set(["a", "b"]))
        XCTAssertEqual(store.runs(for: "a").count, 1)
        XCTAssertEqual(store.runs(for: "b").count, 1)
        XCTAssertFalse(store.isSending(conversationID: "a"))
        XCTAssertFalse(store.isSending(conversationID: "b"))
    }

    func testThreadSettingsUpdateIsOptimisticWhileSaveInFlight() async throws {
        let base = MockConversationService(summaries: [summary(id: "a")])
        let service = GatedTurnStartConversationService(
            base: base,
            gatedThreadSettingsConversationID: "a"
        )
        let store = ConversationStore(service: service, messagePageSize: 50)
        await store.load()

        let save = Task {
            await store.updateThreadSettings(for: "a") { settings in
                settings.executionMode = .readOnly
                settings.maxParallelism = 9
            }
        }
        await service.waitUntilGatedThreadSettingsSaveEntered()

        XCTAssertTrue(store.isSavingThreadSettings(for: "a"))
        XCTAssertEqual(store.threadSettings(for: "a").executionMode, .readOnly)
        XCTAssertEqual(store.threadSettings(for: "a").maxParallelism, 9)

        service.releaseThreadSettingsGate()
        await save.value

        XCTAssertFalse(store.isSavingThreadSettings(for: "a"))
        XCTAssertEqual(store.threadSettings(for: "a").executionMode, .readOnly)
        XCTAssertEqual(store.threadSettings(for: "a").maxParallelism, 9)
        let persisted = try await base.threadSettings(conversationID: "a")
        XCTAssertEqual(persisted.executionMode, .readOnly)
        XCTAssertEqual(persisted.maxParallelism, 9)
    }

    func testThreadSettingsUpdateRollsBackOptimisticStateWhenSaveFails() async {
        let error = NSError(domain: "OpenSKSStudioTests", code: 42)
        let service = GatedTurnStartConversationService(
            base: MockConversationService(summaries: [summary(id: "a")]),
            threadSettingsError: error
        )
        let store = ConversationStore(service: service, messagePageSize: 50)
        await store.load()
        let original = store.threadSettings(for: "a")

        await store.updateThreadSettings(for: "a") { settings in
            settings.executionMode = .readOnly
            settings.maxParallelism = 9
        }

        XCTAssertFalse(store.isSavingThreadSettings(for: "a"))
        XCTAssertEqual(store.threadSettings(for: "a"), original)
        XCTAssertEqual(store.errorMessage, error.localizedDescription)
    }

    func testSendStartsForegroundRunEventSubscriptionAndPreservesLiveTimelineCards() async throws {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()

        await store.send(conversationID: "a", text: "stream progress")

        await waitForSubscribeCalls(mock)
        let runID = try XCTUnwrap(store.runs(for: "a").first?.runId)
        let started = await waitForTimelineItem(
            store,
            conversationID: "a",
            id: "timeline-event-evt-\(runID)-started"
        )
        XCTAssertEqual(started?.kind, .worker)
        XCTAssertEqual(started?.state, "run_started")
        XCTAssertEqual(started?.payload.projection, "live_execution_event")
        await waitForSupervisorTicks(mock, atLeast: 1)
        let completed = await waitForTimelineItem(
            store,
            conversationID: "a",
            id: "timeline-event-evt-\(runID)-terminal"
        )

        XCTAssertEqual(completed?.state, "snapshot_written")
        XCTAssertEqual(completed?.payload.contentRedacted, "Mock supervisor completed.")
    }

    func testDefaultLoadDoesNotDrainRunningTurns() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a")])
        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "queued turn",
            idempotencyKey: "queued-load"
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)

        await store.load()

        XCTAssertEqual(mock.supervisorTickCount, 0)
        XCTAssertEqual(store.runs(for: "a").map(\.runState), [.queued])
        XCTAssertEqual(store.messages.last?.state, .streaming)
    }

    func testDiagnosticSupervisorDrainCanRecoverRunningTurnsAfterObserverLoad() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a")])
        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "queued turn",
            idempotencyKey: "queued-load-recovery"
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)

        await store.load()

        XCTAssertEqual(mock.supervisorTickCount, 0, "product load remains observer-only even with queued turns")
        XCTAssertEqual(store.runs(for: "a").map(\.runState), [.queued])
        XCTAssertEqual(store.messages.last?.state, .streaming)

        let drained = try await store.drainSupervisorQueue()

        XCTAssertEqual(drained, 1)
        XCTAssertEqual(mock.supervisorTickCount, 2, "diagnostic recovery is opt-in via explicit drain only")
        XCTAssertEqual(store.runs(for: "a").map(\.runState), [.completed])
        XCTAssertEqual(store.messages.last?.state, .complete)
    }

    func testDiagnosticSupervisorDrainAppliesTickLifecycleWithoutReloadAfterRelaunch() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a")])
        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "queued turn",
            idempotencyKey: "queued-drain-no-reload"
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)

        await store.load()

        XCTAssertEqual(mock.subscribeRunEventsCallCount, 0, "relaunch load has no active live subscription")
        XCTAssertEqual(store.runs(for: "a").map(\.runState), [.queued])
        XCTAssertEqual(store.messages.last?.state, .streaming)

        let drained = try await store.drainSupervisorQueue(refreshAfterDrain: false)

        XCTAssertEqual(drained, 1)
        XCTAssertEqual(mock.supervisorTickCount, 2, "manual drain still confirms the queue is empty")
        XCTAssertEqual(store.runs(for: "a").map(\.runState), [.completed])
        XCTAssertEqual(store.messages.last?.state, .complete)
        XCTAssertEqual(store.messages.last?.contentRedacted, "...", "reload-free tick application updates lifecycle only")
        XCTAssertEqual(store.selectedSummary?.status, .completed)
    }

    func testSupervisorDrainRespectsBackgroundedConversationAndRehydratesOnActivation() async throws {
        let mock = MockConversationService(summaries: [summary(id: "a")])
        _ = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "queued background turn",
            idempotencyKey: "queued-background-drain"
        )
        let store = ConversationStore(service: mock, messagePageSize: 50)

        await store.load()
        XCTAssertTrue(store.retainsHeavyView("a"))

        await store.setActive(nil)
        XCTAssertFalse(store.retainsHeavyView("a"))
        XCTAssertTrue(store.messages.isEmpty)
        XCTAssertTrue(store.timelineItems(for: "a").isEmpty)

        let drained = try await store.drainSupervisorQueue()

        XCTAssertEqual(drained, 1)
        XCTAssertEqual(store.runs(for: "a").map(\.runState), [.completed])
        XCTAssertEqual(store.selectedSummary?.status, .completed)
        XCTAssertTrue(store.messages.isEmpty, "background drain must not rehydrate the heavy message page")
        XCTAssertTrue(store.timelineItems(for: "a").isEmpty, "background drain must not rehydrate the heavy timeline page")
        XCTAssertFalse(store.retainsHeavyView("a"))

        await store.setActive("a")

        XCTAssertTrue(store.retainsHeavyView("a"))
        XCTAssertEqual(store.messages.last?.state, .complete)
        XCTAssertEqual(store.messages.last?.contentRedacted, "Mock supervisor completed.")
        XCTAssertEqual(store.timelineItems(for: "a").filter { $0.kind == .assistantMessage }.last?.state, "completed")
    }

    func testLiveExecutionEventsAppendTimelineCardsAndDedup() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let event = executionEvent(
            id: "evt-live-1",
            runID: "run-live",
            sequence: 7,
            kind: .workItemRunning,
            message: "worker running"
        )

        store.applyLiveExecutionEvents(
            [event, event],
            conversationID: "a",
            projectID: "mock-project",
            turnID: "turn-live"
        )

        let timeline = store.timelineItems(for: "a")
        XCTAssertEqual(timeline.filter { $0.id == "timeline-event-evt-live-1" }.count, 1)
        let item = timeline.first { $0.id == "timeline-event-evt-live-1" }
        XCTAssertEqual(item?.kind, .worker)
        XCTAssertEqual(item?.state, "work_item_running")
        XCTAssertEqual(item?.payload.contentRedacted, "worker running")
        XCTAssertEqual(item?.payload.projection, "live_execution_event")
        XCTAssertEqual(item?.runId, "run-live")
    }

    func testLiveToolAndPatchEventsExposeTimelineDetails() async throws {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let tool = executionEvent(
            id: "evt-tool-detail",
            runID: "run-detail",
            sequence: 8,
            kind: .workItemCompleted,
            message: "targeted tests passed",
            payload: .object([
                "agent_event_kind": .string("tool_call_completed"),
                "worker_id": .string("worker-code"),
                "work_item_id": .string("work-code"),
                "payload": .object([
                    "tool": .string("test.run_targeted"),
                    "command_redacted": .string("cargo test -p opensks-cli push_cli"),
                    "exit_code": .number(0),
                    "duration_ms": .number(42),
                    "timed_out": .bool(false),
                    "test_targets": .array([.string("opensks-cli::push_cli")])
                ])
            ])
        )
        let patch = executionEvent(
            id: "evt-patch-detail",
            runID: "run-detail",
            sequence: 9,
            kind: .workItemRunning,
            message: "patch applied",
            payload: .object([
                "agent_event_kind": .string("file_patch_applied"),
                "worker_id": .string("worker-code"),
                "payload": .object([
                    "code": .string("patch_applied"),
                    "role": .string("code"),
                    "applied_files": .array([.string("crates/opensks-cli/src/lib.rs")]),
                    "patch_count": .number(1),
                    "apply_result_count": .number(1),
                    "main_workspace_modified": .bool(false)
                ])
            ])
        )

        store.applyLiveExecutionEvents(
            [tool, patch],
            conversationID: "a",
            projectID: "mock-project",
            turnID: "turn-detail"
        )

        let timeline = store.timelineItems(for: "a")
        let toolItem = try XCTUnwrap(timeline.first { $0.id == "timeline-event-evt-tool-detail" })
        XCTAssertEqual(toolItem.kind, .toolCall)
        XCTAssertEqual(toolItem.payload.agentEventKind, "tool_call_completed")
        XCTAssertEqual(toolItem.payload.workerId, "worker-code")
        XCTAssertEqual(toolItem.payload.workItemId, "work-code")
        XCTAssertEqual(toolItem.payload.tool, "test.run_targeted")
        XCTAssertEqual(toolItem.payload.commandRedacted, "cargo test -p opensks-cli push_cli")
        XCTAssertEqual(toolItem.payload.exitCode, 0)
        XCTAssertEqual(toolItem.payload.durationMs, 42)
        XCTAssertEqual(toolItem.payload.timedOut, false)
        XCTAssertEqual(toolItem.payload.testTargets, ["opensks-cli::push_cli"])
        XCTAssertNotNil(ImageRenderer(content: TimelineItemCell(item: toolItem).frame(width: 720, height: 180)).nsImage)

        let patchItem = try XCTUnwrap(timeline.first { $0.id == "timeline-event-evt-patch-detail" })
        XCTAssertEqual(patchItem.kind, .patch)
        XCTAssertEqual(patchItem.payload.agentEventKind, "file_patch_applied")
        XCTAssertEqual(patchItem.payload.workerId, "worker-code")
        XCTAssertEqual(patchItem.payload.roleLabel, "code")
        XCTAssertEqual(patchItem.payload.appliedFiles, ["crates/opensks-cli/src/lib.rs"])
        XCTAssertEqual(patchItem.payload.patchCount, 1)
        XCTAssertEqual(patchItem.payload.applyResultCount, 1)
        XCTAssertEqual(patchItem.payload.mainWorkspaceModified, false)
        XCTAssertNotNil(ImageRenderer(content: TimelineItemCell(item: patchItem).frame(width: 720, height: 180)).nsImage)
    }

    func testLiveAssistantEventsExposeSpecializedTimelineCard() async throws {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let turn = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "stream assistant text",
            idempotencyKey: "assistant-live"
        )
        await store.setActive("a")
        let assistantMessageID = try XCTUnwrap(store.messages.last?.id)
        let secret = "sk-liveassistantsecret1234567890"
        let delta = executionEvent(
            id: "evt-assistant-delta",
            runID: turn.runId,
            sequence: 10,
            kind: .workItemRunning,
            message: "assistant delta",
            payload: .object([
                "agent_event_kind": .string("assistant_text_delta"),
                "payload": .object([
                    "delta": .string("Drafting the final answer"),
                    "assistant_message_id": .string(assistantMessageID),
                    "model_id": .string("model-writer")
                ])
            ])
        )
        let completed = executionEvent(
            id: "evt-assistant-completed",
            runID: turn.runId,
            sequence: 11,
            kind: .workItemCompleted,
            message: "assistant completed",
            payload: .object([
                "agent_event_kind": .string("assistant_text_completed"),
                "payload": .object([
                    "text": .string("Final answer ready with \(secret)."),
                    "assistant_message_id": .string(assistantMessageID),
                    "provider_id": .string("provider-openai"),
                    "model_id": .string("model-writer"),
                    "response_hash": .string("sha256:assistant-response"),
                    "response_bytes": .number(128),
                    "finish_reason": .string("stop")
                ])
            ])
        )

        store.applyLiveExecutionEvents(
            [delta],
            conversationID: "a",
            projectID: "mock-project",
            turnID: "turn-assistant"
        )
        XCTAssertEqual(store.messages.last?.state, .streaming)
        XCTAssertEqual(store.messages.last?.contentRedacted, "Drafting the final answer")
        let streamingTimeline = store.timelineItems(for: "a")
        let streamingMessageItem = try XCTUnwrap(
            streamingTimeline.first { $0.id == "timeline-\(assistantMessageID)" }
        )
        XCTAssertEqual(streamingMessageItem.message?.state, .streaming)
        XCTAssertEqual(streamingMessageItem.message?.contentRedacted, "Drafting the final answer")

        store.applyLiveExecutionEvents(
            [completed],
            conversationID: "a",
            projectID: "mock-project",
            turnID: "turn-assistant"
        )
        XCTAssertEqual(store.messages.last?.state, .complete)
        XCTAssertEqual(store.messages.last?.contentRedacted, "Final answer ready with [REDACTED].")

        let timeline = store.timelineItems(for: "a")
        let completedMessageItem = try XCTUnwrap(timeline.first { $0.id == "timeline-\(assistantMessageID)" })
        XCTAssertEqual(completedMessageItem.message?.state, .complete)
        XCTAssertEqual(completedMessageItem.message?.contentRedacted, "Final answer ready with [REDACTED].")
        XCTAssertFalse(completedMessageItem.message?.contentRedacted.contains(secret) ?? true)
        let deltaItem = try XCTUnwrap(timeline.first { $0.id == "timeline-event-evt-assistant-delta" })
        XCTAssertEqual(deltaItem.kind, .assistantMessage)
        XCTAssertEqual(deltaItem.state, "streaming")
        XCTAssertEqual(deltaItem.payload.agentEventKind, "assistant_text_delta")
        XCTAssertEqual(deltaItem.payload.assistantDelta, "Drafting the final answer")
        XCTAssertEqual(deltaItem.payload.modelId, "model-writer")

        let completedItem = try XCTUnwrap(timeline.first { $0.id == "timeline-event-evt-assistant-completed" })
        XCTAssertEqual(completedItem.kind, .assistantMessage)
        XCTAssertEqual(completedItem.state, "completed")
        XCTAssertNil(completedItem.message)
        XCTAssertEqual(completedItem.payload.assistantMessageId, assistantMessageID)
        XCTAssertEqual(completedItem.payload.providerId, "provider-openai")
        XCTAssertEqual(completedItem.payload.responseHash, "sha256:assistant-response")
        XCTAssertEqual(completedItem.payload.responseBytes, 128)
        XCTAssertEqual(completedItem.payload.completionReason, "stop")
        XCTAssertEqual(completedItem.payload.assistantText, "Final answer ready with [REDACTED].")
        XCTAssertEqual(completedItem.payload.contentRedacted, "Final answer ready with [REDACTED].")
        XCTAssertFalse(completedItem.payload.assistantText?.contains(secret) ?? true)
        XCTAssertNotNil(ImageRenderer(content: AssistantTimelineEventCell(item: completedItem).frame(width: 720, height: 220)).nsImage)
    }

    func testRunCompletedEventFinalizesLiveAssistantTurnWithoutHidingText() async throws {
        let (store, mock) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let turn = try await mock.turnStart(
            conversationID: "a",
            projectID: "mock-project",
            text: "show the natural response",
            idempotencyKey: "assistant-run-completed"
        )
        await store.setActive("a")
        let assistantMessageID = try XCTUnwrap(store.messages.last?.id)
        XCTAssertEqual(store.runs(for: "a").first?.runState, .queued)

        let answer = executionEvent(
            id: "evt-answer-visible",
            runID: turn.runId,
            sequence: 10,
            kind: .workItemCompleted,
            message: "assistant completed",
            payload: .object([
                "agent_event_kind": .string("assistant_text_completed"),
                "payload": .object([
                    "text": .string("Natural language response is visible."),
                    "assistant_message_id": .string(assistantMessageID)
                ])
            ])
        )
        let completed = executionEvent(
            id: "evt-run-completed",
            runID: turn.runId,
            sequence: 11,
            kind: .runCompleted,
            message: "run completed"
        )

        store.applyLiveExecutionEvents(
            [answer, completed],
            conversationID: "a",
            projectID: "mock-project",
            turnID: turn.turnId
        )

        XCTAssertEqual(store.runs(for: "a").first?.runState, .completed)
        XCTAssertEqual(store.selectedSummary?.status, .completed)
        XCTAssertEqual(store.messages.last?.state, .complete)
        XCTAssertEqual(store.messages.last?.contentRedacted, "Natural language response is visible.")

        let timeline = store.timelineItems(for: "a")
        let messageItem = try XCTUnwrap(timeline.first { $0.id == "timeline-\(assistantMessageID)" })
        XCTAssertEqual(messageItem.message?.state, .complete)
        XCTAssertEqual(messageItem.message?.contentRedacted, "Natural language response is visible.")
        XCTAssertEqual(timeline.first { $0.id == "timeline-event-evt-run-completed" }?.state, "run_completed")
        let latestReply = try XCTUnwrap(latestAssistantReplyMessage(in: timeline))
        XCTAssertEqual(latestReply.contentRedacted, "Natural language response is visible.")
        XCTAssertTrue(shouldRenderPinnedAssistantReply(latestReply, in: timeline))
    }

    func testLiveImageArtifactEventsRenderTypedTimelineItem() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let event = executionEvent(
            id: "evt-image",
            runID: "run-image",
            sequence: 8,
            kind: .imageArtifactCreated,
            message: "Image artifact cli-image-asset created.",
            payload: .object([
                "content_redacted": .string("Image artifact cli-image-asset created."),
                "asset_id": .string("cli-image-asset"),
                "provider_id": .string("provider-1"),
                "model_id": .string("provider-1/image-model"),
                "path": .string(".opensks/assets/candidates/cli-image-asset.ppm"),
                "content_hash": .string("sha256:v1:assetbytes"),
                "provenance_hash": .string("sha256:v1:provenance"),
                "operation": .string("generate"),
                "width": .number(512),
                "height": .number(512)
            ])
        )

        store.applyLiveExecutionEvents(
            [event, event],
            conversationID: "a",
            projectID: "mock-project",
            turnID: "turn-live"
        )

        let timeline = store.timelineItems(for: "a")
        XCTAssertEqual(timeline.filter { $0.id == "timeline-event-evt-image" }.count, 1)
        let item = timeline.first { $0.id == "timeline-event-evt-image" }
        XCTAssertEqual(item?.kind, .imageArtifact)
        XCTAssertEqual(item?.state, "image_artifact_created")
        XCTAssertEqual(item?.payload.contentRedacted, "Image artifact cli-image-asset created.")
        XCTAssertEqual(item?.payload.assetId, "cli-image-asset")
        XCTAssertEqual(item?.payload.providerId, "provider-1")
        XCTAssertEqual(item?.payload.modelId, "provider-1/image-model")
        XCTAssertEqual(item?.payload.path, ".opensks/assets/candidates/cli-image-asset.ppm")
        XCTAssertEqual(item?.payload.contentHash, "sha256:v1:assetbytes")
        XCTAssertEqual(item?.payload.provenanceHash, "sha256:v1:provenance")
        XCTAssertEqual(item?.payload.operation, "generate")
        XCTAssertEqual(item?.payload.width, 512)
        XCTAssertEqual(item?.payload.height, 512)
    }

    func testLiveGitReceiptEventsRenderReceiptCards() async throws {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        let commit = executionEvent(
            id: "evt-git-commit",
            runID: "run-git",
            sequence: 8,
            kind: .gitCommitReceipt,
            message: "Commit deadbeef recorded.",
            payload: .object([
                "content_redacted": .string("Commit deadbeef recorded."),
                "commit": .string("deadbeefcafef00d"),
                "paths": .array([.string("src/lib.rs"), .string("README.md")]),
                "message": .string("ship it"),
                "committed": .bool(true),
                "source_schema": .string("opensks.git-commit.v1")
            ])
        )
        let push = executionEvent(
            id: "evt-git-push",
            runID: "run-git",
            sequence: 9,
            kind: .gitPushReceipt,
            message: "Push cafebabe to origin/feature recorded.",
            payload: .object([
                "content_redacted": .string("Push cafebabe to origin/feature recorded."),
                "remote": .string("origin"),
                "ref": .string("feature"),
                "remote_oid": .string("cafebabecafebabe"),
                "local_oid": .string("feedfacefeedface"),
                "already_done": .bool(false),
                "pushed": .bool(true),
                "intent_id": .string("intent-1"),
                "effect_digest": .string("fnv1a64:1234"),
                "idempotency_key": .string("push:intent-1:feedface"),
                "remote_url_redacted": .string("https://github.com/acme/repo.git"),
                "approval_id": .string("approval-1"),
                "approval_matched": .bool(true),
                "source_schema": .string("opensks.push-receipt.v1")
            ])
        )

        store.applyLiveExecutionEvents(
            [push, commit],
            conversationID: "a",
            projectID: "mock-project",
            turnID: "turn-git"
        )

        let timeline = store.timelineItems(for: "a")
        let commitItem = try XCTUnwrap(timeline.first { $0.id == "timeline-event-evt-git-commit" })
        XCTAssertEqual(commitItem.kind, .commitReceipt)
        XCTAssertEqual(commitItem.state, "git_commit_receipt")
        XCTAssertEqual(commitItem.payload.contentRedacted, "Commit deadbeef recorded.")
        XCTAssertEqual(commitItem.payload.projection, "live_execution_event")
        XCTAssertEqual(commitItem.commitCard?.commit, "deadbeefcafef00d")
        XCTAssertEqual(commitItem.commitCard?.paths, ["src/lib.rs", "README.md"])

        let pushItem = try XCTUnwrap(timeline.first { $0.id == "timeline-event-evt-git-push" })
        XCTAssertEqual(pushItem.kind, .pushReceipt)
        XCTAssertEqual(pushItem.state, "git_push_receipt")
        XCTAssertEqual(pushItem.payload.contentRedacted, "Push cafebabe to origin/feature recorded.")
        XCTAssertEqual(pushItem.pushCard?.remote, "origin")
        XCTAssertEqual(pushItem.pushCard?.ref, "feature")
        XCTAssertEqual(pushItem.pushCard?.remoteOid, "cafebabecafebabe")
    }

    func testFailedRunSurfacesDangerPillKind() async {
        let (store, _) = makeStore(summaries: [summary(id: "a")], runStateOnTurn: .failed)
        await store.load()

        await store.send(conversationID: "a", text: "trigger failure")

        let run = try? XCTUnwrap(store.runs(for: "a").first)
        XCTAssertEqual(run?.runState, .failed)
        XCTAssertEqual(run?.runState.pillKind, .danger)
        XCTAssertEqual(store.messages.last?.state, .failed)
        XCTAssertEqual(store.selectedSummary?.status, .failed)
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
        let composer = ConversationComposer(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore()),
            conversationID: "a"
        )
            .frame(width: 720, height: 128)
        let renderer = ImageRenderer(content: composer)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation composer must render non-nil")
    }

    func testThreadWithRunCardRendersNonNil() async throws {
        let (store, _) = makeStore(summaries: [summary(id: "a")])
        await store.load()
        await store.send(conversationID: "a", text: "render me")
        let thread = ConversationThreadView(
            store: store,
            providers: ProviderStore(secretStore: InMemoryProviderSecretStore())
        )
            .frame(width: 720, height: 600)
        XCTAssertNotNil(ImageRenderer(content: thread).nsImage,
                        "thread with composer + run card must render non-nil")
    }

    private func executionEvent(
        id: String,
        runID: String,
        sequence: UInt64,
        kind: ExecutionEventKind,
        message: String,
        payload: JSONValue? = nil
    ) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope(
            schema: "opensks.execution-event-envelope.v1",
            id: id,
            runId: runID,
            sequence: sequence,
            occurredAt: "1.000000000",
            actor: "test",
            causationId: nil,
            correlationId: nil,
            kind: kind,
            payload: payload ?? .object(["message": .string(message)]),
            sensitivity: .public,
            evidenceRefs: []
        )
    }

    private func waitForSubscribeCalls(
        _ mock: MockConversationService,
        atLeast count: Int = 1,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        for _ in 0..<50 {
            if mock.subscribeRunEventsCallCount >= count { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("timed out waiting for run event subscription", file: file, line: line)
    }

    private func waitForSupervisorTicks(
        _ mock: MockConversationService,
        atLeast count: Int,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        for _ in 0..<100 {
            if mock.supervisorTickCount >= count { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("timed out waiting for supervisor ticks", file: file, line: line)
    }

    private func waitForAssistantState(
        _ store: ConversationStore,
        conversationID: String,
        state: MessageState,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        for _ in 0..<100 {
            if store.messages.last?.conversationId == conversationID,
               store.messages.last?.role == .assistant,
               store.messages.last?.state == state {
                return
            }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("timed out waiting for assistant state \(state.rawValue)", file: file, line: line)
    }

    private func waitForTimelineItem(
        _ store: ConversationStore,
        conversationID: String,
        id: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async -> ConversationTimelineItem? {
        for _ in 0..<50 {
            if let item = store.timelineItems(for: conversationID).first(where: { $0.id == id }) {
                return item
            }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("timed out waiting for timeline item \(id)", file: file, line: line)
        return nil
    }
}

private final class GatedTurnStartConversationService: ConversationService, @unchecked Sendable {
    private let base: MockConversationService
    private let gatedConversationID: String?
    private let gatedThreadSettingsConversationID: String?
    private let threadSettingsError: NSError?
    private let turnStartErrorAfterCommit: NSError?
    private let lock = NSLock()
    private var gateContinuation: CheckedContinuation<Void, Never>?
    private var enteredContinuation: CheckedContinuation<Void, Never>?
    private var enteredGate = false
    private var threadSettingsGateContinuation: CheckedContinuation<Void, Never>?
    private var threadSettingsEnteredContinuation: CheckedContinuation<Void, Never>?
    private var enteredThreadSettingsGate = false

    init(
        base: MockConversationService,
        gatedConversationID: String? = nil,
        gatedThreadSettingsConversationID: String? = nil,
        threadSettingsError: NSError? = nil,
        turnStartErrorAfterCommit: NSError? = nil
    ) {
        self.base = base
        self.gatedConversationID = gatedConversationID
        self.gatedThreadSettingsConversationID = gatedThreadSettingsConversationID
        self.threadSettingsError = threadSettingsError
        self.turnStartErrorAfterCommit = turnStartErrorAfterCommit
    }

    func waitUntilGatedTurnStartEntered() async {
        await withCheckedContinuation { continuation in
            lock.lock()
            if enteredGate {
                lock.unlock()
                continuation.resume()
            } else {
                enteredContinuation = continuation
                lock.unlock()
            }
        }
    }

    func waitUntilGatedThreadSettingsSaveEntered() async {
        await withCheckedContinuation { continuation in
            lock.lock()
            if enteredThreadSettingsGate {
                lock.unlock()
                continuation.resume()
            } else {
                threadSettingsEnteredContinuation = continuation
                lock.unlock()
            }
        }
    }

    func releaseGate() {
        lock.lock()
        let continuation = gateContinuation
        gateContinuation = nil
        lock.unlock()
        continuation?.resume()
    }

    func releaseThreadSettingsGate() {
        lock.lock()
        let continuation = threadSettingsGateContinuation
        threadSettingsGateContinuation = nil
        lock.unlock()
        continuation?.resume()
    }

    func list(filter: ConversationFilter, limit: Int?) async throws -> ConversationList {
        try await base.list(filter: filter, limit: limit)
    }

    func create(title: String) async throws -> ConversationSummary {
        try await base.create(title: title)
    }

    func rename(id: String, title: String) async throws {
        try await base.rename(id: id, title: title)
    }

    func setPinned(id: String, pinned: Bool) async throws {
        try await base.setPinned(id: id, pinned: pinned)
    }

    func setArchived(id: String, archived: Bool) async throws {
        try await base.setArchived(id: id, archived: archived)
    }

    func delete(id: String) async throws {
        try await base.delete(id: id)
    }

    func fork(id: String, afterSequence: Int64?) async throws -> ConversationSummary {
        try await base.fork(id: id, afterSequence: afterSequence)
    }

    func messages(id: String, beforeSequence: Int64?, limit: Int?) async throws -> MessagePage {
        try await base.messages(id: id, beforeSequence: beforeSequence, limit: limit)
    }

    func append(id: String, role: MessageRole, text: String) async throws -> ConversationMessage {
        try await base.append(id: id, role: role, text: text)
    }

    func timeline(conversationID: String, limit: Int?) async throws -> ConversationTimeline {
        try await base.timeline(conversationID: conversationID, limit: limit)
    }

    func appendTimelineItem(
        conversationID: String,
        kind: ConversationTimelineItemKind,
        state: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem {
        try await base.appendTimelineItem(
            conversationID: conversationID,
            kind: kind,
            state: state,
            payloadJSON: payloadJSON
        )
    }

    func appendGitReceiptEvent(
        conversationID: String,
        kind: ExecutionEventKind,
        idempotencyKey: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem {
        try await base.appendGitReceiptEvent(
            conversationID: conversationID,
            kind: kind,
            idempotencyKey: idempotencyKey,
            payloadJSON: payloadJSON
        )
    }

    func turnStart(
        conversationID: String,
        projectID: String,
        text: String,
        settings: ConversationTurnSettings?,
        threadSettingsUpdatedAtMs: Int64?,
        context: TurnContextSelection,
        idempotencyKey: String
    ) async throws -> ConversationTurn {
        if conversationID == gatedConversationID {
            await withCheckedContinuation { continuation in
                lock.lock()
                enteredGate = true
                let entered = enteredContinuation
                enteredContinuation = nil
                gateContinuation = continuation
                lock.unlock()
                entered?.resume()
            }
        }
        let turn = try await base.turnStart(
            conversationID: conversationID,
            projectID: projectID,
            text: text,
            settings: settings,
            threadSettingsUpdatedAtMs: threadSettingsUpdatedAtMs,
            context: context,
            idempotencyKey: idempotencyKey
        )
        if let turnStartErrorAfterCommit {
            throw turnStartErrorAfterCommit
        }
        return turn
    }

    func supervisorTick(runID: String? = nil) async throws -> TurnSupervisorTickResult {
        try await base.supervisorTick(runID: runID)
    }

    func subscribeRunEvents(
        runID: String,
        sinceSequence: UInt64,
        tailMs: UInt64?,
        pollIntervalMs: UInt64?
    ) async throws -> EngineRunStream {
        try await base.subscribeRunEvents(
            runID: runID,
            sinceSequence: sinceSequence,
            tailMs: tailMs,
            pollIntervalMs: pollIntervalMs
        )
    }

    func runs(conversationID: String) async throws -> [ConversationRunRef] {
        try await base.runs(conversationID: conversationID)
    }

    func threadSettings(conversationID: String) async throws -> ConversationThreadSettings {
        try await base.threadSettings(conversationID: conversationID)
    }

    func setThreadSettings(
        _ settings: ConversationThreadSettings,
        conversationID: String
    ) async throws -> ConversationThreadSettings {
        if conversationID == gatedThreadSettingsConversationID {
            await withCheckedContinuation { continuation in
                lock.lock()
                enteredThreadSettingsGate = true
                let entered = threadSettingsEnteredContinuation
                threadSettingsEnteredContinuation = nil
                threadSettingsGateContinuation = continuation
                lock.unlock()
                entered?.resume()
            }
        }
        if let threadSettingsError {
            throw threadSettingsError
        }
        return try await base.setThreadSettings(settings, conversationID: conversationID)
    }
}
