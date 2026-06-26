import XCTest
@testable import OpenSKSStudio

final class TerminalDecoderTests: XCTestCase {
    func testSuggestionJSONDecode() throws {
        let lines = [
            """
            {"schema":"opensks.terminal-suggestion-response.v1","request_id":"req-terminal-suggest-1","suggestions":[{"id":"s1","replacement":"git status","display":"git status","description":"Expand shorthand.","source":"deterministic","confidence":0.91,"risk":"safe","requires_approval":false}]}
            """
        ]

        let suggestions = TerminalDaemonClient.decodeSuggestions(from: lines)

        XCTAssertEqual(suggestions.count, 1)
        XCTAssertEqual(suggestions[0].replacement, "git status")
        XCTAssertEqual(suggestions[0].risk, .safe)
        XCTAssertFalse(suggestions[0].requiresApproval)
    }

    func testUnknownRiskLabelDecodeDoesNotCrash() throws {
        let data = Data("""
        {"id":"s2","replacement":"tool run","display":"tool run","description":"Unknown daemon risk.","source":"daemon","confidence":0.2,"risk":"brand_new_risk","requires_approval":true}
        """.utf8)

        let suggestion = try JSONDecoder().decode(TerminalSuggestionModel.self, from: data)

        XCTAssertEqual(suggestion.risk, .unknown)
        XCTAssertTrue(suggestion.requiresApproval)
    }

    func testTerminalSuggestionRequestBuilderMatchesEnvelopeShape() throws {
        let request = EngineRequestEnvelope.terminalSuggestionRequest(
            id: "req-terminal-suggest-1",
            input: "git st",
            cursor: 6,
            cwd: "/workspace",
            includeAI: false
        )

        let data = try JSONEncoder.opensks.encode(request)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let params = try XCTUnwrap(json["params"] as? [String: Any])
        let nested = try XCTUnwrap(params["terminal_suggestion_request"] as? [String: Any])

        XCTAssertEqual(json["schema"] as? String, "opensks.engine-request.v1")
        XCTAssertEqual(json["id"] as? String, "req-terminal-suggest-1")
        XCTAssertEqual(json["kind"] as? String, "terminal_suggestion_request")
        XCTAssertEqual(json["protocol_version"] as? String, "opensks.contracts.v1")
        XCTAssertEqual(nested["schema"] as? String, "opensks.terminal-suggestion-request.v1")
        XCTAssertEqual(nested["request_id"] as? String, "req-terminal-suggest-1")
        XCTAssertEqual(nested["cwd"] as? String, "/workspace")
        XCTAssertEqual(nested["input"] as? String, "git st")
        XCTAssertEqual(nested["cursor"] as? Int, 6)
        XCTAssertEqual(nested["max_suggestions"] as? Int, 8)
        XCTAssertEqual(nested["include_ai"] as? Bool, false)
    }

    func testTerminalAgentTurnRequestBuilderUsesAgentKind() throws {
        let request = EngineRequestEnvelope.terminalAgentTurnStart(
            id: "req-terminal-agent-1",
            prompt: "cargo test failed",
            sessionId: "terminal-1",
            cwd: "/workspace"
        )

        let data = try JSONEncoder.opensks.encode(request)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let params = try XCTUnwrap(json["params"] as? [String: Any])
        let nested = try XCTUnwrap(params["terminal_agent_turn_start"] as? [String: Any])

        XCTAssertEqual(json["kind"] as? String, "terminal_agent_turn_start")
        XCTAssertEqual(nested["schema"] as? String, "opensks.terminal-agent-turn-start.v1")
        XCTAssertEqual(nested["prompt"] as? String, "cargo test failed")
        XCTAssertEqual(nested["session_id"] as? String, "terminal-1")
    }
}
