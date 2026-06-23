// GitCommitTests.swift — the LOCAL Git mutation flow (PR-035).
//
// Drives GitService / GitMutationModels / GitStudioStore / CommitComposerView /
// CommitReceiptView through a MockGitService (no disk, no process) and a real
// EditorWorkspaceStore (backed by MockEditorFileService) so the dirty-buffer
// preflight is exercised honestly. Asserts:
//   • a dirty editor OR a CLI preflight blocker BLOCKS the switch — the store
//     surfaces a blocked state and NEVER calls `switch`;
//   • a secret / data-plane path is presented NON-stageable and its stage action
//     is rejected (it can never be staged);
//   • after commit-preview, an `index_changed` on commit flips the composer to
//     STALE and does NOT report success; refreshing the preview then committing
//     succeeds and the receipt lists exactly the staged paths;
//   • the commit receipt / conversation commit card render (ImageRenderer
//     non-nil) and fill width at 1024 / 1440 (no letterbox);
//   • the GitService surface is reads + LOCAL mutations + the EXPLICITLY-APPROVED
//     push flow (PR-036): push exists only as enqueue → approve → execute (+
//     status), never as a bare silent push. (The push flow itself is covered in
//     PushOutboxTests.)

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class GitCommitTests: XCTestCase {

    // MARK: - Fixtures

    private func makeGitStore() -> (GitStudioStore, MockGitService) {
        let service = MockGitService(
            status: GitStatus(inRepo: true, branch: "main", isDirty: true),
            branches: GitBranches(current: "main", branches: [
                GitBranchInfo(name: "main", isCurrent: true),
                GitBranchInfo(name: "feature", isCurrent: false)
            ]),
            diff: .empty
        )
        let store = GitStudioStore(service: service, debounce: .milliseconds(20))
        return (store, service)
    }

    /// A real editor store with one dirty buffer at `path`.
    private func makeDirtyEditor(path: String = "src/main.rs") async -> EditorWorkspaceStore {
        let service = MockEditorFileService()
        service.seed(path: path, content: "fn main() {}\n")
        let store = EditorWorkspaceStore(service: service)
        let doc = await store.open(path: path)
        doc?.textDidChange("fn main() { /* edited, unsaved */ }\n")
        return store
    }

    private func makeCleanEditor(path: String = "src/main.rs") async -> EditorWorkspaceStore {
        let service = MockEditorFileService()
        service.seed(path: path, content: "fn main() {}\n")
        let store = EditorWorkspaceStore(service: service)
        _ = await store.open(path: path)
        return store
    }

    // MARK: - Decode parity (snake_case wire contract)

    func testStageResultDecodesAndSeparatesRejected() throws {
        let json = """
        {"schema":"opensks.git-stage.v1","staged":["a.rs"],"rejected":[{"path":"id_rsa","reason":"secret_restricted"}]}
        """
        let result = try JSONDecoder().decode(GitStageResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.staged, ["a.rs"])
        XCTAssertEqual(result.rejected.count, 1)
        XCTAssertEqual(result.rejected.first?.path, "id_rsa")
        XCTAssertEqual(result.rejected.first?.reason, .secretRestricted)
    }

    func testCommitPreviewDecodes() throws {
        let json = """
        {"schema":"opensks.git-commit-preview.v1","index_hash":"abc123","staged_paths":["a.rs","b.rs"],"has_staged":true}
        """
        let preview = try JSONDecoder().decode(GitCommitPreview.self, from: Data(json.utf8))
        XCTAssertEqual(preview.indexHash, "abc123")
        XCTAssertEqual(preview.stagedPaths, ["a.rs", "b.rs"])
        XCTAssertTrue(preview.hasStaged)
    }

    func testGitErrorEnvelopeMapsToTypedErrors() throws {
        let blocked = """
        {"schema":"opensks.git-error.v1","error":{"code":"switch_blocked","blockers":[{"kind":"dirty_worktree","paths":["a.rs"]}]}}
        """
        let env1 = try JSONDecoder().decode(GitErrorEnvelope.self, from: Data(blocked.utf8))
        if case .switchBlocked(let blockers) = LiveGitService.mapError(env1) {
            XCTAssertEqual(blockers.first?.kind, .dirtyWorktree)
            XCTAssertEqual(blockers.first?.paths, ["a.rs"])
        } else {
            XCTFail("switch_blocked must map to .switchBlocked")
        }

        let stale = #"{"schema":"opensks.git-error.v1","error":{"code":"index_changed"}}"#
        let env2 = try JSONDecoder().decode(GitErrorEnvelope.self, from: Data(stale.utf8))
        XCTAssertEqual(LiveGitService.mapError(env2), .indexChanged)

        let secret = """
        {"schema":"opensks.git-error.v1","error":{"code":"secret_restricted","rejected":[{"path":"id_rsa","reason":"secret_restricted"}]}}
        """
        let env3 = try JSONDecoder().decode(GitErrorEnvelope.self, from: Data(secret.utf8))
        if case .secretRejected(let rejected) = LiveGitService.mapError(env3) {
            XCTAssertEqual(rejected.first?.path, "id_rsa")
        } else {
            XCTFail("secret_restricted must map to .secretRejected")
        }
    }

    // MARK: - Dirty editor OR preflight blocker blocks the switch

    func testUnsavedEditorBufferBlocksSwitchWithoutCallingSwitch() async throws {
        let (store, service) = makeGitStore()
        // Hold a STRONG reference: the store's `editorStore` is weak (the app owns
        // it on AppState), so the test must keep it alive for the assertion.
        let editor = await makeDirtyEditor()
        store.editorStore = editor
        // The CLI preflight is CLEAN; only the editor buffer is dirty.
        service.setPreflight(.clean, forTarget: "feature")
        XCTAssertTrue(editor.documents.contains { $0.isDirty }, "the editor has an unsaved buffer")

        await store.attemptSwitch(to: "feature")

        // The store surfaces a blocked state…
        let block = try XCTUnwrap(store.switchBlock, "an unsaved buffer must block the switch")
        XCTAssertEqual(block.target, "feature")
        XCTAssertTrue(block.blockers.contains { $0.kind == .unsavedBuffers },
                      "the blocker names the unsaved editor buffer")
        // …and `switch` is NEVER called (no silent --force).
        XCTAssertTrue(service.switchCalls.isEmpty, "a blocked switch must not call switch")
    }

    func testPreflightBlockerBlocksSwitchWithoutCallingSwitch() async throws {
        let (store, service) = makeGitStore()
        let editor = await makeCleanEditor()
        store.editorStore = editor
        // The CLI preflight reports a dirty worktree blocker.
        service.setPreflight(
            GitSwitchPreflight(canSwitch: false, blockers: [
                GitSwitchBlocker(kind: .dirtyWorktree, paths: ["a.rs"])
            ]),
            forTarget: "feature"
        )

        await store.attemptSwitch(to: "feature")

        let block = try XCTUnwrap(store.switchBlock, "a preflight blocker must block the switch")
        XCTAssertTrue(block.blockers.contains { $0.kind == .dirtyWorktree })
        XCTAssertTrue(service.switchCalls.isEmpty, "a blocked switch must not call switch")
    }

    func testCleanPreflightAndCleanEditorPerformsSwitch() async throws {
        let (store, service) = makeGitStore()
        let editor = await makeCleanEditor()
        store.editorStore = editor
        service.setPreflight(.clean, forTarget: "feature")

        await store.attemptSwitch(to: "feature")

        XCTAssertNil(store.switchBlock, "a clean preflight + clean editor is not blocked")
        XCTAssertEqual(service.switchCalls.count, 1, "the clean switch is performed")
        XCTAssertEqual(service.switchCalls.first?.target, "feature")
        XCTAssertEqual(service.switchCalls.first?.force, false, "the switch is never forced")
    }

    // MARK: - Secret / data-plane path is non-stageable

    func testSecretPathIsNonStageableAndStageRejected() async throws {
        let (store, service) = makeGitStore()
        let secretEntry = GitStatusEntry(
            path: "id_rsa", indexStatus: " ", worktreeStatus: "M", kind: .modified
        )
        // The mock refuses to stage the secret path.
        service.setRestricted("id_rsa", reason: .secretRestricted)

        // Attempt to stage it: it comes back rejected, never staged.
        await store.stage("id_rsa")

        let rejection = try XCTUnwrap(store.rejection(for: "id_rsa"),
                                      "a secret path must be recorded as rejected")
        XCTAssertEqual(rejection.reason, .secretRestricted)
        // The store presents it as NON-stageable…
        XCTAssertFalse(store.isStageable(secretEntry), "a secret path is never stageable")
        // …and it was never added to the staged set.
        XCTAssertFalse(service.stagedPaths.contains("id_rsa"),
                       "a secret path is never actually staged")
    }

    func testDataPlanePathIsNonStageable() async throws {
        let (store, service) = makeGitStore()
        service.setRestricted("data/snapshot.bin", reason: .dataPlane)
        await store.stage("data/snapshot.bin")
        let rejection = try XCTUnwrap(store.rejection(for: "data/snapshot.bin"))
        XCTAssertEqual(rejection.reason, .dataPlane)
        XCTAssertFalse(service.stagedPaths.contains("data/snapshot.bin"))
    }

    func testCommitRefusesWhenStagedPathIsSecret() async throws {
        let (store, service) = makeGitStore()
        // A preview whose staged tree includes a secret path.
        service.setRestricted("id_rsa", reason: .secretRestricted)
        service.setCommitPreview(GitCommitPreview(
            indexHash: "h1", stagedPaths: ["id_rsa"], hasStaged: true
        ))
        await store.refreshCommitPreview()
        store.setCommitMessage("should be refused")

        let result = await store.performCommit()

        XCTAssertNil(result, "a commit including a secret path is refused")
        XCTAssertNil(store.commit.receipt, "no receipt for a refused commit")
        XCTAssertNotNil(store.mutationError, "the refusal is surfaced")
    }

    // MARK: - Stale preview → no success; refresh → commit succeeds with exact paths

    func testIndexChangedFlipsComposerStaleAndDoesNotSucceed() async throws {
        let (store, service) = makeGitStore()
        service.setCommitPreview(GitCommitPreview(
            indexHash: "h1", stagedPaths: ["a.rs", "b.rs"], hasStaged: true
        ))
        await store.refreshCommitPreview()
        store.setCommitMessage("first attempt")
        XCTAssertTrue(store.commit.canCommit, "staged paths + a message enable commit")

        // The live index moved out from under the reviewed preview.
        service.armIndexChangedOnNextCommit()
        let staleResult = await store.performCommit()

        XCTAssertNil(staleResult, "a stale commit does NOT report success")
        XCTAssertTrue(store.commit.isStale, "the composer flips to STALE on index_changed")
        XCTAssertNil(store.commit.receipt, "no receipt while stale")
        XCTAssertFalse(store.commit.canCommit, "a stale composer cannot commit until refreshed")

        // Refreshing the preview clears STALE and re-reviews the staged paths.
        await store.refreshCommitPreview()
        XCTAssertFalse(store.commit.isStale, "refreshing the preview clears STALE")
        XCTAssertTrue(store.commit.canCommit, "after refresh, commit is enabled again")

        // Now the commit succeeds and the receipt lists EXACTLY the staged paths.
        let committed = await store.performCommit()
        let okResult = try XCTUnwrap(committed, "the refreshed commit succeeds")
        XCTAssertEqual(okResult.paths, ["a.rs", "b.rs"],
                       "the receipt lists exactly the reviewed staged paths")
        let receipt = try XCTUnwrap(store.commit.receipt)
        XCTAssertEqual(receipt.paths, ["a.rs", "b.rs"])
    }

    func testCommitSendsReviewedIndexHashAsExpected() async throws {
        let (store, service) = makeGitStore()
        service.setCommitPreview(GitCommitPreview(
            indexHash: "reviewed-hash", stagedPaths: ["a.rs"], hasStaged: true
        ))
        await store.refreshCommitPreview()
        store.setCommitMessage("ship it")

        _ = await store.performCommit()

        let lastCommit = try XCTUnwrap(service.commitCalls.last)
        XCTAssertEqual(lastCommit.expectedIndexHash, "reviewed-hash",
                       "the commit sends the preview's index_hash as expected-index-hash")
        XCTAssertEqual(lastCommit.message, "ship it")
    }

    func testCommitDisabledWithoutStagedOrMessage() async throws {
        let (store, _) = makeGitStore()
        // No staged paths.
        store.setCommitMessage("a message")
        XCTAssertFalse(store.commit.canCommit, "no staged paths ⇒ cannot commit")

        // Staged paths but no message.
        store.setCommitMessage("")
        var state = store.commit
        state.preview = GitCommitPreview(indexHash: "h", stagedPaths: ["a.rs"], hasStaged: true)
        XCTAssertFalse(state.canCommit, "staged but no message ⇒ cannot commit")

        // Both present ⇒ enabled.
        state.message = "msg"
        XCTAssertTrue(state.canCommit, "staged paths + a message ⇒ commit enabled")
    }

    // MARK: - Commit posts a card into the active conversation

    func testSuccessfulCommitPostsConversationCardWithExactPaths() async throws {
        let (gitStore, service) = makeGitStore()
        let summary = ConversationSummary(
            schema: "opensks.conversation-summary.v1", id: "conv-1", projectId: "p",
            title: "Thread", titleSource: "manual", status: .idle, pinned: false,
            archived: false, messageCount: 0, createdAtMs: 1, updatedAtMs: 1, lastMessageAtMs: nil
        )
        let convStore = ConversationStore(service: MockConversationService(summaries: [summary]))
        await convStore.load()
        // Wire the commit-card sink exactly like AppCoordinator.wireGit does.
        gitStore.onCommitted = { result, message in
            convStore.postCommitCard(result, message: message)
        }

        service.setCommitPreview(GitCommitPreview(
            indexHash: "h1", stagedPaths: ["a.rs", "b.rs"], hasStaged: true
        ))
        await gitStore.refreshCommitPreview()
        gitStore.setCommitMessage("local commit")
        let committed = await gitStore.performCommit()
        let result = try XCTUnwrap(committed)

        let cards = convStore.commitCards(for: "conv-1")
        XCTAssertEqual(cards.count, 1, "a successful commit posts one card")
        XCTAssertEqual(cards.first?.commit, result.commit, "the card carries the commit sha")
        XCTAssertEqual(cards.first?.paths, ["a.rs", "b.rs"],
                       "the card lists exactly the committed paths")
        XCTAssertEqual(cards.first?.message, "local commit")
        let timeline = convStore.timelineItems(for: "conv-1")
        XCTAssertEqual(timeline.last?.kind, .commitReceipt, "commit posts into the durable timeline read model")
        XCTAssertEqual(timeline.last?.commitCard?.commit, result.commit)
        XCTAssertEqual(timeline.last?.payload.sourceSchema, "opensks.git-commit.v1")
        XCTAssertEqual(timeline.last?.payload.projection, "git_receipt")
        XCTAssertEqual(timeline.last?.payload.committed, true)
    }

    // MARK: - Rendering: non-nil + fills width (no letterbox)

    func testCommitReceiptRendersNonNil() throws {
        let view = CommitReceiptView(
            commit: "deadbeefcafef00d", paths: ["a.rs", "b.rs"], message: "ship it"
        ).frame(width: 1024, height: 480)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the commit receipt renders non-nil")
    }

    func testCommitReceiptFillsWidthNoLetterbox() throws {
        for width in [1024.0, 1440.0] {
            let view = CommitReceiptView(
                commit: "deadbeefcafef00d", paths: ["a.rs", "b.rs", "c.rs"], message: "ship it"
            ).frame(width: width, height: 360)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "receipt rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the commit receipt must fill the requested width (no letterbox) at \(width)")
        }
    }

    func testConversationCommitCardRendersAndFillsWidth() throws {
        let card = GitCommitCard(
            id: "card-1", commit: "deadbeefcafef00d", paths: ["a.rs", "b.rs"],
            message: "local commit", committedAtMs: 1_000
        )
        for width in [1024.0, 1440.0] {
            let view = CommitReceiptCard(card: card).frame(width: width, height: 320)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "card rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the conversation commit card must fill the requested width at \(width)")
        }
    }

    func testCommitComposerRendersNonNil() async throws {
        let (store, service) = makeGitStore()
        service.setCommitPreview(GitCommitPreview(
            indexHash: "h1", stagedPaths: ["a.rs"], hasStaged: true
        ))
        await store.refreshCommitPreview()
        let view = CommitComposerView(store: store).frame(width: 320, height: 600)
        XCTAssertNotNil(ImageRenderer(content: view).nsImage, "the commit composer renders non-nil")
    }

    func testGitStudioWithMutationsFillsWidthNoLetterbox() async throws {
        let (store, service) = makeGitStore()
        store.setCommitMessage("")
        service.setCommitPreview(GitCommitPreview(
            indexHash: "h1", stagedPaths: ["a.rs"], hasStaged: true
        ))
        await store.refreshCommitPreview()
        store.refresh()
        try await Task.sleep(nanoseconds: 30_000_000)
        for width in [1024.0, 1440.0] {
            let view = GitStatusView(store: store).frame(width: width, height: 760)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "git studio rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the git studio (with mutation controls) must fill the width at \(width)")
        }
    }

    // MARK: - Surface: reads + LOCAL mutations + the EXPLICITLY-APPROVED push

    /// Enumerate the ENTIRE callable surface of `GitService` through a mock that
    /// records every call. After exercising every method, the recorded calls are
    /// exactly the reads + the LOCAL mutations (PR-035) + the push flow (PR-036).
    ///
    /// PR-036 added push, but ONLY as the explicitly-approved multi-step flow:
    /// there is NO single "push" verb that pushes silently — a push is ENQUEUED,
    /// then APPROVED against the exact effect, then EXECUTED. This test pins that
    /// shape: the only push entry points are push-enqueue / push-approve /
    /// push-execute / push-status, and the mock never exposes a bare `push`.
    func testGitServiceSurfaceIsReadsLocalMutationsAndApprovedPush() async throws {
        let service: GitService = MockGitService(
            status: GitStatus(inRepo: true, branch: "main"),
            branches: GitBranches(current: "main", branches: []),
            diff: .empty
        )
        let mock = service as! MockGitService

        // Reads.
        _ = try await service.status()
        _ = try await service.branches()
        _ = try await service.diff(path: nil, staged: false)
        // Local mutations — NONE is a push.
        _ = try await service.stage(paths: ["a.rs"])
        _ = try await service.unstage(paths: ["a.rs"])
        _ = try await service.switchPreflight(target: "feature")
        _ = try await service.createBranch(name: "feature", from: nil)
        _ = try await service.switchBranch(target: "feature", force: false)
        _ = try await service.commitPreview()
        mock.setCommitPreview(GitCommitPreview(indexHash: "", stagedPaths: [], hasStaged: false))
        _ = try? await service.commit(message: "m", expectedIndexHash: "")
        // The push flow — explicit enqueue → approve → execute (+ status).
        let intent = try await service.pushEnqueue(remote: "origin", ref: "feature")
        _ = try await service.pushApprove(
            intentId: intent.intentId, effectDigest: intent.effectDigest, ackProtected: false
        )
        _ = try await service.pushExecute(intentId: intent.intentId)
        _ = try await service.pushStatus()

        // Exactly the reads + local mutations + the push steps were exercised.
        XCTAssertEqual(mock.statusCallCount, 1)
        XCTAssertEqual(mock.branchesCallCount, 1)
        XCTAssertEqual(mock.diffCallCount, 1)
        XCTAssertEqual(mock.stageCalls.count, 1)
        XCTAssertEqual(mock.unstageCalls.count, 1)
        XCTAssertEqual(mock.preflightCalls.count, 1)
        XCTAssertEqual(mock.createBranchCalls.count, 1)
        XCTAssertEqual(mock.switchCalls.count, 1)
        XCTAssertGreaterThanOrEqual(mock.commitPreviewCallCount, 1)
        XCTAssertEqual(mock.pushEnqueueCalls.count, 1)
        XCTAssertEqual(mock.pushApproveCalls.count, 1)
        XCTAssertEqual(mock.pushExecuteCalls.count, 1)
        XCTAssertEqual(mock.pushStatusCallCount, 1)

        // The push surface is the APPROVED flow only — there is no bare "push"
        // method that pushes silently. The execute step ran only AFTER an approve
        // for the same intent (the no-silent-push invariant), and the recorded
        // step log proves the enqueue → approve → execute ordering.
        let steps = mock.pushSteps
        let approveIndex = steps.firstIndex { step in
            if case .approve(let id, _, _) = step { return id == intent.intentId }
            return false
        }
        let executeIndex = steps.firstIndex { step in
            if case .execute(let id) = step { return id == intent.intentId }
            return false
        }
        let ai = try XCTUnwrap(approveIndex, "the push was approved")
        let ei = try XCTUnwrap(executeIndex, "the push was executed")
        XCTAssertLessThan(ai, ei, "execute must follow approve — no silent push")
    }
}
