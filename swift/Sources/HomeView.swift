// HomeView.swift — the editor-center cockpit shown when no file is open. It
// reads as a populated agent workspace and states acceptance honestly.

import SwiftUI

struct HomeView: View {
    @EnvironmentObject private var state: AppState
    @EnvironmentObject private var nav: NavigationStore

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.s20) {
                header
                metrics
                startHere
                Text("OpenSKS Studio surfaces real artifacts and honest proof state. It never claims completion that acceptance has not verified.")
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.muted)
            }
            .frame(maxWidth: 720, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 40)
            .padding(.vertical, 36)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.s16) {
            HStack(spacing: Theme.s12) {
                AgentMark(size: 44)
                VStack(alignment: .leading, spacing: 2) {
                    Text("OpenSKS Studio").font(Theme.display).foregroundStyle(Theme.text)
                    Text("An autonomous coding workspace.").font(Theme.ui(13)).foregroundStyle(Theme.muted)
                }
            }
            if let data = state.data {
                HStack(spacing: 8) {
                    Chip(text: data.workspaceLabel, color: Theme.textSoft)
                    Chip(text: "Acceptance \(HonestText.acceptanceLine(data.acceptance)) · \(HonestText.goalState(data.acceptance))", color: Theme.gold)
                    Chip(text: "\(data.gui.providerConfiguredCount) providers", color: Theme.accent)
                }
            }
        }
    }

    private var metrics: some View {
        let gui = state.data?.gui
        let ratio = Int(((state.data?.acceptance.ratio ?? 0) * 100).rounded())
        return HStack(spacing: 0) {
            metric("\(ratio)%", "acceptance passed", Theme.accent)
            divider
            metric("\(gui?.missionCount ?? 0)", "missions", Theme.text)
            divider
            metric("\(gui?.workerLaneCount ?? 0)", "agent lanes", Theme.text)
            divider
            metric("\(gui?.voxelCount ?? 0)", "voxels", Theme.text)
            Spacer(minLength: 0)
        }
        .padding(16)
        .background(RoundedRectangle(cornerRadius: Theme.rXl).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rXl).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private func metric(_ value: String, _ label: String, _ accent: Color) -> some View {
        MetricCallout(value: value, label: label, accent: accent)
    }

    private var divider: some View {
        Rectangle().fill(Theme.stroke).frame(width: 1, height: 38).padding(.horizontal, 22)
    }

    private var startHere: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            SectionHeader(title: "Start here")
            actionCard(
                "Start a coding task",
                "Describe the change you want in Chat, then choose Quick Edit, Plan & Execute, or Parallel Build.",
                "Open Chat", "bubble.left.and.text.bubble.right"
            ) { nav.route = .chat }
            actionCard(
                "Verify proof state",
                "Run the acceptance audit and watch results stream in the Output drawer.",
                "Run acceptance", "checkmark.seal"
            ) { state.runAcceptance() }
            actionCard(
                "Read the codebase",
                "Browse the workspace and open files in the syntax-highlighted viewer.",
                "Open files", "folder"
            ) { state.selectedRail = .files }
        }
    }

    private func actionCard(_ title: String, _ body: String, _ cta: String, _ symbol: String, action: @escaping () -> Void) -> some View {
        HStack(alignment: .center, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(Theme.ui(15, .semibold)).foregroundStyle(Theme.text)
                Text(body).font(Theme.ui(12.5)).foregroundStyle(Theme.muted)
            }
            Spacer()
            PrimaryButton(title: cta, systemImage: symbol, action: action)
                .frame(width: 170)
        }
        .padding(16)
        .background(RoundedRectangle(cornerRadius: Theme.rLg).fill(Theme.panel))
        .overlay(RoundedRectangle(cornerRadius: Theme.rLg).strokeBorder(Theme.stroke, lineWidth: 1))
    }
}
