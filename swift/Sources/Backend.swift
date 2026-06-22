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

actor CLIRunner {
    /// Run `cli args` capturing all stdout. Used for the quick `app-data` read.
    func capture(cli: URL, cwd: URL, args: [String]) -> Data {
        let proc = Process()
        proc.executableURL = cli
        proc.arguments = args
        proc.currentDirectoryURL = cwd
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = Pipe()
        do { try proc.run() } catch { return Data() }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()
        return data
    }

    /// Run `cli args` streaming stdout/stderr line-by-line as events.
    nonisolated func stream(cli: URL, cwd: URL, args: [String]) -> AsyncStream<RunEvent> {
        AsyncStream { continuation in
            let proc = Process()
            proc.executableURL = cli
            proc.arguments = args
            proc.currentDirectoryURL = cwd
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
    var lastLineAt: Date
}

final class EnginePendingResponseRouter: @unchecked Sendable {
    private let lock = NSLock()
    private var partial = Data()
    private var pending: [String: EnginePendingResponse] = [:]
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
            return EngineResponseSnapshot(lines: [], sawRequestEvent: false, lastLineAt: Date())
        }
        return EngineResponseSnapshot(
            lines: response.lines,
            sawRequestEvent: response.sawRequestEvent,
            lastLineAt: response.lastLineAt
        )
    }

    func finish(requestId: String, timedOut: Bool) -> EngineCollectedResponse {
        lock.lock()
        defer { lock.unlock() }
        let response = pending.removeValue(forKey: requestId)
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
    }

    private func route(_ line: String) {
        let requestId = decodedEngineRequestId(from: line)
        let runId = decodedExecutionRunId(from: line)
        let matchedIds = matchedPendingIds(requestId: requestId, runId: runId)
        guard !matchedIds.isEmpty else {
            return
        }
        nextLineOrder += 1
        let lineOrder = nextLineOrder
        let now = Date()
        for id in matchedIds {
            guard var response = pending[id] else { continue }
            response.lines.append(line)
            if requestId == id {
                response.sawRequestEvent = true
            }
            response.lastLineOrder = lineOrder
            response.lastLineAt = now
            pending[id] = response
        }
    }

    private func matchedPendingIds(requestId: String?, runId: String?) -> [String] {
        if let requestId, pending[requestId] != nil {
            return [requestId]
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

    private func decodedEngineRequestId(from line: String) -> String? {
        guard let data = line.data(using: .utf8),
              let event = try? JSONDecoder.opensks.decode(EngineEvent.self, from: data)
        else {
            return nil
        }
        return event.requestId
    }

    private func decodedExecutionRunId(from line: String) -> String? {
        guard let data = line.data(using: .utf8),
              let event = try? JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: data)
        else {
            return nil
        }
        return event.runId
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
        process.currentDirectoryURL = cwd
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

    private func collectResponse(
        from session: EngineDaemonSession,
        request: EngineRequestEnvelope
    ) async -> EngineCollectedResponse {
        let deadline = Date().addingTimeInterval(responseTimeout(for: request))
        let quietWindow: TimeInterval = 0.15

        while Date() < deadline {
            let snapshot = session.responseSnapshot(for: request.id)
            if snapshot.sawRequestEvent && Date().timeIntervalSince(snapshot.lastLineAt) >= quietWindow {
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
        for line in data.split(separator: UInt8(ascii: "\n")) where !line.isEmpty {
            let lineData = Data(line)
            if let engineEvent = try? JSONDecoder.opensks.decode(EngineEvent.self, from: lineData) {
                stream.engineEvents.append(engineEvent)
            } else if let executionEvent = try? JSONDecoder.opensks.decode(ExecutionEventEnvelope.self, from: lineData) {
                stream.executionEvents.append(executionEvent)
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
    let pipelineId: String?
    let graphPath: String?
    let objective: String?
    let runId: String?
    let targetId: String?
    let message: String?
    let reasonCode: String?
    let approvalId: String?
    let scope: String?
    let sinceSequence: UInt64?
    let tailMs: UInt64?
    let pollIntervalMs: UInt64?
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

    static func scan(_ root: URL, depth: Int = 0) -> [FileNode] {
        guard depth < 6 else { return [] }
        let keys: [URLResourceKey] = [.isDirectoryKey]
        guard let items = try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles]
        ) else { return [] }
        var nodes: [FileNode] = []
        for url in items {
            let name = url.lastPathComponent
            let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if isDir && skip.contains(name) { continue }
            if isDir {
                nodes.append(FileNode(id: url.path, name: name, isDir: true, children: scan(url, depth: depth + 1)))
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

    @Published var selectedRail: RailSection = .home
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
    @Published var focusObjective = false
    @Published var showPalette = false

    let workspace: URL
    let cli: URL
    private let runner = CLIRunner()
    private let engine = EngineProcess()
    private var runTask: Task<Void, Never>?

    init() {
        var ws = FileManager.default.currentDirectoryPath
        var cliPath = ws + "/target/debug/opensks"
        if let res = Bundle.main.resourceURL {
            if let txt = try? String(contentsOf: res.appendingPathComponent("workspace-path.txt"), encoding: .utf8) {
                let trimmed = txt.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty { ws = trimmed }
            }
            let candidate = res.appendingPathComponent("opensks-cli")
            if FileManager.default.fileExists(atPath: candidate.path) { cliPath = candidate.path }
        }
        let workspaceURL = URL(fileURLWithPath: ws, isDirectory: true)
        let cliURL = URL(fileURLWithPath: cliPath)
        self.workspace = workspaceURL
        self.cli = cliURL
        self.editorStore = EditorWorkspaceStore(
            service: LiveEditorFileService(cli: cliURL, workspace: workspaceURL)
        )
        self.fileRoots = FileScanner.scan(workspaceURL)
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
            let raw = await runner.capture(cli: cli, cwd: ws, args: ["app-data", ws.path])
            if raw.isEmpty {
                self.loadError = "opensks-cli app-data returned no output"
                return
            }
            do {
                self.data = try JSONDecoder.opensks.decode(AppData.self, from: raw)
                self.loadError = nil
            } catch {
                self.loadError = "could not decode app-data: \(error.localizedDescription)"
            }
        }
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
                self.executionStore.apply(event)
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
            self.selectedRail = .runs
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
            self.selectedRail = .runs
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
                self.executionStore.apply(event)
                self.append(RunLine(text: "[tail] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.isRunning = false
            self.selectedRail = .runs
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
            selectedRail = .graph
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
                self.executionStore.apply(event)
                self.append(RunLine(text: "[graph event] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.isRunning = false
            self.selectedRail = .runs
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
                self.executionStore.apply(event)
                self.append(RunLine(text: "[event] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.selectedRail = .runs
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
                self.executionStore.apply(event)
                self.append(RunLine(text: "[event] \(event.kind.rawValue) #\(event.sequence)", kind: .info))
            }
            self.lastExit = stream.exitCode
            self.selectedRail = .runs
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
