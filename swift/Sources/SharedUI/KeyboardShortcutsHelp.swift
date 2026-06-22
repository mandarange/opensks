// KeyboardShortcutsHelp.swift — a discoverable keyboard-shortcuts reference
// (PR-045). The app is keyboard-navigable: every primary navigation route has a
// ⌘-number shortcut and the primary actions have their own bindings. This sheet
// is the single place a new operator can discover them, so the whole journey is
// reachable without the CLI or a mouse.
//
// Opened with ⌘/ (or the titlebar "?" affordance). Grouped, token-driven, dark.

import SwiftUI

/// One documented shortcut: a human label + its key-equivalent rendered as caps.
struct ShortcutItem: Identifiable {
    let id = UUID()
    let label: String
    let keys: [String]
}

/// A titled group of shortcuts (Navigation / Actions / Editor).
struct ShortcutGroup: Identifiable {
    let id = UUID()
    let title: String
    let items: [ShortcutItem]
}

enum KeyboardShortcuts {
    /// The catalog the help sheet renders. The navigation group is derived from the
    /// route order so it stays in sync with the rail (⌘1 = first route, …).
    static var catalog: [ShortcutGroup] {
        [
            ShortcutGroup(title: "Navigation", items: navigationItems),
            ShortcutGroup(title: "Actions", items: [
                ShortcutItem(label: "Command palette", keys: ["⌘", "K"]),
                ShortcutItem(label: "Keyboard shortcuts", keys: ["⌘", "/"]),
                ShortcutItem(label: "Run acceptance audit", keys: ["⌘", "R"]),
                ShortcutItem(label: "Focus the composer", keys: ["⌘", "L"]),
            ]),
            ShortcutGroup(title: "Editor", items: [
                ShortcutItem(label: "Save file", keys: ["⌘", "S"]),
                ShortcutItem(label: "Save all files", keys: ["⌥", "⌘", "S"]),
                ShortcutItem(label: "Close file", keys: ["⌘", "W"]),
                ShortcutItem(label: "Find in file", keys: ["⌘", "F"]),
            ]),
        ]
    }

    /// Navigation shortcuts, derived from the route order. The first nine routes get
    /// ⌘1…⌘9; later routes are reachable via the rail and the palette.
    static var navigationItems: [ShortcutItem] {
        WorkspaceRoute.allCases.enumerated().compactMap { index, route in
            guard index < 9 else { return nil }
            return ShortcutItem(label: "Go to \(route.label)", keys: ["⌘", "\(index + 1)"])
        }
    }

    /// The numeric key-equivalent for a route's ⌘-number shortcut, or nil if the
    /// route is beyond the first nine. Used by the rail to attach the binding.
    static func navigationKey(for route: WorkspaceRoute) -> Character? {
        guard let index = WorkspaceRoute.allCases.firstIndex(of: route), index < 9 else { return nil }
        return Character("\(index + 1)")
    }
}

struct KeyboardShortcutsHelpView: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().overlay(Theme.stroke)
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.s20) {
                    ForEach(KeyboardShortcuts.catalog) { group in
                        groupView(group)
                    }
                }
                .padding(Theme.s20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(width: 460, height: 520)
        .background(Theme.panel)
        .accessibilityIdentifier("help.shortcuts")
    }

    private var header: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "keyboard")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Theme.accent)
            Text("Keyboard Shortcuts")
                .font(Theme.ui(15, .semibold))
                .foregroundStyle(Theme.text)
            Spacer()
            Button { dismiss() } label: {
                Image(systemName: "xmark").font(.system(size: 11, weight: .bold)).foregroundStyle(Theme.muted)
            }
            .buttonStyle(.plain)
            .keyboardShortcut(.cancelAction)
            .accessibilityLabel("Close keyboard shortcuts")
        }
        .padding(Theme.s16)
    }

    private func groupView(_ group: ShortcutGroup) -> some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            Text(group.title.uppercased())
                .font(Theme.ui(10, .semibold))
                .foregroundStyle(Theme.faint)
            VStack(spacing: 2) {
                ForEach(group.items) { item in
                    row(item)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(group.title)
    }

    private func row(_ item: ShortcutItem) -> some View {
        HStack(spacing: Theme.s10) {
            Text(item.label)
                .font(Theme.ui(12.5))
                .foregroundStyle(Theme.textSoft)
            Spacer()
            HStack(spacing: 4) {
                ForEach(Array(item.keys.enumerated()), id: \.offset) { _, key in
                    keyCap(key)
                }
            }
        }
        .padding(.horizontal, Theme.s10)
        .padding(.vertical, 7)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(item.label): \(item.keys.joined(separator: " "))")
    }

    private func keyCap(_ key: String) -> some View {
        Text(key)
            .font(Theme.mono(11, .semibold))
            .foregroundStyle(Theme.text)
            .frame(minWidth: 22, minHeight: 22)
            .padding(.horizontal, 5)
            .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Theme.input))
            .overlay(RoundedRectangle(cornerRadius: Theme.rSm).strokeBorder(Theme.stroke, lineWidth: 1))
    }
}
