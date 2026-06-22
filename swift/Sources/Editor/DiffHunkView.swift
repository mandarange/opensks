// DiffHunkView.swift — read-only rendering of unified-diff hunks (PR-033).
//
// One renderer drives two surfaces: the editor gutter markers (added/removed)
// and a full read-only diff pane (used for "Compare" in conflict resolution and
// for opening an agent's unified-diff patch). Change kind is shown with BOTH a
// glyph/sign AND a semantic tint (added/removed), never colour alone, per the
// accessibility rule. All colours are semantic design tokens; the surface is
// dark and fills its width (no fixed maxWidth → no letterbox).

import SwiftUI

// MARK: - Diff line model (presentation)

/// A single rendered diff line classified by its unified-diff prefix.
struct DiffDisplayLine: Identifiable, Equatable {
    enum Kind: Equatable {
        case added, removed, context, meta

        /// Semantic tint — a token, never a bare colour. Added reuses the
        /// success/accent token, removed the danger token.
        var tint: Color {
            switch self {
            case .added: return GeneratedDesignTokens.colorStatusSuccess
            case .removed: return GeneratedDesignTokens.colorStatusDanger
            case .context: return Theme.muted
            case .meta: return Theme.violet
            }
        }

        /// A glyph so the kind is legible without relying on colour.
        var sign: String {
            switch self {
            case .added: return "+"
            case .removed: return "-"
            case .context: return " "
            case .meta: return "@"
            }
        }

        var accessibilityWord: String {
            switch self {
            case .added: return "added"
            case .removed: return "removed"
            case .context: return "unchanged"
            case .meta: return "hunk header"
            }
        }
    }

    let id: Int
    let kind: Kind
    let text: String

    /// Classify one raw unified-diff line by its leading character.
    static func classify(_ raw: String, id: Int) -> DiffDisplayLine {
        if raw.hasPrefix("+") {
            return DiffDisplayLine(id: id, kind: .added, text: String(raw.dropFirst()))
        }
        if raw.hasPrefix("-") {
            return DiffDisplayLine(id: id, kind: .removed, text: String(raw.dropFirst()))
        }
        if raw.hasPrefix("@@") || raw.hasPrefix("@") {
            return DiffDisplayLine(id: id, kind: .meta, text: raw)
        }
        let body = raw.hasPrefix(" ") ? String(raw.dropFirst()) : raw
        return DiffDisplayLine(id: id, kind: .context, text: body)
    }
}

// MARK: - Gutter markers (per editor-buffer line)

/// The change marker the gutter draws next to a 1-based buffer line.
enum DiffGutterMarker: Equatable {
    case added
    case removed   // lines deleted at/after this point (no buffer line of their own)
    case changed

    /// Semantic tint — a token, never colour alone (the gutter also varies the
    /// glyph so the marker is legible to colour-blind users).
    var tint: Color {
        switch self {
        case .added: return GeneratedDesignTokens.colorStatusSuccess
        case .removed: return GeneratedDesignTokens.colorStatusDanger
        case .changed: return GeneratedDesignTokens.colorStatusWarning
        }
    }
}

enum DiffGutter {
    /// Map a decoded text-diff into per-buffer-line markers keyed by the 1-based
    /// NEW (buffer) line number. Added lines mark their own line; a hunk that
    /// only removes lines marks the line it sits before so the deletion is still
    /// visible; mixed hunks mark their changed buffer lines.
    static func markers(from response: TextDiffResponse) -> [Int: DiffGutterMarker] {
        var out: [Int: DiffGutterMarker] = [:]
        for hunk in response.hunks {
            let hasAdds = hunk.lines.contains { $0.hasPrefix("+") }
            let hasRemoves = hunk.lines.contains { $0.hasPrefix("-") }
            if hunk.newLines == 0 || (!hasAdds && hasRemoves) {
                // Pure deletion: mark the anchor buffer line.
                out[max(1, hunk.newStart)] = .removed
                continue
            }
            let kind: DiffGutterMarker = (hasAdds && hasRemoves) ? .changed
                : (hunk.kind == .removed ? .removed : .added)
            let upper = max(hunk.newStart, hunk.newStart + hunk.newLines - 1)
            for line in hunk.newStart...upper where line >= 1 {
                // Don't downgrade a `.changed` already recorded for this line.
                if out[line] == nil { out[line] = kind }
            }
        }
        return out
    }
}

enum DiffPresentation {
    /// Flatten decoded text-diff hunks into renderable lines with hunk headers.
    static func lines(from response: TextDiffResponse) -> [DiffDisplayLine] {
        var out: [DiffDisplayLine] = []
        var id = 0
        for hunk in response.hunks {
            let header = "@@ -\(hunk.oldStart),\(hunk.oldLines) +\(hunk.newStart),\(hunk.newLines) @@"
            out.append(DiffDisplayLine(id: id, kind: .meta, text: header)); id += 1
            for raw in hunk.lines {
                out.append(DiffDisplayLine.classify(raw, id: id)); id += 1
            }
        }
        return out
    }

    /// Parse a raw unified-diff patch (e.g. an agent's patch) into lines.
    static func lines(fromUnifiedPatch patch: String) -> [DiffDisplayLine] {
        patch
            .components(separatedBy: "\n")
            .enumerated()
            .map { DiffDisplayLine.classify($0.element, id: $0.offset) }
    }
}

// MARK: - Read-only diff pane

/// A scrollable read-only diff view. Reused by the conflict "Compare" action and
/// by the agent-patch open-in-editor hook.
struct DiffHunkView: View {
    let title: String
    let lines: [DiffDisplayLine]

    init(title: String, lines: [DiffDisplayLine]) {
        self.title = title
        self.lines = lines
    }

    init(title: String, response: TextDiffResponse) {
        self.init(title: title, lines: DiffPresentation.lines(from: response))
    }

    init(title: String, unifiedPatch: String) {
        self.init(title: title, lines: DiffPresentation.lines(fromUnifiedPatch: unifiedPatch))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().overlay(Theme.stroke)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(lines) { line in
                        row(line)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Theme.editor)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.panel)
        .accessibilityIdentifier("editor.diff.view")
    }

    private var header: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "plusminus")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.muted)
            Text(title)
                .font(Theme.ui(11.5, .semibold))
                .foregroundStyle(Theme.textSoft)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.sidebar)
    }

    private func row(_ line: DiffDisplayLine) -> some View {
        HStack(alignment: .top, spacing: Theme.s8) {
            Text(line.kind.sign)
                .font(Theme.mono(11.5, .bold))
                .foregroundStyle(line.kind.tint)
                .frame(width: 12, alignment: .center)
            Text(line.text.isEmpty ? " " : line.text)
                .font(Theme.mono(11.5))
                .foregroundStyle(line.kind == .context ? Theme.textSoft : line.kind.tint)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, 1)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(rowBackground(line.kind))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(line.kind.accessibilityWord): \(line.text)")
    }

    private func rowBackground(_ kind: DiffDisplayLine.Kind) -> Color {
        switch kind {
        case .added: return GeneratedDesignTokens.colorStatusSuccess.opacity(0.10)
        case .removed: return GeneratedDesignTokens.colorStatusDanger.opacity(0.10)
        case .meta: return Theme.violet.opacity(0.08)
        case .context: return Color.clear
        }
    }
}
