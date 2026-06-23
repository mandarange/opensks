// ConversationStore.swift — the @MainActor view model for the conversation
// sidebar + thread. It owns the summaries list, the selected conversation, the
// loaded message page for that selection, per-conversation drafts, and the
// filter / search text. All persistence goes through an injected
// `ConversationService`. PR-025 deliberately has NO run/send functionality —
// engine-driven turns arrive in PR-027.

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

    // True while a send is in flight for the selected conversation (the
    // composer disables its Send button so one Send starts exactly one run).
    @Published private(set) var isSending = false
    @Published private(set) var isSavingThreadSettings = false

    // Filter / search.
    @Published var filter: ConversationFilter = .all
    @Published var searchText: String = ""

    // Status surfacing.
    @Published private(set) var isLoading = false
    @Published private(set) var isLoadingMessages = false
    @Published var errorMessage: String?

    /// Page size for the message pager.
    let messagePageSize: Int

    /// The chat UI drains queued accepted turns opportunistically, but always
    /// with a hard cap so a bad supervisor state cannot pin the main actor.
    private let supervisorDrainMaxTicks = 8
    private var isDrainingSupervisor = false

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
            approvalMatched: nil
        )
        postReceiptTimelineItem(conversationID: id, kind: .commitReceipt, state: "committed", payload: payload, nowMs: now)
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
            approvalMatched: approval?.matched
        )
        postReceiptTimelineItem(conversationID: id, kind: .pushReceipt, state: "pushed", payload: payload, nowMs: now)
        return card
    }

    private func postReceiptTimelineItem(
        conversationID id: String,
        kind: ConversationTimelineItemKind,
        state: String,
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
                _ = try await service.appendTimelineItem(
                    conversationID: id,
                    kind: kind,
                    state: state,
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

    func load(recoverQueuedTurns: Bool = true) async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            let list = try await service.list(filter: filter, limit: nil)
            summaries = list.conversations
            projectId = list.projectId
            let shouldRecoverQueuedTurns = recoverQueuedTurns
                && !isDrainingSupervisor
                && summaries.contains { $0.status == .running }
            // Keep selection valid; if it vanished, select the first.
            if let selected = selectedConversationID, !summaries.contains(where: { $0.id == selected }) {
                selectedConversationID = nil
                messages = []
                hasMoreMessages = false
                timelineByConversation[selected] = nil
            }
            if selectedConversationID == nil, let first = summaries.first {
                await select(first.id)
            }
            if shouldRecoverQueuedTurns {
                _ = try await drainSupervisorQueue(maxTicks: supervisorDrainMaxTicks)
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
    /// `turnStart` for the durable accepted handle, then ask the daemon turn
    /// supervisor to execute one queued turn before reloading messages/runs. The
    /// draft for that conversation is cleared on a successful send. This is the
    /// ONLY primary send path — the legacy engine-run path is never invoked here.
    func send(conversationID: String, text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !isSending else { return }
        isSending = true
        errorMessage = nil
        defer { isSending = false }
        let key = UUID().uuidString
        guard let summary = summaries.first(where: { $0.id == conversationID }) else {
            errorMessage = "conversation not loaded: \(conversationID)"
            return
        }
        do {
            _ = try await service.turnStart(
                conversationID: conversationID,
                projectID: summary.projectId,
                text: trimmed,
                idempotencyKey: key
            )
            _ = try await drainSupervisorQueue(maxTicks: supervisorDrainMaxTicks)
            drafts[conversationID] = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Drain queued accepted turns after a send or a cold-load recovery. Each
    /// supervisor tick claims and executes at most one turn; looping until an
    /// empty tick keeps a relaunched chat from leaving durable accepted turns
    /// stuck in the sidebar as forever-running.
    @discardableResult
    func drainSupervisorQueue(maxTicks: Int = 8) async throws -> Int {
        guard maxTicks > 0, !isDrainingSupervisor else { return 0 }
        isDrainingSupervisor = true
        defer { isDrainingSupervisor = false }

        var claimedTurns = 0
        for _ in 0..<maxTicks {
            let tick = try await service.supervisorTick()
            guard tick.claimed != nil || tick.executed != nil else { break }
            claimedTurns += 1
        }

        await load(recoverQueuedTurns: false)
        if let id = selectedConversationID {
            await loadMessages(for: id)
            await loadRuns(for: id)
            await loadTimeline(for: id)
            await loadThreadSettings(for: id)
        }
        return claimedTurns
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
            timelineByConversation[id] = timeline.items
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Refresh durable thread settings for a conversation from the service.
    func loadThreadSettings(for id: String) async {
        do {
            let settings = try await service.threadSettings(conversationID: id)
            threadSettingsByConversation[id] = settings
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
        guard !isSavingThreadSettings else { return }
        isSavingThreadSettings = true
        errorMessage = nil
        defer { isSavingThreadSettings = false }
        do {
            var next = threadSettings(for: id)
            mutate(&next)
            let saved = try await service.setThreadSettings(next, conversationID: id)
            threadSettingsByConversation[id] = saved
        } catch {
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
            threadSettingsByConversation[id] = nil
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
}
