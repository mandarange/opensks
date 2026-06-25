// ConversationThreadView.swift — the central `.chat` surface. Renders the
// durable conversation timeline oldest -> newest, with the legacy message page
// kept as a pagination/fallback read model during the timeline migration. A
// `ConversationComposer` is pinned to the bottom: one Send starts one durable
// turn, surfaced as an inline `RunCard` under the assistant item it produced.

import SwiftUI

struct ConversationThreadView: View {
    @ObservedObject var store: ConversationStore
    @ObservedObject var providers: ProviderStore
    /// Live node-level projections keyed by run id (PR-029/PR-030). When a run
    /// has a projection, the thread renders a `PipelineRunCard` (node-count
    /// summary + mini graph + controls) alongside the PR-027 `RunCard`. Optional
    /// so the thread still renders in contexts without a pipeline store.
    var pipelines: PipelineProjectionStore?
    /// Invoked when a run card's "Open live graph" is pressed.
    var onOpenGraph: (String) -> Void = { _ in }
    /// Real project git context (branch + uncommitted-change count) for the compact
    /// top context bar (UX-101 / §15.3). Defaults to "no repo" so the thread still
    /// renders in contexts without a git store.
    var gitContext: ChatGitContext = .none

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
            if threadIsEmpty(for: summary.id) {
                EmptyStateView(
                    headline: "No messages yet",
                    detail: "Send a message below to start a run.",
                    systemImage: "text.bubble"
                )
                .frame(maxHeight: .infinity)
            } else {
                messageList(for: summary.id)
            }
            ConversationComposer(store: store, providers: providers, conversationID: summary.id)
        }
    }

    /// UX-101 / §15.3: ONE compact context bar — title + status + the project's real
    /// git context (branch · N changed) + relative time — instead of scattered,
    /// duplicated title/status/proof surfaces.
    private func threadHeader(_ summary: ConversationSummary) -> some View {
        HStack(spacing: Theme.s8) {
            Text(summary.title)
                .font(Theme.ui(15, .semibold))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
                .layoutPriority(1)
            StatusPill(kind: summary.status.pillKind, label: summary.status.displayLabel)
            Spacer(minLength: Theme.s8)
            if gitContext.inRepo {
                gitContextChip
            }
            Text(RelativeTime.string(from: summary.lastActivityDate))
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .lineLimit(1)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("conversation.context-bar")
    }

    /// The compact git chip: real branch + uncommitted-change count, with a menu
    /// listing local branches (current marked). Switching branches lives in the
    /// Changes tab (it has the dirty-buffer preflight), so this is informational.
    private var gitContextChip: some View {
        Menu {
            if gitContext.branchNames.isEmpty {
                Text("No local branches")
            } else {
                ForEach(gitContext.branchNames, id: \.self) { name in
                    Label(
                        name,
                        systemImage: name == gitContext.branch
                            ? "checkmark.circle" : "arrow.triangle.branch"
                    )
                }
            }
            Divider()
            Text("Switch branches in the Changes tab")
        } label: {
            HStack(spacing: 5) {
                Image(systemName: "arrow.triangle.branch")
                    .font(.system(size: 10, weight: .semibold))
                Text(gitContext.branchLabel)
                    .font(Theme.mono(11))
                    .lineLimit(1)
                Text("·")
                    .foregroundStyle(Theme.faint)
                Text("\(gitContext.changedCount) changed")
                    .font(Theme.ui(10.5))
                    .foregroundStyle(Theme.muted)
            }
            .foregroundStyle(Theme.textSoft)
            .padding(.horizontal, Theme.s8)
            .frame(height: 24)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .fill(Theme.panel)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .strokeBorder(Theme.stroke, lineWidth: 1)
            )
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Branch and uncommitted-change count for this project.")
        .accessibilityIdentifier("conversation.git-context")
        .accessibilityLabel("Branch \(gitContext.branchLabel), \(gitContext.changedCount) changed files")
    }

    private func threadIsEmpty(for conversationID: String) -> Bool {
        store.timelineItems(for: conversationID).isEmpty && store.messages.isEmpty
    }

    private func messageList(for conversationID: String) -> some View {
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
                let timeline = store.timelineItems(for: conversationID)
                if timeline.isEmpty {
                    ForEach(store.messages) { message in
                        MessageCell(message: message)
                            .id(message.id)
                        renderRunCards(for: message)
                    }
                } else {
                    ForEach(timeline) { item in
                        if let message = item.message {
                            MessageCell(message: message)
                                .id(item.id)
                            renderRunCards(for: message, timelineRunID: item.runId)
                        } else if let card = item.commitCard {
                            CommitReceiptCard(card: card)
                                .id(item.id)
                        } else if let card = item.pushCard {
                            PushReceiptCard(card: card)
                                .id(item.id)
                        } else if item.kind == .assistantMessage {
                            AssistantTimelineEventCell(item: item)
                                .id(item.id)
                        } else {
                            TimelineItemCell(item: item)
                                .id(item.id)
                        }
                    }
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 16)
        }
    }

    @ViewBuilder
    private func renderRunCards(for message: ConversationMessage, timelineRunID: String? = nil) -> some View {
        let run = timelineRunID.flatMap { store.run(forRunID: $0) } ?? store.run(forMessageID: message.id)
        if let run {
            RunCard(run: run)
                .id("run-\(run.runId)")
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

// MARK: - Assistant timeline event cell

struct AssistantTimelineEventCell: View {
    let item: ConversationTimelineItem

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            HStack(spacing: Theme.s6) {
                Image(systemName: "sparkles")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.violet)
                Text("Assistant")
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(Theme.violet)
                Text(stateLabel)
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.faint)
                if let model = item.payload.modelId {
                    Text(model)
                        .font(Theme.mono(10))
                        .foregroundStyle(Theme.faint)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 0)
                Text(RelativeTime.string(from: item.createdAtDate))
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }
            Text(bodyText)
                .font(Theme.ui(13))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            if !detailRows.isEmpty {
                VStack(alignment: .leading, spacing: Theme.s4) {
                    ForEach(detailRows) { row in
                        HStack(alignment: .firstTextBaseline, spacing: Theme.s6) {
                            Text(row.label)
                                .font(Theme.mono(9.5, .semibold))
                                .foregroundStyle(Theme.faint)
                                .frame(width: 86, alignment: .leading)
                            Text(row.value)
                                .font(Theme.ui(10.5))
                                .foregroundStyle(Theme.textSoft)
                                .lineLimit(2)
                                .truncationMode(.middle)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                        }
                    }
                }
                .padding(.top, Theme.s4)
            }
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
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.timeline.assistantEvent")
        .accessibilityLabel("Assistant \(stateLabel): \(bodyText)")
    }

    private var stateLabel: String {
        switch item.state {
        case "streaming": return "Streaming"
        case "completed": return "Completed"
        default: return item.state
        }
    }

    private var bodyText: String {
        item.payload.assistantDelta
            ?? item.payload.assistantText
            ?? item.payload.contentRedacted
            ?? "Assistant event"
    }

    private var detailRows: [TimelineDetailRow] {
        var rows: [TimelineDetailRow] = []
        append("Message", item.payload.assistantMessageId, to: &rows)
        append("Provider", item.payload.providerId, to: &rows)
        append("Model", item.payload.modelId, to: &rows)
        append("Response", responseSummary, to: &rows)
        append("Reason", item.payload.completionReason, to: &rows)
        append("Event", item.payload.eventId, to: &rows)
        return Array(rows.prefix(6))
    }

    private var responseSummary: String? {
        var parts: [String] = []
        if let bytes = item.payload.responseBytes {
            parts.append("\(bytes) bytes")
        }
        if let hash = item.payload.responseHash {
            parts.append(hash)
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private func append(_ label: String, _ value: String?, to rows: inout [TimelineDetailRow]) {
        guard let value, !value.isEmpty else { return }
        rows.append(TimelineDetailRow(label: label, value: value))
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
        case .tool: return "Tool"
        case .event: return "Event"
        case .unknown: return "Message"
        }
    }

    private var roleSymbol: String {
        switch message.role {
        case .user: return "person.fill"
        case .assistant: return "sparkles"
        case .system: return "gearshape.fill"
        case .tool: return "terminal"
        case .event: return "waveform.path.ecg"
        case .unknown: return "bubble.left"
        }
    }

    private var roleTint: Color {
        switch message.role {
        case .user: return Theme.accent
        case .assistant: return Theme.violet
        case .system, .tool, .event: return Theme.muted
        case .unknown: return Theme.muted
        }
    }

    private var cellFill: Color {
        message.role == .user ? Theme.input : Theme.panel
    }
}

// MARK: - Generic timeline cell

struct TimelineItemCell: View {
    let item: ConversationTimelineItem

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: symbol)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(tint)
                Text(item.kind.displayLabel)
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(tint)
                Text(item.state)
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.faint)
                Spacer(minLength: 0)
                Text(RelativeTime.string(from: item.createdAtDate))
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }
            Text(item.payload.contentRedacted ?? item.id)
                .font(Theme.ui(13))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            if !detailRows.isEmpty {
                VStack(alignment: .leading, spacing: Theme.s4) {
                    ForEach(detailRows) { row in
                        HStack(alignment: .firstTextBaseline, spacing: Theme.s6) {
                            Text(row.label)
                                .font(Theme.mono(9.5, .semibold))
                                .foregroundStyle(Theme.faint)
                                .frame(width: 74, alignment: .leading)
                            Text(row.value)
                                .font(Theme.ui(10.5))
                                .foregroundStyle(Theme.textSoft)
                                .lineLimit(2)
                                .truncationMode(.middle)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                        }
                    }
                }
                .padding(.top, Theme.s4)
            }
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
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(item.kind.displayLabel): \(item.payload.contentRedacted ?? item.state)")
    }

    private var symbol: String {
        switch item.kind {
        case .plan: return "list.bullet.rectangle"
        case .toolCall: return "terminal"
        case .worker: return "person.2"
        case .patch: return "doc.badge.gearshape"
        case .verification: return "checkmark.seal"
        case .approval: return "hand.raised"
        case .commitReceipt: return "checkmark.circle"
        case .pushReceipt: return "arrow.up.circle"
        case .imageArtifact: return "photo"
        case .warning: return "exclamationmark.triangle"
        case .error: return "xmark.octagon"
        case .userMessage: return "person.fill"
        case .assistantMessage: return "sparkles"
        case .unknown: return "circle.dashed"
        }
    }

    private var tint: Color {
        switch item.kind {
        case .error: return Theme.coral
        case .warning, .approval: return Theme.gold
        case .verification, .commitReceipt, .pushReceipt: return Theme.accent
        case .assistantMessage: return Theme.violet
        case .userMessage: return Theme.accent
        default: return Theme.muted
        }
    }

    private var detailRows: [TimelineDetailRow] {
        var rows: [TimelineDetailRow] = []
        append("Tool", item.payload.tool, to: &rows)
        append("Worker", item.payload.workerId, to: &rows)
        append("Work item", item.payload.workItemId, to: &rows)
        append("Role", item.payload.roleLabel, to: &rows)
        append("Model", item.payload.modelId, to: &rows)
        append("Command", item.payload.commandRedacted, to: &rows)
        if let exit = item.payload.exitCode {
            let timeout = item.payload.timedOut == true ? " · timed out" : ""
            append("Exit", "\(exit)\(timeout)", to: &rows)
        }
        if let duration = item.payload.durationMs {
            append("Duration", "\(duration) ms", to: &rows)
        }
        append("Files", pathSummary(item.payload.appliedFiles ?? item.payload.targetPaths ?? item.payload.touchedPaths), to: &rows)
        append("Tests", pathSummary(item.payload.testTargets), to: &rows)
        if let patchCount = item.payload.patchCount, let applyCount = item.payload.applyResultCount {
            append("Patch", "\(patchCount) patches · \(applyCount) results", to: &rows)
        } else if let patchCount = item.payload.patchCount {
            append("Patch", "\(patchCount) patches", to: &rows)
        }
        append("Code", item.payload.code ?? item.payload.reasonCode, to: &rows)
        append("Batch", batchSummary, to: &rows)
        append("Receipt", item.payload.receiptRef ?? item.payload.verificationRef ?? item.payload.repairRef, to: &rows)
        return Array(rows.prefix(8))
    }

    private var batchSummary: String? {
        guard item.payload.parallelBatch == true || item.payload.batchId != nil else { return nil }
        var parts: [String] = []
        if let batchId = item.payload.batchId {
            parts.append(batchId)
        }
        if let lane = item.payload.parallelLaneIndex {
            parts.append("lane \(lane)")
        }
        if let size = item.payload.parallelBatchSize {
            parts.append("size \(size)")
        }
        return parts.isEmpty ? "parallel batch" : parts.joined(separator: " · ")
    }

    private func pathSummary(_ values: [String]?) -> String? {
        guard let values, !values.isEmpty else { return nil }
        let visible = values.prefix(3).joined(separator: ", ")
        if values.count > 3 {
            return "\(visible) +\(values.count - 3)"
        }
        return visible
    }

    private func append(_ label: String, _ value: String?, to rows: inout [TimelineDetailRow]) {
        guard let value, !value.isEmpty else { return }
        rows.append(TimelineDetailRow(label: label, value: value))
    }
}

private struct TimelineDetailRow: Identifiable {
    let label: String
    let value: String

    var id: String { label }
}
