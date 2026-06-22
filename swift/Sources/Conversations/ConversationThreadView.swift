// ConversationThreadView.swift — the central `.chat` surface. Renders the
// selected conversation's stored messages (user / assistant / system cells)
// oldest -> newest, with a "Load older" affordance when there is an older page.
// A `ConversationComposer` is pinned to the bottom (PR-027): one Send starts one
// deterministic engine run, surfaced as an inline `RunCard` under the assistant
// turn it produced. No live token streaming yet — the run completes and the card
// shows its final state honestly (live streaming is PR-029 / PR-030).

import SwiftUI

struct ConversationThreadView: View {
    @ObservedObject var store: ConversationStore
    /// Live node-level projections keyed by run id (PR-029/PR-030). When a run
    /// has a projection, the thread renders a `PipelineRunCard` (node-count
    /// summary + mini graph + controls) alongside the PR-027 `RunCard`. Optional
    /// so the thread still renders in contexts without a pipeline store.
    var pipelines: PipelineProjectionStore?
    /// Invoked when a run card's "Open live graph" is pressed.
    var onOpenGraph: (String) -> Void = { _ in }

    var body: some View {
        Group {
            if let summary = store.selectedSummary {
                thread(for: summary)
            } else {
                EmptyStateView(
                    headline: "No conversation selected",
                    detail: "Pick a conversation from the sidebar, or start a new one.",
                    systemImage: "bubble.left.and.bubble.right"
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .accessibilityIdentifier("conversation.thread")
    }

    @ViewBuilder
    private func thread(for summary: ConversationSummary) -> some View {
        VStack(spacing: 0) {
            threadHeader(summary)
            Divider().overlay(Theme.stroke)
            if store.messages.isEmpty {
                EmptyStateView(
                    headline: "No messages yet",
                    detail: "Send a message below to start a deterministic engine run.",
                    systemImage: "text.bubble"
                )
                .frame(maxHeight: .infinity)
            } else {
                messageList
            }
            ConversationComposer(store: store, conversationID: summary.id)
        }
    }

    private func threadHeader(_ summary: ConversationSummary) -> some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(summary.title)
                    .font(Theme.ui(15, .semibold))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text(RelativeTime.string(from: summary.lastActivityDate))
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 0)
            StatusPill(kind: summary.status.pillKind, label: summary.status.displayLabel)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
    }

    private var messageList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                if store.hasMoreMessages {
                    Button {
                        Task { await store.loadOlderMessages() }
                    } label: {
                        HStack {
                            Spacer()
                            Label("Load older", systemImage: "arrow.up")
                                .font(Theme.ui(12, .semibold))
                            Spacer()
                        }
                    }
                    .buttonStyle(.quietAction)
                    .disabled(store.isLoadingMessages)
                    .accessibilityIdentifier("conversation.loadOlder")
                }
                ForEach(store.messages) { message in
                    MessageCell(message: message)
                        .id(message.id)
                    // An inline run card under the assistant turn it produced.
                    if let run = store.run(forMessageID: message.id) {
                        RunCard(run: run)
                            .id("run-\(run.runId)")
                        // If a live node-level projection exists for this run,
                        // surface it as a PipelineRunCard alongside the PR-027
                        // card. Every number is derived from the projection.
                        if let projection = pipelines?.projection(for: run.runId) {
                            PipelineRunCard(
                                projection: projection,
                                onControl: { control in
                                    if control == .openGraph { onOpenGraph(run.runId) }
                                }
                            )
                            .id("pipeline-run-\(run.runId)")
                        }
                    }
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 16)
        }
    }
}

// MARK: - Message cell

struct MessageCell: View {
    let message: ConversationMessage

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: roleSymbol)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(roleTint)
                Text(roleLabel)
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(roleTint)
                Spacer(minLength: 0)
                Text(RelativeTime.string(from: message.createdAtDate))
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }
            Text(message.contentRedacted)
                .font(Theme.ui(13))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: 720, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(cellFill)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .frame(maxWidth: .infinity, alignment: message.role == .user ? .trailing : .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(roleLabel): \(message.contentRedacted)")
    }

    private var roleLabel: String {
        switch message.role {
        case .user: return "You"
        case .assistant: return "Assistant"
        case .system: return "System"
        case .unknown: return "Message"
        }
    }

    private var roleSymbol: String {
        switch message.role {
        case .user: return "person.fill"
        case .assistant: return "sparkles"
        case .system: return "gearshape.fill"
        case .unknown: return "bubble.left"
        }
    }

    private var roleTint: Color {
        switch message.role {
        case .user: return Theme.accent
        case .assistant: return Theme.violet
        case .system: return Theme.muted
        case .unknown: return Theme.muted
        }
    }

    private var cellFill: Color {
        message.role == .user ? Theme.input : Theme.panel
    }
}
