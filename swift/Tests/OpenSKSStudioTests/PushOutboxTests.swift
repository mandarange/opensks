// PushOutboxTests.swift — the EXPLICITLY-APPROVED push flow (PR-036).
//
// Drives GitService / PushOutboxModels / GitStudioStore / PushApprovalView /
// PushReceiptView through a MockGitService (no disk, no process, no remote) so the
// invariant — a push always requires the operator to approve the EXACT effect, and
// there is NO silent push — is exercised honestly. Asserts:
//   • "Commit & Push" produces a COMMIT receipt FIRST; the push stays pending
//     until approval; execute is NEVER called before approve (the mock records a
//     globally-ordered step log and we assert no execute precedes its approve);
//   • a PROTECTED branch requires the explicit extra ack before approval is
//     allowed (Approve is disabled, and the store does not push without the ack);
//   • an approval whose digest no longer matches (the mock returns
//     `digest_mismatch`) does NOT push and surfaces the mismatch (prompt stays
//     open, no receipt, no push card);
//   • a `push_failed` keeps the COMMIT receipt intact and marks the push retryable
//     (the local commit is preserved);
//   • the approval prompt + both receipts render (ImageRenderer non-nil) and fill
//     width at 1024 / 1440 (no letterbox);
//   • the push outbox (pending / approved / completed) is recovered from
//     push-status so it survives relaunch.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class PushOutboxTests: XCTestCase {

    // MARK: - Fixtures

    /// A git store on a clean-ish dirty repo with a single staged file ready to
    /// commit, and a scriptable push.
    private func makeStore(
        branch: String = "feature",
        preview: GitCommitPreview = GitCommitPreview(indexHash: "h1", stagedPaths: ["a.rs"], hasStaged: true)
    ) async -> (GitStudioStore, MockGitService) {
        let service = MockGitService(
            status: GitStatus(inRepo: true, branch: branch, isDirty: true),
            branches: GitBranches(current: branch, branches: [
                GitBranchInfo(name: branch, isCurrent: true)
            ]),
            diff: .empty
        )
        let store = GitStudioStore(service: service, debounce: .milliseconds(20))
        service.setCommitPreview(preview)
        await store.refreshCommitPreview()
        store.refresh()
        try? await Task.sleep(nanoseconds: 30_000_000)
        store.setCommitMessage("ship it")
        return (store, service)
    }

    /// A non-protected intent the mock returns from push-enqueue.
    private func intent(
        id: String = "intent-1",
        digest: String = "digest-1",
        ref: String = "feature",
        protected: Bool = false,
        remoteExpected: String? = "00aabbccddeeff00112233445566778899aabbcc"
    ) -> GitPushIntent {
        GitPushIntent(
            intentId: id,
            effectDigest: digest,
            remote: "origin",
            remoteUrlRedacted: "https://github.example/<redacted>.git",
            ref: ref,
            localOid: "feedface00000000feedface00000000feedface",
            remoteExpectedOid: remoteExpected,
            protected: protected
        )
    }

    private func failure(
        id: String = "failure-1",
        ref: String = "feature",
        attempts: Int = 1
    ) -> GitPushFailureDiagnostic {
        GitPushFailureDiagnostic(
            intentId: id,
            idempotencyKey: "push:\(id):feedface",
            effectDigest: "digest-\(id)",
            remote: "origin",
            remoteUrlRedacted: "https://github.example/<redacted>.git",
            ref: ref,
            localOid: "feedface00000000feedface00000000feedface",
            remoteExpectedOid: nil,
            reasonCode: "push_failed",
            attempts: attempts
        )
    }

    // MARK: - Decode parity (snake_case wire contract)

    func testPushIntentDecodes() throws {
        let json = """
        {"schema":"opensks.push-intent.v1","intent_id":"i1","effect_digest":"d1","remote":"origin","remote_url_redacted":"https://x/<redacted>.git","ref":"feature","local_oid":"abc123","remote_expected_oid":"def456","protected":false}
        """
        let intent = try JSONDecoder().decode(GitPushIntent.self, from: Data(json.utf8))
        XCTAssertEqual(intent.intentId, "i1")
        XCTAssertEqual(intent.effectDigest, "d1")
        XCTAssertEqual(intent.remote, "origin")
        XCTAssertEqual(intent.remoteUrlRedacted, "https://x/<redacted>.git")
        XCTAssertEqual(intent.ref, "feature")
        XCTAssertEqual(intent.localOid, "abc123")
        XCTAssertEqual(intent.remoteExpectedOid, "def456")
        XCTAssertFalse(intent.protected)
    }

    func testPushReceiptDecodesIdempotency() throws {
        let json = """
        {"schema":"opensks.push-receipt.v1","pushed":true,"remote_oid":"deadbeef","idempotency_key":"k1","already_done":true}
        """
        let receipt = try JSONDecoder().decode(GitPushReceipt.self, from: Data(json.utf8))
        XCTAssertTrue(receipt.pushed)
        XCTAssertEqual(receipt.remoteOid, "deadbeef")
        XCTAssertEqual(receipt.idempotencyKey, "k1")
        XCTAssertTrue(receipt.alreadyDone)
    }

    func testPushApprovalDecodesMatched() throws {
        let json = """
        {"schema":"opensks.push-approval.v1","approval_id":"ap1","intent_id":"i1","matched":true}
        """
        let approval = try JSONDecoder().decode(GitPushApproval.self, from: Data(json.utf8))
        XCTAssertEqual(approval.approvalId, "ap1")
        XCTAssertEqual(approval.intentId, "i1")
        XCTAssertTrue(approval.matched)
    }

    func testPushStatusDecodesPendingApprovedCompleted() throws {
        let json = """
        {"schema":"opensks.push-status.v1",
         "pending":[{"intent_id":"p1","effect_digest":"d","remote":"origin","remote_url_redacted":"u","ref":"feature","local_oid":"a","remote_expected_oid":null,"protected":false}],
         "approved":[{"intent_id":"a1","effect_digest":"d","remote":"origin","remote_url_redacted":"u","ref":"main","local_oid":"b","remote_expected_oid":"c","protected":true}],
         "failures":[{"schema":"opensks.push-failure-diagnostic.v1","intent_id":"f1","idempotency_key":"push:f1:b","effect_digest":"d","remote":"origin","remote_url_redacted":"u","ref":"feature","local_oid":"b","remote_expected_oid":"c","code":"push_failed","reason_code":"push_failed","attempts":2}],
         "completed":[{"intent_id":"c1","ref":"feature","remote":"origin","remote_oid":"cafebabe"}]}
        """
        let status = try JSONDecoder().decode(GitPushStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.pending.count, 1)
        XCTAssertEqual(status.pending.first?.intentId, "p1")
        XCTAssertEqual(status.approved.count, 1)
        XCTAssertTrue(status.approved.first?.protected ?? false)
        XCTAssertEqual(status.failures.count, 1)
        XCTAssertEqual(status.failures.first?.intentId, "f1")
        XCTAssertEqual(status.failures.first?.reasonCode, "push_failed")
        XCTAssertEqual(status.failures.first?.attempts, 2)
        XCTAssertEqual(status.completed.count, 1)
        XCTAssertEqual(status.completed.first?.remoteOid, "cafebabe")
    }

    func testPushErrorEnvelopeMapsToTypedPushErrors() throws {
        let codes: [(String, GitPushError)] = [
            ("digest_mismatch", .digestMismatch),
            ("no_matching_approval", .noMatchingApproval),
            ("protected_branch", .protectedBranch),
        ]
        for (code, expected) in codes {
            let json = #"{"schema":"opensks.git-error.v1","error":{"code":"\#(code)"}}"#
            let env = try JSONDecoder().decode(GitErrorEnvelope.self, from: Data(json.utf8))
            XCTAssertEqual(LiveGitService.mapPushError(env), expected, "\(code) must map to \(expected)")
        }
        let failed = #"{"schema":"opensks.git-error.v1","error":{"code":"push_failed","message":"remote unreachable"}}"#
        let env = try JSONDecoder().decode(GitErrorEnvelope.self, from: Data(failed.utf8))
        XCTAssertEqual(LiveGitService.mapPushError(env), .pushFailed(message: "remote unreachable"))
        // A non-push code yields nil so the caller falls back to the generic error.
        let other = #"{"schema":"opensks.git-error.v1","error":{"code":"index_changed"}}"#
        let env2 = try JSONDecoder().decode(GitErrorEnvelope.self, from: Data(other.utf8))
        XCTAssertNil(LiveGitService.mapPushError(env2), "a non-push code is not a push error")
    }

    // MARK: - Commit & Push: commit receipt FIRST, push pending until approval

    func testCommitAndPushCommitsFirstThenLeavesPushPendingUntilApproval() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent())

        await store.commitAndPush()

        // 1. A COMMIT receipt exists FIRST (the commit succeeded independently).
        let commitReceipt = try XCTUnwrap(store.commit.receipt, "Commit & Push records a commit receipt first")
        XCTAssertEqual(commitReceipt.paths, ["a.rs"])
        XCTAssertEqual(service.commitCalls.count, 1, "the commit ran exactly once")

        // 2. The push is ENQUEUED and a prompt is awaiting approval…
        XCTAssertEqual(service.pushEnqueueCalls.count, 1, "the push was enqueued")
        let prompt = try XCTUnwrap(store.push.prompt, "the push stays pending behind an approval prompt")
        XCTAssertEqual(prompt.intent.ref, "feature")

        // 3. …but execute is NEVER called before approve (no silent push).
        XCTAssertTrue(service.pushApproveCalls.isEmpty, "no approval until the operator approves")
        XCTAssertTrue(service.pushExecuteCalls.isEmpty, "execute is NOT called before approval")
        XCTAssertNil(store.push.receipt, "no push receipt before approval")
    }

    func testApproveThenExecutePushesAndRecordsReceipt_orderingNoExecuteBeforeApprove() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent())

        await store.commitAndPush()
        // Approve the exact effect → execute runs.
        await store.approveAndExecutePush()

        // The push executed exactly once, with a receipt.
        XCTAssertEqual(service.pushApproveCalls.count, 1)
        XCTAssertEqual(service.pushExecuteCalls.count, 1)
        let receipt = try XCTUnwrap(store.push.receipt, "an approved push records a receipt")
        XCTAssertTrue(receipt.pushed)
        XCTAssertFalse(receipt.alreadyDone)
        XCTAssertNil(store.push.prompt, "the prompt closes after a successful push")

        // ORDERING: for every intent, no execute step precedes its approve step.
        assertNoExecuteBeforeApprove(service.pushSteps)
        // And the COMMIT receipt is still standing independently.
        XCTAssertNotNil(store.commit.receipt, "the commit receipt stands after the push")
    }

    func testApproveSendsTheIntentsEffectDigest() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent(digest: "the-exact-digest"))
        await store.commitAndPush()
        await store.approveAndExecutePush()
        let approveCall = try XCTUnwrap(service.pushApproveCalls.first)
        XCTAssertEqual(approveCall.digest, "the-exact-digest",
                       "approval carries the intent's effect_digest")
    }

    // MARK: - Protected branch requires the extra ack

    func testProtectedBranchRequiresAckBeforeApproval() async throws {
        let (store, service) = await makeStore(branch: "main")
        service.setPushIntent(intent(id: "intent-prot", ref: "main", protected: true))

        await store.commitAndPush()
        let prompt = try XCTUnwrap(store.push.prompt)
        XCTAssertTrue(prompt.intent.protected, "the prompt flags the protected branch")

        // Without the ack, Approve is NOT allowed and approving is a no-op.
        XCTAssertFalse(prompt.canApprove, "a protected push cannot be approved without the ack")
        await store.approveAndExecutePush()
        XCTAssertTrue(service.pushApproveCalls.isEmpty, "no approval without the protected-branch ack")
        XCTAssertTrue(service.pushExecuteCalls.isEmpty, "no push without the protected-branch ack")
        XCTAssertNotNil(store.push.prompt, "the prompt stays open awaiting the ack")

        // After ticking the ack, the protected push proceeds.
        store.setAckProtected(true)
        XCTAssertTrue(try XCTUnwrap(store.push.prompt).canApprove, "ack enables approval")
        await store.approveAndExecutePush()
        let approveCall = try XCTUnwrap(service.pushApproveCalls.first)
        XCTAssertTrue(approveCall.ackProtected, "the approval carries --ack-protected")
        XCTAssertEqual(service.pushExecuteCalls.count, 1, "the acked protected push executes")
        XCTAssertNotNil(store.push.receipt, "the acked protected push records a receipt")
    }

    // MARK: - Digest mismatch: no push, mismatch surfaced

    func testDigestMismatchOnApproveDoesNotPushAndSurfacesMismatch() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent())
        await store.commitAndPush()

        // The reviewed effect moved out from under us: approve returns mismatch.
        service.armDigestMismatchOnNextApprove()
        await store.approveAndExecutePush()

        // NO push happened…
        XCTAssertTrue(service.pushExecuteCalls.isEmpty, "a digest mismatch must NOT push")
        XCTAssertNil(store.push.receipt, "no receipt on a digest mismatch")
        // …and the mismatch is surfaced; the prompt stays OPEN to re-review.
        let prompt = try XCTUnwrap(store.push.prompt, "the prompt stays open after a mismatch")
        let notice = try XCTUnwrap(prompt.notice, "the mismatch is surfaced in the prompt")
        XCTAssertTrue(notice.lowercased().contains("moved") || notice.lowercased().contains("re-review"),
                      "the notice explains the effect changed")
        // The COMMIT receipt is unaffected.
        XCTAssertNotNil(store.commit.receipt, "the commit receipt stands despite the mismatch")
    }

    func testDigestChangedAfterApprovalRefusesExecute() async throws {
        // Approve succeeds, then the intent's digest moves before execute → the
        // execute step itself refuses with a mismatch (belt-and-suspenders).
        let (store, service) = await makeStore()
        service.setPushIntent(intent(id: "intent-move", digest: "d-approve"))
        await store.commitAndPush()
        // Move the digest AFTER approval but BEFORE the store reaches execute is
        // not directly reachable through approveAndExecutePush (atomic), so drive
        // the service directly to assert the contract at the service boundary.
        _ = try await service.pushApprove(intentId: "intent-move", effectDigest: "d-approve", ackProtected: false)
        service.moveDigest(forIntent: "intent-move", to: "d-changed")
        do {
            _ = try await service.pushExecute(intentId: "intent-move")
            XCTFail("execute must refuse when the digest changed since approval")
        } catch let error as GitPushError {
            XCTAssertEqual(error, .digestMismatch)
        }
    }

    // MARK: - push_failed: commit intact, push retryable

    func testPushFailedKeepsCommitAndMarksPushRetryable() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent())
        await store.commitAndPush()
        let commitReceipt = try XCTUnwrap(store.commit.receipt)

        // The remote is unreachable: execute fails.
        service.armPushFailedOnNextExecute(message: "remote unreachable")
        await store.approveAndExecutePush()

        // The COMMIT receipt is intact (the local commit is preserved)…
        XCTAssertEqual(store.commit.receipt, commitReceipt, "the commit receipt survives a failed push")
        // …no push receipt…
        XCTAssertNil(store.push.receipt, "a failed push records no success receipt")
        // …and the push is RETRYABLE for the SAME intent.
        let retryable = try XCTUnwrap(store.push.retryable, "a failed push is retryable")
        XCTAssertEqual(retryable.intentId, "intent-1")
        XCTAssertNotNil(store.push.error, "the failure is surfaced")
        XCTAssertNil(store.push.prompt, "the prompt closed; a retry re-opens it")

        // Retry re-opens the approval prompt for the SAME effect (no re-commit).
        store.retryPush()
        let reprompt = try XCTUnwrap(store.push.prompt, "retry re-opens the approval prompt")
        XCTAssertEqual(reprompt.intent.intentId, "intent-1")
        XCTAssertEqual(service.commitCalls.count, 1, "retry never re-commits")

        // The retried push succeeds.
        await store.approveAndExecutePush()
        XCTAssertNotNil(store.push.receipt, "the retried push succeeds")
        XCTAssertNil(store.push.retryable, "a succeeded retry clears the retryable state")
    }

    // MARK: - Idempotent execute (already_done)

    func testRepeatExecuteIsIdempotentAlreadyDone() async throws {
        let (_, service) = await makeStore()
        service.setPushIntent(intent(id: "intent-idem"))
        _ = try await service.pushEnqueue(remote: "origin", ref: "feature")
        _ = try await service.pushApprove(intentId: "intent-idem", effectDigest: "digest-1", ackProtected: false)
        let first = try await service.pushExecute(intentId: "intent-idem")
        XCTAssertFalse(first.alreadyDone, "the first execute actually pushes")
        let second = try await service.pushExecute(intentId: "intent-idem")
        XCTAssertTrue(second.alreadyDone, "a repeat execute reports already_done")
        XCTAssertEqual(first.remoteOid, second.remoteOid, "the same remote oid; pushed exactly once")
    }

    func testExecuteWithoutApprovalIsRefused() async throws {
        let (_, service) = await makeStore()
        service.setPushIntent(intent(id: "intent-noapprove"))
        _ = try await service.pushEnqueue(remote: "origin", ref: "feature")
        do {
            _ = try await service.pushExecute(intentId: "intent-noapprove")
            XCTFail("execute without an approval must be refused")
        } catch let error as GitPushError {
            XCTAssertEqual(error, .noMatchingApproval)
        }
    }

    // MARK: - push-status recovered after relaunch

    func testPushStatusRecoversOutboxAfterRelaunch() async throws {
        // A FRESH store (simulating a relaunch) reads push-status and recovers the
        // pending / approved / completed outbox from "SQLite".
        let service = MockGitService(
            status: GitStatus(inRepo: true, branch: "feature"),
            branches: GitBranches(current: "feature", branches: []),
            diff: .empty
        )
        service.setPushStatus(GitPushStatus(
            pending: [intent(id: "p1")],
            approved: [intent(id: "a1", ref: "main", protected: true)],
            failures: [failure(id: "f1")],
            completed: [GitPushCompleted(intentId: "c1", ref: "feature", remote: "origin", remoteOid: "cafebabe0000")]
        ))
        let store = GitStudioStore(service: service)

        await store.refreshPushStatus()

        XCTAssertEqual(store.push.status.pending.count, 1, "pending recovered after relaunch")
        XCTAssertEqual(store.push.status.approved.count, 1, "approved recovered after relaunch")
        XCTAssertEqual(store.push.status.failures.count, 1, "failed push diagnostic recovered after relaunch")
        XCTAssertEqual(store.push.status.completed.count, 1, "completed recovered after relaunch")
        XCTAssertEqual(store.push.status.completed.first?.remoteOid, "cafebabe0000")
        XCTAssertTrue(store.push.hasOutbox, "the recovered outbox is surfaced")
    }

    func testPushOutboxSummaryRendersRecoveredFailureDiagnostics() throws {
        let status = GitPushStatus(
            pending: [intent(id: "p1")],
            approved: [intent(id: "a1")],
            failures: [failure(id: "f1", attempts: 2)],
            completed: [GitPushCompleted(intentId: "c1", ref: "feature", remote: "origin", remoteOid: "cafebabe0000")]
        )
        XCTAssertFalse(status.isEmpty, "failure diagnostics make the outbox visible")
        for width in [1024.0, 1440.0] {
            let view = PushOutboxSummary(status: status).frame(width: width, height: 220)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "push outbox summary rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the push outbox failure summary must fill the width at \(width)")
        }
    }

    func testPushFailureDiagnosticsPanelRendersRecoveredFailureDetails() throws {
        let failures = [
            failure(id: "f1", attempts: 2),
            failure(id: "f2", ref: "main", attempts: 3)
        ]
        for width in [1024.0, 1440.0] {
            let view = PushFailureDiagnosticsPanel(failures: failures, onReview: {})
                .frame(width: width, height: 260)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "failure diagnostics panel rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the recovered failed-push diagnostics panel must fill the width at \(width)")
        }
    }

    func testGitStudioCompactRendersRecoveredFailureDiagnosticsOutsideCommitPane() async throws {
        let service = MockGitService(
            status: GitStatus(inRepo: true, branch: "feature"),
            branches: GitBranches(current: "feature", branches: []),
            diff: .empty
        )
        service.setPushStatus(GitPushStatus(failures: [failure(id: "f1", attempts: 2)]))
        let store = GitStudioStore(service: service)
        await store.refreshPushStatus()
        XCTAssertEqual(store.push.status.failures.count, 1)

        let view = GitStatusView(store: store).frame(width: 1024, height: 760)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        let image = try XCTUnwrap(renderer.nsImage, "compact git studio rendered with recovered push failure")
        XCTAssertEqual(image.size.width, 1024, accuracy: 1.0,
                       "the compact Git studio must fill the width while showing recovered push diagnostics")
    }

    // MARK: - Commit & Push posts a SEPARATE push card into the conversation

    func testSuccessfulPushPostsSeparatePushCard() async throws {
        let (gitStore, service) = await makeStore()
        let summary = ConversationSummary(
            schema: "opensks.conversation-summary.v1", id: "conv-1", projectId: "p",
            title: "Thread", titleSource: .generated, status: .idle, pinned: false,
            archived: false, messageCount: 0, createdAtMs: 1, updatedAtMs: 1, lastMessageAtMs: nil
        )
        let convStore = ConversationStore(service: MockConversationService(summaries: [summary]))
        await convStore.load()
        // Wire BOTH sinks exactly like AppCoordinator.wireGit does.
        gitStore.onCommitted = { result, message in convStore.postCommitCard(result, message: message) }
        gitStore.onPushed = { receipt, intent, approval in
            convStore.postPushCard(receipt, intent: intent, approval: approval)
        }

        service.setPushIntent(intent())
        await gitStore.commitAndPush()
        // The commit card is posted immediately; the push card only after approval.
        XCTAssertEqual(convStore.commitCards(for: "conv-1").count, 1, "the commit card posts on commit")
        XCTAssertTrue(convStore.pushCards(for: "conv-1").isEmpty, "no push card before approval")
        let commitReceipt = try XCTUnwrap(gitStore.commit.receipt)

        await gitStore.approveAndExecutePush()
        let pushCards = convStore.pushCards(for: "conv-1")
        XCTAssertEqual(pushCards.count, 1, "an approved push posts one SEPARATE push card")
        let receipt = try XCTUnwrap(gitStore.push.receipt)
        XCTAssertEqual(pushCards.first?.remoteOid, receipt.remoteOid, "the card carries the pushed remote oid")
        XCTAssertEqual(pushCards.first?.ref, "feature")
        // Commit and push are two separate receipts.
        XCTAssertEqual(convStore.commitCards(for: "conv-1").count, 1, "the commit card stands separately")
        let timeline = convStore.timelineItems(for: "conv-1")
        XCTAssertEqual(timeline.map(\.kind), [.commitReceipt, .pushReceipt])
        XCTAssertEqual(timeline.last?.pushCard?.remoteOid, receipt.remoteOid)
        XCTAssertEqual(timeline.last?.payload.intentId, "intent-1")
        XCTAssertEqual(timeline.last?.payload.effectDigest, "digest-1")
        XCTAssertEqual(timeline.last?.payload.idempotencyKey, receipt.idempotencyKey)
        XCTAssertEqual(timeline.last?.payload.approvalMatched, true)
        XCTAssertFalse(timeline.last?.payload.protected ?? true)
        let durableCommit = await waitForTimelineItem(
            convStore,
            conversationID: "conv-1",
            id: "timeline-event-git-commit:\(commitReceipt.commit)"
        )
        XCTAssertEqual(durableCommit?.kind, .commitReceipt)
        XCTAssertEqual(durableCommit?.commitCard?.commit, commitReceipt.commit)
        let durablePush = await waitForTimelineItem(
            convStore,
            conversationID: "conv-1",
            id: "timeline-event-git-push:\(receipt.idempotencyKey)"
        )
        XCTAssertEqual(durablePush?.kind, .pushReceipt)
        XCTAssertEqual(durablePush?.pushCard?.remoteOid, receipt.remoteOid)
        XCTAssertEqual(durablePush?.payload.projection, "git_receipt")
    }

    // MARK: - Rendering: approval prompt + both receipts non-nil + fill width

    func testPushApprovalPromptRendersNonNilAndFillsWidth() throws {
        let prompt = GitPushPrompt(intent: intent(protected: true), ackProtected: false)
        for width in [1024.0, 1440.0] {
            let view = PushApprovalView(
                prompt: prompt, onAck: { _ in }, onApprove: {}, onCancel: {}
            ).frame(width: width, height: 420)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "approval prompt rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the push approval prompt must fill the width (no letterbox) at \(width)")
        }
    }

    func testPushReceiptRendersNonNilAndFillsWidth() throws {
        for width in [1024.0, 1440.0] {
            let view = PushReceiptView(
                remote: "origin", ref: "feature", remoteOid: "cafebabe0000deadbeef", alreadyDone: false
            ).frame(width: width, height: 200)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "push receipt rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the push receipt must fill the width at \(width)")
        }
    }

    func testConversationPushCardRendersAndFillsWidth() throws {
        let card = GitPushCard(
            id: "card-1", remote: "origin", ref: "feature",
            remoteOid: "cafebabe0000deadbeef", localOid: "feedface0000",
            alreadyDone: false, pushedAtMs: 1_000
        )
        for width in [1024.0, 1440.0] {
            let view = PushReceiptCard(card: card).frame(width: width, height: 240)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "push card rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the conversation push card must fill the width at \(width)")
        }
    }

    func testPushRetrySurfaceRendersAndFillsWidth() throws {
        for width in [1024.0, 1440.0] {
            let view = PushRetrySurface(
                intent: intent(), message: "remote unreachable", onRetry: {}, onDismiss: nil
            ).frame(width: width, height: 220)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "retry surface rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the retry surface must fill the width at \(width)")
        }
    }

    func testCommitComposerWithPushPromptRendersNonNil() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent(protected: true, remoteExpected: nil))
        await store.commitAndPush()
        XCTAssertNotNil(store.push.prompt, "the composer is showing the approval prompt")
        let view = CommitComposerView(store: store).frame(width: 320, height: 800)
        XCTAssertNotNil(ImageRenderer(content: view).nsImage, "the composer + approval prompt render non-nil")
    }

    func testGitStudioWithPushPromptFillsWidthNoLetterbox() async throws {
        let (store, service) = await makeStore()
        service.setPushIntent(intent())
        await store.commitAndPush()
        for width in [1024.0, 1440.0] {
            let view = GitStatusView(store: store).frame(width: width, height: 760)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "git studio rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "the git studio (with the push prompt) must fill the width at \(width)")
        }
    }

    // MARK: - Helpers

    /// Assert that in the globally-ordered push step log, no `.execute` for an
    /// intent appears before that intent's first `.approve`. This is the core
    /// "no silent push" invariant: a push is never executed before it is approved.
    private func assertNoExecuteBeforeApprove(
        _ steps: [MockGitService.PushStep],
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        var approvedSoFar: Set<String> = []
        for step in steps {
            switch step {
            case .approve(let intentId, _, _):
                approvedSoFar.insert(intentId)
            case .execute(let intentId):
                XCTAssertTrue(approvedSoFar.contains(intentId),
                              "execute(\(intentId)) ran before its approve — a silent push",
                              file: file, line: line)
            case .enqueue, .status:
                break
            }
        }
    }
}

@MainActor
private func waitForTimelineItem(
    _ store: ConversationStore,
    conversationID: String,
    id: String,
    file: StaticString = #filePath,
    line: UInt = #line
) async -> ConversationTimelineItem? {
    for _ in 0..<50 {
        if let item = store.timelineItems(for: conversationID).first(where: { $0.id == id }) {
            return item
        }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    XCTFail("timed out waiting for timeline item \(id)", file: file, line: line)
    return nil
}
