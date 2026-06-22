// GitService.swift — the boundary between the Git studio and the bundled
// `opensks git …` subcommands (PR-034 read-only + PR-035 LOCAL mutations).
//
// `GitService` is the async-throwing protocol the store talks to. The reads —
// status / branches / diff — are unchanged from PR-034. PR-035 adds the LOCAL
// mutation surface: stage / unstage / switch-preflight / create-branch / switch
// / commit-preview / commit. There is deliberately NO push method anywhere: the
// surface is read-only-plus-LOCAL, and the tests assert no push exists.
//
// The LIVE implementation shells the bundled CLI exactly like LiveEditorFileService
// (off-main detached task, decode the shared snake_case JSON). A non-zero exit
// carrying the `opensks.git-error.v1` envelope is mapped to a TYPED error
// (`switchBlocked` / `indexChanged` / `secretRejected`) so the store can react
// precisely. A MOCK drives the tests without touching disk or spawning a process
// and COUNTS calls so behaviour is observable.

import Foundation

// MARK: - Errors

/// Errors surfaced by the git service. The first two are the PR-034 read errors;
/// the rest are the typed PR-035 mutation errors decoded from `opensks.git-error.v1`.
enum GitServiceError: Error, Equatable {
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)
    /// A non-zero exit with a decodable message.
    case service(message: String)
    /// `switch_blocked` — a dirty/conflicted worktree refused the switch without
    /// `--force`. Carries the blockers so the UI can explain (no silent --force).
    case switchBlocked(blockers: [GitSwitchBlocker])
    /// `index_changed` — the live index no longer matches the reviewed preview's
    /// `index_hash`; the stale preview must be refreshed before committing.
    case indexChanged
    /// A staged path was secret / data-plane and the commit was refused. Carries
    /// the offending paths so the UI can name what can never be committed.
    case secretRejected(rejected: [GitStageRejection])
}

// MARK: - Protocol (READ-ONLY + LOCAL-MUTATION surface)

/// The git boundary. Reads — status / branches / diff (PR-034). Local mutations —
/// stage / unstage / switch-preflight / create-branch / switch / commit-preview /
/// commit (PR-035). There is NO push: this surface is read-only-plus-LOCAL.
protocol GitService: Sendable {
    // Reads (PR-034).

    /// `opensks git status --workspace <p>`.
    func status() async throws -> GitStatus
    /// `opensks git branches --workspace <p>`.
    func branches() async throws -> GitBranches
    /// `opensks git diff --workspace <p> [--path <rel>] [--staged]`.
    func diff(path: String?, staged: Bool) async throws -> GitDiff

    // Local mutations (PR-035). NONE of these is a push.

    /// `opensks git stage --workspace <p> --path <rel> …`. Secret / data-plane
    /// paths are NEVER staged: they come back only in `rejected`.
    func stage(paths: [String]) async throws -> GitStageResult
    /// `opensks git unstage --workspace <p> --path <rel> …`.
    func unstage(paths: [String]) async throws -> GitUnstageResult
    /// `opensks git switch-preflight --workspace <p> --target <b>` — read-only.
    func switchPreflight(target: String) async throws -> GitSwitchPreflight
    /// `opensks git create-branch --workspace <p> --name <b> [--from <ref>]`.
    func createBranch(name: String, from: String?) async throws -> GitCreateBranchResult
    /// `opensks git switch --workspace <p> --target <b> [--force]`. Without
    /// `--force`, a dirty worktree throws `.switchBlocked` (never a silent force).
    func switchBranch(target: String, force: Bool) async throws -> GitSwitchResult
    /// `opensks git commit-preview --workspace <p>` — the staged tree + its hash.
    func commitPreview() async throws -> GitCommitPreview
    /// `opensks git commit --workspace <p> --message <m> --expected-index-hash <h>`.
    /// A stale `expectedIndexHash` throws `.indexChanged`; a secret/data-plane
    /// staged path throws `.secretRejected`.
    func commit(message: String, expectedIndexHash: String) async throws -> GitCommitResult
}

extension GitService {
    /// Convenience: the full working-tree diff (unstaged, all paths).
    func diff() async throws -> GitDiff {
        try await diff(path: nil, staged: false)
    }

    /// Stage a single path.
    func stage(path: String) async throws -> GitStageResult {
        try await stage(paths: [path])
    }

    /// Unstage a single path.
    func unstage(path: String) async throws -> GitUnstageResult {
        try await unstage(paths: [path])
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

    // MARK: Local mutations (PR-035) — NO push anywhere here

    func stage(paths: [String]) async throws -> GitStageResult {
        var args = ["git", "stage", "--workspace", workspace.path]
        for path in paths { args.append(contentsOf: ["--path", path]) }
        let result = try await run(args: args)
        return try Self.decodeMutation(result, as: GitStageResult.self)
    }

    func unstage(paths: [String]) async throws -> GitUnstageResult {
        var args = ["git", "unstage", "--workspace", workspace.path]
        for path in paths { args.append(contentsOf: ["--path", path]) }
        let result = try await run(args: args)
        return try Self.decodeMutation(result, as: GitUnstageResult.self)
    }

    func switchPreflight(target: String) async throws -> GitSwitchPreflight {
        let result = try await run(args: ["git", "switch-preflight", "--workspace", workspace.path, "--target", target])
        return try Self.decodeMutation(result, as: GitSwitchPreflight.self)
    }

    func createBranch(name: String, from: String?) async throws -> GitCreateBranchResult {
        var args = ["git", "create-branch", "--workspace", workspace.path, "--name", name]
        if let from { args.append(contentsOf: ["--from", from]) }
        let result = try await run(args: args)
        return try Self.decodeMutation(result, as: GitCreateBranchResult.self)
    }

    func switchBranch(target: String, force: Bool) async throws -> GitSwitchResult {
        var args = ["git", "switch", "--workspace", workspace.path, "--target", target]
        if force { args.append("--force") }
        let result = try await run(args: args)
        return try Self.decodeMutation(result, as: GitSwitchResult.self)
    }

    func commitPreview() async throws -> GitCommitPreview {
        let result = try await run(args: ["git", "commit-preview", "--workspace", workspace.path])
        return try Self.decodeMutation(result, as: GitCommitPreview.self)
    }

    func commit(message: String, expectedIndexHash: String) async throws -> GitCommitResult {
        let result = try await run(args: [
            "git", "commit", "--workspace", workspace.path,
            "--message", message, "--expected-index-hash", expectedIndexHash
        ])
        return try Self.decodeMutation(result, as: GitCommitResult.self)
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

    /// Decode a mutation result. On a non-zero exit the CLI emits the
    /// `opensks.git-error.v1` envelope (on stdout or stderr); map its `code` to a
    /// TYPED `GitServiceError` so the store reacts precisely (a blocked switch is
    /// not a generic failure; a stale preview must trigger a refresh; a secret
    /// staged path is refused). Unknown codes fall back to `.service`.
    private static func decodeMutation<T: Decodable>(
        _ result: ProcessResult,
        as type: T.Type
    ) throws -> T {
        let decoder = JSONDecoder()
        if result.exitCode == 0, let value = try? decoder.decode(T.self, from: result.stdout) {
            return value
        }
        // Try the structured error envelope on either stream.
        if let envelope = decodeErrorEnvelope(result.stdout, decoder)
            ?? decodeErrorEnvelope(result.stderr, decoder) {
            throw Self.mapError(envelope)
        }
        if result.exitCode == 0 {
            throw GitServiceError.transport(
                message: "could not decode \(T.self) from opensks git output"
            )
        }
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        throw GitServiceError.service(
            message: stderrText.isEmpty
                ? "opensks git exited \(result.exitCode)"
                : "opensks git exited \(result.exitCode): \(stderrText)"
        )
    }

    private static func decodeErrorEnvelope(_ data: Data, _ decoder: JSONDecoder) -> GitErrorEnvelope? {
        guard !data.isEmpty else { return nil }
        return try? decoder.decode(GitErrorEnvelope.self, from: data)
    }

    /// Map a decoded `opensks.git-error.v1` to a typed error.
    static func mapError(_ envelope: GitErrorEnvelope) -> GitServiceError {
        switch envelope.error.code {
        case "switch_blocked":
            return .switchBlocked(blockers: envelope.error.blockers)
        case "index_changed":
            return .indexChanged
        case "secret_restricted", "data_plane", "secret_rejected":
            return .secretRejected(rejected: envelope.error.rejected)
        default:
            return .service(message: envelope.error.message ?? "opensks git error: \(envelope.error.code)")
        }
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory git service for tests. Returns canned reads + scriptable LOCAL
/// mutations and COUNTS each call so the store's debounce + mutation flow can be
/// asserted (rapid triggers coalesce; a switch is/ isn't blocked; a commit goes
/// stale on `index_changed`). It NEVER touches disk or spawns a process, and it
/// has NO push entry point — the mutation surface here is local-only.
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

    // MARK: Mutation scripting + call records (PR-035)

    /// Preflight verdicts keyed by target branch; default is clean (`canSwitch`).
    private var preflightByTarget: [String: GitSwitchPreflight] = [:]
    /// Paths the mock treats as secret / data-plane: a stage attempt refuses them
    /// (they appear in `rejected`), and a commit that includes one throws
    /// `.secretRejected`.
    private var restrictedPaths: [String: GitStageRejectReason] = [:]
    /// The preview the mock returns from `commitPreview()`.
    private var cannedPreview: GitCommitPreview = .empty
    /// When set, the NEXT `commit(...)` throws `.indexChanged` once (a stale
    /// preview), then clears so a refreshed-and-retried commit succeeds.
    private var failNextCommitIndexChanged = false
    /// The result `commit(...)` returns on success.
    private var cannedCommitResult: GitCommitResult?
    /// The set of currently-staged paths (driven by stage/unstage on the mock).
    private(set) var stagedPaths: [String] = []

    private(set) var stageCalls: [[String]] = []
    private(set) var unstageCalls: [[String]] = []
    private(set) var preflightCalls: [String] = []
    private(set) var createBranchCalls: [(name: String, from: String?)] = []
    private(set) var switchCalls: [(target: String, force: Bool)] = []
    private(set) var commitPreviewCallCount = 0
    private(set) var commitCalls: [(message: String, expectedIndexHash: String)] = []

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

    // MARK: Mutation scripting (PR-035)

    /// Script the preflight verdict for a target branch.
    func setPreflight(_ preflight: GitSwitchPreflight, forTarget target: String) {
        lock.lock(); defer { lock.unlock() }
        preflightByTarget[target] = preflight
    }

    /// Mark a path secret / data-plane: stage refuses it (it appears in
    /// `rejected`); a commit including it throws `.secretRejected`.
    func setRestricted(_ path: String, reason: GitStageRejectReason = .secretRestricted) {
        lock.lock(); defer { lock.unlock() }
        restrictedPaths[path] = reason
    }

    /// Script the commit preview returned by `commitPreview()`.
    func setCommitPreview(_ preview: GitCommitPreview) {
        lock.lock(); defer { lock.unlock() }
        cannedPreview = preview
        stagedPaths = preview.stagedPaths
    }

    /// Arm the NEXT `commit(...)` to throw `.indexChanged` once (a stale preview).
    func armIndexChangedOnNextCommit() {
        lock.lock(); defer { lock.unlock() }
        failNextCommitIndexChanged = true
    }

    /// Script the commit result returned on a successful commit.
    func setCommitResult(_ result: GitCommitResult) {
        lock.lock(); defer { lock.unlock() }
        cannedCommitResult = result
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

    // MARK: GitService (local mutations) — NO push exists on this mock
    //
    // Each funnels its mutation through a fully-scoped SYNCHRONOUS critical
    // section (no lock held across an `await`), matching the read methods so the
    // mock is correct under Swift 6 strict concurrency. A scripted failure is
    // surfaced as a thrown `GitServiceError` from inside the critical section.

    func stage(paths: [String]) async throws -> GitStageResult {
        stageLocked(paths)
    }

    func unstage(paths: [String]) async throws -> GitUnstageResult {
        unstageLocked(paths)
    }

    func switchPreflight(target: String) async throws -> GitSwitchPreflight {
        switchPreflightLocked(target)
    }

    func createBranch(name: String, from: String?) async throws -> GitCreateBranchResult {
        createBranchLocked(name: name, from: from)
    }

    func switchBranch(target: String, force: Bool) async throws -> GitSwitchResult {
        try switchBranchLocked(target: target, force: force)
    }

    func commitPreview() async throws -> GitCommitPreview {
        commitPreviewLocked()
    }

    func commit(message: String, expectedIndexHash: String) async throws -> GitCommitResult {
        try commitLocked(message: message, expectedIndexHash: expectedIndexHash)
    }

    // MARK: Synchronous critical sections

    private func stageLocked(_ paths: [String]) -> GitStageResult {
        lock.lock(); defer { lock.unlock() }
        stageCalls.append(paths)
        var staged: [String] = []
        var rejected: [GitStageRejection] = []
        for path in paths {
            if let reason = restrictedPaths[path] {
                rejected.append(GitStageRejection(path: path, reason: reason))
            } else {
                staged.append(path)
                if !stagedPaths.contains(path) { stagedPaths.append(path) }
            }
        }
        return GitStageResult(staged: staged, rejected: rejected)
    }

    private func unstageLocked(_ paths: [String]) -> GitUnstageResult {
        lock.lock(); defer { lock.unlock() }
        unstageCalls.append(paths)
        stagedPaths.removeAll { paths.contains($0) }
        return GitUnstageResult(unstaged: paths)
    }

    private func switchPreflightLocked(_ target: String) -> GitSwitchPreflight {
        lock.lock(); defer { lock.unlock() }
        preflightCalls.append(target)
        return preflightByTarget[target] ?? .clean
    }

    private func createBranchLocked(name: String, from: String?) -> GitCreateBranchResult {
        lock.lock(); defer { lock.unlock() }
        createBranchCalls.append((name: name, from: from))
        return GitCreateBranchResult(created: true, branch: name, head: "mockhead")
    }

    private func switchBranchLocked(target: String, force: Bool) throws -> GitSwitchResult {
        lock.lock(); defer { lock.unlock() }
        switchCalls.append((target: target, force: force))
        // Honour a scripted blocker unless forced (matches the CLI: a blocked
        // switch without --force throws; with --force it goes through).
        if !force, let preflight = preflightByTarget[target], !preflight.canSwitch {
            throw GitServiceError.switchBlocked(blockers: preflight.blockers)
        }
        return GitSwitchResult(switched: true, branch: target)
    }

    private func commitPreviewLocked() -> GitCommitPreview {
        lock.lock(); defer { lock.unlock() }
        commitPreviewCallCount += 1
        return cannedPreview
    }

    private func commitLocked(message: String, expectedIndexHash: String) throws -> GitCommitResult {
        lock.lock(); defer { lock.unlock() }
        commitCalls.append((message: message, expectedIndexHash: expectedIndexHash))
        // A stale preview: the reviewed hash no longer matches the live index.
        if failNextCommitIndexChanged {
            failNextCommitIndexChanged = false
            throw GitServiceError.indexChanged
        }
        if expectedIndexHash != cannedPreview.indexHash {
            throw GitServiceError.indexChanged
        }
        // Refuse if any staged path is secret / data-plane — never commit one.
        let rejected = cannedPreview.stagedPaths.compactMap { path -> GitStageRejection? in
            guard let reason = restrictedPaths[path] else { return nil }
            return GitStageRejection(path: path, reason: reason)
        }
        if !rejected.isEmpty {
            throw GitServiceError.secretRejected(rejected: rejected)
        }
        if let scripted = cannedCommitResult { return scripted }
        return GitCommitResult(
            committed: true,
            commit: "deadbeefcafef00d",
            paths: cannedPreview.stagedPaths
        )
    }

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
