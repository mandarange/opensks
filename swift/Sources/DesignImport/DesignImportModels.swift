// DesignImportModels.swift — the LOCAL design-import domain model (PR-039).
//
// Codable mirrors of the SHARED IMPORT JSON CONTRACT (snake_case) emitted by the
// bundled `opensks design import|import-approve|import-reject|import-status`
// subcommands. The import is LOCAL and HUMAN-REVIEWED: `import` only QUARANTINES
// and validates a local dir / .zip (it never promotes), `import-approve` promotes
// a quarantined package to the registry ONLY after the operator explicitly
// approves, and `import-reject` safely deletes the quarantine. NONE of these
// uploads the user's data anywhere — there is no network surface in this module.
//
// A rejected import carries a TYPED reason mapped to a clear human message + an
// SF Symbol so the view can explain WHY a package was refused (zip slip, a
// symlink, an executable/script, too many files, too large, a disallowed MIME, or
// too many archive entries) without relying on colour alone.

import SwiftUI

// MARK: - Source kind (local dir vs. local archive)

/// What kind of LOCAL source is being imported. Both are LOCAL paths: a directory
/// on disk or a `.zip` archive on disk. There is no remote/URL source here.
enum DesignImportKind: String, Codable, Sendable, Equatable, CaseIterable {
    case local
    case archive

    /// The CLI `--kind` argument value.
    var cliValue: String { rawValue }

    var label: String {
        switch self {
        case .local: return "Local folder"
        case .archive: return "Local archive (.zip)"
        }
    }

    var symbol: String {
        switch self {
        case .local: return "folder"
        case .archive: return "doc.zipper"
        }
    }
}

// MARK: - Quarantine status

/// The lifecycle status of a quarantined import. `quarantined` ⇒ validated and
/// awaiting human review (can be approved or rejected); `rejected` ⇒ refused at
/// validation (carries a `rejectedReason`, can NEVER be approved). `unknown` keeps
/// decoding total so a new server status never crashes the view.
enum DesignImportStatus: String, Codable, Sendable, Equatable, CaseIterable {
    case quarantined
    case rejected
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = DesignImportStatus(rawValue: raw) ?? .unknown
    }

    /// Whether a package in this status may be promoted (human-approved). Only a
    /// cleanly-quarantined package is ever approvable.
    var isApprovable: Bool { self == .quarantined }

    var label: String {
        switch self {
        case .quarantined: return "Quarantined"
        case .rejected: return "Rejected"
        case .unknown: return "Unknown"
        }
    }

    var symbol: String {
        switch self {
        case .quarantined: return "tray.and.arrow.down"
        case .rejected: return "xmark.octagon.fill"
        case .unknown: return "questionmark.circle"
        }
    }

    /// Semantic tint — a token, never a bare colour. The glyph + label carry the
    /// same meaning so colour is never the only signal.
    var tint: Color {
        switch self {
        case .quarantined: return GeneratedDesignTokens.colorStatusWarning
        case .rejected: return GeneratedDesignTokens.colorStatusDanger
        case .unknown: return GeneratedDesignTokens.colorTextMuted
        }
    }
}

// MARK: - Rejected reason

/// Why a candidate package was refused at validation. Each maps to a clear human
/// message + an SF Symbol so the view explains the refusal. `unknown` keeps
/// decoding total. These are the LOCAL hardening checks — a zip-slip path, a
/// symlink, an executable/script, too many files / entries, an over-size package,
/// or a disallowed MIME type.
enum DesignImportRejectedReason: String, Codable, Sendable, Equatable, CaseIterable {
    case zipSlip = "zip_slip"
    case symlink
    case executableOrScript = "executable_or_script"
    case tooManyFiles = "too_many_files"
    case tooLarge = "too_large"
    case mimeNotAllowed = "mime_not_allowed"
    case tooManyArchiveEntries = "too_many_archive_entries"
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = DesignImportRejectedReason(rawValue: raw) ?? .unknown
    }

    /// A short, clear human message naming exactly why the package was refused.
    var message: String {
        switch self {
        case .zipSlip:
            return "An archive entry tried to escape its folder (zip-slip path traversal)."
        case .symlink:
            return "The package contains a symbolic link, which is not allowed."
        case .executableOrScript:
            return "The package contains an executable or script, which is not allowed."
        case .tooManyFiles:
            return "The package contains too many files."
        case .tooLarge:
            return "The package is too large."
        case .mimeNotAllowed:
            return "The package contains a file type that is not allowed."
        case .tooManyArchiveEntries:
            return "The archive contains too many entries."
        case .unknown:
            return "The package was rejected by a safety check."
        }
    }

    /// SF Symbol so the reason is legible without relying on colour.
    var symbol: String {
        switch self {
        case .zipSlip: return "arrow.up.right.square"
        case .symlink: return "link"
        case .executableOrScript: return "terminal"
        case .tooManyFiles: return "doc.on.doc"
        case .tooLarge: return "scalemass"
        case .mimeNotAllowed: return "nosign"
        case .tooManyArchiveEntries: return "square.stack.3d.up"
        case .unknown: return "exclamationmark.triangle"
        }
    }

    /// A short label (also the accessibility word).
    var label: String {
        switch self {
        case .zipSlip: return "Path traversal"
        case .symlink: return "Symbolic link"
        case .executableOrScript: return "Executable or script"
        case .tooManyFiles: return "Too many files"
        case .tooLarge: return "Too large"
        case .mimeNotAllowed: return "Disallowed file type"
        case .tooManyArchiveEntries: return "Too many archive entries"
        case .unknown: return "Rejected"
        }
    }
}

// MARK: - Provenance

/// The provenance shown for a quarantined import: where it came from, its license
/// (if any) and the commit it was captured at (if any). Surfaced to the operator
/// so a human review has the facts before approving a promotion.
struct DesignImportProvenance: Codable, Sendable, Equatable {
    let source: String
    let license: String?
    let commit: String?

    enum CodingKeys: String, CodingKey {
        case source, license, commit
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        source = try c.decodeIfPresent(String.self, forKey: .source) ?? ""
        license = try c.decodeIfPresent(String.self, forKey: .license)
        commit = try c.decodeIfPresent(String.self, forKey: .commit)
    }

    init(source: String, license: String? = nil, commit: String? = nil) {
        self.source = source
        self.license = license
        self.commit = commit
    }

    /// The license to display, falling back to an honest "Unknown" when the
    /// package declared none (never silently implies a permissive license).
    var licenseDisplay: String { license ?? "Unknown" }

    /// The commit to display, falling back to "—" when none was captured.
    var commitDisplay: String { commit ?? "—" }
}

// MARK: - Import result (`opensks.design-import.v1`)

/// The result of `opensks design import …`. A QUARANTINE result — it never
/// promotes. `status` is `quarantined` (validated, awaiting human review) or
/// `rejected` (refused, carrying `rejectedReason`). `fileCount` / `byteSize`
/// describe what was quarantined.
struct DesignImportResult: Codable, Sendable, Equatable, Identifiable {
    let schema: String
    let quarantineId: String
    let status: DesignImportStatus
    let provenance: DesignImportProvenance
    let fileCount: Int
    let byteSize: Int
    let rejectedReason: DesignImportRejectedReason?

    var id: String { quarantineId }

    enum CodingKeys: String, CodingKey {
        case schema
        case quarantineId = "quarantine_id"
        case status
        case provenance
        case fileCount = "file_count"
        case byteSize = "byte_size"
        case rejectedReason = "rejected_reason"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-import.v1"
        quarantineId = try c.decodeIfPresent(String.self, forKey: .quarantineId) ?? ""
        status = try c.decodeIfPresent(DesignImportStatus.self, forKey: .status) ?? .unknown
        provenance = try c.decodeIfPresent(DesignImportProvenance.self, forKey: .provenance)
            ?? DesignImportProvenance(source: "")
        fileCount = try c.decodeIfPresent(Int.self, forKey: .fileCount) ?? 0
        byteSize = try c.decodeIfPresent(Int.self, forKey: .byteSize) ?? 0
        rejectedReason = try c.decodeIfPresent(DesignImportRejectedReason.self, forKey: .rejectedReason)
    }

    init(
        schema: String = "opensks.design-import.v1",
        quarantineId: String,
        status: DesignImportStatus,
        provenance: DesignImportProvenance,
        fileCount: Int,
        byteSize: Int,
        rejectedReason: DesignImportRejectedReason? = nil
    ) {
        self.schema = schema
        self.quarantineId = quarantineId
        self.status = status
        self.provenance = provenance
        self.fileCount = fileCount
        self.byteSize = byteSize
        self.rejectedReason = rejectedReason
    }

    /// True for a cleanly-quarantined package awaiting review (approvable).
    var isQuarantined: Bool { status == .quarantined }
    /// True for a package refused at validation (carries a reason, NOT approvable).
    var isRejected: Bool { status == .rejected }

    /// A human-readable byte size for the provenance card.
    var byteSizeDisplay: String {
        ByteCountFormatter.string(fromByteCount: Int64(byteSize), countStyle: .file)
    }
}

// MARK: - Quarantine entry (`opensks.design-import-status.v1` item)

/// One entry in the quarantine listing returned by `import-status`. Structurally
/// the same shape as `DesignImportResult` (the contract reuses the fields), with a
/// tolerant decode so a minimal item still loads.
struct DesignQuarantineEntry: Codable, Sendable, Equatable, Identifiable {
    let quarantineId: String
    let status: DesignImportStatus
    let provenance: DesignImportProvenance
    let fileCount: Int
    let byteSize: Int
    let rejectedReason: DesignImportRejectedReason?

    var id: String { quarantineId }

    enum CodingKeys: String, CodingKey {
        case quarantineId = "quarantine_id"
        case status
        case provenance
        case fileCount = "file_count"
        case byteSize = "byte_size"
        case rejectedReason = "rejected_reason"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        quarantineId = try c.decodeIfPresent(String.self, forKey: .quarantineId) ?? ""
        status = try c.decodeIfPresent(DesignImportStatus.self, forKey: .status) ?? .unknown
        provenance = try c.decodeIfPresent(DesignImportProvenance.self, forKey: .provenance)
            ?? DesignImportProvenance(source: "")
        fileCount = try c.decodeIfPresent(Int.self, forKey: .fileCount) ?? 0
        byteSize = try c.decodeIfPresent(Int.self, forKey: .byteSize) ?? 0
        rejectedReason = try c.decodeIfPresent(DesignImportRejectedReason.self, forKey: .rejectedReason)
    }

    init(
        quarantineId: String,
        status: DesignImportStatus,
        provenance: DesignImportProvenance,
        fileCount: Int,
        byteSize: Int,
        rejectedReason: DesignImportRejectedReason? = nil
    ) {
        self.quarantineId = quarantineId
        self.status = status
        self.provenance = provenance
        self.fileCount = fileCount
        self.byteSize = byteSize
        self.rejectedReason = rejectedReason
    }

    var isQuarantined: Bool { status == .quarantined }
    var isRejected: Bool { status == .rejected }

    var byteSizeDisplay: String {
        ByteCountFormatter.string(fromByteCount: Int64(byteSize), countStyle: .file)
    }

    /// Build an entry from a fresh import result (so a just-imported package shows
    /// in the list before a `refreshStatus()` round-trip).
    init(from result: DesignImportResult) {
        self.init(
            quarantineId: result.quarantineId,
            status: result.status,
            provenance: result.provenance,
            fileCount: result.fileCount,
            byteSize: result.byteSize,
            rejectedReason: result.rejectedReason
        )
    }
}

// MARK: - Status listing (`opensks.design-import-status.v1`)

/// The result of `opensks design import-status …` — the current quarantine list.
struct DesignImportStatusResult: Codable, Sendable, Equatable {
    let schema: String
    let quarantined: [DesignQuarantineEntry]

    enum CodingKeys: String, CodingKey {
        case schema, quarantined
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-import-status.v1"
        quarantined = try c.decodeIfPresent([DesignQuarantineEntry].self, forKey: .quarantined) ?? []
    }

    init(schema: String = "opensks.design-import-status.v1", quarantined: [DesignQuarantineEntry]) {
        self.schema = schema
        self.quarantined = quarantined
    }

    static let empty = DesignImportStatusResult(quarantined: [])
}

// MARK: - Approve / reject results

/// The result of `opensks design import-approve …` — the HUMAN-REVIEWED promotion
/// to the registry. `promoted: true` + the new `packageId` confirm the package
/// landed in `.opensks/design-systems/<id>/` after a RE-validation.
struct DesignImportApproveResult: Codable, Sendable, Equatable {
    let schema: String
    let promoted: Bool
    let packageId: String

    enum CodingKeys: String, CodingKey {
        case schema, promoted
        case packageId = "package_id"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-import-approve.v1"
        promoted = try c.decodeIfPresent(Bool.self, forKey: .promoted) ?? false
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
    }

    init(schema: String = "opensks.design-import-approve.v1", promoted: Bool, packageId: String) {
        self.schema = schema
        self.promoted = promoted
        self.packageId = packageId
    }
}

/// The result of `opensks design import-reject …` — the quarantine directory was
/// safely deleted. `rejected: true` + `deleted: true` confirm the candidate was
/// discarded (it was NEVER promoted).
struct DesignImportRejectResult: Codable, Sendable, Equatable {
    let schema: String
    let rejected: Bool
    let deleted: Bool

    enum CodingKeys: String, CodingKey {
        case schema, rejected, deleted
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-import-reject.v1"
        rejected = try c.decodeIfPresent(Bool.self, forKey: .rejected) ?? false
        deleted = try c.decodeIfPresent(Bool.self, forKey: .deleted) ?? false
    }

    init(schema: String = "opensks.design-import-reject.v1", rejected: Bool, deleted: Bool) {
        self.schema = schema
        self.rejected = rejected
        self.deleted = deleted
    }
}

// MARK: - The documented Open Design site (user-initiated URL only)

/// The ONE outward affordance is opening the documented Open Design site in the
/// user's browser. It is a USER-INITIATED action that opens a URL — NOT an API
/// call, and it uploads nothing. The URL is a constant here so the view never
/// fabricates an endpoint.
enum DesignImportLinks {
    /// The documented Open Design website the operator can open to find packages.
    static let openDesignURL = URL(string: "https://opensks.dev/open-design")!
}
