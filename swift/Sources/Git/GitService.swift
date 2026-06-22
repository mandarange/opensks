// GitService.swift — the boundary between the Git studio and the bundled
// `opensks git …` subcommands (PR-034 read-only + PR-035 LOCAL mutations +
// PR-036 the EXPLICITLY-APPROVED push flow).
//
// `GitService` is the async-throwing protocol the store talks to. The reads —
// status / branches / diff — are unchanged from PR-034. PR-035 adds the LOCAL
// mutation surface: stage / unstage / switch-preflight / create-branch / switch
// / commit-preview / commit. PR-036 adds the PUSH flow as four explicit steps —
// push-enqueue → push-approve → push-execute (+ push-status) — each a SUBCOMMAND
// of the existing `git` verb. There is NO silent push: a push always requires the
// operator to approve the EXACT effect, and the executor may run the real
// `git push` ONLY to the workspace's configured remote (in tests/smoke a LOCAL
// bare repo, never a network/external remote).
//
// The LIVE implementation shells the bundled CLI exactly like LiveEditorFileService
// (off-main detached task, decode the shared snake_case JSON). A non-zero exit
// carrying the `opensks.git-error.v1` envelope is mapped to a TYPED error
// (`switchBlocked` / `indexChanged` / `secretRejected`; or, for the push steps,
// `digestMismatch` / `noMatchingApproval` / `protectedBranch` / `pushFailed`) so
// the store can react precisely. A MOCK drives the tests without touching disk or
// spawning a process and COUNTS calls so behaviour — including the strict
// enqueue → approve → execute ORDERING — is observable.

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

// MARK: - Protocol (READ-ONLY + LOCAL-MUTATION + APPROVED-PUSH surface)

/// The git boundary. Reads — status / branches / diff (PR-034). Local mutations —
/// stage / unstage / switch-preflight / create-branch / switch / commit-preview /
/// commit (PR-035). The PUSH flow (PR-036) — push-enqueue / push-approve /
/// push-execute / push-status — is the ONLY remote-touching surface, and a push
/// NEVER happens without an explicit approval of the exact effect.
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

    // The push flow (PR-036) — NO silent push: every push is enqueued, approved
    // against the exact effect, then executed. Only ever the workspace's
    // configured remote.

    /// `opensks git push-enqueue --workspace <p> --remote <name> --ref <branch>` —
    /// persist a durable push INTENT (the exact effect + a stable `effect_digest`)
    /// in SQLite. Touches no remote; it only records what a push WOULD do.
    func pushEnqueue(remote: String, ref: String) async throws -> GitPushIntent
    /// `opensks git push-approve --workspace <p> --intent <id> --effect-digest <d>
    /// [--ack-protected]` — record an APPROVAL only if `effectDigest` still matches
    /// the intent's current digest. A mismatch throws `.digestMismatch` and records
    /// NO usable approval; a protected ref without `ackProtected` throws
    /// `.protectedBranch`.
    func pushApprove(intentId: String, effectDigest: String, ackProtected: Bool) async throws -> GitPushApproval
    /// `opensks git push-execute --workspace <p> --intent <id>` — run the real
    /// `git push` ONLY with a matching, still-valid approval. Throws
    /// `.noMatchingApproval` / `.digestMismatch` / `.protectedBranch` on a refusal,
    /// or `.pushFailed` on a remote failure (the local commit + intent are
    /// preserved for retry). Idempotent: a repeat execute reports `alreadyDone`.
    func pushExecute(intentId: String) async throws -> GitPushReceipt
    /// `opensks git push-status --workspace <p>` — the push outbox (pending /
    /// approved / completed), recovered from SQLite so it survives relaunch.
    func pushStatus() async throws -> GitPushStatus
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

    // MARK: Push flow (PR-036) — enqueue → approve → execute → status
    //
    // Each step maps its `opensks.git-error.v1` envelope to a TYPED `GitPushError`
    // (digest_mismatch / no_matching_approval / protected_branch / push_failed) so
    // the store reacts precisely: a mismatch re-reviews the effect, a missing ack
    // re-prompts, and a failed push leaves the commit + intent for retry.

    func pushEnqueue(remote: String, ref: String) async throws -> GitPushIntent {
        let result = try await run(args: [
            "git", "push-enqueue", "--workspace", workspace.path,
            "--remote", remote, "--ref", ref
        ])
        return try Self.decodePush(result, as: GitPushIntent.self)
    }

    func pushApprove(intentId: String, effectDigest: String, ackProtected: Bool) async throws -> GitPushApproval {
        var args = [
            "git", "push-approve", "--workspace", workspace.path,
            "--intent", intentId, "--effect-digest", effectDigest
        ]
        if ackProtected { args.append("--ack-protected") }
        let result = try await run(args: args)
        return try Self.decodePush(result, as: GitPushApproval.self)
    }

    func pushExecute(intentId: String) async throws -> GitPushReceipt {
        let result = try await run(args: [
            "git", "push-execute", "--workspace", workspace.path, "--intent", intentId
        ])
        return try Self.decodePush(result, as: GitPushReceipt.self)
    }

    func pushStatus() async throws -> GitPushStatus {
        let result = try await run(args: ["git", "push-status", "--workspace", workspace.path])
        return try Self.decodePush(result, as: GitPushStatus.self)
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

    /// Decode a PUSH step result. On a non-zero exit the CLI emits the
    /// `opensks.git-error.v1` envelope; map its push-specific `code` to a TYPED
    /// `GitPushError` so the store reacts precisely (a digest mismatch re-reviews
    /// the effect; a missing approval re-prompts; a protected ref without ack
    /// re-prompts with the warning; a failed push leaves the commit + intent for
    /// retry). A non-push code falls back to the generic `GitServiceError`.
    private static func decodePush<T: Decodable>(
        _ result: ProcessResult,
        as type: T.Type
    ) throws -> T {
        let decoder = JSONDecoder()
        if result.exitCode == 0, let value = try? decoder.decode(T.self, from: result.stdout) {
            return value
        }
        if let envelope = decodeErrorEnvelope(result.stdout, decoder)
            ?? decodeErrorEnvelope(result.stderr, decoder) {
            if let pushError = Self.mapPushError(envelope) {
                throw pushError
            }
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

    /// Map a decoded `opensks.git-error.v1` to a typed PUSH error, or nil when the
    /// code is not a push code (the caller then falls back to `mapError`).
    static func mapPushError(_ envelope: GitErrorEnvelope) -> GitPushError? {
        switch envelope.error.code {
        case "digest_mismatch":
            return .digestMismatch
        case "no_matching_approval":
            return .noMatchingApproval
        case "protected_branch":
            return .protectedBranch
        case "push_failed":
            return .pushFailed(message: envelope.error.message)
        default:
            return nil
        }
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory git service for tests. Returns canned reads + scriptable LOCAL
/// mutations + a scriptable PUSH flow, and COUNTS each call so the store's
/// debounce + mutation + push flow can be asserted (rapid triggers coalesce; a
/// switch is/ isn't blocked; a commit goes stale on `index_changed`; a push stays
/// pending until approved and is NEVER executed before approval; a digest
/// mismatch / protected branch / failed push behave per the contract). It NEVER
/// touches disk, spawns a process, or contacts a remote — `push-execute` records
/// a synthetic receipt — so tests are hermetic.
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

    // MARK: Push scripting + call records (PR-036)

    /// The intent the next `pushEnqueue(...)` returns. When nil, the mock
    /// synthesizes a deterministic intent from the remote + ref.
    private var cannedPushIntent: GitPushIntent?
    /// When true, the NEXT `pushApprove(...)` throws `.digestMismatch` regardless
    /// of the supplied digest (the live index/ref moved out from under the
    /// reviewed effect). Cleared after firing once.
    private var failNextApproveDigestMismatch = false
    /// When true, the NEXT `pushExecute(...)` throws `.pushFailed` (e.g. an
    /// unreachable remote). The local commit + the pending intent are preserved.
    /// Cleared after firing once.
    private var failNextExecutePushFailed = false
    /// The message carried by a scripted `.pushFailed`.
    private var pushFailedMessage: String? = "remote unreachable"
    /// Intent ids that have a recorded matching approval (set by `pushApprove`).
    private var approvedIntentIds: Set<String> = []
    /// The digest recorded at approval time per intent (execute refuses if the
    /// intent's CURRENT digest differs — i.e. it moved after approval).
    private var approvedDigestByIntent: [String: String] = [:]
    /// The CURRENT digest per enqueued intent (so a test can move it after
    /// approval to drive an execute-time `.digestMismatch`).
    private var currentDigestByIntent: [String: String] = [:]
    /// Intent ids that have already completed a push (drives idempotent
    /// `alreadyDone:true` on a repeat execute).
    private var completedIntentIds: Set<String> = []
    /// The remote oid a completed push landed at, per intent.
    private var completedRemoteOidByIntent: [String: String] = [:]
    /// The scriptable push-status snapshot (what `pushStatus()` returns), e.g. to
    /// simulate state recovered from SQLite after relaunch.
    private var cannedPushStatus: GitPushStatus?

    /// The strict, GLOBALLY-ORDERED log of push steps the mock saw. A test asserts
    /// no `.execute` precedes its `.approve` for the same intent — proving the push
    /// is never executed before it is approved.
    enum PushStep: Equatable {
        case enqueue(remote: String, ref: String)
        case approve(intentId: String, digest: String, ackProtected: Bool)
        case execute(intentId: String)
        case status
    }
    private(set) var pushSteps: [PushStep] = []

    private(set) var pushEnqueueCalls: [(remote: String, ref: String)] = []
    private(set) var pushApproveCalls: [(intentId: String, digest: String, ackProtected: Bool)] = []
    private(set) var pushExecuteCalls: [String] = []
    private(set) var pushStatusCallCount = 0

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

    // MARK: Push scripting (PR-036)

    /// Script the intent the next `pushEnqueue(...)` returns (e.g. a protected
    /// branch, or a specific digest). The mock tracks this intent's current digest
    /// so a later `moveDigest` can drive an execute-time mismatch.
    func setPushIntent(_ intent: GitPushIntent) {
        lock.lock(); defer { lock.unlock() }
        cannedPushIntent = intent
        currentDigestByIntent[intent.intentId] = intent.effectDigest
    }

    /// Arm the NEXT `pushApprove(...)` to throw `.digestMismatch` once (the
    /// supplied digest no longer matches the intent's current digest).
    func armDigestMismatchOnNextApprove() {
        lock.lock(); defer { lock.unlock() }
        failNextApproveDigestMismatch = true
    }

    /// Arm the NEXT `pushExecute(...)` to throw `.pushFailed` once (the remote was
    /// unreachable). The local commit + pending intent are preserved for retry.
    func armPushFailedOnNextExecute(message: String? = "remote unreachable") {
        lock.lock(); defer { lock.unlock() }
        failNextExecutePushFailed = true
        pushFailedMessage = message
    }

    /// Move an intent's CURRENT digest after approval so a later `pushExecute`
    /// throws `.digestMismatch` (the reviewed effect changed since approval).
    func moveDigest(forIntent intentId: String, to digest: String) {
        lock.lock(); defer { lock.unlock() }
        currentDigestByIntent[intentId] = digest
    }

    /// Script the push-status snapshot returned by `pushStatus()` (e.g. to model
    /// state recovered from SQLite after a relaunch).
    func setPushStatus(_ status: GitPushStatus) {
        lock.lock(); defer { lock.unlock() }
        cannedPushStatus = status
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

    // MARK: GitService (push flow, PR-036)
    //
    // The mock NEVER contacts a remote: `pushExecute` records a synthetic receipt.
    // Each step records itself in the GLOBALLY-ORDERED `pushSteps` log so a test
    // can assert the strict enqueue → approve → execute ordering (no execute
    // before approve), and scripted failures surface as thrown `GitPushError`s.

    func pushEnqueue(remote: String, ref: String) async throws -> GitPushIntent {
        pushEnqueueLocked(remote: remote, ref: ref)
    }

    func pushApprove(intentId: String, effectDigest: String, ackProtected: Bool) async throws -> GitPushApproval {
        try pushApproveLocked(intentId: intentId, effectDigest: effectDigest, ackProtected: ackProtected)
    }

    func pushExecute(intentId: String) async throws -> GitPushReceipt {
        try pushExecuteLocked(intentId: intentId)
    }

    func pushStatus() async throws -> GitPushStatus {
        pushStatusLocked()
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

    // MARK: Push critical sections (PR-036)

    private func pushEnqueueLocked(remote: String, ref: String) -> GitPushIntent {
        lock.lock(); defer { lock.unlock() }
        pushEnqueueCalls.append((remote: remote, ref: ref))
        pushSteps.append(.enqueue(remote: remote, ref: ref))
        let intent: GitPushIntent
        if let canned = cannedPushIntent {
            intent = canned
        } else {
            intent = GitPushIntent(
                intentId: "intent-\(pushEnqueueCalls.count)",
                effectDigest: "digest-\(remote)-\(ref)",
                remote: remote,
                remoteUrlRedacted: "https://\(remote).example/<redacted>.git",
                ref: ref,
                localOid: "feedface00000000feedface00000000feedface",
                remoteExpectedOid: nil,
                protected: false
            )
        }
        // Track the intent's current digest so a later approval/execute can detect
        // a moved effect.
        if currentDigestByIntent[intent.intentId] == nil {
            currentDigestByIntent[intent.intentId] = intent.effectDigest
        }
        return intent
    }

    private func pushApproveLocked(intentId: String, effectDigest: String, ackProtected: Bool) throws -> GitPushApproval {
        lock.lock(); defer { lock.unlock() }
        pushApproveCalls.append((intentId: intentId, digest: effectDigest, ackProtected: ackProtected))
        pushSteps.append(.approve(intentId: intentId, digest: effectDigest, ackProtected: ackProtected))
        // A scripted one-shot mismatch (the index/ref moved out from under the
        // reviewed effect). Records NO usable approval.
        if failNextApproveDigestMismatch {
            failNextApproveDigestMismatch = false
            throw GitPushError.digestMismatch
        }
        // The supplied digest must match the intent's CURRENT digest.
        if let current = currentDigestByIntent[intentId], current != effectDigest {
            throw GitPushError.digestMismatch
        }
        // A protected ref requires the explicit ack (the store gates this too; the
        // mock enforces it so an un-acked protected approval is refused honestly).
        if let canned = cannedPushIntent, canned.intentId == intentId, canned.protected, !ackProtected {
            throw GitPushError.protectedBranch
        }
        approvedIntentIds.insert(intentId)
        approvedDigestByIntent[intentId] = effectDigest
        return GitPushApproval(
            approvalId: "approval-\(pushApproveCalls.count)",
            intentId: intentId,
            matched: true
        )
    }

    private func pushExecuteLocked(intentId: String) throws -> GitPushReceipt {
        lock.lock(); defer { lock.unlock() }
        pushExecuteCalls.append(intentId)
        pushSteps.append(.execute(intentId: intentId))
        // Idempotent: a repeat execute with the same evidence reports already_done
        // and pushes exactly once.
        if completedIntentIds.contains(intentId) {
            return GitPushReceipt(
                pushed: true,
                remoteOid: completedRemoteOidByIntent[intentId] ?? "",
                idempotencyKey: "idem-\(intentId)",
                alreadyDone: true
            )
        }
        // Refuse without a recorded matching approval.
        guard approvedIntentIds.contains(intentId) else {
            throw GitPushError.noMatchingApproval
        }
        // Refuse if the digest changed since approval (the effect moved).
        if let approved = approvedDigestByIntent[intentId],
           let current = currentDigestByIntent[intentId],
           approved != current {
            throw GitPushError.digestMismatch
        }
        // A scripted one-shot push failure (e.g. unreachable remote). The local
        // commit + the pending intent are preserved (the mock keeps the approval
        // so a retry can re-execute the same effect).
        if failNextExecutePushFailed {
            failNextExecutePushFailed = false
            throw GitPushError.pushFailed(message: pushFailedMessage)
        }
        let remoteOid = "cafe\(String(intentId.suffix(4)))babe0000cafebabe0000cafebabe0000"
        completedIntentIds.insert(intentId)
        completedRemoteOidByIntent[intentId] = remoteOid
        return GitPushReceipt(
            pushed: true,
            remoteOid: remoteOid,
            idempotencyKey: "idem-\(intentId)",
            alreadyDone: false
        )
    }

    private func pushStatusLocked() -> GitPushStatus {
        lock.lock(); defer { lock.unlock() }
        pushStatusCallCount += 1
        pushSteps.append(.status)
        return cannedPushStatus ?? .empty
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
