// EvidenceWorkspaceView.swift — the `.evidence` route (PR-045). The proof-state
// workspace: it reports acceptance results and QA / security posture straight
// from the decoded `app-data` (AppState.data), never fabricating a pass it did
// not read. Until that data is loaded it shows a truthful empty state with a
// first-run onboarding affordance ("Run the acceptance audit") so the journey
// completes from the UI without the CLI.
//
// Honest by construction: completion language flows through `HonestText`, which
// never returns "complete". A zero-total acceptance set is shown as "Not audited
// yet", not as a pass.

import SwiftUI

struct EvidenceWorkspaceView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        Group {
            if let data = state.data {
                content(for: data)
            } else {
                EmptyStateView(
                    headline: "No proof state loaded yet",
                    detail: "Run the acceptance audit to populate the proof chain. Results stream into the Output drawer and are summarized here.",
                    systemImage: "checkmark.seal",
                    actionTitle: "Run acceptance audit",
                    action: { state.runAcceptance() }
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .accessibilityIdentifier("evidence.workspace")
    }

    @ViewBuilder
    private func content(for data: AppData) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.s16) {
                header(for: data)
                if data.acceptance.total == 0 {
                    notAuditedYet
                } else {
                    acceptanceCard(data.acceptance)
                }
                if let release = data.release, release.hasEvidence {
                    releaseProofCard(release)
                }
                postureCard(data.gui)
                Text("Evidence reflects only what acceptance has verified. A result is never shown as passed unless the audit reported it.")
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: 760, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 40)
            .padding(.vertical, 32)
        }
    }

    // MARK: - Header

    private func header(for data: AppData) -> some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s12) {
                Image(systemName: "checkmark.seal")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Evidence & Proof Chain")
                        .font(Theme.ui(18, .semibold))
                        .foregroundStyle(Theme.text)
                    Text("Acceptance results and review posture for \(data.workspaceLabel).")
                        .font(Theme.ui(12))
                        .foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 0)
                Button {
                    state.runAcceptance()
                } label: {
                    Label("Run acceptance audit", systemImage: "play.fill")
                }
                .buttonStyle(.secondaryAction)
                .frame(maxWidth: 210)
                .accessibilityIdentifier("evidence.run.acceptance")
                .help("Run the acceptance audit and stream results into the Output drawer.")
            }
        }
        .accessibilityElement(children: .contain)
    }

    // MARK: - Acceptance

    private var notAuditedYet: some View {
        card {
            VStack(alignment: .leading, spacing: Theme.s8) {
                HStack(spacing: Theme.s8) {
                    StatusPill(kind: .neutral, label: "Not audited yet")
                    Spacer()
                }
                Text("No acceptance results have been recorded for this workspace. Run the audit to build the proof chain — nothing is reported as passing until it is.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityIdentifier("evidence.acceptance.empty")
    }

    private func acceptanceCard(_ acceptance: Acceptance) -> some View {
        card {
            VStack(alignment: .leading, spacing: Theme.s12) {
                HStack(spacing: Theme.s8) {
                    Text("Acceptance")
                        .font(Theme.ui(13, .semibold))
                        .foregroundStyle(Theme.text)
                    Spacer()
                    StatusPill(
                        kind: acceptancePillKind(acceptance),
                        label: HonestText.goalState(acceptance)
                    )
                }
                // Pass / partial / fail tallies — read straight from the audit.
                HStack(spacing: 0) {
                    tally("\(acceptance.passed)", "passed", Theme.pass)
                    tallyDivider
                    tally("\(acceptance.partial)", "partial", Theme.partial)
                    tallyDivider
                    tally("\(acceptance.failed)", "failed", Theme.fail)
                    tallyDivider
                    tally("\(acceptance.total)", "total", Theme.textSoft)
                    Spacer(minLength: 0)
                }
                ProofRatioBar(ratio: acceptance.ratio)
                    .accessibilityLabel("\(Int((acceptance.ratio * 100).rounded())) percent of acceptance criteria passed")
            }
        }
        .accessibilityIdentifier("evidence.acceptance.card")
        .accessibilityElement(children: .contain)
    }

    private func acceptancePillKind(_ a: Acceptance) -> StatusPill.Kind {
        if a.failed > 0 { return .danger }
        if a.partial > 0 { return .warning }
        if a.goalComplete == true { return .success }
        return .neutral
    }

    private func tally(_ value: String, _ label: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value).font(Theme.ui(22, .semibold)).foregroundStyle(color)
            Text(label).font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
        }
        .frame(minWidth: 64, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(value) \(label)")
    }

    private var tallyDivider: some View {
        Rectangle().fill(Theme.stroke).frame(width: 1, height: 34).padding(.horizontal, 18)
    }

    // MARK: - Release proof

    private func releaseProofCard(_ release: ReleaseProofSummary) -> some View {
        card {
            VStack(alignment: .leading, spacing: Theme.s12) {
                HStack(spacing: Theme.s8) {
                    Text("Release proof")
                        .font(Theme.ui(13, .semibold))
                        .foregroundStyle(Theme.text)
                    Spacer()
                    StatusPill(kind: release.pillKind, label: release.displayStatus)
                }
                if !release.blockers.isEmpty {
                    VStack(alignment: .leading, spacing: Theme.s8) {
                        Text("Blockers")
                            .font(Theme.ui(11.5, .semibold))
                            .foregroundStyle(Theme.textSoft)
                        ForEach(release.blockers) { blocker in
                            releaseBlockerRow(blocker)
                        }
                    }
                }
                if !release.remediationActions.isEmpty {
                    VStack(alignment: .leading, spacing: Theme.s8) {
                        Text("Remediation actions")
                            .font(Theme.ui(11.5, .semibold))
                            .foregroundStyle(Theme.textSoft)
                        ForEach(release.remediationActions) { action in
                            releaseActionRow(action)
                        }
                    }
                }
            }
        }
        .accessibilityIdentifier("evidence.release.card")
        .accessibilityElement(children: .contain)
    }

    private func releaseBlockerRow(_ blocker: ReleaseProofBlocker) -> some View {
        HStack(alignment: .top, spacing: Theme.s10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.partial)
                .frame(width: 16, height: 18)
            VStack(alignment: .leading, spacing: 3) {
                Text(blocker.code)
                    .font(Theme.mono(11.5, .medium))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                Text(blocker.message)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(blocker.code): \(blocker.message)")
    }

    private func releaseActionRow(_ action: ReleaseRemediationAction) -> some View {
        HStack(alignment: .top, spacing: Theme.s10) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.accent)
                .frame(width: 16, height: 18)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: Theme.s8) {
                    Text(action.blocker)
                        .font(Theme.mono(11.5, .medium))
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                    Text(action.scope)
                        .font(Theme.ui(10.5, .medium))
                        .foregroundStyle(Theme.muted)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Capsule().fill(Theme.input))
                }
                Text(action.action)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.textSoft)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(action.blocker): \(action.action)")
    }

    // MARK: - Review posture

    private func postureCard(_ gui: Gui) -> some View {
        card {
            VStack(alignment: .leading, spacing: Theme.s10) {
                Text("Review posture")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                postureRow(label: "QA", status: gui.qaStatus, systemImage: "checklist")
                postureRow(label: "Security", status: gui.securityStatus, systemImage: "lock.shield")
                postureRow(
                    label: "PRD coverage",
                    status: "\(gui.prdImplemented) implemented / \(gui.prdTotal) total",
                    systemImage: "doc.text"
                )
            }
        }
        .accessibilityIdentifier("evidence.posture.card")
        .accessibilityElement(children: .contain)
    }

    private func postureRow(label: String, status: String, systemImage: String) -> some View {
        HStack(spacing: Theme.s10) {
            Label {
                Text(label).font(Theme.ui(12, .medium)).foregroundStyle(Theme.textSoft)
            } icon: {
                Image(systemName: systemImage).font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            .frame(width: 130, alignment: .leading)
            Text(status)
                .font(Theme.mono(11.5))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label): \(status)")
    }

    // MARK: - Card chrome

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .padding(Theme.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: Theme.rLg).fill(Theme.panel))
            .overlay(RoundedRectangle(cornerRadius: Theme.rLg).strokeBorder(Theme.stroke, lineWidth: 1))
    }
}

/// A thin, token-driven progress bar conveying the acceptance pass ratio. Width
/// is shown by the fill; the value is also exposed to VoiceOver by the caller so
/// the proportion is never communicated by colour alone.
private struct ProofRatioBar: View {
    let ratio: Double

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.input)
                Capsule()
                    .fill(Theme.accent)
                    .frame(width: max(0, min(1, ratio)) * geo.size.width)
            }
        }
        .frame(height: 8)
        .accessibilityIdentifier("evidence.acceptance.ratiobar")
    }
}
