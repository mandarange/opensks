// Components.swift — shared building blocks: real macOS vibrancy, the brand
// mark, and the small widgets that give the app its calm, elevated language.

import SwiftUI
import AppKit

/// Real `NSVisualEffectView` vibrancy behind the window chrome.
struct VibrantBackground: NSViewRepresentable {
    var material: NSVisualEffectView.Material = .underWindowBackground

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }
    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = material
    }
}

/// The OpenSKS mark, rendered from the canonical bundled logo asset.
/// Thin wrapper over `OpenSKSLogoView` kept so existing call sites compile; the
/// former synthetic gradient + SF Symbol substitute has been removed (PR-021).
struct AgentMark: View {
    var size: CGFloat
    var body: some View {
        OpenSKSLogoView(size: size)
    }
}

struct StatusDot: View {
    var color: Color
    var pulse: Bool = false
    var size: CGFloat = 7

    var body: some View {
        Group {
            if pulse {
                TimelineView(.animation) { context in
                    let t = context.date.timeIntervalSinceReferenceDate
                    let a = 0.55 + 0.45 * (0.5 + 0.5 * sin(t * 3.0))
                    Circle().fill(color).opacity(a)
                }
            } else {
                Circle().fill(color)
            }
        }
        .frame(width: size, height: size)
    }
}

struct StatePill: View {
    var label: String
    var color: Color
    var pulse: Bool = false

    var body: some View {
        HStack(spacing: 6) {
            StatusDot(color: color, pulse: pulse)
            Text(label)
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.textSoft)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(Capsule().fill(Theme.input))
        .overlay(Capsule().strokeBorder(Theme.stroke, lineWidth: 1))
    }
}

struct Chip: View {
    var text: String
    var color: Color = Theme.textSoft
    var body: some View {
        Text(text)
            .font(Theme.ui(11, .semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(Capsule().fill(Theme.input))
            .overlay(Capsule().strokeBorder(Theme.stroke, lineWidth: 1))
    }
}

struct SectionHeader: View {
    var title: String
    var trailing: String? = nil
    var body: some View {
        HStack {
            Text(title.uppercased())
                .font(Theme.ui(10.5, .semibold))
                .tracking(0.8)
                .foregroundStyle(Theme.muted)
            Spacer()
            if let trailing {
                Text(trailing)
                    .font(Theme.ui(10.5, .medium))
                    .foregroundStyle(Theme.faint)
            }
        }
    }
}

struct PrimaryButton: View {
    var title: String
    var systemImage: String? = nil
    var enabled: Bool = true
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if let systemImage { Image(systemName: systemImage).font(.system(size: 11, weight: .bold)) }
                Text(title).font(Theme.ui(12.5, .semibold))
            }
            .foregroundStyle(Theme.accentInk)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .background(
                RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                    .fill(LinearGradient(colors: [Theme.accent, Color(hex: "4FD3B6")],
                                         startPoint: .top, endPoint: .bottom))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(enabled ? 1 : 0.4)
        .disabled(!enabled)
    }
}

struct GhostButton: View {
    var title: String
    var systemImage: String? = nil
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if let systemImage { Image(systemName: systemImage).font(.system(size: 11, weight: .medium)) }
                Text(title).font(Theme.ui(12.5, .medium))
            }
            .foregroundStyle(Theme.textSoft)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous).fill(Theme.input))
            .overlay(RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous).strokeBorder(Theme.stroke, lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// A custom segmented control with a sliding selection tile.
struct SegmentedControl: View {
    var options: [String]
    @Binding var selection: Int

    var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(options.enumerated()), id: \.offset) { idx, label in
                let active = idx == selection
                Button {
                    withAnimation(.easeOut(duration: 0.14)) { selection = idx }
                } label: {
                    Text(label)
                        .font(Theme.ui(11.5, .semibold))
                        .foregroundStyle(active ? Theme.accent : Theme.muted)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 5)
                        .background(
                            RoundedRectangle(cornerRadius: Theme.rSm - 1, style: .continuous)
                                .fill(active ? Theme.accentTint : Color.clear)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: Theme.rSm - 1, style: .continuous)
                                .strokeBorder(active ? Theme.accentSeam : Color.clear, lineWidth: 1)
                        )
                }
                .buttonStyle(.plain)
            }
        }
        .padding(3)
        .background(RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous).fill(Theme.panelDeep))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous).strokeBorder(Theme.stroke, lineWidth: 1))
    }
}

struct MetricCallout: View {
    var value: String
    var label: String
    var accent: Color = Theme.text

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(value)
                .font(.system(size: 25, weight: .semibold, design: .default))
                .monospacedDigit()
                .foregroundStyle(accent)
            Text(label)
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
        }
    }
}

/// A 1pt vertical divider with a wider drag affordance for resizing a pane.
struct DragDivider: View {
    @Binding var width: CGFloat
    var range: ClosedRange<CGFloat>
    /// Set when the divider sits on the *leading* edge of the bound pane, so
    /// dragging right shrinks it.
    var invert: Bool = false
    @State private var base: CGFloat?

    var body: some View {
        Rectangle()
            .fill(Theme.stroke)
            .frame(width: 1)
            .overlay(
                Color.clear
                    .frame(width: 9)
                    .contentShape(Rectangle())
                    .onHover { inside in
                        if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
                    }
                    .gesture(
                        DragGesture()
                            .onChanged { value in
                                let start = base ?? width
                                if base == nil { base = start }
                                let delta = invert ? -value.translation.width : value.translation.width
                                width = min(max(start + delta, range.lowerBound), range.upperBound)
                            }
                            .onEnded { _ in base = nil }
                    )
            )
    }
}

/// The honest, segmented proof bar (passed / partial / failed).
struct ProofBar: View {
    var passed: Int
    var partial: Int
    var failed: Int

    var body: some View {
        GeometryReader { geo in
            let total = max(1, passed + partial + failed)
            let w = geo.size.width
            HStack(spacing: 0) {
                Rectangle().fill(Theme.pass).frame(width: w * CGFloat(passed) / CGFloat(total))
                Rectangle().fill(Theme.partial).frame(width: w * CGFloat(partial) / CGFloat(total))
                Rectangle().fill(Theme.fail).frame(width: w * CGFloat(failed) / CGFloat(total))
                if failed == 0 && partial == 0 && passed == 0 {
                    Rectangle().fill(Theme.input)
                }
            }
        }
        .frame(height: 7)
        .clipShape(Capsule())
        .background(Capsule().fill(Theme.input))
    }
}
