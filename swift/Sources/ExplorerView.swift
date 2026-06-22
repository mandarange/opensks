// ExplorerView.swift — column-2 content host. Its body swaps with the selected
// rail section and keeps not-live surfaces explicitly marked.

import SwiftUI

struct ExplorerView: View {
    @EnvironmentObject private var state: AppState
    @EnvironmentObject private var nav: NavigationStore
    @EnvironmentObject private var coordinator: AppCoordinator

    var body: some View {
        // The `.chat` route owns its own pane: the conversation sidebar replaces
        // the legacy rail-section content (PR-025). All other routes keep the
        // existing context pane below.
        if nav.route == .chat {
            ConversationSidebar(
                store: coordinator.conversations,
                projectName: state.data?.workspaceLabel ?? "Workspace"
            )
        } else {
            legacyPane
        }
    }

    private var legacyPane: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(nav.route.label.uppercased())
                    .font(Theme.ui(10.5, .semibold))
                    .tracking(0.8)
                    .foregroundStyle(Theme.muted)
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.top, 14)
            .padding(.bottom, 8)

            switch state.selectedRail {
            case .home: home
            case .graph: graphEditor
            case .runs: agentRuns
            case .queue: queue
            case .models: providers
            case .intelligence: projectIntelligence
            case .git: notVerified("Git Studio requires real worktree and Outbox APIs.")
            case .evidence: proof
            case .files: fileTree
            case .settings: artifacts
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.explorer)
    }

    private var fileTree: some View {
        List {
            OutlineGroup(state.fileRoots, children: \.children) { node in
                FileRow(node: node, selected: state.activeEditorPath == node.id)
                    .listRowBackground(Color.clear)
            }
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        .environment(\.defaultMinListRowHeight, 26)
    }

    private var home: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                MetricCallout(value: state.engineStatus, label: "engine daemon", accent: state.engineStatus == "Ready" ? Theme.accent : Theme.gold)
                ForEach(state.engineEvents.prefix(4)) { event in
                    eventRow(event)
                }
                GhostButton(title: "Check engine health", systemImage: "heart.text.square") {
                    state.connectEngine()
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    private func eventRow(_ event: EngineEvent) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(event.eventType.rawValue)
                .font(Theme.ui(12, .semibold))
                .foregroundStyle(event.severity.isError ? Theme.coral : Theme.text)
            Text(event.message)
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .lineLimit(2)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private func notVerified(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            StatePill(label: "Not verified", color: Theme.gold)
            Text(message)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 12)
    }

    private var agentRuns: some View {
        ScrollView {
            VStack(spacing: 8) {
                if !state.executionStore.runs.isEmpty {
                    LazyVGrid(columns: [
                        GridItem(.flexible(), spacing: 8),
                        GridItem(.flexible(), spacing: 8)
                    ], alignment: .leading, spacing: 8) {
                        GhostButton(title: "Pause", systemImage: "pause.fill") {
                            state.pauseEngineRun()
                        }
                        GhostButton(title: "Resume", systemImage: "play.fill") {
                            state.resumeEngineRun()
                        }
                        GhostButton(title: "Cancel", systemImage: "xmark.circle") {
                            state.cancelEngineRun()
                        }
                        GhostButton(title: "Replay", systemImage: "arrow.clockwise") {
                            state.replayEngineRun()
                        }
                        GhostButton(title: "Tail", systemImage: "dot.radiowaves.left.and.right") {
                            state.tailEngineRun()
                        }
                    }
                    LazyVGrid(columns: [
                        GridItem(.flexible(), spacing: 8),
                        GridItem(.flexible(), spacing: 8)
                    ], alignment: .leading, spacing: 8) {
                        GhostButton(title: "Request", systemImage: "hand.raised") {
                            state.requestEngineApproval()
                        }
                        GhostButton(title: "Approve", systemImage: "checkmark.seal") {
                            state.approveFirstApproval()
                        }
                        GhostButton(title: "Deny", systemImage: "xmark.seal") {
                            state.denyFirstApproval()
                        }
                    }
                    ForEach(state.executionStore.runs) { run in
                        runCard(run)
                    }
                    ForEach(state.executionStore.approvals) { approval in
                        approvalCard(approval)
                    }
                } else {
                    ForEach((state.data?.workerLanes ?? []).reversed()) { lane in
                        laneCard(lane)
                    }
                }
                if state.executionStore.runs.isEmpty && (state.data?.workerLanes ?? []).isEmpty {
                    emptyNote("No agent runs yet.")
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    private var queue: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                StatePill(label: state.executionStore.queueItems.isEmpty ? "Not verified" : "Event sourced", color: state.executionStore.queueItems.isEmpty ? Theme.gold : Theme.accent)
                ForEach(state.executionStore.queueItems) { item in
                    queueCard(item)
                }
                if state.executionStore.queueItems.isEmpty {
                    emptyNote("Queue state will reconstruct from execution events.")
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    private var graphEditor: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                StatePill(label: state.graphEditorStore.problems.isEmpty ? "Compiled" : "Problems", color: state.graphEditorStore.problems.isEmpty ? Theme.accent : Theme.gold)
                LazyVGrid(columns: [
                    GridItem(.flexible(), spacing: 8),
                    GridItem(.flexible(), spacing: 8)
                ], alignment: .leading, spacing: 8) {
                    GhostButton(title: "Template", systemImage: "square.grid.2x2") {
                        state.loadGraphTemplate()
                    }
                    GhostButton(title: "Save", systemImage: "square.and.arrow.down") {
                        state.saveGraphEditorDocument()
                    }
                    GhostButton(title: "Load", systemImage: "folder") {
                        state.loadGraphEditorDocument()
                    }
                    GhostButton(title: "Run", systemImage: "play.fill") {
                        state.runGraphEditorDocument()
                    }
                }
                MetricCallout(value: "\(state.graphEditorStore.nodes.count)", label: "graph nodes", accent: Theme.accent)
                Text(state.graphEditorStore.documentName)
                    .font(Theme.ui(12, .semibold))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                if let path = state.graphEditorStore.lastSavedPath {
                    Text(path)
                        .font(Theme.mono(10.5))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                if let path = state.graphEditorStore.lastExportedGraphPath {
                    Text("export \(path)")
                        .font(Theme.mono(10.5))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                ForEach(state.graphEditorStore.visibleNodes(limit: 12)) { node in
                    HStack {
                        Text(node.title).font(Theme.ui(12, .semibold)).foregroundStyle(Theme.text).lineLimit(1)
                        Spacer()
                        Chip(text: node.kind, color: Theme.muted)
                    }
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
                }
                ForEach(state.graphEditorStore.problems.prefix(4)) { problem in
                    Text(problem.message).font(Theme.ui(11)).foregroundStyle(Theme.gold)
                }
                if state.graphEditorStore.nodes.isEmpty {
                    emptyNote("Graph editor state is ready for templates and compile diagnostics.")
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    private var projectIntelligence: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                StatePill(label: state.intelligenceStore.freshnessLabel, color: state.intelligenceStore.freshnessLabel == "Fresh" ? Theme.accent : Theme.gold)
                MetricCallout(value: "\(state.intelligenceStore.records.count)", label: "intelligence records", accent: Theme.accent)
                ForEach(state.intelligenceStore.visibleRecords(limit: 12)) { record in
                    Button {
                        if let path = state.intelligenceStore.sourcePath(for: record.id) {
                            state.openFile(path)
                            state.selectedRail = .files
                        }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(record.title).font(Theme.ui(12, .semibold)).foregroundStyle(Theme.text).lineLimit(1)
                                Text(record.summary).font(Theme.ui(10.5)).foregroundStyle(Theme.muted).lineLimit(2)
                            }
                            Spacer()
                            Chip(text: record.kind, color: Theme.muted)
                        }
                    }
                    .buttonStyle(.plain)
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
                    .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
                }
                if state.intelligenceStore.records.isEmpty {
                    emptyNote("Project Intelligence will lazy-load CodeGraph and TriWiki records.")
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    private func runCard(_ run: RunRecord) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(run.id).font(Theme.ui(11.5, .semibold)).foregroundStyle(Theme.text).lineLimit(1)
                Spacer()
                Chip(text: run.state, color: LaneStatus.color(run.state))
            }
            Text(run.lastMessage).font(Theme.ui(10.5)).foregroundStyle(Theme.muted).lineLimit(2)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private func queueCard(_ item: QueueItemRecord) -> some View {
        HStack(spacing: 10) {
            Image(systemName: LaneStatus.symbol(item.state))
                .font(.system(size: 12))
                .foregroundStyle(LaneStatus.color(item.state))
            VStack(alignment: .leading, spacing: 2) {
                Text(item.id).font(Theme.ui(11.5, .semibold)).foregroundStyle(Theme.text).lineLimit(1)
                Text("\(item.state) · priority \(item.priority)").font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
            }
            Spacer()
            Chip(text: "#\(item.lastSequence)", color: Theme.muted)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private func approvalCard(_ approval: ApprovalRecord) -> some View {
        HStack(spacing: 10) {
            Image(systemName: approval.state == "approved" ? "checkmark.seal.fill" : "hand.raised")
                .font(.system(size: 12))
                .foregroundStyle(approval.state == "denied" ? Theme.coral : Theme.gold)
            VStack(alignment: .leading, spacing: 2) {
                Text(approval.id).font(Theme.ui(11.5, .semibold)).foregroundStyle(Theme.text).lineLimit(1)
                Text("\(approval.scope) · \(approval.state)").font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
            }
            Spacer()
            Chip(text: "#\(approval.lastSequence)", color: Theme.muted)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
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
        Button {
            if !node.isDir { state.openFile(node.id) }
        } label: {
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
        }
        .buttonStyle(.plain)
        .accessibilityLabel(node.isDir ? "Folder \(node.name)" : "Open \(node.name)")
    }
}
