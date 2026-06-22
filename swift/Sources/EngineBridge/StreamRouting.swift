// StreamRouting.swift — client-side cursor dedup, per-stream routing, and the
// process-death fanout for the explicit streaming protocol v2 (PR-026).
//
// Completion is detected ONLY from an explicit terminal frame. On daemon/process
// death, every pending stream is failed immediately with a typed error — never
// left to a timeout.

import Foundation

enum CursorDecision: Equatable {
    case accept
    case duplicateOrOld
    case gap(expected: UInt64, got: UInt64)
}

/// Per-stream cursor tracker: dedups replays, surfaces gaps for reconnect.
struct StreamCursorTracker {
    private(set) var last: UInt64?
    private(set) var terminated = false

    mutating func accept(_ cursor: UInt64) -> CursorDecision {
        if let last {
            if cursor <= last { return .duplicateOrOld }
            if cursor == last + 1 { self.last = cursor; return .accept }
            return .gap(expected: last + 1, got: cursor)
        } else {
            if cursor == 0 { last = 0; return .accept }
            return .gap(expected: 0, got: cursor)
        }
    }

    mutating func observeTerminal() { terminated = true }
}

/// Decodes NDJSON lines into frames, quarantining malformed lines so one bad
/// payload cannot crash the reader.
struct StreamFrameReader {
    private(set) var quarantined: [String] = []
    private let decoder = JSONDecoder()

    mutating func decode(line: String) -> EngineStreamFrame? {
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        guard
            let data = trimmed.data(using: .utf8),
            let frame = try? decoder.decode(EngineStreamFrame.self, from: data)
        else {
            quarantined.append(trimmed)
            return nil
        }
        return frame
    }
}

enum TerminalReason: Equatable {
    case completed(String)
    case failed(PublicEngineError, resumable: Bool)
}

/// Routes interleaved frames to per-stream state, deduping by cursor and
/// accumulating events until an explicit terminal frame.
final class MultiStreamRouter {
    struct StreamState {
        var tracker = StreamCursorTracker()
        var events: [JSONValue] = []
        var terminal: TerminalReason?
    }

    private(set) var streams: [String: StreamState] = [:]

    @discardableResult
    func ingest(_ frame: EngineStreamFrame) -> CursorDecision {
        var state = streams[frame.streamID] ?? StreamState()
        let decision = state.tracker.accept(frame.cursor)
        if decision == .accept {
            switch frame {
            case .event(_, _, let payload):
                state.events.append(payload)
            case .completed(_, _, let reason):
                state.terminal = .completed(reason)
                state.tracker.observeTerminal()
            case .failed(_, _, let error, let resumable):
                state.terminal = .failed(error, resumable: resumable)
                state.tracker.observeTerminal()
            default:
                break
            }
        }
        streams[frame.streamID] = state
        return decision
    }

    func isOpen(_ streamID: String) -> Bool {
        guard let s = streams[streamID] else { return false }
        return s.terminal == nil
    }
}

/// A sink that receives frames for one stream, or a terminal failure.
protocol StreamSink: AnyObject {
    func deliver(_ frame: EngineStreamFrame)
    func fail(_ error: PublicEngineError)
}

/// Holds pending stream sinks. On process death, `failAll` finishes every
/// pending stream immediately with a typed error (no waiting for a timeout).
final class PendingStreamRegistry {
    private var sinks: [String: StreamSink] = [:]

    var pendingCount: Int { sinks.count }

    func register(streamID: String, sink: StreamSink) {
        sinks[streamID] = sink
    }

    /// Route a frame to its stream's sink; a terminal frame retires the sink.
    func deliver(_ frame: EngineStreamFrame) {
        guard let sink = sinks[frame.streamID] else { return }
        sink.deliver(frame)
        if frame.isTerminal {
            sinks[frame.streamID] = nil
        }
    }

    /// Process/daemon death: fail every pending stream at once.
    func failAll(_ error: PublicEngineError) {
        let all = Array(sinks.values)
        sinks.removeAll()
        for sink in all {
            sink.fail(error)
        }
    }
}
