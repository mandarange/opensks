import SwiftUI

struct TerminalRiskBadge: View {
    let risk: TerminalRiskLevel
    var requiresApproval: Bool

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: symbol)
                .font(.system(size: 9, weight: .bold))
            Text(risk.displayLabel)
                .font(Theme.ui(10.5, .semibold))
                .lineLimit(1)
            if requiresApproval {
                Image(systemName: "lock.fill")
                    .font(.system(size: 8.5, weight: .bold))
            }
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(Capsule().fill(tint.opacity(0.13)))
        .overlay(Capsule().strokeBorder(tint.opacity(0.32), lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(risk.displayLabel) risk")
    }

    private var tint: Color {
        switch risk {
        case .safe:
            return Theme.green
        case .caution:
            return Theme.gold
        case .privileged, .networkMutation:
            return Theme.blue
        case .destructive, .secretExposure:
            return Theme.coral
        case .unknown:
            return Theme.gold
        }
    }

    private var symbol: String {
        switch risk {
        case .safe:
            return "checkmark.circle.fill"
        case .caution:
            return "exclamationmark.triangle.fill"
        case .privileged:
            return "lock.shield.fill"
        case .networkMutation:
            return "network"
        case .destructive:
            return "xmark.octagon.fill"
        case .secretExposure:
            return "eye.slash.fill"
        case .unknown:
            return "questionmark.circle.fill"
        }
    }
}
