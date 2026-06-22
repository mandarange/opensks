// RunInspector.swift — a node-detail inspector for the pipeline graph (PR-030).
// Given a run's projection and a selected node id, it surfaces that node's
// state, provenance (provider_ref / model_ref), attempt count, touched paths,
// and last public message — all straight from the PR-029 projection, never
// fabricated.
//
// Accessibility requirement: an approval node (state == waiting_for_approval)
// must be keyboard-focusable / reachable. We make the whole inspector header a
// `Button` carrying a focusable element with a stable identifier, and route
// keyboard focus to it via `@FocusState` so an operator can tab to the pending
// approval and act on it. The button is an honest no-op placeholder when no
// approval handler is wired (it does not fake an approval).

import SwiftUI

struct RunInspector: View {
    let projection: PipelineExecutionProjection
    let selectedNodeId: String?
    /// Invoked when the operator activates a waiting-for-approval node. Defaults
    /// to a no-op so the inspector is usable without a wired approval service.
    var onApprovalFocusActivate: (NodeExecutionProjection) -> Void = { _ in }

    @FocusState private var approvalFocused: Bool

    private var node: NodeExecutionProjection? {
        guard let id = selectedNodeId else { return nil }
        return projection.nodes.first { $0.nodeId == id }
    }

    var body: some View {
        Group {
            if let node {
                detail(for: node)
            } else {
                EmptyStateView(
                    headline: "No node selected",
                    detail: "Select a node in the graph to inspect its state, provenance, and touched files.",
                    systemImage: "point.3.connected.trianglepath.dotted"
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.panel)
        .accessibilityIdentifier("pipeline.inspector")
    }

    @ViewBuilder
    private func detail(for node: NodeExecutionProjection) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header(for: node)
                Divider().overlay(Theme.stroke)
                provenance(for: node)
                if !node.touchedPaths.isEmpty {
                    touchedPaths(for: node)
                }
                if let message = node.lastPublicMessage, !message.isEmpty {
                    lastMessage(message)
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - Header (approval-focusable)

    @ViewBuilder
    private func header(for node: NodeExecutionProjection) -> some View {
        let isApproval = node.state == .waitingForApproval
        // The header is a real Button so the whole tile is the hit + focus target
        // (no onTapGesture). For approval nodes it is keyboard-focusable and the
        // default-focused element, satisfying the "reachable approval" rule.
        Button {
            if isApproval { onApprovalFocusActivate(node) }
        } label: {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(node.nodeId)
                        .font(Theme.mono(13, .semibold))
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                    Spacer(minLength: 0)
                    StatusPill(kind: node.state.pillKind, label: node.state.displayLabel)
                }
                if isApproval {
                    Label("Awaiting your approval", systemImage: "exclamationmark.triangle.fill")
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(GeneratedDesignTokens.colorStatusWarning)
                }
            }
        }
        .buttonStyle(SurfaceButtonStyle(emphasis: isApproval ? .primary : .quiet, minHeight: isApproval ? 56 : 44))
        .focusable(isApproval)
        .focused($approvalFocused)
        .accessibilityIdentifier(
            isApproval
                ? "pipeline.inspector.approval.\(node.nodeId)"
                : "pipeline.inspector.node.\(node.nodeId)"
        )
        .accessibilityLabel(
            isApproval
                ? "Node \(node.nodeId) awaiting approval. Activate to review."
                : "Node \(node.nodeId), \(node.state.displayLabel)"
        )
        .accessibilityAddTraits(isApproval ? .isButton : [])
        .onAppear {
            // Route keyboard focus to a pending approval so it is reachable by tab.
            if isApproval { approvalFocused = true }
        }
        .onChange(of: selectedNodeId) { _ in
            approvalFocused = isApproval
        }
    }

    // MARK: - Sections

    private func provenance(for node: NodeExecutionProjection) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionTitle("Provenance")
            field("Provider", node.providerRef ?? "—", id: "provider", nodeId: node.nodeId)
            field("Model", node.modelRef ?? "—", id: "model", nodeId: node.nodeId)
            field("Attempt", "\(node.attempt)", id: "attempt", nodeId: node.nodeId)
        }
    }

    private func touchedPaths(for node: NodeExecutionProjection) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionTitle("Touched paths (\(node.touchedPaths.count))")
            ForEach(node.touchedPaths, id: \.self) { path in
                Text(path)
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textSoft)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .accessibilityIdentifier("pipeline.inspector.touchedPaths.\(node.nodeId)")
    }

    private func lastMessage(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionTitle("Last public message")
            Text(message)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier("pipeline.inspector.lastMessage")
    }

    // MARK: - Bits

    private func sectionTitle(_ text: String) -> some View {
        Text(text.uppercased())
            .font(Theme.ui(10, .semibold))
            .foregroundStyle(Theme.faint)
    }

    private func field(_ label: String, _ value: String, id: String, nodeId: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label)
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .frame(width: 72, alignment: .leading)
            Text(value)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label): \(value)")
        .accessibilityIdentifier("pipeline.inspector.field.\(id).\(nodeId)")
    }
}
