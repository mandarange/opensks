// GitStudioStore.swift — the @MainActor owner of the READ-ONLY Git studio state
// (PR-034).
//
// Holds the decoded status / branches / diff and the file selection. A single
// async load is CANCELLABLE so a refresh on a large repo never blocks the UI:
// firing a new refresh cancels the in-flight one. Rapid triggers are COALESCED
// behind a debounce window so a burst of file-system pokes turns into a bounded
// number of service calls. While a refresh is running the store exposes a
// `stale` flag the view dims on, then clears it when the fresh data lands.
//
// READ-ONLY: every code path here is a read. There is no stage/commit/switch/
// push — the store only ever calls the three read methods on `GitService`.

import SwiftUI

@MainActor
final class GitStudioStore: ObservableObject {
    // MARK: Published state

    @Published private(set) var status: GitStatus = .empty
    @Published private(set) var branches: GitBranches = .empty
    /// The diff for the currently selected file (empty when nothing is selected).
    @Published private(set) var selectedDiff: GitDiff = .empty
    /// The workspace-relative path of the selected change, if any.
    @Published private(set) var selectedPath: String?

    /// True while a refresh is in flight: the view shows the previous data dimmed
    /// (`stale`) rather than flashing empty, then this clears when fresh data lands.
    @Published private(set) var isRefreshing = false
    /// A non-fatal banner for the last failed read.
    @Published private(set) var loadError: String?

    /// Derived grouping (staged / unstaged / untracked / conflicted), recomputed
    /// whenever `status` changes. Pure function of the decoded entries.
    @Published private(set) var groups = GitStatusGroups(from: [])

    private var service: GitService
    /// Debounce window for coalescing rapid refresh triggers.
    private let debounce: Duration

    /// The in-flight refresh; cancelled when a newer refresh supersedes it so the
    /// UI never waits on stale work.
    private var refreshTask: Task<Void, Never>?
    /// The pending debounced trigger; replaced (and the old one cancelled) on
    /// each rapid poke so a burst collapses to a single trailing refresh.
    private var debounceTask: Task<Void, Never>?

    init(service: GitService, debounce: Duration = .milliseconds(120)) {
        self.service = service
        self.debounce = debounce
    }

    // MARK: - Rebinding

    /// Swap the live service (e.g. once the real workspace + bundled CLI are
    /// resolved) and kick a fresh read. Cancels any in-flight work first.
    func rebind(service: GitService) {
        refreshTask?.cancel()
        debounceTask?.cancel()
        self.service = service
        clearSelection()
        refresh()
    }

    // MARK: - Stale flag

    /// True when the displayed data is being refreshed (the view dims, never blanks).
    var stale: Bool { isRefreshing }

    // MARK: - Loading (cancellable)

    /// Refresh status + branches immediately (cancelling any in-flight refresh)
    /// and re-fetch the selected file's diff if one is selected. Cancellable so a
    /// large-repo read can be superseded without blocking the UI.
    func refresh() {
        refreshTask?.cancel()
        isRefreshing = true
        let path = selectedPath
        refreshTask = Task { [weak self] in
            guard let self else { return }
            do {
                async let statusResult = self.service.status()
                async let branchesResult = self.service.branches()
                let status = try await statusResult
                let branches = try await branchesResult
                if Task.isCancelled { return }
                self.apply(status: status, branches: branches)

                if let path {
                    let diff = try await self.service.diff(path: path, staged: false)
                    if Task.isCancelled { return }
                    self.selectedDiff = diff
                }
                self.loadError = nil
            } catch is CancellationError {
                return
            } catch {
                if Task.isCancelled { return }
                self.loadError = Self.describe(error)
            }
            if Task.isCancelled { return }
            self.isRefreshing = false
        }
    }

    /// Coalesced refresh: rapid calls within the debounce window collapse to a
    /// SINGLE trailing `refresh()`. This is what the watcher pokes so a burst of
    /// file-system events does not spawn one service round-trip per event.
    func debouncedRefresh() {
        // Mark stale right away so the UI reacts to the burst, but defer the
        // actual service work to the trailing edge of the window.
        isRefreshing = true
        debounceTask?.cancel()
        let window = debounce
        debounceTask = Task { [weak self] in
            try? await Task.sleep(for: window)
            guard let self, !Task.isCancelled else { return }
            self.refresh()
        }
    }

    // MARK: - Selection (read-only)

    /// Select a changed file and load its diff. A conflicted/untracked file still
    /// shows whatever diff the read-only service returns. Selecting the same path
    /// is a no-op refresh of that diff.
    func select(_ entry: GitStatusEntry) {
        select(path: entry.path, staged: entry.isStaged && !entry.isUnstaged)
    }

    func select(path: String, staged: Bool = false) {
        selectedPath = path
        selectedDiff = .empty
        let service = self.service
        Task { [weak self] in
            guard let self else { return }
            do {
                let diff = try await service.diff(path: path, staged: staged)
                if self.selectedPath == path { self.selectedDiff = diff }
            } catch is CancellationError {
                return
            } catch {
                self.loadError = Self.describe(error)
            }
        }
    }

    func clearSelection() {
        selectedPath = nil
        selectedDiff = .empty
    }

    /// The selected file's diff (if the selected path is present in the diff set).
    var selectedDiffFile: GitDiffFile? {
        guard let selectedPath else { return nil }
        return selectedDiff.file(forPath: selectedPath) ?? selectedDiff.files.first
    }

    // MARK: - Internals

    private func apply(status: GitStatus, branches: GitBranches) {
        self.status = status
        self.branches = branches
        self.groups = GitStatusGroups(from: status.entries)
        // Drop a selection whose path no longer appears in the new status.
        if let path = selectedPath,
           !status.entries.contains(where: { $0.path == path }) {
            clearSelection()
        }
    }

    private static func describe(_ error: Error) -> String {
        if let gitError = error as? GitServiceError {
            switch gitError {
            case .transport(let m), .service(let m): return m
            }
        }
        return error.localizedDescription
    }
}
