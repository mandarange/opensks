// GitService.swift — the READ-ONLY boundary between the Git studio and the
// bundled `opensks git status|branches|diff` subcommands (PR-034).
//
// `GitService` is the async-throwing protocol the store talks to. Its surface is
// READ-ONLY BY CONSTRUCTION: the ONLY methods are status / branches / diff.
// There is deliberately NO stage/commit/switch/push — a mutation method cannot
// be added without changing this protocol, which the tests assert against.
//
// The LIVE implementation shells the bundled CLI exactly like LiveEditorFileService
// (off-main detached task, decode the shared snake_case JSON). A MOCK drives the
// tests without touching disk or spawning a process and COUNTS calls so the
// debounce behaviour is observable.

import Foundation

// MARK: - Errors

/// Errors surfaced by the read-only git service.
enum GitServiceError: Error, Equatable {
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)
    /// A non-zero exit with a decodable message.
    case service(message: String)
}

// MARK: - Protocol (READ-ONLY surface)

/// The read-only git boundary. Exactly three reads — status, branches, diff.
/// No mutation exists here; this is the compile-time guarantee the PR rests on.
protocol GitService: Sendable {
    /// `opensks git status --workspace <p>`.
    func status() async throws -> GitStatus
    /// `opensks git branches --workspace <p>`.
    func branches() async throws -> GitBranches
    /// `opensks git diff --workspace <p> [--path <rel>] [--staged]`.
    func diff(path: String?, staged: Bool) async throws -> GitDiff
}

extension GitService {
    /// Convenience: the full working-tree diff (unstaged, all paths).
    func diff() async throws -> GitDiff {
        try await diff(path: nil, staged: false)
    }
}

// MARK: - Live (CLI-backed) implementation

/// Shells the bundled `opensks git …` READ-ONLY subcommands. Process work runs
/// on a detached cooperative task; decoding maps the shared snake_case contract.
struct LiveGitService: GitService {
    let cli: URL
    let workspace: URL

    func status() async throws -> GitStatus {
        let result = try await run(args: ["git", "status", "--workspace", workspace.path])
        return try Self.decodeOrThrow(result, as: GitStatus.self)
    }

    func branches() async throws -> GitBranches {
        let result = try await run(args: ["git", "branches", "--workspace", workspace.path])
        return try Self.decodeOrThrow(result, as: GitBranches.self)
    }

    func diff(path: String?, staged: Bool) async throws -> GitDiff {
        var args = ["git", "diff", "--workspace", workspace.path]
        if let path { args.append(contentsOf: ["--path", path]) }
        if staged { args.append("--staged") }
        let result = try await run(args: args)
        return try Self.decodeOrThrow(result, as: GitDiff.self)
    }

    // MARK: Process plumbing (mirrors LiveEditorFileService)

    private struct ProcessResult {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
    }

    private func run(args: [String]) async throws -> ProcessResult {
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
                process.standardOutput = outPipe
                process.standardError = errPipe
                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: GitServiceError.transport(
                        message: "could not launch opensks git: \(error.localizedDescription)"
                    ))
                    return
                }
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

    private static func decodeOrThrow<T: Decodable>(
        _ result: ProcessResult,
        as type: T.Type
    ) throws -> T {
        let decoder = JSONDecoder()
        if result.exitCode == 0, let value = try? decoder.decode(T.self, from: result.stdout) {
            return value
        }
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if result.exitCode == 0 {
            throw GitServiceError.transport(
                message: "could not decode \(T.self) from opensks git output"
            )
        }
        throw GitServiceError.service(
            message: stderrText.isEmpty
                ? "opensks git exited \(result.exitCode)"
                : "opensks git exited \(result.exitCode): \(stderrText)"
        )
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory read-only git service for tests. Returns canned status / branches
/// / diff and COUNTS each call so the store's debounce can be asserted (rapid
/// triggers must coalesce into a bounded number of service calls).
final class MockGitService: GitService, @unchecked Sendable {
    private let lock = NSLock()
    private var cannedStatus: GitStatus
    private var cannedBranches: GitBranches
    private var cannedDiff: GitDiff
    private var diffByPath: [String: GitDiff] = [:]
    /// An artificial delay applied to `status()` so a test can fire several
    /// refreshes while the first is still in flight (to prove coalescing).
    private var statusDelayNanos: UInt64 = 0

    private(set) var statusCallCount = 0
    private(set) var branchesCallCount = 0
    private(set) var diffCallCount = 0
    private(set) var diffCalls: [(path: String?, staged: Bool)] = []

    init(
        status: GitStatus = .empty,
        branches: GitBranches = .empty,
        diff: GitDiff = .empty
    ) {
        self.cannedStatus = status
        self.cannedBranches = branches
        self.cannedDiff = diff
    }

    // MARK: Test setup

    func setStatus(_ status: GitStatus) {
        lock.lock(); defer { lock.unlock() }
        cannedStatus = status
    }

    func setBranches(_ branches: GitBranches) {
        lock.lock(); defer { lock.unlock() }
        cannedBranches = branches
    }

    func setDiff(_ diff: GitDiff, forPath path: String? = nil) {
        lock.lock(); defer { lock.unlock() }
        if let path { diffByPath[path] = diff } else { cannedDiff = diff }
    }

    /// Make `status()` take ~`millis` ms so concurrent refreshes overlap.
    func setStatusDelay(millis: UInt64) {
        lock.lock(); defer { lock.unlock() }
        statusDelayNanos = millis * 1_000_000
    }

    // MARK: GitService (read-only)
    //
    // Each method takes its lock in a fully-scoped SYNCHRONOUS critical section
    // (no lock held across an `await`), matching MockEditorFileService so the
    // mock is correct under Swift 6 strict concurrency.

    func status() async throws -> GitStatus {
        let delay = recordStatusCall()
        if delay > 0 { try? await Task.sleep(nanoseconds: delay) }
        return readStatus()
    }

    func branches() async throws -> GitBranches {
        readBranchesRecording()
    }

    func diff(path: String?, staged: Bool) async throws -> GitDiff {
        readDiffRecording(path: path, staged: staged)
    }

    // MARK: Synchronous critical sections

    private func recordStatusCall() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        statusCallCount += 1
        return statusDelayNanos
    }

    private func readStatus() -> GitStatus {
        lock.lock(); defer { lock.unlock() }
        return cannedStatus
    }

    private func readBranchesRecording() -> GitBranches {
        lock.lock(); defer { lock.unlock() }
        branchesCallCount += 1
        return cannedBranches
    }

    private func readDiffRecording(path: String?, staged: Bool) -> GitDiff {
        lock.lock(); defer { lock.unlock() }
        diffCallCount += 1
        diffCalls.append((path: path, staged: staged))
        if let path, let canned = diffByPath[path] { return canned }
        return cannedDiff
    }
}
