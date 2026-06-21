// EditorView.swift — center-top code surface: a tab strip, a breadcrumb, and a
// virtualized read-only viewer with a line-number gutter and per-line syntax
// highlighting. Falls back to the Home cockpit when no file is open.

import SwiftUI

struct EditorView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        Group {
            if let tab = state.activeFileTab {
                VStack(spacing: 0) {
                    tabStrip
                    breadcrumb(tab)
                    Divider().overlay(Theme.stroke)
                    codeView(tab)
                }
            } else {
                HomeView()
            }
        }
        .background(Theme.editor)
    }

    private var tabStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(state.tabs) { tab in
                    tabChip(tab)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
        }
        .background(Theme.sidebar)
        .frame(height: 38)
    }

    private func tabChip(_ tab: FileTab) -> some View {
        let active = state.activeTab == tab.id
        return HStack(spacing: 7) {
            Circle().fill(tab.lang.dotColor).frame(width: 6, height: 6)
            Text(tab.name)
                .font(Theme.ui(11.5, active ? .medium : .regular))
                .foregroundStyle(active ? Theme.text : Theme.muted)
            Button { state.closeTab(tab.id) } label: {
                Image(systemName: "xmark").font(.system(size: 8, weight: .bold)).foregroundStyle(Theme.muted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm)
                .fill(active ? Theme.editor : Color.clear)
        )
        .overlay(alignment: .top) {
            if active {
                Rectangle().fill(Theme.accent).frame(height: 2)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { state.activeTab = tab.id }
    }

    private func breadcrumb(_ tab: FileTab) -> some View {
        HStack(spacing: 6) {
            Text(tab.path)
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.faint)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
            Text("\(tab.lang.label) · \(tab.lines.count) lines · UTF-8")
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.muted)
        }
        .padding(.horizontal, 12)
        .frame(height: 26)
    }

    private func codeView(_ tab: FileTab) -> some View {
        ScrollView([.horizontal, .vertical]) {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(Array(tab.lines.enumerated()), id: \.offset) { idx, line in
                    HStack(alignment: .top, spacing: 0) {
                        Text("\(idx + 1)")
                            .font(Theme.mono(11))
                            .foregroundStyle(Theme.gutterText)
                            .frame(width: 52, alignment: .trailing)
                            .padding(.trailing, 10)
                        Text(SyntaxHighlighter.line(line.isEmpty ? " " : line, lang: tab.lang))
                            .textSelection(.enabled)
                        Spacer(minLength: 0)
                    }
                    .frame(minHeight: 17)
                }
            }
            .padding(.vertical, 8)
        }
        .background(Theme.editor)
    }
}
