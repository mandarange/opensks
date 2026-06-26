import SwiftUI

struct TerminalSuggestionView: View {
    let suggestion: TerminalSuggestionModel
    let onInsert: () -> Void
    let onRun: () -> Void
    let onExplain: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("AI suggestion")
                    .font(Theme.ui(11.5, .semibold))
                    .foregroundStyle(Theme.muted)
                TerminalRiskBadge(
                    risk: suggestion.risk,
                    requiresApproval: suggestion.requiresApproval
                )
                Spacer()
                Text(suggestion.source)
                    .font(Theme.mono(10.5))
                    .foregroundStyle(Theme.faint)
            }
            Text(suggestion.display)
                .font(Theme.mono(12.5, .semibold))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if !suggestion.description.isEmpty {
                Text("reason: \(suggestion.description)")
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.textSoft)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                Button(action: onInsert) {
                    Label("Insert", systemImage: "text.insert")
                }
                Button(action: onRun) {
                    Label("Run", systemImage: "play.fill")
                }
                .disabled(suggestion.risk.blockedByDefault)
                Button(action: onExplain) {
                    Label("Explain", systemImage: "questionmark.circle")
                }
                Spacer()
            }
            .buttonStyle(TerminalSmallButtonStyle())
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.stroke)
        )
    }
}

private struct TerminalSmallButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.ui(11, .semibold))
            .foregroundStyle(configuration.isPressed ? Theme.accentInk : Theme.text)
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .fill(configuration.isPressed ? Theme.accent : Theme.input)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .strokeBorder(Theme.stroke)
            )
    }
}
