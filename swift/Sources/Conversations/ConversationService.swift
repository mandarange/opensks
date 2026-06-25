// ConversationService.swift — the persistence boundary for conversations. Most
// CRUD verbs still shell the bundled `opensks-cli conversation <sub>` exactly
// like `AppState` resolves and runs the CLI, but live turn-start now uses the
// persistent daemon typed `conversation_turn_start` request and returns as soon
// as the durable accepted handle is committed. A `MockConversationService` backs
// tests and previews with in-memory arrays so the UI never needs a process.

import Foundation

// MARK: - Protocol

protocol ConversationService: Sendable {
    func list(filter: ConversationFilter, limit: Int?) async throws -> ConversationList
    func create(title: String) async throws -> ConversationSummary
    func rename(id: String, title: String) async throws
    func setPinned(id: String, pinned: Bool) async throws
    func setArchived(id: String, archived: Bool) async throws
    func delete(id: String) async throws
    func fork(id: String, afterSequence: Int64?) async throws -> ConversationSummary
    func messages(id: String, beforeSequence: Int64?, limit: Int?) async throws -> MessagePage
    func append(id: String, role: MessageRole, text: String) async throws -> ConversationMessage
    func timeline(conversationID: String, limit: Int?) async throws -> ConversationTimeline
    func appendTimelineItem(
        conversationID: String,
        kind: ConversationTimelineItemKind,
        state: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem
    func appendGitReceiptEvent(
        conversationID: String,
        kind: ExecutionEventKind,
        idempotencyKey: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem

    /// Start ONE conversation runtime run for `id`: persist the redacted user
    /// message + an assistant placeholder, run the engine, set the assistant
    /// content from the result, link the run, and return the ids. Passing the
    /// same `idempotencyKey` again returns the SAME ids with `reused == true`
    /// and does NOT start a second run.
    func turnStart(
        conversationID: String,
        projectID: String,
        text: String,
        settings: ConversationTurnSettings?,
        threadSettingsUpdatedAtMs: Int64?,
        context: TurnContextSelection,
        idempotencyKey: String
    ) async throws -> ConversationTurn

    /// Ask the daemon turn supervisor to recover expired leases, claim at most
    /// one queued accepted turn, execute it, and persist its final projections.
    func supervisorTick() async throws -> TurnSupervisorTickResult

    /// Subscribe to a run's durable execution-event stream. Live chat uses this
    /// as the product path for incremental run progress; reloads only reconcile
    /// the durable projection afterward.
    func subscribeRunEvents(
        runID: String,
        sinceSequence: UInt64,
        tailMs: UInt64?,
        pollIntervalMs: UInt64?
    ) async throws -> EngineRunStream

    /// The runs linked to a conversation (`opensks.conversation-run-list.v1`).
    func runs(conversationID: String) async throws -> [ConversationRunRef]

    /// Durable per-thread Chat settings (`opensks.thread-settings.v1`).
    func threadSettings(conversationID: String) async throws -> ConversationThreadSettings
    func setThreadSettings(
        _ settings: ConversationThreadSettings,
        conversationID: String
    ) async throws -> ConversationThreadSettings
}

extension ConversationService {
    func turnStart(
        conversationID: String,
        projectID: String,
        text: String,
        settings: ConversationTurnSettings? = nil,
        context: TurnContextSelection,
        idempotencyKey: String
    ) async throws -> ConversationTurn {
        try await turnStart(
            conversationID: conversationID,
            projectID: projectID,
            text: text,
            settings: settings,
            threadSettingsUpdatedAtMs: nil,
            context: context,
            idempotencyKey: idempotencyKey
        )
    }

    func turnStart(
        conversationID: String,
        projectID: String,
        text: String,
        idempotencyKey: String
    ) async throws -> ConversationTurn {
        try await turnStart(
            conversationID: conversationID,
            projectID: projectID,
            text: text,
            settings: nil,
            threadSettingsUpdatedAtMs: nil,
            context: .empty,
            idempotencyKey: idempotencyKey
        )
    }
}

// MARK: - Errors

enum ConversationServiceError: LocalizedError {
    case emptyOutput(String)
    case decodeFailed(String, underlying: String)
    case nonZeroExit(Int32, stderr: String)
    case launchFailed(String)

    var errorDescription: String? {
        switch self {
        case .emptyOutput(let verb):
            return "opensks conversation \(verb) returned no output"
        case .decodeFailed(let verb, let underlying):
            return "could not decode conversation \(verb): \(underlying)"
        case .nonZeroExit(let code, let stderr):
            let trimmed = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            return "conversation command exited \(code)\(trimmed.isEmpty ? "" : ": \(trimmed)")"
        case .launchFailed(let message):
            return "could not start conversation command: \(message)"
        }
    }
}

// MARK: - Live service

/// Shells the bundled `opensks-cli`. The `cli` URL and `workspace` URL are the
/// SAME values `AppState` computes (bundled `opensks-cli` resource, workspace
/// path from `workspace-path.txt` / cwd), so live conversations are scoped to the
/// per-workspace default project created by the CLI.
struct LiveConversationService: ConversationService {
    let cli: URL
    let workspace: URL
    let engine = EngineProcess()
    private static let chatSupervisorId = "swift-chat-supervisor"
    private static let chatSupervisorLeaseTtlMs: UInt64 = 30_000

    func list(filter: ConversationFilter, limit: Int?) async throws -> ConversationList {
        var args = ["conversation", "list", "--workspace", workspace.path, "--filter", filter.rawValue]
        if let limit { args += ["--limit", String(limit)] }
        return try await run(args, verb: "list")
    }

    func create(title: String) async throws -> ConversationSummary {
        try await run(
            ["conversation", "create", "--workspace", workspace.path, "--title", title],
            verb: "create"
        )
    }

    func rename(id: String, title: String) async throws {
        let _: ConversationAck = try await run(
            ["conversation", "rename", "--workspace", workspace.path, "--conversation", id, "--title", title],
            verb: "rename"
        )
    }

    func setPinned(id: String, pinned: Bool) async throws {
        let _: ConversationAck = try await run(
            ["conversation", pinned ? "pin" : "unpin", "--workspace", workspace.path, "--conversation", id],
            verb: pinned ? "pin" : "unpin"
        )
    }

    func setArchived(id: String, archived: Bool) async throws {
        let _: ConversationAck = try await run(
            ["conversation", archived ? "archive" : "unarchive", "--workspace", workspace.path, "--conversation", id],
            verb: archived ? "archive" : "unarchive"
        )
    }

    func delete(id: String) async throws {
        // The delete envelope is `{"ok":true,"messages":N,"runs":N}`; decoding it
        // as `ConversationAck` (which only reads `ok`) is sufficient.
        let _: ConversationAck = try await run(
            ["conversation", "delete", "--workspace", workspace.path, "--conversation", id],
            verb: "delete"
        )
    }

    func fork(id: String, afterSequence: Int64?) async throws -> ConversationSummary {
        var args = ["conversation", "fork", "--workspace", workspace.path, "--conversation", id]
        if let afterSequence { args += ["--after-sequence", String(afterSequence)] }
        return try await run(args, verb: "fork")
    }

    func messages(id: String, beforeSequence: Int64?, limit: Int?) async throws -> MessagePage {
        var args = ["conversation", "messages", "--workspace", workspace.path, "--conversation", id]
        if let beforeSequence { args += ["--before-sequence", String(beforeSequence)] }
        if let limit { args += ["--limit", String(limit)] }
        return try await run(args, verb: "messages")
    }

    func append(id: String, role: MessageRole, text: String) async throws -> ConversationMessage {
        try await run(
            ["conversation", "append", "--workspace", workspace.path,
             "--conversation", id, "--role", role.rawValue, "--text", text],
            verb: "append"
        )
    }

    func timeline(conversationID id: String, limit: Int?) async throws -> ConversationTimeline {
        var args = ["conversation", "timeline", "--workspace", workspace.path, "--conversation", id]
        if let limit { args += ["--limit", String(limit)] }
        return try await run(args, verb: "timeline")
    }

    func appendTimelineItem(
        conversationID id: String,
        kind: ConversationTimelineItemKind,
        state: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem {
        try await run(
            ["conversation", "timeline-append", "--workspace", workspace.path,
             "--conversation", id, "--kind", kind.rawValue, "--state", state, "--payload", payloadJSON],
            verb: "timeline-append"
        )
    }

    func appendGitReceiptEvent(
        conversationID id: String,
        kind: ExecutionEventKind,
        idempotencyKey: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem {
        try await run(
            ["conversation", "receipt-event-append", "--workspace", workspace.path,
             "--conversation", id, "--kind", kind.rawValue, "--idempotency-key", idempotencyKey,
             "--payload", payloadJSON],
            verb: "receipt-event-append"
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
        let requestId = "req-conversation-turn-\(UUID().uuidString)"
        let request = ConversationTurnStartRequest(
            schema: "opensks.conversation-turn-start-request.v1",
            requestId: requestId,
            projectId: projectID,
            conversationId: conversationID,
            clientTurnId: "client-\(UUID().uuidString)",
            message: UserMessageInput(text: text, attachmentRefs: []),
            threadSettingsUpdatedAtMs: threadSettingsUpdatedAtMs,
            settings: settings,
            context: context,
            idempotencyKey: idempotencyKey
        )
        let result = await engine.conversationTurnStart(cli: cli, cwd: workspace, request: request)
        if result.stream.exitCode != 0 {
            throw ConversationServiceError.nonZeroExit(
                result.stream.exitCode ?? 1,
                stderr: Self.daemonErrorText(result.stream)
            )
        }
        guard let accepted = result.accepted else {
            throw ConversationServiceError.emptyOutput("turn-start")
        }
        guard accepted.requestId == requestId else {
            throw ConversationServiceError.decodeFailed(
                "turn-start",
                underlying: "accepted request_id \(accepted.requestId) did not match \(requestId)"
            )
        }
        return ConversationTurn(
            schema: "opensks.conversation-turn.v1",
            turnId: accepted.turnId,
            userMessageId: accepted.userMessageId,
            assistantMessageId: accepted.assistantMessageId,
            runId: accepted.runId,
            runState: accepted.state,
            reused: false
        )
    }

    func supervisorTick() async throws -> TurnSupervisorTickResult {
        let requestId = "req-conversation-supervisor-\(UUID().uuidString)"
        let result = await engine.conversationSupervisorTick(
            cli: cli,
            cwd: workspace,
            requestId: requestId,
            supervisorId: Self.chatSupervisorId,
            leaseTtlMs: Self.chatSupervisorLeaseTtlMs
        )
        if result.stream.exitCode != 0 {
            throw ConversationServiceError.nonZeroExit(
                result.stream.exitCode ?? 1,
                stderr: Self.daemonErrorText(result.stream)
            )
        }
        guard let tick = result.tick else {
            throw ConversationServiceError.emptyOutput("supervisor-tick")
        }
        guard tick.requestId == requestId else {
            throw ConversationServiceError.decodeFailed(
                "supervisor-tick",
                underlying: "tick request_id \(tick.requestId) did not match \(requestId)"
            )
        }
        return tick
    }

    func subscribeRunEvents(
        runID: String,
        sinceSequence: UInt64,
        tailMs: UInt64?,
        pollIntervalMs: UInt64?
    ) async throws -> EngineRunStream {
        let stream = await engine.subscribeEvents(
            cli: cli,
            cwd: workspace,
            runId: runID,
            sinceSequence: sinceSequence,
            tailMs: tailMs,
            pollIntervalMs: pollIntervalMs
        )
        if stream.exitCode != 0 && stream.streamFailures.isEmpty {
            throw ConversationServiceError.nonZeroExit(
                stream.exitCode ?? 1,
                stderr: Self.daemonErrorText(stream)
            )
        }
        return stream
    }

    func runs(conversationID: String) async throws -> [ConversationRunRef] {
        let list: ConversationRunList = try await run(
            ["conversation", "runs", "--workspace", workspace.path, "--conversation", conversationID],
            verb: "runs"
        )
        return list.runs
    }

    func threadSettings(conversationID: String) async throws -> ConversationThreadSettings {
        try await run(
            ["conversation", "settings-get", "--workspace", workspace.path, "--conversation", conversationID],
            verb: "settings-get"
        )
    }

    func setThreadSettings(
        _ settings: ConversationThreadSettings,
        conversationID: String
    ) async throws -> ConversationThreadSettings {
        var normalized = settings
        normalized.conversationId = conversationID
        let encoded = try JSONEncoder.opensks.encode(normalized)
        let json = String(decoding: encoded, as: UTF8.self)
        return try await run(
            ["conversation", "settings-set", "--workspace", workspace.path,
             "--conversation", conversationID, "--settings", json],
            verb: "settings-set"
        )
    }

    // MARK: Process plumbing

    private func run<T: Decodable>(_ args: [String], verb: String) async throws -> T {
        let result = try await Self.capture(cli: cli, cwd: workspace, args: args)
        if result.exitCode != 0 {
            throw ConversationServiceError.nonZeroExit(result.exitCode, stderr: result.stderr)
        }
        guard !result.stdout.isEmpty else {
            throw ConversationServiceError.emptyOutput(verb)
        }
        do {
            return try JSONDecoder.opensks.decode(T.self, from: result.stdout)
        } catch {
            throw ConversationServiceError.decodeFailed(verb, underlying: error.localizedDescription)
        }
    }

    private static func daemonErrorText(_ stream: EngineRunStream) -> String {
        let eventText = stream.engineEvents
            .filter { $0.severity.isError }
            .map(\.message)
            .joined(separator: "\n")
        let pieces = [eventText, stream.stderr]
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        return pieces.joined(separator: "\n")
    }

    private struct CaptureResult {
        let stdout: Data
        let stderr: String
        let exitCode: Int32
    }

    /// Off-main-actor process capture, mirroring `CLIRunner.capture` but also
    /// returning stderr + exit code so failures surface honestly.
    private static func capture(cli: URL, cwd: URL, args: [String]) async throws -> CaptureResult {
        // Shared child-process runner: concurrent drain + cancel-kill (§19.2).
        do {
            let result = try await ProcessSupervisor().run(
                ProcessSupervisor.Spec(
                    executable: cli,
                    arguments: args,
                    workingDirectory: cwd
                )
            )
            return CaptureResult(
                stdout: result.stdout,
                stderr: String(decoding: result.stderr, as: UTF8.self),
                exitCode: result.exitCode
            )
        } catch {
            throw ConversationServiceError.launchFailed(error.localizedDescription)
        }
    }
}

// MARK: - Mock service

/// In-memory implementation for tests and previews. Thread-safe via an internal
/// lock so it can be shared across the `Sendable` boundary like the live one.
final class MockConversationService: ConversationService, @unchecked Sendable {
    private let lock = NSLock()
    private var summaries: [ConversationSummary]
    private var messagesByConversation: [String: [ConversationMessage]]
    private var timelineItemsByConversation: [String: [ConversationTimelineItem]] = [:]
    private var runsByConversation: [String: [ConversationRunRef]] = [:]
    private var settingsByConversation: [String: ConversationThreadSettings] = [:]
    private var executionEventsByRun: [String: [ExecutionEventEnvelope]] = [:]
    private var defaultSubscribeRunFailure: EngineStreamFailure?
    /// Idempotency ledger: `"<conversationID>\u{1}<key>" -> turn` so a replayed
    /// key returns the same ids (reused) without starting a second run.
    private var turnsByIdempotencyKey: [String: ConversationTurn] = [:]
    private var nextId = 0
    private var clock: Int64 = 1_000
    private(set) var supervisorTickCount = 0
    private(set) var subscribeRunEventsCallCount = 0
    private(set) var subscribeRunEventsRequests: [(runID: String, sinceSequence: UInt64, tailMs: UInt64?, pollIntervalMs: UInt64?)] = []
    private(set) var turnStartRequests: [(
        conversationID: String,
        projectID: String,
        text: String,
        settings: ConversationTurnSettings?,
        threadSettingsUpdatedAtMs: Int64?,
        context: TurnContextSelection,
        idempotencyKey: String
    )] = []

    let projectId: String

    /// State the mock reports for the accepted run. Defaults to `.queued`,
    /// matching the daemon accepted-handle path; a test can request `.failed` to
    /// exercise the danger pill without touching the engine.
    let runStateOnTurn: RunState

    init(
        projectId: String = "mock-project",
        summaries: [ConversationSummary] = [],
        messages: [String: [ConversationMessage]] = [:],
        runStateOnTurn: RunState = .queued
    ) {
        self.projectId = projectId
        self.summaries = summaries
        self.messagesByConversation = messages
        self.runStateOnTurn = runStateOnTurn
    }

    private func tick() -> Int64 {
        clock += 1
        return clock
    }

    private func mintId(_ prefix: String) -> String {
        nextId += 1
        return "\(prefix)-\(nextId)"
    }

    /// Synchronous scoped lock — async methods funnel mutations through this so
    /// they never hold an `NSLock` across a suspension point.
    private func withLock<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try body()
    }

    func list(filter: ConversationFilter, limit: Int?) async throws -> ConversationList {
        try withLock { try listLocked(filter: filter, limit: limit) }
    }

    private func listLocked(filter: ConversationFilter, limit: Int?) throws -> ConversationList {
        var items = summaries
        switch filter {
        case .all: items = items.filter { !$0.archived }
        case .running: items = items.filter { $0.status == .running && !$0.archived }
        case .pinned: items = items.filter { $0.pinned && !$0.archived }
        case .archived: items = items.filter { $0.archived }
        }
        items.sort { lhs, rhs in
            if lhs.pinned != rhs.pinned { return lhs.pinned && !rhs.pinned }
            return lhs.activityMs > rhs.activityMs
        }
        if let limit, items.count > limit { items = Array(items.prefix(limit)) }
        return ConversationList(schema: "opensks.conversation-list.v1", projectId: projectId, conversations: items)
    }

    func create(title: String) async throws -> ConversationSummary {
        withLock {
        let now = tick()
        let summary = ConversationSummary(
            schema: "opensks.conversation-summary.v1",
            id: mintId("conv"),
            projectId: projectId,
            title: title.isEmpty ? "Untitled" : title,
            titleSource: "manual",
            status: .idle,
            pinned: false,
            archived: false,
            messageCount: 0,
            createdAtMs: now,
            updatedAtMs: now,
            lastMessageAtMs: nil
        )
        summaries.append(summary)
        return summary
        }
    }

    func rename(id: String, title: String) async throws {
        try mutate(id) { $0.with(title: title, titleSource: "manual", updatedAtMs: tick()) }
    }

    func setPinned(id: String, pinned: Bool) async throws {
        try mutate(id) { $0.with(pinned: pinned, updatedAtMs: tick()) }
    }

    func setArchived(id: String, archived: Bool) async throws {
        try mutate(id) {
            $0.with(
                status: archived ? .archived : .idle,
                archived: archived,
                updatedAtMs: tick()
            )
        }
    }

    func delete(id: String) async throws {
        withLock {
            summaries.removeAll { $0.id == id }
            messagesByConversation[id] = nil
            timelineItemsByConversation[id] = nil
            runsByConversation[id] = nil
            settingsByConversation[id] = nil
            turnsByIdempotencyKey = turnsByIdempotencyKey.filter { !$0.key.hasPrefix("\(id)\u{1}") }
        }
    }

    func fork(id: String, afterSequence: Int64?) async throws -> ConversationSummary {
        try withLock { try forkLocked(id: id, afterSequence: afterSequence) }
    }

    private func forkLocked(id: String, afterSequence: Int64?) throws -> ConversationSummary {
        guard let source = summaries.first(where: { $0.id == id }) else {
            throw ConversationServiceError.emptyOutput("fork")
        }
        let now = tick()
        let copiedMessages = (messagesByConversation[id] ?? []).filter { msg in
            guard let cutoff = afterSequence else { return true }
            return msg.sequence <= cutoff
        }
        let fork = ConversationSummary(
            schema: "opensks.conversation-summary.v1",
            id: mintId("conv"),
            projectId: projectId,
            title: "\(source.title) (fork)",
            titleSource: "fork",
            status: .idle,
            pinned: false,
            archived: false,
            messageCount: copiedMessages.count,
            createdAtMs: now,
            updatedAtMs: now,
            lastMessageAtMs: copiedMessages.last?.createdAtMs
        )
        summaries.append(fork)
        messagesByConversation[fork.id] = copiedMessages.map { $0.with(conversationId: fork.id) }
        return fork
    }

    func messages(id: String, beforeSequence: Int64?, limit: Int?) async throws -> MessagePage {
        withLock { messagesLocked(id: id, beforeSequence: beforeSequence, limit: limit) }
    }

    private func messagesLocked(id: String, beforeSequence: Int64?, limit: Int?) -> MessagePage {
        let all = (messagesByConversation[id] ?? []).sorted { $0.sequence < $1.sequence }
        var older = all
        if let beforeSequence { older = older.filter { $0.sequence < beforeSequence } }
        let pageLimit = limit ?? all.count
        // Take the newest `pageLimit` of the eligible (older) window, oldest->newest.
        let page = Array(older.suffix(pageLimit))
        let hasMore = page.count == pageLimit && older.count > pageLimit
        return MessagePage(conversationId: id, messages: page, hasMore: hasMore)
    }

    func append(id: String, role: MessageRole, text: String) async throws -> ConversationMessage {
        withLock { appendLocked(id: id, role: role, text: text) }
    }

    func timeline(conversationID id: String, limit: Int?) async throws -> ConversationTimeline {
        withLock { timelineLocked(conversationID: id, limit: limit) }
    }

    func appendTimelineItem(
        conversationID id: String,
        kind: ConversationTimelineItemKind,
        state: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem {
        try withLock {
            guard summaries.contains(where: { $0.id == id }) else {
                throw ConversationServiceError.emptyOutput("timeline-append")
            }
            let payload = try JSONDecoder.opensks.decode(
                ConversationTimelinePayload.self,
                from: Data(payloadJSON.utf8)
            )
            let now = tick()
            let existing = timelineItemsByConversation[id] ?? []
            let messageMax = (messagesByConversation[id] ?? []).map(\.sequence).max() ?? 0
            let timelineMax = existing.map(\.sequence).max() ?? 0
            let item = ConversationTimelineItem(
                schema: "opensks.timeline-item.v1",
                id: mintId("timeline"),
                projectId: projectId,
                conversationId: id,
                turnId: nil,
                runId: nil,
                sequence: max(messageMax, timelineMax) + 1,
                kind: kind,
                state: state,
                payload: payload,
                createdAtMs: now,
                updatedAtMs: now
            )
            timelineItemsByConversation[id] = existing + [item]
            if let idx = summaries.firstIndex(where: { $0.id == id }) {
                summaries[idx] = summaries[idx].with(updatedAtMs: now)
            }
            return item
        }
    }

    func appendGitReceiptEvent(
        conversationID id: String,
        kind: ExecutionEventKind,
        idempotencyKey: String,
        payloadJSON: String
    ) async throws -> ConversationTimelineItem {
        try withLock {
            guard summaries.contains(where: { $0.id == id }) else {
                throw ConversationServiceError.emptyOutput("receipt-event-append")
            }
            let payload = try JSONDecoder.opensks.decode(
                ConversationTimelinePayload.self,
                from: Data(payloadJSON.utf8)
            )
            let now = tick()
            let existing = timelineItemsByConversation[id] ?? []
            let durableID = "timeline-event-\(idempotencyKey)"
            if let already = existing.first(where: { $0.id == durableID }) {
                return already
            }
            let messageMax = (messagesByConversation[id] ?? []).map(\.sequence).max() ?? 0
            let timelineMax = existing.map(\.sequence).max() ?? 0
            let itemKind: ConversationTimelineItemKind
            let state: String
            switch kind {
            case .gitCommitReceipt:
                itemKind = .commitReceipt
                state = "committed"
            case .gitPushReceipt:
                itemKind = .pushReceipt
                state = "pushed"
            case .gitPushFailed:
                itemKind = .pushReceipt
                state = "failed"
            default:
                itemKind = .warning
                state = kind.rawValue
            }
            let item = ConversationTimelineItem(
                schema: "opensks.timeline-item.v1",
                id: durableID,
                projectId: projectId,
                conversationId: id,
                turnId: nil,
                runId: "git-receipt-\(idempotencyKey)",
                sequence: max(messageMax, timelineMax) + 1,
                kind: itemKind,
                state: state,
                payload: payload,
                createdAtMs: now,
                updatedAtMs: now
            )
            timelineItemsByConversation[id] = existing + [item]
            if let idx = summaries.firstIndex(where: { $0.id == id }) {
                summaries[idx] = summaries[idx].with(updatedAtMs: now)
            }
            return item
        }
    }

    private func appendLocked(id: String, role: MessageRole, text: String) -> ConversationMessage {
        let now = tick()
        let existing = messagesByConversation[id] ?? []
        let message = ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: mintId("msg"),
            projectId: projectId,
            conversationId: id,
            turnId: nil,
            role: role,
            state: .complete,
            contentRedacted: text,
            sequence: Int64(existing.count + 1),
            createdAtMs: now,
            updatedAtMs: now
        )
        messagesByConversation[id] = existing + [message]
        if let idx = summaries.firstIndex(where: { $0.id == id }) {
            summaries[idx] = summaries[idx].with(
                messageCount: existing.count + 1,
                updatedAtMs: now,
                lastMessageAtMs: now
            )
        }
        return message
    }

    private func timelineLocked(conversationID id: String, limit: Int?) -> ConversationTimeline {
        let page = messagesLocked(id: id, beforeSequence: nil, limit: limit)
        var runsByMessage: [String: ConversationRunRef] = [:]
        for run in runsByConversation[id] ?? [] {
            runsByMessage[run.messageId] = run
        }
        var items = page.messages.map { message -> ConversationTimelineItem in
            let run = runsByMessage[message.id]
            let kind: ConversationTimelineItemKind
            switch message.role {
            case .user:
                kind = .userMessage
            case .assistant:
                kind = .assistantMessage
            case .tool:
                kind = .toolCall
            case .event, .system, .unknown:
                kind = .warning
            }
            let state = run?.runState.rawValue ?? message.state.rawValue
            return ConversationTimelineItem(
                schema: "opensks.timeline-item.v1",
                id: "timeline-\(message.id)",
                projectId: message.projectId,
                conversationId: message.conversationId,
                turnId: message.turnId,
                runId: run?.runId,
                sequence: message.sequence,
                kind: kind,
                state: state,
                payload: ConversationTimelinePayload(
                    messageId: message.id,
                    role: message.role,
                    messageState: message.state,
                    contentRedacted: message.contentRedacted,
                    runRelation: run?.relation,
                    commit: nil,
                    paths: nil,
                    message: nil,
                    remote: nil,
                    ref: nil,
                    remoteOid: nil,
                    localOid: nil,
                    alreadyDone: nil,
                    sourceSchema: nil,
                    projection: nil,
                    committed: nil,
                    pushed: nil,
                    intentId: nil,
                    effectDigest: nil,
                    idempotencyKey: nil,
                    remoteUrlRedacted: nil,
                    remoteExpectedOid: nil,
                    protected: nil,
                    approvalId: nil,
                    approvalMatched: nil,
                    stagedDiffHash: nil,
                    stagedDiffRef: nil,
                    reviewedStagedDiffHash: nil,
                    reviewedStagedDiffRef: nil,
                    integrationFinalDiffHash: nil,
                    integrationFinalDiffRef: nil,
                    integrationRunId: nil,
                    integrationCandidateId: nil
                ),
                createdAtMs: message.createdAtMs,
                updatedAtMs: message.updatedAtMs
            )
        }
        items.append(contentsOf: timelineItemsByConversation[id] ?? [])
        items.sort { lhs, rhs in
            if lhs.sequence != rhs.sequence { return lhs.sequence < rhs.sequence }
            return lhs.id < rhs.id
        }
        if let limit, items.count > limit {
            items = Array(items.suffix(limit))
        }
        return ConversationTimeline(
            schema: "opensks.conversation-timeline.v1",
            conversationId: id,
            items: items
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
        try withLock {
            try turnStartLocked(
                conversationID: conversationID,
                projectID: projectID,
                text: text,
                settings: settings,
                threadSettingsUpdatedAtMs: threadSettingsUpdatedAtMs,
                context: context,
                idempotencyKey: idempotencyKey
            )
        }
    }

    private func turnStartLocked(
        conversationID id: String,
        projectID: String,
        text: String,
        settings: ConversationTurnSettings?,
        threadSettingsUpdatedAtMs: Int64?,
        context: TurnContextSelection,
        idempotencyKey: String
    ) throws -> ConversationTurn {
        turnStartRequests.append((
            conversationID: id,
            projectID: projectID,
            text: text,
            settings: settings,
            threadSettingsUpdatedAtMs: threadSettingsUpdatedAtMs,
            context: context,
            idempotencyKey: idempotencyKey
        ))
        guard summaries.contains(where: { $0.id == id && $0.projectId == projectID }) else {
            throw ConversationServiceError.emptyOutput("turn-start")
        }
        // Replayed idempotency key: return the SAME turn, mark reused, and do
        // NOT start a second run (no new messages, no new run-list entry).
        let ledgerKey = "\(id)\u{1}\(idempotencyKey)"
        if let existing = turnsByIdempotencyKey[ledgerKey] {
            return ConversationTurn(
                schema: existing.schema,
                turnId: existing.turnId,
                userMessageId: existing.userMessageId,
                assistantMessageId: existing.assistantMessageId,
                runId: existing.runId,
                runState: existing.runState,
                reused: true
            )
        }

        let turnId = mintId("turn")
        let runId = mintId("run")

        // 1. Persist the redacted user message BEFORE the run.
        let userMessage = appendLocked(id: id, role: .user, text: text)
        // 2. Persist the assistant placeholder BEFORE the run.
        let assistantPlaceholder = appendLocked(id: id, role: .assistant, text: "")

        // 3. Link both messages to the accepted turn. The normal accepted path
        //    leaves the assistant placeholder streaming; it does not fabricate a
        //    completed adapter result.
        let runState = runStateOnTurn
        let assistantContent = runState == .failed ? "Run failed." : "..."
        if var msgs = messagesByConversation[id],
           let idx = msgs.firstIndex(where: { $0.id == assistantPlaceholder.id }) {
            msgs[idx] = assistantPlaceholder.with(
                turnId: turnId,
                state: runState == .failed ? .failed : .streaming,
                contentRedacted: assistantContent
            )
            // Also link the user message to the turn.
            if let uIdx = msgs.firstIndex(where: { $0.id == userMessage.id }) {
                msgs[uIdx] = userMessage.with(turnId: turnId)
            }
            messagesByConversation[id] = msgs
        }

        // 4. Link the run to the conversation (primary relation).
        let runRef = ConversationRunRef(
            turnId: turnId,
            runId: runId,
            messageId: assistantPlaceholder.id,
            relation: "primary",
            runState: runState
        )
        runsByConversation[id, default: []].append(runRef)
        if let summaryIndex = summaries.firstIndex(where: { $0.id == id }) {
            let now = tick()
            summaries[summaryIndex] = summaries[summaryIndex].with(
                status: runState == .failed ? .failed : .running,
                updatedAtMs: now,
                lastMessageAtMs: now
            )
        }

        let turn = ConversationTurn(
            schema: "opensks.conversation-turn.v1",
            turnId: turnId,
            userMessageId: userMessage.id,
            assistantMessageId: assistantPlaceholder.id,
            runId: runId,
            runState: runState,
            reused: false
        )
        turnsByIdempotencyKey[ledgerKey] = turn
        executionEventsByRun[runId, default: []].append(executionEvent(
            id: "evt-\(runId)-started",
            runID: runId,
            sequence: 1,
            kind: .runStarted,
            message: "run started",
            occurredAtMs: clock
        ))
        return turn
    }

    func supervisorTick() async throws -> TurnSupervisorTickResult {
        withLock { supervisorTickLocked() }
    }

    private func supervisorTickLocked() -> TurnSupervisorTickResult {
        supervisorTickCount += 1
        for conversationID in runsByConversation.keys.sorted() {
            var runs = runsByConversation[conversationID] ?? []
            guard let runIndex = runs.firstIndex(where: { $0.runState == .queued || $0.runState == .running }) else {
                continue
            }
            let run = runs[runIndex]
            let finalRunState: RunState = runStateOnTurn == .failed ? .failed : .completed
            let now = tick()
            let assistantState: MessageState = finalRunState == .failed ? .failed : .complete
            let assistantText = finalRunState == .failed ? "Run failed." : "Mock supervisor completed."

            if var messages = messagesByConversation[conversationID],
               let messageIndex = messages.firstIndex(where: { $0.id == run.messageId }) {
                messages[messageIndex] = messages[messageIndex].with(
                    state: assistantState,
                    contentRedacted: assistantText
                )
                messagesByConversation[conversationID] = messages
            }

            runs[runIndex] = ConversationRunRef(
                turnId: run.turnId,
                runId: run.runId,
                messageId: run.messageId,
                relation: run.relation,
                runState: finalRunState
            )
            runsByConversation[conversationID] = runs

            if let summaryIndex = summaries.firstIndex(where: { $0.id == conversationID }) {
                summaries[summaryIndex] = summaries[summaryIndex].with(
                    status: finalRunState == .failed ? .failed : .completed,
                    updatedAtMs: now,
                    lastMessageAtMs: now
                )
            }
            executionEventsByRun[run.runId, default: []].append(executionEvent(
                id: "evt-\(run.runId)-terminal",
                runID: run.runId,
                sequence: 2,
                kind: finalRunState == .failed ? .verificationFailed : .snapshotWritten,
                message: assistantText,
                occurredAtMs: now
            ))

            return TurnSupervisorTickResult(
                schema: "opensks.turn-supervisor-tick.v1",
                requestId: "mock-supervisor-\(supervisorTickCount)",
                supervisorId: "mock-supervisor",
                recoveredExpiredLeases: 0,
                claimed: TurnSupervisorClaimedTurn(
                    turnId: run.turnId,
                    runId: run.runId,
                    projectId: projectId,
                    conversationId: conversationID,
                    assistantMessageId: run.messageId,
                    leaseOwner: "mock-supervisor",
                    leaseExpiresAtMs: UInt64(now + 30_000),
                    hasModelRoutingDecision: true
                ),
                executed: TurnSupervisorExecution(
                    status: "executed",
                    runState: finalRunState,
                    assistantMessageId: run.messageId,
                    lastEventSequence: 1,
                    patchCount: 0,
                    applyResultCount: 0,
                    error: nil
                )
            )
        }

        return TurnSupervisorTickResult(
            schema: "opensks.turn-supervisor-tick.v1",
            requestId: "mock-supervisor-\(supervisorTickCount)",
            supervisorId: "mock-supervisor",
            recoveredExpiredLeases: 0,
            claimed: nil,
            executed: nil
        )
    }

    func subscribeRunEvents(
        runID: String,
        sinceSequence: UInt64,
        tailMs: UInt64?,
        pollIntervalMs: UInt64?
    ) async throws -> EngineRunStream {
        withLock {
            subscribeRunEventsCallCount += 1
            subscribeRunEventsRequests.append((runID, sinceSequence, tailMs, pollIntervalMs))
            let events = (executionEventsByRun[runID] ?? [])
                .filter { $0.sequence > sinceSequence }
            return EngineRunStream(
                engineEvents: [],
                executionEvents: events,
                exitCode: 0,
                stderr: "",
                streamFailures: defaultSubscribeRunFailure.map { [$0] } ?? [],
                rawLines: []
            )
        }
    }

    func setDefaultSubscribeRunFailure(_ failure: EngineStreamFailure?) {
        withLock {
            defaultSubscribeRunFailure = failure
        }
    }

    func runs(conversationID id: String) async throws -> [ConversationRunRef] {
        withLock { runsByConversation[id] ?? [] }
    }

    func threadSettings(conversationID id: String) async throws -> ConversationThreadSettings {
        withLock { settingsByConversation[id] ?? ConversationThreadSettings.defaultFor(conversationID: id, updatedAtMs: clock) }
    }

    func setThreadSettings(
        _ settings: ConversationThreadSettings,
        conversationID id: String
    ) async throws -> ConversationThreadSettings {
        try withLock {
            guard summaries.contains(where: { $0.id == id }) else {
                throw ConversationServiceError.emptyOutput("settings-set")
            }
            var normalized = settings
            normalized.conversationId = id
            normalized.updatedAtMs = tick()
            settingsByConversation[id] = normalized
            return normalized
        }
    }

    private func mutate(_ id: String, _ transform: (ConversationSummary) -> ConversationSummary) throws {
        lock.lock(); defer { lock.unlock() }
        guard let idx = summaries.firstIndex(where: { $0.id == id }) else {
            throw ConversationServiceError.emptyOutput("mutate")
        }
        summaries[idx] = transform(summaries[idx])
    }
}

// MARK: - Mutation helpers (mock only)

private extension ConversationSummary {
    func with(
        title: String? = nil,
        titleSource: String? = nil,
        status: ConversationStatus? = nil,
        pinned: Bool? = nil,
        archived: Bool? = nil,
        messageCount: Int? = nil,
        updatedAtMs: Int64? = nil,
        lastMessageAtMs: Int64?? = nil
    ) -> ConversationSummary {
        ConversationSummary(
            schema: schema,
            id: id,
            projectId: projectId,
            title: title ?? self.title,
            titleSource: titleSource ?? self.titleSource,
            status: status ?? self.status,
            pinned: pinned ?? self.pinned,
            archived: archived ?? self.archived,
            messageCount: messageCount ?? self.messageCount,
            createdAtMs: createdAtMs,
            updatedAtMs: updatedAtMs ?? self.updatedAtMs,
            lastMessageAtMs: lastMessageAtMs ?? self.lastMessageAtMs
        )
    }
}

private extension ConversationMessage {
    func with(conversationId: String) -> ConversationMessage {
        ConversationMessage(
            schema: schema,
            id: id,
            projectId: projectId,
            conversationId: conversationId,
            turnId: turnId,
            role: role,
            state: state,
            contentRedacted: contentRedacted,
            sequence: sequence,
            createdAtMs: createdAtMs,
            updatedAtMs: updatedAtMs
        )
    }

    func with(
        turnId: String? = nil,
        state: MessageState? = nil,
        contentRedacted: String? = nil
    ) -> ConversationMessage {
        ConversationMessage(
            schema: schema,
            id: id,
            projectId: projectId,
            conversationId: conversationId,
            turnId: turnId ?? self.turnId,
            role: role,
            state: state ?? self.state,
            contentRedacted: contentRedacted ?? self.contentRedacted,
            sequence: sequence,
            createdAtMs: createdAtMs,
            updatedAtMs: updatedAtMs
        )
    }
}

private func executionEvent(
    id: String,
    runID: String,
    sequence: UInt64,
    kind: ExecutionEventKind,
    message: String,
    occurredAtMs: Int64
) -> ExecutionEventEnvelope {
    ExecutionEventEnvelope(
        schema: "opensks.execution-event-envelope.v1",
        id: id,
        runId: runID,
        sequence: sequence,
        occurredAt: "\(occurredAtMs / 1_000).000000000",
        actor: "mock-conversation-service",
        causationId: nil,
        correlationId: nil,
        kind: kind,
        payload: .object(["message": .string(message)]),
        sensitivity: .public,
        evidenceRefs: ["mock:conversation-service"]
    )
}
