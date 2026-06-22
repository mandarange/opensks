// CommitReceiptView.swift — the receipt for a successful LOCAL commit (PR-035).
//
// After a commit lands, the studio shows the commit sha + the EXACT paths
// committed, and a `GitCommitCard` is posted into the active conversation thread
// where the same honest receipt renders inline. Because the receipt is built
// from `GitCommitResult.paths` (the precise paths the commit captured), what the
// operator sees is exactly what was committed — no more, no less. LOCAL ONLY:
// there is no push affordance here.

import SwiftUI

// MARK: - Conversation commit card model

/// A LOCAL commit card attached to a conversation thread (PR-035). Carries the
/// commit sha + the exact committed paths + the message. Not a persisted message:
/// it is a UI affordance the thread renders under its messages.
struct GitCommitCard: Identifiable, Sendable, Equatable {
    let id: String
    let commit: String
    let paths: [String]
    let message: String
    let committedAtMs: Int64

    /// First 8 chars of the commit sha for a compact, honest reference.
    var shortSha: String { String(commit.prefix(8)) }

    var committedAtDate: Date {
        Date(timeIntervalSince1970: Double(committedAtMs) / 1000.0)
    }
}

// MARK: - Receipt card

/// Renders a commit receipt: the sha, the message, and the exact paths. Shared by
/// the Git studio (post-commit confirmation) and the conversation thread (the
/// posted commit card). Full-tile, dark, token-driven; fills its container width.
struct CommitReceiptView: View {
    let commit: String
    let paths: [String]
    let message: String
    /// Optional relative-time line (shown in the conversation card).
    var subtitle: String?
    /// Optional dismiss handler (shown as an X in the studio confirmation).
    var onDismiss: (() -> Void)?

    private var shortSha: String { String(commit.prefix(8)) }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            header
            if !message.isEmpty {
                Text(message)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.textSoft)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            pathsList
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityIdentifier("git.commit.receipt.\(shortSha)")
    }

    private var header: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "checkmark.seal.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusSuccess)
            Text("Committed")
                .font(Theme.ui(12.5, .semibold))
                .foregroundStyle(Theme.text)
            Text(shortSha)
                .font(Theme.mono(11.5, .semibold))
                .foregroundStyle(Theme.muted)
                .textSelection(.enabled)
            if let subtitle {
                Text(subtitle)
                    .font(Theme.ui(10.5))
                    .foregroundStyle(Theme.faint)
            }
            Spacer(minLength: 0)
            StatusPill(kind: .success, label: "Local")
            if let onDismiss {
                Button(action: onDismiss) {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(Theme.muted)
                }
                .buttonStyle(IconTileButtonStyle(size: 24))
                .accessibilityLabel("Dismiss receipt")
            }
        }
    }

    private var pathsList: some View {
        VStack(alignment: .leading, spacing: Theme.s4) {
            HStack(spacing: Theme.s6) {
                Text("\(paths.count) FILE\(paths.count == 1 ? "" : "S")")
                    .font(Theme.ui(9.5, .bold))
                    .foregroundStyle(Theme.muted)
                Spacer(minLength: 0)
            }
            ForEach(paths, id: \.self) { path in
                HStack(spacing: Theme.s6) {
                    Image(systemName: "doc.text")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.faint)
                    Text(path)
                        .font(Theme.mono(11))
                        .foregroundStyle(Theme.textSoft)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.bg.opacity(0.6))
        )
    }

    private var accessibilityLabel: String {
        let fileList = paths.joined(separator: ", ")
        return "Committed \(shortSha): \(message). \(paths.count) files: \(fileList)"
    }
}

// MARK: - Conversation commit card

/// The conversation-thread wrapper around `CommitReceiptView` for a posted
/// `GitCommitCard`. Constrained like a message cell, leading-aligned.
struct CommitReceiptCard: View {
    let card: GitCommitCard

    var body: some View {
        CommitReceiptView(
            commit: card.commit,
            paths: card.paths,
            message: card.message,
            subtitle: RelativeTime.string(from: card.committedAtDate)
        )
        .frame(maxWidth: 720, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("conversation.commitCard.\(card.shortSha)")
    }
}
