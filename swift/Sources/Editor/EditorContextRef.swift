// EditorContextRef.swift — a "selection → chat" context reference (PR-033).
//
// When the user runs "Add selection to chat" the editor captures a ContextRef:
// the workspace-relative path, the 1-based inclusive line range of the
// selection, and a content hash of the SELECTED TEXT at capture time. The ref is
// a value type so it can be carried into the conversation composer without
// coupling the editor to the conversation store.
//
// A ref is STALE when the file's selected lines no longer hash to the captured
// `contentHash` (the underlying text moved on after the ref was attached). A
// stale ref is shown as such rather than silently pointing at the wrong code —
// the honesty invariant for context references.

import Foundation

/// An inclusive 1-based line range within a file.
struct EditorLineRange: Hashable, Sendable {
    let start: Int
    let end: Int

    init(start: Int, end: Int) {
        // Normalize so `start <= end` and both are at least 1.
        let lo = max(1, min(start, end))
        let hi = max(1, max(start, end))
        self.start = lo
        self.end = hi
    }

    var lineCount: Int { end - start + 1 }

    /// A compact "L12" / "L12–L20" label for display.
    var label: String {
        start == end ? "L\(start)" : "L\(start)–L\(end)"
    }

    /// Backend wire label for `editor://` refs. Keep this ASCII-only and stable:
    /// the Rust resolver accepts `L1` or `L1-L2`; tests and fixtures use
    /// `L1-L1` even for single-line refs.
    var wireLabel: String {
        "L\(start)-L\(end)"
    }
}

/// A reference from an editor selection to a chat message. Carries enough to
/// re-locate the code and to detect staleness, never the secret-bearing bytes.
struct EditorContextRef: Hashable, Sendable, Identifiable {
    let id: UUID
    let workspaceRelativePath: String
    let lineRange: EditorLineRange
    /// Hash of the SELECTED text at capture time (`fnv1a64:` form).
    let contentHash: String
    let displayName: String

    init(
        id: UUID = UUID(),
        workspaceRelativePath: String,
        lineRange: EditorLineRange,
        contentHash: String,
        displayName: String
    ) {
        self.id = id
        self.workspaceRelativePath = workspaceRelativePath
        self.lineRange = lineRange
        self.contentHash = contentHash
        self.displayName = displayName
    }

    /// Slice the 1-based inclusive `lineRange` out of `fullText`, returning the
    /// joined selected lines (or nil if the range no longer fits the file).
    static func selectedText(in fullText: String, range: EditorLineRange) -> String? {
        let lines = fullText.components(separatedBy: "\n")
        guard range.start >= 1, range.end <= lines.count else { return nil }
        let slice = lines[(range.start - 1)...(range.end - 1)]
        return slice.joined(separator: "\n")
    }

    /// Build a ref from a document's CURRENT text + a selected line range. The
    /// captured hash is over the selected lines so a later edit anywhere in those
    /// lines makes the ref stale.
    static func capture(
        workspaceRelativePath: String,
        displayName: String,
        fullText: String,
        lineRange: EditorLineRange
    ) -> EditorContextRef? {
        guard let selected = selectedText(in: fullText, range: lineRange) else { return nil }
        return EditorContextRef(
            workspaceRelativePath: workspaceRelativePath,
            lineRange: lineRange,
            contentHash: EditorContentHash.compute(selected),
            displayName: displayName
        )
    }

    /// Compute the CURRENT hash of this ref's lines against the live file text.
    /// nil when the range no longer fits (also a form of staleness).
    func currentHash(in fullText: String) -> String? {
        guard let selected = Self.selectedText(in: fullText, range: lineRange) else { return nil }
        return EditorContentHash.compute(selected)
    }

    /// A ref is fresh only when its lines still hash to the captured value.
    func isStale(against fullText: String) -> Bool {
        currentHash(in: fullText) != contentHash
    }

    /// The compact ref consumed by the daemon context-pack builder. It carries
    /// only path/range/hash, never the selected source bytes.
    var wireReference: String {
        "editor://\(workspaceRelativePath)#\(lineRange.wireLabel)#\(contentHash)"
    }

    var contextLabel: String {
        "\(displayName) \(lineRange.label)"
    }
}
