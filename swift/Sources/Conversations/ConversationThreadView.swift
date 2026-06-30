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
    /// Active workspace folder, shown next to the composer so operators can jump
    /// to the project path from the same place they send work.
    var workspaceURL: URL?
    var openProject: (URL) -> Void = { _ in }

    @State private var activeScrollConversationID: String?
    @State private var isFollowingLatest = true
    @State private var latestAnchorVisible = false
    @State private var pendingInitialLatestScrollConversationID: String?
    @State private var showingHeaderFailureDetails = false

    private let latestAnchorID = "conversation.thread.latest"

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
        let workerItems = shouldShowWorkerRail(for: summary.status)
            ? workerRailTimelineItems(store.timelineItems(for: summary.id))
            : []
        VStack(spacing: 0) {
            threadHeader(summary)
            Divider().overlay(Theme.stroke)
            HStack(spacing: 0) {
                VStack(spacing: 0) {
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
                    ConversationComposer(
                        store: store,
                        providers: providers,
                        conversationID: summary.id,
                        workspaceURL: workspaceURL,
                        openProject: openProject
                    )
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)

                if !workerItems.isEmpty {
                    Divider().overlay(Theme.stroke)
                    WorkerRailView(items: workerItems)
                        .frame(width: 304)
                }
            }
        }
    }

    private func shouldShowWorkerRail(for status: ConversationStatus) -> Bool {
        switch status {
        case .queued, .running, .waitingForInput, .waitingForApproval, .paused, .failed:
            return true
        case .idle, .completed, .archived, .unknown:
            return false
        }
    }

    /// UX-101 / §15.3: ONE compact context bar — title + status + the project's real
    /// git context (branch · N changed) + relative time — instead of scattered,
    /// duplicated title/status/proof surfaces.
    private func threadHeader(_ summary: ConversationSummary) -> some View {
        let failureDiagnostics = headerFailureDiagnostics(
            for: summary.id,
            projectId: summary.projectId,
            status: summary.status
        )
        return HStack(spacing: Theme.s8) {
            Text(summary.title)
                .font(Theme.ui(15, .semibold))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
                .layoutPriority(1)
            if let failureDiagnostics {
                Button {
                    showingHeaderFailureDetails = true
                } label: {
                    HStack(spacing: Theme.s4) {
                        StatusPill(kind: summary.status.pillKind, label: summary.status.displayLabel)
                        Image(systemName: "info.circle")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(summary.status.pillKind.tint)
                    }
                    .padding(.trailing, 2)
                    .frame(minHeight: 24)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Show failure details")
                .accessibilityLabel("Show \(summary.status.displayLabel.lowercased()) conversation details")
                .accessibilityIdentifier("conversation.header.failureDetails")
                .popover(isPresented: $showingHeaderFailureDetails, arrowEdge: .bottom) {
                    RunFailureDiagnosticsPopover(
                        diagnostics: failureDiagnostics,
                        shortRunID: shortRunID(failureDiagnostics.runId)
                    )
                }
            } else {
                StatusPill(kind: summary.status.pillKind, label: summary.status.displayLabel)
            }
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
        .accessibilityIdentifier("conversation.context-bar")
    }

    private func headerFailureDiagnostics(
        for conversationID: String,
        projectId: String,
        status: ConversationStatus
    ) -> RunFailureDiagnostics? {
        guard shouldExposeConversationHeaderFailureDetails(status: status) else { return nil }
        let timeline = store.timelineItems(for: conversationID)
        let runs = store.runs(for: conversationID)
        if let run = runs.last(where: { $0.runState == .failed || $0.runState == .cancelled }) {
            let relatedTimeline = timeline.filter { item in
                item.runId == run.runId
                    || item.turnId == run.turnId
                    || item.payload.messageId == run.messageId
                    || item.payload.assistantMessageId == run.messageId
            }
            let fallbackSequence = relatedTimeline.last?.sequence ?? 0
            let fallbackCreatedAtMs = relatedTimeline.first?.createdAtMs ?? 0
            let fallbackUpdatedAtMs = relatedTimeline.last?.updatedAtMs ?? fallbackCreatedAtMs
            let message = store.messages.first { $0.id == run.messageId }
                ?? latestTimelineMessage(for: run, in: timeline)
                ?? ConversationMessage(
                    schema: "opensks.conversation-message.v1",
                    id: run.messageId,
                    projectId: projectId,
                    conversationId: conversationID,
                    turnId: run.turnId,
                    role: .assistant,
                    state: .failed,
                    contentRedacted: run.runState.displayLabel,
                    sequence: fallbackSequence,
                    createdAtMs: fallbackCreatedAtMs,
                    updatedAtMs: fallbackUpdatedAtMs
                )
            return failureDiagnostics(for: run, message: message, timeline: timeline)
        }

        let signals = timeline.filter(isFailureSignal)
        let evidence = signals.isEmpty ? timeline.filter { $0.kind == .error || $0.kind == .warning } : signals
        let fallbackItem = evidence.last ?? timeline.last
        let fallbackSequence = fallbackItem?.sequence ?? store.messages.last?.sequence ?? 0
        let fallbackCreatedAtMs = fallbackItem?.createdAtMs ?? store.messages.last?.createdAtMs ?? 0
        let fallbackUpdatedAtMs = fallbackItem?.updatedAtMs ?? store.messages.last?.updatedAtMs ?? fallbackCreatedAtMs
        let message = fallbackItem.flatMap(timelineMessage)
            ?? store.messages.last(where: { $0.state == .failed || $0.role == .assistant })
            ?? ConversationMessage(
                schema: "opensks.conversation-message.v1",
                id: fallbackItem?.payload.messageId ?? fallbackItem?.id ?? "\(conversationID)-failure-message",
                projectId: projectId,
                conversationId: conversationID,
                turnId: fallbackItem?.turnId,
                role: .assistant,
                state: .failed,
                contentRedacted: fallbackItem.flatMap(failureSignalText) ?? status.displayLabel,
                sequence: fallbackSequence,
                createdAtMs: fallbackCreatedAtMs,
                updatedAtMs: fallbackUpdatedAtMs
            )
        let syntheticRun = ConversationRunRef(
            turnId: fallbackItem?.turnId ?? message.turnId ?? "conversation-\(conversationID)",
            runId: fallbackItem?.runId ?? "conversation-\(conversationID)-failure",
            messageId: message.id,
            relation: "primary",
            runState: .failed
        )
        return buildFailureDiagnostics(
            for: syntheticRun,
            message: message,
            evidence: evidence.isEmpty ? fallbackItem.map { [$0] } ?? [] : evidence
        )
    }

    private func latestTimelineMessage(
        for run: ConversationRunRef,
        in timeline: [ConversationTimelineItem]
    ) -> ConversationMessage? {
        timeline
            .filter { item in
                item.runId == run.runId
                    || item.turnId == run.turnId
                    || item.payload.messageId == run.messageId
                    || item.payload.assistantMessageId == run.messageId
            }
            .reversed()
            .compactMap(timelineMessage)
            .first
    }

    private func shortRunID(_ runID: String) -> String {
        String(runID.suffix(8))
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
        store.timelineItems(for: conversationID).isEmpty
            && store.messages.isEmpty
            && store.localPendingSend(for: conversationID) == nil
    }

    private func localPendingMessages(for conversationID: String) -> [ConversationMessage] {
        localPendingConversationMessages(store.localPendingSend(for: conversationID), conversationID: conversationID)
    }

    private func messageList(for conversationID: String) -> some View {
        let latestToken = latestRenderToken(for: conversationID)
        let localSendToken = store.localSendToken(for: conversationID)
        let sending = store.isSending(conversationID: conversationID)

        return ScrollViewReader { proxy in
            ZStack(alignment: .bottomTrailing) {
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
                        let fullTimeline = store.timelineItems(for: conversationID)
                        let latestErrorTextByRunID = latestErrorTextByRunID(in: fullTimeline)
                        let timeline = mainThreadTimelineItems(
                            fullTimeline,
                            additionalDurableAssistantMessageIDs: durableAssistantMessageIDs(in: store.messages)
                        )
                        if timeline.isEmpty {
                            let pendingMessages = localPendingMessages(for: conversationID)
                            if !pendingMessages.isEmpty {
                                ForEach(pendingMessages) { message in
                                    MessageCell(message: message)
                                        .id(message.id)
                                }
                            } else {
                                ForEach(orderedConversationMessages(store.messages)) { message in
                                    MessageCell(message: message)
                                        .id(message.id)
                                    renderRunCards(for: message, timeline: fullTimeline)
                                }
                            }
                        } else {
                            ForEach(timeline) { item in
                                if let message = timelineMessage(for: item) {
                                    MessageCell(message: message)
                                        .id(item.id)
                                    renderRunCards(
                                        for: message,
                                        timeline: fullTimeline,
                                        timelineRunID: item.runId,
                                        forcedFailureSummary: item.runId.flatMap { latestErrorTextByRunID[$0] }
                                    )
                                } else if let card = CodeChangeSummaryCardModel(item: item) {
                                    CodeChangeSummaryCard(
                                        card: card,
                                        isApplying: store.isApplyingIntegration(runID: card.runID)
                                    ) { card in
                                        guard let runID = card.runID else { return }
                                        Task {
                                            await store.applyIntegrationCandidate(
                                                runID: runID,
                                                conversationID: conversationID
                                            )
                                        }
                                    }
                                        .id(item.id)
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
                            if let assistantReply = latestAssistantReplyMessage(
                                in: fullTimeline,
                                durableMessages: store.messages
                            ),
                               shouldRenderPinnedAssistantReply(assistantReply, in: timeline) {
                                MessageCell(message: assistantReply)
                                    .id("latest-assistant-reply-\(assistantReply.id)-\(assistantReply.updatedAtMs)")
                            }
                            let pendingMessages = appendedLocalPendingMessagesForRenderedTimeline(
                                timeline,
                                pendingMessages: localPendingMessages(for: conversationID)
                            )
                            ForEach(pendingMessages) { message in
                                MessageCell(message: message)
                                    .id(message.id)
                            }
                        }
                        Color.clear
                            .frame(height: 1)
                            .id(latestAnchorID)
                            .onAppear {
                                latestAnchorVisible = true
                                isFollowingLatest = true
                            }
                            .onDisappear {
                                latestAnchorVisible = false
                                pauseFollowingLatestIfStillAway(conversationID: conversationID)
                            }
                    }
                    .padding(.horizontal, 18)
                    .padding(.vertical, 16)
                }
                .accessibilityIdentifier("conversation.messageList")
                .onAppear {
                    resetLatestScrollState(for: conversationID)
                    scrollToLatest(proxy, animated: false)
                }
                .onChange(of: conversationID) { newValue in
                    resetLatestScrollState(for: newValue)
                    scrollToLatest(proxy, animated: false)
                }
                .onChange(of: latestToken) { _ in
                    if pendingInitialLatestScrollConversationID == conversationID {
                        pendingInitialLatestScrollConversationID = nil
                        isFollowingLatest = true
                    }
                    guard isFollowingLatest else { return }
                    scrollToLatest(proxy, animated: true)
                }
                .onChange(of: localSendToken) { _ in
                    isFollowingLatest = true
                    scrollToLatest(proxy, animated: true)
                }
                .onChange(of: sending) { isSending in
                    guard !isSending, isFollowingLatest else { return }
                    scrollToLatest(proxy, animated: true)
                }

                if !isFollowingLatest {
                    Button {
                        isFollowingLatest = true
                        scrollToLatest(proxy, animated: true)
                    } label: {
                        Image(systemName: "arrow.down")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Theme.accent)
                    }
                    .buttonStyle(IconTileButtonStyle(size: 34))
                    .background(
                        RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                            .fill(Theme.panel)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                            .strokeBorder(Theme.stroke, lineWidth: 1)
                    )
                    .padding(.trailing, 20)
                    .padding(.bottom, 14)
                    .help("Jump to latest")
                    .accessibilityLabel("Jump to latest message")
                    .accessibilityIdentifier("conversation.jumpToLatest")
                }
            }
        }
    }

    private func resetLatestScrollState(for conversationID: String) {
        activeScrollConversationID = conversationID
        latestAnchorVisible = false
        isFollowingLatest = true
        pendingInitialLatestScrollConversationID = conversationID
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_600_000_000)
            guard pendingInitialLatestScrollConversationID == conversationID else { return }
            pendingInitialLatestScrollConversationID = nil
            if activeScrollConversationID == conversationID, !latestAnchorVisible {
                isFollowingLatest = false
            }
        }
    }

    private func scrollToLatest(_ proxy: ScrollViewProxy, animated: Bool) {
        let scroll: () -> Void = {
            if animated {
                withAnimation(.easeOut(duration: 0.16)) {
                    proxy.scrollTo(latestAnchorID, anchor: .bottom)
                }
            } else {
                proxy.scrollTo(latestAnchorID, anchor: .bottom)
            }
        }
        for delay in [0.0, 0.24, 0.72] {
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
                scroll()
            }
        }
    }

    private func pauseFollowingLatestIfStillAway(conversationID: String) {
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 900_000_000)
            guard activeScrollConversationID == conversationID, !latestAnchorVisible else { return }
            guard pendingInitialLatestScrollConversationID != conversationID else { return }
            guard !store.isSending(conversationID: conversationID) else { return }
            isFollowingLatest = false
        }
    }

    private func latestRenderToken(for conversationID: String) -> String {
        let timeline = store.timelineItems(for: conversationID)
        let assistantReply = latestAssistantReplyMessage(in: timeline, durableMessages: store.messages)
        if let last = timeline.last {
            return [
                conversationID,
                "timeline",
                "\(timeline.count)",
                last.id,
                last.state,
                last.payload.contentRedacted ?? "",
                last.payload.assistantDelta ?? "",
                last.payload.assistantText ?? "",
                last.payload.eventId ?? "",
                last.payload.responseHash ?? "",
                assistantReply?.id ?? "",
                assistantReply?.state.rawValue ?? "",
                assistantReply?.contentRedacted ?? ""
            ].joined(separator: "|")
        }

        if let last = orderedConversationMessages(store.messages).last {
            return [
                conversationID,
                "messages",
                "\(store.messages.count)",
                last.id,
                last.state.rawValue,
                "\(last.updatedAtMs)",
                "\(last.contentRedacted.count)"
            ].joined(separator: "|")
        }

        return "\(conversationID)|empty"
    }

    private func timelineMessage(for item: ConversationTimelineItem) -> ConversationMessage? {
        if let message = item.message {
            return message
        }
        guard let messageId = item.payload.messageId,
              item.payload.assistantMessageId == nil,
              let content = item.payload.contentRedacted
        else { return nil }
        let role: MessageRole
        switch item.kind {
        case .userMessage:
            role = .user
        case .assistantMessage:
            role = .assistant
        default:
            return nil
        }
        let state: MessageState = item.state == "streaming" ? .streaming : .complete
        return ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: messageId,
            projectId: item.projectId,
            conversationId: item.conversationId,
            turnId: item.turnId,
            role: role,
            state: state,
            contentRedacted: content,
            sequence: item.sequence,
            createdAtMs: item.createdAtMs,
            updatedAtMs: item.updatedAtMs
        )
    }

    @ViewBuilder
    private func renderRunCards(
        for message: ConversationMessage,
        timeline: [ConversationTimelineItem]? = nil,
        timelineRunID: String? = nil,
        forcedFailureSummary: String? = nil
    ) -> some View {
        let run = timelineRunID.flatMap { store.run(forRunID: $0) } ?? store.run(forMessageID: message.id)
        if let run {
            let timeline = timeline ?? store.timelineItems(for: message.conversationId)
            let diagnostics = failureDiagnostics(for: run, message: message, timeline: timeline)
            let resolvedFailureSummary = forcedFailureSummary ?? latestErrorTextByRunID(in: timeline)[run.runId]
            let finalDiagnostics = resolvedFailureSummary.map { summary in
                RunFailureDiagnostics(
                    run: run,
                    summary: summary,
                    details: diagnostics?.details ?? [],
                    recoveryHints: diagnostics?.recoveryHints ?? []
                )
            } ?? diagnostics
            RunCard(
                run: run,
                failureDiagnostics: finalDiagnostics
            )
                .id("run-\(run.runId)-\(finalDiagnostics?.summary ?? run.runState.rawValue)")
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

    private func failureDiagnostics(
        for run: ConversationRunRef,
        message: ConversationMessage,
        timeline: [ConversationTimelineItem]
    ) -> RunFailureDiagnostics? {
        guard run.runState == .failed || run.runState == .cancelled else { return nil }
        let related = timeline.filter { item in
            item.runId == run.runId
                || item.turnId == run.turnId
                || item.payload.messageId == run.messageId
                || item.payload.assistantMessageId == message.id
        }
        let evidenceCandidates = dedupTimelineItems(
            related
                + adjacentFailureSignals(after: message, in: timeline)
                + conversationFailureSignals(for: message.conversationId, in: timeline)
        )
        let signals = evidenceCandidates.filter(isFailureSignal)
        let evidence = signals.isEmpty ? evidenceCandidates : signals

        return buildFailureDiagnostics(for: run, message: message, evidence: evidence)
    }

    private func conversationFailureSignals(
        for conversationID: String,
        in timeline: [ConversationTimelineItem]
    ) -> [ConversationTimelineItem] {
        timeline.filter { item in
            item.conversationId == conversationID
                && (item.kind == .error || item.state.lowercased().contains("fail"))
                && isFailureSignal(item)
        }
    }

    private func adjacentFailureSignals(
        after message: ConversationMessage,
        in timeline: [ConversationTimelineItem]
    ) -> [ConversationTimelineItem] {
        let nextMessageSequence = timeline
            .filter { item in
                item.conversationId == message.conversationId
                    && item.sequence > message.sequence
                    && (item.kind == .userMessage || item.kind == .assistantMessage)
                    && item.payload.messageId != nil
            }
            .map(\.sequence)
            .min() ?? Int64.max
        return timeline.filter { item in
            item.conversationId == message.conversationId
                && item.sequence >= message.sequence
                && item.sequence < nextMessageSequence
                && isFailureSignal(item)
        }
    }

    private func dedupTimelineItems(_ items: [ConversationTimelineItem]) -> [ConversationTimelineItem] {
        var seen = Set<String>()
        return items
            .sorted {
                if $0.sequence != $1.sequence {
                    return $0.sequence < $1.sequence
                }
                return $0.id < $1.id
            }
            .filter { item in
                guard !seen.contains(item.id) else { return false }
                seen.insert(item.id)
                return true
            }
    }

    private func buildFailureDiagnostics(
        for run: ConversationRunRef,
        message: ConversationMessage,
        evidence: [ConversationTimelineItem]
    ) -> RunFailureDiagnostics {
        var details: [RunFailureDetail] = []
        appendFailureDetail("Run", run.runId, to: &details)
        appendFailureDetail("Turn", run.turnId, to: &details)
        appendFailureDetail("Relation", run.relation, to: &details)
        for item in evidence.suffix(6) {
            appendFailureDetail(item.kind.displayLabel, failureSignalText(item), to: &details)
            appendFailureDetail("State", "\(item.kind.displayLabel) · \(item.state)", to: &details)
            appendFailureDetail("Worker", item.payload.workerId, to: &details)
            appendFailureDetail("Tool", item.payload.tool, to: &details)
            appendFailureDetail("Command", item.payload.commandRedacted, to: &details)
            if let exitCode = item.payload.exitCode, exitCode != 0 {
                appendFailureDetail("Exit", "\(exitCode)", to: &details)
            }
            if item.payload.timedOut == true {
                appendFailureDetail("Timeout", "The command or worker timed out.", to: &details)
            }
            appendFailureDetail("Reason", item.payload.reasonCode ?? item.payload.code, to: &details)
        }

        return RunFailureDiagnostics(
            run: run,
            summary: failureSummary(message: message, evidence: evidence),
            details: dedupFailureDetails(details),
            recoveryHints: failureRecoveryHints(message: message, evidence: evidence)
        )
    }

    private func isFailureSignal(_ item: ConversationTimelineItem) -> Bool {
        let state = item.state.lowercased()
        return item.kind == .error
            || state.contains("fail")
            || state.contains("error")
            || state.contains("cancel")
            || item.payload.reasonCode != nil
            || item.payload.code != nil
            || (item.payload.exitCode ?? 0) != 0
            || item.payload.timedOut == true
    }

    private func failureSummary(
        message: ConversationMessage,
        evidence: [ConversationTimelineItem]
    ) -> String {
        runFailureSummary(messageContent: message.contentRedacted, evidence: evidence)
    }

    private func failureSignalText(_ item: ConversationTimelineItem) -> String? {
        runFailureSignalText(item)
    }

    private func isVisibleFailureText(_ text: String) -> Bool {
        isVisibleRunFailureText(text)
    }

    private func failureRecoveryHints(
        message: ConversationMessage,
        evidence: [ConversationTimelineItem]
    ) -> [String] {
        var hints: [String] = []
        let failureText = ([message.contentRedacted] + evidence.compactMap(failureSignalText))
            .joined(separator: " ")
            .lowercased()
        if failureText.contains("code-capable model")
            || failureText.contains("connect at least one")
            || failureText.contains("model_not_selected")
            || failureText.contains("provider_not_configured")
        {
            hints.append("Select or connect a code-capable provider/model before retrying.")
        }
        if failureText.contains("provider response had no message content")
            || failureText.contains("provider_call_failed")
        {
            hints.append("Retry on the current app build; if this repeats, inspect the provider response shape and model selection first.")
        }
        if evidence.contains(where: { $0.payload.timedOut == true }) {
            hints.append("Reduce the worker scope or increase the timeout before retrying.")
        }
        if evidence.contains(where: { ($0.payload.exitCode ?? 0) != 0 }) {
            hints.append("Open the command output, fix the failing command, then retry the turn.")
        }
        if evidence.contains(where: { $0.payload.reasonCode != nil || $0.payload.code != nil }) {
            hints.append("Use the reason/code signal above as the first fix target.")
        }
        if evidence.contains(where: { $0.payload.workerId != nil }) {
            hints.append("Retire or rerun the failing worker after its specific blocker is fixed.")
        }
        if hints.isEmpty {
            hints.append("Inspect the latest error event and retry after fixing the reported cause.")
        }
        return hints
    }

    private func appendFailureDetail(_ label: String, _ value: String?, to details: inout [RunFailureDetail]) {
        guard let value else { return }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        details.append(RunFailureDetail(label: label, value: trimmed))
    }

    private func dedupFailureDetails(_ details: [RunFailureDetail]) -> [RunFailureDetail] {
        var seen = Set<String>()
        return details.filter { detail in
            let key = "\(detail.label)\u{1f}\(detail.value)"
            guard !seen.contains(key) else { return false }
            seen.insert(key)
            return true
        }
    }
}

func localPendingConversationMessages(
    _ pending: ConversationPendingSend?,
    conversationID: String
) -> [ConversationMessage] {
    guard let pending else { return [] }
    let user = ConversationMessage(
        schema: "opensks.conversation-message.v1",
        id: "local-pending-user-\(pending.token)",
        projectId: pending.projectId,
        conversationId: conversationID,
        turnId: nil,
        role: .user,
        state: .complete,
        contentRedacted: pending.text,
        sequence: Int64.max - 1,
        createdAtMs: pending.createdAtMs,
        updatedAtMs: pending.createdAtMs
    )
    let assistant = ConversationMessage(
        schema: "opensks.conversation-message.v1",
        id: "local-pending-assistant-\(pending.token)",
        projectId: pending.projectId,
        conversationId: conversationID,
        turnId: nil,
        role: .assistant,
        state: .streaming,
        contentRedacted: "Thinking...",
        sequence: Int64.max,
        createdAtMs: pending.createdAtMs,
        updatedAtMs: pending.createdAtMs
    )
    return [user, assistant]
}

func appendedLocalPendingMessagesForRenderedTimeline(
    _ timeline: [ConversationTimelineItem],
    pendingMessages: [ConversationMessage]
) -> [ConversationMessage] {
    timeline.isEmpty ? [] : pendingMessages
}

func shouldExposeConversationHeaderFailureDetails(status: ConversationStatus) -> Bool {
    status == .failed
}

func mainThreadTimelineItems(
    _ timeline: [ConversationTimelineItem],
    additionalDurableAssistantMessageIDs: Set<String> = []
) -> [ConversationTimelineItem] {
    let durableAssistantMessageIDs = durableAssistantMessageIDs(in: timeline)
        .union(additionalDurableAssistantMessageIDs)
    let integratedApplyRunIDs = integratedIntegrationApplyRunIDs(in: timeline)
    let latestIntegratedApplyItemIDs = latestIntegrationApplyItemIDs(in: timeline)
    return orderedConversationTimelineItems(timeline).filter { item in
        !isAssistantExecutionEvent(item)
            && !isSupersededAssistantEvent(item, durableAssistantMessageIDs: durableAssistantMessageIDs)
            && !isSupersededIntegrationCandidateItem(item, integratedApplyRunIDs: integratedApplyRunIDs)
            && !isSupersededIntegrationFailureItem(item, integratedApplyRunIDs: integratedApplyRunIDs)
            && !isSupersededIntegrationApplyCompletedItem(
                item,
                latestIntegrationApplyItemIDs: latestIntegratedApplyItemIDs
            )
            && !isWorkerRailTimelineItem(item, durableAssistantMessageIDs: durableAssistantMessageIDs)
    }
}

func workerRailTimelineItems(_ timeline: [ConversationTimelineItem]) -> [ConversationTimelineItem] {
    let durableAssistantMessageIDs = durableAssistantMessageIDs(in: timeline)
    var groups: [String: [ConversationTimelineItem]] = [:]
    for item in timeline where isWorkerRailTimelineItem(item, durableAssistantMessageIDs: durableAssistantMessageIDs) {
        groups[workerRailGroupKey(for: item), default: []].append(item)
    }
    let representatives = groups.values
        .compactMap(workerRailRepresentativeItem)
    return Array(orderedConversationTimelineItems(representatives).suffix(16))
}

func isActiveWorkerItem(_ item: ConversationTimelineItem) -> Bool {
    let tokens = [
        item.state,
        item.payload.eventKind,
        item.payload.agentEventKind,
        item.payload.message,
        item.payload.contentRedacted
    ]
    .compactMap { $0?.lowercased() }
    .joined(separator: " ")

    if tokens.contains("complete")
        || tokens.contains("success")
        || tokens.contains("passed")
        || tokens.contains("fail")
        || tokens.contains("error")
        || tokens.contains("cancel") {
        return false
    }
    return tokens.contains("running")
        || tokens.contains("queued")
        || tokens.contains("leased")
        || tokens.contains("started")
        || tokens.contains("stream")
        || tokens.contains("progress")
}

private func isSupersededAssistantEvent(
    _ item: ConversationTimelineItem,
    durableAssistantMessageIDs: Set<String>
) -> Bool {
    guard item.kind == .assistantMessage,
          let assistantMessageId = item.payload.assistantMessageId,
          durableAssistantMessageIDs.contains(assistantMessageId)
    else { return false }
    return true
}

private func isAssistantExecutionEvent(_ item: ConversationTimelineItem) -> Bool {
    guard item.kind == .assistantMessage else { return false }
    if item.payload.messageId != nil || item.payload.role == .assistant {
        return false
    }
    return item.payload.agentEventKind != nil
        || item.payload.workerId != nil
        || item.payload.assistantMessageId != nil
        || item.payload.projection == "assistant_execution_event"
}

private func workerRailGroupKey(for item: ConversationTimelineItem) -> String {
    let scope = item.runId ?? item.turnId ?? item.conversationId
    if let worker = normalizedWorkerRailToken(item.payload.workerId) {
        return "\(scope)|worker|\(worker)"
    }
    if let holder = normalizedWorkerRailToken(item.payload.leaseHolder) {
        return "\(scope)|lease|\(holder)"
    }
    if let workItem = normalizedWorkerRailToken(item.payload.workItemId) {
        return "\(scope)|work|\(workItem)"
    }
    if let role = normalizedWorkerRailToken(item.payload.roleLabel) {
        return "\(scope)|role|\(role)"
    }
    if let event = normalizedWorkerRailToken(item.payload.agentEventKind ?? item.payload.eventKind) {
        return "\(scope)|event|\(event)"
    }
    return "\(scope)|\(item.kind.rawValue)|\(item.id)"
}

private func workerRailRepresentativeItem(_ items: [ConversationTimelineItem]) -> ConversationTimelineItem? {
    items.max { lhs, rhs in
        let left = (workerRailRepresentativeScore(lhs), lhs.sequence, lhs.updatedAtMs, lhs.id)
        let right = (workerRailRepresentativeScore(rhs), rhs.sequence, rhs.updatedAtMs, rhs.id)
        return left < right
    }
}

private func workerRailRepresentativeScore(_ item: ConversationTimelineItem) -> Int {
    if isWorkerRailFailureSignal(item) {
        return 100
    }
    if hasWorkerRailDialogue(item) {
        return 90
    }
    switch item.kind {
    case .worker:
        return item.state.lowercased().contains("running") ? 80 : 70
    case .patch:
        return 65
    case .verification:
        return 60
    case .plan:
        return 55
    case .approval:
        return 50
    case .toolCall:
        return 20
    case .userMessage, .assistantMessage, .commitReceipt, .pushReceipt, .imageArtifact, .warning, .error, .unknown:
        return 40
    }
}

private func hasWorkerRailDialogue(_ item: ConversationTimelineItem) -> Bool {
    if isNonEmptyWorkerRailText(item.payload.assistantText)
        || isNonEmptyWorkerRailText(item.payload.assistantDelta)
        || isNonEmptyWorkerRailText(item.payload.message) {
        return true
    }
    guard item.kind == .worker || item.kind == .assistantMessage else { return false }
    return isNonEmptyWorkerRailText(item.payload.contentRedacted)
}

private func isWorkerRailFailureSignal(_ item: ConversationTimelineItem) -> Bool {
    let state = item.state.lowercased()
    return item.kind == .error
        || state.contains("fail")
        || state.contains("error")
        || state.contains("cancel")
        || item.payload.reasonCode != nil
        || item.payload.code != nil
        || (item.payload.exitCode ?? 0) != 0
        || item.payload.timedOut == true
}

private func normalizedWorkerRailToken(_ value: String?) -> String? {
    let normalized = value?
        .trimmingCharacters(in: .whitespacesAndNewlines)
        .lowercased()
    guard let normalized, !normalized.isEmpty else { return nil }
    return normalized
}

private func isNonEmptyWorkerRailText(_ value: String?) -> Bool {
    guard let value else { return false }
    let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
    return !trimmed.isEmpty && trimmed != "..."
}

private func durableAssistantMessageIDs(in timeline: [ConversationTimelineItem]) -> Set<String> {
    Set(timeline.compactMap { item in
        guard item.kind == .assistantMessage,
              item.payload.role == .assistant,
              let messageID = item.payload.messageId
        else { return nil }
        return messageID
    })
}

private func durableAssistantMessageIDs(in messages: [ConversationMessage]) -> Set<String> {
    Set(messages.compactMap { message in
        guard message.role == .assistant,
              isVisibleAssistantReply(message.contentRedacted)
        else { return nil }
        return message.id
    })
}

private func isWorkerRailTimelineItem(
    _ item: ConversationTimelineItem,
    durableAssistantMessageIDs: Set<String>
) -> Bool {
    if item.kind == .assistantMessage,
       item.payload.role != .assistant,
       (item.payload.agentEventKind != nil
           || item.payload.eventId != nil
           || item.payload.workerId != nil
           || item.payload.roleLabel != nil
           || item.payload.assistantMessageId.map { durableAssistantMessageIDs.contains($0) } == true) {
            return true
    }
    switch item.kind {
    case .warning:
        return isWorkerScopedDiagnostic(item)
    case .error:
        return isWorkerScopedDiagnostic(item)
    case .userMessage, .assistantMessage, .commitReceipt, .pushReceipt, .imageArtifact, .unknown:
        return false
    case .patch:
        return !isMainWorkspacePatchChangeItem(item)
    case .worker:
        return !isIntegrationCandidateReadyItem(item) && !isIntegrationApplyCompletedItem(item)
    case .toolCall, .verification, .plan, .approval:
        return true
    }
}

private func isWorkerScopedDiagnostic(_ item: ConversationTimelineItem) -> Bool {
    item.payload.workerId != nil
        || item.payload.workItemId != nil
        || item.payload.leaseHolder != nil
        || item.payload.roleLabel != nil
        || item.payload.tool != nil
        || item.payload.agentEventKind != nil
        || item.payload.eventKind == ExecutionEventKind.workItemRunning.rawValue
}

private func integratedIntegrationApplyRunIDs(in timeline: [ConversationTimelineItem]) -> Set<String> {
    Set(timeline.compactMap { item in
        guard isIntegrationApplyCompletedItem(item) else { return nil }
        return item.payload.integrationRunId ?? item.runId
    })
}

private func latestIntegrationApplyItemIDs(in timeline: [ConversationTimelineItem]) -> Set<String> {
    var latestByRunID: [String: ConversationTimelineItem] = [:]
    for item in timeline where isIntegrationApplyCompletedItem(item) {
        guard let runID = item.payload.integrationRunId ?? item.runId else { continue }
        if let existing = latestByRunID[runID] {
            let existingKey = (existing.sequence, existing.updatedAtMs, existing.id)
            let candidateKey = (item.sequence, item.updatedAtMs, item.id)
            if existingKey < candidateKey {
                latestByRunID[runID] = item
            }
        } else {
            latestByRunID[runID] = item
        }
    }
    return Set(latestByRunID.values.map(\.id))
}

private func isSupersededIntegrationCandidateItem(
    _ item: ConversationTimelineItem,
    integratedApplyRunIDs: Set<String>
) -> Bool {
    guard isIntegrationCandidateReadyItem(item),
          let runID = item.payload.integrationRunId ?? item.runId
    else { return false }
    return integratedApplyRunIDs.contains(runID)
}

private func isSupersededIntegrationApplyCompletedItem(
    _ item: ConversationTimelineItem,
    latestIntegrationApplyItemIDs: Set<String>
) -> Bool {
    guard isIntegrationApplyCompletedItem(item) else { return false }
    return !latestIntegrationApplyItemIDs.contains(item.id)
}

private func isSupersededIntegrationFailureItem(
    _ item: ConversationTimelineItem,
    integratedApplyRunIDs: Set<String>
) -> Bool {
    guard item.kind == .error,
          let runID = item.payload.integrationRunId ?? item.runId,
          integratedApplyRunIDs.contains(runID),
          item.payload.eventKind == ExecutionEventKind.verificationFailed.rawValue,
          item.payload.repairRef?.contains("/integration-candidates/") == true
    else { return false }
    return true
}

func isIntegrationCandidateReadyItem(_ item: ConversationTimelineItem) -> Bool {
    item.kind == .worker
        && item.payload.code == "integration_candidate_ready"
        && item.payload.approvalRequired == true
        && item.payload.mainWorkspaceModified == false
        && (item.payload.receiptRef != nil || item.payload.patchRef != nil)
        && (item.runId != nil || item.payload.integrationRunId != nil)
}

func isIntegrationApplyCompletedItem(_ item: ConversationTimelineItem) -> Bool {
    item.kind == .worker
        && item.payload.eventKind == ExecutionEventKind.workItemCompleted.rawValue
        && item.payload.mainWorkspaceModified == true
        && item.payload.verifierPassed == true
        && (
            item.payload.reasonCode == "candidate_applied_to_main_workspace"
                || item.payload.reasonCode == "candidate_already_applied_to_main_workspace"
        )
        && (item.runId != nil || item.payload.integrationRunId != nil)
}

func latestErrorTextByRunID(in timeline: [ConversationTimelineItem]) -> [String: String] {
    var latest: [String: (sequence: Int64, text: String)] = [:]
    for item in timeline where item.kind == .error {
        guard let runID = item.runId,
              let text = item.payload.contentRedacted?.trimmingCharacters(in: .whitespacesAndNewlines),
              !text.isEmpty
        else { continue }
        if let existing = latest[runID], existing.sequence > item.sequence {
            continue
        }
        latest[runID] = (item.sequence, normalizedRunFailureText(text))
    }
    return latest.mapValues(\.text)
}

func runFailureSummary(messageContent: String, evidence: [ConversationTimelineItem]) -> String {
    for item in evidence.reversed() where item.kind == .error {
        if let text = runFailureSignalText(item), isVisibleRunFailureText(text) {
            return normalizedRunFailureText(text)
        }
    }
    for item in evidence.reversed() {
        if let text = runFailureSignalText(item), isVisibleRunFailureText(text) {
            return normalizedRunFailureText(text)
        }
    }
    if isVisibleRunFailureText(messageContent) {
        return normalizedRunFailureText(messageContent)
    }
    return "The run failed before a detailed assistant response was available."
}

func runFailureSignalText(_ item: ConversationTimelineItem) -> String? {
    item.payload.message
        ?? item.payload.contentRedacted
        ?? item.payload.assistantText
        ?? item.payload.assistantDelta
        ?? item.payload.reasonCode
        ?? item.payload.code
}

func isVisibleRunFailureText(_ text: String) -> Bool {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    return !trimmed.isEmpty && trimmed != "..." && trimmed.lowercased() != "run failed."
}

func normalizedRunFailureText(_ text: String) -> String {
    if isStepBudgetExhaustionText(text) {
        return "The agent exhausted its step budget before producing a final answer."
    }
    return text
}

func isMainWorkspacePatchChangeItem(_ item: ConversationTimelineItem) -> Bool {
    guard item.kind == .patch else { return false }
    if item.payload.mainWorkspaceModified == false {
        return false
    }
    if item.payload.mainWorkspaceModified == true {
        return true
    }
    if item.payload.integrationFinalDiffRef != nil || item.payload.finalDiffRef != nil {
        return true
    }
    return false
}

struct CodeChangeSummaryCardModel: Identifiable, Equatable {
    let id: String
    let title: String
    let body: String
    let files: [String]
    let patchSummary: String?
    let diffEvidence: String?
    let receiptRef: String?
    let changedAtDate: Date
    let runID: String?
    let approvalRequired: Bool
    let mainWorkspaceModified: Bool
    let pendingIntegrationCandidate: Bool

    var canApplyIntegration: Bool {
        pendingIntegrationCandidate && approvalRequired && !mainWorkspaceModified && runID != nil
    }

    init?(item: ConversationTimelineItem) {
        let pendingIntegrationCandidate = isIntegrationCandidateReadyItem(item)
        let completedIntegrationApply = isIntegrationApplyCompletedItem(item)
        guard pendingIntegrationCandidate || completedIntegrationApply || isMainWorkspacePatchChangeItem(item) else { return nil }
        id = item.id
        runID = item.payload.integrationRunId ?? item.runId
        approvalRequired = !completedIntegrationApply && item.payload.approvalRequired == true
        mainWorkspaceModified = completedIntegrationApply || item.payload.mainWorkspaceModified == true
        self.pendingIntegrationCandidate = pendingIntegrationCandidate
        files = Self.uniquePaths(from: [
            item.payload.appliedFiles,
            item.payload.touchedPaths,
            item.payload.targetPaths,
            item.payload.paths,
            item.payload.path.map { [$0] }
        ])
        let countLabel = files.isEmpty
            ? "code changes"
            : "\(files.count) \(files.count == 1 ? "file" : "files")"
        if pendingIntegrationCandidate {
            title = "Code changes ready"
        } else if completedIntegrationApply {
            title = "Applied \(countLabel)"
        } else {
            title = "Edited \(countLabel)"
        }
        if pendingIntegrationCandidate {
            body = item.payload.message
                ?? "Prepared isolated integration candidate. Apply to write these changes into the project."
        } else if completedIntegrationApply {
            body = item.payload.message
                ?? item.payload.contentRedacted
                ?? "Main workspace changes were applied."
        } else {
            body = item.payload.contentRedacted
                ?? item.payload.message
                ?? "Main workspace changes were applied."
        }
        if let patchCount = item.payload.patchCount,
           let applyCount = item.payload.applyResultCount {
            patchSummary = "\(patchCount) patches · \(applyCount) results"
        } else if let patchCount = item.payload.patchCount {
            patchSummary = "\(patchCount) patches"
        } else {
            patchSummary = nil
        }
        let diffHash = item.payload.integrationFinalDiffHash
            ?? item.payload.reviewedStagedDiffHash
            ?? item.payload.stagedDiffHash
        let diffRef = item.payload.finalDiffRef
            ?? item.payload.integrationFinalDiffRef
            ?? item.payload.patchRef
            ?? item.payload.reviewedStagedDiffRef
            ?? item.payload.stagedDiffRef
        if let diffHash, let diffRef {
            diffEvidence = "\(Self.shortEvidence(diffHash)) · \(diffRef)"
        } else {
            diffEvidence = diffHash.map(Self.shortEvidence) ?? diffRef
        }
        receiptRef = item.payload.receiptRef ?? item.payload.repairRef
        changedAtDate = item.createdAtDate
    }

    static func uniquePaths(from groups: [[String]?]) -> [String] {
        var seen = Set<String>()
        var paths: [String] = []
        for group in groups {
            for path in group ?? [] {
                let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmed.isEmpty, !seen.contains(trimmed) else { continue }
                seen.insert(trimmed)
                paths.append(trimmed)
            }
        }
        return paths
    }

    private static func shortEvidence(_ value: String) -> String {
        if let suffix = value.split(separator: ":").last, !suffix.isEmpty {
            return String(suffix.prefix(12))
        }
        return String(value.prefix(12))
    }
}

struct CodeChangeSummaryCard: View {
    let card: CodeChangeSummaryCardModel
    var isApplying = false
    var onApplyIntegration: ((CodeChangeSummaryCardModel) -> Void)? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "square.and.pencil")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(GeneratedDesignTokens.colorStatusSuccess)
                Text(card.title)
                    .font(Theme.ui(12.5, .semibold))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer(minLength: 0)
                Text(RelativeTime.string(from: card.changedAtDate))
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
                    .lineLimit(1)
            }
            Text(card.body)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.textSoft)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            if !card.files.isEmpty {
                CodeChangeFilesSection(files: card.files)
            }
            if card.patchSummary != nil || card.diffEvidence != nil || card.receiptRef != nil {
                VStack(alignment: .leading, spacing: Theme.s4) {
                    detailRow("Patch", card.patchSummary)
                    detailRow("Diff", card.diffEvidence)
                    detailRow("Receipt", card.receiptRef)
                }
            }
            if card.canApplyIntegration {
                CodeChangeApplySection(
                    card: card,
                    isApplying: isApplying,
                    onApplyIntegration: onApplyIntegration
                )
            }
        }
        .padding(Theme.s12)
        .frame(maxWidth: 720, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .strokeBorder(GeneratedDesignTokens.colorStatusSuccess.opacity(0.35), lineWidth: 1)
        )
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.codeChangeCard.\(card.id)")
        .accessibilityLabel("\(card.title): \(card.files.joined(separator: ", "))")
    }

    @ViewBuilder
    private func detailRow(_ label: String, _ value: String?) -> some View {
        if let value, !value.isEmpty {
            HStack(alignment: .firstTextBaseline, spacing: Theme.s6) {
                Text(label)
                    .font(Theme.mono(9.5, .semibold))
                    .foregroundStyle(Theme.faint)
                    .frame(width: 54, alignment: .leading)
                Text(value)
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.textSoft)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
        }
    }
}

private struct CodeChangeFilesSection: View {
    let files: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s4) {
            ForEach(files.prefix(6), id: \.self) { path in
                CodeChangeFileRow(path: path)
            }
            if files.count > 6 {
                Text("+\(files.count - 6) more")
                    .font(Theme.ui(10.5, .semibold))
                    .foregroundStyle(Theme.faint)
            }
        }
        .padding(Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.bg.opacity(0.45))
        )
    }
}

private struct CodeChangeFileRow: View {
    let path: String

    var body: some View {
        HStack(spacing: Theme.s6) {
            Image(systemName: "doc.text")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.muted)
                .frame(width: 14)
            Text(path)
                .font(Theme.mono(10.5))
                .foregroundStyle(Theme.textSoft)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
        }
    }
}

private struct CodeChangeApplySection: View {
    let card: CodeChangeSummaryCardModel
    let isApplying: Bool
    let onApplyIntegration: ((CodeChangeSummaryCardModel) -> Void)?

    var body: some View {
        HStack(spacing: Theme.s8) {
            Button {
                onApplyIntegration?(card)
            } label: {
                if isApplying {
                    ProgressView()
                        .controlSize(.small)
                    Text("Applying")
                } else {
                    Label("Apply", systemImage: "checkmark.circle")
                }
            }
            .buttonStyle(.primaryAction)
            .disabled(isApplying || onApplyIntegration == nil)
            .accessibilityIdentifier("conversation.codeChangeCard.apply.\(card.id)")

            Text("Verified worktree candidate")
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.faint)
                .lineLimit(2)
        }
        .padding(.top, Theme.s4)
    }
}

struct WorkerRailView: View {
    let items: [ConversationTimelineItem]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "briefcase")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text("Workers")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer(minLength: 0)
                Text("\(items.count)")
                    .font(Theme.mono(11, .semibold))
                    .foregroundStyle(Theme.faint)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)

            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.s8) {
                    ForEach(items.suffix(24)) { item in
                        WorkerRailItemRow(item: item)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.bottom, 14)
            }
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(Theme.panelDeep)
        .accessibilityIdentifier("conversation.workerRail")
    }
}

private struct WorkerRailItemRow: View {
    let item: ConversationTimelineItem
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: Theme.s6) {
                if let summary = summaryText {
                    Text(summary)
                        .font(Theme.ui(11))
                        .foregroundStyle(Theme.textSoft)
                        .lineLimit(expanded ? nil : 3)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                ForEach(detailRows) { row in
                    HStack(alignment: .firstTextBaseline, spacing: Theme.s6) {
                        Text(row.label)
                            .font(Theme.mono(9, .semibold))
                            .foregroundStyle(Theme.faint)
                            .frame(width: 58, alignment: .leading)
                        Text(row.value)
                            .font(Theme.ui(10))
                            .foregroundStyle(Theme.textSoft)
                            .lineLimit(2)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                }
            }
            .padding(.top, Theme.s6)
        } label: {
            HStack(alignment: .top, spacing: Theme.s8) {
                statusIndicator
                VStack(alignment: .leading, spacing: 2) {
                    Text(workerTitle)
                        .font(Theme.ui(11.5, .semibold))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(Theme.mono(9.5))
                        .foregroundStyle(Theme.faint)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
                Text(RelativeTime.string(from: item.createdAtDate))
                    .font(Theme.ui(9.5))
                    .foregroundStyle(Theme.faint)
                    .lineLimit(1)
            }
        }
        .disclosureGroupStyle(.automatic)
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Worker \(workerTitle): \(summaryText ?? item.state)")
    }

    private var workerTitle: String {
        let raw = item.payload.roleLabel
            ?? item.payload.workerId
            ?? item.payload.workItemId
            ?? item.kind.displayLabel
        return raw
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
            .capitalized
    }

    private var subtitle: String {
        [
            item.payload.workItemId,
            item.payload.tool,
            item.state
        ]
        .compactMap { value in
            let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return trimmed.isEmpty ? nil : trimmed
        }
        .prefix(2)
        .joined(separator: " · ")
    }

    private var summaryText: String? {
        let text = item.payload.assistantText
            ?? item.payload.assistantDelta
            ?? item.payload.contentRedacted
            ?? item.payload.message
            ?? item.payload.commandRedacted
        return text.map(normalizedRunFailureText)
    }

    private var statusColor: Color {
        let state = item.state.lowercased()
        if state.contains("fail") || state.contains("error") || state.contains("cancel") {
            return Theme.coral
        }
        if state.contains("complete") || item.payload.workerOk == true || item.payload.verifierPassed == true {
            return Theme.accent
        }
        return Theme.blue
    }

    @ViewBuilder
    private var statusIndicator: some View {
        if isActiveWorkerItem(item) {
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.62)
                .frame(width: 12, height: 12)
                .padding(.top, 3)
                .accessibilityHidden(true)
        } else {
            Circle()
                .fill(statusColor)
                .frame(width: 7, height: 7)
                .padding(.top, 6)
        }
    }

    private var detailRows: [TimelineDetailRow] {
        var rows: [TimelineDetailRow] = []
        append("Worker", item.payload.workerId, to: &rows)
        append("Work", item.payload.workItemId, to: &rows)
        append("Tool", item.payload.tool, to: &rows)
        append("Model", item.payload.modelId, to: &rows)
        append("Batch", batchSummary, to: &rows)
        append("Files", pathSummary(item.payload.appliedFiles ?? item.payload.targetPaths ?? item.payload.touchedPaths), to: &rows)
        append("Receipt", item.payload.receiptRef ?? item.payload.verificationRef ?? item.payload.repairRef, to: &rows)
        append("Reason", item.payload.reasonCode ?? item.payload.code, to: &rows)
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
        let visible = values.prefix(2).joined(separator: ", ")
        if values.count > 2 {
            return "\(visible) +\(values.count - 2)"
        }
        return visible
    }

    private func append(_ label: String, _ value: String?, to rows: inout [TimelineDetailRow]) {
        guard let value, !value.isEmpty else { return }
        rows.append(TimelineDetailRow(label: label, value: value))
    }
}

// MARK: - Assistant timeline event cell

func latestAssistantReplyMessage(
    in timeline: [ConversationTimelineItem],
    durableMessages: [ConversationMessage] = []
) -> ConversationMessage? {
    let durableCandidates = durableMessages.filter { message in
        message.role == .assistant && isVisibleAssistantReply(message.contentRedacted)
    }
    let timelineCandidates = timeline.compactMap { item -> ConversationMessage? in
        if isAssistantExecutionEvent(item) {
            return nil
        }
        if let message = item.message,
           message.role == .assistant,
           isVisibleAssistantReply(message.contentRedacted) {
            return message
        }
        guard item.kind == .assistantMessage else { return nil }
        guard let text = item.payload.assistantText
            ?? item.payload.contentRedacted
            ?? item.payload.assistantDelta,
            isVisibleAssistantReply(text)
        else { return nil }
        let state: MessageState = item.state == "streaming" ? .streaming : .complete
        return ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: item.payload.assistantMessageId ?? item.payload.messageId ?? item.id,
            projectId: item.projectId,
            conversationId: item.conversationId,
            turnId: item.turnId,
            role: .assistant,
            state: state,
            contentRedacted: text,
            sequence: item.sequence,
            createdAtMs: item.createdAtMs,
            updatedAtMs: item.updatedAtMs
        )
    }
    return (durableCandidates + timelineCandidates).max { lhs, rhs in
        let left = (lhs.updatedAtMs, lhs.sequence, lhs.id)
        let right = (rhs.updatedAtMs, rhs.sequence, rhs.id)
        return left < right
    }
}

func shouldRenderPinnedAssistantReply(
    _ assistantReply: ConversationMessage,
    in timeline: [ConversationTimelineItem]
) -> Bool {
    if timeline.contains(where: { item in
        if let message = item.message,
           message.id == assistantReply.id,
           message.role == .assistant,
           isVisibleAssistantReply(message.contentRedacted) {
            return true
        }
        return item.kind == .assistantMessage
            && item.payload.messageId == assistantReply.id
            && item.payload.role == .assistant
            && isVisibleAssistantReply(item.payload.contentRedacted ?? "")
    }) {
        return false
    }
    guard let last = timeline.last else { return false }
    if let message = last.message,
       message.id == assistantReply.id,
       message.role == .assistant,
       isVisibleAssistantReply(message.contentRedacted) {
        return false
    }
    if last.kind == .assistantMessage,
       (last.payload.assistantMessageId == assistantReply.id || last.payload.messageId == assistantReply.id),
       isVisibleAssistantReply(last.payload.assistantText ?? last.payload.contentRedacted ?? last.payload.assistantDelta ?? "") {
        return false
    }
    return true
}

private func isVisibleAssistantReply(_ text: String) -> Bool {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    return !trimmed.isEmpty && trimmed != "..."
}

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
                if isLiveMessage {
                    ProgressView()
                        .controlSize(.small)
                        .scaleEffect(0.62)
                        .frame(width: 12, height: 12)
                        .accessibilityHidden(true)
                }
                Spacer(minLength: 0)
                Text(RelativeTime.string(from: message.createdAtDate))
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }
            Text(messageCellDisplayText(message))
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
        .accessibilityLabel("\(roleLabel): \(messageCellDisplayText(message))")
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

    private var isLiveMessage: Bool {
        [.queued, .pending, .streaming].contains(message.state)
    }
}

func messageCellDisplayText(_ message: ConversationMessage) -> String {
    let trimmed = message.contentRedacted.trimmingCharacters(in: .whitespacesAndNewlines)
    if message.role == .assistant, isStepBudgetExhaustionText(trimmed) {
        return "The agent exhausted its step budget before producing a final answer."
    }
    if message.role == .assistant,
       trimmed == "...",
       [.queued, .pending, .streaming].contains(message.state) {
        return "Thinking..."
    }
    return message.contentRedacted
}

func isStepBudgetExhaustionText(_ text: String) -> Bool {
    let lowercased = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    return lowercased.contains("step budget")
        && lowercased.contains("final answer")
        && (lowercased.contains("stopped after") || lowercased.contains("exhausted"))
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
