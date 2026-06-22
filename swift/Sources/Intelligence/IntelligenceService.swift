// IntelligenceService.swift — the boundary between the Project Intelligence UI
// and the bundled `opensks intel …` subcommands (PR-041). These are SUBCOMMANDS
// of a NEW `intel` verb:
//
//   • `opensks intel freshness        --workspace <p>`                      — the CURRENT stamp.
//   • `opensks intel freshness-check  --workspace <p> [--head <h>] [--worktree <h>] [--index <h>]`
//                                                                            — compare a STAMPED freshness against current.
//   • `opensks intel codegraph-query  --workspace <p> --query <text> [--limit N] [--offset N]`
//                                                                            — a PAGED window of the code graph.
//   • `opensks intel glossary         --workspace <p>`
//   • `opensks intel architecture     --workspace <p>`
//
// The LIVE implementation shells the bundled CLI exactly like LiveGitService /
// LiveConversationService (an off-main detached task, decode the shared
// snake_case JSON). A MOCK drives the tests without touching disk or spawning a
// process and COUNTS calls so the freshness + paging behaviour is observable.

import Foundation

// MARK: - Errors

enum IntelligenceServiceError: LocalizedError, Equatable {
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)
    /// A non-zero exit with a decodable message.
    case service(message: String)

    var errorDescription: String? {
        switch self {
        case .transport(let message): return message
        case .service(let message): return message
        }
    }
}

// MARK: - Protocol

/// The Project Intelligence boundary — all SUBCOMMANDS of the `intel` verb. Every
/// listing carries a freshness stamp; the code graph is PAGED (limit/offset) so a
/// large graph is never loaded whole.
protocol IntelligenceService: Sendable {
    /// `opensks intel freshness --workspace <p>` — the CURRENT freshness stamp.
    func freshness() async throws -> IntelFreshnessStamp

    /// `opensks intel freshness-check --workspace <p> [--head <h>] [--worktree <h>] [--index <h>]`
    /// — compare a STAMPED freshness against current. `fresh == true` ONLY when
    /// every provided stamp matches current; any divergence (or a missing/unknown
    /// stamp) is reported as NOT fresh with a reason.
    func freshnessCheck(stamp: IntelFreshnessStamp) async throws -> IntelFreshnessCheck

    /// `opensks intel codegraph-query --workspace <p> --query <text> [--limit N] [--offset N]`
    /// — ONE PAGE of the code graph. The whole graph is never requested at once.
    func codeGraphQuery(query: String, limit: Int, offset: Int) async throws -> IntelCodeGraphPage

    /// `opensks intel glossary --workspace <p>`.
    func glossary() async throws -> IntelGlossary

    /// `opensks intel architecture --workspace <p>`.
    func architecture() async throws -> IntelArchitecture
}

// MARK: - Live (CLI-backed) implementation

/// Shells the bundled `opensks intel …` subcommands. Process work runs on a
/// detached cooperative task; decoding maps the shared snake_case contract via
/// `JSONDecoder.opensks` (`.convertFromSnakeCase`), exactly like the rest of the app.
struct LiveIntelligenceService: IntelligenceService {
    let cli: URL
    let workspace: URL

    func freshness() async throws -> IntelFreshnessStamp {
        let result = try await run(args: ["intel", "freshness", "--workspace", workspace.path])
        return try Self.decode(result, as: IntelFreshnessStamp.self)
    }

    func freshnessCheck(stamp: IntelFreshnessStamp) async throws -> IntelFreshnessCheck {
        var args = ["intel", "freshness-check", "--workspace", workspace.path]
        // A nil head_hash (not a repo / no commit) is passed only when present;
        // its ABSENCE is itself meaningful to the check (an unknown stamp is never
        // treated as fresh on the CLI side).
        if let head = stamp.headHash { args.append(contentsOf: ["--head", head]) }
        args.append(contentsOf: ["--worktree", stamp.worktreeHash])
        args.append(contentsOf: ["--index", stamp.indexHash])
        let result = try await run(args: args)
        return try Self.decode(result, as: IntelFreshnessCheck.self)
    }

    func codeGraphQuery(query: String, limit: Int, offset: Int) async throws -> IntelCodeGraphPage {
        let result = try await run(args: [
            "intel", "codegraph-query", "--workspace", workspace.path,
            "--query", query, "--limit", String(limit), "--offset", String(offset)
        ])
        return try Self.decode(result, as: IntelCodeGraphPage.self)
    }

    func glossary() async throws -> IntelGlossary {
        let result = try await run(args: ["intel", "glossary", "--workspace", workspace.path])
        return try Self.decode(result, as: IntelGlossary.self)
    }

    func architecture() async throws -> IntelArchitecture {
        let result = try await run(args: ["intel", "architecture", "--workspace", workspace.path])
        return try Self.decode(result, as: IntelArchitecture.self)
    }

    // MARK: Process plumbing (mirrors LiveGitService / LiveDesignStudioService)

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
                    continuation.resume(throwing: IntelligenceServiceError.transport(
                        message: "could not launch opensks intel: \(error.localizedDescription)"
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

    private static func decode<T: Decodable>(_ result: ProcessResult, as type: T.Type) throws -> T {
        if result.exitCode == 0, let value = try? JSONDecoder.opensks.decode(T.self, from: result.stdout) {
            return value
        }
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if result.exitCode == 0 {
            throw IntelligenceServiceError.transport(
                message: "could not decode \(T.self) from opensks intel output"
            )
        }
        throw IntelligenceServiceError.service(
            message: stderrText.isEmpty
                ? "opensks intel exited \(result.exitCode)"
                : "opensks intel exited \(result.exitCode): \(stderrText)"
        )
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory intelligence service for tests. Returns scriptable freshness /
/// freshness-check / code-graph pages / glossary / architecture and COUNTS each
/// call so the store's behaviour is observable:
///   • the badge flips to STALE when `freshness-check` reports a diverged current;
///   • a matching stamp shows FRESH;
///   • the code-graph explorer requests pages (limit/offset) and the mock records
///     each page request so a test can prove the store never asks for the whole graph.
/// It NEVER touches disk or spawns a process — tests are hermetic.
final class MockIntelligenceService: IntelligenceService, @unchecked Sendable {
    private let lock = NSLock()

    /// The CURRENT stamp the mock reports from `freshness()` and embeds as the
    /// `current` block of a `freshness-check`. A test moves this to model the
    /// workspace changing under previously-loaded data.
    private var current: IntelFreshnessStamp
    /// The architecture / glossary the mock returns (with their loaded-at stamp).
    private var cannedArchitecture: IntelArchitecture
    private var cannedGlossary: IntelGlossary
    /// The FULL code-graph corpus the mock pages over. `codeGraphQuery` returns the
    /// requested `[offset, offset+limit)` window and `total = corpus.count`, so the
    /// store can never receive the whole graph in one page unless it asks for it.
    private var codeGraphCorpus: [IntelCodeGraphRecord]

    private(set) var freshnessCallCount = 0
    private(set) var freshnessCheckCalls: [IntelFreshnessStamp] = []
    /// Each (limit, offset) the store requested — proves PAGING.
    private(set) var codeGraphPageRequests: [(limit: Int, offset: Int)] = []
    private(set) var glossaryCallCount = 0
    private(set) var architectureCallCount = 0

    init(
        current: IntelFreshnessStamp = IntelFreshnessStamp(
            headHash: "head0", worktreeHash: "wt0", indexHash: "idx0", inRepo: true
        ),
        architecture: IntelArchitecture = .empty,
        glossary: IntelGlossary = .empty,
        codeGraphCorpus: [IntelCodeGraphRecord] = []
    ) {
        self.current = current
        self.cannedArchitecture = architecture
        self.cannedGlossary = glossary
        self.codeGraphCorpus = codeGraphCorpus
    }

    // MARK: Test setup

    /// Replace the CURRENT stamp (e.g. to model the working tree changing after
    /// data was loaded, so a later freshness-check diverges → STALE).
    func setCurrent(_ stamp: IntelFreshnessStamp) {
        lock.lock(); defer { lock.unlock() }
        current = stamp
    }

    func setArchitecture(_ architecture: IntelArchitecture) {
        lock.lock(); defer { lock.unlock() }
        cannedArchitecture = architecture
    }

    func setGlossary(_ glossary: IntelGlossary) {
        lock.lock(); defer { lock.unlock() }
        cannedGlossary = glossary
    }

    /// Seed a large code-graph corpus the mock pages over.
    func setCodeGraphCorpus(_ records: [IntelCodeGraphRecord]) {
        lock.lock(); defer { lock.unlock() }
        codeGraphCorpus = records
    }

    // MARK: IntelligenceService — each method funnels through a fully-scoped
    // SYNCHRONOUS critical section (no lock held across an await).

    func freshness() async throws -> IntelFreshnessStamp {
        freshnessLocked()
    }

    func freshnessCheck(stamp: IntelFreshnessStamp) async throws -> IntelFreshnessCheck {
        freshnessCheckLocked(stamp: stamp)
    }

    func codeGraphQuery(query: String, limit: Int, offset: Int) async throws -> IntelCodeGraphPage {
        codeGraphLocked(query: query, limit: limit, offset: offset)
    }

    func glossary() async throws -> IntelGlossary {
        glossaryLocked()
    }

    func architecture() async throws -> IntelArchitecture {
        architectureLocked()
    }

    // MARK: Synchronous critical sections

    private func freshnessLocked() -> IntelFreshnessStamp {
        lock.lock(); defer { lock.unlock() }
        freshnessCallCount += 1
        return current
    }

    /// Compare the supplied stamp against the CURRENT triple. `fresh` only when
    /// EVERY provided component matches current; the first divergence (head →
    /// worktree → index) names the reason. A nil head on either side that does not
    /// match is treated as a head change (an unknown stamp is never "fresh").
    private func freshnessCheckLocked(stamp: IntelFreshnessStamp) -> IntelFreshnessCheck {
        lock.lock(); defer { lock.unlock() }
        freshnessCheckCalls.append(stamp)
        let reason: IntelStaleReason?
        if stamp.headHash != current.headHash {
            reason = .headChanged
        } else if stamp.worktreeHash != current.worktreeHash {
            reason = .worktreeChanged
        } else if stamp.indexHash != current.indexHash {
            reason = .indexChanged
        } else {
            reason = nil
        }
        return IntelFreshnessCheck(
            schema: "opensks.intel-freshness-check.v1",
            fresh: reason == nil,
            staleReason: reason,
            current: current
        )
    }

    private func codeGraphLocked(query: String, limit: Int, offset: Int) -> IntelCodeGraphPage {
        lock.lock(); defer { lock.unlock() }
        codeGraphPageRequests.append((limit: limit, offset: offset))
        let total = codeGraphCorpus.count
        let lower = max(0, min(offset, total))
        let upper = max(lower, min(offset + limit, total))
        let window = Array(codeGraphCorpus[lower..<upper])
        return IntelCodeGraphPage(
            schema: "opensks.intel-codegraph.v1",
            total: total,
            limit: limit,
            offset: offset,
            records: window,
            freshness: current
        )
    }

    private func glossaryLocked() -> IntelGlossary {
        lock.lock(); defer { lock.unlock() }
        glossaryCallCount += 1
        return cannedGlossary
    }

    private func architectureLocked() -> IntelArchitecture {
        lock.lock(); defer { lock.unlock() }
        architectureCallCount += 1
        return cannedArchitecture
    }
}
