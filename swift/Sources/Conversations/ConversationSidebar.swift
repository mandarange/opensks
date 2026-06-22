// ConversationSidebar.swift — the conversation list pane for the `.chat` route.
// Header (project name + New conversation), a search field, a filter control,
// then a list of full-row conversation tiles. The ENTIRE tile is the hit target
// (a Button + contentShape), never just the title — per the PR-023 interaction
// rule. Arrow up/down move the selection and Return opens it.

import SwiftUI

struct ConversationSidebar: View {
    @ObservedObject var store: ConversationStore
    /// Project name for the header (resolved from AppState workspace label).
    var projectName: String = "Workspace"

    @FocusState private var listFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            searchField
            filterControl
            Divider().overlay(Theme.stroke)
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.explorer)
        .task { await store.load() }
        .accessibilityIdentifier("conversation.sidebar")
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text("CONVERSATIONS")
                    .font(Theme.ui(10.5, .semibold))
                    .tracking(0.8)
                    .foregroundStyle(Theme.muted)
                Text(projectName)
                    .font(Theme.ui(12.5, .semibold))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            Button {
                Task { await store.create() }
            } label: {
                Image(systemName: "square.and.pencil")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.accent)
            }
            .buttonStyle(IconTileButtonStyle(size: 30))
            .help("New conversation")
            .accessibilityLabel("New conversation")
            .accessibilityIdentifier("conversation.new")
        }
        .padding(.horizontal, 12)
        .padding(.top, 14)
        .padding(.bottom, 8)
    }

    // MARK: - Search

    private var searchField: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11))
                .foregroundStyle(Theme.muted)
            TextField("Search conversations", text: $store.searchText)
                .textFieldStyle(.plain)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.text)
                .accessibilityIdentifier("conversation.search")
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Theme.input))
        .overlay(RoundedRectangle(cornerRadius: Theme.rSm).strokeBorder(Theme.stroke, lineWidth: 1))
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
    }

    // MARK: - Filter

    private var filterControl: some View {
        Picker("Filter", selection: filterBinding) {
            ForEach(ConversationFilter.allCases) { option in
                Text(option.label).tag(option)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .padding(.horizontal, 12)
        .padding(.bottom, 10)
        .accessibilityIdentifier("conversation.filter")
    }

    private var filterBinding: Binding<ConversationFilter> {
        Binding(
            get: { store.filter },
            set: { newValue in Task { await store.applyFilter(newValue) } }
        )
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        if store.isLoading && store.summaries.isEmpty {
            EmptyStateView(
                headline: "Loading conversations",
                detail: "Reading the project's saved conversations.",
                systemImage: "bubble.left.and.bubble.right"
            )
        } else if let error = store.errorMessage, store.summaries.isEmpty {
            EmptyStateView(
                headline: "Could not load conversations",
                detail: error,
                systemImage: "exclamationmark.triangle",
                actionTitle: "Retry",
                action: { Task { await store.load() } }
            )
        } else if store.visibleSummaries.isEmpty {
            EmptyStateView(
                headline: store.searchText.isEmpty ? "No conversations yet" : "No matches",
                detail: store.searchText.isEmpty
                    ? "Start a new conversation to begin a thread for this project."
                    : "No conversations match “\(store.searchText)”.",
                systemImage: "bubble.left.and.bubble.right",
                actionTitle: store.searchText.isEmpty ? "New conversation" : nil,
                action: store.searchText.isEmpty ? { Task { await store.create() } } : nil
            )
        } else {
            list
        }
    }

    private var list: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 4) {
                    ForEach(store.visibleSummaries) { summary in
                        ConversationRow(
                            summary: summary,
                            isSelected: store.selectedConversationID == summary.id
                        ) {
                            Task { await store.select(summary.id) }
                        }
                        .id(summary.id)
                        .contextMenu { rowMenu(for: summary) }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
            }
            .focusable()
            .focused($listFocused)
            .onMoveCommand { direction in
                handleMove(direction, proxy: proxy)
            }
            .onAppear { listFocused = true }
        }
    }

    @ViewBuilder
    private func rowMenu(for summary: ConversationSummary) -> some View {
        Button(summary.pinned ? "Unpin" : "Pin") {
            Task { await store.togglePinned(summary.id) }
        }
        Button(summary.archived ? "Unarchive" : "Archive") {
            Task { await store.archive(summary.id, archived: !summary.archived) }
        }
        Button("Fork") {
            Task { await store.fork(summary.id) }
        }
        Divider()
        Button("Delete", role: .destructive) {
            Task { await store.delete(summary.id) }
        }
    }

    // MARK: - Keyboard

    private func handleMove(_ direction: MoveCommandDirection, proxy: ScrollViewProxy) {
        let rows = store.visibleSummaries
        guard !rows.isEmpty else { return }
        let currentIndex = rows.firstIndex { $0.id == store.selectedConversationID }
        switch direction {
        case .up:
            let next = max(0, (currentIndex ?? 0) - 1)
            moveSelection(to: rows[next].id, proxy: proxy)
        case .down:
            let next = min(rows.count - 1, (currentIndex ?? -1) + 1)
            moveSelection(to: rows[next].id, proxy: proxy)
        default:
            break
        }
    }

    private func moveSelection(to id: String, proxy: ScrollViewProxy) {
        Task { await store.select(id) }
        withAnimation(.easeOut(duration: 0.12)) {
            proxy.scrollTo(id, anchor: .center)
        }
    }
}

// MARK: - Row

/// A single conversation tile. The whole tile is one Button (entire row is the
/// hit target via `contentShape`), showing title + relative time + status pill.
struct ConversationRow: View {
    let summary: ConversationSummary
    let isSelected: Bool
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        if summary.pinned {
                            Image(systemName: "pin.fill")
                                .font(.system(size: 9))
                                .foregroundStyle(Theme.accent)
                                .accessibilityLabel("Pinned")
                        }
                        Text(summary.title)
                            .font(Theme.ui(12.5, .semibold))
                            .foregroundStyle(isSelected ? Theme.text : Theme.textSoft)
                            .lineLimit(1)
                    }
                    HStack(spacing: 8) {
                        Text(RelativeTime.string(from: summary.lastActivityDate))
                            .font(Theme.ui(10.5))
                            .foregroundStyle(Theme.muted)
                        if summary.messageCount > 0 {
                            Text("\(summary.messageCount) msg")
                                .font(Theme.ui(10.5))
                                .foregroundStyle(Theme.faint)
                        }
                    }
                }
                Spacer(minLength: 0)
                StatusPill(kind: summary.status.pillKind, label: summary.status.displayLabel)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 9)
            .contentShape(Rectangle())
            .background(
                RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                    .fill(isSelected ? Theme.accentTint : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                    .strokeBorder(isSelected ? Theme.strokeSoft : Color.clear, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("conversation.row.\(summary.id)")
        .accessibilityLabel("Conversation \(summary.title), \(summary.status.displayLabel)")
        .accessibilityAddTraits(isSelected ? [.isSelected, .isButton] : .isButton)
    }
}

// MARK: - Relative time

enum RelativeTime {
    private static let formatter: RelativeDateTimeFormatter = {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .abbreviated
        return f
    }()

    static func string(from date: Date, now: Date = Date()) -> String {
        if now.timeIntervalSince(date) < 45 { return "just now" }
        return formatter.localizedString(for: date, relativeTo: now)
    }
}
