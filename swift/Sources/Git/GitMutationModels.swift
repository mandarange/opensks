// GitMutationModels.swift — Codable mirrors of the SHARED GIT-MUTATION JSON
// CONTRACT (snake_case) for the LOCAL git mutations introduced in PR-035:
// stage / unstage, create-branch, switch (+ read-only switch-preflight), and
// commit (+ commit-preview). Each mutation is a SUBCOMMAND of the existing
// `git` verb — there is no new root dispatch arm on either half.
//
// LOCAL ONLY: there is deliberately NO push model anywhere in this file. The
// commit flow is gated on a reviewed `index_hash` so a commit can only ever
// contain the exact staged paths the operator reviewed (a stale preview is
// refused server-side with `index_changed`, surfaced here as a typed error).
//
// Secret / data-plane paths are NEVER staged: the stage result carries a
// `rejected` list naming each refused path and why, and the UI presents such a
// path as non-stageable. Decoding is tolerant (`.unknown` fallbacks, optional
// fields default) so a future server value never crashes the decoder.

import SwiftUI

// MARK: - Stage rejection reason

/// Why a path was refused for staging. A secret-bearing or data-plane path can
/// NEVER be staged; the reason drives the non-stageable presentation + label.
enum GitStageRejectReason: String, Codable, Sendable, Equatable {
    case secretRestricted = "secret_restricted"
    case dataPlane = "data_plane"
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = GitStageRejectReason(rawValue: raw) ?? .unknown
    }

    /// Human label (also the accessibility word) for the refusal.
    var label: String {
        switch self {
        case .secretRestricted: return "Secret — never staged"
        case .dataPlane: return "Data plane — never staged"
        case .unknown: return "Restricted — never staged"
        }
    }

    /// A longer, honest explanation surfaced as the non-stageable row's reason.
    var explanation: String {
        switch self {
        case .secretRestricted:
            return "This path is secret-restricted and can never be staged or committed."
        case .dataPlane:
            return "This path is a data-plane artifact and can never be staged or committed."
        case .unknown:
            return "This path is restricted and can never be staged or committed."
        }
    }

    var symbol: String {
        switch self {
        case .secretRestricted: return "key.slash"
        case .dataPlane: return "externaldrive.badge.xmark"
        case .unknown: return "hand.raised.slash"
        }
    }
}

/// One refused path from a stage attempt (or a pre-flight classification).
struct GitStageRejection: Codable, Sendable, Equatable, Identifiable {
    let path: String
    let reason: GitStageRejectReason

    var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path, reason
    }

    init(path: String, reason: GitStageRejectReason) {
        self.path = path
        self.reason = reason
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        reason = try c.decodeIfPresent(GitStageRejectReason.self, forKey: .reason) ?? .unknown
    }
}

// MARK: - Switch preflight (read-only check)

/// The kind of blocker that prevents a safe branch switch.
enum GitSwitchBlockerKind: String, Codable, Sendable, Equatable {
    case dirtyWorktree = "dirty_worktree"
    case conflict
    /// Surfaced by the Swift half when an open editor buffer is unsaved: the
    /// switch must not proceed silently over unsaved work. Never sent by the CLI.
    case unsavedBuffers = "unsaved_buffers"
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = GitSwitchBlockerKind(rawValue: raw) ?? .unknown
    }

    var label: String {
        switch self {
        case .dirtyWorktree: return "Uncommitted changes"
        case .conflict: return "Unresolved conflicts"
        case .unsavedBuffers: return "Unsaved editor buffers"
        case .unknown: return "Switch blocked"
        }
    }

    var symbol: String {
        switch self {
        case .dirtyWorktree: return "pencil.and.outline"
        case .conflict: return "exclamationmark.triangle"
        case .unsavedBuffers: return "doc.badge.ellipsis"
        case .unknown: return "hand.raised"
        }
    }
}

/// One blocker — a kind plus the paths it concerns.
struct GitSwitchBlocker: Codable, Sendable, Equatable, Identifiable {
    let kind: GitSwitchBlockerKind
    let paths: [String]

    var id: String { kind.rawValue + ":" + paths.joined(separator: ",") }

    enum CodingKeys: String, CodingKey {
        case kind, paths
    }

    init(kind: GitSwitchBlockerKind, paths: [String]) {
        self.kind = kind
        self.paths = paths
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decodeIfPresent(GitSwitchBlockerKind.self, forKey: .kind) ?? .unknown
        paths = try c.decodeIfPresent([String].self, forKey: .paths) ?? []
    }

    /// Honest one-line explanation including how many paths are affected.
    var explanation: String {
        let n = paths.count
        switch kind {
        case .dirtyWorktree:
            return "\(n) uncommitted change\(n == 1 ? "" : "s") in the working tree."
        case .conflict:
            return "\(n) path\(n == 1 ? "" : "s") with unresolved merge conflicts."
        case .unsavedBuffers:
            return "\(n) open editor buffer\(n == 1 ? "" : "s") with unsaved edits."
        case .unknown:
            return "\(n) path\(n == 1 ? "" : "s") blocking the switch."
        }
    }
}

/// `opensks.git-switch-preflight.v1` — the read-only "can I switch?" answer.
struct GitSwitchPreflight: Codable, Sendable, Equatable {
    let schema: String
    let canSwitch: Bool
    let blockers: [GitSwitchBlocker]

    enum CodingKeys: String, CodingKey {
        case schema
        case canSwitch = "can_switch"
        case blockers
    }

    init(schema: String = "opensks.git-switch-preflight.v1", canSwitch: Bool, blockers: [GitSwitchBlocker]) {
        self.schema = schema
        self.canSwitch = canSwitch
        self.blockers = blockers
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-switch-preflight.v1"
        canSwitch = try c.decodeIfPresent(Bool.self, forKey: .canSwitch) ?? false
        blockers = try c.decodeIfPresent([GitSwitchBlocker].self, forKey: .blockers) ?? []
    }

    static let clean = GitSwitchPreflight(canSwitch: true, blockers: [])
}

// MARK: - Create branch

/// `opensks.git-create-branch.v1` — the result of creating a local branch.
struct GitCreateBranchResult: Codable, Sendable, Equatable {
    let schema: String
    let created: Bool
    let branch: String
    let head: String

    enum CodingKeys: String, CodingKey {
        case schema, created, branch, head
    }

    init(schema: String = "opensks.git-create-branch.v1", created: Bool, branch: String, head: String) {
        self.schema = schema
        self.created = created
        self.branch = branch
        self.head = head
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-create-branch.v1"
        created = try c.decodeIfPresent(Bool.self, forKey: .created) ?? false
        branch = try c.decode(String.self, forKey: .branch)
        head = try c.decodeIfPresent(String.self, forKey: .head) ?? ""
    }
}

// MARK: - Switch

/// `opensks.git-switch.v1` — the result of a successful branch switch.
struct GitSwitchResult: Codable, Sendable, Equatable {
    let schema: String
    let switched: Bool
    let branch: String

    enum CodingKeys: String, CodingKey {
        case schema, switched, branch
    }

    init(schema: String = "opensks.git-switch.v1", switched: Bool, branch: String) {
        self.schema = schema
        self.switched = switched
        self.branch = branch
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-switch.v1"
        switched = try c.decodeIfPresent(Bool.self, forKey: .switched) ?? false
        branch = try c.decode(String.self, forKey: .branch)
    }
}

// MARK: - Stage / unstage

/// `opensks.git-stage.v1` — what was staged and what was refused. A secret /
/// data-plane path appears ONLY in `rejected`, never in `staged`.
struct GitStageResult: Codable, Sendable, Equatable {
    let schema: String
    let staged: [String]
    let rejected: [GitStageRejection]

    enum CodingKeys: String, CodingKey {
        case schema, staged, rejected
    }

    init(schema: String = "opensks.git-stage.v1", staged: [String], rejected: [GitStageRejection] = []) {
        self.schema = schema
        self.staged = staged
        self.rejected = rejected
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-stage.v1"
        staged = try c.decodeIfPresent([String].self, forKey: .staged) ?? []
        rejected = try c.decodeIfPresent([GitStageRejection].self, forKey: .rejected) ?? []
    }
}

/// `opensks.git-unstage.v1` — what was moved back out of the index.
struct GitUnstageResult: Codable, Sendable, Equatable {
    let schema: String
    let unstaged: [String]

    enum CodingKeys: String, CodingKey {
        case schema, unstaged
    }

    init(schema: String = "opensks.git-unstage.v1", unstaged: [String]) {
        self.schema = schema
        self.unstaged = unstaged
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-unstage.v1"
        unstaged = try c.decodeIfPresent([String].self, forKey: .unstaged) ?? []
    }
}

// MARK: - Commit preview

/// `opensks.git-commit-preview.v1` — the staged tree the next commit would
/// capture. `indexHash` is the OPTIMISTIC-CONCURRENCY token: it is echoed back
/// on commit as `--expected-index-hash`; if the live index has moved, the commit
/// is refused with `index_changed` so a commit can only contain reviewed paths.
struct GitCommitPreview: Codable, Sendable, Equatable {
    let schema: String
    let indexHash: String
    let stagedPaths: [String]
    let hasStaged: Bool

    enum CodingKeys: String, CodingKey {
        case schema
        case indexHash = "index_hash"
        case stagedPaths = "staged_paths"
        case hasStaged = "has_staged"
    }

    init(
        schema: String = "opensks.git-commit-preview.v1",
        indexHash: String,
        stagedPaths: [String],
        hasStaged: Bool
    ) {
        self.schema = schema
        self.indexHash = indexHash
        self.stagedPaths = stagedPaths
        self.hasStaged = hasStaged
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-commit-preview.v1"
        indexHash = try c.decodeIfPresent(String.self, forKey: .indexHash) ?? ""
        stagedPaths = try c.decodeIfPresent([String].self, forKey: .stagedPaths) ?? []
        hasStaged = try c.decodeIfPresent(Bool.self, forKey: .hasStaged) ?? false
    }

    static let empty = GitCommitPreview(indexHash: "", stagedPaths: [], hasStaged: false)
}

// MARK: - Commit

/// `opensks.git-commit.v1` — the result of a successful local commit. Carries the
/// new commit sha and the EXACT paths that were committed (the receipt + the
/// conversation commit card render precisely these).
struct GitCommitResult: Codable, Sendable, Equatable {
    let schema: String
    let committed: Bool
    let commit: String
    let paths: [String]

    enum CodingKeys: String, CodingKey {
        case schema, committed, commit, paths
    }

    init(schema: String = "opensks.git-commit.v1", committed: Bool, commit: String, paths: [String]) {
        self.schema = schema
        self.committed = committed
        self.commit = commit
        self.paths = paths
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-commit.v1"
        committed = try c.decodeIfPresent(Bool.self, forKey: .committed) ?? false
        commit = try c.decode(String.self, forKey: .commit)
        paths = try c.decodeIfPresent([String].self, forKey: .paths) ?? []
    }

    /// First 8 chars of the commit sha for a compact, honest reference.
    var shortSha: String { String(commit.prefix(8)) }
}

// MARK: - Error envelope

/// `opensks.git-error.v1` — the structured error a mutation emits on a non-zero
/// exit. `blockers` is present for `switch_blocked`; `rejected` for a secret /
/// data-plane refusal; both empty for `index_changed`.
struct GitErrorEnvelope: Codable, Sendable, Equatable {
    let schema: String
    let error: Payload

    struct Payload: Codable, Sendable, Equatable {
        let code: String
        let blockers: [GitSwitchBlocker]
        let rejected: [GitStageRejection]
        let message: String?

        enum CodingKeys: String, CodingKey {
            case code, blockers, rejected, message
        }

        init(code: String, blockers: [GitSwitchBlocker] = [], rejected: [GitStageRejection] = [], message: String? = nil) {
            self.code = code
            self.blockers = blockers
            self.rejected = rejected
            self.message = message
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            code = try c.decode(String.self, forKey: .code)
            blockers = try c.decodeIfPresent([GitSwitchBlocker].self, forKey: .blockers) ?? []
            rejected = try c.decodeIfPresent([GitStageRejection].self, forKey: .rejected) ?? []
            message = try c.decodeIfPresent(String.self, forKey: .message)
        }
    }

    enum CodingKeys: String, CodingKey {
        case schema, error
    }

    init(schema: String = "opensks.git-error.v1", error: Payload) {
        self.schema = schema
        self.error = error
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-error.v1"
        error = try c.decode(Payload.self, forKey: .error)
    }
}

// MARK: - Store-side view state (not wire types)

/// The visible "this switch was blocked" state. Non-nil ⇒ the store did NOT call
/// `switch`; the view names the target and the blockers (CLI worktree dirt/
/// conflicts AND/OR unsaved editor buffers) and offers no silent --force.
struct GitSwitchBlockState: Equatable, Identifiable {
    let target: String
    let blockers: [GitSwitchBlocker]

    var id: String { target }

    /// A combined, honest summary line for the banner.
    var summary: String {
        let kinds = blockers.map(\.kind.label)
        let unique = Array(NSOrderedSet(array: kinds)) as? [String] ?? kinds
        let reasons = unique.joined(separator: " · ")
        return "Can't switch to \(target): \(reasons.isEmpty ? "blocked" : reasons)."
    }
}

/// The commit-composer state owned by the store: the reviewed preview, the
/// message draft, the in-flight + STALE flags. A commit is only enabled with
/// staged paths AND a non-empty message, and a STALE preview must be refreshed
/// before committing (so a commit only ever contains the reviewed paths).
struct GitCommitComposerState: Equatable {
    /// The reviewed preview (staged paths + the optimistic-concurrency hash).
    var preview: GitCommitPreview = .empty
    /// The commit message draft.
    var message: String = ""
    /// True after an `index_changed`: the preview no longer matches the live
    /// index and MUST be refreshed before a commit is allowed.
    var isStale: Bool = false
    /// True while a commit is in flight (the button disables so one click = one
    /// commit).
    var isCommitting: Bool = false
    /// The most recent successful commit receipt, surfaced as the receipt card.
    var receipt: GitCommitResult?

    /// The staged paths under review (exactly what a commit would capture).
    var stagedPaths: [String] { preview.stagedPaths }

    /// There is something reviewed to commit.
    var hasStaged: Bool { preview.hasStaged && !preview.stagedPaths.isEmpty }

    /// The trimmed message is non-empty.
    var hasMessage: Bool {
        !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Commit is allowed ONLY with staged paths, a message, not stale, not busy.
    var canCommit: Bool {
        hasStaged && hasMessage && !isStale && !isCommitting
    }
}
