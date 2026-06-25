// CommitComposerView.swift — the LOCAL commit composer (PR-035).
//
// Shows the reviewed commit-preview (the staged paths that would be captured) +
// a message field. The Commit button is DISABLED unless there are staged paths
// AND a message. On commit the composer sends the preview's `index_hash` as
// `expected-index-hash`; if the live index has moved (`index_changed`) the
// composer flips to a STALE state and must be REFRESHED before committing again,
// so a commit can only ever contain the exact paths the operator reviewed.
//
// PR-036 adds an explicitly-approved push track ALONGSIDE the local commit. The
// composer offers a "Commit & Push" action that commits first (to a COMMIT
// receipt) and then enqueues a push, surfacing a `PushApprovalView` with the
// EXACT effect; the push only runs after the operator approves it (and ack's a
// protected branch). Commit and push are SEPARATE receipts here: the commit
// receipt stands while a push is pending or has failed (and is retryable).

import SwiftUI

struct CommitComposerView: View {
    @ObservedObject var store: GitStudioStore

    private var commit: GitCommitComposerState { store.commit }
    private var push: GitPushOutboxState { store.push }

    private var messageBinding: Binding<String> {
        Binding(
            get: { store.commit.message },
            set: { store.setCommitMessage($0) }
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().overlay(Theme.stroke)
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.s10) {
                    if commit.isStale { staleBanner }
                    if let error = store.mutationError { errorBanner(error) }
                    stagedSection
                    messageField
                    commitButton
                    commitAndPushButton
                    if let receipt = commit.receipt {
                        CommitReceiptView(
                            commit: receipt.commit,
                            paths: receipt.paths,
                            message: "",
                            reviewedStagedDiffHash: receipt.reviewedStagedDiffHash,
                            reviewedStagedDiffRef: receipt.reviewedStagedDiffRef,
                            integrationFinalDiffHash: receipt.integrationFinalDiffHash,
                            integrationFinalDiffRef: receipt.integrationFinalDiffRef,
                            integrationRunId: receipt.integrationRunId,
                            integrationCandidateId: receipt.integrationCandidateId,
                            onDismiss: { store.dismissReceipt() }
                        )
                    }
                    pushSection
                }
                .padding(Theme.s12)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.panel)
        .accessibilityIdentifier("git.commit.composer")
        .task { await store.refreshPushStatus() }
    }

    // MARK: - Push section (PR-036) — approval prompt + receipt + retry + outbox

    @ViewBuilder
    private var pushSection: some View {
        // The explicit approval prompt: the push only runs after the operator
        // approves the EXACT effect shown here.
        if let prompt = push.prompt {
            PushApprovalView(
                prompt: prompt,
                onAck: { store.setAckProtected($0) },
                onApprove: { Task { await store.approveAndExecutePush() } },
                onCancel: { store.dismissPushPrompt() }
            )
        }
        // A failed push: the commit stands, the push is retryable (commit preserved).
        if let retryable = push.retryable {
            PushRetrySurface(
                intent: retryable,
                message: push.error,
                onRetry: { store.retryPush() },
                onDismiss: nil
            )
        }
        // A successful push receipt (the pushed remote oid).
        if let receipt = push.receipt {
            PushReceiptView(
                remote: pushReceiptRemote,
                ref: pushReceiptRef,
                remoteOid: receipt.remoteOid,
                alreadyDone: receipt.alreadyDone,
                onDismiss: { store.dismissPushReceipt() }
            )
        }
        // A non-fatal push error not already shown by the retry surface.
        if let error = push.error, push.retryable == nil, push.prompt == nil {
            errorBanner(error)
        }
        // The relaunch-surviving push outbox (pending / approved / completed).
        if !push.status.isEmpty {
            PushOutboxSummary(status: push.status)
        }
    }

    /// The remote shown on the push receipt — the completed intent's remote when
    /// available, else the store's default remote.
    private var pushReceiptRemote: String {
        push.status.completed.last?.remote ?? store.defaultRemote
    }

    /// The ref shown on the push receipt — the completed intent's ref when
    /// available, else the store's current push ref.
    private var pushReceiptRef: String {
        push.status.completed.last?.ref ?? (store.pushRef ?? "—")
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "checkmark.circle")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.muted)
            Text("Commit")
                .font(Theme.ui(11.5, .semibold))
                .foregroundStyle(Theme.textSoft)
            Spacer(minLength: 0)
            Button {
                Task { await store.refreshCommitPreview() }
            } label: {
                Label("Refresh preview", systemImage: "arrow.clockwise")
                    .font(Theme.ui(10.5, .semibold))
                    .labelStyle(.titleAndIcon)
                    .foregroundStyle(commit.isStale ? Theme.accent : Theme.muted)
            }
            .buttonStyle(.quietAction)
            .frame(width: 150)
            .accessibilityIdentifier("git.commit.refreshPreview")
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.titlebarBottom)
    }

    // MARK: - Stale banner

    private var staleBanner: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "exclamationmark.arrow.triangle.2.circlepath")
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusWarning)
            Text("Preview is stale — the staged files changed. Refresh the preview to re-review before committing.")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.textSoft)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(GeneratedDesignTokens.colorStatusWarning.opacity(0.12))
        )
        .accessibilityIdentifier("git.commit.staleBanner")
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Commit preview is stale; refresh before committing")
    }

    private func errorBanner(_ error: String) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusDanger)
            Text(error)
                .font(Theme.ui(11))
                .foregroundStyle(Theme.textSoft)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(GeneratedDesignTokens.colorStatusDanger.opacity(0.12))
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Commit error: \(error)")
    }

    // MARK: - Staged section (the reviewed paths)

    private var stagedSection: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            HStack(spacing: Theme.s6) {
                Text("STAGED")
                    .font(Theme.ui(9.5, .bold))
                    .foregroundStyle(Theme.muted)
                Text("\(commit.stagedPaths.count)")
                    .font(Theme.mono(9.5, .semibold))
                    .foregroundStyle(Theme.faint)
                Spacer(minLength: 0)
            }
            if commit.stagedPaths.isEmpty {
                Text("Nothing staged. Stage a file from the changes list to commit it.")
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(commit.stagedPaths, id: \.self) { path in
                    HStack(spacing: Theme.s6) {
                        Image(systemName: "plus.circle.fill")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(GeneratedDesignTokens.colorStatusSuccess)
                        Text(path)
                            .font(Theme.mono(11))
                            .foregroundStyle(Theme.textSoft)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer(minLength: 0)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(Theme.bg.opacity(0.5))
        )
        .accessibilityIdentifier("git.commit.staged")
    }

    // MARK: - Message field

    private var messageField: some View {
        VStack(alignment: .leading, spacing: Theme.s4) {
            Text("MESSAGE")
                .font(Theme.ui(9.5, .bold))
                .foregroundStyle(Theme.muted)
            TextField("Describe this commit…", text: messageBinding, axis: .vertical)
                .textFieldStyle(.plain)
                .font(Theme.ui(13))
                .foregroundStyle(Theme.text)
                .lineLimit(2...6)
                .padding(.horizontal, Theme.s10)
                .padding(.vertical, Theme.s8)
                .background(
                    RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                        .fill(Theme.input)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                        .strokeBorder(Theme.stroke, lineWidth: 1)
                )
                .accessibilityIdentifier("git.commit.messageField")
        }
    }

    // MARK: - Commit button

    private var commitButton: some View {
        Button {
            Task { await store.performCommit() }
        } label: {
            Label(commit.isCommitting ? "Committing…" : "Commit", systemImage: "checkmark.seal.fill")
                .labelStyle(.titleAndIcon)
        }
        .buttonStyle(.primaryAction)
        .frame(maxWidth: .infinity)
        .disabled(!commit.canCommit)
        .accessibilityIdentifier("git.commit.button")
        .accessibilityHint(commitHint)
    }

    private var commitHint: String {
        if commit.isStale { return "Refresh the stale preview before committing" }
        if !commit.hasStaged { return "Stage at least one file to commit" }
        if !commit.hasMessage { return "Enter a commit message" }
        return "Commit the reviewed staged files locally"
    }

    // MARK: - Commit & Push button (PR-036)

    /// Commit locally AND prepare a push: this commits first (a COMMIT receipt),
    /// then enqueues a push and opens the approval prompt. The push itself does NOT
    /// run until the operator approves the exact effect — this button never pushes
    /// silently.
    private var commitAndPushButton: some View {
        Button {
            Task { await store.commitAndPush() }
        } label: {
            Label(
                commit.isCommitting ? "Committing…" : "Commit & Push…",
                systemImage: "arrow.up.circle"
            )
            .labelStyle(.titleAndIcon)
        }
        .buttonStyle(.secondaryAction)
        .frame(maxWidth: .infinity)
        .disabled(!store.canCommitAndPush)
        .accessibilityIdentifier("git.commitAndPush.button")
        .accessibilityHint(commitAndPushHint)
    }

    private var commitAndPushHint: String {
        if store.pushRef == nil { return "A detached HEAD has no branch to push" }
        if !commit.canCommit { return commitHint }
        return "Commit locally, then review and approve the push to the remote"
    }
}
