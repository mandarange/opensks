// DesignImportService.swift — the boundary between the design-import UI and the
// bundled `opensks design import|import-approve|import-reject|import-status`
// subcommands (PR-039).
//
// `DesignImportService` is the async-throwing protocol the store talks to. The
// ENTIRE surface is LOCAL: `import` quarantines + validates a local dir / .zip
// (it does NOT promote), `approve` promotes a quarantined package to the registry
// ONLY after the operator explicitly approves (a human-review step), `reject`
// safely deletes a quarantine, and `status` lists the quarantine. There is NO
// upload method, NO network method, NO call to any undocumented external API —
// nothing here sends the user's data anywhere. The promotion is gated on an
// explicit `approve(quarantineId:)` call; `import(...)` never promotes.
//
// The LIVE implementation routes CLI execution through ProcessSupervisor and
// decodes the shared snake_case JSON. A MOCK drives the tests without touching
// disk or spawning a process and COUNTS calls so the quarantine → human-review →
// promote flow is observable — in particular a test can assert that `import(...)`
// does NOT call `approve(...)`.

import Foundation

// MARK: - Errors

/// Errors surfaced by the design-import service.
enum DesignImportServiceError: Error, Equatable {
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)
    /// A non-zero exit with a decodable message.
    case service(message: String)
}

// MARK: - Protocol (LOCAL-ONLY surface: import / approve / reject / status)

/// The design-import boundary. The whole surface is LOCAL and human-reviewed:
///   • `importLocal(source:kind:)` — quarantine + validate a LOCAL dir / .zip. It
///     NEVER promotes; the result is `quarantined` or `rejected`.
///   • `approve(quarantineId:)` — promote a quarantined package to the registry,
///     ONLY ever called from an explicit operator approval (human review).
///   • `reject(quarantineId:)` — safely delete a quarantine directory.
///   • `status()` — list the current quarantine.
///
/// There is deliberately NO upload / sync / network method on this protocol: the
/// import never sends the user's data anywhere. (A test pins this surface.)
protocol DesignImportService: Sendable {
    /// `opensks design import --workspace <p> --source <path> --kind local|archive`.
    /// Quarantines + validates a LOCAL dir / .zip. Does NOT promote.
    func importLocal(source: String, kind: DesignImportKind) async throws -> DesignImportResult

    /// `opensks design import-approve --workspace <p> --quarantine <id>`. The
    /// HUMAN-REVIEW promotion to `.opensks/design-systems/<id>/` after a
    /// RE-validation. Only ever invoked from an explicit operator approval.
    func approve(quarantineId: String) async throws -> DesignImportApproveResult

    /// `opensks design import-reject --workspace <p> --quarantine <id>`. Safely
    /// deletes the quarantine directory (the candidate is discarded, never promoted).
    func reject(quarantineId: String) async throws -> DesignImportRejectResult

    /// `opensks design import-status --workspace <p>`. The current quarantine list.
    func status() async throws -> DesignImportStatusResult
}

// MARK: - Live (CLI-backed) implementation

/// Runs the bundled `opensks design import…` subcommands through the shared
/// ProcessSupervisor; decoding maps the shared snake_case contract. LOCAL only —
/// there is no upload/network path anywhere in here.
struct LiveDesignImportService: DesignImportService {
    let cli: URL
    let workspace: URL
    private let supervisor = ProcessSupervisor()

    func importLocal(source: String, kind: DesignImportKind) async throws -> DesignImportResult {
        let result = try await run(args: [
            "design", "import",
            "--workspace", workspace.path,
            "--source", source,
            "--kind", kind.cliValue
        ])
        return try Self.decodeOrThrow(result, as: DesignImportResult.self)
    }

    func approve(quarantineId: String) async throws -> DesignImportApproveResult {
        let result = try await run(args: [
            "design", "import-approve",
            "--workspace", workspace.path,
            "--quarantine", quarantineId
        ])
        return try Self.decodeOrThrow(result, as: DesignImportApproveResult.self)
    }

    func reject(quarantineId: String) async throws -> DesignImportRejectResult {
        let result = try await run(args: [
            "design", "import-reject",
            "--workspace", workspace.path,
            "--quarantine", quarantineId
        ])
        return try Self.decodeOrThrow(result, as: DesignImportRejectResult.self)
    }

    func status() async throws -> DesignImportStatusResult {
        let result = try await run(args: [
            "design", "import-status",
            "--workspace", workspace.path
        ])
        return try Self.decodeOrThrow(result, as: DesignImportStatusResult.self)
    }

    // MARK: Process plumbing

    private struct ProcessResult {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
    }

    private func run(args: [String]) async throws -> ProcessResult {
        do {
            let result = try await supervisor.run(ProcessSupervisor.Spec(
                executable: cli,
                arguments: args,
                workingDirectory: workspace,
                timeoutSeconds: 120
            ))
            if result.timedOut {
                throw DesignImportServiceError.transport(
                    message: "opensks design timed out"
                )
            }
            return ProcessResult(
                exitCode: result.exitCode,
                stdout: result.stdout,
                stderr: result.stderr
            )
        } catch let error as DesignImportServiceError {
            throw error
        } catch {
            throw DesignImportServiceError.transport(
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
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if result.exitCode == 0 {
            throw DesignImportServiceError.transport(
                message: "could not decode \(T.self) from opensks design output"
            )
        }
        throw DesignImportServiceError.service(
            message: stderrText.isEmpty
                ? "opensks design exited \(result.exitCode)"
                : "opensks design exited \(result.exitCode): \(stderrText)"
        )
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory design-import service for tests. Returns scriptable quarantine /
/// reject / approve results and COUNTS each call so the store's quarantine →
/// human-review → promote flow is observable: a test can assert that `import(...)`
/// produces a QUARANTINED entry and does NOT call `approve(...)`, that a REJECTED
/// import cannot be approved, and that `reject(...)` removes the entry. It NEVER
/// touches disk, spawns a process, or contacts any network — there is no upload
/// surface to drive — so tests are hermetic.
final class MockDesignImportService: DesignImportService, @unchecked Sendable {
    private let lock = NSLock()

    /// The result the NEXT `importLocal(...)` returns. When nil, the mock
    /// synthesizes a deterministic quarantined result from the source + kind.
    private var cannedImport: DesignImportResult?
    /// The canned approve result (defaults to a successful promotion).
    private var cannedApprove: DesignImportApproveResult?
    /// The canned reject result (defaults to a successful deletion).
    private var cannedReject: DesignImportRejectResult?
    /// The canned status listing returned by `status()`.
    private var cannedStatus: DesignImportStatusResult?
    /// When set, the NEXT call of the named method throws this error once.
    private var failNextImport: DesignImportServiceError?
    private var failNextApprove: DesignImportServiceError?

    private(set) var importCalls: [(source: String, kind: DesignImportKind)] = []
    private(set) var approveCalls: [String] = []
    private(set) var rejectCalls: [String] = []
    private(set) var statusCallCount = 0

    init() {}

    // MARK: Test setup

    /// Script the result the next `importLocal(...)` returns.
    func setImportResult(_ result: DesignImportResult) {
        lock.lock(); defer { lock.unlock() }
        cannedImport = result
    }

    /// Script the approve result.
    func setApproveResult(_ result: DesignImportApproveResult) {
        lock.lock(); defer { lock.unlock() }
        cannedApprove = result
    }

    /// Script the reject result.
    func setRejectResult(_ result: DesignImportRejectResult) {
        lock.lock(); defer { lock.unlock() }
        cannedReject = result
    }

    /// Script the status listing.
    func setStatusResult(_ result: DesignImportStatusResult) {
        lock.lock(); defer { lock.unlock() }
        cannedStatus = result
    }

    /// Arm the NEXT `importLocal(...)` to throw once.
    func armImportFailure(_ error: DesignImportServiceError) {
        lock.lock(); defer { lock.unlock() }
        failNextImport = error
    }

    /// Arm the NEXT `approve(...)` to throw once.
    func armApproveFailure(_ error: DesignImportServiceError) {
        lock.lock(); defer { lock.unlock() }
        failNextApprove = error
    }

    // MARK: DesignImportService — each method funnels through a fully-scoped
    // SYNCHRONOUS critical section (no lock held across an await).

    func importLocal(source: String, kind: DesignImportKind) async throws -> DesignImportResult {
        try importLocked(source: source, kind: kind)
    }

    func approve(quarantineId: String) async throws -> DesignImportApproveResult {
        try approveLocked(quarantineId: quarantineId)
    }

    func reject(quarantineId: String) async throws -> DesignImportRejectResult {
        rejectLocked(quarantineId: quarantineId)
    }

    func status() async throws -> DesignImportStatusResult {
        statusLocked()
    }

    // MARK: Synchronous critical sections

    private func importLocked(source: String, kind: DesignImportKind) throws -> DesignImportResult {
        lock.lock(); defer { lock.unlock() }
        importCalls.append((source: source, kind: kind))
        if let error = failNextImport {
            failNextImport = nil
            throw error
        }
        if let canned = cannedImport { return canned }
        return DesignImportResult(
            quarantineId: "quarantine-\(importCalls.count)",
            status: .quarantined,
            provenance: DesignImportProvenance(source: source, license: nil, commit: nil),
            fileCount: 1,
            byteSize: 1024,
            rejectedReason: nil
        )
    }

    private func approveLocked(quarantineId: String) throws -> DesignImportApproveResult {
        lock.lock(); defer { lock.unlock() }
        approveCalls.append(quarantineId)
        if let error = failNextApprove {
            failNextApprove = nil
            throw error
        }
        if let canned = cannedApprove { return canned }
        return DesignImportApproveResult(promoted: true, packageId: "package-\(quarantineId)")
    }

    private func rejectLocked(quarantineId: String) -> DesignImportRejectResult {
        lock.lock(); defer { lock.unlock() }
        rejectCalls.append(quarantineId)
        return cannedReject ?? DesignImportRejectResult(rejected: true, deleted: true)
    }

    private func statusLocked() -> DesignImportStatusResult {
        lock.lock(); defer { lock.unlock() }
        statusCallCount += 1
        return cannedStatus ?? .empty
    }
}
