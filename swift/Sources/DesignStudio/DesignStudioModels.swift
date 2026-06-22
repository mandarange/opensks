// DesignStudioModels.swift — the Design Studio domain model (PR-040).
//
// Codable mirrors of the SHARED STUDIO JSON CONTRACT (snake_case) emitted by the
// bundled `opensks design audit|activate|active-status|revision-…` subcommands.
// These are SUBCOMMANDS of the EXISTING `design` verb (the same verb the PR-039
// import surface uses).
//
// The studio is ATOMIC and HONEST about state: an `activate` always runs the
// audit first, and a FAILING audit (`blocks_activation:true`) BLOCKS the
// activation — the previously active package is left in place. Every finding and
// every revision conveys its state with an ICON + a LABEL + a semantic token,
// never colour alone. A revision is always LINKED TO A PROOF (`proof_ref`).
//
// Decoding is tolerant: unknown finding kinds / severities / revision states /
// audit error codes fall back to `.unknown` so a future server value never
// crashes the view.

import SwiftUI

// MARK: - Finding kind

/// What an audit finding is about. `unknown` keeps decoding total. Each kind maps
/// to an SF Symbol + a short label so the finding is legible without colour.
enum DesignAuditFindingKind: String, Codable, Sendable, Equatable, CaseIterable {
    case contrast
    case hitTarget = "hit_target"
    case layout
    case accessibility
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = DesignAuditFindingKind(rawValue: raw) ?? .unknown
    }

    var label: String {
        switch self {
        case .contrast: return "Contrast"
        case .hitTarget: return "Hit target"
        case .layout: return "Layout"
        case .accessibility: return "Accessibility"
        case .unknown: return "Other"
        }
    }

    /// SF Symbol so the kind reads without relying on colour.
    var symbol: String {
        switch self {
        case .contrast: return "circle.lefthalf.filled"
        case .hitTarget: return "hand.tap"
        case .layout: return "rectangle.3.group"
        case .accessibility: return "figure.wave"
        case .unknown: return "questionmark.circle"
        }
    }
}

// MARK: - Finding severity

/// How serious a finding is. An `error` is what makes an audit FAIL and blocks
/// activation; a `warning` is advisory. `unknown` keeps decoding total. State is
/// shown with an icon + a label + a semantic token — never colour alone.
enum DesignAuditSeverity: String, Codable, Sendable, Equatable, CaseIterable {
    case error
    case warning
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = DesignAuditSeverity(rawValue: raw) ?? .unknown
    }

    var label: String {
        switch self {
        case .error: return "Error"
        case .warning: return "Warning"
        case .unknown: return "Note"
        }
    }

    var symbol: String {
        switch self {
        case .error: return "xmark.octagon.fill"
        case .warning: return "exclamationmark.triangle.fill"
        case .unknown: return "info.circle"
        }
    }

    /// Semantic tint — a token, never a bare literal. The glyph + label carry the
    /// same meaning so colour is never the only signal.
    var tint: Color {
        switch self {
        case .error: return GeneratedDesignTokens.colorStatusDanger
        case .warning: return GeneratedDesignTokens.colorStatusWarning
        case .unknown: return GeneratedDesignTokens.colorTextMuted
        }
    }

    /// Sort weight so errors surface above warnings above notes.
    var order: Int {
        switch self {
        case .error: return 0
        case .warning: return 1
        case .unknown: return 2
        }
    }
}

// MARK: - Finding (`findings[]` item)

/// One audit finding: its kind, its severity, a human `detail`, and the token
/// `ref` it points at (e.g. `color.text.muted`). Identifiable for `ForEach`.
struct DesignAuditFinding: Codable, Sendable, Equatable, Identifiable {
    let kind: DesignAuditFindingKind
    let severity: DesignAuditSeverity
    let detail: String
    let ref: String?

    /// Stable id from the (kind, severity, ref, detail) tuple so list diffing is
    /// stable without a server-supplied id.
    var id: String { "\(kind.rawValue)|\(severity.rawValue)|\(ref ?? "")|\(detail)" }

    enum CodingKeys: String, CodingKey {
        case kind, severity, detail, ref
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decodeIfPresent(DesignAuditFindingKind.self, forKey: .kind) ?? .unknown
        severity = try c.decodeIfPresent(DesignAuditSeverity.self, forKey: .severity) ?? .unknown
        detail = try c.decodeIfPresent(String.self, forKey: .detail) ?? ""
        ref = try c.decodeIfPresent(String.self, forKey: .ref)
    }

    init(
        kind: DesignAuditFindingKind,
        severity: DesignAuditSeverity,
        detail: String,
        ref: String? = nil
    ) {
        self.kind = kind
        self.severity = severity
        self.detail = detail
        self.ref = ref
    }

    /// The token the finding points at, falling back to an honest "—".
    var refDisplay: String { ref ?? "—" }
}

// MARK: - Audit report (`opensks.design-audit.v1`)

/// The result of `opensks design audit …`. `passed` is the overall verdict;
/// `blocksActivation` is whether a failing audit BLOCKS an activation (an `error`
/// finding does). The `findings` are grouped/sorted by the view.
struct DesignAuditReport: Codable, Sendable, Equatable {
    let schema: String
    let packageId: String
    let passed: Bool
    let blocksActivation: Bool
    let findings: [DesignAuditFinding]

    enum CodingKeys: String, CodingKey {
        case schema
        case packageId = "package_id"
        case passed
        case blocksActivation = "blocks_activation"
        case findings
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-audit.v1"
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
        passed = try c.decodeIfPresent(Bool.self, forKey: .passed) ?? false
        blocksActivation = try c.decodeIfPresent(Bool.self, forKey: .blocksActivation) ?? false
        findings = try c.decodeIfPresent([DesignAuditFinding].self, forKey: .findings) ?? []
    }

    init(
        schema: String = "opensks.design-audit.v1",
        packageId: String,
        passed: Bool,
        blocksActivation: Bool,
        findings: [DesignAuditFinding]
    ) {
        self.schema = schema
        self.packageId = packageId
        self.passed = passed
        self.blocksActivation = blocksActivation
        self.findings = findings
    }

    /// Findings that are `error`s — the ones that make the audit fail / block.
    var errors: [DesignAuditFinding] { findings.filter { $0.severity == .error } }
    /// Findings that are `warning`s — advisory.
    var warnings: [DesignAuditFinding] { findings.filter { $0.severity == .warning } }

    /// Findings grouped by kind, each group's findings sorted error → warning. The
    /// groups themselves are returned in `DesignAuditFindingKind.allCases` order so
    /// the Audit tab is stable.
    var groupedByKind: [(kind: DesignAuditFindingKind, findings: [DesignAuditFinding])] {
        DesignAuditFindingKind.allCases.compactMap { kind in
            let matching = findings
                .filter { $0.kind == kind }
                .sorted { $0.severity.order < $1.severity.order }
            return matching.isEmpty ? nil : (kind: kind, findings: matching)
        }
    }
}

// MARK: - Activate result (`opensks.design-activate.v1`)

/// The result of a SUCCESSFUL `opensks design activate …`. The audit passed and
/// `activated:true`; `previousActive` is the package that was active before (or
/// nil). A FAILING audit does NOT yield this — it yields a `DesignStudioError`
/// envelope instead, and the previously active package is left untouched.
struct DesignActivateResult: Codable, Sendable, Equatable {
    let schema: String
    let activated: Bool
    let packageId: String
    let previousActive: String?
    let auditPassed: Bool

    enum CodingKeys: String, CodingKey {
        case schema, activated
        case packageId = "package_id"
        case previousActive = "previous_active"
        case auditPassed = "audit_passed"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-activate.v1"
        activated = try c.decodeIfPresent(Bool.self, forKey: .activated) ?? false
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
        previousActive = try c.decodeIfPresent(String.self, forKey: .previousActive)
        auditPassed = try c.decodeIfPresent(Bool.self, forKey: .auditPassed) ?? false
    }

    init(
        schema: String = "opensks.design-activate.v1",
        activated: Bool,
        packageId: String,
        previousActive: String?,
        auditPassed: Bool
    ) {
        self.schema = schema
        self.activated = activated
        self.packageId = packageId
        self.previousActive = previousActive
        self.auditPassed = auditPassed
    }
}

// MARK: - Active status (`opensks.design-active.v1`)

/// The result of `opensks design active-status …` — which package (if any) is
/// currently active, and which revision it was activated at. Both are nullable.
struct DesignActiveStatus: Codable, Sendable, Equatable {
    let schema: String
    let activePackage: String?
    let activatedRevision: String?

    enum CodingKeys: String, CodingKey {
        case schema
        case activePackage = "active_package"
        case activatedRevision = "activated_revision"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-active.v1"
        activePackage = try c.decodeIfPresent(String.self, forKey: .activePackage)
        activatedRevision = try c.decodeIfPresent(String.self, forKey: .activatedRevision)
    }

    init(
        schema: String = "opensks.design-active.v1",
        activePackage: String? = nil,
        activatedRevision: String? = nil
    ) {
        self.schema = schema
        self.activePackage = activePackage
        self.activatedRevision = activatedRevision
    }

    static let none = DesignActiveStatus()

    /// The active package to display, falling back to an honest "None".
    var activePackageDisplay: String { activePackage ?? "None" }
    /// The activated revision to display, falling back to "—".
    var activatedRevisionDisplay: String { activatedRevision ?? "—" }
}

// MARK: - Revision state

/// The lifecycle state of a revision. `unknown` keeps decoding total. Each state
/// maps to an SF Symbol + a label + a semantic token so the state reads without
/// relying on colour.
enum DesignRevisionState: String, Codable, Sendable, Equatable, CaseIterable {
    case proposed
    case accepted
    case rejected
    case rolledBack = "rolled_back"
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = DesignRevisionState(rawValue: raw) ?? .unknown
    }

    var label: String {
        switch self {
        case .proposed: return "Proposed"
        case .accepted: return "Accepted"
        case .rejected: return "Rejected"
        case .rolledBack: return "Rolled back"
        case .unknown: return "Unknown"
        }
    }

    var symbol: String {
        switch self {
        case .proposed: return "hourglass"
        case .accepted: return "checkmark.seal.fill"
        case .rejected: return "xmark.circle.fill"
        case .rolledBack: return "arrow.uturn.backward.circle.fill"
        case .unknown: return "questionmark.circle"
        }
    }

    var tint: Color {
        switch self {
        case .proposed: return GeneratedDesignTokens.colorStatusRunning
        case .accepted: return GeneratedDesignTokens.colorStatusSuccess
        case .rejected: return GeneratedDesignTokens.colorStatusDanger
        case .rolledBack: return GeneratedDesignTokens.colorStatusWarning
        case .unknown: return GeneratedDesignTokens.colorTextMuted
        }
    }

    /// True for a revision that is still open to accept/reject (a proposed one).
    var isPending: Bool { self == .proposed }
}

// MARK: - Revision (`opensks.design-revision.v1`)

/// A revision of a package, always LINKED TO A PROOF (`proofRef`). `propose`
/// returns a `proposed` revision; `accept`/`reject`/`rollback` transition the
/// `state`. Identifiable for `ForEach`.
struct DesignRevision: Codable, Sendable, Equatable, Identifiable {
    let schema: String
    let revisionId: String
    let packageId: String
    let state: DesignRevisionState
    let proofRef: String?

    var id: String { revisionId }

    enum CodingKeys: String, CodingKey {
        case schema
        case revisionId = "revision_id"
        case packageId = "package_id"
        case state
        case proofRef = "proof_ref"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-revision.v1"
        revisionId = try c.decodeIfPresent(String.self, forKey: .revisionId) ?? ""
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
        state = try c.decodeIfPresent(DesignRevisionState.self, forKey: .state) ?? .unknown
        proofRef = try c.decodeIfPresent(String.self, forKey: .proofRef)
    }

    init(
        schema: String = "opensks.design-revision.v1",
        revisionId: String,
        packageId: String,
        state: DesignRevisionState,
        proofRef: String?
    ) {
        self.schema = schema
        self.revisionId = revisionId
        self.packageId = packageId
        self.state = state
        self.proofRef = proofRef
    }

    /// The proof/evidence ref to display, falling back to an honest "—". A revision
    /// is always linked to a proof; this never fabricates one.
    var proofRefDisplay: String { proofRef ?? "—" }
}

// MARK: - Error envelope (`opensks.design-error.v1`)

/// The structured error a FAILING `activate` (or any failed studio subcommand)
/// emits on a non-zero exit. `code:"audit_failed"` carries the blocking
/// `findings` so the UI can explain exactly why the activation was blocked.
struct DesignStudioErrorEnvelope: Codable, Sendable, Equatable {
    struct Body: Codable, Sendable, Equatable {
        let code: String
        let message: String?
        let findings: [DesignAuditFinding]

        enum CodingKeys: String, CodingKey {
            case code, message, findings
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            code = try c.decodeIfPresent(String.self, forKey: .code) ?? ""
            message = try c.decodeIfPresent(String.self, forKey: .message)
            findings = try c.decodeIfPresent([DesignAuditFinding].self, forKey: .findings) ?? []
        }

        init(code: String, message: String? = nil, findings: [DesignAuditFinding] = []) {
            self.code = code
            self.message = message
            self.findings = findings
        }
    }

    let schema: String
    let error: Body

    enum CodingKeys: String, CodingKey {
        case schema, error
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-error.v1"
        error = try c.decode(Body.self, forKey: .error)
    }

    init(schema: String = "opensks.design-error.v1", error: Body) {
        self.schema = schema
        self.error = error
    }
}

// MARK: - Save / compile / list results (PR-056 — DESIGN-002 / DESIGN-101)

/// The result of `opensks design save-tokens …` (`opensks.design-save-tokens.v1`):
/// how many token paths were updated, which were unknown (reported, never
/// created), the total token count, and the new content hash of tokens.json.
struct DesignSaveResult: Codable, Sendable, Equatable {
    let schema: String
    let packageId: String
    let updated: Int
    let unknownPaths: [String]
    let total: Int
    let contentHash: String

    enum CodingKeys: String, CodingKey {
        case schema
        case packageId = "package_id"
        case updated
        case unknownPaths = "unknown_paths"
        case total
        case contentHash = "content_hash"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-save-tokens.v1"
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
        updated = try c.decodeIfPresent(Int.self, forKey: .updated) ?? 0
        unknownPaths = try c.decodeIfPresent([String].self, forKey: .unknownPaths) ?? []
        total = try c.decodeIfPresent(Int.self, forKey: .total) ?? 0
        contentHash = try c.decodeIfPresent(String.self, forKey: .contentHash) ?? ""
    }

    init(
        schema: String = "opensks.design-save-tokens.v1",
        packageId: String,
        updated: Int,
        unknownPaths: [String] = [],
        total: Int,
        contentHash: String = ""
    ) {
        self.schema = schema
        self.packageId = packageId
        self.updated = updated
        self.unknownPaths = unknownPaths
        self.total = total
        self.contentHash = contentHash
    }

    /// A short human summary for the editor's save status line.
    var summary: String {
        let base = "Saved \(updated) token\(updated == 1 ? "" : "s")"
        return unknownPaths.isEmpty ? base : "\(base) · \(unknownPaths.count) unknown skipped"
    }
}

/// The result of `opensks design compile …` (`opensks.design-compile.v1`): whether
/// the package's tokens compile, the generated Swift byte count, and the compiler
/// error (if any). Compiling is isolated — it does NOT activate.
struct DesignCompileResult: Codable, Sendable, Equatable {
    let schema: String
    let packageId: String
    let ok: Bool
    let swiftBytes: Int
    let error: String?

    enum CodingKeys: String, CodingKey {
        case schema
        case packageId = "package_id"
        case ok
        case swiftBytes = "swift_bytes"
        case error
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-compile.v1"
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
        ok = try c.decodeIfPresent(Bool.self, forKey: .ok) ?? false
        swiftBytes = try c.decodeIfPresent(Int.self, forKey: .swiftBytes) ?? 0
        error = try c.decodeIfPresent(String.self, forKey: .error)
    }

    init(
        schema: String = "opensks.design-compile.v1",
        packageId: String,
        ok: Bool,
        swiftBytes: Int,
        error: String? = nil
    ) {
        self.schema = schema
        self.packageId = packageId
        self.ok = ok
        self.swiftBytes = swiftBytes
        self.error = error
    }
}

/// One entry in `opensks design list` (`opensks.design-package-list.v1`): a package
/// id, its display title, and whether it is the active package.
struct DesignPackageListEntry: Codable, Sendable, Equatable {
    let packageId: String
    let title: String
    let active: Bool

    enum CodingKeys: String, CodingKey {
        case packageId = "package_id"
        case title
        case active
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        packageId = try c.decodeIfPresent(String.self, forKey: .packageId) ?? ""
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        active = try c.decodeIfPresent(Bool.self, forKey: .active) ?? false
    }

    init(packageId: String, title: String, active: Bool) {
        self.packageId = packageId
        self.title = title
        self.active = active
    }
}

/// The `opensks design list` envelope wrapping the registry-driven catalog.
struct DesignPackageList: Codable, Sendable, Equatable {
    let schema: String
    let packages: [DesignPackageListEntry]

    enum CodingKeys: String, CodingKey {
        case schema, packages
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.design-package-list.v1"
        packages = try c.decodeIfPresent([DesignPackageListEntry].self, forKey: .packages) ?? []
    }

    init(schema: String = "opensks.design-package-list.v1", packages: [DesignPackageListEntry]) {
        self.schema = schema
        self.packages = packages
    }
}

// MARK: - Design token (the Tokens tab editor model)

/// One editable token in the Tokens tab: a dotted `path` (e.g. `color.text.muted`)
/// and its current `value`. The editor lists every path/value and lets the value
/// be edited locally. The studio is the source of truth; this is the in-UI draft.
struct DesignTokenEntry: Sendable, Equatable, Identifiable {
    let path: String
    var value: String

    var id: String { path }

    init(path: String, value: String) {
        self.path = path
        self.value = value
    }

    /// True when the value parses as a `#RRGGBB`/`#RRGGBBAA` colour, so the editor
    /// can show a swatch alongside the value.
    var isColor: Bool {
        let raw = value.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        guard raw.count == 6 || raw.count == 8 else { return false }
        return raw.allSatisfy { $0.isHexDigit }
    }
}

// MARK: - Catalog package (the sidebar catalog model)

/// One package in the catalog (the Design route sidebar). Carries the small set of
/// tokens it ships so the Tokens tab has content without an extra round-trip; the
/// audit / activation / revisions are loaded on demand via the service.
struct DesignPackage: Sendable, Equatable, Identifiable {
    let packageId: String
    let title: String
    let tokens: [DesignTokenEntry]

    var id: String { packageId }

    init(packageId: String, title: String? = nil, tokens: [DesignTokenEntry] = []) {
        self.packageId = packageId
        self.title = title ?? packageId
        self.tokens = tokens
    }
}

// MARK: - Component state matrix model

/// The interaction states the Components tab previews each control across. Default
/// is the resting state; the rest mirror the app's real control states.
enum DesignControlState: String, CaseIterable, Sendable, Equatable, Identifiable {
    case defaultState
    case hover
    case pressed
    case disabled
    case focused

    var id: String { rawValue }

    var label: String {
        switch self {
        case .defaultState: return "Default"
        case .hover: return "Hover"
        case .pressed: return "Pressed"
        case .disabled: return "Disabled"
        case .focused: return "Focused"
        }
    }
}
