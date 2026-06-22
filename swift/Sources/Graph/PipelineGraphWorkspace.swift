// PipelineGraphWorkspace.swift — the central surface for the `.graph` route.
// It picks the active run's live projection from the `PipelineProjectionStore`
// and lays out the full pannable/zoomable `PipelineGraphView` beside a
// `RunInspector`. When no run is active (nothing has streamed yet) it shows an
// honest empty state rather than a fabricated graph.
//
// Multiple concurrent runs are supported by the store (one reducer per run id);
// this workspace renders whichever run `AppCoordinator.activeGraphRunId` points
// at, and a run picker lets the operator switch between live runs.

import SwiftUI

struct PipelineGraphWorkspace: View {
    @ObservedObject var store: PipelineProjectionStore
    @Binding var activeRunId: String?

    @State private var selectedNodeId: String?

    /// The projection currently displayed: the active run if set, else the first
    /// live run, else nil.
    private var activeProjection: PipelineExecutionProjection? {
        if let id = activeRunId, let projection = store.projection(for: id) {
            return projection
        }
        return store.projections.first
    }

    var body: some View {
        Group {
            if let projection = activeProjection {
                content(for: projection)
            } else {
                EmptyStateView(
                    headline: "No live pipeline run",
                    detail: "Start a run from a conversation to watch its nodes execute here.",
                    systemImage: "point.3.connected.trianglepath.dotted"
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("pipeline.graph.workspace")
        .onAppear { syncSelection() }
        .onChange(of: activeProjection?.runId) { _ in syncSelection() }
    }

    @ViewBuilder
    private func content(for projection: PipelineExecutionProjection) -> some View {
        VStack(spacing: 0) {
            toolbar(for: projection)
            Divider().overlay(Theme.stroke)
            HStack(spacing: 0) {
                PipelineGraphView(projection: projection, selectedNodeId: $selectedNodeId)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Divider().overlay(Theme.stroke)
                RunInspector(projection: projection, selectedNodeId: selectedNodeId)
                    .frame(width: 300)
            }
        }
    }

    private func toolbar(for projection: PipelineExecutionProjection) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.violet)
            Text(projection.pipelineId ?? "Pipeline")
                .font(Theme.ui(13, .semibold))
                .foregroundStyle(Theme.text)
            StatusPill(kind: projection.state.pillKind, label: projection.state.displayLabel)

            Spacer(minLength: 0)

            // Run picker — switch between concurrent live runs. Each is a real
            // Button (full hit area), never a tap gesture on a label.
            if store.projections.count > 1 {
                Menu {
                    ForEach(store.projections) { run in
                        Button {
                            activeRunId = run.runId
                        } label: {
                            Text("\(run.pipelineId ?? "Pipeline") · \(String(run.runId.prefix(8)))")
                        }
                    }
                } label: {
                    Label(String(projection.runId.prefix(8)), systemImage: "rectangle.stack")
                        .font(Theme.mono(11))
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .accessibilityIdentifier("pipeline.graph.runPicker")
            } else {
                Text(String(projection.runId.prefix(8)))
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.muted)
                    .textSelection(.enabled)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    /// Default the selection to a pending approval (operator's call-to-action)
    /// when the run changes; otherwise leave selection alone if still valid.
    private func syncSelection() {
        guard let projection = activeProjection else {
            selectedNodeId = nil
            return
        }
        if let current = selectedNodeId,
           projection.nodes.contains(where: { $0.nodeId == current }) {
            return
        }
        selectedNodeId = projection.nodes.first(where: { $0.state == .waitingForApproval })?.nodeId
            ?? projection.nodes.first?.nodeId
    }
}
