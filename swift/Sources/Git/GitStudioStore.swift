// GitStudioStore.swift — the @MainActor owner of the Git studio state (PR-034
// reads + PR-035 LOCAL mutations).
//
// Holds the decoded status / branches / diff and the file selection. A single
// async load is CANCELLABLE so a refresh on a large repo never blocks the UI:
// firing a new refresh cancels the in-flight one. Rapid triggers are COALESCED
// behind a debounce window so a burst of file-system pokes turns into a bounded
// number of service calls. While a refresh is running the store exposes a
// `stale` flag the view dims on, then clears it when the fresh data lands.
//
// PR-035 adds the LOCAL mutation flow: stage / unstage; a dirty-aware branch
// switch (the preflight consults BOTH the CLI and the EditorWorkspaceStore for
// unsaved buffers — a dirty result BLOCKS the switch, never a silent --force);
// and a reviewed-hash commit (`commit-preview` → `commit`, where an
// `index_changed` flips the composer to STALE so a commit only ever contains the
// reviewed paths). There is NO push anywhere — the store never calls a push
// method because none exists on `GitService`.

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

    // MARK: Mutation state (PR-035)

    /// The blocked-switch explanation surfaced when a preflight (CLI blockers OR
    /// unsaved editor buffers) refuses the switch. Non-nil ⇒ the switch did NOT
    /// proceed; the view explains the blockers and offers no silent --force.
    @Published private(set) var switchBlock: GitSwitchBlockState?
    /// A non-fatal banner for the last failed mutation (stage/unstage/commit/…).
    @Published var mutationError: String?
    /// Paths refused by the most recent stage attempt (secret / data-plane). The
    /// view marks these non-stageable with a clear reason.
    @Published private(set) var stageRejections: [GitStageRejection] = []
    /// The reviewed commit preview + its staleness, owned here so the composer can
    /// disable Commit until there are staged paths + a message, send the reviewed
    /// `index_hash`, and flip to STALE on `index_changed`.
    @Published private(set) var commit = GitCommitComposerState()

    private var service: GitService
    /// Debounce window for coalescing rapid refresh triggers.
    private let debounce: Duration

    /// The editor store consulted by the switch preflight: an unsaved buffer is a
    /// switch blocker on the Swift side, independent of the CLI worktree check, so
    /// a branch switch never silently strands unsaved edits. Weak so the store
    /// does not retain the editor; nil in read-only contexts (the CLI preflight
    /// still applies).
    weak var editorStore: EditorWorkspaceStore?

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

    // MARK: - Stage / unstage (PR-035, local)

    /// Stage one path. Secret / data-plane paths are NEVER staged: they come back
    /// in `rejected`, which the store surfaces so the view marks them
    /// non-stageable. After a stage the status + the commit preview refresh so the
    /// composer sees the new staged tree.
    func stage(_ path: String) async {
        await stage([path])
    }

    func stage(_ paths: [String]) async {
        mutationError = nil
        do {
            let result = try await service.stage(paths: paths)
            stageRejections = result.rejected
            refresh()
            await refreshCommitPreview()
        } catch {
            handleMutationError(error)
        }
    }

    /// Unstage one path; refresh status + the commit preview afterwards.
    func unstage(_ path: String) async {
        await unstage([path])
    }

    func unstage(_ paths: [String]) async {
        mutationError = nil
        do {
            _ = try await service.unstage(paths: paths)
            refresh()
            await refreshCommitPreview()
        } catch {
            handleMutationError(error)
        }
    }

    /// True when a path can NEVER be staged (it is secret / data-plane, as
    /// reported by a prior stage rejection). The view renders it non-stageable.
    func isStageable(_ entry: GitStatusEntry) -> Bool {
        rejection(for: entry.path) == nil
    }

    /// The recorded rejection for a path, if it was refused as secret/data-plane.
    func rejection(for path: String) -> GitStageRejection? {
        stageRejections.first { $0.path == path }
    }

    // MARK: - Branch switch (PR-035, dirty-aware, local)

    /// Attempt to switch to `target`. The preflight consults BOTH the CLI
    /// (`switch-preflight`) AND the EditorWorkspaceStore for unsaved buffers; if
    /// EITHER reports dirt the switch is BLOCKED — `switchBlock` is set and
    /// `switch` is NEVER called (no silent --force). Only a fully-clean preflight
    /// proceeds to the actual switch.
    func attemptSwitch(to target: String) async {
        mutationError = nil
        switchBlock = nil
        do {
            let preflight = try await service.switchPreflight(target: target)
            var blockers = preflight.blockers
            // Editor-side blocker: any unsaved buffer strands work on a switch.
            if let editorBlocker = unsavedBufferBlocker() {
                blockers.append(editorBlocker)
            }
            let clean = preflight.canSwitch && blockers.isEmpty
            guard clean else {
                // BLOCKED: surface the explanation, do NOT call switch.
                switchBlock = GitSwitchBlockState(target: target, blockers: blockers)
                return
            }
            let result = try await service.switchBranch(target: target, force: false)
            if result.switched {
                clearSelection()
                stageRejections = []
                refresh()
                await refreshCommitPreview()
            }
        } catch let error as GitServiceError {
            if case .switchBlocked(let blockers) = error {
                switchBlock = GitSwitchBlockState(target: target, blockers: blockers)
            } else {
                handleMutationError(error)
            }
        } catch {
            handleMutationError(error)
        }
    }

    /// Dismiss the blocked-switch banner (the operator saves / commits first, then
    /// retries — there is no force path).
    func dismissSwitchBlock() {
        switchBlock = nil
    }

    /// Create a local branch (optionally from a ref) and refresh the branch list.
    /// This is a LOCAL operation — it never pushes.
    func createBranch(name: String, from: String? = nil) async {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        mutationError = nil
        do {
            _ = try await service.createBranch(name: trimmed, from: from)
            refresh()
        } catch {
            handleMutationError(error)
        }
    }

    /// An editor-side switch blocker for any unsaved buffer, or nil when clean.
    private func unsavedBufferBlocker() -> GitSwitchBlocker? {
        guard let editorStore else { return nil }
        let dirtyPaths = editorStore.documents
            .filter { $0.isDirty }
            .map { $0.workspaceRelativePath }
        guard !dirtyPaths.isEmpty else { return nil }
        return GitSwitchBlocker(kind: .unsavedBuffers, paths: dirtyPaths)
    }

    // MARK: - Commit (PR-035, reviewed-hash, local)

    /// Refresh the commit preview (the staged tree + its `index_hash`). Clears the
    /// STALE flag because the preview now matches the live index again. The
    /// message draft + any receipt are preserved.
    func refreshCommitPreview() async {
        do {
            let preview = try await service.commitPreview()
            commit.preview = preview
            commit.isStale = false
        } catch {
            handleMutationError(error)
        }
    }

    /// Bind the commit message draft (two-way for the composer field).
    func setCommitMessage(_ text: String) {
        commit.message = text
    }

    /// Commit the reviewed staged tree. Sends the preview's `index_hash` as
    /// `expected-index-hash`; if the live index has moved the service throws
    /// `.indexChanged`, which flips the composer to STALE and does NOT report
    /// success — the operator must refresh the preview (re-reviewing the paths)
    /// before retrying, so a commit can only ever contain the reviewed paths. On
    /// success the receipt is recorded and `onCommitted` fires so a commit card is
    /// posted into the active conversation.
    @discardableResult
    func performCommit() async -> GitCommitResult? {
        guard commit.canCommit else { return nil }
        mutationError = nil
        commit.isCommitting = true
        defer { commit.isCommitting = false }
        let message = commit.message.trimmingCharacters(in: .whitespacesAndNewlines)
        let expected = commit.preview.indexHash
        do {
            let result = try await service.commit(message: message, expectedIndexHash: expected)
            commit.receipt = result
            commit.message = ""
            stageRejections = []
            refresh()
            await refreshCommitPreview()
            onCommitted?(result, message)
            return result
        } catch let error as GitServiceError {
            switch error {
            case .indexChanged:
                // STALE: the reviewed hash no longer matches the live index. Do
                // NOT report success; force a re-review before the next commit.
                commit.isStale = true
                mutationError = "The staged files changed since the preview. Refresh the preview before committing."
            case .secretRejected(let rejected):
                stageRejections = rejected
                let names = rejected.map(\.path).joined(separator: ", ")
                mutationError = "Refused to commit restricted path(s): \(names)."
            default:
                handleMutationError(error)
            }
            return nil
        } catch {
            handleMutationError(error)
            return nil
        }
    }

    /// Dismiss the receipt card after the operator has read it.
    func dismissReceipt() {
        commit.receipt = nil
    }

    /// Fired after a successful commit so the host can post a commit card into the
    /// active conversation thread. Receives the commit result + the message used.
    var onCommitted: ((GitCommitResult, String) -> Void)?

    private func handleMutationError(_ error: Error) {
        mutationError = Self.describe(error)
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
            case .transport(let m), .service(let m):
                return m
            case .switchBlocked:
                return "Switch blocked by uncommitted changes."
            case .indexChanged:
                return "The staged files changed since the preview. Refresh the preview before committing."
            case .secretRejected(let rejected):
                let names = rejected.map(\.path).joined(separator: ", ")
                return "Refused to stage/commit restricted path(s): \(names)."
            }
        }
        return error.localizedDescription
    }
}
