// RailView.swift — the 56pt left icon rail. Selecting an item swaps the
// Explorer pane's content (rail → pane, never a top tab bar).

import SwiftUI

struct RailView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(spacing: 4) {
            Spacer().frame(height: 30)
            ForEach(RailSection.allCases) { section in
                RailButton(
                    section: section,
                    active: state.selectedRail == section
                ) { state.selectedRail = section }
            }
            Spacer()
            RailButton(section: nil, active: false, symbol: "gearshape") {
                state.reveal(state.data?.artifactDir ?? state.workspace.path)
            }
            Spacer().frame(height: 12)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.sidebar)
    }
}

private struct RailButton: View {
    var section: RailSection?
    var active: Bool
    var symbol: String? = nil
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                if active {
                    RoundedRectangle(cornerRadius: 9, style: .continuous)
                        .fill(Theme.accentTint)
                        .frame(width: 36, height: 36)
                    HStack {
                        RoundedRectangle(cornerRadius: 1.5)
                            .fill(Theme.accent)
                            .frame(width: 3, height: 22)
                        Spacer()
                    }
                    .frame(width: 44)
                }
                Image(systemName: symbol ?? section?.symbol ?? "circle")
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(active ? Theme.accent : Theme.muted)
                    .frame(width: 36, height: 36)
            }
        }
        .buttonStyle(.plain)
        .help(section?.label ?? "Reveal artifacts in Finder")
    }
}
