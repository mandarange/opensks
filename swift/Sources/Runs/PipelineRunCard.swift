// PipelineRunCard.swift — an inline card surfacing the LIVE node-level state of
// one pipeline run, driven entirely by its PR-029 `PipelineExecutionProjection`.
// Unlike `RunCard` (PR-027, a single completed deterministic run), this card
// summarises a run with many nodes: a node-count breakdown, a wrapping grid of
// one cell per node (`NodeStateGrid`), and the run control affordances.
//
// Honesty rules this card obeys:
//   * Every number it shows is DERIVED from the projection — nothing is faked or
//     animated to look "live". If the store has no projection for a run, the
//     card says so plainly.
//   * State is shown via `StatusPill` (glyph + tint) and never colour alone.
//   * Controls are real `Button`s using `SurfaceButtonStyle`, so the whole tile
//     is the hit target (no `onTapGesture` on button-like controls). They route
//     into an optional control closure; when none is wired they are honest
//     no-ops rather than fabricating state transitions.

import SwiftUI

// MARK: - Derived view model

/// A pure, testable summary derived from a `PipelineExecutionProjection`. The
/// run card binds to this so the count/label logic is unit-testable without a
/// view hierarchy.
struct PipelineRunCardModel: Equatable {
    let runId: String
    let pipelineLabel: String
    let runState: RunProjectionState
    let totalNodes: Int
    let completed: Int
    let active: Int
    let queued: Int
    let failed: Int
    /// Nodes specifically awaiting human approval (a subset of `active`),
    /// surfaced separately because it is the operator's call-to-action.
    let awaitingApproval: Int

    init(projection: PipelineExecutionProjection) {
        runId = projection.runId
        pipelineLabel = projection.pipelineId ?? "Pipeline"
        runState = projection.state
        totalNodes = projection.nodes.count

        var completed = 0, active = 0, queued = 0, failed = 0, approval = 0
        for node in projection.nodes {
            switch node.state {
            case .succeeded, .skipped:
                completed += 1
            case .failed, .cancelled:
                failed += 1
            case .running, .dispatching:
                active += 1
            case .waitingForApproval:
                active += 1
                approval += 1
            case .queued:
                queued += 1
            case .unknown:
                break
            }
        }
        self.completed = completed
        self.active = active
        self.queued = queued
        self.failed = failed
        self.awaitingApproval = approval
    }

    /// First 8 characters of the run id — a compact, honest reference.
    var shortRunID: String { String(runId.prefix(8)) }

    /// "5/12 complete · 3 active · 2 queued · 1 approval · 1 failed". Segments
    /// for zero-count buckets are dropped so the line stays scannable, but the
    /// "complete" fraction is always present (it is the headline metric).
    var summaryLine: String {
        var parts: [String] = ["\(completed)/\(totalNodes) complete"]
        if active > 0 { parts.append("\(active) active") }
        if queued > 0 { parts.append("\(queued) queued") }
        if awaitingApproval > 0 { parts.append("\(awaitingApproval) approval") }
        if failed > 0 { parts.append("\(failed) failed") }
        return parts.joined(separator: " · ")
    }

    /// A spoken-language accessibility summary of the same data.
    var accessibilitySummary: String {
        "Run \(shortRunID), \(pipelineLabel), \(runState.displayLabel). \(summaryLine)."
    }
}

// MARK: - Run controls

/// The control verbs a run card can offer. The host decides which are enabled
/// and what they do; the card never assumes a transition happened.
enum PipelineRunControl: String, CaseIterable, Identifiable {
    case openGraph
    case pause
    case resume
    case cancel
    case steer

    var id: String { rawValue }

    var label: String {
        switch self {
        case .openGraph: return "Open live graph"
        case .pause: return "Pause"
        case .resume: return "Resume"
        case .cancel: return "Cancel"
        case .steer: return "Steer"
        }
    }

    var symbol: String {
        switch self {
        case .openGraph: return "point.3.connected.trianglepath.dotted"
        case .pause: return "pause.fill"
        case .resume: return "play.fill"
        case .cancel: return "stop.fill"
        case .steer: return "scope"
        }
    }

    var emphasis: SurfaceButtonStyle.Emphasis {
        switch self {
        case .openGraph: return .primary
        case .cancel: return .destructive
        default: return .secondary
        }
    }
}

// MARK: - Card

struct PipelineRunCard: View {
    let projection: PipelineExecutionProjection
    /// Invoked when a control is pressed. Defaults to a no-op so the card is
    /// usable in previews/tests without wiring a live service. NEVER fabricates
    /// a state change — the host is responsible for any real mutation.
    var onControl: (PipelineRunControl) -> Void = { _ in }

    private var model: PipelineRunCardModel { PipelineRunCardModel(projection: projection) }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            NodeStateGrid(states: projection.nodes.map(\.state))
                .frame(height: gridHeight)
            controls
        }
        .padding(12)
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
        .accessibilityLabel(model.accessibilitySummary)
        .accessibilityIdentifier("pipeline.runCard.\(model.runId)")
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.violet)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(model.pipelineLabel)
                        .font(Theme.ui(12, .semibold))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    Text(model.shortRunID)
                        .font(Theme.mono(11))
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
                // The DERIVED node-count summary — every number from the projection.
                Text(model.summaryLine)
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.textSoft)
                    .accessibilityIdentifier("pipeline.runCard.summary.\(model.runId)")
            }

            Spacer(minLength: 0)

            StatusPill(kind: model.runState.pillKind, label: model.runState.displayLabel)

            // Chevron affordance for "open the full live graph" — a real Button
            // (not a whole-header tap gesture) per this card's hit-target
            // convention, but visually echoing the expand cue of the node grid.
            Button {
                onControl(.openGraph)
            } label: {
                Image(systemName: "chevron.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.faint)
                    .frame(width: 20, height: 20)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Open live graph")
            .accessibilityLabel("Open live graph for run \(model.shortRunID)")
            .accessibilityIdentifier("pipeline.runCard.chevron.\(model.runId)")
        }
    }

    /// Enough rows for a typical run to show every node without truncation at
    /// common card widths, capped so a huge run stays a compact "at a glance"
    /// view rather than growing unbounded — the summary line above already
    /// carries the complete counts.
    private var gridHeight: CGFloat {
        switch model.totalNodes {
        case ..<20: return 18
        case ..<80: return 34
        case ..<200: return 54
        default: return 74
        }
    }

    private var controls: some View {
        HStack(spacing: 8) {
            ForEach(controlOrder) { control in
                Button {
                    onControl(control)
                } label: {
                    Label(control.label, systemImage: control.symbol)
                        .labelStyle(.titleAndIcon)
                        .font(Theme.ui(11.5, .semibold))
                }
                .buttonStyle(SurfaceButtonStyle(emphasis: control.emphasis, minHeight: 32))
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier("pipeline.runCard.control.\(control.rawValue).\(model.runId)")
            }
        }
    }

    /// Pause vs. Resume is mutually exclusive on run state, so we show only the
    /// applicable one rather than both. Terminal runs offer neither.
    private var controlOrder: [PipelineRunControl] {
        var controls: [PipelineRunControl] = [.openGraph]
        switch model.runState {
        case .running, .queued:
            controls.append(.pause)
        case .paused:
            controls.append(.resume)
        case .completed, .failed, .cancelled, .unknown:
            break
        }
        if !model.runState.isTerminal {
            controls.append(.steer)
            controls.append(.cancel)
        }
        return controls
    }
}

