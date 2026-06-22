// IntelligenceModels.swift — Codable mirrors of the PR-041 Project Intelligence
// wire contract (snake_case JSON). Every type here decodes the SHARED `opensks
// intel …` subcommand output verbatim:
//
//   • freshness        -> `opensks.intel-freshness.v1`        (the CURRENT stamp)
//   • freshness-check  -> `opensks.intel-freshness-check.v1`  (a STAMPED check)
//   • codegraph-query  -> `opensks.intel-codegraph.v1`        (a PAGED slice)
//   • glossary         -> `opensks.intel-glossary.v1`
//   • architecture     -> `opensks.intel-architecture.v1`
//
// FRESHNESS HONESTY (the central invariant of this PR): a freshness STAMP is the
// `(head, worktree, index)` triple the data was loaded with. A `freshness-check`
// compares a stamp against the CURRENT triple; it reports `fresh == true` ONLY
// when EVERY provided stamp matches current. Any divergence — or a missing /
// unknown stamp — yields `fresh == false` with a `stale_reason`. The badge state
// derived from that result is therefore NEVER `fresh` unless the check explicitly
// said so: a stale (or unverified) item can never render as "current".

import Foundation
import SwiftUI

// MARK: - Freshness stamp (the loaded-at triple)

/// Mirrors `opensks.intel-freshness.v1` — the CURRENT freshness stamp. Also the
/// `current` block inside a freshness-check result, and the `freshness` block
/// embedded in every paged / listing payload (so each surface carries the exact
/// stamp its data was loaded with).
struct IntelFreshnessStamp: Codable, Sendable, Equatable {
    /// Schema id; absent when this is the embedded `current`/`freshness` block.
    let schema: String?
    /// HEAD commit hash. `nil` when the workspace is not a git repo (or detached
    /// with no commit). A `nil` head is a real, honest value — not "fresh".
    let headHash: String?
    /// A digest of the working tree (tracked content). Always present.
    let worktreeHash: String
    /// A digest of the git index (staged content). Always present.
    let indexHash: String
    /// Whether the workspace is inside a git repository. When false, head_hash is
    /// expected to be nil and freshness is governed by the worktree/index digests.
    let inRepo: Bool

    init(
        schema: String? = nil,
        headHash: String?,
        worktreeHash: String,
        indexHash: String,
        inRepo: Bool
    ) {
        self.schema = schema
        self.headHash = headHash
        self.worktreeHash = worktreeHash
        self.indexHash = indexHash
        self.inRepo = inRepo
    }
}

// MARK: - Freshness check (a stamped comparison against current)

/// Why a stamped item is no longer fresh. Lenient string enum with an `.unknown`
/// fallback so a future reason never crashes the decoder — and an unknown reason
/// is still treated as NOT fresh (never silently "current").
enum IntelStaleReason: String, Codable, Sendable, Equatable {
    case headChanged = "head_changed"
    case worktreeChanged = "worktree_changed"
    case indexChanged = "index_changed"
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = IntelStaleReason(rawValue: raw) ?? .unknown
    }

    /// Human-readable, surfaced ALONGSIDE the badge glyph so "stale" is textual,
    /// never colour-alone.
    var label: String {
        switch self {
        case .headChanged: return "HEAD moved"
        case .worktreeChanged: return "Working tree changed"
        case .indexChanged: return "Index changed"
        case .unknown: return "Out of date"
        }
    }
}

/// Mirrors `opensks.intel-freshness-check.v1` — the result of comparing a STAMPED
/// freshness against the current triple. `fresh` is true ONLY when every provided
/// stamp matched current; otherwise `fresh == false` with a `staleReason`.
struct IntelFreshnessCheck: Codable, Sendable, Equatable {
    let schema: String
    let fresh: Bool
    /// Why it is stale; `nil` only when `fresh == true`.
    let staleReason: IntelStaleReason?
    /// The current triple at check time (so the UI can re-stamp on reload).
    let current: IntelFreshnessStamp
}

// MARK: - Code graph (PAGED)

/// One symbol record in the code graph: a symbol at a path/line. Identity is the
/// `path:line:symbol` triple so `ForEach` is stable across pages.
struct IntelCodeGraphRecord: Codable, Sendable, Equatable, Identifiable {
    let path: String
    let symbol: String
    let kind: String
    let line: Int

    var id: String { "\(path):\(line):\(symbol)" }
}

/// Mirrors `opensks.intel-codegraph.v1` — ONE PAGE of the code graph. The graph
/// can be huge (thousands of symbols), so the surface NEVER loads it whole: it
/// requests `limit`/`offset` windows and `total` reports the full size for the
/// pager. Each page carries the freshness stamp its data was read with.
struct IntelCodeGraphPage: Codable, Sendable, Equatable {
    let schema: String
    let total: Int
    let limit: Int
    let offset: Int
    let records: [IntelCodeGraphRecord]
    let freshness: IntelFreshnessStamp

    /// An empty first page (used as the store's pre-load placeholder).
    static let empty = IntelCodeGraphPage(
        schema: "opensks.intel-codegraph.v1",
        total: 0,
        limit: 0,
        offset: 0,
        records: [],
        freshness: IntelFreshnessStamp(headHash: nil, worktreeHash: "", indexHash: "", inRepo: false)
    )
}

// MARK: - Glossary

/// One glossary term + its definition + the refs (paths / symbols) that anchor it.
struct IntelGlossaryTerm: Codable, Sendable, Equatable, Identifiable {
    let term: String
    let definition: String
    let refs: [String]

    var id: String { term }
}

/// Mirrors `opensks.intel-glossary.v1`.
struct IntelGlossary: Codable, Sendable, Equatable {
    let schema: String
    let terms: [IntelGlossaryTerm]
    let freshness: IntelFreshnessStamp

    static let empty = IntelGlossary(
        schema: "opensks.intel-glossary.v1",
        terms: [],
        freshness: IntelFreshnessStamp(headHash: nil, worktreeHash: "", indexHash: "", inRepo: false)
    )
}

// MARK: - Architecture

/// One architecture record: a titled note with detail and refs (paths / symbols
/// / run / conversation ids) that the UI can deep-link from.
struct IntelArchitectureRecord: Codable, Sendable, Equatable, Identifiable {
    let id: String
    let title: String
    let detail: String
    let refs: [String]
}

/// Mirrors `opensks.intel-architecture.v1`.
struct IntelArchitecture: Codable, Sendable, Equatable {
    let schema: String
    let records: [IntelArchitectureRecord]
    let freshness: IntelFreshnessStamp

    static let empty = IntelArchitecture(
        schema: "opensks.intel-architecture.v1",
        records: [],
        freshness: IntelFreshnessStamp(headHash: nil, worktreeHash: "", indexHash: "", inRepo: false)
    )
}

// MARK: - Badge state (NEVER colour alone; stale never renders as current)

/// The freshness badge a view shows for a section. The two cases are mutually
/// exclusive and there is NO third "unknown-but-treated-as-fresh" case: anything
/// that is not provably fresh is `.stale`. This is the type-level guarantee that
/// a stale (or unverified) item can never be drawn as "current".
enum IntelFreshnessBadge: Equatable {
    /// Provably current: a freshness-check returned `fresh == true`.
    case fresh
    /// Not provably current: a check returned `fresh == false` (carrying a reason)
    /// OR a check has not completed / failed (reason `nil` → "Checking…"/"Unknown").
    case stale(reason: IntelStaleReason?)

    /// Build the badge from a completed freshness-check result. `fresh == true`
    /// → `.fresh`; otherwise `.stale(reason:)` — a divergence is NEVER fresh.
    init(check: IntelFreshnessCheck) {
        if check.fresh {
            self = .fresh
        } else {
            self = .stale(reason: check.staleReason)
        }
    }

    /// True ONLY for the provably-current case. The view binds "show as current"
    /// to THIS, so a `.stale` value can never be presented as fresh.
    var isFresh: Bool {
        if case .fresh = self { return true }
        return false
    }

    /// The text label surfaced next to the glyph (never colour alone).
    var label: String {
        switch self {
        case .fresh:
            return "Fresh"
        case .stale(let reason):
            // A stale item is ALWAYS textually marked stale; an unknown/absent
            // reason still says "Stale", never "Fresh"/"Current".
            if let reason { return "Stale · \(reason.label)" }
            return "Stale"
        }
    }

    /// SF Symbol paired with the colour so the state reads without colour.
    var symbol: String {
        switch self {
        case .fresh: return "checkmark.seal.fill"
        case .stale: return "exclamationmark.triangle.fill"
        }
    }

    /// Semantic-token colour. Fresh → success; stale → warning. ALWAYS paired with
    /// `symbol` + `label`, never used as the sole signal.
    var tint: Color {
        switch self {
        case .fresh: return GeneratedDesignTokens.colorStatusSuccess
        case .stale: return GeneratedDesignTokens.colorStatusWarning
        }
    }

    /// A `StatusPill.Kind` for surfaces that prefer the shared pill (glyph+tint+label).
    var pillKind: StatusPill.Kind {
        switch self {
        case .fresh: return .success
        case .stale: return .warning
        }
    }
}
