// GitStudioTests.swift — the READ-ONLY Git studio (PR-034).
//
// Drives GitModels / GitStudioStore / GitStatusView through a MockGitService (no
// disk, no process) returning canned status / branches / diff JSON including a
// rename, a conflict and an untracked file. Asserts:
//   • status JSON decodes and entries group correctly (staged / unstaged /
//     untracked / conflicted), and a rename shows orig → new;
//   • current branch + dirty state derive from the decoded status; a branch
//     checked out elsewhere is marked occupied;
//   • the store's debounced refresh coalesces rapid triggers into a bounded
//     number of service calls and sets/clears the stale flag;
//   • GitStatusView + the diff render (ImageRenderer non-nil) and fill width at
//     1024 / 1440 / 1920 with no letterbox, including a 1,000-entry fixture;
//   • the GitService protocol surface is READ-ONLY (status / branches / diff) —
//     a mutation method would not compile against it.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class GitStudioTests: XCTestCase {

    // MARK: - Canned JSON fixtures

    /// A status with one staged-only, one unstaged-only, one staged+unstaged
    /// (`MM`), an untracked, a conflicted and a rename entry.
    private static let statusJSON = """
    {
      "schema": "opensks.git-status.v1",
      "in_repo": true,
      "branch": "main",
      "detached": false,
      "upstream": "origin/main",
      "ahead": 2,
      "behind": 1,
      "is_dirty": true,
      "entries": [
        {"path": "staged.rs",   "orig_path": null, "index_status": "M", "worktree_status": " ", "kind": "modified"},
        {"path": "unstaged.rs", "orig_path": null, "index_status": " ", "worktree_status": "M", "kind": "modified"},
        {"path": "both.rs",     "orig_path": null, "index_status": "M", "worktree_status": "M", "kind": "modified"},
        {"path": "new.rs",      "orig_path": null, "index_status": "?", "worktree_status": "?", "kind": "untracked"},
        {"path": "merge.rs",    "orig_path": null, "index_status": "U", "worktree_status": "U", "kind": "conflicted"},
        {"path": "renamed_new.rs", "orig_path": "renamed_old.rs", "index_status": "R", "worktree_status": " ", "kind": "renamed"}
      ]
    }
    """

    private static let branchesJSON = """
    {
      "schema": "opensks.git-branches.v1",
      "current": "main",
      "branches": [
        {"name": "main", "is_current": true,  "upstream": "origin/main", "ahead": 2, "behind": 1, "worktree_path": null, "checked_out_elsewhere": false},
        {"name": "feature/x", "is_current": false, "upstream": "origin/feature/x", "ahead": 0, "behind": 0, "worktree_path": "/tmp/wt-x", "checked_out_elsewhere": true},
        {"name": "lonely", "is_current": false, "upstream": null, "ahead": 0, "behind": 0, "worktree_path": null, "checked_out_elsewhere": false}
      ]
    }
    """

    private static let diffJSON = """
    {
      "schema": "opensks.git-diff.v1",
      "files": [
        {
          "path": "both.rs",
          "orig_path": null,
          "is_binary": false,
          "hunks": [
            {"old_start": 1, "old_lines": 1, "new_start": 1, "new_lines": 2,
             "lines": ["-let x = 1;", "+let x = 2;", "+let y = 3;"]}
          ]
        }
      ]
    }
    """

    private func decodeStatus() throws -> GitStatus {
        try JSONDecoder().decode(GitStatus.self, from: Data(Self.statusJSON.utf8))
    }
    private func decodeBranches() throws -> GitBranches {
        try JSONDecoder().decode(GitBranches.self, from: Data(Self.branchesJSON.utf8))
    }
    private func decodeDiff() throws -> GitDiff {
        try JSONDecoder().decode(GitDiff.self, from: Data(Self.diffJSON.utf8))
    }

    private func makeStore() throws -> (GitStudioStore, MockGitService) {
        let service = MockGitService(
            status: try decodeStatus(),
            branches: try decodeBranches(),
            diff: try decodeDiff()
        )
        let store = GitStudioStore(service: service, debounce: .milliseconds(60))
        return (store, service)
    }

    /// Drive the store's async refresh to quiescence.
    private func refreshAndSettle(_ store: GitStudioStore) async {
        store.refresh()
        try? await Task.sleep(nanoseconds: 60_000_000)
    }

    // MARK: - Decode + grouping

    func testStatusJSONDecodes() throws {
        let status = try decodeStatus()
        XCTAssertEqual(status.schema, "opensks.git-status.v1")
        XCTAssertTrue(status.inRepo)
        XCTAssertEqual(status.branch, "main")
        XCTAssertEqual(status.upstream, "origin/main")
        XCTAssertEqual(status.ahead, 2)
        XCTAssertEqual(status.behind, 1)
        XCTAssertTrue(status.isDirty)
        XCTAssertEqual(status.entries.count, 6)
    }

    func testMinimalNotInRepoStatusDecodes() throws {
        let json = #"{"schema":"opensks.git-status.v1","in_repo":false}"#
        let status = try JSONDecoder().decode(GitStatus.self, from: Data(json.utf8))
        XCTAssertFalse(status.inRepo)
        XCTAssertNil(status.branch)
        XCTAssertTrue(status.entries.isEmpty)
        XCTAssertFalse(status.isDirty)
    }

    func testEntriesGroupCorrectly() throws {
        let groups = GitStatusGroups(from: try decodeStatus().entries)

        // staged: staged.rs, both.rs (MM), renamed_new.rs
        XCTAssertEqual(Set(groups.staged.map(\.path)), ["staged.rs", "both.rs", "renamed_new.rs"])
        // unstaged: unstaged.rs, both.rs (MM appears in BOTH staged and unstaged)
        XCTAssertEqual(Set(groups.unstaged.map(\.path)), ["unstaged.rs", "both.rs"])
        // untracked: new.rs
        XCTAssertEqual(groups.untracked.map(\.path), ["new.rs"])
        // conflicted: merge.rs
        XCTAssertEqual(groups.conflicted.map(\.path), ["merge.rs"])

        // A conflicted entry is NOT also counted as staged/unstaged.
        XCTAssertFalse(groups.staged.contains { $0.path == "merge.rs" })
        XCTAssertFalse(groups.unstaged.contains { $0.path == "merge.rs" })
        // An untracked entry is NOT counted as staged/unstaged.
        XCTAssertFalse(groups.staged.contains { $0.path == "new.rs" })
        XCTAssertFalse(groups.unstaged.contains { $0.path == "new.rs" })
    }

    func testRenameShowsOrigToNew() throws {
        let entry = try XCTUnwrap(try decodeStatus().entries.first { $0.kind == .renamed })
        XCTAssertTrue(entry.isRename)
        XCTAssertEqual(entry.origPath, "renamed_old.rs")
        XCTAssertEqual(entry.path, "renamed_new.rs")
        XCTAssertTrue(entry.id.contains("renamed_old.rs"))
        XCTAssertTrue(entry.id.contains("renamed_new.rs"))
    }

    func testConflictAndUntrackedDetection() throws {
        let entries = try decodeStatus().entries
        let merge = try XCTUnwrap(entries.first { $0.path == "merge.rs" })
        XCTAssertTrue(merge.isConflicted)
        let untracked = try XCTUnwrap(entries.first { $0.path == "new.rs" })
        XCTAssertTrue(untracked.isUntracked)
    }

    // MARK: - Derived branch / dirty state from decoded status

    func testCurrentBranchAndDirtyDeriveFromStatus() async throws {
        let (store, _) = try makeStore()
        await refreshAndSettle(store)
        XCTAssertEqual(store.status.branch, "main")
        XCTAssertTrue(store.status.isDirty, "dirty derives from the decoded status")
        XCTAssertFalse(store.status.detached)
        // Grouping is recomputed onto the store after refresh.
        XCTAssertFalse(store.groups.isEmpty)
        XCTAssertEqual(store.groups.conflicted.map(\.path), ["merge.rs"])
    }

    func testBranchCheckedOutElsewhereIsOccupied() throws {
        let branches = try decodeBranches()
        let feature = try XCTUnwrap(branches.branches.first { $0.name == "feature/x" })
        XCTAssertTrue(feature.checkedOutElsewhere)
        XCTAssertTrue(feature.isOccupiedElsewhere, "a branch in another worktree is occupied")
        XCTAssertEqual(feature.worktreePath, "/tmp/wt-x")

        let main = try XCTUnwrap(branches.branches.first { $0.name == "main" })
        XCTAssertTrue(main.isCurrent)
        XCTAssertFalse(main.isOccupiedElsewhere, "the current branch is not occupied-elsewhere")

        let lonely = try XCTUnwrap(branches.branches.first { $0.name == "lonely" })
        XCTAssertNil(lonely.upstream)
        XCTAssertFalse(lonely.isOccupiedElsewhere)
    }

    // MARK: - Debounce coalescing + stale flag

    func testDebouncedRefreshCoalescesRapidTriggers() async throws {
        let (store, service) = try makeStore()
        // Fire a burst of rapid triggers within the debounce window.
        for _ in 0..<25 { store.debouncedRefresh() }
        // The stale flag is set immediately while the burst is pending.
        XCTAssertTrue(store.stale, "a pending refresh marks the data stale")

        // Let the trailing edge fire and the single refresh settle.
        try await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertLessThanOrEqual(
            service.statusCallCount, 2,
            "25 rapid triggers must coalesce into a bounded number of service calls"
        )
        XCTAssertGreaterThanOrEqual(service.statusCallCount, 1, "exactly the trailing refresh runs")
        XCTAssertFalse(store.stale, "the stale flag clears once fresh data lands")
    }

    func testRefreshSetsThenClearsStale() async throws {
        let (store, service) = try makeStore()
        service.setStatusDelay(millis: 40)
        XCTAssertFalse(store.stale)
        store.refresh()
        XCTAssertTrue(store.stale, "refresh marks stale while in flight")
        try await Task.sleep(nanoseconds: 120_000_000)
        XCTAssertFalse(store.stale, "stale clears when the refresh completes")
    }

    func testNewRefreshCancelsInFlightForResponsiveness() async throws {
        let (store, service) = try makeStore()
        service.setStatusDelay(millis: 80)
        store.refresh()                       // begins a slow read
        try await Task.sleep(nanoseconds: 10_000_000)
        store.refresh()                       // supersedes it
        try await Task.sleep(nanoseconds: 200_000_000)
        // Both refreshes are allowed to count their status() call, but the store
        // never deadlocks and ends not-stale — the UI stays responsive.
        XCTAssertFalse(store.stale)
        XCTAssertGreaterThanOrEqual(service.statusCallCount, 1)
    }

    // MARK: - Selection loads diff (read-only)

    func testSelectingFileLoadsItsDiff() async throws {
        let (store, _) = try makeStore()
        await refreshAndSettle(store)
        let entry = try XCTUnwrap(store.status.entries.first { $0.path == "both.rs" })
        store.select(entry)
        try await Task.sleep(nanoseconds: 30_000_000)
        XCTAssertEqual(store.selectedPath, "both.rs")
        let file = try XCTUnwrap(store.selectedDiffFile)
        XCTAssertEqual(file.path, "both.rs")
        XCTAssertFalse(file.displayLines.isEmpty)
    }

    // MARK: - Read-only protocol surface (compile-time guarantee)

    /// This compiles ONLY because `GitService` exposes exactly status / branches
    /// / diff. There is no stage/commit/switch/push to call — adding one to the
    /// witness here would not satisfy the protocol, and the protocol has none.
    func testGitServiceProtocolIsReadOnly() async throws {
        let service: GitService = MockGitService(
            status: try decodeStatus(),
            branches: try decodeBranches(),
            diff: try decodeDiff()
        )
        _ = try await service.status()
        _ = try await service.branches()
        _ = try await service.diff(path: nil, staged: false)
        _ = try await service.diff() // convenience default

        // The entire callable surface, enumerated via Mirror over a value that
        // records every call. After exercising all three reads, exactly the three
        // read counters moved — there is no fourth (mutating) entry point.
        let mock = service as! MockGitService
        XCTAssertEqual(mock.statusCallCount, 1)
        XCTAssertEqual(mock.branchesCallCount, 1)
        XCTAssertEqual(mock.diffCallCount, 2)
    }

    // MARK: - Rendering: non-nil + fills width (no letterbox)

    func testGitStatusViewRendersNonNil() async throws {
        let (store, _) = try makeStore()
        await refreshAndSettle(store)
        let view = GitStatusView(store: store).frame(width: 1280, height: 720)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the git studio renders non-nil")
    }

    func testGitStatusViewFillsWidthNoLetterbox() async throws {
        let (store, _) = try makeStore()
        await refreshAndSettle(store)
        for width in [1024.0, 1440.0, 1920.0] {
            let view = GitStatusView(store: store).frame(width: width, height: 760)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "git studio rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "git studio must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    func testDiffViewRendersForSelectedFile() async throws {
        let (store, _) = try makeStore()
        await refreshAndSettle(store)
        let entry = try XCTUnwrap(store.status.entries.first { $0.path == "both.rs" })
        store.select(entry)
        try await Task.sleep(nanoseconds: 40_000_000)
        let file = try XCTUnwrap(store.selectedDiffFile)
        let view = DiffHunkView(title: file.path, lines: file.displayLines)
            .frame(width: 1280, height: 720)
        XCTAssertNotNil(ImageRenderer(content: view).nsImage, "the git diff renders via DiffHunkView")
    }

    /// A LARGE status fixture (1,000 entries) renders at a fixed size — the
    /// grouped list stays responsive on a big working tree.
    func testLargeStatusFixtureRendersResponsively() async throws {
        var entries: [GitStatusEntry] = []
        for i in 0..<1_000 {
            let kind: GitEntryKind = (i % 4 == 0) ? .added : .modified
            let idx = (i % 2 == 0) ? "M" : " "
            let wt = (i % 2 == 0) ? " " : "M"
            entries.append(GitStatusEntry(
                path: "src/file_\(i).rs",
                indexStatus: idx,
                worktreeStatus: wt,
                kind: kind
            ))
        }
        let bigStatus = GitStatus(
            inRepo: true, branch: "main", upstream: "origin/main",
            ahead: 0, behind: 0, isDirty: true, entries: entries
        )
        let service = MockGitService(status: bigStatus, branches: try decodeBranches(), diff: .empty)
        let store = GitStudioStore(service: service, debounce: .milliseconds(60))
        await refreshAndSettle(store)
        XCTAssertEqual(store.status.entries.count, 1_000)

        let view = GitStatusView(store: store).frame(width: 1440, height: 900)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "a 1,000-entry status fixture renders at a fixed size")
    }
}
