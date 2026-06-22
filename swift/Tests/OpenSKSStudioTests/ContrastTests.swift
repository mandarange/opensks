import AppKit
import SwiftUI
import XCTest

@testable import OpenSKSStudio

/// WCAG contrast gates for the dark theme (recovery directive §17.4). Reads the
/// REAL `Theme` colors (via NSColor) and computes the contrast ratio, so a token
/// that regresses below its gate fails CI. Visual rendering acceptance still
/// happens at the GUI gate; this locks the deterministic contrast math.
final class ContrastTests: XCTestCase {
    private func relativeLuminance(_ color: Color) -> Double {
        let ns = NSColor(color).usingColorSpace(.sRGB) ?? .black
        func channel(_ c: CGFloat) -> Double {
            let c = Double(c)
            return c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * channel(ns.redComponent)
            + 0.7152 * channel(ns.greenComponent)
            + 0.0722 * channel(ns.blueComponent)
    }

    private func ratio(_ fg: Color, on bg: Color) -> Double {
        let a = relativeLuminance(fg)
        let b = relativeLuminance(bg)
        let (hi, lo) = a > b ? (a, b) : (b, a)
        return (hi + 0.05) / (lo + 0.05)
    }

    func testReadableTextMeetsNormalTextGate() {
        // Normal body/metadata text must clear 4.5:1 on its surface.
        XCTAssertGreaterThanOrEqual(ratio(Theme.text, on: Theme.bg), 4.5)
        XCTAssertGreaterThanOrEqual(ratio(Theme.textSoft, on: Theme.bg), 4.5)
        XCTAssertGreaterThanOrEqual(ratio(Theme.muted, on: Theme.bg), 4.5)
        // faint hints and gutter line numbers also carry readable info.
        XCTAssertGreaterThanOrEqual(ratio(Theme.faint, on: Theme.bg), 4.5)
        XCTAssertGreaterThanOrEqual(ratio(Theme.gutterText, on: Theme.gutter), 4.5)
    }

    func testAccentMeetsLargeOrFocusGate() {
        // The accent is used for large/active affordances and focus — 3:1.
        XCTAssertGreaterThanOrEqual(ratio(Theme.accent, on: Theme.bg), 3.0)
    }
}
