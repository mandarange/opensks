import SwiftUI

struct TerminalCommandBlockView: View {
    let block: TerminalCommandBlockModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text("$")
                    .font(Theme.mono(12, .bold))
                    .foregroundStyle(Theme.accent)
                Text(block.commandRedacted)
                    .font(Theme.mono(12, .semibold))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .lineLimit(2)
                if block.redacted {
                    TerminalRiskBadge(risk: .secretExposure, requiresApproval: false)
                }
                Spacer()
                exitLabel
            }
            Text(block.outputPreview)
                .font(Theme.mono(11.5))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.panelDeep)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.stroke)
        )
    }

    @ViewBuilder private var exitLabel: some View {
        if let exitCode = block.exitCode {
            Text("exit: \(exitCode)")
                .font(Theme.mono(10.5, .semibold))
                .foregroundStyle(exitCode == 0 ? Theme.green : Theme.coral)
        } else if block.isRunning {
            Text("pending")
                .font(Theme.mono(10.5, .semibold))
                .foregroundStyle(Theme.gold)
        }
    }
}
