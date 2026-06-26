// Backend.swift — the concurrency core. A nonisolated CLIRunner actor owns the
// Process + pipe reads and yields Sendable events over an AsyncStream; the
// @MainActor AppState is the single source of truth that views observe. All
// Process work is off the main actor; every UI mutation is on the main actor.

import SwiftUI
import Foundation
import AppKit

// MARK: - Streamed output

enum LineKind: Sendable {
    case cmd, info, warn, danger, done

    var color: Color {
        switch self {
        case .cmd: return Theme.accent
        case .info: return Theme.textSoft
        case .warn: return Theme.gold
        case .danger: return Theme.coral
        case .done: return Theme.faint
        }
    }
}

struct RunLine: Sendable, Identifiable {
    let id = UUID()
    let text: String
    let kind: LineKind
}

enum RunEvent: Sendable {
    case line(RunLine)
    case finished(Int32)
}

func classifyLine(_ s: String) -> LineKind {
    let l = s.lowercased()
    if l.contains("error") || l.contains("failed") || l.contains("panic") { return .danger }
    if l.contains("warn") || l.contains("partial") { return .warn }
    return .info
}

// MARK: - Process runner (off the main actor)

struct CLICaptureResult: Sendable {
    let stdout: Data
    let stderr: String
    let exitCode: Int32?
    let timedOut: Bool
    let launchError: String?
}

actor CLIRunner {
    /// Run `cli args` capturing all stdout. Used for the quick `app-data` read.
    func capture(cli: URL, cwd: URL, args: [String]) async -> CLICaptureResult {
        do {
            let result = try await ProcessSupervisor().run(
                ProcessSupervisor.Spec(
                    executable: cli,
                    arguments: args,
                    workingDirectory: OpenSKSCLIProcess.workingDirectory(for: cwd),
                    environment: OpenSKSCLIProcess.environmentOverlay(for: cwd),
                    timeoutSeconds: OpenSKSCLIProcess.commandTimeoutSeconds,
                    maxCaptureBytes: 4 * 1024 * 1024
                )
            )
            return CLICaptureResult(
                stdout: result.stdout,
                stderr: String(decoding: result.stderr, as: UTF8.self),
                exitCode: result.exitCode,
                timedOut: result.timedOut,
                launchError: nil
            )
        } catch {
            return CLICaptureResult(
                stdout: Data(),
                stderr: "",
                exitCode: nil,
                timedOut: false,
                launchError: error.localizedDescription
            )
        }
    }

    /// Run `cli args` streaming stdout/stderr line-by-line as events.
    nonisolated func stream(cli: URL, cwd: URL, args: [String]) -> AsyncStream<RunEvent> {
        AsyncStream { continuation in
            let proc = Process()
            proc.executableURL = cli
            proc.arguments = args
            proc.currentDirectoryURL = OpenSKSCLIProcess.workingDirectory(for: cwd)
            proc.environment = OpenSKSCLIProcess.environment(for: cwd)
            let outPipe = Pipe()
            let errPipe = Pipe()
            proc.standardOutput = outPipe
            proc.standardError = errPipe

            outPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
                for piece in text.split(separator: "\n", omittingEmptySubsequences: false) where !piece.isEmpty {
                    let s = String(piece)
                    continuation.yield(.line(RunLine(text: s, kind: classifyLine(s))))
                }
            }
            errPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
                for piece in text.split(separator: "\n", omittingEmptySubsequences: false) where !piece.isEmpty {
                    continuation.yield(.line(RunLine(text: "! " + String(piece), kind: .warn)))
                }
            }
            proc.terminationHandler = { p in
                outPipe.fileHandleForReading.readabilityHandler = nil
                errPipe.fileHandleForReading.readabilityHandler = nil
                continuation.yield(.finished(p.terminationStatus))
                continuation.finish()
            }

            do {
                try proc.run()
            } catch {
                continuation.yield(.line(RunLine(text: "could not start command: \(error.localizedDescription)", kind: .danger)))
                continuation.yield(.finished(-1))
                continuation.finish()
            }
            continuation.onTermination = { _ in
                if proc.isRunning { proc.terminate() }
            }
        }
    }
}

final class EngineLineBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private let maxLines: Int
    private var partial = Data()
    private var lines: [String] = []

    init(maxLines: Int = 4_096) {
        self.maxLines = maxLines
    }

    func append(_ data: Data) {
        guard !data.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }

        partial.append(data)
        while let newline = partial.firstIndex(of: UInt8(ascii: "\n")) {
            let lineData = partial[..<newline]
            let next = partial.index(after: newline)
            partial.removeSubrange(partial.startIndex..<next)
            let trimmed = lineData.last == UInt8(ascii: "\r") ? lineData.dropLast() : lineData
            let line = String(decoding: trimmed, as: UTF8.self)
            if !line.isEmpty {
                lines.append(line)
            }
        }
        if lines.count > maxLines {
            lines.removeFirst(lines.count - maxLines)
        }
    }

    func drainLines() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        let drained = lines
        lines.removeAll(keepingCapacity: true)
        return drained
    }

    func drainText(includePartial: Bool = false) -> String {
        lock.lock()
        defer { lock.unlock() }
        var pieces = lines
        lines.removeAll(keepingCapacity: true)
        if includePartial, !partial.isEmpty {
            pieces.append(String(decoding: partial, as: UTF8.self))
            partial.removeAll(keepingCapacity: true)
        }
        return pieces.joined(separator: "\n")
    }
}

private struct EngineSessionKey: Equatable {
    let cliPath: String
    let cwdPath: String
}

struct EngineCollectedResponse {
    let lines: [String]
    let sawRequestEvent: Bool
    let timedOut: Bool
}

struct EngineResponseSnapshot {
    let lines: [String]
    let sawRequestEvent: Bool
    /// STREAM-001: true once the explicit per-request terminal marker
    /// (`request_completed`) has arrived for this request. Completion keys off this,
    /// never off a silence/quiet-window heuristic.
    let isComplete: Bool
    let lastLineAt: Date
}

private struct EnginePendingResponse {
    let requestId: String
    let kind: String
    let runId: String?
    let registeredAt: UInt64
    var lastLineOrder: UInt64
    var lines: [String]
    var sawRequestEvent: Bool
    var isComplete: Bool
    var lastLineAt: Date
}

final class EnginePendingResponseRouter: @unchecked Sendable {
    private let lock = NSLock()
    private var partial = Data()
    private var pending: [String: EnginePendingResponse] = [:]
    private var streamOwners: [String: String] = [:]
    private var nextRegistrationOrder: UInt64 = 0
    private var nextLineOrder: UInt64 = 0

    func register(_ request: EngineRequestEnvelope) {
        lock.lock()
        defer { lock.unlock() }
        nextRegistrationOrder += 1
        pending[request.id] = EnginePendingResponse(
            requestId: request.id,
            kind: request.kind,
            runId: request.params.runId,
            registeredAt: nextRegistrationOrder,
            lastLineOrder: 0,
            lines: [],
            sawRequestEvent: false,
            isComplete: false,
            lastLineAt: Date()
        )
    }

    func append(_ data: Data) {
        guard !data.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }

        partial.append(data)
        while let newline = partial.firstIndex(of: UInt8(ascii: "\n")) {
            let lineData = partial[..<newline]
            let next = partial.index(after: newline)
            partial.removeSubrange(partial.startIndex..<next)
            let trimmed = lineData.last == UInt8(ascii: "\r") ? lineData.dropLast() : lineData
            let line = String(decoding: trimmed, as: UTF8.self)
            if !line.isEmpty {
                route(line)
            }
        }
    }

    func snapshot(for requestId: String) -> EngineResponseSnapshot {
        lock.lock()
        defer { lock.unlock() }
        guard let response = pending[requestId] else {
            return EngineResponseSnapshot(
                lines: [], sawRequestEvent: false, isComplete: false, lastLineAt: Date()
            )
        }
        return EngineResponseSnapshot(
            lines: response.lines,
            sawRequestEvent: response.sawRequestEvent,
            isComplete: response.isComplete,
            lastLineAt: response.lastLineAt
        )
    }

    func finish(requestId: String, timedOut: Bool) -> EngineCollectedResponse {
        lock.lock()
        defer { lock.unlock() }
        let response = pending.removeValue(forKey: requestId)
        streamOwners = streamOwners.filter { $0.value != requestId }
        return EngineCollectedResponse(
            lines: response?.lines ?? [],
            sawRequestEvent: response?.sawRequestEvent ?? false,
            timedOut: timedOut
        )
    }

    func cancel(requestId: String) {
        lock.lock()
        defer { lock.unlock() }
        pending.removeValue(forKey: requestId)
        streamOwners = streamOwners.filter { $0.value != requestId }
    }

    private func route(_ line: String) {
        let engine = decodedEngineEvent(from: line)
        let frame = decodedStreamFrame(from: line)
        let requestId = engine?.requestId
            ?? frame?.requestId
            ?? decodedConversationTurnAcceptedRequestId(from: line)
            ?? decodedTurnSupervisorTickRequestId(from: line)
        let isTerminal = engine?.isTerminal ?? false
        let runId = decodedExecutionRunId(from: line) ?? frame?.runId
        let matchedIds = matchedPendingIds(
            requestId: requestId,
            runId: runId,
            streamId: frame?.streamId
        )
        guard !matchedIds.isEmpty else {
            return
        }
        if let frame, let requestId, pending[requestId] != nil {
            streamOwners[frame.streamId] = requestId
        }
        nextLineOrder += 1
        let lineOrder = nextLineOrder
        let now = Date()
        for id in matchedIds {
            guard var response = pending[id] else { continue }
            if isTerminal && requestId == id {
                // STREAM-001: the explicit terminal marker completes the response.
                // It is an envelope-level signal, not a user-facing event, so it is
                // NOT appended to the response lines — the decoded stream stays clean.
                response.isComplete = true
                response.sawRequestEvent = true
            } else {
                response.lines.append(line)
                if requestId == id {
                    response.sawRequestEvent = true
                }
            }
            response.lastLineOrder = lineOrder
            response.lastLineAt = now
            pending[id] = response
        }
    }

    private func matchedPendingIds(requestId: String?, runId: String?, streamId: String?) -> [String] {
        if let requestId, pending[requestId] != nil {
            return [requestId]
        }
        if let streamId,
           let owner = streamOwners[streamId],
           pending[owner] != nil {
            return [owner]
        }
        guard let runId else {
            return []
        }
        let sameRun = pending.values
            .filter { $0.runId == runId }
            .sorted { $0.registeredAt < $1.registeredAt }
        let accepted = sameRun.filter(\.sawRequestEvent)
        let owner = accepted.max { left, right in
            if left.lastLineOrder == right.lastLineOrder {
                return left.registeredAt < right.registeredAt
            }
            return left.lastLineOrder < right.lastLineOrder
        }
            ?? sameRun.first { $0.kind == "run_start" }
            ?? sameRun.first
        return owner.map { [$0.requestId] } ?? []
    }

    /// Decode an engine event line once, returning its correlation request id and
    /// whether it is the explicit per-request terminal marker (STREAM-001).
    private func decodedEngineEvent(from line: String) -> (requestId: String?, isTerminal: Bool)? {
        guard let data = line.data(using: .utf8),
              let event = try? JSONDecoder.opensks.decode(EngineEvent.self, from: data)
        else {
            return nil
        }
        return (event.requestId, event.eventType == .requestCompleted)
    }

    private func decodedConversationTurnAcceptedRequestId(from line: String) -> String? {
        guard let data = line.data(using: .utf8),
              let accepted = try? JSONDecoder.opensks.decode(ConversationTurnAccepted.self, from: data),
              accepted.schema == "opensks.conversation-turn-accepted.v1"
        else {
            return nil
        }
        return accepted.requestId
    }

    private func decodedTurnSupervisorTickRequestId(from line: String) -> String? {
        guard let data = line.data(using: .utf8),
              let tick = try? JSONDecoder.opensks.decode(TurnSupervisorTickResult.self, from: data),
              tick.schema == "opensks.turn-supervisor-tick.v1"
        else {
            return nil
        }
        return tick.requestId
    }

    private func decodedExecutionRunId(from line: String) -> String? {
        guard let data = line.data(using: .utf8),
              let event = try? JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: data)
        else {
            return nil
        }
        return event.runId
    }

    private func decodedStreamFrame(from line: String) -> (requestId: String?, runId: String?, streamId: String)? {
        guard let data = line.data(using: .utf8),
              let frame = try? JSONDecoder().decode(EngineStreamFrame.self, from: data)
        else {
            return nil
        }
        switch frame {
        case .opened(let streamID, let requestID, _, _, let runID, _, _):
            return (requestID, runID, streamID)
        default:
            return (nil, nil, frame.streamID)
        }
    }
}

private final class EngineDaemonSession: @unchecked Sendable {
    let key: EngineSessionKey
    private let process: Process
    private let stdinPipe: Pipe
    private let stdoutPipe: Pipe
    private let stderrPipe: Pipe
    private let responseRouter = EnginePendingResponseRouter()
    private let stderrBuffer = EngineLineBuffer(maxLines: 512)
    private let writeLock = NSLock()

    init(cli: URL, cwd: URL, key: EngineSessionKey) throws {
        self.key = key
        self.process = Process()
        self.stdinPipe = Pipe()
        self.stdoutPipe = Pipe()
        self.stderrPipe = Pipe()

        process.executableURL = cli
        process.arguments = ["daemon", "--stdio", "--workspace", cwd.path]
        process.currentDirectoryURL = OpenSKSCLIProcess.workingDirectory(for: cwd)
        process.environment = OpenSKSCLIProcess.environment(for: cwd)
        process.standardInput = stdinPipe
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        stdoutPipe.fileHandleForReading.readabilityHandler = { [responseRouter] handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            responseRouter.append(data)
        }
        stderrPipe.fileHandleForReading.readabilityHandler = { [stderrBuffer] handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            stderrBuffer.append(data)
        }
        process.terminationHandler = { [stdoutPipe, stderrPipe] _ in
            stdoutPipe.fileHandleForReading.readabilityHandler = nil
            stderrPipe.fileHandleForReading.readabilityHandler = nil
        }

        try process.run()
    }

    deinit {
        terminate()
    }

    var isRunning: Bool {
        process.isRunning
    }

    var terminationStatus: Int32 {
        process.terminationStatus
    }

    func writeRequest(_ data: Data) throws {
        writeLock.lock()
        defer { writeLock.unlock() }
        try stdinPipe.fileHandleForWriting.write(contentsOf: data)
    }

    func registerRequest(_ request: EngineRequestEnvelope) {
        responseRouter.register(request)
    }

    func responseSnapshot(for requestId: String) -> EngineResponseSnapshot {
        responseRouter.snapshot(for: requestId)
    }

    func finishResponse(for requestId: String, timedOut: Bool) -> EngineCollectedResponse {
        responseRouter.finish(requestId: requestId, timedOut: timedOut)
    }

    func cancelResponse(for requestId: String) {
        responseRouter.cancel(requestId: requestId)
    }

    func drainStderrText() -> String {
        stderrBuffer.drainText(includePartial: true)
    }

    func terminate() {
        stdoutPipe.fileHandleForReading.readabilityHandler = nil
        stderrPipe.fileHandleForReading.readabilityHandler = nil
        stdinPipe.fileHandleForWriting.closeFile()
        if process.isRunning {
            process.terminate()
        }
    }
}

actor EngineProcess {
    private var session: EngineDaemonSession?

    func health(cli: URL, cwd: URL) async -> [EngineEvent] {
        let stream = await sendRequest(
            cli: cli,
            cwd: cwd,
            request: EngineRequestEnvelope.health(id: "req-health-\(UInt64(Date().timeIntervalSince1970 * 1000))")
        )
        return stream.engineEvents
    }

    func runStart(
        cli: URL,
        cwd: URL,
        pipelineId: String,
        objective: String,
        runId: String,
        graphPath: String? = nil
    ) async -> EngineRunStream {
        let request = EngineRequestEnvelope.runStart(
            id: "req-\(runId)",
            pipelineId: pipelineId,
            objective: objective,
            runId: runId,
            graphPath: graphPath
        )
        return await sendRequest(cli: cli, cwd: cwd, request: request)
    }

    func runControl(
        cli: URL,
        cwd: URL,
        kind: String,
        runId: String,
        targetId: String?,
        message: String,
        reasonCode: String
    ) async -> EngineRunStream {
        let request = EngineRequestEnvelope.runControl(
            id: "req-\(kind)-\(runId)",
            kind: kind,
            runId: runId,
            targetId: targetId,
            message: message,
            reasonCode: reasonCode
        )
        return await sendRequest(cli: cli, cwd: cwd, request: request)
    }

    func approval(
        cli: URL,
        cwd: URL,
        kind: String,
        runId: String,
        approvalId: String,
        scope: String,
        message: String,
        reasonCode: String
    ) async -> EngineRunStream {
        let request = EngineRequestEnvelope.approval(
            id: "req-\(kind)-\(approvalId)",
            kind: kind,
            runId: runId,
            approvalId: approvalId,
            scope: scope,
            message: message,
            reasonCode: reasonCode
        )
        return await sendRequest(cli: cli, cwd: cwd, request: request)
    }

    func subscribeEvents(
        cli: URL,
        cwd: URL,
        runId: String,
        sinceSequence: UInt64,
        tailMs: UInt64? = nil,
        pollIntervalMs: UInt64? = nil
    ) async -> EngineRunStream {
        let request = EngineRequestEnvelope.subscribeEvents(
            id: "req-subscribe-\(runId)-\(UUID().uuidString)",
            runId: runId,
            sinceSequence: sinceSequence,
            tailMs: tailMs,
            pollIntervalMs: pollIntervalMs
        )
        return await sendRequest(cli: cli, cwd: cwd, request: request)
    }

    func conversationTurnStart(
        cli: URL,
        cwd: URL,
        request: ConversationTurnStartRequest
    ) async -> EngineConversationTurnStartResult {
        let stream = await sendRequest(
            cli: cli,
            cwd: cwd,
            request: EngineRequestEnvelope.conversationTurnStart(request)
        )
        let accepted = Self.decodeConversationTurnAccepted(stream.rawLines)
        var finalStream = stream
        if accepted == nil {
            finalStream.exitCode = finalStream.exitCode == 0 ? 1 : finalStream.exitCode
            finalStream.engineEvents.append(.localError("daemon did not return conversation turn accepted for \(request.requestId)"))
        }
        return EngineConversationTurnStartResult(accepted: accepted, stream: finalStream)
    }

    func conversationSupervisorTick(
        cli: URL,
        cwd: URL,
        requestId: String,
        supervisorId: String,
        leaseTtlMs: UInt64
    ) async -> EngineTurnSupervisorTickResult {
        let stream = await sendRequest(
            cli: cli,
            cwd: cwd,
            request: EngineRequestEnvelope.conversationSupervisorTick(
                id: requestId,
                supervisorId: supervisorId,
                leaseTtlMs: leaseTtlMs
            )
        )
        let tick = Self.decodeTurnSupervisorTickResult(stream.rawLines)
        var finalStream = stream
        if tick == nil {
            finalStream.exitCode = finalStream.exitCode == 0 ? 1 : finalStream.exitCode
            finalStream.engineEvents.append(.localError("daemon did not return turn supervisor tick for \(requestId)"))
        }
        return EngineTurnSupervisorTickResult(tick: tick, stream: finalStream)
    }

    private func ensureSession(cli: URL, cwd: URL) throws -> EngineDaemonSession {
        let key = EngineSessionKey(cliPath: cli.path, cwdPath: cwd.path)
        if let existing = session, existing.key == key, existing.isRunning {
            return existing
        }
        session?.terminate()
        let next = try EngineDaemonSession(cli: cli, cwd: cwd, key: key)
        session = next
        return next
    }

    private func sendRequest(
        cli: URL,
        cwd: URL,
        request: EngineRequestEnvelope
    ) async -> EngineRunStream {
        let daemonSession: EngineDaemonSession
        let body: Data
        do {
            daemonSession = try ensureSession(cli: cli, cwd: cwd)
            var encoded = try JSONEncoder.opensks.encode(request)
            encoded.append(UInt8(ascii: "\n"))
            body = encoded
            daemonSession.registerRequest(request)
            try daemonSession.writeRequest(body)
        } catch {
            session?.cancelResponse(for: request.id)
            return EngineRunStream(
                engineEvents: [.localError("could not send daemon request: \(error.localizedDescription)")],
                executionEvents: [],
                exitCode: -1,
                stderr: ""
            )
        }

        let response = await collectResponse(from: daemonSession, request: request)
        var stream = Self.decodeRunStream(response.lines)
        stream.rawLines = response.lines
        stream.stderr = daemonSession.drainStderrText()

        if daemonSession.isRunning {
            stream.exitCode = stream.engineEvents.contains { $0.severity.isError } ? 1 : 0
        } else {
            let code = daemonSession.terminationStatus
            stream.exitCode = code
            session = nil
            if code != 0 {
                stream.engineEvents.append(.localError("daemon exited \(code): \(stream.stderr)"))
            }
        }

        if !response.sawRequestEvent {
            stream.exitCode = stream.exitCode == 0 ? 1 : stream.exitCode
            stream.engineEvents.append(.localError("daemon did not return a correlated event for \(request.id)"))
        }
        if response.timedOut {
            stream.exitCode = stream.exitCode == 0 ? 1 : stream.exitCode
            stream.engineEvents.append(.localError("daemon response timed out for \(request.id)"))
        }
        return stream
    }

    static func decodeConversationTurnAccepted(_ lines: [String]) -> ConversationTurnAccepted? {
        for line in lines {
            guard let data = line.data(using: .utf8),
                  let accepted = try? JSONDecoder.opensks.decode(ConversationTurnAccepted.self, from: data),
                  accepted.schema == "opensks.conversation-turn-accepted.v1"
            else {
                continue
            }
            return accepted
        }
        return nil
    }

    static func decodeTurnSupervisorTickResult(_ lines: [String]) -> TurnSupervisorTickResult? {
        for line in lines {
            guard let data = line.data(using: .utf8),
                  let tick = try? JSONDecoder.opensks.decode(TurnSupervisorTickResult.self, from: data),
                  tick.schema == "opensks.turn-supervisor-tick.v1"
            else {
                continue
            }
            return tick
        }
        return nil
    }

    private func collectResponse(
        from session: EngineDaemonSession,
        request: EngineRequestEnvelope
    ) async -> EngineCollectedResponse {
        // STREAM-001: completion is signalled by the EXPLICIT per-request terminal
        // marker (`request_completed`), never by a silence/quiet-window heuristic.
        // The deadline is only a timeout safety net — it reports a timeout, it does
        // not fabricate a successful completion. The 20ms sleep is a cheap poll
        // interval, not a completion signal.
        let deadline = Date().addingTimeInterval(responseTimeout(for: request))

        while Date() < deadline {
            let snapshot = session.responseSnapshot(for: request.id)
            if snapshot.isComplete {
                return session.finishResponse(for: request.id, timedOut: false)
            }
            if !session.isRunning {
                return session.finishResponse(for: request.id, timedOut: false)
            }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }

        return session.finishResponse(for: request.id, timedOut: true)
    }

    private func responseTimeout(for request: EngineRequestEnvelope) -> TimeInterval {
        let tailSeconds = Double(request.params.tailMs ?? 0) / 1_000.0
        return max(8.0, tailSeconds + 3.0)
    }

    static func decodeRunStream(_ lines: [String]) -> EngineRunStream {
        let joined = lines.joined(separator: "\n")
        return decodeRunStream(Data(joined.utf8))
    }

    static func decodeRunStream(_ data: Data) -> EngineRunStream {
        var stream = EngineRunStream(engineEvents: [], executionEvents: [], exitCode: nil, stderr: "")
        var seenExecutionEventIDs = Set<String>()
        func appendExecutionEvent(_ event: ExecutionEventEnvelope) {
            guard seenExecutionEventIDs.insert(event.id).inserted else { return }
            stream.executionEvents.append(event)
        }
        for line in data.split(separator: UInt8(ascii: "\n")) where !line.isEmpty {
            let lineData = Data(line)
            if let engineEvent = try? JSONDecoder.opensks.decode(EngineEvent.self, from: lineData) {
                stream.engineEvents.append(engineEvent)
            } else if let executionEvent = try? JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: lineData) {
                appendExecutionEvent(executionEvent)
            } else if let frame = try? JSONDecoder().decode(EngineStreamFrame.self, from: lineData) {
                switch frame {
                case .event(_, _, let eventPayload):
                    if let eventData = try? JSONEncoder().encode(eventPayload),
                       let executionEvent = try? JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: eventData) {
                        appendExecutionEvent(executionEvent)
                    }
                case .failed(let streamID, let cursor, let error, let resumable):
                    stream.streamFailures.append(
                        EngineStreamFailure(
                            streamID: streamID,
                            cursor: cursor,
                            error: error,
                            resumable: resumable
                        )
                    )
                default:
                    break
                }
            }
        }
        return stream
    }
}

extension JSONDecoder {
    static var opensks: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }
}

extension JSONEncoder {
    static var opensks: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return encoder
    }
}

extension EngineEvent {
    static func localError(_ message: String) -> EngineEvent {
        EngineEvent(
            schema: "opensks.engine-event.v1",
            eventId: UUID().uuidString,
            requestId: nil,
            eventType: .error,
            severity: .error,
            message: message,
            protocolVersion: "opensks.contracts.v1",
            timestampMs: 0,
            evidenceRefs: [],
            redacted: true
        )
    }
}

struct EngineRunStream: Sendable {
    var engineEvents: [EngineEvent]
    var executionEvents: [ExecutionEventEnvelope]
    var exitCode: Int32?
    var stderr: String
    var streamFailures: [EngineStreamFailure] = []
    var rawLines: [String] = []
}

struct EngineStreamFailure: Sendable, Equatable {
    let streamID: String
    let cursor: UInt64
    let error: PublicEngineError
    let resumable: Bool
}

struct EngineConversationTurnStartResult: Sendable {
    let accepted: ConversationTurnAccepted?
    let stream: EngineRunStream
}

struct EngineTurnSupervisorTickResult: Sendable {
    let tick: TurnSupervisorTickResult?
    let stream: EngineRunStream
}

struct EngineRequestEnvelope: Encodable {
    let schema: String
    let id: String
    let kind: String
    let protocolVersion: String
    let params: EngineRequestParams

    static func health(id: String) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: id,
            kind: "health",
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(
                pipelineId: nil,
                graphPath: nil,
                objective: nil,
                runId: nil,
                targetId: nil,
                message: nil,
                reasonCode: nil,
                approvalId: nil,
                scope: nil,
                sinceSequence: nil,
                tailMs: nil,
                pollIntervalMs: nil
            )
        )
    }

    static func conversationTurnStart(_ request: ConversationTurnStartRequest) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: request.requestId,
            kind: "conversation_turn_start",
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(conversationTurnStart: request)
        )
    }

    static func conversationSupervisorTick(
        id: String,
        supervisorId: String,
        leaseTtlMs: UInt64
    ) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: id,
            kind: "conversation_supervisor_tick",
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(
                supervisorId: supervisorId,
                leaseTtlMs: leaseTtlMs,
                reasonCode: "conversation_supervisor_tick_requested"
            )
        )
    }

    static func runStart(
        id: String,
        pipelineId: String,
        objective: String,
        runId: String,
        graphPath: String? = nil
    ) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: id,
            kind: "run_start",
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(
                pipelineId: pipelineId,
                graphPath: graphPath,
                objective: objective,
                runId: runId,
                targetId: nil,
                message: nil,
                reasonCode: nil,
                approvalId: nil,
                scope: nil,
                sinceSequence: nil,
                tailMs: nil,
                pollIntervalMs: nil
            )
        )
    }

    static func runControl(
        id: String,
        kind: String,
        runId: String,
        targetId: String?,
        message: String,
        reasonCode: String
    ) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: id,
            kind: kind,
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(
                pipelineId: nil,
                graphPath: nil,
                objective: nil,
                runId: runId,
                targetId: targetId,
                message: message,
                reasonCode: reasonCode,
                approvalId: nil,
                scope: nil,
                sinceSequence: nil,
                tailMs: nil,
                pollIntervalMs: nil
            )
        )
    }

    static func approval(
        id: String,
        kind: String,
        runId: String,
        approvalId: String,
        scope: String,
        message: String,
        reasonCode: String
    ) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: id,
            kind: kind,
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(
                pipelineId: nil,
                graphPath: nil,
                objective: nil,
                runId: runId,
                targetId: nil,
                message: message,
                reasonCode: reasonCode,
                approvalId: approvalId,
                scope: scope,
                sinceSequence: nil,
                tailMs: nil,
                pollIntervalMs: nil
            )
        )
    }

    static func subscribeEvents(
        id: String,
        runId: String,
        sinceSequence: UInt64,
        tailMs: UInt64? = nil,
        pollIntervalMs: UInt64? = nil
    ) -> EngineRequestEnvelope {
        EngineRequestEnvelope(
            schema: "opensks.engine-request.v1",
            id: id,
            kind: "subscribe_events",
            protocolVersion: "opensks.contracts.v1",
            params: EngineRequestParams(
                pipelineId: nil,
                graphPath: nil,
                objective: nil,
                runId: runId,
                targetId: nil,
                message: nil,
                reasonCode: "reconnect_replay_requested",
                approvalId: nil,
                scope: nil,
                sinceSequence: sinceSequence,
                tailMs: tailMs,
                pollIntervalMs: pollIntervalMs
            )
        )
    }
}

struct EngineRequestParams: Encodable {
    var conversationTurnStart: ConversationTurnStartRequest? = nil
    var supervisorId: String? = nil
    var leaseTtlMs: UInt64? = nil
    var pipelineId: String? = nil
    var graphPath: String? = nil
    var objective: String? = nil
    var runId: String? = nil
    var targetId: String? = nil
    var message: String? = nil
    var reasonCode: String? = nil
    var approvalId: String? = nil
    var scope: String? = nil
    var sinceSequence: UInt64? = nil
    var tailMs: UInt64? = nil
    var pollIntervalMs: UInt64? = nil
}

// MARK: - File tree

struct FileNode: Identifiable {
    let id: String
    let name: String
    let isDir: Bool
    var children: [FileNode]?
}

enum FileScanner {
    private static let skip: Set<String> = ["target", "node_modules", ".git", ".opensks", ".sneakoscope", ".build"]
    private static let secretMarkers = [".env", ".key", ".pem", ".p12", ".pfx", "id_rsa", "credentials", ".token", ".secret", "secret", ".keychain"]

    static func scan(_ root: URL, depth: Int = 0, maxNodes: Int = 600) -> [FileNode] {
        var remaining = maxNodes
        return scan(root, depth: depth, remaining: &remaining)
    }

    private static func scan(_ root: URL, depth: Int, remaining: inout Int) -> [FileNode] {
        guard remaining > 0 else { return [] }
        guard depth < 6 else { return [] }
        let keys: [URLResourceKey] = [.isDirectoryKey]
        guard let items = try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles]
        ) else { return [] }
        var nodes: [FileNode] = []
        for url in items {
            guard remaining > 0 else { break }
            let name = url.lastPathComponent
            let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if isDir && skip.contains(name) { continue }
            remaining -= 1
            if isDir {
                nodes.append(FileNode(id: url.path, name: name, isDir: true, children: scan(url, depth: depth + 1, remaining: &remaining)))
            } else {
                nodes.append(FileNode(id: url.path, name: name, isDir: false, children: nil))
            }
        }
        return nodes.sorted { a, b in
            a.isDir != b.isDir ? a.isDir : a.name.lowercased() < b.name.lowercased()
        }
    }

    static func looksSecret(_ path: String) -> Bool {
        let file = (path as NSString).lastPathComponent.lowercased()
        return secretMarkers.contains { file.contains($0) }
    }

    static func read(_ path: String) -> String {
        if looksSecret(path) { return "// hidden for safety — this path may contain credentials." }
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)) else {
            return "// could not read file."
        }
        if data.count > 512 * 1024 { return "// file too large to preview (512 KB cap)." }
        if data.prefix(8000).contains(0) { return "// binary file — preview not available." }
        return String(decoding: data, as: UTF8.self)
    }
}

// MARK: - Proof artifact freshness

struct ProofArtifactFingerprint: Equatable, Sendable {
    let files: [ProofArtifactFileFingerprint]
}

struct ProofArtifactFileFingerprint: Equatable, Sendable {
    let relativePath: String
    let exists: Bool
    let isDirectory: Bool
    let byteCount: Int
    let childCount: Int
    let contentHash: UInt64
}

enum ProofArtifactMonitor {
    static let relativePaths = [
        ".opensks/acceptance/acceptance-summary.json",
        ".opensks/qa/qa-report.json",
        ".opensks/qa/security-audit.json",
        ".opensks/security/security-audit.json",
        ".opensks/providers/provider-dashboard.json",
        ".opensks/triwiki/voxel-index-report.json",
        ".opensks/missions",
        ".opensks/browser",
        ".opensks/computer-use",
        ".opensks/app-use",
    ]

    static func fingerprint(workspace: URL, fileManager: FileManager = .default) -> ProofArtifactFingerprint {
        ProofArtifactFingerprint(files: relativePaths.map {
            fingerprint(relativePath: $0, workspace: workspace, fileManager: fileManager)
        })
    }

    private static func fingerprint(
        relativePath: String,
        workspace: URL,
        fileManager: FileManager
    ) -> ProofArtifactFileFingerprint {
        let url = workspace.appendingPathComponent(relativePath, isDirectory: false)
        var isDirectory = ObjCBool(false)
        guard fileManager.fileExists(atPath: url.path, isDirectory: &isDirectory) else {
            return ProofArtifactFileFingerprint(
                relativePath: relativePath,
                exists: false,
                isDirectory: false,
                byteCount: 0,
                childCount: 0,
                contentHash: 0
            )
        }

        if isDirectory.boolValue {
            let children = (try? fileManager.contentsOfDirectory(atPath: url.path).sorted()) ?? []
            return ProofArtifactFileFingerprint(
                relativePath: relativePath,
                exists: true,
                isDirectory: true,
                byteCount: 0,
                childCount: children.count,
                contentHash: hash(strings: children)
            )
        }

        let attributes = (try? fileManager.attributesOfItem(atPath: url.path)) ?? [:]
        let byteCount = (attributes[.size] as? NSNumber)?.intValue ?? 0
        let modifiedAt = (attributes[.modificationDate] as? Date)?.timeIntervalSince1970 ?? 0
        return ProofArtifactFileFingerprint(
            relativePath: relativePath,
            exists: true,
            isDirectory: false,
            byteCount: byteCount,
            childCount: 0,
            contentHash: hash(strings: [String(byteCount), String(modifiedAt)])
        )
    }

    private static func hash(data: Data) -> UInt64 {
        var value: UInt64 = 1_469_598_103_934_665_603
        for byte in data {
            value ^= UInt64(byte)
            value &*= 1_099_511_628_211
        }
        return value
    }

    private static func hash(strings: [String]) -> UInt64 {
        var value: UInt64 = 1_469_598_103_934_665_603
        for string in strings {
            for byte in string.utf8 {
                value ^= UInt64(byte)
                value &*= 1_099_511_628_211
            }
            value ^= 0xff
            value &*= 1_099_511_628_211
        }
        return value
    }
}

// MARK: - App state (single source of truth)

@MainActor
final class AppState: ObservableObject {
    @Published var data: AppData?
    @Published var loadError: String?

    @Published var lines: [RunLine] = []
    @Published var isRunning = false
    @Published var lastExit: Int32?
    @Published var lastVerb = ""
    @Published var engineEvents: [EngineEvent] = []
    @Published var engineStatus = "Not verified"
    @Published var currentEngineRunId: String?
    let executionStore = ExecutionStore()
    let intelligenceStore = ProjectIntelligenceStore()
    let graphEditorStore = GraphEditorStore()
    /// The node-level pipeline projection that drives the graph. Wired at
    /// bootstrap (RootView.onAppear) so streamed execution events reach it —
    /// previously nothing called `ingest`, so the graph stayed empty (PIPE-001).
    weak var pipelines: PipelineProjectionStore?

    /// Apply a streamed execution event to BOTH read models: the flat
    /// `ExecutionStore` and the node-level pipeline projection. Centralised so a
    /// new stream consumer cannot forget one (PIPE-001).
    func applyExecutionEvent(_ event: ExecutionEventEnvelope) {
        executionStore.apply(event)
        pipelines?.ingest(event)
    }

    // Navigation has one source of truth (NavigationStore.route); the explorer
    // pane is derived from the route via WorkspaceRoute.legacySection (SHELL-003).
    @Published var terminalTab: TerminalTab = .output
    @Published var terminalCollapsed = false

    @Published var fileRoots: [FileNode] = []

    /// The real editable code workspace (PR-032). Backed by the bundled
    /// `opensks file …` CLI through the hardened file service.
    let editorStore: EditorWorkspaceStore
    /// Absolute path of the active editor document, for the Explorer's row
    /// selection highlight (kept in sync as documents are focused).
    @Published var activeEditorPath: String?

    @Published var objective = ""
    @Published var runMode: RunMode = .goal
    @Published var showPalette = false
    /// Drives the discoverable keyboard-shortcuts help sheet (PR-045). Opened with
    /// ⌘/ or the titlebar "?" affordance.
    @Published var showHelp = false

    let workspace: URL
    let cli: URL
    private let runner = CLIRunner()
    private let engine = EngineProcess()
    private var runTask: Task<Void, Never>?
    private var proofRefreshTask: Task<Void, Never>?
    private var lastProofArtifactFingerprint: ProofArtifactFingerprint?
    private var workspaceSecurityScope: URL?

    init() {
        let fileManager = FileManager.default
        let cwd = fileManager.currentDirectoryPath
        var ws = cwd == "/" ? fileManager.homeDirectoryForCurrentUser.path : cwd
        var cliPath = ws + "/target/debug/opensks"
        for res in Self.launchResourceDirectories() {
            if let txt = try? String(contentsOf: res.appendingPathComponent("workspace-path.txt"), encoding: .utf8) {
                let trimmed = txt.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty { ws = trimmed }
            }
            let candidate = res.appendingPathComponent("opensks-cli")
            if fileManager.fileExists(atPath: candidate.path) { cliPath = candidate.path }
        }
        let workspaceURL = URL(fileURLWithPath: ws, isDirectory: true)
        let cliURL = URL(fileURLWithPath: cliPath)
        self.workspace = workspaceURL
        self.cli = cliURL
        self.editorStore = EditorWorkspaceStore(
            service: LiveEditorFileService(cli: cliURL, workspace: workspaceURL)
        )
        refreshFileRoots()
    }

    deinit {
        workspaceSecurityScope?.stopAccessingSecurityScopedResource()
    }

    func refreshFileRoots() {
        self.fileRoots = []
        let workspaceForScan = workspace
        Task { [weak self] in
            let roots = await Task.detached(priority: .utility) {
                FileScanner.scan(workspaceForScan)
            }.value
            guard let self, self.workspace == workspaceForScan else { return }
            self.fileRoots = roots
        }
    }

    func requestWorkspaceAccess() {
        let panel = NSOpenPanel()
        panel.title = "Open Workspace"
        panel.prompt = "Allow"
        panel.message = "Choose the active OpenSKS workspace folder."
        panel.directoryURL = Self.workspaceAccessPanelDirectory(for: workspace)
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = false
        panel.allowsMultipleSelection = false

        guard panel.runModal() == .OK, let selected = panel.url else { return }

        let selectedPath = selected.standardizedFileURL.resolvingSymlinksInPath().path
        let workspacePath = workspace.standardizedFileURL.resolvingSymlinksInPath().path
        guard selectedPath == workspacePath else {
            loadError = "selected folder is not the active workspace"
            return
        }

        workspaceSecurityScope?.stopAccessingSecurityScopedResource()
        workspaceSecurityScope = selected.startAccessingSecurityScopedResource() ? selected : nil
        loadError = nil
        append(RunLine(text: "[workspace] access refreshed for \(workspace.path)", kind: .info))
        refreshFileRoots()
        loadData()
        connectEngine()
    }

    static func workspaceAccessPanelDirectory(for workspace: URL) -> URL {
        workspace.deletingLastPathComponent()
    }

    private static func launchResourceDirectories(bundle: Bundle = .main) -> [URL] {
        var candidates: [URL] = []
        if let resourceURL = bundle.resourceURL {
            candidates.append(resourceURL)
        }
        candidates.append(bundle.bundleURL.appendingPathComponent("Contents/Resources", isDirectory: true))
        if let executableURL = bundle.executableURL {
            candidates.append(
                executableURL
                    .deletingLastPathComponent()
                    .deletingLastPathComponent()
                    .appendingPathComponent("Resources", isDirectory: true)
            )
        }
        var seen = Set<String>()
        return candidates.filter { url in
            let path = url.standardizedFileURL.path
            return seen.insert(path).inserted
        }
    }

    /// Convert an absolute (or already-relative) path into a workspace-relative
    /// path the file service understands. Returns nil for paths outside the
    /// workspace (the service would reject them as an escape anyway).
    func workspaceRelativePath(for path: String) -> String? {
        let wsPath = workspace.standardizedFileURL.path
        let std = URL(fileURLWithPath: path).standardizedFileURL.path
        if std == wsPath { return "." }
        let prefix = wsPath.hasSuffix("/") ? wsPath : wsPath + "/"
        if std.hasPrefix(prefix) {
            return String(std.dropFirst(prefix.count))
        }
        // Not absolute under the workspace: assume it is already relative.
        if !path.hasPrefix("/") { return path }
        return nil
    }

    /// Sync the Explorer's selection highlight from the editor store's active
    /// document. Call after any open/close in the editor.
    func syncActiveEditorPath() {
        guard let rel = editorStore.activeDocument?.workspaceRelativePath else {
            activeEditorPath = nil
            return
        }
        if rel == "." {
            activeEditorPath = workspace.path
        } else {
            activeEditorPath = workspace.appendingPathComponent(rel).path
        }
    }

    func loadData() {
        let cli = self.cli
        let ws = self.workspace
        let runner = self.runner
        Task {
            let result = await runner.capture(cli: cli, cwd: ws, args: ["app-data", ws.path])
            if result.exitCode != 0 || result.timedOut || result.stdout.isEmpty {
                self.loadError = Self.appDataLoadError(result)
                return
            }
            do {
                let data = try JSONDecoder.opensks.decode(AppData.self, from: result.stdout)
                let fingerprint = await Self.proofArtifactFingerprint(workspace: ws)
                self.data = data
                self.lastProofArtifactFingerprint = fingerprint
                self.loadError = nil
            } catch {
                self.loadError = "could not decode app-data: \(error.localizedDescription)"
            }
        }
    }

    static func appDataLoadError(_ result: CLICaptureResult) -> String {
        var message: String
        if let launchError = result.launchError {
            message = "opensks-cli app-data launch failed: \(singleLineDiagnostic(launchError))"
        } else if result.timedOut {
            message = "opensks-cli app-data timed out after \(Int(OpenSKSCLIProcess.commandTimeoutSeconds))s"
        } else if let exitCode = result.exitCode, exitCode != 0 {
            message = "opensks-cli app-data exited \(exitCode)"
        } else {
            message = "opensks-cli app-data returned no output"
        }

        let stderr = singleLineDiagnostic(result.stderr)
        if !stderr.isEmpty {
            message += " - \(stderr)"
        }
        return message
    }

    private static func singleLineDiagnostic(_ raw: String, limit: Int = 180) -> String {
        let normalized = raw
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        guard normalized.count > limit else { return normalized }
        let end = normalized.index(normalized.startIndex, offsetBy: limit)
        return String(normalized[..<end]) + "..."
    }

    func startProofArtifactRefresh(intervalNanoseconds: UInt64 = 2_000_000_000) {
        guard proofRefreshTask == nil else { return }
        proofRefreshTask = Task { [weak self] in
            guard let self else { return }
            self.lastProofArtifactFingerprint = await Self.proofArtifactFingerprint(workspace: self.workspace)
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: intervalNanoseconds)
                let next = await Self.proofArtifactFingerprint(workspace: self.workspace)
                if next != self.lastProofArtifactFingerprint {
                    self.lastProofArtifactFingerprint = next
                    self.loadData()
                }
            }
        }
    }

    private static func proofArtifactFingerprint(workspace: URL) async -> ProofArtifactFingerprint {
        await Task.detached(priority: .utility) {
            ProofArtifactMonitor.fingerprint(workspace: workspace)
        }.value
    }

    func stopProofArtifactRefresh() {
        proofRefreshTask?.cancel()
        proofRefreshTask = nil
    }

    func connectEngine() {
        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        Task {
            let events = await engine.health(cli: cli, cwd: ws)
            self.engineEvents = events
            self.engineStatus = events.contains { $0.eventType == .engineHealth } ? "Ready" : "Not verified"
            for event in events {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
        }
    }

    func startRun() {
        let trimmed = objective.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        runVerb(label: "\(runMode.verb) run", args: [runMode.verb, trimmed])
    }

    func startEngineRun() {
        let trimmed = objective.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !isRunning else { return }
        isRunning = true
        lastExit = nil
        lastVerb = "engine run"
        terminalCollapsed = false
        terminalTab = .output
        let runId = "studio-\(UInt64(Date().timeIntervalSince1970 * 1000))"
        currentEngineRunId = runId
        append(RunLine(
            text: "$ opensks daemon run_start single-model-safe \(runId)",
            kind: .cmd
        ))

        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        runTask = Task {
            let stream = await engine.runStart(
                cli: cli,
                cwd: ws,
                pipelineId: "single-model-safe",
                objective: trimmed,
                runId: runId
            )
            self.engineEvents = stream.engineEvents
            for event in stream.engineEvents {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
            for event in stream.executionEvents {
                self.applyExecutionEvent(event)
                self.append(RunLine(
                    text: "[event] \(event.kind.rawValue) #\(event.sequence)",
                    kind: .info
                ))
            }
            if !stream.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                self.append(RunLine(text: "! \(stream.stderr)", kind: .warn))
            }
            self.lastExit = stream.exitCode
            self.isRunning = false
            self.loadData()
        }
    }

    func pauseEngineRun() {
        sendEngineControl(kind: "run_pause", message: "pause requested", reasonCode: "paused_by_user")
    }

    func resumeEngineRun() {
        sendEngineControl(kind: "run_resume", message: "resume requested", reasonCode: "resumed_by_user")
    }

    func cancelEngineRun() {
        sendEngineControl(kind: "run_cancel", message: "cancel requested", reasonCode: "cancelled_by_user")
    }

    func steerEngineRun() {
        let message = objective.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else { return }
        sendEngineControl(
            kind: "run_steer",
            targetId: executionStore.queueItems.first?.id,
            message: message,
            reasonCode: "user_steering"
        )
    }

    func requestEngineApproval() {
        guard let runId = currentEngineRunId ?? executionStore.runs.first?.id else { return }
        let approvalId = "approval-\(UInt64(Date().timeIntervalSince1970 * 1000))"
        sendApproval(
            kind: "approval_request",
            runId: runId,
            approvalId: approvalId,
            scope: "git_push",
            message: "Approval requested for external side effect",
            reasonCode: "approval_required"
        )
    }

    func approveFirstApproval() {
        guard let approval = executionStore.approvals.first else { return }
        sendApproval(
            kind: "approval_approve",
            runId: approval.runId,
            approvalId: approval.id,
            scope: approval.scope,
            message: "Approval granted",
            reasonCode: "approved_by_user"
        )
    }

    func denyFirstApproval() {
        guard let approval = executionStore.approvals.first else { return }
        sendApproval(
            kind: "approval_deny",
            runId: approval.runId,
            approvalId: approval.id,
            scope: approval.scope,
            message: "Approval denied",
            reasonCode: "denied_by_user"
        )
    }

    func replayEngineRun() {
        guard let runId = currentEngineRunId ?? executionStore.runs.first?.id, !isRunning else { return }
        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        append(RunLine(text: "$ opensks daemon subscribe_events \(runId)", kind: .cmd))
        isRunning = true
        lastVerb = "replay"
        runTask = Task {
            let stream = await engine.subscribeEvents(
                cli: cli,
                cwd: ws,
                runId: runId,
                sinceSequence: 0
            )
            self.engineEvents = stream.engineEvents
            for event in stream.engineEvents {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
            if !stream.executionEvents.isEmpty {
                self.executionStore.rebuild(from: stream.executionEvents)
            }
            for event in stream.executionEvents {
                self.append(RunLine(text: "[replay] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.isRunning = false
        }
    }

    func tailEngineRun() {
        guard let runId = currentEngineRunId ?? executionStore.runs.first?.id, !isRunning else { return }
        let sinceSequence = executionStore.latestSequence(for: runId)
        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        append(RunLine(
            text: "$ opensks daemon subscribe_events \(runId) --since \(sinceSequence) --tail-ms 1500",
            kind: .cmd
        ))
        isRunning = true
        lastVerb = "tail"
        runTask = Task {
            let stream = await engine.subscribeEvents(
                cli: cli,
                cwd: ws,
                runId: runId,
                sinceSequence: sinceSequence,
                tailMs: 1_500,
                pollIntervalMs: 100
            )
            self.engineEvents = stream.engineEvents
            for event in stream.engineEvents {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
            for event in stream.executionEvents {
                self.applyExecutionEvent(event)
                self.append(RunLine(text: "[tail] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.isRunning = false
        }
    }

    func loadGraphTemplate() {
        graphEditorStore.loadSingleModelSafeTemplate()
        append(RunLine(text: "[graph] loaded Single Model Safe template", kind: .info))
    }

    func saveGraphEditorDocument() {
        do {
            let url = try graphEditorStore.saveCurrentDocument(workspace: workspace)
            append(RunLine(text: "[graph] saved \(url.path)", kind: .info))
            if let graphPath = graphEditorStore.lastExportedGraphPath {
                append(RunLine(text: "[graph] exported \(graphPath)", kind: .info))
            }
        } catch {
            append(RunLine(text: "[graph] save failed: \(error.localizedDescription)", kind: .danger))
        }
    }

    func loadGraphEditorDocument() {
        do {
            let document = try graphEditorStore.loadSavedDocument(workspace: workspace)
            append(RunLine(text: "[graph] loaded \(document.name)", kind: .info))
        } catch {
            append(RunLine(text: "[graph] load failed: \(error.localizedDescription)", kind: .danger))
        }
    }

    func runGraphEditorDocument() {
        if graphEditorStore.nodes.isEmpty {
            graphEditorStore.loadSingleModelSafeTemplate()
        }
        guard graphEditorStore.problems.isEmpty else {
            append(RunLine(text: "[graph] run blocked by compile problems", kind: .warn))
            return
        }
        do {
            let url = try graphEditorStore.saveCurrentDocument(workspace: workspace)
            append(RunLine(text: "[graph] saved \(url.lastPathComponent) before run", kind: .info))
            if let graphPath = graphEditorStore.lastExportedGraphPath {
                append(RunLine(text: "[graph] exported \(URL(fileURLWithPath: graphPath).lastPathComponent) before run", kind: .info))
            }
        } catch {
            append(RunLine(text: "[graph] save before run failed: \(error.localizedDescription)", kind: .danger))
            return
        }
        guard !isRunning else { return }
        isRunning = true
        lastExit = nil
        lastVerb = "graph run"
        terminalCollapsed = false
        terminalTab = .output
        let runId = "graph-\(UInt64(Date().timeIntervalSince1970 * 1000))"
        currentEngineRunId = runId
        append(RunLine(
            text: "$ opensks daemon run_start \(graphEditorStore.documentId) \(runId) --graph-path \(graphEditorStore.exportedGraphRelativePath)",
            kind: .cmd
        ))

        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        let pipelineId = graphEditorStore.documentId
        let objective = objective.trimmingCharacters(in: .whitespacesAndNewlines)
        runTask = Task {
            let stream = await engine.runStart(
                cli: cli,
                cwd: ws,
                pipelineId: pipelineId,
                objective: objective.isEmpty ? "Run graph editor document" : objective,
                runId: runId,
                graphPath: self.graphEditorStore.exportedGraphRelativePath
            )
            self.engineEvents = stream.engineEvents
            for event in stream.engineEvents {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
            for event in stream.executionEvents {
                self.applyExecutionEvent(event)
                self.append(RunLine(text: "[graph event] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.isRunning = false
        }
    }

    private func sendEngineControl(
        kind: String,
        targetId: String? = nil,
        message: String,
        reasonCode: String
    ) {
        guard let runId = currentEngineRunId ?? executionStore.runs.first?.id, !isRunning else { return }
        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        append(RunLine(text: "$ opensks daemon \(kind) \(runId)", kind: .cmd))
        runTask = Task {
            let stream = await engine.runControl(
                cli: cli,
                cwd: ws,
                kind: kind,
                runId: runId,
                targetId: targetId,
                message: message,
                reasonCode: reasonCode
            )
            self.engineEvents = stream.engineEvents
            for event in stream.engineEvents {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
            for event in stream.executionEvents {
                self.applyExecutionEvent(event)
                self.append(RunLine(text: "[event] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
        }
    }

    private func sendApproval(
        kind: String,
        runId: String,
        approvalId: String,
        scope: String,
        message: String,
        reasonCode: String
    ) {
        guard !isRunning else { return }
        let cli = self.cli
        let ws = self.workspace
        let engine = self.engine
        append(RunLine(text: "$ opensks daemon \(kind) \(approvalId)", kind: .cmd))
        runTask = Task {
            let stream = await engine.approval(
                cli: cli,
                cwd: ws,
                kind: kind,
                runId: runId,
                approvalId: approvalId,
                scope: scope,
                message: message,
                reasonCode: reasonCode
            )
            self.engineEvents = stream.engineEvents
            for event in stream.engineEvents {
                self.append(RunLine(
                    text: "[engine] \(event.eventType.rawValue): \(event.message)",
                    kind: event.severity.isError ? .danger : .info
                ))
            }
            for event in stream.executionEvents {
                self.applyExecutionEvent(event)
                self.append(RunLine(text: "[event] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
        }
    }

    func runVerb(label: String, args: [String]) {
        guard !isRunning else { return }
        isRunning = true
        lastExit = nil
        lastVerb = label
        terminalCollapsed = false
        terminalTab = .output
        append(RunLine(text: "$ opensks " + args.joined(separator: " "), kind: .cmd))

        let cli = self.cli
        let ws = self.workspace
        let runner = self.runner
        runTask = Task {
            for await event in runner.stream(cli: cli, cwd: ws, args: args) {
                switch event {
                case .line(let line):
                    self.append(line)
                case .finished(let code):
                    self.lastExit = code
                    self.append(RunLine(
                        text: "— finished (exit \(code)) —",
                        kind: code == 0 ? .done : .danger
                    ))
                }
            }
            self.isRunning = false
            self.loadData()
        }
    }

    func runAcceptance() { runVerb(label: "acceptance audit", args: ["acceptance", "audit"]) }

    func runReleaseProof() {
        runVerb(label: "release proof", args: ["release", "proof"])
    }

    func runProviderAdapterCheck() {
        runVerb(label: "provider adapter-check", args: ["provider", "adapter-check"])
    }

    func runSecurityAudit() {
        runVerb(label: "security audit", args: ["security", "audit"])
    }

    func rebuildExecutionState(from events: [ExecutionEventEnvelope]) {
        executionStore.rebuild(from: events)
        objectWillChange.send()
    }

    private func append(_ line: RunLine) {
        lines.append(line)
        if lines.count > 2000 { lines.removeFirst(lines.count - 2000) }
    }

    func clearOutput() { lines.removeAll() }

    /// Open a file (absolute or workspace-relative path) in the real editable
    /// code workspace. Routing through the editor store focuses an existing tab
    /// for the same path instead of duplicating it; the Explorer highlight is
    /// kept in sync.
    func openFile(_ path: String) {
        guard let rel = workspaceRelativePath(for: path) else {
            editorStore.openError = "\((path as NSString).lastPathComponent): outside the workspace"
            return
        }
        Task {
            await editorStore.open(path: rel)
            self.syncActiveEditorPath()
        }
    }

    /// Save the active editor document.
    func saveActiveFile() {
        Task { await editorStore.save() }
    }

    /// Save every dirty editor document.
    func saveAllFiles() {
        Task { await editorStore.saveAll() }
    }

    /// Close the active editor tab with dirty protection.
    func closeActiveFile() {
        _ = editorStore.closeActive(dirtyProtection: true)
        syncActiveEditorPath()
    }

    func reveal(_ path: String) {
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }
}
