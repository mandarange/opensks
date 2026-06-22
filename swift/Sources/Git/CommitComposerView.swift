// CommitComposerView.swift — the LOCAL commit composer (PR-035).
//
// Shows the reviewed commit-preview (the staged paths that would be captured) +
// a message field. The Commit button is DISABLED unless there are staged paths
// AND a message. On commit the composer sends the preview's `index_hash` as
// `expected-index-hash`; if the live index has moved (`index_changed`) the
// composer flips to a STALE state and must be REFRESHED before committing again,
// so a commit can only ever contain the exact paths the operator reviewed.
//
// LOCAL ONLY: there is no push button here, and the store this drives has no
// push method to call. After a successful commit the receipt is shown and a
// commit card is posted into the active conversation (wired by the host).

import SwiftUI

struct CommitComposerView: View {
    @ObservedObject var store: GitStudioStore

    private var commit: GitCommitComposerState { store.commit }

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
                    if let receipt = commit.receipt {
                        CommitReceiptView(
                            commit: receipt.commit,
                            paths: receipt.paths,
                            message: "",
                            onDismiss: { store.dismissReceipt() }
                        )
                    }
                }
                .padding(Theme.s12)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.panel)
        .accessibilityIdentifier("git.commit.composer")
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
}
