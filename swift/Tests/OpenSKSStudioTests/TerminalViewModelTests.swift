import XCTest
@testable import OpenSKSStudio

@MainActor
final class TerminalViewModelTests: XCTestCase {
    func testGitStatusSuggestionAcceptChangesInputOnly() async {
        let client = MockTerminalDaemonClient()
        client.suggestions = [
            TerminalSuggestionModel(
                id: "git-status",
                replacement: "git status",
                display: "git status",
                description: "Expand shorthand.",
                source: "deterministic",
                confidence: 0.95,
                risk: .safe,
                requiresApproval: false
            )
        ]
        let model = TerminalViewModel(client: client, sessionID: "terminal-test", cwd: "/workspace", shell: "zsh")
        model.input = "git st"

        await model.requestSuggestionsAsync()
        model.acceptGhostSuggestion()

        XCTAssertEqual(model.input, "git status")
        XCTAssertEqual(client.sentInputs.count, 0)
    }

    func testAgentInputUsesAgentRequestBuilderPath() async {
        let client = MockTerminalDaemonClient()
        let model = TerminalViewModel(client: client, sessionID: "terminal-test", cwd: "/workspace", shell: "zsh")
        model.input = "/agent cargo test failed"

        await model.submitInputAsync()

        XCTAssertEqual(client.agentPrompts, ["cargo test failed"])
        XCTAssertEqual(client.sentInputs.count, 0)
        XCTAssertEqual(model.input, "")
    }

    func testNaturalLanguageInputRoutesToAgentProposalOnly() async {
        let client = MockTerminalDaemonClient()
        let model = TerminalViewModel(client: client, sessionID: "terminal-test", cwd: "/workspace", shell: "zsh")
        model.input = "왜 cargo test 실패해?"

        await model.submitInputAsync()

        XCTAssertEqual(client.agentPrompts, ["왜 cargo test 실패해?"])
        XCTAssertEqual(client.sentInputs.count, 0)
    }

    func testForceShellStripsBangAndSendsShellInput() async {
        let client = MockTerminalDaemonClient()
        let model = TerminalViewModel(client: client, sessionID: "terminal-test", cwd: "/workspace", shell: "zsh")
        model.input = "!echo hello"

        await model.submitInputAsync()

        XCTAssertEqual(client.sentInputs.map(\.text), ["echo hello"])
        XCTAssertEqual(client.sentInputs.map(\.inputKind), ["shell"])
    }

    func testRequiresApprovalSuggestionDoesNotRunImmediately() async {
        let client = MockTerminalDaemonClient()
        let model = TerminalViewModel(client: client, sessionID: "terminal-test", cwd: "/workspace", shell: "zsh")
        let suggestion = TerminalSuggestionModel(
            id: "danger",
            replacement: "rm -rf build",
            display: "rm -rf build",
            description: "Deletes build output.",
            source: "agent",
            confidence: 0.7,
            risk: .destructive,
            requiresApproval: true
        )

        await model.runSuggestionAsync(suggestion)

        XCTAssertEqual(client.sentInputs.count, 0)
        XCTAssertEqual(model.pendingApproval?.id, "danger")
    }

    func testDaemonErrorEventShowsLastErrorWithoutCrash() async {
        let client = MockTerminalDaemonClient()
        client.error = TerminalDaemonClientError(message: "daemon response timed out for req-terminal-input")
        let model = TerminalViewModel(client: client, sessionID: "terminal-test", cwd: "/workspace", shell: "zsh")
        model.input = "git status"

        await model.submitInputAsync()

        XCTAssertEqual(model.daemonStatus, .unavailable)
        XCTAssertEqual(model.lastError, "daemon response timed out for req-terminal-input")
        XCTAssertEqual(model.blocks.last?.exitCode, 1)
    }
}

private final class MockTerminalDaemonClient: TerminalDaemonClientProtocol {
    var suggestions: [TerminalSuggestionModel] = []
    var agentSuggestions: [TerminalSuggestionModel] = []
    var sentInputs: [(sessionId: String, text: String, inputKind: String)] = []
    var agentPrompts: [String] = []
    var error: Error?

    func startTerminalSession(sessionId: String, cwd: String, shell: String?) async throws {
        if let error { throw error }
    }

    func sendTerminalInput(sessionId: String, text: String, inputKind: String) async throws {
        if let error { throw error }
        sentInputs.append((sessionId, text, inputKind))
    }

    func resizeTerminal(sessionId: String, cols: Int, rows: Int) async throws {
        if let error { throw error }
    }

    func stopTerminalSession(sessionId: String) async throws {
        if let error { throw error }
    }

    func requestTerminalSuggestions(
        input: String,
        cursor: Int,
        cwd: String,
        includeAI: Bool
    ) async throws -> [TerminalSuggestionModel] {
        if let error { throw error }
        return suggestions
    }

    func startTerminalAgentTurn(
        prompt: String,
        sessionId: String,
        cwd: String
    ) async throws -> [TerminalSuggestionModel] {
        if let error { throw error }
        agentPrompts.append(prompt)
        return agentSuggestions
    }
}
