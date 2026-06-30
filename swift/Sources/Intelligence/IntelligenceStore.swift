// IntelligenceStore.swift — the @MainActor view model for the Project
// Intelligence surface (PR-041). It owns:
//
//   • the architecture records + their loaded-at freshness stamp + badge,
//   • the glossary + its loaded-at stamp + badge,
//   • ONE PAGE of the code graph (limit/offset) + its loaded-at stamp + badge,
//   • per-view freshness badges that a watcher re-evaluates.
//
// FRESHNESS WATCHER (the central invariant): when data is loaded, the store
// records the EXACT stamp it was loaded with. A `recheckFreshness()` pass asks the
// service to compare each section's loaded-at stamp against the CURRENT triple
// (`freshness-check`); the moment current diverges, that section's badge flips to
// `.stale(reason:)`. A section badge is `.fresh` ONLY when its check returned
// `fresh == true` — the previously-loaded data is NEVER relabelled "current" once
// the underlying workspace has moved.
//
// PAGING: the code-graph explorer requests pages (limit/offset) and the store
// NEVER loads the whole graph at once. `codeGraphTotal` drives the pager; `nextPage`
// / `previousPage` step the offset by `codeGraphLimit`.

import SwiftUI

@MainActor
final class IntelligenceStore: ObservableObject {
    // Service boundary. Swappable so the live service can be (re)bound once the
    // workspace path is known at runtime.
    @Published private(set) var service: IntelligenceService

    // MARK: Architecture
    @Published private(set) var architecture: [IntelArchitectureRecord] = []
    @Published private(set) var architectureBadge: IntelFreshnessBadge = .stale(reason: nil)
    /// The stamp the architecture data was loaded with (the watcher checks this).
    private(set) var architectureStamp: IntelFreshnessStamp?

    // MARK: Glossary
    @Published private(set) var glossary: [IntelGlossaryTerm] = []
    @Published private(set) var glossaryBadge: IntelFreshnessBadge = .stale(reason: nil)
    private(set) var glossaryStamp: IntelFreshnessStamp?

    // MARK: Code graph (PAGED — only ever one page in memory)
    @Published private(set) var codeGraphRecords: [IntelCodeGraphRecord] = []
    @Published private(set) var codeGraphTotal = 0
    @Published private(set) var codeGraphOffset = 0
    @Published private(set) var codeGraphBadge: IntelFreshnessBadge = .stale(reason: nil)
    @Published var codeGraphQuery: String = ""
    private(set) var codeGraphStamp: IntelFreshnessStamp?
    /// Fixed page size — the explorer requests `[offset, offset+limit)` windows.
    let codeGraphLimit: Int

    // MARK: Status
    @Published private(set) var isLoading = false
    @Published var errorMessage: String?

    init(service: IntelligenceService, codeGraphLimit: Int = 100) {
        self.service = service
        self.codeGraphLimit = codeGraphLimit
    }

    /// Rebind the service (e.g. when the live workspace path becomes known).
    func updateService(_ service: IntelligenceService) {
        self.service = service
    }

    // MARK: - Derived (paging)

    /// 1-based index of the page currently shown (for the pager label).
    var codeGraphPageIndex: Int { codeGraphLimit > 0 ? (codeGraphOffset / codeGraphLimit) + 1 : 1 }

    /// Total number of pages for the current `total` (at least 1).
    var codeGraphPageCount: Int {
        guard codeGraphLimit > 0, codeGraphTotal > 0 else { return 1 }
        return (codeGraphTotal + codeGraphLimit - 1) / codeGraphLimit
    }

    var hasNextCodeGraphPage: Bool { codeGraphOffset + codeGraphLimit < codeGraphTotal }
    var hasPreviousCodeGraphPage: Bool { codeGraphOffset > 0 }

    // MARK: - Loading

    /// Load every section once. Each section records the stamp its data was loaded
    /// with and derives a FRESH badge (it was just read against current); the
    /// watcher then keeps the badge honest as the workspace moves.
    func loadAll() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        await loadArchitecture()
        await loadGlossary()
        await loadCodeGraphPage(offset: 0)
    }

    func loadArchitecture() async {
        do {
            let result = try await service.architecture()
            architecture = result.records
            architectureStamp = result.freshness
            // Just read against current → fresh at load time. The watcher flips it
            // to stale the moment current diverges.
            architectureBadge = .fresh
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func loadGlossary() async {
        do {
            let result = try await service.glossary()
            glossary = result.terms
            glossaryStamp = result.freshness
            glossaryBadge = .fresh
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Load ONE page of the code graph at `offset` for the current query. The whole
    /// graph is never loaded — only this `[offset, offset+limit)` window is held.
    func loadCodeGraphPage(offset: Int) async {
        guard shouldRunCodeGraphQuery else {
            clearCodeGraphResults()
            return
        }
        do {
            let page = try await service.codeGraphQuery(
                query: normalizedCodeGraphQuery,
                limit: codeGraphLimit,
                offset: max(0, offset)
            )
            codeGraphRecords = page.records
            codeGraphTotal = page.total
            codeGraphOffset = page.offset
            codeGraphStamp = page.freshness
            codeGraphBadge = .fresh
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Re-run the query from the first page (used when the query text changes).
    func runCodeGraphQuery() async {
        await loadCodeGraphPage(offset: 0)
    }

    /// Step to the next page (no-op if there is none). PAGING: still one window.
    func nextCodeGraphPage() async {
        guard hasNextCodeGraphPage else { return }
        await loadCodeGraphPage(offset: codeGraphOffset + codeGraphLimit)
    }

    /// Step to the previous page (no-op if at the first).
    func previousCodeGraphPage() async {
        guard hasPreviousCodeGraphPage else { return }
        await loadCodeGraphPage(offset: codeGraphOffset - codeGraphLimit)
    }

    // MARK: - Freshness watcher

    /// Re-check every loaded section's freshness against the CURRENT triple. Each
    /// section's badge is recomputed from a `freshness-check` of the stamp its data
    /// was loaded with: the moment current diverges, the badge flips to STALE. A
    /// section whose data was never loaded (no stamp) stays STALE — it is never
    /// shown as current. This is the method a timer / on-appear / focus change drives.
    func recheckFreshness() async {
        await recheckArchitecture()
        await recheckGlossary()
        await recheckCodeGraph()
    }

    private func recheckArchitecture() async {
        guard let stamp = architectureStamp else {
            architectureBadge = .stale(reason: nil)
            return
        }
        await recheck(stamp) { self.architectureBadge = $0 }
    }

    private func recheckGlossary() async {
        guard let stamp = glossaryStamp else {
            glossaryBadge = .stale(reason: nil)
            return
        }
        await recheck(stamp) { self.glossaryBadge = $0 }
    }

    private func recheckCodeGraph() async {
        guard let stamp = codeGraphStamp else {
            codeGraphBadge = .stale(reason: nil)
            return
        }
        await recheck(stamp) { self.codeGraphBadge = $0 }
    }

    /// Run one freshness-check and hand the derived badge to `assign`. On a service
    /// error the badge becomes STALE (reason unknown) — an unverifiable section is
    /// never presented as fresh.
    private func recheck(_ stamp: IntelFreshnessStamp, assign: (IntelFreshnessBadge) -> Void) async {
        do {
            let check = try await service.freshnessCheck(stamp: stamp)
            assign(IntelFreshnessBadge(check: check))
        } catch {
            assign(.stale(reason: nil))
            errorMessage = error.localizedDescription
        }
    }

    private var normalizedCodeGraphQuery: String {
        codeGraphQuery.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var shouldRunCodeGraphQuery: Bool {
        !normalizedCodeGraphQuery.isEmpty
    }

    private func clearCodeGraphResults() {
        codeGraphRecords = []
        codeGraphTotal = 0
        codeGraphOffset = 0
        codeGraphStamp = nil
        codeGraphBadge = .stale(reason: nil)
    }

    // MARK: - Deep links

    /// Resolve an architecture record to a deep-link TARGET. The record's `refs`
    /// (or its first ref) determine where "open" navigates: a `conversation:` ref →
    /// the chat thread, a `run:` ref → the live run graph, anything else (a path) →
    /// the code editor. The first recognised ref wins; a record with no resolvable
    /// ref yields `nil` (the UI then offers no deep link rather than a fake one).
    func deepLinkTarget(forRecord id: String) -> IntelDeepLinkTarget? {
        guard let record = architecture.first(where: { $0.id == id }) else { return nil }
        return IntelDeepLinkTarget(refs: record.refs)
    }

    /// Resolve a code-graph result to a deep-link target — always its source file.
    func deepLinkTarget(forCodeGraph record: IntelCodeGraphRecord) -> IntelDeepLinkTarget {
        .file(path: record.path, line: record.line)
    }
}

// MARK: - Deep-link target

/// Where a record / result deep-links to. The view maps each case onto the EXISTING
/// routes (chat / graph / code) via the coordinator — no new route is invented and
/// no existing route is removed.
enum IntelDeepLinkTarget: Equatable {
    /// Open a conversation thread (the `.chat` route), selecting this conversation.
    case conversation(id: String)
    /// Open a run's live graph (the `.graph` route), focusing this run.
    case run(id: String)
    /// Open a file in the code editor (the `.code` route) at an optional line.
    case file(path: String, line: Int?)

    /// Build a target from a record's refs. Refs are matched by prefix:
    ///   `conversation:<id>` / `conv:<id>` → conversation;
    ///   `run:<id>`                        → run;
    ///   otherwise                         → file (the ref is a path).
    /// The first recognised ref wins so a record can point at the most relevant
    /// target. Returns nil only when there are no refs at all.
    init?(refs: [String]) {
        guard let first = refs.first else { return nil }
        self = IntelDeepLinkTarget(ref: first)
    }

    /// Classify a single ref into a target.
    init(ref: String) {
        if let id = IntelDeepLinkTarget.strip(ref, prefixes: ["conversation:", "conv:"]) {
            self = .conversation(id: id)
        } else if let id = IntelDeepLinkTarget.strip(ref, prefixes: ["run:"]) {
            self = .run(id: id)
        } else {
            self = .file(path: ref, line: nil)
        }
    }

    private static func strip(_ ref: String, prefixes: [String]) -> String? {
        for prefix in prefixes where ref.hasPrefix(prefix) {
            return String(ref.dropFirst(prefix.count))
        }
        return nil
    }

    /// A stable id for the resolved target (for assertions + accessibility).
    var targetId: String {
        switch self {
        case .conversation(let id): return id
        case .run(let id): return id
        case .file(let path, _): return path
        }
    }
}
