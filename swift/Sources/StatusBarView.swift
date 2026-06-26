// StatusBarView.swift — a persistent 26pt bottom bar that always carries the
// honest proof state. Completion language routes through HonestText only.

import SwiftUI

struct StatusBarView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: "folder").font(.system(size: 10)).foregroundStyle(Theme.muted)
            Text(state.data?.workspaceLabel ?? (state.loadError == nil ? "loading…" : "workspace unavailable"))
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.muted)
                .lineLimit(1)

            if let error = state.loadError {
                Text("· \(error)")
                    .font(Theme.ui(10.5))
                    .foregroundStyle(Theme.coral)
                    .lineLimit(1)

                Button {
                    state.requestWorkspaceAccess()
                } label: {
                    Image(systemName: "folder.badge.gearshape")
                        .font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.textSoft)
                .accessibilityLabel("Grant workspace folder access")
                .accessibilityIdentifier("status.workspace.access")
                .help("Grant workspace folder access")
            }

            Spacer()

            if let a = state.data?.acceptance {
                proofSegment("\(a.passed)", "passed", Theme.pass)
                proofSegment("\(a.partial)", "partial", Theme.partial)
                proofSegment("\(a.failed)", "failed", Theme.fail)
                Text(HonestText.goalState(a))
                    .font(Theme.ui(10.5, .semibold))
                    .foregroundStyle(Theme.gold)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(Theme.gold.opacity(0.12)))
            }

            if !state.lastVerb.isEmpty {
                Divider().frame(height: 12).overlay(Theme.stroke)
                HStack(spacing: 5) {
                    Text(state.lastVerb).font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
                    if let code = state.lastExit {
                        Text("exit \(code)")
                            .font(Theme.mono(10))
                            .foregroundStyle(code == 0 ? Theme.accent : Theme.coral)
                    } else if state.isRunning {
                        Text("running").font(Theme.ui(10)).foregroundStyle(Theme.accent)
                    }
                }
            }

            if let gui = state.data?.gui {
                Divider().frame(height: 12).overlay(Theme.stroke)
                Text("\(gui.missionCount) missions · \(gui.voxelCount) voxels")
                    .font(Theme.ui(10.5))
                    .monospacedDigit()
                    .foregroundStyle(Theme.faint)
            }
        }
        .frame(height: 26)
        .padding(.horizontal, 12)
        .background(Theme.panelDeep)
    }

    private func proofSegment(_ value: String, _ label: String, _ color: Color) -> some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(value).font(Theme.ui(10.5, .semibold)).monospacedDigit().foregroundStyle(Theme.textSoft)
            Text(label).font(Theme.ui(10.5)).foregroundStyle(Theme.muted)
        }
    }
}
