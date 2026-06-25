import SwiftUI
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
            titleSource: "manual",
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
        {"schema":"opensks.conversation-summary.v1","id":"c1","project_id":"p1","title":"Hello","title_source":"manual","status":"running","pinned":true,"archived":false,"message_count":3,"created_at_ms":10,"updated_at_ms":20,"last_message_at_ms":25}
        """
        let decoded = try JSONDecoder.opensks.decode(ConversationSummary.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.id, "c1")
        XCTAssertEqual(decoded.projectId, "p1")
        XCTAssertEqual(decoded.status, .running)
        XCTAssertTrue(decoded.pinned)
        XCTAssertEqual(decoded.messageCount, 3)
        XCTAssertEqual(decoded.lastMessageAtMs, 25)
        XCTAssertEqual(decoded.activityMs, 25)
    }

    func testUnknownEnumValuesFallBackInsteadOfThrowing() throws {
        let json = """
        {"schema":"s","id":"m1","project_id":"p1","conversation_id":"c1","turn_id":null,"role":"future_role","state":"buffering","content_redacted":"hi","sequence":1,"created_at_ms":1,"updated_at_ms":1}
        """
        let decoded = try JSONDecoder.opensks.decode(ConversationMessage.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.role, .unknown)
        XCTAssertEqual(decoded.state, .unknown)
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
        let sidebar = ConversationSidebar(store: store, projectName: "OpenSKS")
            .frame(width: 280, height: 600)
        let renderer = ImageRenderer(content: sidebar)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "conversation sidebar must render non-nil")
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
