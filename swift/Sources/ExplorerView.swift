// ExplorerView.swift — column-2 content host. Its body swaps with the selected
// rail section: a real file tree, agent-run cards, or honest provider/proof/
// artifact panels.

import SwiftUI

struct ExplorerView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(state.selectedRail.label.uppercased())
                    .font(Theme.ui(10.5, .semibold))
                    .tracking(0.8)
                    .foregroundStyle(Theme.muted)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.top, 14)
            .padding(.bottom, 8)

            switch state.selectedRail {
            case .explorer: fileTree
            case .agentRuns: agentRuns
            case .providers: providers
            case .proof: proof
            case .artifacts: artifacts
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.explorer)
    }

    private var fileTree: some View {
        List {
            OutlineGroup(state.fileRoots, children: \.children) { node in
                FileRow(node: node, selected: state.activeFileTab?.path == node.id)
                    .listRowBackground(Color.clear)
            }
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 26)
    }

    private var agentRuns: some View {
        ScrollView {
            VStack(spacing: 8) {
                ForEach((state.data?.workerLanes ?? []).reversed()) { lane in
                    laneCard(lane)
                }
                if (state.data?.workerLanes ?? []).isEmpty {
                    emptyNote("No agent runs yet.")
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    private func laneCard(_ lane: WorkerLane) -> some View {
        HStack(spacing: 10) {
            Image(systemName: LaneStatus.symbol(lane.status))
                .font(.system(size: 12))
                .foregroundStyle(LaneStatus.color(lane.status))
            VStack(alignment: .leading, spacing: 2) {
                Text(lane.missionId).font(Theme.ui(11.5, .semibold)).foregroundStyle(Theme.text).lineLimit(1)
                Text("\(lane.status) · \(lane.executionMode)").font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
            }
            Spacer()
            Chip(text: "\(lane.laneCount)", color: Theme.accent)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private var providers: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                MetricCallout(value: "\(state.data?.gui.providerConfiguredCount ?? 0)", label: "configured adapters", accent: Theme.accent)
                providerRow("OpenRouter")
                providerRow("OpenAI")
                Text("Readiness is verified by the CLI, never by reading keys.")
                    .font(Theme.ui(11)).foregroundStyle(Theme.muted)
                GhostButton(title: "Run provider check", systemImage: "powerplug") {
                    state.runVerb(label: "provider adapter-check", args: ["provider", "adapter-check"])
                }
            }
            .padding(.horizontal, 12).padding(.bottom, 12)
        }
    }

    private func providerRow(_ name: String) -> some View {
        HStack(spacing: 8) {
            Circle().fill(Theme.muted).frame(width: 8, height: 8)
            Text(name).font(Theme.ui(12.5)).foregroundStyle(Theme.textSoft)
            Spacer()
            Chip(text: "secret hidden", color: Theme.muted)
        }
    }

    private var proof: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                if let a = state.data?.acceptance {
                    ProofBar(passed: a.passed, partial: a.partial, failed: a.failed)
                    proofStat("Passed", "\(a.passed)", Theme.pass)
                    proofStat("Partial", "\(a.partial)", Theme.partial)
                    proofStat("Failed", "\(a.failed)", Theme.fail)
                    proofStat("Goal", HonestText.goalState(a), Theme.gold)
                }
                if let gui = state.data?.gui {
                    proofStat("Live gaps", "\(gui.prdMissingLive)", Theme.muted)
                    proofStat("QA", gui.qaStatus, isPass(gui.qaStatus) ? Theme.accent : Theme.gold)
                    proofStat("Security", gui.securityStatus, isPass(gui.securityStatus) ? Theme.accent : Theme.gold)
                }
                GhostButton(title: "Run acceptance audit", systemImage: "checkmark.seal") {
                    state.runAcceptance()
                }
            }
            .padding(.horizontal, 12).padding(.bottom, 12)
        }
    }

    private func proofStat(_ label: String, _ value: String, _ color: Color) -> some View {
        HStack {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(label).font(Theme.ui(12)).foregroundStyle(Theme.muted)
            Spacer()
            Text(value).font(Theme.ui(12, .semibold)).foregroundStyle(Theme.text)
        }
    }

    private var artifacts: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                artifactRow("App bundle", state.data?.appBundle)
                artifactRow("Artifact data", state.data?.artifactDir)
                artifactRow("Dashboard data", state.data?.dashboardHtml)
                artifactRow("Missions", state.data?.missionsDir)
            }
            .padding(.horizontal, 12).padding(.bottom, 12)
        }
    }

    private func artifactRow(_ label: String, _ path: String?) -> some View {
        GhostButton(title: "Open \(label)", systemImage: "arrow.up.forward.app") {
            if let path { state.reveal(path) }
        }
    }

    private func emptyNote(_ text: String) -> some View {
        Text(text).font(Theme.ui(12)).foregroundStyle(Theme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct FileRow: View {
    var node: FileNode
    var selected: Bool

    @EnvironmentObject private var state: AppState

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: node.isDir ? "folder.fill" : "doc.text")
                .font(.system(size: 11))
                .foregroundStyle(node.isDir ? Theme.blue : Theme.muted)
            Text(node.name)
                .font(Theme.ui(12))
                .foregroundStyle(selected ? Theme.text : Theme.textSoft)
                .lineLimit(1)
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if !node.isDir { state.openFile(node.id) }
        }
    }
}
