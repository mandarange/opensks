// EngineStreamFrame.swift — Swift mirror of the explicit streaming protocol v2
// (PR-026). Decodes the daemon's framed wire envelope. Completion is signalled
// by an explicit terminal frame (stream_completed / stream_failed) — there is no
// quiet-window / silence heuristic anywhere in this path.

import Foundation

// Event / snapshot payloads reuse the module's existing `JSONValue` (Models.swift)
// — an opaque, Codable/Equatable JSON value. The typed ExecutionEventEnvelope v2
// and PipelineExecutionProjection arrive in PR-029.

struct PublicEngineError: Equatable, Codable {
    let schema: String
    let code: String
    let message: String
    let retryable: Bool
    var remediation: String?
    var evidenceRefs: [String]
    let redacted: Bool

    enum CodingKeys: String, CodingKey {
        case schema, code, message, retryable, remediation
        case evidenceRefs = "evidence_refs"
        case redacted
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        code = try c.decode(String.self, forKey: .code)
        message = try c.decode(String.self, forKey: .message)
        retryable = try c.decode(Bool.self, forKey: .retryable)
        remediation = try c.decodeIfPresent(String.self, forKey: .remediation)
        evidenceRefs = try c.decodeIfPresent([String].self, forKey: .evidenceRefs) ?? []
        redacted = try c.decodeIfPresent(Bool.self, forKey: .redacted) ?? true
    }

    init(code: String, message: String, retryable: Bool) {
        self.schema = "opensks.public-engine-error.v1"
        self.code = code
        self.message = message
        self.retryable = retryable
        self.remediation = nil
        self.evidenceRefs = []
        self.redacted = true
    }
}

enum EngineStreamFrame: Equatable {
    case opened(streamID: String, requestID: String, projectID: String,
                conversationID: String, runID: String?, protocolVersion: String, cursor: UInt64)
    case event(streamID: String, cursor: UInt64, event: JSONValue)
    case snapshot(streamID: String, cursor: UInt64, projection: JSONValue)
    case heartbeat(streamID: String, cursor: UInt64, serverTimeMs: UInt64)
    case completed(streamID: String, cursor: UInt64, reasonCode: String)
    case failed(streamID: String, cursor: UInt64, error: PublicEngineError, resumable: Bool)

    var streamID: String {
        switch self {
        case .opened(let s, _, _, _, _, _, _): return s
        case .event(let s, _, _), .snapshot(let s, _, _), .heartbeat(let s, _, _),
             .completed(let s, _, _), .failed(let s, _, _, _):
            return s
        }
    }

    var cursor: UInt64 {
        switch self {
        case .opened(_, _, _, _, _, _, let c): return c
        case .event(_, let c, _), .snapshot(_, let c, _), .heartbeat(_, let c, _),
             .completed(_, let c, _), .failed(_, let c, _, _):
            return c
        }
    }

    var isTerminal: Bool {
        switch self {
        case .completed, .failed: return true
        default: return false
        }
    }
}

extension EngineStreamFrame: Decodable {
    private enum K: String, CodingKey {
        case frameType = "frame_type"
        case streamId = "stream_id"
        case requestId = "request_id"
        case projectId = "project_id"
        case conversationId = "conversation_id"
        case runId = "run_id"
        case protocolVersion = "protocol_version"
        case cursor, event, projection
        case serverTimeMs = "server_time_ms"
        case reasonCode = "reason_code"
        case error, resumable
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: K.self)
        let type = try c.decode(String.self, forKey: .frameType)
        let streamID = try c.decode(String.self, forKey: .streamId)
        let cursor = try c.decode(UInt64.self, forKey: .cursor)
        switch type {
        case "stream_opened":
            self = .opened(
                streamID: streamID,
                requestID: try c.decode(String.self, forKey: .requestId),
                projectID: try c.decode(String.self, forKey: .projectId),
                conversationID: try c.decode(String.self, forKey: .conversationId),
                runID: try c.decodeIfPresent(String.self, forKey: .runId),
                protocolVersion: try c.decode(String.self, forKey: .protocolVersion),
                cursor: cursor
            )
        case "event":
            self = .event(streamID: streamID, cursor: cursor,
                          event: try c.decode(JSONValue.self, forKey: .event))
        case "snapshot":
            self = .snapshot(streamID: streamID, cursor: cursor,
                             projection: try c.decode(JSONValue.self, forKey: .projection))
        case "heartbeat":
            self = .heartbeat(streamID: streamID, cursor: cursor,
                              serverTimeMs: try c.decode(UInt64.self, forKey: .serverTimeMs))
        case "stream_completed":
            self = .completed(streamID: streamID, cursor: cursor,
                              reasonCode: try c.decode(String.self, forKey: .reasonCode))
        case "stream_failed":
            self = .failed(streamID: streamID, cursor: cursor,
                           error: try c.decode(PublicEngineError.self, forKey: .error),
                           resumable: try c.decode(Bool.self, forKey: .resumable))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: K.frameType, in: c,
                debugDescription: "unknown frame_type \(type)"
            )
        }
    }
}
