// Theme.swift — all design tokens in code (no asset catalog).
// One teal accent carries brand/action/ready/pass; violet is reserved for the
// mark + keywords; gold/coral are the honesty palette only. Hierarchy comes from
// a four-plane elevation ladder + real materials, not borders-on-everything.

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
    static let bg = Color(hex: "0E1015")
    static let sidebar = Color(hex: "101216")
    static let explorer = Color(hex: "0F1116")
    static let panel = Color(hex: "13161B")
    static let panelDeep = Color(hex: "0F1116")
    static let input = Color(hex: "181B21")
    static let editor = Color(hex: "0E1015")
    static let gutter = Color(hex: "0C0E12")
    static let terminal = Color(hex: "0C0E12")
    static let titlebarTop = Color(hex: "1B1E24")
    static let titlebarBottom = Color(hex: "15171C")
    static let currentLine = Color(hex: "14181E")

    // Strokes / seams.
    static let stroke = Color(hex: "262A32")
    static let strokeSoft = Color(hex: "2C313A")
    static let seam = Color(hex: "2A605A")

    // Text.
    static let text = Color(hex: "E9EDF3")
    static let textSoft = Color(hex: "BCC4D0")
    static let muted = Color(hex: "7E8796")
    static let faint = Color(hex: "5A6473")
    static let gutterText = Color(hex: "4A5260")

    // Accent + semantic.
    static let accent = Color(hex: "5EDEC4")
    static let accentInk = Color(hex: "09100F")
    static let accentTint = Color(hex: "5EDEC422")
    static let accentSeam = Color(hex: "3A6B64")
    static let violet = Color(hex: "9D8EF5")
    static let blue = Color(hex: "70B0F4")
    static let green = Color(hex: "7FD98C")
    static let gold = Color(hex: "E0B25C")
    static let coral = Color(hex: "E0876E")

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
