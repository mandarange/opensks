// ButtonStyles.swift — the unified interaction system (PR-023).
//
// Every primary/secondary action uses a shared ButtonStyle so the WHOLE visible
// tile is the hit target (contentShape), with consistent hover / pressed /
// disabled states. This fixes the "only the glyph/text feels clickable" defect.
// Keyboard focus uses the system focus ring.

import SwiftUI

/// The primary interactive surface style.
struct SurfaceButtonStyle: ButtonStyle {
    enum Emphasis { case primary, secondary, quiet, destructive }

    var emphasis: Emphasis = .secondary
    var minHeight: CGFloat = 40

    func makeBody(configuration: Configuration) -> some View {
        SurfaceButtonBody(configuration: configuration, emphasis: emphasis, minHeight: minHeight)
    }
}

private struct SurfaceButtonBody: View {
    let configuration: ButtonStyle.Configuration
    let emphasis: SurfaceButtonStyle.Emphasis
    let minHeight: CGFloat
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        configuration.label
            .font(Theme.ui(12.5, .semibold))
            .frame(maxWidth: .infinity, minHeight: minHeight)
            .padding(.horizontal, 12)
            .foregroundStyle(foreground)
            .background(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                    .fill(fill(pressed: configuration.isPressed))
            )
            .overlay(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                    .strokeBorder(stroke, lineWidth: 1)
            )
            .contentShape(Rectangle())
            .opacity(isEnabled ? 1 : 0.45)
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .onHover { hovering = isEnabled && $0 }
            .animation(.easeOut(duration: 0.12), value: hovering)
    }

    private var foreground: Color {
        switch emphasis {
        case .primary: return Theme.accentInk
        case .secondary, .quiet: return Theme.textSoft
        case .destructive: return Theme.coral
        }
    }

    private func fill(pressed: Bool) -> Color {
        let base: Color
        switch emphasis {
        case .primary: base = Theme.accent
        case .secondary: base = Theme.input
        case .quiet: return hovering ? Theme.panel : Color.clear
        case .destructive: base = Theme.input
        }
        if pressed { return base.opacity(0.85) }
        if hovering { return base.opacity(0.92) }
        return base
    }

    private var stroke: Color {
        switch emphasis {
        case .primary: return Color.clear
        case .secondary, .quiet: return hovering ? Theme.strokeSoft : Theme.stroke
        case .destructive: return Theme.coral.opacity(0.5)
        }
    }
}

extension ButtonStyle where Self == SurfaceButtonStyle {
    static var primaryAction: SurfaceButtonStyle { SurfaceButtonStyle(emphasis: .primary, minHeight: 40) }
    static var secondaryAction: SurfaceButtonStyle { SurfaceButtonStyle(emphasis: .secondary, minHeight: 40) }
    static var quietAction: SurfaceButtonStyle { SurfaceButtonStyle(emphasis: .quiet, minHeight: 36) }
    static var destructiveAction: SurfaceButtonStyle { SurfaceButtonStyle(emphasis: .destructive, minHeight: 40) }
}

/// Dense toolbar icon button: small glyph, but a >=30pt interactive frame.
struct ToolbarButtonStyle: ButtonStyle {
    var minSize: CGFloat = 30
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(minWidth: minSize, minHeight: minSize)
            .contentShape(Rectangle())
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// Square icon tile; the whole tile is the hit target.
struct IconTileButtonStyle: ButtonStyle {
    var size: CGFloat = 32
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(width: size, height: size)
            .background(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                    .fill(configuration.isPressed ? Theme.accentTint : Color.clear)
            )
            .contentShape(Rectangle())
    }
}

/// Full-width, leading-aligned list row button (the entire row is hittable).
struct ListRowButtonStyle: ButtonStyle {
    var minHeight: CGFloat = 30
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity, minHeight: minHeight, alignment: .leading)
            .background(configuration.isPressed ? Theme.accentTint : Color.clear)
            .contentShape(Rectangle())
    }
}
