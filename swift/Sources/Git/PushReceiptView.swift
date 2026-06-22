// PushReceiptView.swift — the receipt for a SUCCESSFUL push (PR-036), plus the
// conversation push card and the retryable-failed-push surface.
//
// Commit and push are SEPARATE receipts. A push receipt confirms exactly what
// landed on the remote: the remote oid the ref now points at, the remote name,
// and whether this execute actually pushed or was idempotently `already done`.
// Because the receipt is built from `GitPushReceipt.remoteOid`, what the operator
// sees is exactly where the remote is now — no more, no less.
//
// A FAILED push is NOT a receipt: it is a retryable surface that shows the commit
// as done and the push as retryable (the LOCAL commit is preserved). The operator
// retries the SAME effect (re-approving it) — there is no silent re-push.
//
// Dark, token-driven; fills its container width (no letterbox).

import SwiftUI

// MARK: - Conversation push card model

/// A push card attached to a conversation thread (PR-036). Carries the pushed
/// remote oid + the remote/ref + whether it was already done. Not a persisted
/// message: it is a UI affordance the thread renders under its messages, posted
/// after a successful push so the thread shows the commit card AND the push card
/// as two honest, separate receipts.
struct GitPushCard: Identifiable, Sendable, Equatable {
    let id: String
    let remote: String
    let ref: String
    let remoteOid: String
    let localOid: String
    let alreadyDone: Bool
    let pushedAtMs: Int64

    var shortRemoteOid: String { String(remoteOid.prefix(8)) }
    var shortLocalOid: String { String(localOid.prefix(8)) }

    var pushedAtDate: Date {
        Date(timeIntervalSince1970: Double(pushedAtMs) / 1000.0)
    }
}

// MARK: - Push receipt card

/// Renders a push receipt: the remote, the ref, and the remote oid the ref now
/// points at. Shared by the Git studio (post-push confirmation) and the
/// conversation thread (the posted push card). Full-tile, dark, token-driven;
/// fills its container width.
struct PushReceiptView: View {
    let remote: String
    let ref: String
    let remoteOid: String
    /// Whether this execute actually pushed (false) or was idempotently a no-op
    /// because the same effect had already been pushed (true).
    var alreadyDone: Bool = false
    /// Optional relative-time line (shown in the conversation card).
    var subtitle: String?
    /// Optional dismiss handler (shown as an X in the studio confirmation).
    var onDismiss: (() -> Void)?

    private var shortRemoteOid: String { String(remoteOid.prefix(8)) }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            header
            detail
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
        .accessibilityIdentifier("git.push.receipt.\(shortRemoteOid)")
    }

    private var header: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: alreadyDone ? "checkmark.circle" : "checkmark.seal.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusSuccess)
            Text(alreadyDone ? "Already pushed" : "Pushed")
                .font(Theme.ui(12.5, .semibold))
                .foregroundStyle(Theme.text)
            Text(shortRemoteOid)
                .font(Theme.mono(11.5, .semibold))
                .foregroundStyle(Theme.muted)
                .textSelection(.enabled)
            if let subtitle {
                Text(subtitle)
                    .font(Theme.ui(10.5))
                    .foregroundStyle(Theme.faint)
            }
            Spacer(minLength: 0)
            StatusPill(kind: .success, label: "Remote")
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

    private var detail: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "arrow.up.circle")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.faint)
            Text("\(remote)/\(ref)")
                .font(Theme.mono(11))
                .foregroundStyle(Theme.textSoft)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
            Spacer(minLength: 0)
            Text("→ \(shortRemoteOid)")
                .font(Theme.mono(11, .semibold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusSuccess)
                .textSelection(.enabled)
        }
        .padding(Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.bg.opacity(0.6))
        )
    }

    private var accessibilityLabel: String {
        let verb = alreadyDone ? "Already pushed" : "Pushed"
        return "\(verb) \(remote)/\(ref) to remote \(shortRemoteOid)"
    }
}

// MARK: - Conversation push card

/// The conversation-thread wrapper around `PushReceiptView` for a posted
/// `GitPushCard`. Constrained like a message cell, leading-aligned. This renders
/// alongside (and after) the commit card so the thread shows the commit and the
/// push as two SEPARATE receipts.
struct PushReceiptCard: View {
    let card: GitPushCard

    var body: some View {
        PushReceiptView(
            remote: card.remote,
            ref: card.ref,
            remoteOid: card.remoteOid,
            alreadyDone: card.alreadyDone,
            subtitle: RelativeTime.string(from: card.pushedAtDate)
        )
        .frame(maxWidth: 720, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("conversation.pushCard.\(card.shortRemoteOid)")
    }
}

// MARK: - Retryable failed-push surface

/// The surface shown when a push FAILED at execute: the commit is done, but the
/// push needs a retry. The LOCAL commit is preserved — Retry re-opens the approval
/// prompt for the SAME effect (it never re-commits, never silently re-pushes).
struct PushRetrySurface: View {
    let intent: GitPushIntent
    /// The (optional) error detail to explain why the push failed.
    var message: String?
    var onRetry: () -> Void
    var onDismiss: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "arrow.up.circle.badge.xmark")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(GeneratedDesignTokens.colorStatusDanger)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Push failed — commit saved")
                        .font(Theme.ui(12.5, .semibold))
                        .foregroundStyle(Theme.text)
                    Text("Your local commit (\(intent.shortLocalOid)) is preserved. Retry the push to \(intent.remote)/\(intent.ref).")
                        .font(Theme.ui(10.5))
                        .foregroundStyle(Theme.textSoft)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
                StatusPill(kind: .danger, label: "Retryable")
                if let onDismiss {
                    Button(action: onDismiss) {
                        Image(systemName: "xmark")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundStyle(Theme.muted)
                    }
                    .buttonStyle(IconTileButtonStyle(size: 24))
                    .accessibilityLabel("Dismiss")
                }
            }
            if let message {
                Text(message)
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button(action: onRetry) {
                Label("Retry push", systemImage: "arrow.clockwise")
                    .labelStyle(.titleAndIcon)
            }
            .buttonStyle(.primaryAction)
            .frame(maxWidth: .infinity)
            .accessibilityIdentifier("git.push.retry")
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(GeneratedDesignTokens.colorStatusDanger.opacity(0.4), lineWidth: 1)
        )
        .contentShape(Rectangle())
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("git.push.retrySurface")
    }
}

// MARK: - Push outbox (pending / approved / completed)

/// A compact, relaunch-surviving view of the push outbox recovered from SQLite
/// (`push-status`): how many intents are pending approval, approved-but-unexecuted,
/// and completed. Shown in the commit composer so in-flight push work is visible
/// after a restart. Read-only — acting on a pending intent re-opens the approval
/// prompt elsewhere.
struct PushOutboxSummary: View {
    let status: GitPushStatus

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            HStack(spacing: Theme.s6) {
                Image(systemName: "tray.full")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.muted)
                Text("PUSH OUTBOX")
                    .font(Theme.ui(9.5, .bold))
                    .foregroundStyle(Theme.muted)
                Spacer(minLength: 0)
            }
            HStack(spacing: Theme.s8) {
                outboxCount(kind: .warning, label: "Pending", count: status.pending.count)
                outboxCount(kind: .running, label: "Approved", count: status.approved.count)
                outboxCount(kind: .success, label: "Completed", count: status.completed.count)
            }
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(Theme.bg.opacity(0.5))
        )
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("git.push.outbox")
        .accessibilityLabel(
            "Push outbox: \(status.pending.count) pending, \(status.approved.count) approved, \(status.completed.count) completed"
        )
    }

    private func outboxCount(kind: StatusPill.Kind, label: String, count: Int) -> some View {
        HStack(spacing: Theme.s4) {
            Image(systemName: kind.symbol)
                .font(.system(size: 9, weight: .bold))
                .foregroundStyle(kind.tint)
            Text("\(count)")
                .font(Theme.mono(11, .semibold))
                .foregroundStyle(Theme.textSoft)
            Text(label)
                .font(Theme.ui(10))
                .foregroundStyle(Theme.muted)
        }
    }
}
