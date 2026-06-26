import Foundation

protocol TerminalDaemonClientProtocol {
    func startTerminalSession(sessionId: String, cwd: String, shell: String?) async throws
    func sendTerminalInput(sessionId: String, text: String, inputKind: String) async throws
    func resizeTerminal(sessionId: String, cols: Int, rows: Int) async throws
    func stopTerminalSession(sessionId: String) async throws
    func requestTerminalSuggestions(input: String, cursor: Int, cwd: String, includeAI: Bool) async throws -> [TerminalSuggestionModel]
    func startTerminalAgentTurn(prompt: String, sessionId: String, cwd: String) async throws -> [TerminalSuggestionModel]
}

struct TerminalDaemonClient: TerminalDaemonClientProtocol {
    let engine: EngineProcess
    let cli: URL
    let workspace: URL

    func startTerminalSession(sessionId: String, cwd: String, shell: String?) async throws {
        let id = requestID(prefix: "req-terminal-session-start")
        let stream = await engine.terminalRequest(
            cli: cli,
            cwd: workspace,
            request: .terminalSessionStart(
                id: id,
                sessionId: sessionId,
                cwd: cwd,
                shell: shell
            )
        )
        try Self.throwIfFailed(stream)
    }

    func sendTerminalInput(sessionId: String, text: String, inputKind: String) async throws {
        let id = requestID(prefix: "req-terminal-input")
        let stream = await engine.terminalRequest(
            cli: cli,
            cwd: workspace,
            request: .terminalInput(
                id: id,
                sessionId: sessionId,
                text: text,
                inputKind: inputKind
            )
        )
        try Self.throwIfFailed(stream)
    }

    func resizeTerminal(sessionId: String, cols: Int, rows: Int) async throws {
        let id = requestID(prefix: "req-terminal-resize")
        let stream = await engine.terminalRequest(
            cli: cli,
            cwd: workspace,
            request: .terminalResize(
                id: id,
                sessionId: sessionId,
                cols: cols,
                rows: rows
            )
        )
        try Self.throwIfFailed(stream)
    }

    func stopTerminalSession(sessionId: String) async throws {
        let id = requestID(prefix: "req-terminal-session-stop")
        let stream = await engine.terminalRequest(
            cli: cli,
            cwd: workspace,
            request: .terminalSessionStop(id: id, sessionId: sessionId)
        )
        try Self.throwIfFailed(stream)
    }

    func requestTerminalSuggestions(
        input: String,
        cursor: Int,
        cwd: String,
        includeAI: Bool
    ) async throws -> [TerminalSuggestionModel] {
        let id = requestID(prefix: "req-terminal-suggest")
        let stream = await engine.terminalRequest(
            cli: cli,
            cwd: workspace,
            request: .terminalSuggestionRequest(
                id: id,
                input: input,
                cursor: cursor,
                cwd: cwd,
                includeAI: includeAI
            )
        )
        try Self.throwIfFailed(stream)
        return Self.decodeSuggestions(from: stream.rawLines)
    }

    func startTerminalAgentTurn(
        prompt: String,
        sessionId: String,
        cwd: String
    ) async throws -> [TerminalSuggestionModel] {
        let id = requestID(prefix: "req-terminal-agent")
        let stream = await engine.terminalRequest(
            cli: cli,
            cwd: workspace,
            request: .terminalAgentTurnStart(
                id: id,
                prompt: prompt,
                sessionId: sessionId,
                cwd: cwd
            )
        )
        try Self.throwIfFailed(stream)
        return Self.decodeSuggestions(from: stream.rawLines)
    }

    private func requestID(prefix: String) -> String {
        "\(prefix)-\(UUID().uuidString)"
    }

    static func decodeSuggestions(from lines: [String]) -> [TerminalSuggestionModel] {
        var decoded: [TerminalSuggestionModel] = []
        for line in lines {
            guard let data = line.data(using: .utf8) else { continue }
            if let envelope = try? JSONDecoder().decode(TerminalSuggestionListEnvelope.self, from: data) {
                decoded.append(contentsOf: envelope.suggestions ?? [])
                decoded.append(contentsOf: envelope.proposals ?? [])
            } else if let suggestion = try? JSONDecoder().decode(TerminalSuggestionModel.self, from: data) {
                decoded.append(suggestion)
            }
        }
        return decoded
    }

    static func throwIfFailed(_ stream: EngineRunStream) throws {
        if let event = stream.engineEvents.first(where: { $0.severity.isError }) {
            throw TerminalDaemonClientError(message: event.message)
        }
        if let exitCode = stream.exitCode, exitCode != 0 {
            let message = stream.stderr.isEmpty ? "daemon exited \(exitCode)" : stream.stderr
            throw TerminalDaemonClientError(message: message)
        }
    }
}

struct TerminalDaemonClientError: LocalizedError, Equatable {
    let message: String

    var errorDescription: String? { message }
}

private struct TerminalSuggestionListEnvelope: Decodable {
    let suggestions: [TerminalSuggestionModel]?
    let proposals: [TerminalSuggestionModel]?
}
