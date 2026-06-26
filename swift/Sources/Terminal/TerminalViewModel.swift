import Foundation
import SwiftUI

@MainActor
final class TerminalViewModel: ObservableObject {
    @Published var session: TerminalSessionState
    @Published var blocks: [TerminalCommandBlockModel] = []
    @Published var input = ""
    @Published var ghostSuggestion: TerminalSuggestionModel?
    @Published var suggestions: [TerminalSuggestionModel] = []
    @Published var agentMessages: [TerminalAgentMessage] = []
    @Published var pendingApproval: TerminalSuggestionModel?
    @Published var daemonStatus: TerminalDaemonStatus = .unknown
    @Published var lastError: String?

    private var client: TerminalDaemonClientProtocol?
    private var suggestionTask: Task<Void, Never>?

    init(
        client: TerminalDaemonClientProtocol? = nil,
        sessionID: String = "terminal-\(UUID().uuidString)",
        cwd: String = FileManager.default.currentDirectoryPath,
        shell: String = TerminalViewModel.defaultShell()
    ) {
        self.client = client
        self.session = TerminalSessionState(
            id: sessionID,
            cwd: cwd,
            shell: shell,
            status: .disconnected,
            startedAt: nil,
            lastExitCode: nil
        )
        self.daemonStatus = client == nil ? .unavailable : .unknown
    }

    func configure(client: TerminalDaemonClientProtocol, cwd: String, shell: String? = nil) {
        self.client = client
        session.cwd = cwd
        session.shell = shell ?? session.shell
        if daemonStatus == .unavailable {
            daemonStatus = .unknown
        }
    }

    func startSession() {
        Task { await startSessionAsync() }
    }

    func stopSession() {
        Task { await stopSessionAsync() }
    }

    func submitInput() {
        Task { await submitInputAsync() }
    }

    func requestSuggestions() {
        Task { await requestSuggestionsAsync() }
    }

    func scheduleSuggestionRequest() {
        suggestionTask?.cancel()
        suggestionTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 180_000_000)
            guard !Task.isCancelled else { return }
            await self?.requestSuggestionsAsync()
        }
    }

    func acceptGhostSuggestion() {
        guard let ghostSuggestion else { return }
        input = ghostSuggestion.replacement
        self.ghostSuggestion = nil
    }

    func insertSuggestion(_ suggestion: TerminalSuggestionModel) {
        if suggestion.requiresApproval || suggestion.risk.requiresApprovalByDefault {
            pendingApproval = suggestion
        } else {
            input = suggestion.replacement
        }
    }

    func runSuggestion(_ suggestion: TerminalSuggestionModel) {
        Task { await runSuggestionAsync(suggestion) }
    }

    func explainLastBlock() {
        guard let block = blocks.last else { return }
        sendAgentPrompt(
            "Explain this terminal command and output:\n$ \(block.commandRedacted)\n\(block.outputPreview)"
        )
    }

    func explainSuggestion(_ suggestion: TerminalSuggestionModel) {
        sendAgentPrompt("Explain this proposed command without running it:\n\(suggestion.replacement)")
    }

    func sendAgentPrompt(_ prompt: String) {
        Task { await sendAgentPromptAsync(prompt) }
    }

    func approvePendingInsert() {
        guard let pendingApproval else { return }
        input = pendingApproval.replacement
        self.pendingApproval = nil
        lastError = nil
    }

    func cancelPendingApproval() {
        pendingApproval = nil
    }

    func startSessionAsync() async {
        #if os(macOS)
        guard let client else {
            showUnavailable()
            return
        }
        daemonStatus = .starting
        session.status = .starting
        do {
            try await client.startTerminalSession(
                sessionId: session.id,
                cwd: session.cwd,
                shell: session.shell
            )
            session.status = .running
            session.startedAt = Date()
            daemonStatus = .healthy
            lastError = nil
        } catch {
            session.status = .failed
            daemonStatus = status(for: error)
            lastError = error.localizedDescription
        }
        #else
        daemonStatus = .unsupportedPlatform
        lastError = "PTY terminal runtime is not supported on this platform yet."
        #endif
    }

    func stopSessionAsync() async {
        guard let client else {
            showUnavailable()
            return
        }
        session.status = .stopping
        do {
            try await client.stopTerminalSession(sessionId: session.id)
            session.status = .exited
            daemonStatus = .unavailable
        } catch {
            session.status = .failed
            daemonStatus = status(for: error)
            lastError = error.localizedDescription
        }
    }

    func submitInputAsync() async {
        let raw = input
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        if trimmed.hasPrefix("/agent ") {
            let prompt = String(trimmed.dropFirst("/agent ".count))
            input = ""
            await sendAgentPromptAsync(prompt)
            return
        }

        if trimmed.hasPrefix("!") {
            let forced = String(trimmed.dropFirst())
            input = ""
            await sendShellInput(forced, inputKind: "shell")
            return
        }

        if TerminalInputClassifier.isLikelyNaturalLanguage(trimmed) {
            input = ""
            await sendAgentPromptAsync(trimmed)
            return
        }

        let risk = TerminalInputClassifier.localRisk(for: trimmed)
        if risk.requiresApprovalByDefault {
            pendingApproval = TerminalSuggestionModel(
                id: "approval-\(nowMs())",
                replacement: trimmed,
                display: TerminalInputClassifier.redacted(trimmed).text,
                description: "This command requires approval before execution.",
                source: "local-risk-classifier",
                confidence: 1,
                risk: risk,
                requiresApproval: true
            )
            return
        }

        input = ""
        await sendShellInput(trimmed, inputKind: "shell")
    }

    func requestSuggestionsAsync() async {
        let query = input
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            suggestions = []
            ghostSuggestion = nil
            return
        }
        guard let client else {
            showUnavailable()
            return
        }

        do {
            let next = try await client.requestTerminalSuggestions(
                input: query,
                cursor: query.count,
                cwd: session.cwd,
                includeAI: false
            )
            suggestions = next
            ghostSuggestion = next.first { $0.risk == .safe && !$0.requiresApproval }
            daemonStatus = .healthy
            lastError = nil
        } catch {
            suggestions = []
            ghostSuggestion = nil
            daemonStatus = status(for: error)
            lastError = error.localizedDescription
        }
    }

    func runSuggestionAsync(_ suggestion: TerminalSuggestionModel) async {
        guard !suggestion.requiresApproval && !suggestion.risk.requiresApprovalByDefault else {
            pendingApproval = suggestion
            return
        }
        input = ""
        await sendShellInput(suggestion.replacement, inputKind: "shell")
    }

    func sendAgentPromptAsync(_ prompt: String) async {
        let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        guard let client else {
            showUnavailable()
            return
        }
        agentMessages.append(
            TerminalAgentMessage(
                id: "agent-\(nowMs())",
                text: trimmed,
                createdAtMs: nowMs(),
                isError: false
            )
        )
        do {
            let proposals = try await client.startTerminalAgentTurn(
                prompt: trimmed,
                sessionId: session.id,
                cwd: session.cwd
            )
            suggestions = proposals
            ghostSuggestion = proposals.first { $0.risk == .safe && !$0.requiresApproval }
            daemonStatus = .healthy
            lastError = nil
        } catch {
            daemonStatus = status(for: error)
            lastError = error.localizedDescription
            agentMessages.append(
                TerminalAgentMessage(
                    id: "agent-error-\(nowMs())",
                    text: error.localizedDescription,
                    createdAtMs: nowMs(),
                    isError: true
                )
            )
        }
    }

    private func sendShellInput(_ command: String, inputKind: String) async {
        guard let client else {
            showUnavailable()
            return
        }
        let redacted = TerminalInputClassifier.redacted(command)
        let blockID = "block-\(nowMs())"
        blocks.append(
            TerminalCommandBlockModel(
                id: blockID,
                commandRedacted: redacted.text,
                outputPreview: "Waiting for daemon output...",
                exitCode: nil,
                startedAtMs: nowMs(),
                finishedAtMs: nil,
                redacted: redacted.redacted
            )
        )
        trimBlocks()
        do {
            try await client.sendTerminalInput(
                sessionId: session.id,
                text: command,
                inputKind: inputKind
            )
            finishBlock(id: blockID, output: "Command submitted to daemon.", exitCode: nil)
            daemonStatus = .healthy
            lastError = nil
        } catch {
            finishBlock(id: blockID, output: error.localizedDescription, exitCode: 1)
            session.lastExitCode = 1
            daemonStatus = status(for: error)
            lastError = error.localizedDescription
        }
    }

    private func finishBlock(id: String, output: String, exitCode: Int?) {
        guard let index = blocks.firstIndex(where: { $0.id == id }) else { return }
        blocks[index].outputPreview = TerminalPreviewSanitizer.plainPreview(from: output)
        blocks[index].exitCode = exitCode
        blocks[index].finishedAtMs = nowMs()
    }

    private func trimBlocks() {
        if blocks.count > terminalMaxBlocksInMemory {
            blocks.removeFirst(blocks.count - terminalMaxBlocksInMemory)
        }
    }

    private func showUnavailable() {
        daemonStatus = .unavailable
        session.status = .failed
        lastError = "Terminal daemon is not connected.\nRun `cargo run -- terminal smoke` to verify the local runtime."
    }

    private func status(for error: Error) -> TerminalDaemonStatus {
        let message = error.localizedDescription.lowercased()
        if message.contains("provider") || message.contains("model") {
            return .providerUnavailable
        }
        if message.contains("unsupported") || message.contains("pty") {
            return .unsupportedPlatform
        }
        return .unavailable
    }

    nonisolated private static func defaultShell() -> String {
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "zsh"
        return URL(fileURLWithPath: shell).lastPathComponent.isEmpty
            ? "zsh"
            : URL(fileURLWithPath: shell).lastPathComponent
    }
}

func nowMs() -> UInt64 {
    UInt64(Date().timeIntervalSince1970 * 1_000)
}
