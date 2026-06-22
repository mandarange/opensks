// ConversationStore.swift — the @MainActor view model for the conversation
// sidebar + thread. It owns the summaries list, the selected conversation, the
// loaded message page for that selection, per-conversation drafts, and the
// filter / search text. All persistence goes through an injected
// `ConversationService`. PR-025 deliberately has NO run/send functionality —
// engine-driven turns arrive in PR-027.

import SwiftUI

@MainActor
final class ConversationStore: ObservableObject {
    // Service boundary. Swappable so the live service can be (re)bound once the
    // workspace path is known at runtime (RootView.onAppear).
    @Published private(set) var service: ConversationService

    // List + selection.
    @Published var summaries: [ConversationSummary] = []
    @Published var selectedConversationID: String?
    @Published var projectId: String?

    // Loaded message page for the current selection.
    @Published private(set) var messages: [ConversationMessage] = []
    @Published private(set) var hasMoreMessages = false

    // Per-conversation composer drafts (no send yet — seeded/manual text only).
    @Published var drafts: [String: String] = [:]

    // Filter / search.
    @Published var filter: ConversationFilter = .all
    @Published var searchText: String = ""

    // Status surfacing.
    @Published private(set) var isLoading = false
    @Published private(set) var isLoadingMessages = false
    @Published var errorMessage: String?

    /// Page size for the message pager.
    let messagePageSize: Int

    init(service: ConversationService, messagePageSize: Int = 50) {
        self.service = service
        self.messagePageSize = messagePageSize
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
            }
            if selectedConversationID == nil, let first = summaries.first {
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
        selectedConversationID = id
        await loadMessages(for: id)
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
            if selectedConversationID == id {
                selectedConversationID = nil
                messages = []
                hasMoreMessages = false
            }
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
