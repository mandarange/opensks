// PushApprovalView.swift — the EXPLICIT push approval prompt (PR-036).
//
// THE INVARIANT: a push always requires the operator to approve the EXACT effect.
// This prompt shows precisely what a push would do — the redacted remote URL, the
// ref, and the local oid the push would publish against the remote's expected oid
// — and nothing happens until the operator presses Approve & push. The approval
// carries the intent's `effect_digest`; if the effect has moved (a digest
// mismatch) the prompt stays open with a notice so the operator re-reviews.
//
// PROTECTED BRANCH: a protected ref shows a distinct, prominent warning and an
// explicit acknowledgement toggle. Approve & push is DISABLED until the operator
// ticks the acknowledgement (the store sends `--ack-protected` only then). There
// is no way to push a protected branch without that explicit extra confirmation.
//
// Dark, token-driven, full-tile hit areas; fills its container width (no
// letterbox). Status is conveyed by icon + label + a semantic token, never colour
// alone.

import SwiftUI

struct PushApprovalView: View {
    /// The prompt state (the exact effect + ack + working flags). Approve/Cancel
    /// are wired through the closures so the view stays presentation-only.
    let prompt: GitPushPrompt
    /// Toggle the protected-branch acknowledgement.
    var onAck: (Bool) -> Void
    /// Approve the exact effect and execute the push.
    var onApprove: () -> Void
    /// Decline — dismiss the prompt without pushing.
    var onCancel: () -> Void

    private var intent: GitPushIntent { prompt.intent }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s10) {
            header
            if intent.protected { protectedWarning }
            effectGrid
            if let notice = prompt.notice { noticeBanner(notice) }
            actions
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(
                    (intent.protected ? GeneratedDesignTokens.colorStatusWarning : Theme.stroke)
                        .opacity(intent.protected ? 0.5 : 1),
                    lineWidth: 1
                )
        )
        .contentShape(Rectangle())
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("git.push.approval")
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "arrow.up.circle")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text("Approve push")
                    .font(Theme.ui(12.5, .semibold))
                    .foregroundStyle(Theme.text)
                Text("Review the exact effect before pushing. Nothing is sent until you approve.")
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
            StatusPill(kind: .warning, label: "Pending")
        }
    }

    // MARK: - Protected-branch warning

    private var protectedWarning: some View {
        HStack(alignment: .top, spacing: Theme.s8) {
            Image(systemName: "exclamationmark.shield.fill")
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusWarning)
            VStack(alignment: .leading, spacing: Theme.s4) {
                Text("Protected branch")
                    .font(Theme.ui(11.5, .semibold))
                    .foregroundStyle(Theme.text)
                Text("\(intent.ref) is protected. Pushing to it needs your explicit acknowledgement.")
                    .font(Theme.ui(10.5))
                    .foregroundStyle(Theme.textSoft)
                    .fixedSize(horizontal: false, vertical: true)
                ackToggle
            }
            Spacer(minLength: 0)
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(GeneratedDesignTokens.colorStatusWarning.opacity(0.12))
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Protected branch \(intent.ref). Pushing requires explicit acknowledgement.")
    }

    /// The explicit protected-branch acknowledgement (the whole row is the hit
    /// target). Approve & push stays disabled until this is ticked.
    private var ackToggle: some View {
        Button {
            onAck(!prompt.ackProtected)
        } label: {
            HStack(spacing: Theme.s8) {
                Image(systemName: prompt.ackProtected ? "checkmark.square.fill" : "square")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(prompt.ackProtected ? GeneratedDesignTokens.colorStatusWarning : Theme.muted)
                Text("I understand this pushes to a protected branch")
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(Theme.textSoft)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .padding(.vertical, Theme.s6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(prompt.isWorking)
        .accessibilityIdentifier("git.push.ackProtected")
        .accessibilityLabel("Acknowledge protected-branch push")
        .accessibilityAddTraits(prompt.ackProtected ? [.isSelected, .isButton] : .isButton)
    }

    // MARK: - Exact effect

    private var effectGrid: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            Text("EXACT EFFECT")
                .font(Theme.ui(9.5, .bold))
                .foregroundStyle(Theme.muted)
            effectRow(icon: "globe", label: "Remote", value: intent.remote, mono: false)
            effectRow(icon: "link", label: "URL", value: intent.remoteUrlRedacted, mono: true)
            effectRow(icon: "arrow.triangle.branch", label: "Branch", value: intent.ref, mono: false)
            effectRow(icon: "shippingbox", label: "Local", value: intent.shortLocalOid, mono: true)
            effectRow(icon: "arrow.up.to.line", label: "Remote at", value: intent.remoteExpectedLabel, mono: true)
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(Theme.bg.opacity(0.6))
        )
        .accessibilityIdentifier("git.push.effect")
    }

    private func effectRow(icon: String, label: String, value: String, mono: Bool) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.s8) {
            Image(systemName: icon)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.faint)
                .frame(width: 16)
            Text(label.uppercased())
                .font(Theme.ui(9.5, .bold))
                .foregroundStyle(Theme.muted)
                .frame(width: 70, alignment: .leading)
            Text(value)
                .font(mono ? Theme.mono(11) : Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.textSoft)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label): \(value)")
    }

    private func noticeBanner(_ notice: String) -> some View {
        HStack(alignment: .top, spacing: Theme.s8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(GeneratedDesignTokens.colorStatusDanger)
            Text(notice)
                .font(Theme.ui(10.5))
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
        .accessibilityIdentifier("git.push.notice")
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Push notice: \(notice)")
    }

    // MARK: - Actions

    private var actions: some View {
        HStack(spacing: Theme.s8) {
            Button(action: onCancel) {
                Label("Cancel", systemImage: "xmark")
                    .labelStyle(.titleAndIcon)
            }
            .buttonStyle(.secondaryAction)
            .disabled(prompt.isWorking)
            .accessibilityIdentifier("git.push.cancel")

            Button(action: onApprove) {
                Label(prompt.isWorking ? "Pushing…" : "Approve & push", systemImage: "arrow.up.circle.fill")
                    .labelStyle(.titleAndIcon)
            }
            .buttonStyle(.primaryAction)
            .disabled(!prompt.canApprove)
            .accessibilityIdentifier("git.push.approve")
            .accessibilityHint(approveHint)
        }
    }

    private var approveHint: String {
        if prompt.isWorking { return "A push is in progress" }
        if intent.protected && !prompt.ackProtected {
            return "Acknowledge the protected-branch push to enable this"
        }
        return "Approve the exact effect and push to \(intent.remote)/\(intent.ref)"
    }
}
