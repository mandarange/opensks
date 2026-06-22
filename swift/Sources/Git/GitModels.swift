// GitModels.swift — the READ-ONLY Git studio domain model (PR-034).
//
// Codable mirrors of the SHARED GIT JSON CONTRACT (snake_case) emitted by the
// bundled `opensks git status|branches|diff` read-only subcommands. The studio
// never mutates: there is no stage/commit/switch/push anywhere in this module.
//
// Change kind is conveyed by an SF Symbol + a label + a SEMANTIC design token —
// never colour alone, per the accessibility rule. Grouping (staged / unstaged /
// untracked / conflicted) is derived purely from the decoded index/worktree
// status characters so the two halves cannot drift over presentation.

import SwiftUI

// MARK: - Entry kind

/// The semantic kind of a single status entry. Drives the icon, the label and
/// the tint shown in the changes list. `unknown` keeps decoding total so a new
/// server kind never crashes the view.
enum GitEntryKind: String, Codable, Sendable, Equatable, CaseIterable {
    case modified
    case added
    case deleted
    case renamed
    case copied
    case untracked
    case conflicted
    case ignored
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = GitEntryKind(rawValue: raw) ?? .unknown
    }

    /// Semantic tint — a token, never a bare colour. Icon + label always carry
    /// the same meaning so colour is never the only signal.
    var tint: Color {
        switch self {
        case .added, .untracked: return GeneratedDesignTokens.colorStatusSuccess
        case .deleted, .conflicted: return GeneratedDesignTokens.colorStatusDanger
        case .modified, .renamed, .copied: return GeneratedDesignTokens.colorStatusWarning
        case .ignored, .unknown: return GeneratedDesignTokens.colorTextMuted
        }
    }

    /// SF Symbol so the kind is legible without relying on colour.
    var symbol: String {
        switch self {
        case .modified: return "pencil"
        case .added: return "plus.circle"
        case .deleted: return "minus.circle"
        case .renamed: return "arrow.right.circle"
        case .copied: return "doc.on.doc"
        case .untracked: return "questionmark.circle"
        case .conflicted: return "exclamationmark.triangle"
        case .ignored: return "eye.slash"
        case .unknown: return "circle"
        }
    }

    /// Human label (also the accessibility word).
    var label: String {
        switch self {
        case .modified: return "Modified"
        case .added: return "Added"
        case .deleted: return "Deleted"
        case .renamed: return "Renamed"
        case .copied: return "Copied"
        case .untracked: return "Untracked"
        case .conflicted: return "Conflicted"
        case .ignored: return "Ignored"
        case .unknown: return "Changed"
        }
    }
}

// MARK: - Status

/// `opensks.git-status.v1` — the working-tree status.
struct GitStatus: Codable, Sendable, Equatable {
    let schema: String
    let inRepo: Bool
    let branch: String?
    let detached: Bool
    let upstream: String?
    let ahead: Int
    let behind: Int
    let isDirty: Bool
    let entries: [GitStatusEntry]

    enum CodingKeys: String, CodingKey {
        case schema
        case inRepo = "in_repo"
        case branch
        case detached
        case upstream
        case ahead
        case behind
        case isDirty = "is_dirty"
        case entries
    }

    /// Tolerant decode: a minimal `in_repo: false` object omits most fields.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-status.v1"
        inRepo = try c.decodeIfPresent(Bool.self, forKey: .inRepo) ?? false
        branch = try c.decodeIfPresent(String.self, forKey: .branch)
        detached = try c.decodeIfPresent(Bool.self, forKey: .detached) ?? false
        upstream = try c.decodeIfPresent(String.self, forKey: .upstream)
        ahead = try c.decodeIfPresent(Int.self, forKey: .ahead) ?? 0
        behind = try c.decodeIfPresent(Int.self, forKey: .behind) ?? 0
        isDirty = try c.decodeIfPresent(Bool.self, forKey: .isDirty) ?? false
        entries = try c.decodeIfPresent([GitStatusEntry].self, forKey: .entries) ?? []
    }

    init(
        schema: String = "opensks.git-status.v1",
        inRepo: Bool,
        branch: String?,
        detached: Bool = false,
        upstream: String? = nil,
        ahead: Int = 0,
        behind: Int = 0,
        isDirty: Bool = false,
        entries: [GitStatusEntry] = []
    ) {
        self.schema = schema
        self.inRepo = inRepo
        self.branch = branch
        self.detached = detached
        self.upstream = upstream
        self.ahead = ahead
        self.behind = behind
        self.isDirty = isDirty
        self.entries = entries
    }

    /// An honest empty status used before the first load / when not in a repo.
    static let empty = GitStatus(inRepo: false, branch: nil)
}

/// One porcelain entry. `indexStatus` / `worktreeStatus` are the two raw status
/// characters (` ` = clean) and `kind` is the server-decided semantic kind.
struct GitStatusEntry: Codable, Sendable, Equatable, Identifiable {
    let path: String
    let origPath: String?
    let indexStatus: String
    let worktreeStatus: String
    let kind: GitEntryKind

    enum CodingKeys: String, CodingKey {
        case path
        case origPath = "orig_path"
        case indexStatus = "index_status"
        case worktreeStatus = "worktree_status"
        case kind
    }

    /// Stable identity for ForEach — a rename keys on old→new so the row is
    /// distinct from a same-named modify elsewhere.
    var id: String {
        if let origPath { return "\(origPath)->\(path)" }
        return "\(indexStatus)\(worktreeStatus):\(path)"
    }

    init(
        path: String,
        origPath: String? = nil,
        indexStatus: String,
        worktreeStatus: String,
        kind: GitEntryKind
    ) {
        self.path = path
        self.origPath = origPath
        self.indexStatus = indexStatus
        self.worktreeStatus = worktreeStatus
        self.kind = kind
    }

    /// A single non-space status char, or `nil` if the side is clean.
    private static func sig(_ s: String) -> Character? {
        let c = s.first ?? " "
        return c == " " ? nil : c
    }

    private var indexChar: Character? { Self.sig(indexStatus) }
    private var worktreeChar: Character? { Self.sig(worktreeStatus) }

    /// A merge conflict — both sides carry a `U`, or `DD`/`AA` etc. The kind
    /// already says `conflicted`, but the raw characters are the source of truth
    /// the grouping derives from so presentation can't drift from the contract.
    var isConflicted: Bool {
        if kind == .conflicted { return true }
        let i = indexStatus.first ?? " "
        let w = worktreeStatus.first ?? " "
        if i == "U" || w == "U" { return true }
        return (i == "A" && w == "A") || (i == "D" && w == "D")
    }

    /// Untracked: the porcelain `??` (both chars `?`) or the decoded kind.
    var isUntracked: Bool {
        if kind == .untracked { return true }
        return indexStatus.first == "?" && worktreeStatus.first == "?"
    }

    /// Has a staged change in the index (anything other than space/`?` there).
    var isStaged: Bool {
        guard !isConflicted, !isUntracked else { return false }
        return indexChar != nil
    }

    /// Has an unstaged working-tree change.
    var isUnstaged: Bool {
        guard !isConflicted, !isUntracked else { return false }
        return worktreeChar != nil
    }

    /// A rename/copy carries an original path the row renders as `orig → new`.
    var isRename: Bool { origPath != nil && origPath != path }
}

// MARK: - Branches

/// `opensks.git-branches.v1` — the local branch list with upstream tracking and
/// worktree occupancy.
struct GitBranches: Codable, Sendable, Equatable {
    let schema: String
    let current: String?
    let branches: [GitBranchInfo]

    enum CodingKeys: String, CodingKey {
        case schema, current, branches
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-branches.v1"
        current = try c.decodeIfPresent(String.self, forKey: .current)
        branches = try c.decodeIfPresent([GitBranchInfo].self, forKey: .branches) ?? []
    }

    init(
        schema: String = "opensks.git-branches.v1",
        current: String?,
        branches: [GitBranchInfo]
    ) {
        self.schema = schema
        self.current = current
        self.branches = branches
    }

    static let empty = GitBranches(current: nil, branches: [])
}

/// One local branch. `worktreePath` / `checkedOutElsewhere` reflect that a
/// branch is checked out in ANOTHER worktree (so it is "occupied" and a switch
/// would be unsafe — rendered disabled, even though this PR has no switch action).
struct GitBranchInfo: Codable, Sendable, Equatable, Identifiable {
    let name: String
    let isCurrent: Bool
    let upstream: String?
    let ahead: Int
    let behind: Int
    let worktreePath: String?
    let checkedOutElsewhere: Bool

    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name
        case isCurrent = "is_current"
        case upstream
        case ahead
        case behind
        case worktreePath = "worktree_path"
        case checkedOutElsewhere = "checked_out_elsewhere"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        isCurrent = try c.decodeIfPresent(Bool.self, forKey: .isCurrent) ?? false
        upstream = try c.decodeIfPresent(String.self, forKey: .upstream)
        ahead = try c.decodeIfPresent(Int.self, forKey: .ahead) ?? 0
        behind = try c.decodeIfPresent(Int.self, forKey: .behind) ?? 0
        worktreePath = try c.decodeIfPresent(String.self, forKey: .worktreePath)
        checkedOutElsewhere = try c.decodeIfPresent(Bool.self, forKey: .checkedOutElsewhere) ?? false
    }

    init(
        name: String,
        isCurrent: Bool,
        upstream: String? = nil,
        ahead: Int = 0,
        behind: Int = 0,
        worktreePath: String? = nil,
        checkedOutElsewhere: Bool = false
    ) {
        self.name = name
        self.isCurrent = isCurrent
        self.upstream = upstream
        self.ahead = ahead
        self.behind = behind
        self.worktreePath = worktreePath
        self.checkedOutElsewhere = checkedOutElsewhere
    }

    /// A branch occupied by another worktree (or carrying a worktree path that
    /// is not the current checkout) is "occupied" — surfaced as disabled.
    var isOccupiedElsewhere: Bool {
        checkedOutElsewhere || (worktreePath != nil && !isCurrent)
    }
}

// MARK: - Diff

/// `opensks.git-diff.v1` — a set of file diffs (status or staged). Decode-only:
/// the studio never re-encodes a diff, and the reused `TextDiffHunk` is itself
/// `Decodable`-only.
struct GitDiff: Decodable, Sendable, Equatable {
    let schema: String
    let files: [GitDiffFile]

    enum CodingKeys: String, CodingKey {
        case schema, files
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.git-diff.v1"
        files = try c.decodeIfPresent([GitDiffFile].self, forKey: .files) ?? []
    }

    init(schema: String = "opensks.git-diff.v1", files: [GitDiffFile]) {
        self.schema = schema
        self.files = files
    }

    static let empty = GitDiff(files: [])

    /// The diff for a single path, if present (rename diffs key on new path).
    func file(forPath path: String) -> GitDiffFile? {
        files.first { $0.path == path }
    }
}

/// One hunk of `opensks.git-diff.v1`. The git-diff contract carries ONLY the
/// four 1-based line counters and the `+`/`-`/context `lines` — there is no
/// per-hunk `kind` (unlike PR-033's editor `TextDiffHunk`), so this is its own
/// faithful type. `lines` feed the reused `DiffHunkView` line classifier.
struct GitDiffHunk: Decodable, Sendable, Equatable {
    let oldStart: Int
    let oldLines: Int
    let newStart: Int
    let newLines: Int
    let lines: [String]

    enum CodingKeys: String, CodingKey {
        case oldStart = "old_start"
        case oldLines = "old_lines"
        case newStart = "new_start"
        case newLines = "new_lines"
        case lines
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        oldStart = try c.decodeIfPresent(Int.self, forKey: .oldStart) ?? 0
        oldLines = try c.decodeIfPresent(Int.self, forKey: .oldLines) ?? 0
        newStart = try c.decodeIfPresent(Int.self, forKey: .newStart) ?? 0
        newLines = try c.decodeIfPresent(Int.self, forKey: .newLines) ?? 0
        lines = try c.decodeIfPresent([String].self, forKey: .lines) ?? []
    }

    init(oldStart: Int, oldLines: Int, newStart: Int, newLines: Int, lines: [String]) {
        self.oldStart = oldStart
        self.oldLines = oldLines
        self.newStart = newStart
        self.newLines = newLines
        self.lines = lines
    }
}

/// One file's diff. Its hunks flatten into the SAME `DiffDisplayLine` model
/// PR-033's `DiffHunkView` renders, so the git diff view reuses that renderer
/// with zero new rendering code.
struct GitDiffFile: Decodable, Sendable, Equatable, Identifiable {
    let path: String
    let origPath: String?
    let isBinary: Bool
    let hunks: [GitDiffHunk]

    var id: String {
        if let origPath { return "\(origPath)->\(path)" }
        return path
    }

    enum CodingKeys: String, CodingKey {
        case path
        case origPath = "orig_path"
        case isBinary = "is_binary"
        case hunks
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        origPath = try c.decodeIfPresent(String.self, forKey: .origPath)
        isBinary = try c.decodeIfPresent(Bool.self, forKey: .isBinary) ?? false
        hunks = try c.decodeIfPresent([GitDiffHunk].self, forKey: .hunks) ?? []
    }

    init(path: String, origPath: String? = nil, isBinary: Bool = false, hunks: [GitDiffHunk]) {
        self.path = path
        self.origPath = origPath
        self.isBinary = isBinary
        self.hunks = hunks
    }

    /// Flatten this file's hunks into renderable diff lines for `DiffHunkView`.
    var displayLines: [DiffDisplayLine] {
        var out: [DiffDisplayLine] = []
        var id = 0
        for hunk in hunks {
            let header = "@@ -\(hunk.oldStart),\(hunk.oldLines) +\(hunk.newStart),\(hunk.newLines) @@"
            out.append(DiffDisplayLine(id: id, kind: .meta, text: header)); id += 1
            for raw in hunk.lines {
                out.append(DiffDisplayLine.classify(raw, id: id)); id += 1
            }
        }
        return out
    }
}

// MARK: - Grouping (derived purely from decoded status)

/// The four status groups the changes list renders. Derivation is a pure
/// function of the decoded entries so presentation can never drift from the
/// contract: an entry can appear in BOTH staged and unstaged when it has both an
/// index and a worktree change (Git's `MM`).
struct GitStatusGroups: Equatable {
    var staged: [GitStatusEntry] = []
    var unstaged: [GitStatusEntry] = []
    var untracked: [GitStatusEntry] = []
    var conflicted: [GitStatusEntry] = []

    var isEmpty: Bool {
        staged.isEmpty && unstaged.isEmpty && untracked.isEmpty && conflicted.isEmpty
    }

    init(from entries: [GitStatusEntry]) {
        for entry in entries {
            if entry.isConflicted {
                conflicted.append(entry)
                continue
            }
            if entry.isUntracked {
                untracked.append(entry)
                continue
            }
            if entry.isStaged { staged.append(entry) }
            if entry.isUnstaged { unstaged.append(entry) }
        }
    }
}
