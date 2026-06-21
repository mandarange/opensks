// ComposerView.swift — the right-hand hero. Objective → mode → run → live lanes
// → pinned honest proof footer. The run buttons map 1:1 to CLI verbs.

import SwiftUI

struct ComposerView: View {
    @EnvironmentObject private var state: AppState
    @FocusState private var objectiveFocused: Bool

    private var canRun: Bool {
        !state.objective.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !state.isRunning
    }

    private var modeSelection: Binding<Int> {
        Binding(
            get: { RunMode.allCases.firstIndex(of: state.runMode) ?? 0 },
            set: { state.runMode = RunMode.allCases[$0] }
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            objectiveField
            modeBlock
            runBar
            Divider().overlay(Theme.stroke).padding(.vertical, Theme.s12)
            lanes
            Divider().overlay(Theme.stroke).padding(.top, Theme.s8)
            proofFooter
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(
            LinearGradient(colors: [Theme.panel, Theme.panelDeep], startPoint: .top, endPoint: .bottom)
        )
        .overlay(alignment: .leading) {
            Rectangle().fill(Theme.seam).frame(width: 1)
        }
    }

    private var header: some View {
        HStack {
            Text("Composer").font(Theme.ui(15, .semibold)).foregroundStyle(Theme.text)
            Spacer()
            StatePill(
                label: state.isRunning ? "Running" : "Ready",
                color: state.isRunning ? Theme.accent : Theme.muted,
                pulse: state.isRunning
            )
        }
        .padding(.bottom, Theme.s6)
    }

    private var objectiveField: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Describe an autonomous coding objective.")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .padding(.bottom, Theme.s8)
            ZStack(alignment: .topLeading) {
                if state.objective.isEmpty {
                    Text("e.g. Add a bounded retry to the goal loop and prove it with acceptance…")
                        .font(Theme.ui(12.5))
                        .foregroundStyle(Theme.faint)
                        .padding(.horizontal, 9)
                        .padding(.vertical, 9)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $state.objective)
                    .focused($objectiveFocused)
                    .font(Theme.ui(12.5))
                    .foregroundStyle(Theme.text)
                    .scrollContentBackground(.hidden)
                    .padding(4)
            }
            .frame(height: 112)
            .background(RoundedRectangle(cornerRadius: Theme.rLg).fill(Theme.input))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rLg)
                    .strokeBorder(objectiveFocused ? Theme.accentSeam : Theme.strokeSoft,
                                  lineWidth: objectiveFocused ? 1.4 : 1)
            )
            .onChange(of: state.focusObjective) { _, newValue in
                if newValue { objectiveFocused = true; state.focusObjective = false }
            }
        }
    }

    private var modeBlock: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            SegmentedControl(options: RunMode.allCases.map(\.label), selection: modeSelection)
                .padding(.top, Theme.s10)
            Text(state.runMode.caption)
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
        }
    }

    private var runBar: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            HStack(spacing: 8) {
                PrimaryButton(
                    title: "Start \(state.runMode.label) run",
                    systemImage: "play.fill",
                    enabled: canRun
                ) { state.startRun() }
                .keyboardShortcut(.return, modifiers: .command)

                GhostButton(title: "Acceptance", systemImage: "checkmark.seal") {
                    state.runAcceptance()
                }
            }
            .padding(.top, Theme.s12)
            Text("⌘↵ to start · runs shell the OpenSKS CLI")
                .font(Theme.ui(10))
                .foregroundStyle(Theme.muted)
        }
    }

    private var lanes: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            SectionHeader(title: "Lanes", trailing: "\(state.data?.gui.workerLaneMissions ?? 0) groups")
            ScrollView {
                VStack(spacing: 6) {
                    let all = state.data?.workerLanes ?? []
                    if all.isEmpty {
                        Text("No lanes yet — start a run to populate this.")
                            .font(Theme.ui(12))
                            .foregroundStyle(Theme.muted)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    ForEach(all.reversed()) { lane in
                        laneRow(lane)
                    }
                }
            }
            .frame(maxHeight: .infinity)
        }
        .frame(maxHeight: .infinity)
    }

    private func laneRow(_ lane: WorkerLane) -> some View {
        HStack(spacing: 8) {
            Image(systemName: LaneStatus.symbol(lane.status))
                .font(.system(size: 11))
                .foregroundStyle(LaneStatus.color(lane.status))
            Text(lane.missionId).font(Theme.ui(11)).foregroundStyle(Theme.textSoft).lineLimit(1)
            Spacer()
            Text("\(lane.laneCount) lanes").font(Theme.ui(10)).foregroundStyle(Theme.muted)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Theme.input.opacity(0.6)))
    }

    private var proofFooter: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            if let a = state.data?.acceptance {
                ProofBar(passed: a.passed, partial: a.partial, failed: a.failed)
                    .padding(.top, Theme.s8)
                HStack {
                    Text(HonestText.acceptanceLine(a))
                        .font(Theme.ui(11, .semibold))
                        .monospacedDigit()
                        .foregroundStyle(Theme.textSoft)
                    Spacer()
                    Text(HonestText.goalState(a))
                        .font(Theme.ui(10.5, .semibold))
                        .foregroundStyle(Theme.gold)
                        .padding(.horizontal, 8).padding(.vertical, 3)
                        .background(Capsule().fill(Theme.gold.opacity(0.12)))
                }
            }
        }
    }
}
