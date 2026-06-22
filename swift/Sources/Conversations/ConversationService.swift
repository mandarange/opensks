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
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let proc = Process()
                proc.executableURL = cli
                proc.arguments = args
                proc.currentDirectoryURL = cwd
                let outPipe = Pipe()
                let errPipe = Pipe()
                proc.standardOutput = outPipe
                proc.standardError = errPipe
                do {
                    try proc.run()
                } catch {
                    continuation.resume(throwing: ConversationServiceError.launchFailed(error.localizedDescription))
                    return
                }
                let outData = outPipe.fileHandleForReading.readDataToEndOfFile()
                let errData = errPipe.fileHandleForReading.readDataToEndOfFile()
                proc.waitUntilExit()
                continuation.resume(returning: CaptureResult(
                    stdout: outData,
                    stderr: String(decoding: errData, as: UTF8.self),
                    exitCode: proc.terminationStatus
                ))
            }
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
    private var nextId = 0
    private var clock: Int64 = 1_000

    let projectId: String

    init(
        projectId: String = "mock-project",
        summaries: [ConversationSummary] = [],
        messages: [String: [ConversationMessage]] = [:]
    ) {
        self.projectId = projectId
        self.summaries = summaries
        self.messagesByConversation = messages
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
}
