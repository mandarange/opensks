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
// reviewed paths).
//
// PR-036 adds the EXPLICITLY-APPROVED push flow as a SEPARATE receipt track. A
// "Commit & Push" runs the PR-035 commit to a COMMIT receipt FIRST, then enqueues
// a push (`push-enqueue`) and surfaces a `GitPushPrompt` showing the EXACT effect
// (redacted remote, ref, local oid → remote-expected oid, a protected-branch
// warning). The push is NEVER executed until the operator approves from the
// prompt — approve carries the intent's `effect_digest`; a `digest_mismatch`
// keeps the prompt open to re-review; a protected ref needs the extra ack; a
// `push_failed` preserves the LOCAL commit + the pending intent as RETRYABLE.
// Commit and push are independent: a commit receipt can stand while a push is
// still pending or failed.

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

    // MARK: Push outbox state (PR-036)

    /// The push outbox: the active approval prompt (the exact effect awaiting the
    /// operator's approval), the most recent push receipt, a retryable failed push,
    /// the recovered push status, and the last push error. Kept SEPARATE from
    /// `commit` so a commit receipt stands independently of any pending/failed push.
    @Published private(set) var push = GitPushOutboxState()

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
    /// The optional worktree watcher feeding `debouncedRefresh()` (GIT-102). Held so
    /// it is stopped/released on rebind; nil until the workspace is known.
    private var worktreeWatcher: GitWorktreeWatcher?

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
        // The old watcher pointed at the previous workspace; drop it. A fresh one is
        // installed by the caller (bindGit) via `startWatching` for the new workspace.
        stopWatching()
        self.service = service
        clearSelection()
        refresh()
    }

    // MARK: - Worktree watching (GIT-102)

    /// Install a worktree watcher whose file-system events poke the COALESCED
    /// refresh. The watcher's callback may arrive off the main actor, so it hops
    /// back before touching store state. Replaces (and stops) any prior watcher.
    func startWatching(_ watcher: GitWorktreeWatcher) {
        stopWatching()
        worktreeWatcher = watcher
        watcher.start { [weak self] in
            Task { @MainActor in self?.debouncedRefresh() }
        }
    }

    /// Stop and release the worktree watcher (idempotent).
    func stopWatching() {
        worktreeWatcher?.stop()
        worktreeWatcher = nil
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
    /// SINGLE trailing `refresh()`. The worktree watcher (GIT-102) pokes this on
    /// file-system events, so a burst does not spawn one service round-trip per event.
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

    /// The default remote to push to (the workspace's configured remote). The
    /// executor only ever pushes to THIS remote; the UI shows the redacted URL.
    let defaultRemote = "origin"

    /// The ref a push would target — the current branch (or its upstream branch
    /// name). A detached HEAD has no push ref, so Commit & Push is unavailable.
    var pushRef: String? {
        if status.detached { return nil }
        return status.branch ?? branches.current
    }

    /// True when a "Commit & Push" is possible: a commit is allowed AND there is a
    /// ref to push. (The push itself still requires explicit approval afterward.)
    var canCommitAndPush: Bool {
        commit.canCommit && pushRef != nil
    }

    // MARK: - Commit & Push (PR-036, explicitly-approved)

    /// Convenience: "Commit & Push" using the configured remote + the current
    /// branch as the ref. No-op if there is no push ref (detached HEAD).
    func commitAndPush() async {
        guard let ref = pushRef else { return }
        await commitAndPush(remote: defaultRemote, ref: ref)
    }

    /// Run a LOCAL commit (PR-035) to a COMMIT receipt FIRST, then ENQUEUE a push
    /// of `ref` to `remote` and open the approval prompt with the exact effect.
    /// The push is NOT executed here — it waits for the operator to approve the
    /// exact effect from the prompt. If the commit fails (stale / secret / busy)
    /// NO push is enqueued. Commit and push are separate receipts: the commit
    /// receipt stands even if the push is later abandoned or fails.
    func commitAndPush(remote: String, ref: String) async {
        // 1. Commit first — this records the COMMIT receipt + posts the commit card.
        let committed = await performCommit()
        guard committed != nil else { return }
        // 2. Enqueue the push (durable intent + effect digest). No remote contact.
        await enqueuePush(remote: remote, ref: ref)
    }

    /// Enqueue a push intent and open the approval prompt for its exact effect.
    /// Persists durably (SQLite) on the CLI side so the intent survives relaunch.
    /// Never contacts the remote — only records what a push WOULD do.
    func enqueuePush(remote: String, ref: String) async {
        push.error = nil
        do {
            let intent = try await service.pushEnqueue(remote: remote, ref: ref)
            push.retryable = nil
            push.prompt = GitPushPrompt(intent: intent)
            await refreshPushStatus()
        } catch {
            push.error = Self.describePush(error)
        }
    }

    /// Toggle the operator's explicit protected-branch acknowledgement in the open
    /// prompt. Required before a protected push can be approved (never auto-set).
    func setAckProtected(_ ack: Bool) {
        guard push.prompt != nil else { return }
        push.prompt?.ackProtected = ack
    }

    /// Approve the open push prompt's EXACT effect, then EXECUTE the push. Approval
    /// carries the intent's `effect_digest`; the CLI records an approval only if it
    /// still matches the intent's current digest. A `digest_mismatch` keeps the
    /// prompt open (the effect moved — re-review). A protected ref requires the
    /// extra ack (the prompt's Approve is disabled until then). On a successful
    /// execute the receipt is recorded and `onPushed` fires (the conversation push
    /// card). A `push_failed` keeps the COMMIT intact and marks the push RETRYABLE
    /// (the local commit is preserved).
    func approveAndExecutePush() async {
        guard let prompt = push.prompt, prompt.canApprove else { return }
        let intent = prompt.intent
        push.error = nil
        push.prompt?.notice = nil
        push.prompt?.isWorking = true
        defer { push.prompt?.isWorking = false }
        do {
            let approval = try await service.pushApprove(
                intentId: intent.intentId,
                effectDigest: intent.effectDigest,
                ackProtected: prompt.ackProtected
            )
            guard approval.matched else {
                // A non-matching approval is the same as a mismatch: do NOT push.
                push.prompt?.notice = Self.digestMismatchNotice
                await refreshPushStatus()
                return
            }
            // Approved the exact effect → execute the push.
            let receipt = try await service.pushExecute(intentId: intent.intentId)
            push.receipt = receipt
            push.retryable = nil
            push.prompt = nil
            await refreshPushStatus()
            onPushed?(receipt, intent, approval)
        } catch let error as GitPushError {
            handlePushError(error, intent: intent)
        } catch {
            push.error = Self.describePush(error)
        }
    }

    /// Retry a push that FAILED at execute: re-open the approval prompt for the
    /// SAME intent so the operator re-approves the (unchanged) exact effect. The
    /// local commit is preserved — this never re-commits.
    func retryPush() {
        guard let intent = push.retryable else { return }
        push.error = nil
        push.prompt = GitPushPrompt(intent: intent)
    }

    /// Dismiss the open approval prompt WITHOUT pushing (the operator declined the
    /// effect). The intent remains pending on the CLI side (durable) and the LOCAL
    /// commit is untouched.
    func dismissPushPrompt() {
        push.prompt = nil
    }

    /// Dismiss the most recent push receipt after the operator has read it.
    func dismissPushReceipt() {
        push.receipt = nil
    }

    /// Refresh the push outbox (pending / approved / completed) from the CLI. Reads
    /// state recovered from SQLite so the outbox survives relaunch.
    func refreshPushStatus() async {
        do {
            push.status = try await service.pushStatus()
        } catch {
            // A status read failure is non-fatal — keep the last known outbox.
            push.error = Self.describePush(error)
        }
    }

    /// Fired after a successful push so the host can post a push card into the
    /// active conversation thread. Receives the push receipt + the intent pushed.
    var onPushed: ((GitPushReceipt, GitPushIntent, GitPushApproval) -> Void)?

    /// Map a typed push error onto the prompt / outbox state. A digest mismatch or
    /// missing ack keeps the prompt OPEN (re-review / re-ack); a failed push closes
    /// the prompt but marks the push RETRYABLE with the commit preserved.
    private func handlePushError(_ error: GitPushError, intent: GitPushIntent) {
        switch error {
        case .digestMismatch:
            // The exact effect moved (oid/ref changed) — do NOT push. Keep the
            // prompt open so the operator re-reviews; no usable approval was made.
            push.prompt?.notice = Self.digestMismatchNotice
        case .noMatchingApproval:
            push.prompt?.notice = "No approval was recorded for this push. Approve the exact effect to push."
        case .protectedBranch:
            // The protected ref needs the explicit ack — re-prompt with the toggle.
            push.prompt?.ackProtected = false
            push.prompt?.notice = "This is a protected branch. Acknowledge the protected-branch push to continue."
        case .pushFailed(let message):
            // The commit is intact; the intent is preserved for retry.
            push.prompt = nil
            push.retryable = intent
            let detail = message.map { ": \($0)" } ?? "."
            push.error = "The push failed\(detail) Your commit is saved — retry the push when the remote is reachable."
        }
    }

    private static let digestMismatchNotice =
        "The push target moved since this effect was prepared (the commit or branch changed). Re-review the new effect before approving."

    private func handleMutationError(_ error: Error) {
        mutationError = Self.describe(error)
    }

    private static func describePush(_ error: Error) -> String {
        if let pushError = error as? GitPushError {
            switch pushError {
            case .digestMismatch: return digestMismatchNotice
            case .noMatchingApproval: return "No approval was recorded for this push."
            case .protectedBranch: return "This is a protected branch; acknowledge the protected-branch push first."
            case .pushFailed(let message):
                let detail = message.map { ": \($0)" } ?? "."
                return "The push failed\(detail) Your commit is saved — retry when the remote is reachable."
            }
        }
        return Self.describe(error)
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
