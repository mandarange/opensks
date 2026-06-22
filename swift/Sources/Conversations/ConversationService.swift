// ConversationService.swift — the persistence boundary for PR-025. A
// `LiveConversationService` shells the bundled `opensks-cli conversation <sub>`
// exactly like `AppState` resolves and runs the CLI (same binary URL + workspace
// path, `.convertFromSnakeCase` JSON decoding). A `MockConversationService`
// backs tests and previews with in-memory arrays so the UI never needs a process.

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

    /// Start ONE deterministic engine run for `id`: persist the redacted user
    /// message + an assistant placeholder, run the engine, set the assistant
    /// content from the result, link the run, and return the ids. Passing the
    /// same `idempotencyKey` again returns the SAME ids with `reused == true`
    /// and does NOT start a second run.
    func turnStart(conversationID: String, text: String, idempotencyKey: String) async throws -> ConversationTurn

    /// The runs linked to a conversation (`opensks.conversation-run-list.v1`).
    func runs(conversationID: String) async throws -> [ConversationRunRef]
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

    func turnStart(conversationID: String, text: String, idempotencyKey: String) async throws -> ConversationTurn {
        try await run(
            ["conversation", "turn-start", "--workspace", workspace.path,
             "--conversation", conversationID, "--text", text,
             "--idempotency-key", idempotencyKey],
            verb: "turn-start"
        )
    }

    func runs(conversationID: String) async throws -> [ConversationRunRef] {
        let list: ConversationRunList = try await run(
            ["conversation", "runs", "--workspace", workspace.path, "--conversation", conversationID],
            verb: "runs"
        )
        return list.runs
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
    private var runsByConversation: [String: [ConversationRunRef]] = [:]
    /// Idempotency ledger: `"<conversationID>\u{1}<key>" -> turn` so a replayed
    /// key returns the same ids (reused) without starting a second run.
    private var turnsByIdempotencyKey: [String: ConversationTurn] = [:]
    private var nextId = 0
    private var clock: Int64 = 1_000

    let projectId: String

    /// Final state the mock reports for the deterministic run it "completes".
    /// Defaults to `.completed`; a test can request `.failed` to exercise the
    /// danger pill without touching the engine.
    let runStateOnTurn: RunState

    init(
        projectId: String = "mock-project",
        summaries: [ConversationSummary] = [],
        messages: [String: [ConversationMessage]] = [:],
        runStateOnTurn: RunState = .completed
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
            runsByConversation[id] = nil
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

    func turnStart(conversationID: String, text: String, idempotencyKey: String) async throws -> ConversationTurn {
        try withLock { try turnStartLocked(conversationID: conversationID, text: text, idempotencyKey: idempotencyKey) }
    }

    private func turnStartLocked(conversationID id: String, text: String, idempotencyKey: String) throws -> ConversationTurn {
        guard summaries.contains(where: { $0.id == id }) else {
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

        // 3. "Run" the deterministic engine and set the assistant content from
        //    its result, linking both messages to the turn.
        let runState = runStateOnTurn
        let assistantContent = runState == .failed
            ? "Run failed."
            : "Deterministic run \(runId) completed."
        if var msgs = messagesByConversation[id],
           let idx = msgs.firstIndex(where: { $0.id == assistantPlaceholder.id }) {
            msgs[idx] = assistantPlaceholder.with(
                turnId: turnId,
                state: runState == .failed ? .failed : .complete,
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
        return turn
    }

    func runs(conversationID id: String) async throws -> [ConversationRunRef] {
        withLock { runsByConversation[id] ?? [] }
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
