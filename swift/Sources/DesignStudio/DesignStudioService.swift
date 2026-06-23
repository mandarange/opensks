// DesignStudioService.swift — the boundary between the Design Studio UI and the
// bundled `opensks design audit|activate|active-status|revision-…` subcommands
// (PR-040). These are SUBCOMMANDS of the EXISTING `design` verb.
//
// `DesignStudioService` is the async-throwing protocol the store talks to:
//   • `audit(packageId:)`         — run the audit rules over a package's tokens /
//                                    components and return the report.
//   • `activate(packageId:)`      — ATOMIC: audit first; a FAILING audit BLOCKS the
//                                    activation (throws `.auditFailed` carrying the
//                                    blocking findings) and leaves the previously
//                                    active package in place. A PASSING audit
//                                    activates and returns the result.
//   • `activeStatus()`            — which package (if any) is currently active.
//   • `proposeRevision(packageId:)` / `acceptRevision|rejectRevision|rollbackRevision(revisionId:)`
//                                    — each revision is LINKED TO A PROOF.
//
// The LIVE implementation shells the bundled CLI exactly like LiveGitService /
// LiveDesignImportService (an off-main detached task, decode the shared snake_case
// JSON). A non-zero exit carrying the `opensks.design-error.v1` envelope is mapped
// to a TYPED error (`auditFailed`) so the store can react precisely — surfacing
// the blocked state and NOT changing the shown active package. A MOCK drives the
// tests without touching disk or spawning a process and COUNTS calls so the
// audit-blocks-activation invariant is observable.

import Foundation

// MARK: - Errors

/// Errors surfaced by the design studio service.
enum DesignStudioServiceError: Error, Equatable {
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)
    /// A non-zero exit with a decodable message.
    case service(message: String)
    /// `audit_failed` — an `activate` was BLOCKED because its audit failed. Carries
    /// the blocking findings so the UI explains exactly why, and the store leaves
    /// the previously active package in place (the activation did NOT happen).
    case auditFailed(findings: [DesignAuditFinding])
}

// MARK: - Protocol

/// The Design Studio boundary — all SUBCOMMANDS of the `design` verb:
///   • `audit` runs the rules over a package; `activate` is ATOMIC (audit gates it);
///   • `activeStatus` reports the active package + revision;
///   • the revision lifecycle (propose / accept / reject / rollback) is proof-linked.
protocol DesignStudioService: Sendable {
    /// `opensks design audit --workspace <p> --package <id>`.
    func audit(packageId: String) async throws -> DesignAuditReport

    /// `opensks design activate --workspace <p> --package <id>`. ATOMIC: the CLI
    /// audits first; a failing audit BLOCKS the activation. On a block this throws
    /// `.auditFailed(findings:)` and the previously active package is unchanged.
    func activate(packageId: String) async throws -> DesignActivateResult

    /// `opensks design active-status --workspace <p>`.
    func activeStatus() async throws -> DesignActiveStatus

    /// `opensks design revision-propose --workspace <p> --package <id>`. The new
    /// revision is `proposed` and LINKED TO A PROOF (`proof_ref`).
    func proposeRevision(packageId: String) async throws -> DesignRevision

    /// `opensks design revision-accept --workspace <p> --revision <id>`.
    func acceptRevision(revisionId: String) async throws -> DesignRevision
    /// `opensks design revision-reject --workspace <p> --revision <id>`.
    func rejectRevision(revisionId: String) async throws -> DesignRevision
    /// `opensks design revision-rollback --workspace <p> --revision <id>`.
    func rollbackRevision(revisionId: String) async throws -> DesignRevision

    /// `opensks design save-tokens --workspace <p> --package <id>` (draft JSON on
    /// stdin). Persists edited token values into the package's tokens.json — only
    /// existing paths are updated, unknown ones reported (DESIGN-002).
    func saveTokens(packageId: String, tokens: [DesignTokenEntry]) async throws -> DesignSaveResult

    /// `opensks design compile --workspace <p> --package <id>`. Compiles/validates
    /// the package's tokens in isolation (no activation), for editor feedback.
    func compile(packageId: String) async throws -> DesignCompileResult

    /// `opensks design list --workspace <p>`. The registry-driven catalog (DESIGN-101).
    func listPackages() async throws -> [DesignPackageListEntry]
}

/// The stdin payload for `design save-tokens`: the edited token paths/values.
private struct DesignSaveTokensRequest: Encodable {
    struct Entry: Encodable {
        let path: String
        let value: String
    }
    let tokens: [Entry]
}

// MARK: - Live (CLI-backed) implementation

/// Shells the bundled `opensks design …` studio subcommands. Process work runs on
/// a detached cooperative task; decoding maps the shared snake_case contract. A
/// failing `activate` maps its `opensks.design-error.v1` envelope to the typed
/// `.auditFailed` so the store keeps the previous active package in place.
struct LiveDesignStudioService: DesignStudioService {
    let cli: URL
    let workspace: URL

    func audit(packageId: String) async throws -> DesignAuditReport {
        let result = try await run(args: [
            "design", "audit", "--workspace", workspace.path, "--package", packageId
        ])
        return try Self.decodeOrThrow(result, as: DesignAuditReport.self)
    }

    func activate(packageId: String) async throws -> DesignActivateResult {
        let result = try await run(args: [
            "design", "activate", "--workspace", workspace.path, "--package", packageId
        ])
        return try Self.decodeActivate(result)
    }

    func activeStatus() async throws -> DesignActiveStatus {
        let result = try await run(args: [
            "design", "active-status", "--workspace", workspace.path
        ])
        return try Self.decodeOrThrow(result, as: DesignActiveStatus.self)
    }

    func proposeRevision(packageId: String) async throws -> DesignRevision {
        let result = try await run(args: [
            "design", "revision-propose", "--workspace", workspace.path, "--package", packageId
        ])
        return try Self.decodeOrThrow(result, as: DesignRevision.self)
    }

    func acceptRevision(revisionId: String) async throws -> DesignRevision {
        try await revisionStep("revision-accept", revisionId: revisionId)
    }

    func rejectRevision(revisionId: String) async throws -> DesignRevision {
        try await revisionStep("revision-reject", revisionId: revisionId)
    }

    func rollbackRevision(revisionId: String) async throws -> DesignRevision {
        try await revisionStep("revision-rollback", revisionId: revisionId)
    }

    private func revisionStep(_ verb: String, revisionId: String) async throws -> DesignRevision {
        let result = try await run(args: [
            "design", verb, "--workspace", workspace.path, "--revision", revisionId
        ])
        return try Self.decodeOrThrow(result, as: DesignRevision.self)
    }

    func saveTokens(packageId: String, tokens: [DesignTokenEntry]) async throws -> DesignSaveResult {
        let payload = DesignSaveTokensRequest(
            tokens: tokens.map { DesignSaveTokensRequest.Entry(path: $0.path, value: $0.value) }
        )
        let body = try JSONEncoder().encode(payload)
        let result = try await run(
            args: ["design", "save-tokens", "--workspace", workspace.path, "--package", packageId],
            stdin: body
        )
        return try Self.decodeOrThrow(result, as: DesignSaveResult.self)
    }

    func compile(packageId: String) async throws -> DesignCompileResult {
        let result = try await run(args: [
            "design", "compile", "--workspace", workspace.path, "--package", packageId
        ])
        return try Self.decodeOrThrow(result, as: DesignCompileResult.self)
    }

    func listPackages() async throws -> [DesignPackageListEntry] {
        let result = try await run(args: [
            "design", "list", "--workspace", workspace.path
        ])
        return try Self.decodeOrThrow(result, as: DesignPackageList.self).packages
    }

    // MARK: Process plumbing (mirrors LiveGitService / LiveDesignImportService)

    private struct ProcessResult {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
    }

    /// Shared child-process runner (concurrent drain + cancel-kill, §19.2).
    private let supervisor = ProcessSupervisor()

    private func run(args: [String], stdin: Data? = nil) async throws -> ProcessResult {
        do {
            let result = try await supervisor.run(
                ProcessSupervisor.Spec(
                    executable: cli,
                    arguments: args,
                    workingDirectory: workspace,
                    stdin: stdin
                )
            )
            return ProcessResult(
                exitCode: result.exitCode,
                stdout: result.stdout,
                stderr: result.stderr
            )
        } catch {
            throw DesignStudioServiceError.transport(
                message: "could not launch opensks design: \(error.localizedDescription)"
            )
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
        // A structured error envelope may accompany a non-zero exit on either stream.
        if let envelope = decodeErrorEnvelope(result.stdout, decoder)
            ?? decodeErrorEnvelope(result.stderr, decoder) {
            throw mapError(envelope)
        }
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if result.exitCode == 0 {
            throw DesignStudioServiceError.transport(
                message: "could not decode \(T.self) from opensks design output"
            )
        }
        throw DesignStudioServiceError.service(
            message: stderrText.isEmpty
                ? "opensks design exited \(result.exitCode)"
                : "opensks design exited \(result.exitCode): \(stderrText)"
        )
    }

    /// Decode an `activate` result. A SUCCESS is `opensks.design-activate.v1`; a
    /// BLOCKED activation is a non-zero exit carrying `opensks.design-error.v1` with
    /// `code:"audit_failed"`, mapped to the typed `.auditFailed` so the store leaves
    /// the previous active package in place.
    private static func decodeActivate(_ result: ProcessResult) throws -> DesignActivateResult {
        let decoder = JSONDecoder()
        if result.exitCode == 0,
           let value = try? decoder.decode(DesignActivateResult.self, from: result.stdout) {
            return value
        }
        if let envelope = decodeErrorEnvelope(result.stdout, decoder)
            ?? decodeErrorEnvelope(result.stderr, decoder) {
            throw mapError(envelope)
        }
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        throw DesignStudioServiceError.service(
            message: stderrText.isEmpty
                ? "opensks design activate exited \(result.exitCode)"
                : "opensks design activate exited \(result.exitCode): \(stderrText)"
        )
    }

    private static func decodeErrorEnvelope(_ data: Data, _ decoder: JSONDecoder) -> DesignStudioErrorEnvelope? {
        guard !data.isEmpty else { return nil }
        return try? decoder.decode(DesignStudioErrorEnvelope.self, from: data)
    }

    /// Map a decoded `opensks.design-error.v1` to a typed error. `audit_failed`
    /// becomes `.auditFailed(findings:)`; anything else falls back to `.service`.
    static func mapError(_ envelope: DesignStudioErrorEnvelope) -> DesignStudioServiceError {
        switch envelope.error.code {
        case "audit_failed":
            return .auditFailed(findings: envelope.error.findings)
        default:
            return .service(message: envelope.error.message ?? "opensks design error: \(envelope.error.code)")
        }
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory design studio service for tests. Returns scriptable audit /
/// activate / active-status / revision results and COUNTS each call so the store's
/// invariants are observable: a FAILING audit BLOCKS activation (activate throws
/// `.auditFailed` and the active status is NOT updated), a PASSING audit activates,
/// and the revision lifecycle transitions predictably. It NEVER touches disk or
/// spawns a process — tests are hermetic.
final class MockDesignStudioService: DesignStudioService, @unchecked Sendable {
    private let lock = NSLock()

    /// Audit reports keyed by package id. When none is scripted the mock returns a
    /// clean PASS so the default path activates.
    private var auditByPackage: [String: DesignAuditReport] = [:]
    /// The active status the mock reports. `activate` updates this ONLY on a passing
    /// audit, modelling the CLI's atomic behaviour.
    private var active = DesignActiveStatus.none
    /// Canned revision results keyed by the step name+id (so a test can script a
    /// specific proof_ref / state); otherwise the mock synthesizes deterministically.
    private var cannedRevisions: [String: DesignRevision] = [:]

    private(set) var auditCalls: [String] = []
    private(set) var activateCalls: [String] = []
    private(set) var activeStatusCallCount = 0
    private(set) var proposeCalls: [String] = []
    private(set) var acceptCalls: [String] = []
    private(set) var rejectCalls: [String] = []
    private(set) var rollbackCalls: [String] = []
    private(set) var saveCalls: [(packageId: String, tokens: [DesignTokenEntry])] = []
    private(set) var compileCalls: [String] = []
    private(set) var listCallCount = 0
    /// The packages the registry listing returns (settable per test).
    private var registryPackages: [DesignPackageListEntry] = []

    init() {}

    // MARK: Test setup

    /// Script the audit report a package returns (e.g. a failing/blocking one).
    func setAudit(_ report: DesignAuditReport, forPackage packageId: String) {
        lock.lock(); defer { lock.unlock() }
        auditByPackage[packageId] = report
    }

    /// Seed the initially-active package (e.g. to prove a blocked activation leaves
    /// it in place).
    func setActive(_ status: DesignActiveStatus) {
        lock.lock(); defer { lock.unlock() }
        active = status
    }

    /// Script the revision returned by a propose step for a package.
    func setProposedRevision(_ revision: DesignRevision, forPackage packageId: String) {
        lock.lock(); defer { lock.unlock() }
        cannedRevisions["propose|\(packageId)"] = revision
    }

    // MARK: DesignStudioService — each method funnels through a fully-scoped
    // SYNCHRONOUS critical section (no lock held across an await).

    func audit(packageId: String) async throws -> DesignAuditReport {
        auditLocked(packageId: packageId)
    }

    func activate(packageId: String) async throws -> DesignActivateResult {
        try activateLocked(packageId: packageId)
    }

    func activeStatus() async throws -> DesignActiveStatus {
        activeStatusLocked()
    }

    func proposeRevision(packageId: String) async throws -> DesignRevision {
        proposeLocked(packageId: packageId)
    }

    func acceptRevision(revisionId: String) async throws -> DesignRevision {
        transitionLocked(revisionId: revisionId, to: .accepted, record: { self.acceptCalls.append($0) })
    }

    func rejectRevision(revisionId: String) async throws -> DesignRevision {
        transitionLocked(revisionId: revisionId, to: .rejected, record: { self.rejectCalls.append($0) })
    }

    func rollbackRevision(revisionId: String) async throws -> DesignRevision {
        transitionLocked(revisionId: revisionId, to: .rolledBack, record: { self.rollbackCalls.append($0) })
    }

    /// Seed the packages the registry listing returns (DESIGN-101 test setup).
    func setRegistryPackages(_ packages: [DesignPackageListEntry]) {
        lock.lock(); defer { lock.unlock() }
        registryPackages = packages
    }

    func saveTokens(packageId: String, tokens: [DesignTokenEntry]) async throws -> DesignSaveResult {
        saveTokensLocked(packageId: packageId, tokens: tokens)
    }

    func compile(packageId: String) async throws -> DesignCompileResult {
        compileLocked(packageId: packageId)
    }

    func listPackages() async throws -> [DesignPackageListEntry] {
        listPackagesLocked()
    }

    // MARK: Synchronous critical sections

    private func saveTokensLocked(packageId: String, tokens: [DesignTokenEntry]) -> DesignSaveResult {
        lock.lock(); defer { lock.unlock() }
        saveCalls.append((packageId: packageId, tokens: tokens))
        return DesignSaveResult(
            packageId: packageId,
            updated: tokens.count,
            unknownPaths: [],
            total: tokens.count,
            contentHash: "mock"
        )
    }

    private func compileLocked(packageId: String) -> DesignCompileResult {
        lock.lock(); defer { lock.unlock() }
        compileCalls.append(packageId)
        return DesignCompileResult(packageId: packageId, ok: true, swiftBytes: 100, error: nil)
    }

    private func listPackagesLocked() -> [DesignPackageListEntry] {
        lock.lock(); defer { lock.unlock() }
        listCallCount += 1
        return registryPackages
    }

    private func auditLocked(packageId: String) -> DesignAuditReport {
        lock.lock(); defer { lock.unlock() }
        auditCalls.append(packageId)
        return scriptedAudit(packageId)
    }

    private func activateLocked(packageId: String) throws -> DesignActivateResult {
        lock.lock(); defer { lock.unlock() }
        activateCalls.append(packageId)
        // ATOMIC: audit gates the activation. A failing/blocking audit throws and
        // does NOT change the active package — exactly the CLI's behaviour.
        let report = scriptedAudit(packageId)
        if !report.passed && report.blocksActivation {
            throw DesignStudioServiceError.auditFailed(findings: report.findings)
        }
        let previous = active.activePackage
        active = DesignActiveStatus(activePackage: packageId, activatedRevision: active.activatedRevision)
        return DesignActivateResult(
            activated: true,
            packageId: packageId,
            previousActive: previous,
            auditPassed: true
        )
    }

    private func activeStatusLocked() -> DesignActiveStatus {
        lock.lock(); defer { lock.unlock() }
        activeStatusCallCount += 1
        return active
    }

    private func proposeLocked(packageId: String) -> DesignRevision {
        lock.lock(); defer { lock.unlock() }
        proposeCalls.append(packageId)
        if let canned = cannedRevisions["propose|\(packageId)"] { return canned }
        return DesignRevision(
            revisionId: "rev-\(proposeCalls.count)",
            packageId: packageId,
            state: .proposed,
            proofRef: "proof://\(packageId)/rev-\(proposeCalls.count)"
        )
    }

    private func transitionLocked(
        revisionId: String,
        to state: DesignRevisionState,
        record: (String) -> Void
    ) -> DesignRevision {
        lock.lock(); defer { lock.unlock() }
        record(revisionId)
        return DesignRevision(
            revisionId: revisionId,
            packageId: "",
            state: state,
            proofRef: "proof://\(revisionId)/\(state.rawValue)"
        )
    }

    /// The scripted audit for a package, or a clean PASS by default.
    private func scriptedAudit(_ packageId: String) -> DesignAuditReport {
        auditByPackage[packageId] ?? DesignAuditReport(
            packageId: packageId,
            passed: true,
            blocksActivation: false,
            findings: []
        )
    }
}
