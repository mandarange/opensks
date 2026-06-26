// ConversationStore.swift — the @MainActor view model for the conversation
// sidebar + thread. It owns the summaries list, the selected conversation, the
// loaded message page for that selection, per-conversation drafts, and the
// filter / search text. All persistence goes through an injected
// `ConversationService`. PR-025 deliberately has NO run/send functionality —
// engine-driven turns arrive in PR-027.

import Foundation
import SwiftUI

@MainActor
final class ConversationStore: ObservableObject, BackgroundReleasable {
    // Service boundary. Swappable so the live service can be (re)bound once the
    // workspace path is known at runtime (RootView.onAppear).
    @Published private(set) var service: ConversationService

    // List + selection.
    @Published var summaries: [ConversationSummary] = []
    @Published var selectedConversationID: String?
    @Published var projectId: String?

    // Loaded message page for the current selection. This is the HEAVY
    // materialized view (PR-043): only the FOREGROUND conversation retains its
    // loaded page; backgrounding releases it so an inactive conversation does not
    // retain a full thread in memory. The light `summaries` entry survives.
    @Published private(set) var messages: [ConversationMessage] = []
    @Published private(set) var hasMoreMessages = false
    @Published private(set) var timelineByConversation: [String: [ConversationTimelineItem]] = [:]

    // Per-conversation composer drafts. Cleared on a successful send.
    @Published var drafts: [String: String] = [:]

    // Per-conversation runs linked to that conversation's turns (PR-027). A
    // RunCard renders one of these under the assistant turn it belongs to.
    @Published private(set) var runsByConversation: [String: [ConversationRunRef]] = [:]

    // Durable per-conversation Chat settings. The daemon snapshots these at turn
    // accept time; the composer edits this SSOT instead of sending ad-hoc
    // settings that product runtime would ignore.
    @Published private(set) var threadSettingsByConversation: [String: ConversationThreadSettings] = [:]

    // Editor context refs staged into a conversation draft. These are refreshed
    // against live editor text so stale refs are visible before send; the daemon
    // receives only path/range/hash refs and resolves bytes itself.
    @Published private(set) var contextAttachmentsByConversation: [String: [ConversationContextAttachment]] = [:]

    // Compatibility mirrors for Git receipts. The rendered Chat source of truth
    // is `timelineByConversation`; these arrays preserve older tests/call sites
    // while receipt posting migrates through durable timeline append.
    @Published private(set) var commitCardsByConversation: [String: [GitCommitCard]] = [:]

    // Per-conversation push cards (PR-036). After a SUCCESSFUL approved push the
    // Git studio posts one of these; the thread renders a `PushReceiptCard` with
    // the pushed remote oid. Commit and push are SEPARATE receipts: a commit card
    // can stand while a push card is absent (push pending or failed). Like commit
    // cards, these are thread-attached UI affordances, never persisted messages.
    @Published private(set) var pushCardsByConversation: [String: [GitPushCard]] = [:]

    // Per-conversation operation state. A single conversation still admits one
    // send/settings save at a time, but unrelated conversations can progress
    // independently.
    @Published private(set) var sendingConversationIDs: Set<String> = []
    @Published private(set) var savingThreadSettingsConversationIDs: Set<String> = []

    // Filter / search.
    @Published var filter: ConversationFilter = .all
    @Published var searchText: String = ""

    // Status surfacing.
    @Published private(set) var isLoading = false
    @Published private(set) var isLoadingMessages = false
    @Published var errorMessage: String?

    /// Page size for the message pager.
    let messagePageSize: Int

    /// Manual diagnostic queue drains use a hard cap so a bad supervisor state
    /// cannot pin the main actor.
    private let supervisorDrainMaxTicks = 8
    private let runEventSubscriptionTailMs: UInt64 = 30_000
    private let runEventSubscriptionPollMs: UInt64 = 100
    private var isDrainingSupervisor = false
    private var runEventSubscriptions: [String: Task<Void, Never>] = [:]
    private var runEventCursors: [String: UInt64] = [:]

    /// The conversation whose HEAVY message page is retained. Only this one holds
    /// a full thread page; backgrounding (`releaseBackgroundViews` / memory
    /// pressure) drops the page for any non-active selection. Re-activating
    /// (`setActive` / `select`) reloads it from the service.
    private(set) var activeConversationID: String?

    init(service: ConversationService, messagePageSize: Int = 50) {
        self.service = service
        self.messagePageSize = messagePageSize
    }

    /// Subscribe this store to a memory-pressure monitor: any event releases the
    /// heavy page for every non-active conversation. Idempotent across calls.
    func registerForMemoryPressure(_ monitor: MemoryPressureMonitor) {
        monitor.addHandler { [weak self] _ in
            self?.releaseBackgroundViews()
        }
    }

    /// Rebind the service (e.g. when the live workspace path becomes known).
    func updateService(_ service: ConversationService) {
        self.service = service
    }

    // MARK: - Derived

    /// Summaries after the in-memory search filter (the CLI already applied the
    /// status/pinned/archived filter).
    var visibleSummaries: [ConversationSummary] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return summaries }
        return summaries.filter { $0.title.lowercased().contains(query) }
    }

    var selectedSummary: ConversationSummary? {
        guard let id = selectedConversationID else { return nil }
        return summaries.first { $0.id == id }
    }

    func draft(for id: String) -> String { drafts[id] ?? "" }

    func setDraft(_ text: String, for id: String) { drafts[id] = text }

    var isSending: Bool {
        if let id = selectedConversationID {
            return isSending(conversationID: id)
        }
        return !sendingConversationIDs.isEmpty
    }

    var isSavingThreadSettings: Bool {
        if let id = selectedConversationID {
            return isSavingThreadSettings(for: id)
        }
        return !savingThreadSettingsConversationIDs.isEmpty
    }

    func isSending(conversationID id: String) -> Bool {
        sendingConversationIDs.contains(id)
    }

    func isSavingThreadSettings(for id: String) -> Bool {
        savingThreadSettingsConversationIDs.contains(id)
    }

    /// Runs linked to a conversation (most recent last), for the run card.
    func runs(for id: String) -> [ConversationRunRef] { runsByConversation[id] ?? [] }

    /// Durable timeline items for a conversation. This is the preferred Chat
    /// read model; `messages` remain loaded for pagination and compatibility.
    func timelineItems(for id: String) -> [ConversationTimelineItem] {
        timelineByConversation[id] ?? []
    }

    /// Durable Chat settings for a conversation, falling back to the typed safe
    /// defaults until the service has loaded or persisted a row.
    func threadSettings(for id: String) -> ConversationThreadSettings {
        threadSettingsByConversation[id] ?? ConversationThreadSettings.defaultFor(conversationID: id)
    }

    private func cacheThreadSettings(_ settings: ConversationThreadSettings, for id: String) {
        var next = threadSettingsByConversation
        next[id] = settings
        threadSettingsByConversation = next
    }

    private func removeCachedThreadSettings(for id: String) {
        var next = threadSettingsByConversation
        next.removeValue(forKey: id)
        threadSettingsByConversation = next
    }

    private func markSavingThreadSettings(for id: String) {
        var next = savingThreadSettingsConversationIDs
        next.insert(id)
        savingThreadSettingsConversationIDs = next
    }

    private func clearSavingThreadSettings(for id: String) {
        var next = savingThreadSettingsConversationIDs
        next.remove(id)
        savingThreadSettingsConversationIDs = next
    }

    func contextAttachments(for id: String) -> [ConversationContextAttachment] {
        contextAttachmentsByConversation[id] ?? []
    }

    func attachEditorContext(_ ref: EditorContextRef, to conversationID: String, currentText: String? = nil) {
        let currentHash = currentText.flatMap { ref.currentHash(in: $0) }
        let attachment = ConversationContextAttachment(
            ref: ref,
            currentHash: currentHash,
            isStale: currentText.map { ref.isStale(against: $0) } ?? false,
            checkedAtMs: nowMs()
        )
        var attachments = contextAttachmentsByConversation[conversationID] ?? []
        attachments.removeAll {
            $0.ref.workspaceRelativePath == ref.workspaceRelativePath
                && $0.ref.lineRange == ref.lineRange
        }
        attachments.append(attachment)
        contextAttachmentsByConversation[conversationID] = attachments
    }

    func removeContextAttachment(_ attachmentID: UUID, from conversationID: String) {
        var attachments = contextAttachmentsByConversation[conversationID] ?? []
        attachments.removeAll { $0.id == attachmentID }
        contextAttachmentsByConversation[conversationID] = attachments
    }

    func refreshEditorContexts(workspaceRelativePath: String, fullText: String) {
        guard !contextAttachmentsByConversation.isEmpty else { return }
        var refreshed = contextAttachmentsByConversation
        var changed = false
        for conversationID in refreshed.keys {
            guard var attachments = refreshed[conversationID] else { continue }
            var touched = false
            for index in attachments.indices where attachments[index].ref.workspaceRelativePath == workspaceRelativePath {
                let ref = attachments[index].ref
                let currentHash = ref.currentHash(in: fullText)
                let updated = ConversationContextAttachment(
                    ref: ref,
                    currentHash: currentHash,
                    isStale: currentHash != ref.contentHash,
                    checkedAtMs: nowMs()
                )
                if attachments[index] != updated {
                    attachments[index] = updated
                    touched = true
                }
            }
            if touched {
                refreshed[conversationID] = attachments
                changed = true
            }
        }
        if changed {
            contextAttachmentsByConversation = refreshed
        }
    }

    /// Commit cards posted into a conversation (most recent last).
    func commitCards(for id: String) -> [GitCommitCard] { commitCardsByConversation[id] ?? [] }

    /// Push cards posted into a conversation (most recent last).
    func pushCards(for id: String) -> [GitPushCard] { pushCardsByConversation[id] ?? [] }

    /// Post a LOCAL commit receipt into the active conversation timeline.
    /// Returns the compatibility card while the service persists the same
    /// receipt as a durable `commit_receipt` timeline item.
    @discardableResult
    func postCommitCard(_ result: GitCommitResult, message: String, conversationID: String? = nil) -> GitCommitCard? {
        guard let id = conversationID ?? selectedConversationID else { return nil }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let card = GitCommitCard(
            id: UUID().uuidString,
            commit: result.commit,
            paths: result.paths,
            message: message,
            committedAtMs: now
        )
        commitCardsByConversation[id, default: []].append(card)
        let payload = ConversationTimelinePayload(
            messageId: nil,
            role: nil,
            messageState: nil,
            contentRedacted: "Commit \(card.shortSha) recorded.",
            runRelation: nil,
            commit: result.commit,
            paths: result.paths,
            message: message,
            remote: nil,
            ref: nil,
            remoteOid: nil,
            localOid: nil,
            alreadyDone: nil,
            sourceSchema: result.schema,
            projection: "git_receipt",
            committed: result.committed,
            pushed: nil,
            intentId: nil,
            effectDigest: nil,
            idempotencyKey: nil,
            remoteUrlRedacted: nil,
            remoteExpectedOid: nil,
            protected: nil,
            approvalId: nil,
            approvalMatched: nil,
            stagedDiffHash: nil,
            stagedDiffRef: nil,
            reviewedStagedDiffHash: result.reviewedStagedDiffHash,
            reviewedStagedDiffRef: result.reviewedStagedDiffRef,
            integrationFinalDiffHash: result.integrationFinalDiffHash,
            integrationFinalDiffRef: result.integrationFinalDiffRef,
            integrationRunId: result.integrationRunId,
            integrationCandidateId: result.integrationCandidateId
        )
        postReceiptTimelineItem(
            conversationID: id,
            kind: .commitReceipt,
            state: "committed",
            eventKind: .gitCommitReceipt,
            idempotencyKey: "git-commit:\(result.commit)",
            payload: payload,
            nowMs: now
        )
        return card
    }

    /// Post an approved push receipt into the active conversation timeline.
    @discardableResult
    func postPushCard(
        _ receipt: GitPushReceipt,
        intent: GitPushIntent,
        approval: GitPushApproval? = nil,
        conversationID: String? = nil
    ) -> GitPushCard? {
        guard let id = conversationID ?? selectedConversationID else { return nil }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let card = GitPushCard(
            id: UUID().uuidString,
            remote: intent.remote,
            ref: intent.ref,
            remoteOid: receipt.remoteOid,
            localOid: intent.localOid,
            alreadyDone: receipt.alreadyDone,
            pushedAtMs: now
        )
        pushCardsByConversation[id, default: []].append(card)
        let payload = ConversationTimelinePayload(
            messageId: nil,
            role: nil,
            messageState: nil,
            contentRedacted: "Push \(card.shortRemoteOid) to \(intent.remote)/\(intent.ref) recorded.",
            runRelation: nil,
            commit: nil,
            paths: nil,
            message: nil,
            remote: intent.remote,
            ref: intent.ref,
            remoteOid: receipt.remoteOid,
            localOid: intent.localOid,
            alreadyDone: receipt.alreadyDone,
            sourceSchema: receipt.schema,
            projection: "git_receipt",
            committed: nil,
            pushed: receipt.pushed,
            intentId: intent.intentId,
            effectDigest: intent.effectDigest,
            idempotencyKey: receipt.idempotencyKey,
            remoteUrlRedacted: intent.remoteUrlRedacted,
            remoteExpectedOid: intent.remoteExpectedOid,
            protected: intent.protected,
            approvalId: approval?.approvalId,
            approvalMatched: approval?.matched,
            stagedDiffHash: nil,
            stagedDiffRef: nil,
            reviewedStagedDiffHash: nil,
            reviewedStagedDiffRef: nil,
            integrationFinalDiffHash: nil,
            integrationFinalDiffRef: nil,
            integrationRunId: nil,
            integrationCandidateId: nil
        )
        postReceiptTimelineItem(
            conversationID: id,
            kind: .pushReceipt,
            state: "pushed",
            eventKind: .gitPushReceipt,
            idempotencyKey: "git-push:\(receipt.idempotencyKey)",
            payload: payload,
            nowMs: now
        )
        return card
    }

    private func postReceiptTimelineItem(
        conversationID id: String,
        kind: ConversationTimelineItemKind,
        state: String,
        eventKind: ExecutionEventKind,
        idempotencyKey: String,
        payload: ConversationTimelinePayload,
        nowMs: Int64
    ) {
        guard let encoded = try? JSONEncoder.opensks.encode(payload) else {
            errorMessage = "could not encode \(kind.rawValue) timeline payload"
            return
        }
        let payloadJSON = String(decoding: encoded, as: UTF8.self)
        let local = ConversationTimelineItem(
            schema: "opensks.timeline-item.v1",
            id: "local-\(kind.rawValue)-\(UUID().uuidString)",
            projectId: projectId ?? selectedSummary?.projectId ?? "workspace",
            conversationId: id,
            turnId: nil,
            runId: nil,
            sequence: nextLocalTimelineSequence(for: id),
            kind: kind,
            state: state,
            payload: payload,
            createdAtMs: nowMs,
            updatedAtMs: nowMs
        )
        timelineByConversation[id, default: []].append(local)
        timelineByConversation[id]?.sort { lhs, rhs in
            if lhs.sequence != rhs.sequence { return lhs.sequence < rhs.sequence }
            return lhs.id < rhs.id
        }
        Task { @MainActor in
            do {
                _ = try await service.appendGitReceiptEvent(
                    conversationID: id,
                    kind: eventKind,
                    idempotencyKey: idempotencyKey,
                    payloadJSON: payloadJSON
                )
                await loadTimeline(for: id, limit: timelineItems(for: id).count)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    private func nextLocalTimelineSequence(for id: String) -> Int64 {
        let timelineMax = timelineByConversation[id]?.map(\.sequence).max() ?? 0
        let messageMax = selectedConversationID == id ? messages.map(\.sequence).max() ?? 0 : 0
        return max(timelineMax, messageMax) + 1
    }

    /// The run linked to a specific assistant message, if any — lets the thread
    /// render a `RunCard` directly under the turn that produced it.
    func run(forMessageID messageID: String) -> ConversationRunRef? {
        guard let id = selectedConversationID else { return nil }
        return runsByConversation[id]?.first { $0.messageId == messageID }
    }

    func run(forRunID runID: String) -> ConversationRunRef? {
        guard let id = selectedConversationID else { return nil }
        return runsByConversation[id]?.first { $0.runId == runID }
    }

    // MARK: - Loading

    func load() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            let list = try await service.list(filter: filter, limit: nil)
            summaries = list.conversations
            projectId = list.projectId
            // Keep selection valid; if it vanished, select the first.
            if let selected = selectedConversationID, !summaries.contains(where: { $0.id == selected }) {
                selectedConversationID = nil
                messages = []
                hasMoreMessages = false
                timelineByConversation[selected] = nil
            }
            if let selected = selectedConversationID {
                await loadThreadSettings(for: selected)
            } else if let first = summaries.first {
                await select(first.id)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Reapply the current filter against the service (used by the filter control).
    func applyFilter(_ filter: ConversationFilter) async {
        self.filter = filter
        await load()
    }

    // MARK: - Selection + messages

    func select(_ id: String) async {
        // Selecting a conversation makes it the FOREGROUND view: it gets its heavy
        // message page (re)loaded; the previously-selected conversation is now
        // backgrounded and its page released by the swap below (a single page is
        // ever held). `activeConversationID` tracks the retained heavy view.
        selectedConversationID = id
        activeConversationID = id
        await loadMessages(for: id)
        await loadRuns(for: id)
        await loadTimeline(for: id)
        await loadThreadSettings(for: id)
    }

    private func loadMessages(for id: String) async {
        isLoadingMessages = true
        defer { isLoadingMessages = false }
        do {
            let page = try await service.messages(id: id, beforeSequence: nil, limit: messagePageSize)
            // Only apply if the selection hasn't changed underneath us.
            guard selectedConversationID == id else { return }
            messages = page.messages
            hasMoreMessages = page.hasMore
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func loadOlderMessages() async {
        guard let id = selectedConversationID, hasMoreMessages,
              let oldest = messages.first else { return }
        isLoadingMessages = true
        defer { isLoadingMessages = false }
        do {
            let page = try await service.messages(
                id: id,
                beforeSequence: oldest.sequence,
                limit: messagePageSize
            )
            guard selectedConversationID == id else { return }
            // Prepend older page; de-dup defensively on id.
            let existing = Set(messages.map(\.id))
            let prepend = page.messages.filter { !existing.contains($0.id) }
            messages = prepend + messages
            hasMoreMessages = page.hasMore
            await loadTimeline(for: id, limit: messages.count)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: - Send (PR-027)

    /// Start ONE turn for `conversationID`: generate an idempotency key, call
    /// `turnStart` for the durable accepted handle, subscribe to its run event
    /// stream, then reconcile the accepted read models. The resident daemon owns
    /// execution; Swift observes and can explicitly request supervisor ticks via
    /// `drainSupervisorQueue`, but send itself is not the executor.
    func send(conversationID: String, text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !isSending(conversationID: conversationID) else { return }
        sendingConversationIDs.insert(conversationID)
        errorMessage = nil
        defer { sendingConversationIDs.remove(conversationID) }
        let key = UUID().uuidString
        guard let summary = summaries.first(where: { $0.id == conversationID }) else {
            errorMessage = "conversation not loaded: \(conversationID)"
            return
        }
        do {
            let threadSettings = threadSettings(for: conversationID)
            let context = turnContextSelection(for: conversationID)
            let turn = try await service.turnStart(
                conversationID: conversationID,
                projectID: summary.projectId,
                text: trimmed,
                settings: nil,
                threadSettingsUpdatedAtMs: threadSettings.updatedAtMs,
                context: context,
                idempotencyKey: key
            )
            startRunEventSubscription(
                runID: turn.runId,
                conversationID: conversationID,
                projectID: summary.projectId,
                turnID: turn.turnId
            )
            drafts[conversationID] = nil
            await refreshConversationReadModels(for: conversationID)
            if let streamFailureMessage = latestLiveStreamFailureMessage(for: conversationID) {
                errorMessage = streamFailureMessage
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func startRunEventSubscription(
        runID: String,
        conversationID: String,
        projectID: String,
        turnID: String?
    ) {
        guard runEventSubscriptions[runID] == nil else { return }
        let service = service
        let tailMs = runEventSubscriptionTailMs
        let pollIntervalMs = runEventSubscriptionPollMs
        let startingCursor = runEventCursors[runID] ?? 0
        runEventSubscriptions[runID] = Task { [weak self] in
            var cursor = startingCursor
            while !Task.isCancelled {
                do {
                    let stream = try await service.subscribeRunEvents(
                        runID: runID,
                        sinceSequence: cursor,
                        tailMs: tailMs,
                        pollIntervalMs: pollIntervalMs
                    )
                    let events = stream.executionEvents.sorted {
                        $0.sequence == $1.sequence ? $0.id < $1.id : $0.sequence < $1.sequence
                    }
                    if !events.isEmpty {
                        cursor = max(cursor, events.map(\.sequence).max() ?? cursor)
                        await MainActor.run { [weak self] in
                            guard let self else { return }
                            self.runEventCursors[runID] = max(self.runEventCursors[runID] ?? 0, cursor)
                            self.applyLiveExecutionEvents(
                                events,
                                conversationID: conversationID,
                                projectID: projectID,
                                turnID: turnID
                            )
                        }
                    }
                    if let failure = stream.streamFailures.first {
                        let reconnectSequence = recommendedReconnectSequence(from: failure.error)
                        if let reconnectSequence {
                            cursor = reconnectSequence
                        }
                        await MainActor.run { [weak self] in
                            guard let self else { return }
                            if let reconnectSequence {
                                self.runEventCursors[runID] = reconnectSequence
                            }
                            self.applyLiveStreamFailure(
                                failure,
                                conversationID: conversationID,
                                projectID: projectID,
                                turnID: turnID,
                                runID: runID
                            )
                            self.errorMessage = streamFailureMessage(failure)
                        }
                        break
                    }
                    if events.contains(where: liveExecutionEventTerminatesRun) {
                        break
                    }
                    if events.isEmpty {
                        try await Task.sleep(nanoseconds: 100_000_000)
                    }
                } catch is CancellationError {
                    break
                } catch {
                    await MainActor.run { [weak self] in
                        self?.errorMessage = "run event subscription failed: \(error.localizedDescription)"
                    }
                    break
                }
            }
            await MainActor.run { [weak self] in
                self?.runEventSubscriptions[runID] = nil
            }
        }
    }

    /// Explicitly request queued accepted-turn recovery. Each
    /// supervisor tick claims and executes at most one turn; looping until an
    /// empty tick keeps a relaunched chat from leaving durable accepted turns
    /// stuck in the sidebar as forever-running. This is a diagnostic/manual
    /// controller action, not part of the primary send/load observer path.
    @discardableResult
    func drainSupervisorQueue(maxTicks: Int = 8, refreshAfterDrain: Bool = true) async throws -> Int {
        guard maxTicks > 0, !isDrainingSupervisor else { return 0 }
        isDrainingSupervisor = true
        defer { isDrainingSupervisor = false }

        var claimedTurns = 0
        for _ in 0..<maxTicks {
            let tick = try await service.supervisorTick()
            guard tick.claimed != nil || tick.executed != nil else { break }
            claimedTurns += 1
            applySupervisorTickResult(tick)
        }

        if refreshAfterDrain {
            await refreshConversationReadModels()
        }
        return claimedTurns
    }

    private func applySupervisorTickResult(_ tick: TurnSupervisorTickResult) {
        guard let claimed = tick.claimed, let executed = tick.executed else { return }
        let conversationID = claimed.conversationId
        let now = nowMs()

        if var runs = runsByConversation[conversationID],
           let index = runs.firstIndex(where: { $0.runId == claimed.runId }) {
            let existing = runs[index]
            runs[index] = ConversationRunRef(
                turnId: existing.turnId,
                runId: existing.runId,
                messageId: existing.messageId,
                relation: existing.relation,
                runState: executed.runState
            )
            runsByConversation[conversationID] = runs
        }

        let assistantMessageID = executed.assistantMessageId ?? claimed.assistantMessageId
        if selectedConversationID == conversationID,
           let index = messages.firstIndex(where: { $0.id == assistantMessageID }) {
            let existing = messages[index]
            messages[index] = ConversationMessage(
                schema: existing.schema,
                id: existing.id,
                projectId: existing.projectId,
                conversationId: existing.conversationId,
                turnId: existing.turnId,
                role: existing.role,
                state: messageState(for: executed.runState),
                contentRedacted: existing.contentRedacted,
                sequence: existing.sequence,
                createdAtMs: existing.createdAtMs,
                updatedAtMs: now
            )
        }

        if let index = summaries.firstIndex(where: { $0.id == conversationID }) {
            let existing = summaries[index]
            summaries[index] = ConversationSummary(
                schema: existing.schema,
                id: existing.id,
                projectId: existing.projectId,
                title: existing.title,
                titleSource: existing.titleSource,
                status: conversationStatus(for: executed.runState),
                pinned: existing.pinned,
                archived: existing.archived,
                messageCount: existing.messageCount,
                createdAtMs: existing.createdAtMs,
                updatedAtMs: now,
                lastMessageAtMs: existing.lastMessageAtMs
            )
        }
    }

    private func messageState(for runState: RunState) -> MessageState {
        switch runState {
        case .queued, .running, .paused:
            return .streaming
        case .completed:
            return .complete
        case .failed, .cancelled:
            return .failed
        case .unknown:
            return .unknown
        }
    }

    private func conversationStatus(for runState: RunState) -> ConversationStatus {
        switch runState {
        case .queued, .running:
            return .running
        case .paused:
            return .paused
        case .completed:
            return .completed
        case .failed, .cancelled:
            return .failed
        case .unknown:
            return .unknown
        }
    }

    private func refreshConversationReadModels(for conversationID: String? = nil) async {
        await load()
        if let id = conversationID ?? selectedConversationID {
            await loadRuns(for: id)
            await loadThreadSettings(for: id)
            if activeConversationID == id {
                await loadMessages(for: id)
                await loadTimeline(for: id)
            }
        }
    }

    /// Refresh the run list for a conversation from the service.
    func loadRuns(for id: String) async {
        do {
            let runs = try await service.runs(conversationID: id)
            runsByConversation[id] = runs
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Apply execution events observed from `subscribe_events` immediately to
    /// the visible timeline. The durable repository projection is still reloaded
    /// afterward; this local path makes the Chat surface react to the live stream
    /// instead of waiting only on a final reload.
    func applyLiveExecutionEvents(
        _ events: [ExecutionEventEnvelope],
        conversationID: String,
        projectID: String,
        turnID: String?
    ) {
        guard !events.isEmpty else { return }
        var items = timelineByConversation[conversationID] ?? []
        for event in events.sorted(by: { $0.sequence == $1.sequence ? $0.id < $1.id : $0.sequence < $1.sequence }) {
            let id = "timeline-event-\(event.id)"
            let sequence: Int64
            if let existing = items.first(where: { $0.id == id }) {
                sequence = existing.sequence
            } else {
                sequence = (items.map(\.sequence).max() ?? 0) + 1
            }
            let item = ConversationTimelineItem(
                schema: "opensks.timeline-item.v1",
                id: id,
                projectId: projectID,
                conversationId: conversationID,
                turnId: turnID,
                runId: event.runId,
                sequence: sequence,
                kind: liveTimelineKind(for: event),
                state: liveTimelineState(for: event),
                payload: .liveExecutionEvent(event: event, contentRedacted: liveTimelineText(for: event)),
                createdAtMs: liveTimelineOccurredAtMs(event.occurredAt),
                updatedAtMs: liveTimelineOccurredAtMs(event.occurredAt)
            )
            if let index = items.firstIndex(where: { $0.id == id }) {
                items[index] = item
            } else {
                items.append(item)
            }
        }
        items.sort { lhs, rhs in
            lhs.sequence == rhs.sequence ? lhs.id < rhs.id : lhs.sequence < rhs.sequence
        }
        timelineByConversation[conversationID] = items
    }

    func applyLiveStreamFailure(
        _ failure: EngineStreamFailure,
        conversationID: String,
        projectID: String,
        turnID: String?,
        runID: String
    ) {
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let id = "timeline-event-stream-failure-\(runID)-\(failure.error.code)-\(failure.cursor)"
        let item = ConversationTimelineItem(
            schema: "opensks.timeline-item.v1",
            id: id,
            projectId: projectID,
            conversationId: conversationID,
            turnId: turnID,
            runId: runID,
            sequence: nextLocalTimelineSequence(for: conversationID),
            kind: .error,
            state: "stream_failed",
            payload: .liveStreamFailure(failure),
            createdAtMs: now,
            updatedAtMs: now
        )
        var items = timelineByConversation[conversationID] ?? []
        if let index = items.firstIndex(where: { $0.id == id }) {
            items[index] = item
        } else {
            items.append(item)
        }
        items.sort { lhs, rhs in
            lhs.sequence == rhs.sequence ? lhs.id < rhs.id : lhs.sequence < rhs.sequence
        }
        timelineByConversation[conversationID] = items
    }

    /// Refresh the durable conversation timeline projection. The CLI timeline
    /// currently pages only by latest N, so use the visible message count when
    /// older pages have already been loaded.
    func loadTimeline(for id: String, limit: Int? = nil) async {
        do {
            let visibleCount = selectedConversationID == id ? messages.count : 0
            let timeline = try await service.timeline(
                conversationID: id,
                limit: max(limit ?? messagePageSize, visibleCount, 1)
            )
            timelineByConversation[id] = mergedTimelineItems(
                durableItems: timeline.items,
                preservingLocalLiveEventsFor: id
            )
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func mergedTimelineItems(
        durableItems: [ConversationTimelineItem],
        preservingLocalLiveEventsFor id: String
    ) -> [ConversationTimelineItem] {
        let durableIDs = Set(durableItems.map(\.id))
        let pendingLiveItems = (timelineByConversation[id] ?? []).filter { item in
            item.id.hasPrefix("timeline-event-")
                && (item.payload.projection == "live_execution_event"
                    || item.payload.projection == "live_stream_failure")
                && !durableIDs.contains(item.id)
        }
        var merged = durableItems + pendingLiveItems
        merged.sort { lhs, rhs in
            lhs.sequence == rhs.sequence ? lhs.id < rhs.id : lhs.sequence < rhs.sequence
        }
        return merged
    }

    private func latestLiveStreamFailureMessage(for id: String) -> String? {
        timelineByConversation[id]?
            .last(where: { $0.payload.projection == "live_stream_failure" })?
            .payload
            .contentRedacted
    }

    /// Refresh durable thread settings for a conversation from the service.
    func loadThreadSettings(for id: String) async {
        do {
            let settings = try await service.threadSettings(conversationID: id)
            cacheThreadSettings(settings, for: id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Persist a settings edit through the repository-backed service. The saved
    /// row returned by the service is used as truth because the backend stamps
    /// `conversation_id` and `updated_at_ms`.
    func updateThreadSettings(
        for id: String,
        mutate: (inout ConversationThreadSettings) -> Void
    ) async {
        guard !isSavingThreadSettings(for: id) else { return }
        markSavingThreadSettings(for: id)
        errorMessage = nil
        defer { clearSavingThreadSettings(for: id) }
        let previous = threadSettingsByConversation[id]
        var next = threadSettings(for: id)
        mutate(&next)
        cacheThreadSettings(next, for: id)
        do {
            let saved = try await service.setThreadSettings(next, conversationID: id)
            cacheThreadSettings(saved, for: id)
        } catch {
            if let previous {
                cacheThreadSettings(previous, for: id)
            } else {
                removeCachedThreadSettings(for: id)
            }
            errorMessage = error.localizedDescription
        }
    }

    // MARK: - Mutations

    func create(title: String = "New conversation") async {
        errorMessage = nil
        do {
            let summary = try await service.create(title: title)
            await load()
            await select(summary.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func rename(_ id: String, to title: String) async {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        errorMessage = nil
        do {
            try await service.rename(id: id, title: trimmed)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func togglePinned(_ id: String) async {
        guard let summary = summaries.first(where: { $0.id == id }) else { return }
        errorMessage = nil
        do {
            try await service.setPinned(id: id, pinned: !summary.pinned)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func archive(_ id: String, archived: Bool = true) async {
        errorMessage = nil
        do {
            try await service.setArchived(id: id, archived: archived)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func fork(_ id: String, afterSequence: Int64? = nil) async {
        errorMessage = nil
        do {
            let fork = try await service.fork(id: id, afterSequence: afterSequence)
            await load()
            await select(fork.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func delete(_ id: String) async {
        errorMessage = nil
        do {
            try await service.delete(id: id)
            drafts[id] = nil
            runsByConversation[id] = nil
            removeCachedThreadSettings(for: id)
            contextAttachmentsByConversation[id] = nil
            timelineByConversation[id] = nil
            commitCardsByConversation[id] = nil
            pushCardsByConversation[id] = nil
            if selectedConversationID == id {
                selectedConversationID = nil
                activeConversationID = nil
                messages = []
                hasMoreMessages = false
            }
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: - Background release (PR-043)

    /// Make `id` the foreground conversation: hydrate its heavy message page and
    /// release any other retained page. Passing `nil` backgrounds everything (no
    /// heavy page retained). This is the explicit active/inactive transition the
    /// app drives on focus / route changes; `select` also routes through it.
    func setActive(_ id: String?) async {
        activeConversationID = id
        guard let id else {
            // Background everything: drop the heavy page, keep summaries.
            messages = []
            hasMoreMessages = false
            if let selected = selectedConversationID {
                timelineByConversation[selected] = nil
            }
            return
        }
        selectedConversationID = id
        await loadMessages(for: id)
        await loadRuns(for: id)
        await loadTimeline(for: id)
        await loadThreadSettings(for: id)
    }

    /// Release the heavy message page for any conversation that is NOT the active
    /// one, keeping the light `summaries` list intact. Idempotent. Driven by
    /// memory pressure or an app "background everything" transition. The active
    /// conversation keeps its page; everything else is reclaimed.
    func releaseBackgroundViews() {
        // A single page is ever held — for the selected conversation. If that
        // selection is not the active (foreground) conversation, drop the page.
        guard let selected = selectedConversationID else { return }
        if selected != activeConversationID {
            messages = []
            hasMoreMessages = false
            timelineByConversation[selected] = nil
        }
    }

    /// True if `id` currently retains its heavy message page (i.e. it is the
    /// active conversation and its page is loaded).
    func retainsHeavyView(_ id: String) -> Bool {
        id == activeConversationID
            && selectedConversationID == id
            && (!messages.isEmpty || !(timelineByConversation[id] ?? []).isEmpty)
    }

    private func turnContextSelection(for id: String) -> TurnContextSelection {
        let refs = contextAttachments(for: id).map(\.wireRef)
        return refs.isEmpty ? .empty : TurnContextSelection(refs: refs)
    }

    private func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1_000)
    }
}

private func liveTimelineKind(for event: ExecutionEventEnvelope) -> ConversationTimelineItemKind {
    if let agentKind = event.payload["agent_event_kind"]?.stringValue {
        switch agentKind {
        case "plan_updated":
            return .plan
        case "tool_call_started", "tool_call_output", "tool_call_completed":
            return .toolCall
        case "file_patch_proposed", "file_patch_applied":
            return .patch
        case "verification_started", "verification_completed":
            return .verification
        case "approval_requested", "approval_resolved":
            return .approval
        case "worker_spawned", "worker_progress", "worker_completed":
            return .worker
        case "image_artifact_created":
            return .imageArtifact
        case "warning":
            return .warning
        case "error":
            return .error
        case "assistant_text_delta", "assistant_text_completed":
            return .assistantMessage
        default:
            return .warning
        }
    }

    switch event.kind {
    case .approvalRequested, .approvalApproved, .approvalDenied:
        return .approval
    case .verificationPassed:
        return .verification
    case .verificationFailed:
        return .error
    case .gitCommitReceipt:
        return .commitReceipt
    case .gitPushReceipt, .gitPushFailed:
        return .pushReceipt
    case .imageArtifactCreated:
        return .imageArtifact
    case .unknown, .unrecognized:
        return .warning
    default:
        return .worker
    }
}

private func liveTimelineState(for event: ExecutionEventEnvelope) -> String {
    switch event.payload["agent_event_kind"]?.stringValue {
    case "assistant_text_delta":
        return "streaming"
    case "assistant_text_completed":
        return "completed"
    default:
        return event.kind.rawValue
    }
}

private func liveTimelineText(for event: ExecutionEventEnvelope) -> String {
    if event.sensitivity == .secret {
        return "Secret execution event redacted"
    }
    if let agentKind = event.payload["agent_event_kind"]?.stringValue,
       agentKind == "assistant_text_delta" || agentKind == "assistant_text_completed",
       let text = assistantEventTextValue(in: event.payload)?.value {
        return redactedTimelineSnippet(text)
    }
    let text = event.payload["content_redacted"]?.stringValue
        ?? event.payload["payload"]?["content_redacted"]?.stringValue
        ?? event.payload["payload"]?["message"]?.stringValue
        ?? event.payload["message"]?.stringValue
        ?? event.payload["agent_event_kind"]?.stringValue
        ?? event.kind.rawValue.replacingOccurrences(of: "_", with: " ")
    return redactedTimelineSnippet(text)
}

private func assistantEventTextValue(in payload: JSONValue) -> (target: String, value: String)? {
    for (source, target) in [
        ("assistant_delta", "assistant_delta"),
        ("text_delta", "assistant_delta"),
        ("delta", "assistant_delta"),
        ("content_delta", "assistant_delta"),
        ("assistant_text", "assistant_text"),
        ("text", "assistant_text"),
        ("content", "assistant_text")
    ] {
        if let value = timelinePayloadString(payload, source) {
            return (target, value)
        }
    }
    return nil
}

private func timelinePayloadString(_ payload: JSONValue, _ key: String) -> String? {
    payload[key]?.stringValue ?? payload["payload"]?[key]?.stringValue
}

private func redactedTimelineSnippet(_ value: String, maxCharacters: Int = 700) -> String {
    let redacted = redactTimelineSecrets(value)
    if redacted.count <= maxCharacters {
        return redacted
    }
    return String(redacted.prefix(maxCharacters)) + "..."
}

private func redactTimelineSecrets(_ value: String) -> String {
    var redacted = value
    for pattern in [
        #"(?i)Authorization\s*:\s*Bearer\s+\S+"#,
        #"(?i)(api[_-]?key|token|secret)\s*[:=]\s*\S+"#,
        #"sk-[A-Za-z0-9][A-Za-z0-9_-]{8,}"#
    ] {
        guard let expression = try? NSRegularExpression(pattern: pattern) else { continue }
        let range = NSRange(redacted.startIndex..<redacted.endIndex, in: redacted)
        redacted = expression.stringByReplacingMatches(
            in: redacted,
            range: range,
            withTemplate: "[REDACTED]"
        )
    }
    return redacted
}

private func liveExecutionEventTerminatesRun(_ event: ExecutionEventEnvelope) -> Bool {
    switch event.kind {
    case .snapshotWritten, .verificationFailed, .runCancelled:
        return true
    default:
        return false
    }
}

private func streamFailureMessage(_ failure: EngineStreamFailure) -> String {
    var message = "Run event stream failed: \(failure.error.message)"
    if failure.resumable {
        message += " Reconnect is available."
    }
    if let remediation = failure.error.remediation, !remediation.isEmpty {
        message += " \(remediation)."
    }
    return message
}

private func recommendedReconnectSequence(from error: PublicEngineError) -> UInt64? {
    guard error.code == "subscription_cursor_gap",
          let remediation = error.remediation
    else {
        return nil
    }
    return remediation
        .split(whereSeparator: { !$0.isNumber })
        .last
        .flatMap { UInt64($0) }
}

private func liveTimelineOccurredAtMs(_ occurredAt: String) -> Int64 {
    let pieces = occurredAt.split(separator: ".", maxSplits: 1)
    guard let seconds = pieces.first.flatMap({ Int64($0) }) else {
        return Int64(Date().timeIntervalSince1970 * 1_000)
    }
    let millis: Int64
    if pieces.count > 1 {
        let nanosText = pieces[1].prefix(9)
        let padded = nanosText + String(repeating: "0", count: max(0, 9 - nanosText.count))
        millis = (Int64(padded) ?? 0) / 1_000_000
    } else {
        millis = 0
    }
    return seconds.saturatingMultiply(1_000).saturatingAdd(millis)
}

private extension ConversationTimelinePayload {
    static func liveStreamFailure(_ failure: EngineStreamFailure) -> ConversationTimelinePayload {
        ConversationTimelinePayload(
            messageId: nil,
            role: nil,
            messageState: nil,
            contentRedacted: streamFailureMessage(failure),
            runRelation: nil,
            commit: nil,
            paths: nil,
            message: failure.error.message,
            remote: nil,
            ref: nil,
            remoteOid: nil,
            localOid: nil,
            alreadyDone: nil,
            sourceSchema: failure.error.schema,
            projection: "live_stream_failure",
            committed: nil,
            pushed: nil,
            intentId: nil,
            effectDigest: nil,
            idempotencyKey: nil,
            remoteUrlRedacted: nil,
            remoteExpectedOid: nil,
            protected: nil,
            approvalId: nil,
            approvalMatched: nil,
            stagedDiffHash: nil,
            stagedDiffRef: nil,
            reviewedStagedDiffHash: nil,
            reviewedStagedDiffRef: nil,
            integrationFinalDiffHash: nil,
            integrationFinalDiffRef: nil,
            integrationRunId: nil,
            integrationCandidateId: nil
        )
    }

    static func liveExecutionEvent(event: ExecutionEventEnvelope, contentRedacted: String) -> ConversationTimelinePayload {
        let payload = event.payload
        let nestedPayload = payload["payload"] ?? payload
        func string(_ key: String) -> String? {
            payload[key]?.stringValue ?? nestedPayload[key]?.stringValue
        }
        func int(_ key: String) -> Int? {
            payload[key]?.intValue ?? nestedPayload[key]?.intValue
        }
        func bool(_ key: String) -> Bool? {
            payload[key]?.boolValue ?? nestedPayload[key]?.boolValue
        }
        func strings(_ key: String) -> [String]? {
            payload[key]?.stringArrayValue ?? nestedPayload[key]?.stringArrayValue
        }
        let sourceSchema = string("source_schema") ?? event.schema
        let paths = strings("paths")
        let assistantTextValue = assistantEventTextValue(in: payload)
        return ConversationTimelinePayload(
            messageId: nil,
            role: nil,
            messageState: nil,
            contentRedacted: contentRedacted,
            runRelation: nil,
            commit: string("commit"),
            paths: paths,
            message: string("message"),
            remote: string("remote"),
            ref: string("ref"),
            remoteOid: string("remote_oid"),
            localOid: string("local_oid"),
            alreadyDone: bool("already_done"),
            sourceSchema: sourceSchema,
            projection: "live_execution_event",
            committed: bool("committed"),
            pushed: bool("pushed"),
            intentId: string("intent_id"),
            effectDigest: string("effect_digest"),
            idempotencyKey: string("idempotency_key"),
            remoteUrlRedacted: string("remote_url_redacted"),
            remoteExpectedOid: string("remote_expected_oid"),
            protected: bool("protected"),
            approvalId: string("approval_id"),
            approvalMatched: bool("approval_matched"),
            stagedDiffHash: string("staged_diff_hash"),
            stagedDiffRef: string("staged_diff_ref"),
            reviewedStagedDiffHash: string("reviewed_staged_diff_hash"),
            reviewedStagedDiffRef: string("reviewed_staged_diff_ref"),
            integrationFinalDiffHash: string("integration_final_diff_hash"),
            integrationFinalDiffRef: string("integration_final_diff_ref"),
            integrationRunId: string("integration_run_id"),
            integrationCandidateId: string("integration_candidate_id"),
            assetId: string("asset_id"),
            providerId: string("provider_id"),
            modelId: string("model_id"),
            path: string("path"),
            contentHash: string("content_hash"),
            provenanceHash: string("provenance_hash"),
            operation: string("operation"),
            width: int("width"),
            height: int("height"),
            eventId: event.id,
            eventKind: event.kind.rawValue,
            eventSequence: Int(event.sequence),
            actor: event.actor,
            agentEventKind: string("agent_event_kind"),
            assistantMessageId: string("assistant_message_id"),
            assistantText: assistantTextValue?.target == "assistant_text"
                ? redactedTimelineSnippet(assistantTextValue?.value ?? "")
                : string("assistant_text").map { redactedTimelineSnippet($0) },
            assistantDelta: assistantTextValue?.target == "assistant_delta"
                ? redactedTimelineSnippet(assistantTextValue?.value ?? "")
                : string("assistant_delta").map { redactedTimelineSnippet($0) },
            completionReason: string("completion_reason") ?? string("finish_reason"),
            workerId: string("worker_id"),
            workItemId: string("work_item_id"),
            leaseId: string("lease_id"),
            leaseHolder: string("lease_holder"),
            fencingToken: string("fencing_token"),
            fencingHolder: string("fencing_holder"),
            batchId: string("batch_id"),
            roleLabel: string("role_label") ?? string("role"),
            tool: string("tool"),
            commandRedacted: string("command_redacted"),
            exitCode: int("exit_code"),
            timedOut: bool("timed_out"),
            durationMs: int("duration_ms"),
            patchCount: int("patch_count"),
            applyResultCount: int("apply_result_count"),
            appliedFiles: strings("applied_files"),
            targetPaths: strings("target_paths"),
            touchedPaths: strings("touched_paths"),
            testTargets: strings("test_targets"),
            code: string("code"),
            reasonCode: string("reason_code"),
            receiptRef: string("receipt_ref"),
            patchRef: string("patch_ref"),
            verificationRef: string("verification_ref"),
            repairRef: string("repair_ref"),
            finalDiffRef: string("final_diff_ref"),
            contextPackRef: string("context_pack_ref"),
            workerContextPackRef: string("worker_context_pack_ref"),
            workerOk: bool("worker_ok"),
            mainWorkspaceModified: bool("main_workspace_modified"),
            approvalRequired: bool("approval_required"),
            verifierPassed: bool("verifier_passed"),
            modelCall: bool("model_call"),
            parallelBatch: bool("parallel_batch"),
            parallelBatchSize: int("parallel_batch_size"),
            parallelLaneIndex: int("parallel_lane_index"),
            responseHash: string("response_hash"),
            responseBytes: int("response_bytes")
        )
    }
}

private extension JSONValue {
    var boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    var intValue: Int? {
        if case .number(let value) = self { return Int(value) }
        return nil
    }

    var stringArrayValue: [String]? {
        if case .array(let values) = self {
            return values.compactMap(\.stringValue)
        }
        return nil
    }
}

private extension Int64 {
    func saturatingMultiply(_ rhs: Int64) -> Int64 {
        let (value, overflow) = multipliedReportingOverflow(by: rhs)
        return overflow ? Int64.max : value
    }

    func saturatingAdd(_ rhs: Int64) -> Int64 {
        let (value, overflow) = addingReportingOverflow(rhs)
        return overflow ? Int64.max : value
    }
}
