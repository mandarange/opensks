// Theme.swift — semantic color aliases over `GeneratedDesignTokens` (compiled
// from the `opensks-studio-dark` design package, .opensks/design-systems/...).
// One teal accent carries brand/action/ready/pass; violet is reserved for the
// mark + keywords; gold/coral are the honesty palette only. Hierarchy comes from
// a four-plane elevation ladder + real materials, not borders-on-everything.
// Remaining literals are app-local and migrate to tokens in later PRs.

import SwiftUI

extension Color {
    init(hex: String) {
        let raw = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        var value: UInt64 = 0
        Scanner(string: raw).scanHexInt64(&value)
        let r, g, b, a: Double
        if raw.count == 8 {
            r = Double((value >> 24) & 0xFF) / 255
            g = Double((value >> 16) & 0xFF) / 255
            b = Double((value >> 8) & 0xFF) / 255
            a = Double(value & 0xFF) / 255
        } else {
            r = Double((value >> 16) & 0xFF) / 255
            g = Double((value >> 8) & 0xFF) / 255
            b = Double(value & 0xFF) / 255
            a = 1
        }
        self.init(.sRGB, red: r, green: g, blue: b, opacity: a)
    }
}

enum Theme {
    // Surfaces — elevation ladder.
    static let bg = GeneratedDesignTokens.colorCanvas
    static let sidebar = GeneratedDesignTokens.colorSurfaceSidebar
    static let explorer = Color(hex: "0F1116")
    static let panel = GeneratedDesignTokens.colorSurfaceBase
    static let panelDeep = Color(hex: "0F1116")
    static let input = GeneratedDesignTokens.colorSurfaceRaised
    static let editor = Color(hex: "0E1015")
    static let gutter = Color(hex: "0C0E12")
    static let terminal = Color(hex: "0C0E12")
    static let titlebarTop = Color(hex: "1B1E24")
    static let titlebarBottom = Color(hex: "15171C")
    static let currentLine = Color(hex: "14181E")

    // Strokes / seams.
    static let stroke = GeneratedDesignTokens.colorBorderSubtle
    static let strokeSoft = GeneratedDesignTokens.colorBorderStrong
    static let seam = Color(hex: "2A605A")
    /// Keyboard-focus ring token (the generated `colorFocus`). Used by previews of
    /// the focused control state so focus is shown by a ring, not colour alone.
    static let focusRing = GeneratedDesignTokens.colorFocus

    // Text.
    static let text = GeneratedDesignTokens.colorTextPrimary
    static let textSoft = GeneratedDesignTokens.colorTextSecondary
    static let muted = GeneratedDesignTokens.colorTextMuted
    // Contrast-gated (recovery directive §17.3/§17.4): faint hints and gutter
    // line numbers carry readable info, so they must clear 4.5:1 on their dark
    // surfaces. The previous #5A6473 (3.18:1) and #4A5260 (2.45:1) failed; these
    // targets pass (verified by ContrastTests).
    static let faint = Color(hex: "7E8A9B")
    static let gutterText = Color(hex: "8D98A8")

    // Accent + semantic.
    static let accent = GeneratedDesignTokens.colorAccentPrimary
    static let accentInk = Color(hex: "09100F")
    static let accentTint = Color(hex: "5EDEC422")
    static let accentSeam = Color(hex: "3A6B64")
    static let violet = GeneratedDesignTokens.colorAccentSecondary
    static let blue = GeneratedDesignTokens.colorStatusRunning
    static let green = Color(hex: "7FD98C")
    static let gold = GeneratedDesignTokens.colorStatusWarning
    static let coral = GeneratedDesignTokens.colorStatusDanger

    // Honesty palette aliases.
    static let pass = accent
    static let partial = gold
    static let fail = coral

    // Radii.
    static let rSm: CGFloat = 7
    static let rMd: CGFloat = 9
    static let rLg: CGFloat = 12
    static let rXl: CGFloat = 14

    // Spacing.
    static let s4: CGFloat = 4
    static let s6: CGFloat = 6
    static let s8: CGFloat = 8
    static let s10: CGFloat = 10
    static let s12: CGFloat = 12
    static let s16: CGFloat = 16
    static let s20: CGFloat = 20
    static let s24: CGFloat = 24

    // Type ramp.
    static func ui(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .default)
    }
    static func mono(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }
    static let display = Font.system(size: 27, weight: .semibold, design: .default)
}
