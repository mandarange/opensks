// FileService.swift — the boundary between the editor and the hardened workspace
// file service (PR-032).
//
// `EditorFileService` is the async-throwing protocol the store talks to. The
// LIVE implementation shells the bundled `opensks file open|save|stat` CLI; on a
// nonzero exit it decodes the `opensks.file-error.v1` envelope into a typed
// `EditorFileServiceError` (mapping `file_changed_on_disk` to `.conflict`, which
// the store turns into a non-destructive conflict banner). A MOCK implementation
// drives the tests without touching disk or spawning a process.

import Foundation

// MARK: - Typed errors

/// Errors surfaced by the file service. `.conflict` is special-cased by the
/// store so an out-of-band change never silently overwrites the user's edits.
enum EditorFileServiceError: Error, Equatable {
    /// `file_changed_on_disk` — the on-disk file moved past our baseline.
    case conflict(message: String)
    /// `file_secret_restricted`.
    case secretRestricted(message: String)
    /// `file_binary`.
    case binary(message: String)
    /// `file_too_large`.
    case tooLarge(message: String)
    /// `file_not_found`.
    case notFound(message: String)
    /// Any other typed service error (`workspace_path_escape`, etc.).
    case service(code: String, message: String)
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)

    var code: String {
        switch self {
        case .conflict: return "file_changed_on_disk"
        case .secretRestricted: return "file_secret_restricted"
        case .binary: return "file_binary"
        case .tooLarge: return "file_too_large"
        case .notFound: return "file_not_found"
        case .service(let code, _): return code
        case .transport: return "transport_error"
        }
    }

    var message: String {
        switch self {
        case .conflict(let m), .secretRestricted(let m), .binary(let m),
             .tooLarge(let m), .notFound(let m), .transport(let m):
            return m
        case .service(_, let m):
            return m
        }
    }

    /// Build the typed error from a decoded `opensks.file-error.v1` payload.
    static func fromPayload(_ payload: EditorErrorResponse.Payload) -> EditorFileServiceError {
        switch payload.code {
        case "file_changed_on_disk": return .conflict(message: payload.message)
        case "file_secret_restricted": return .secretRestricted(message: payload.message)
        case "file_binary": return .binary(message: payload.message)
        case "file_too_large": return .tooLarge(message: payload.message)
        case "file_not_found": return .notFound(message: payload.message)
        default: return .service(code: payload.code, message: payload.message)
        }
    }
}

// MARK: - Protocol

/// The async-throwing file-service boundary. All paths are workspace-relative.
protocol EditorFileService: Sendable {
    func open(path: String) async throws -> EditorOpenResponse
    func save(
        path: String,
        content: String,
        expectedHash: String,
        expectedMtime: UInt64?
    ) async throws -> EditorSaveResponse
    func stat(path: String) async throws -> EditorStatResponse

    // MARK: PR-033 — diff / incremental-index / working-change

    /// Compute the diff between the editor's CURRENT buffer (piped on STDIN) and
    /// the on-disk file. Shells `opensks file diff --stdin`.
    func diff(path: String, currentBuffer: String) async throws -> TextDiffResponse

    /// Incrementally re-index a SINGLE file after a save. Shells
    /// `opensks codegraph update --path` — never a workspace re-index.
    func codegraphUpdate(path: String) async throws -> CodegraphUpdateResponse

    /// Ask whether the working-tree file has diverged from the editor baseline
    /// (e.g. a branch switch). Shells `opensks git working-change`.
    func workingChange(path: String, baselineHash: String) async throws -> WorkingChangeResponse
}

// MARK: - Live (CLI-backed) implementation

/// Shells the bundled `opensks file …` CLI. All process work happens on a
/// detached cooperative task; decoding maps the shared JSON contract to domain
/// types and the error envelope to `EditorFileServiceError`.
struct LiveEditorFileService: EditorFileService {
    let cli: URL
    let workspace: URL

    func open(path: String) async throws -> EditorOpenResponse {
        let result = try await run(
            args: ["file", "open", "--workspace", workspace.path, "--path", path],
            stdin: nil
        )
        return try Self.decodeOrThrow(result, as: EditorOpenResponse.self)
    }

    func save(
        path: String,
        content: String,
        expectedHash: String,
        expectedMtime: UInt64?
    ) async throws -> EditorSaveResponse {
        var args = ["file", "save", "--workspace", workspace.path, "--path", path,
                    "--expected-hash", expectedHash]
        if let expectedMtime {
            args.append(contentsOf: ["--expected-mtime", String(expectedMtime)])
        }
        args.append("--stdin")
        let result = try await run(args: args, stdin: Data(content.utf8))
        return try Self.decodeOrThrow(result, as: EditorSaveResponse.self)
    }

    func stat(path: String) async throws -> EditorStatResponse {
        let result = try await run(
            args: ["file", "stat", "--workspace", workspace.path, "--path", path],
            stdin: nil
        )
        return try Self.decodeOrThrow(result, as: EditorStatResponse.self)
    }

    // MARK: PR-033 — diff / incremental-index / working-change

    func diff(path: String, currentBuffer: String) async throws -> TextDiffResponse {
        // SUBCOMMAND of the existing `file` verb. The buffer is piped on STDIN
        // and compared against the on-disk file.
        let result = try await run(
            args: ["file", "diff", "--workspace", workspace.path, "--path", path, "--stdin"],
            stdin: Data(currentBuffer.utf8)
        )
        return try Self.decodeOrThrow(result, as: TextDiffResponse.self)
    }

    func codegraphUpdate(path: String) async throws -> CodegraphUpdateResponse {
        // SUBCOMMAND of the existing `codegraph` verb — single-file incremental
        // re-index (CodeGraph::update_file), NEVER index_workspace.
        let result = try await run(
            args: ["codegraph", "update", "--workspace", workspace.path, "--path", path],
            stdin: nil
        )
        return try Self.decodeOrThrow(result, as: CodegraphUpdateResponse.self)
    }

    func workingChange(path: String, baselineHash: String) async throws -> WorkingChangeResponse {
        // SUBCOMMAND of the existing `git` verb — did the working-tree file move
        // away from the editor baseline (e.g. after a branch switch)?
        let result = try await run(
            args: ["git", "working-change", "--workspace", workspace.path,
                   "--path", path, "--baseline-hash", baselineHash],
            stdin: nil
        )
        return try Self.decodeOrThrow(result, as: WorkingChangeResponse.self)
    }

    // MARK: Process plumbing

    private struct ProcessResult {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
    }

    private func run(args: [String], stdin: Data?) async throws -> ProcessResult {
        let cli = self.cli
        let workspace = self.workspace
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = cli
                process.arguments = args
                process.currentDirectoryURL = workspace
                let outPipe = Pipe()
                let errPipe = Pipe()
                let inPipe = Pipe()
                process.standardOutput = outPipe
                process.standardError = errPipe
                process.standardInput = inPipe
                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: EditorFileServiceError.transport(
                        message: "could not launch opensks file: \(error.localizedDescription)"
                    ))
                    return
                }
                if let stdin {
                    inPipe.fileHandleForWriting.write(stdin)
                }
                inPipe.fileHandleForWriting.closeFile()
                let out = outPipe.fileHandleForReading.readDataToEndOfFile()
                let err = errPipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                continuation.resume(returning: ProcessResult(
                    exitCode: process.terminationStatus,
                    stdout: out,
                    stderr: err
                ))
            }
        }
    }

    /// Decode the success type on exit 0; on nonzero exit decode the file-error
    /// envelope into a typed error. Unparseable output becomes `.transport`.
    private static func decodeOrThrow<T: Decodable>(
        _ result: ProcessResult,
        as type: T.Type
    ) throws -> T {
        let decoder = JSONDecoder()
        if result.exitCode == 0 {
            if let value = try? decoder.decode(T.self, from: result.stdout) {
                return value
            }
            // Some guard failures may still print the error envelope on exit 0
            // defensively check before declaring a transport failure.
            if let envelope = try? decoder.decode(EditorErrorResponse.self, from: result.stdout) {
                throw EditorFileServiceError.fromPayload(envelope.error)
            }
            throw EditorFileServiceError.transport(
                message: "could not decode \(T.self) from opensks file output"
            )
        }
        // Nonzero exit: the contract guarantees a file-error envelope on stdout.
        if let envelope = try? decoder.decode(EditorErrorResponse.self, from: result.stdout) {
            throw EditorFileServiceError.fromPayload(envelope.error)
        }
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        throw EditorFileServiceError.transport(
            message: stderrText.isEmpty
                ? "opensks file exited \(result.exitCode)"
                : "opensks file exited \(result.exitCode): \(stderrText)"
        )
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory file service for tests. Records saves and lets a test script a
/// `file_changed_on_disk` conflict on the next save of a given path.
final class MockEditorFileService: EditorFileService, @unchecked Sendable {
    struct Entry {
        var content: String
        var hash: String
        var mtimeMs: UInt64
        var isSecretRestricted: Bool
        var isBinary: Bool
        var byteSize: Int
    }

    private let lock = NSLock()
    private var files: [String: Entry] = [:]
    private var conflictArmedPaths: Set<String> = []
    /// Paths whose next `stat` should report a divergent on-disk hash (an
    /// external/branch-switch change the watcher must surface as a conflict).
    private var externalHashByPath: [String: String] = [:]
    /// Canned diff responses keyed by path, set up by a test.
    private var diffByPath: [String: TextDiffResponse] = [:]

    /// The content most recently persisted by `save`, keyed by path.
    private(set) var savedContent: [String: String] = [:]
    /// The number of save calls observed, keyed by path.
    private(set) var saveCount: [String: Int] = [:]
    /// Ordered record of every `codegraphUpdate(path:)` call (one entry per
    /// call). A save must produce EXACTLY one single-file update for its path.
    private(set) var codegraphUpdateCalls: [String] = []
    /// Ordered record of every `diff` call's path.
    private(set) var diffCalls: [String] = []
    /// Ordered record of every `workingChange` call's path.
    private(set) var workingChangeCalls: [String] = []

    init() {}

    /// Seed a file the editor can open.
    func seed(
        path: String,
        content: String,
        mtimeMs: UInt64 = 1_000,
        isSecretRestricted: Bool = false,
        isBinary: Bool = false
    ) {
        lock.lock(); defer { lock.unlock() }
        files[path] = Entry(
            content: content,
            hash: EditorContentHash.compute(content),
            mtimeMs: mtimeMs,
            isSecretRestricted: isSecretRestricted,
            isBinary: isBinary,
            byteSize: content.utf8.count
        )
    }

    /// Arm a single `file_changed_on_disk` conflict for the next save of `path`.
    func armConflict(path: String) {
        lock.lock(); defer { lock.unlock() }
        conflictArmedPaths.insert(path)
    }

    /// Simulate an EXTERNAL edit (another process / a branch switch): the on-disk
    /// content + hash diverge from what the editor holds, WITHOUT going through
    /// the editor's own save. The next `stat` / `workingChange` reports the new
    /// hash so a watcher can flag the open doc as conflicted.
    func simulateExternalChange(path: String, newContent: String) {
        lock.lock(); defer { lock.unlock() }
        guard var entry = files[path] else { return }
        let newHash = EditorContentHash.compute(newContent)
        entry.content = newContent
        entry.hash = newHash
        entry.mtimeMs += 1
        entry.byteSize = newContent.utf8.count
        files[path] = entry
        externalHashByPath[path] = newHash
    }

    /// Seed a canned diff response a test wants `diff(path:currentBuffer:)` to
    /// return, regardless of buffer contents.
    func setDiff(path: String, response: TextDiffResponse) {
        lock.lock(); defer { lock.unlock() }
        diffByPath[path] = response
    }

    func currentContent(path: String) -> String? {
        lock.lock(); defer { lock.unlock() }
        return files[path]?.content
    }

    func open(path: String) async throws -> EditorOpenResponse {
        try openSync(path: path)
    }

    func save(
        path: String,
        content: String,
        expectedHash: String,
        expectedMtime: UInt64?
    ) async throws -> EditorSaveResponse {
        try saveSync(path: path, content: content, expectedHash: expectedHash)
    }

    func stat(path: String) async throws -> EditorStatResponse {
        try statSync(path: path)
    }

    func diff(path: String, currentBuffer: String) async throws -> TextDiffResponse {
        try diffSync(path: path, currentBuffer: currentBuffer)
    }

    func codegraphUpdate(path: String) async throws -> CodegraphUpdateResponse {
        try codegraphUpdateSync(path: path)
    }

    func workingChange(path: String, baselineHash: String) async throws -> WorkingChangeResponse {
        try workingChangeSync(path: path, baselineHash: baselineHash)
    }

    // MARK: Synchronous critical sections (lock fully scoped, no async suspension)

    private func openSync(path: String) throws -> EditorOpenResponse {
        lock.lock(); defer { lock.unlock() }
        guard let entry = files[path] else {
            throw EditorFileServiceError.notFound(message: "file_not_found: \(path)")
        }
        return EditorOpenResponse(
            schema: "opensks.text-document.v1",
            workspaceRelativePath: path,
            content: entry.content,
            contentHash: entry.hash,
            encoding: "utf-8",
            lineEnding: "lf",
            byteSize: entry.byteSize,
            isSecretRestricted: entry.isSecretRestricted,
            isBinary: entry.isBinary,
            onDiskModificationMs: entry.mtimeMs,
            permissionsMode: 420
        )
    }

    private func saveSync(
        path: String,
        content: String,
        expectedHash: String
    ) throws -> EditorSaveResponse {
        lock.lock(); defer { lock.unlock() }
        saveCount[path, default: 0] += 1

        if conflictArmedPaths.contains(path) {
            conflictArmedPaths.remove(path)
            // A conflict NEVER persists; the user's edits stay in the editor.
            throw EditorFileServiceError.conflict(message: "file_changed_on_disk: \(path)")
        }

        guard var entry = files[path] else {
            throw EditorFileServiceError.notFound(message: "file_not_found: \(path)")
        }
        guard entry.hash == expectedHash else {
            throw EditorFileServiceError.conflict(message: "file_changed_on_disk: \(path)")
        }
        let newHash = EditorContentHash.compute(content)
        let newMtime = entry.mtimeMs + 1
        entry.content = content
        entry.hash = newHash
        entry.mtimeMs = newMtime
        entry.byteSize = content.utf8.count
        files[path] = entry
        savedContent[path] = content
        return EditorSaveResponse(
            schema: "opensks.save-result.v1",
            newHash: newHash,
            newMtimeMs: newMtime
        )
    }

    private func statSync(path: String) throws -> EditorStatResponse {
        lock.lock(); defer { lock.unlock() }
        guard let entry = files[path] else {
            throw EditorFileServiceError.notFound(message: "file_not_found: \(path)")
        }
        return EditorStatResponse(
            schema: "opensks.workspace-entry.v1",
            workspaceRelativePath: path,
            byteSize: entry.byteSize,
            modificationMs: entry.mtimeMs,
            contentHash: entry.hash,
            isSecretRestricted: entry.isSecretRestricted
        )
    }

    private func diffSync(path: String, currentBuffer: String) throws -> TextDiffResponse {
        lock.lock(); defer { lock.unlock() }
        diffCalls.append(path)
        if let canned = diffByPath[path] {
            return canned
        }
        // Default: a trivial whole-file line diff between on-disk and buffer.
        let onDisk = files[path]?.content ?? ""
        let oldLines = onDisk.isEmpty ? [] : onDisk.components(separatedBy: "\n")
        let newLines = currentBuffer.isEmpty ? [] : currentBuffer.components(separatedBy: "\n")
        let changed = onDisk != currentBuffer
        var hunkLines: [String] = []
        if changed {
            hunkLines.append(contentsOf: oldLines.map { "-\($0)" })
            hunkLines.append(contentsOf: newLines.map { "+\($0)" })
        }
        let hunks: [TextDiffHunk] = changed
            ? [TextDiffHunk(kind: .changed, oldStart: 1, oldLines: oldLines.count,
                            newStart: 1, newLines: newLines.count, lines: hunkLines)]
            : []
        return TextDiffResponse(
            schema: "opensks.text-diff.v1",
            path: path,
            changed: changed,
            hunks: hunks,
            addedLines: changed ? newLines.count : 0,
            removedLines: changed ? oldLines.count : 0
        )
    }

    private func codegraphUpdateSync(path: String) throws -> CodegraphUpdateResponse {
        lock.lock(); defer { lock.unlock() }
        codegraphUpdateCalls.append(path)
        return CodegraphUpdateResponse(
            schema: "opensks.codegraph-update.v1",
            path: path,
            symbolCount: 0,
            // The invariant under test: a save NEVER triggers a full workspace scan.
            fullScan: false
        )
    }

    private func workingChangeSync(path: String, baselineHash: String) throws -> WorkingChangeResponse {
        lock.lock(); defer { lock.unlock() }
        workingChangeCalls.append(path)
        let currentHash = files[path]?.hash
        let changed = (currentHash != nil) && currentHash != baselineHash
        return WorkingChangeResponse(
            schema: "opensks.working-change.v1",
            path: path,
            inRepo: true,
            changed: changed,
            currentHash: currentHash,
            headHash: externalHashByPath[path] ?? currentHash
        )
    }
}
