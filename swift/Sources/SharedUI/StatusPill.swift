// StatusPill.swift — a compact status indicator. State is shown with BOTH a
// glyph and a tint (never color alone), per the accessibility rule.

import SwiftUI

struct StatusPill: View {
    enum Kind {
        case neutral, success, warning, danger, running

        var tint: Color {
            switch self {
            case .neutral: return Theme.muted
            case .success: return GeneratedDesignTokens.colorStatusSuccess
            case .warning: return GeneratedDesignTokens.colorStatusWarning
            case .danger: return GeneratedDesignTokens.colorStatusDanger
            case .running: return GeneratedDesignTokens.colorStatusRunning
            }
        }

        var symbol: String {
            switch self {
            case .neutral: return "circle"
            case .success: return "checkmark.circle.fill"
            case .warning: return "exclamationmark.triangle.fill"
            case .danger: return "xmark.octagon.fill"
            case .running: return "arrow.triangle.2.circlepath"
            }
        }
    }

    let kind: Kind
    let label: String

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: kind.symbol)
                .font(.system(size: 9, weight: .bold))
            Text(label)
                .font(Theme.ui(11, .semibold))
        }
        .foregroundStyle(kind.tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(kind.tint.opacity(0.14)))
        .overlay(Capsule().strokeBorder(kind.tint.opacity(0.3), lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label) status")
    }
}
