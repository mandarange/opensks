// ConversationSidebar.swift — the conversation list pane for the `.chat` route.
// Header (project name + New conversation), a search field, a filter control,
// then a list of full-row conversation tiles. The ENTIRE tile is the hit target
// (a Button + contentShape), never just the title — per the PR-023 interaction
// rule. Arrow up/down move the selection and Return opens it.

import SwiftUI

struct ConversationSidebar: View {
    @ObservedObject var store: ConversationStore
    @ObservedObject var providers: ProviderStore
    /// Project name for the header (resolved from AppState workspace label).
    var projectName: String = "Workspace"
    var onOpenProviderSettings: () -> Void = {}

    @FocusState private var listFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            providerControl
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

    // MARK: - Providers

    private var providerControl: some View {
        HStack(spacing: 8) {
            Menu {
                if providers.connections.isEmpty {
                    Button("Connect provider") {
                        providers.showingAddProvider = true
                    }
                } else {
                    ForEach(providers.connections) { provider in
                        Button {
                            providers.selectedProviderID = provider.id
                        } label: {
                            Label(
                                provider.displayName,
                                systemImage: provider.id == providers.selectedProviderID
                                    ? "checkmark"
                                    : providerStatusIcon(provider)
                            )
                        }
                    }
                    Divider()
                    Button("Add provider") {
                        providers.showingAddProvider = true
                    }
                }
                Button("Provider settings") {
                    onOpenProviderSettings()
                }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "dot.radiowaves.left.and.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(providers.hasEligibleTextModel ? Theme.accent : Theme.gold)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("LLM providers")
                            .font(Theme.ui(11.5, .semibold))
                            .foregroundStyle(Theme.textSoft)
                        Text(providerSummaryText)
                            .font(Theme.ui(10.5))
                            .foregroundStyle(Theme.muted)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(Theme.faint)
                }
                .padding(.horizontal, 10)
                .frame(height: 42)
                .background(RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous).fill(Theme.input))
                .overlay(RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous).strokeBorder(Theme.stroke, lineWidth: 1))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .help(providers.providerReadinessDetail)
            .accessibilityLabel("LLM provider settings")
            .accessibilityIdentifier("conversation.sidebar.providers.menu")

            Button {
                onOpenProviderSettings()
            } label: {
                Image(systemName: "slider.horizontal.3")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.textSoft)
            }
            .buttonStyle(IconTileButtonStyle(size: 30))
            .help("Open provider settings")
            .accessibilityLabel("Open provider settings")
            .accessibilityIdentifier("conversation.sidebar.providers.settings")
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
        .accessibilityIdentifier("conversation.sidebar.providers")
    }

    private var providerSummaryText: String {
        if providers.connections.isEmpty { return "No provider connected" }
        let ready = providers.eligibleTextModels.count
        if ready == 0 {
            return "\(providers.enabledProviderCount) provider\(providers.enabledProviderCount == 1 ? "" : "s") · no ready model"
        }
        return "\(providers.enabledProviderCount) provider\(providers.enabledProviderCount == 1 ? "" : "s") · \(ready) code model\(ready == 1 ? "" : "s")"
    }

    private func providerStatusIcon(_ provider: ProviderConnectionViewModel) -> String {
        switch provider.statusPillKind {
        case .success: return "checkmark.circle"
        case .warning: return "exclamationmark.triangle"
        case .danger: return "xmark.octagon"
        case .running: return "arrow.triangle.2.circlepath"
        case .neutral: return "circle"
        }
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
            VStack(spacing: 8) {
                if let error = store.errorMessage {
                    SidebarInlineError(message: error)
                        .padding(.horizontal, 10)
                        .padding(.top, 8)
                }
                list
            }
        }
    }

    private var list: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 4) {
                    ForEach(store.visibleSummaries) { summary in
                        ConversationRow(
                            summary: summary,
                            isSelected: store.selectedConversationID == summary.id,
                            onTogglePinned: {
                                Task { await store.togglePinned(summary.id) }
                            }
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

private struct SidebarInlineError: View {
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.fail)
            Text(message)
                .font(Theme.ui(11, .medium))
                .foregroundStyle(Theme.text)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Theme.fail.opacity(0.12))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(Theme.fail.opacity(0.35), lineWidth: 1)
        )
        .accessibilityIdentifier("conversation.sidebar.error")
    }
}

// MARK: - Row

/// A single conversation tile. The title/status area opens the thread, while the
/// trailing pin button exposes the same pin/unpin action as the context menu.
struct ConversationRow: View {
    let summary: ConversationSummary
    let isSelected: Bool
    let onTogglePinned: () -> Void
    let onSelect: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 6) {
            Button(action: onSelect) {
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
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier("conversation.row.\(summary.id)")
            .accessibilityLabel("Conversation \(summary.title), \(summary.status.displayLabel)")
            .accessibilityAddTraits(isSelected ? [.isSelected, .isButton] : .isButton)

            Button(action: onTogglePinned) {
                Image(systemName: summary.pinned ? "pin.slash.fill" : "pin")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(summary.pinned ? Theme.accent : Theme.muted)
                    .frame(width: 24, height: 24)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .help(summary.pinned ? "Unpin conversation" : "Pin conversation (up to 5)")
            .accessibilityLabel(summary.pinned ? "Unpin conversation" : "Pin conversation")
            .accessibilityIdentifier("conversation.pin.\(summary.id)")

            StatusPill(kind: summary.status.pillKind, label: summary.status.displayLabel)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 9)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .fill(isSelected ? Theme.accentTint : Color.clear)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                .strokeBorder(isSelected ? Theme.strokeSoft : Color.clear, lineWidth: 1)
        )
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
