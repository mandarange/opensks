// TerminalView.swift — bottom drawer with live streamed Output, an honest
// Problems list derived from AppData, and an Activity feed. Collapsible.

import SwiftUI

struct TerminalView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(spacing: 0) {
            header
            if !state.terminalCollapsed {
                Divider().overlay(Theme.stroke)
                content
            }
        }
        .background(Theme.terminal)
    }

    private var header: some View {
        HStack(spacing: 6) {
            ForEach(TerminalTab.allCases) { tab in
                tabButton(tab)
            }
            if state.terminalTab == .problems, let count = problemCount, count > 0 {
                Text("\(count)")
                    .font(Theme.ui(9.5, .bold))
                    .foregroundStyle(Theme.accentInk)
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .background(Capsule().fill(Theme.coral))
            }
            Spacer()
            if state.terminalTab == .output && !state.lines.isEmpty {
                Button { state.clearOutput() } label: {
                    Text("Clear").font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
                }.buttonStyle(.plain)
            }
            Button {
                withAnimation(.easeOut(duration: 0.15)) { state.terminalCollapsed.toggle() }
            } label: {
                Image(systemName: state.terminalCollapsed ? "chevron.up" : "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.muted)
            }.buttonStyle(.plain)
        }
        .padding(.horizontal, 12)
        .frame(height: 30)
    }

    private func tabButton(_ tab: TerminalTab) -> some View {
        let active = state.terminalTab == tab
        return Button {
            state.terminalTab = tab
            state.terminalCollapsed = false
        } label: {
            Text(tab.label)
                .font(Theme.ui(11.5, .semibold))
                .foregroundStyle(active ? Theme.accent : Theme.muted)
                .padding(.vertical, 6)
                .overlay(alignment: .bottom) {
                    if active { Rectangle().fill(Theme.accent).frame(height: 2) }
                }
        }.buttonStyle(.plain)
    }

    @ViewBuilder private var content: some View {
        switch state.terminalTab {
        case .output: output
        case .problems: problems
        case .activity: activity
        }
    }

    private var output: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 1) {
                    if state.lines.isEmpty {
                        Text("Output from CLI runs appears here.")
                            .font(Theme.mono(11.5)).foregroundStyle(Theme.muted)
                    }
                    ForEach(state.lines) { line in
                        Text(line.text)
                            .font(Theme.mono(11.5))
                            .foregroundStyle(line.kind.color)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .id(line.id)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
            }
            .onChange(of: state.lines.count) { _ in
                withAnimation(.linear(duration: 0.1)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
        }
    }

    private var problemCount: Int? {
        guard let d = state.data else { return nil }
        var n = 0
        if d.acceptance.partial > 0 { n += 1 }
        if d.acceptance.failed > 0 { n += 1 }
        if !isPass(d.gui.qaStatus) { n += 1 }
        if !isPass(d.gui.securityStatus) { n += 1 }
        if d.gui.prdMissingLive > 0 { n += 1 }
        return n
    }

    private var problems: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if let d = state.data {
                    if d.acceptance.partial > 0 {
                        problemRow(Theme.partial, "\(d.acceptance.partial) acceptance criteria partial", "acceptance audit")
                    }
                    if d.acceptance.failed > 0 {
                        problemRow(Theme.fail, "\(d.acceptance.failed) acceptance criteria failed", "acceptance audit")
                    }
                    if !isPass(d.gui.qaStatus) {
                        problemRow(Theme.partial, "QA status: \(d.gui.qaStatus)", "qa run")
                    }
                    if !isPass(d.gui.securityStatus) {
                        problemRow(Theme.partial, "Security status: \(d.gui.securityStatus)", "security audit")
                    }
                    if d.gui.prdMissingLive > 0 {
                        problemRow(Theme.muted, "\(d.gui.prdMissingLive) requirements missing live implementation", "prd coverage")
                    }
                    if (problemCount ?? 0) == 0 {
                        Text("No tracked gaps.").font(Theme.ui(12)).foregroundStyle(Theme.muted)
                    }
                } else {
                    Text("Loading…").font(Theme.ui(12)).foregroundStyle(Theme.muted)
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
        }
    }

    private func problemRow(_ color: Color, _ title: String, _ verb: String) -> some View {
        HStack(spacing: 10) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(title).font(Theme.ui(12)).foregroundStyle(Theme.textSoft)
            Spacer()
            Text(verb).font(Theme.mono(10.5)).foregroundStyle(Theme.faint)
        }
        .padding(.vertical, 5)
    }

    private var activity: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 6) {
                activityRow(state.isRunning ? "Agent run in progress" : "Agent idle")
                if !state.lastVerb.isEmpty {
                    activityRow("Last: \(state.lastVerb)" + (state.lastExit.map { " (exit \($0))" } ?? ""))
                }
                if let gui = state.data?.gui {
                    activityRow("\(gui.missionCount) missions tracked · \(gui.workerLaneCount) lanes")
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
        }
    }

    private func activityRow(_ text: String) -> some View {
        HStack(spacing: 8) {
            Text("•").foregroundStyle(Theme.muted)
            Text(text).font(Theme.ui(12)).foregroundStyle(Theme.textSoft)
            Spacer()
        }
    }
}
