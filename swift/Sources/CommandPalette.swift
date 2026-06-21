// CommandPalette.swift — a ⌘K sheet to discover and run OpenSKS CLI verbs.

import SwiftUI

struct PaletteCommand: Identifiable {
    let id = UUID()
    let title: String
    let symbol: String
    let args: [String]
}

struct CommandPalette: View {
    @EnvironmentObject private var state: AppState
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""

    private let commands: [PaletteCommand] = [
        PaletteCommand(title: "Acceptance audit", symbol: "checkmark.seal", args: ["acceptance", "audit"]),
        PaletteCommand(title: "Provider check", symbol: "powerplug", args: ["provider", "adapter-check"]),
        PaletteCommand(title: "QA run", symbol: "checklist", args: ["qa", "run"]),
        PaletteCommand(title: "Security audit", symbol: "lock.shield", args: ["security", "audit"]),
        PaletteCommand(title: "Voxel index", symbol: "cube", args: ["voxel", "index"]),
        PaletteCommand(title: "PRD coverage", symbol: "doc.text", args: ["prd", "coverage"]),
        PaletteCommand(title: "Cache warm", symbol: "flame", args: ["cache", "warm"]),
        PaletteCommand(title: "Benchmark", symbol: "speedometer", args: ["bench"]),
        PaletteCommand(title: "Design QA", symbol: "paintbrush", args: ["design", "qa"]),
    ]

    private var filtered: [PaletteCommand] {
        guard !query.isEmpty else { return commands }
        return commands.filter { $0.title.lowercased().contains(query.lowercased()) }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.muted)
                TextField("Run a command…", text: $query)
                    .textFieldStyle(.plain)
                    .font(Theme.ui(14))
                    .foregroundStyle(Theme.text)
            }
            .padding(14)
            Divider().overlay(Theme.stroke)

            ScrollView {
                VStack(spacing: 2) {
                    ForEach(filtered) { command in
                        Button {
                            state.runVerb(label: command.title, args: command.args)
                            dismiss()
                        } label: {
                            HStack(spacing: 10) {
                                Image(systemName: command.symbol)
                                    .font(.system(size: 13))
                                    .foregroundStyle(Theme.accent)
                                    .frame(width: 20)
                                Text(command.title)
                                    .font(Theme.ui(13))
                                    .foregroundStyle(Theme.textSoft)
                                Spacer()
                                Text("opensks " + command.args.joined(separator: " "))
                                    .font(Theme.mono(10.5))
                                    .foregroundStyle(Theme.faint)
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 9)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                            .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Color.clear))
                        }
                        .buttonStyle(PaletteRowStyle())
                    }
                }
                .padding(8)
            }
        }
        .frame(width: 480, height: 400)
        .background(Theme.panel)
    }
}

private struct PaletteRowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm)
                    .fill(configuration.isPressed ? Theme.accentTint : Color.clear)
            )
    }
}
