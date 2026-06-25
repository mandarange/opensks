// CodeEditorRepresentable.swift — the real editable text surface (PR-032).
//
// An NSViewRepresentable hosting a TextKit 2 NSTextView (created with
// `usingTextLayoutManager: true`). It is a code editor, not a rich-text field:
// monospaced font, plain text, undo enabled, NO smart substitutions/quotes that
// would corrupt source. A line-number ruler tracks the gutter; per-edit syntax
// highlighting reuses `SyntaxHighlighter`. Edits flow up through the coordinator
// to `EditorDocumentState.textDidChange`. Selection is preserved across external
// text updates so a re-render never yanks the cursor. The view fills its parent
// (maxWidth/maxHeight .infinity) so there is no letterbox. Secret / binary /
// oversized documents bind read-only behind a banner.

import SwiftUI
import AppKit

struct CodeEditorRepresentable: NSViewRepresentable {
    @ObservedObject var document: EditorDocumentState
    /// Per-buffer-line diff markers (added/removed/changed) for the gutter. Empty
    /// when there is no on-disk-vs-buffer difference to surface.
    var diffMarkers: [Int: DiffGutterMarker] = [:]

    func makeCoordinator() -> Coordinator {
        Coordinator(document: document)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.drawsBackground = true
        scrollView.backgroundColor = NSColor(Theme.editor)
        scrollView.translatesAutoresizingMaskIntoConstraints = true

        // TextKit 2 stack.
        let textView = NSTextView(usingTextLayoutManager: true)
        textView.isEditable = document.isEditable
        textView.isSelectable = true
        textView.isRichText = false
        textView.allowsUndo = true
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isContinuousSpellCheckingEnabled = false
        textView.isGrammarCheckingEnabled = false
        textView.smartInsertDeleteEnabled = false
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true
        textView.font = NSFont.monospacedSystemFont(ofSize: 12.5, weight: .regular)
        textView.textColor = NSColor(Theme.text)
        textView.backgroundColor = NSColor(Theme.editor)
        textView.drawsBackground = true
        textView.insertionPointColor = NSColor(Theme.accent)
        textView.textContainerInset = NSSize(width: 6, height: 8)
        textView.delegate = context.coordinator

        // Full-width: track the container width, grow vertically.
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.minSize = NSSize(width: 0, height: 0)
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude,
                                  height: CGFloat.greatestFiniteMagnitude)
        if let container = textView.textContainer {
            container.widthTracksTextView = true
            container.containerSize = NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        }

        scrollView.documentView = textView

        // Line-number gutter ruler.
        let ruler = LineNumberRulerView(textView: textView)
        ruler.diffMarkers = diffMarkers
        scrollView.verticalRulerView = ruler
        scrollView.hasVerticalRuler = true
        scrollView.rulersVisible = true

        context.coordinator.textView = textView
        context.coordinator.rulerView = ruler

        // Seed content + initial highlight.
        context.coordinator.applyExternalText(document.text)
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        context.coordinator.document = document
        guard let textView = context.coordinator.textView else { return }

        textView.isEditable = document.isEditable

        // Push external text only when it actually diverges from the buffer
        // (e.g. a conflict "take disk" reload). This preserves the cursor for
        // normal typing because typing already updated the buffer.
        if textView.string != document.text {
            context.coordinator.applyExternalText(document.text)
        }

        // Refresh gutter diff markers (added/removed) when they change.
        if let ruler = context.coordinator.rulerView, ruler.diffMarkers != diffMarkers {
            ruler.diffMarkers = diffMarkers
            ruler.needsDisplay = true
        }
    }

    // MARK: - Coordinator

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var document: EditorDocumentState
        weak var textView: NSTextView?
        weak var rulerView: LineNumberRulerView?
        private var isApplyingExternal = false

        init(document: EditorDocumentState) {
            self.document = document
        }

        /// Replace the whole buffer (open / conflict reload) without firing the
        /// dirty path, preserving the selection where possible.
        func applyExternalText(_ text: String) {
            guard let textView else { return }
            isApplyingExternal = true
            let previousSelection = textView.selectedRange()
            textView.string = text
            let clamped = NSRange(
                location: min(previousSelection.location, (text as NSString).length),
                length: 0
            )
            textView.setSelectedRange(clamped)
            highlight()
            isApplyingExternal = false
            updateSelection(from: textView)
            rulerView?.needsDisplay = true
        }

        /// Coalesces rapid keystrokes into one re-highlight (EDIT-001).
        private var pendingHighlight: DispatchWorkItem?
        /// Above this length syntax highlighting is skipped (large-file mode,
        /// §14.4) so typing stays within the §23.5 budget.
        private let largeFileHighlightLimit = 1_000_000

        func textDidChange(_ notification: Notification) {
            guard !isApplyingExternal, let textView else { return }
            let newText = textView.string
            document.textDidChange(newText)
            updateSelection(from: textView)
            rulerView?.needsDisplay = true
            scheduleHighlight()
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !isApplyingExternal, let textView else { return }
            updateSelection(from: textView)
        }

        private func updateSelection(from textView: NSTextView) {
            document.updateSelectedLineRange(Self.lineRange(for: textView.selectedRange(), in: textView.string))
        }

        private static func lineRange(for selection: NSRange, in text: String) -> EditorLineRange? {
            let nsText = text as NSString
            guard nsText.length > 0, selection.location != NSNotFound else { return nil }
            let startLocation = min(max(selection.location, 0), nsText.length)
            let rawEnd = selection.length == 0 ? startLocation : NSMaxRange(selection) - 1
            let endLocation = min(max(rawEnd, startLocation), max(0, nsText.length - 1))
            return EditorLineRange(
                start: lineNumber(at: startLocation, in: nsText),
                end: lineNumber(at: endLocation, in: nsText)
            )
        }

        private static func lineNumber(at location: Int, in text: NSString) -> Int {
            let clamped = min(max(location, 0), text.length)
            guard clamped > 0 else { return 1 }
            return text.substring(to: clamped).reduce(into: 1) { count, ch in
                if ch == "\n" { count += 1 }
            }
        }

        /// Debounce: a burst of keystrokes collapses to a single re-highlight
        /// ~60ms after the last change, instead of re-colouring the whole
        /// document on every keystroke (EDIT-001).
        private func scheduleHighlight() {
            pendingHighlight?.cancel()
            let work = DispatchWorkItem { [weak self] in self?.highlight() }
            pendingHighlight = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.06, execute: work)
        }

        /// Apply per-line syntax colors to the text storage in place, preserving
        /// the user's selection (we mutate attributes, not the string).
        func highlight() {
            guard let textView, let storage = textView.textStorage else { return }
            let fullText = textView.string as NSString
            let font = NSFont.monospacedSystemFont(ofSize: 12.5, weight: .regular)
            storage.beginEditing()
            let whole = NSRange(location: 0, length: fullText.length)
            storage.setAttributes([
                .font: font,
                .foregroundColor: NSColor(Theme.text)
            ], range: whole)

            // Large-file mode: skip per-line syntax highlighting beyond the limit
            // so a big file stays within the typing budget (§14.4 / §23.5). The
            // base font/colour above still applies, so the text stays readable.
            if fullText.length <= largeFileHighlightLimit {
                let language = document.language
                fullText.enumerateSubstrings(in: whole, options: .byLines) { line, lineRange, _, _ in
                    guard let line, !line.isEmpty else { return }
                    let attributed = SyntaxHighlighter.line(line, lang: language)
                    Self.applyAttributedColors(attributed, baseLocation: lineRange.location,
                                               into: storage, font: font)
                }
            }
            storage.endEditing()
        }

        /// Bridge the SwiftUI AttributedString colors onto the NSTextStorage at
        /// the given line offset.
        private static func applyAttributedColors(
            _ attributed: AttributedString,
            baseLocation: Int,
            into storage: NSTextStorage,
            font: NSFont
        ) {
            let ns = NSAttributedString(attributed)
            ns.enumerateAttribute(.foregroundColor, in: NSRange(location: 0, length: ns.length)) { value, range, _ in
                guard let color = value as? NSColor else { return }
                let target = NSRange(location: baseLocation + range.location, length: range.length)
                if target.location + target.length <= storage.length {
                    storage.addAttribute(.foregroundColor, value: color, range: target)
                }
            }
        }
    }
}

// MARK: - Line number ruler

/// A gutter ruler that draws 1-based line numbers aligned to each laid-out line,
/// plus per-line diff markers (added/removed/changed) using semantic tints AND a
/// distinct glyph so the marker never relies on colour alone.
final class LineNumberRulerView: NSRulerView {
    private weak var editor: NSTextView?
    /// 1-based buffer line → change marker, refreshed by the representable.
    var diffMarkers: [Int: DiffGutterMarker] = [:]

    init(textView: NSTextView) {
        self.editor = textView
        super.init(scrollView: textView.enclosingScrollView, orientation: .verticalRuler)
        self.clientView = textView
        self.ruleThickness = 52
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        guard let textView = editor,
              let layoutManager = textView.layoutManager,
              let container = textView.textContainer else { return }

        NSColor(Theme.gutter).setFill()
        bounds.fill()

        let text = textView.string as NSString
        let visibleRect = textView.visibleRect
        let glyphRange = layoutManager.glyphRange(forBoundingRect: visibleRect, in: container)
        let inset = textView.textContainerInset.height
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular),
            .foregroundColor: NSColor(Theme.gutterText)
        ]

        var lineNumber = 1
        // Count newlines before the first visible glyph to get the start line.
        let firstCharIndex = layoutManager.characterIndexForGlyph(at: glyphRange.location)
        if firstCharIndex > 0 {
            lineNumber += text.substring(to: firstCharIndex)
                .reduce(into: 0) { count, ch in if ch == "\n" { count += 1 } }
        }

        var index = glyphRange.location
        while index < NSMaxRange(glyphRange) {
            let charIndex = layoutManager.characterIndexForGlyph(at: index)
            var lineRange = NSRange()
            let lineFragmentRect = layoutManager.lineFragmentRect(
                forGlyphAt: index, effectiveRange: &lineRange, withoutAdditionalLayout: true
            )
            let charRange = text.lineRange(for: NSRange(location: charIndex, length: 0))
            // Only label the first fragment of a logical line.
            if charIndex == charRange.location {
                let y = lineFragmentRect.minY + inset - textView.visibleRect.minY

                // Diff marker (added/removed/changed): a tinted bar on the gutter
                // edge plus a sign glyph so it reads without colour.
                if let marker = diffMarkers[lineNumber] {
                    drawMarker(marker, atY: y, height: lineFragmentRect.height)
                }

                let label = "\(lineNumber)" as NSString
                let size = label.size(withAttributes: attrs)
                label.draw(
                    at: NSPoint(x: ruleThickness - size.width - 8, y: y + 1),
                    withAttributes: attrs
                )
                lineNumber += 1
            }
            index = NSMaxRange(lineRange)
        }
    }

    /// Draw one diff marker: a tinted vertical bar at the gutter's inner edge and
    /// a small sign glyph (`+`/`-`/`~`) so the change kind is legible without
    /// depending on colour.
    private func drawMarker(_ marker: DiffGutterMarker, atY y: CGFloat, height: CGFloat) {
        let tint = NSColor(marker.tint)
        let barWidth: CGFloat = 3
        let barRect = NSRect(x: ruleThickness - barWidth, y: y, width: barWidth, height: max(height, 12))
        tint.setFill()
        barRect.fill()

        let sign: String
        switch marker {
        case .added: sign = "+"
        case .removed: sign = "−"
        case .changed: sign = "~"
        }
        let glyphAttrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 9, weight: .bold),
            .foregroundColor: tint
        ]
        (sign as NSString).draw(at: NSPoint(x: 3, y: y + 1), withAttributes: glyphAttrs)
    }
}
