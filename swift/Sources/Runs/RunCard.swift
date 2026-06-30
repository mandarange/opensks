// RunCard.swift — an inline card surfacing ONE deterministic engine run linked
// to an assistant turn (PR-027). It shows a short run id, the relation
// ("primary"), and the run's final state via a `StatusPill` (running / success /
// danger — glyph + tint, never colour alone). Active runs also show a native
// progress spinner so the card does not look frozen while the worker is running.

import SwiftUI

struct RunFailureDetail: Identifiable, Equatable {
    let label: String
    let value: String

    var id: String { "\(label):\(value)" }
}

struct RunFailureDiagnostics: Equatable {
    let runId: String
    let state: RunState
    let summary: String
    let details: [RunFailureDetail]
    let recoveryHints: [String]

    init(
        run: ConversationRunRef,
        summary: String? = nil,
        details: [RunFailureDetail] = [],
        recoveryHints: [String] = []
    ) {
        runId = run.runId
        state = run.runState
        let trimmed = summary?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.summary = trimmed.isEmpty ? "The run ended as \(run.runState.displayLabel)." : trimmed
        self.details = details
        self.recoveryHints = recoveryHints.isEmpty
            ? ["Open the failed run details, fix the reported cause, then retry the turn."]
            : recoveryHints
    }
}

struct RunCard: View {
    let run: ConversationRunRef
    var failureDiagnostics: RunFailureDiagnostics?

    @State private var showingFailureDetails = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "sparkles")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.violet)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text("Run")
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(Theme.textSoft)
                    Text(shortRunID)
                        .font(Theme.mono(11))
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
                Text(run.relation.capitalized)
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }

            Spacer(minLength: 0)

            statusSurface
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .frame(maxWidth: 720, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Run \(shortRunID), \(run.relation), \(run.runState.displayLabel)")
        .accessibilityIdentifier("run.card.\(run.runId)")
    }

    /// First 8 characters of the run id for a compact, honest reference (the
    /// full id is selectable for copy).
    private var shortRunID: String {
        String(run.runId.prefix(8))
    }

    @ViewBuilder
    private var statusSurface: some View {
        if shouldShowFailureDetails {
            HStack(spacing: Theme.s6) {
                Button {
                    showingFailureDetails = true
                } label: {
                    StatusPill(kind: run.runState.pillKind, label: run.runState.displayLabel)
                        .frame(minHeight: 22)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Show failure details")
                .accessibilityLabel("Show \(run.runState.displayLabel.lowercased()) run details")
                .accessibilityIdentifier("run.card.failureDetails.\(run.runId)")

                Button {
                    showingFailureDetails = true
                } label: {
                    Image(systemName: "info.circle")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(run.runState.pillKind.tint)
                        .frame(width: 22, height: 22)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Show failure details")
                .accessibilityLabel("Failure details")
                .accessibilityIdentifier("run.card.failureInfo.\(run.runId)")
            }
            .popover(isPresented: $showingFailureDetails, arrowEdge: .bottom) {
                RunFailureDiagnosticsPopover(
                    diagnostics: failureDiagnostics ?? RunFailureDiagnostics(run: run),
                    shortRunID: shortRunID
                )
            }
        } else {
            if run.runState.isActive {
                HStack(spacing: Theme.s6) {
                    ProgressView()
                        .controlSize(.small)
                        .scaleEffect(0.72)
                        .frame(width: 14, height: 14)
                        .accessibilityHidden(true)
                    StatusPill(kind: run.runState.pillKind, label: run.runState.displayLabel)
                }
                .accessibilityIdentifier("run.card.progress.\(run.runId)")
            } else {
                StatusPill(kind: run.runState.pillKind, label: run.runState.displayLabel)
            }
        }
    }

    private var shouldShowFailureDetails: Bool {
        run.runState == .failed || run.runState == .cancelled || failureDiagnostics != nil
    }
}

extension RunState {
    var isActive: Bool {
        self == .queued || self == .running
    }
}

struct RunFailureDiagnosticsPopover: View {
    let diagnostics: RunFailureDiagnostics
    let shortRunID: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Theme.s8) {
                Image(systemName: diagnostics.state.pillKind.symbol)
                    .foregroundStyle(diagnostics.state.pillKind.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Run \(shortRunID) \(diagnostics.state.displayLabel)")
                        .font(Theme.ui(13, .semibold))
                        .foregroundStyle(Theme.text)
                    Text("Failure details")
                        .font(Theme.ui(10.5))
                        .foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 0)
            }
            .padding(12)
            Divider().overlay(Theme.stroke)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    section("Summary") {
                        Text(diagnostics.summary)
                            .font(Theme.ui(12))
                            .foregroundStyle(Theme.textSoft)
                            .textSelection(.enabled)
                            .lineLimit(nil)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if !diagnostics.details.isEmpty {
                        section("Signals") {
                            VStack(alignment: .leading, spacing: Theme.s6) {
                                ForEach(diagnostics.details) { detail in
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(detail.label)
                                            .font(Theme.mono(9.5, .semibold))
                                            .foregroundStyle(Theme.faint)
                                        Text(detail.value)
                                            .font(Theme.ui(11))
                                            .foregroundStyle(Theme.textSoft)
                                            .textSelection(.enabled)
                                            .lineLimit(nil)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                            }
                        }
                    }
                    section("Next step") {
                        VStack(alignment: .leading, spacing: Theme.s6) {
                            ForEach(Array(diagnostics.recoveryHints.enumerated()), id: \.offset) { _, hint in
                                Text(hint)
                                    .font(Theme.ui(11.5))
                                    .foregroundStyle(Theme.textSoft)
                                    .textSelection(.enabled)
                                    .lineLimit(nil)
                                    .fixedSize(horizontal: false, vertical: true)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
                .padding(14)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .scrollIndicators(.visible)
        }
        .frame(width: 560)
        .frame(minHeight: 320, idealHeight: 520, maxHeight: 620)
        .background(Theme.bg)
        .accessibilityIdentifier("run.failureDiagnostics.popover")
    }

    private func section<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            Text(title)
                .font(Theme.ui(10.5, .semibold))
                .foregroundStyle(Theme.muted)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
