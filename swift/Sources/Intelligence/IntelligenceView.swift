// IntelligenceView.swift — the Project Intelligence route (PR-041). A dark,
// token-driven, full-width surface with four sections, EACH carrying a freshness
// badge (glyph + tint + label, never colour alone; a STALE section is visibly and
// textually marked stale and is NEVER drawn as "current"):
//
//   1. Architecture records — titled notes with refs; each row deep-links to the
//      relevant conversation / run / file via the EXISTING routes.
//   2. CodeGraph explorer  — the large code graph drawn into a SINGLE SwiftUI
//      `Canvas` (reusing the PR-030 pan/zoom + LOD technique) and PAGED (limit/
//      offset) so a 5,000-symbol graph never loads whole. A result deep-links to
//      its source file.
//   3. Glossary            — terms + definitions + refs.
//   4. Source navigation   — a compact index of every deep-link target on screen.
//
// Deep links call into `AppCoordinator` to navigate the EXISTING chat / graph /
// code routes (no new route is invented, none removed). Full-tile hit areas use
// the shared button styles; the view fills width (no letterbox).

import SwiftUI

struct IntelligenceView: View {
    @ObservedObject var store: IntelligenceStore
    /// Deep-link sink. Defaults to a no-op so the view renders/tests without a
    /// wired coordinator; the host passes a closure that navigates the existing
    /// routes (chat / graph / code).
    var onOpen: (IntelDeepLinkTarget) -> Void = { _ in }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.s20) {
                header
                architectureSection
                codeGraphSection
                glossarySection
                sourceNavigationSection
            }
            .padding(Theme.s20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .accessibilityIdentifier("intelligence.view")
        .task { await store.loadAll() }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Theme.s10) {
            Image(systemName: "brain.head.profile")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(Theme.violet)
            VStack(alignment: .leading, spacing: 2) {
                Text("Project Intelligence")
                    .font(Theme.ui(18, .semibold))
                    .foregroundStyle(Theme.text)
                Text("Architecture, code graph, glossary — each stamped with its freshness.")
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 0)
            Button {
                Task { await store.recheckFreshness() }
            } label: {
                Label("Re-check freshness", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.secondaryAction)
            .fixedSize()
            .accessibilityIdentifier("intelligence.recheck")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Architecture

    private var architectureSection: some View {
        SectionCard(
            title: "Architecture",
            systemImage: "building.columns",
            badge: store.architectureBadge,
            badgeIdentifier: "intelligence.badge.architecture"
        ) {
            if store.architecture.isEmpty {
                EmptyRow(text: "No architecture records yet.")
            } else {
                VStack(spacing: Theme.s8) {
                    ForEach(store.architecture) { record in
                        ArchitectureRow(
                            record: record,
                            target: store.deepLinkTarget(forRecord: record.id),
                            onOpen: onOpen
                        )
                    }
                }
            }
        }
        .accessibilityIdentifier("intelligence.section.architecture")
    }

    // MARK: - Code graph (paged + LOD canvas)

    private var codeGraphSection: some View {
        SectionCard(
            title: "Code graph",
            systemImage: "point.3.connected.trianglepath.dotted",
            badge: store.codeGraphBadge,
            badgeIdentifier: "intelligence.badge.codegraph"
        ) {
            VStack(alignment: .leading, spacing: Theme.s10) {
                codeGraphToolbar
                CodeGraphExplorer(
                    records: store.codeGraphRecords,
                    onSelect: { record in onOpen(store.deepLinkTarget(forCodeGraph: record)) }
                )
                .frame(height: 320)
                .frame(maxWidth: .infinity)
                .background(
                    RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                        .fill(Theme.panelDeep)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                        .strokeBorder(Theme.stroke, lineWidth: 1)
                )
                .accessibilityIdentifier("intelligence.codegraph.canvas.container")
                codeGraphPager
            }
        }
        .accessibilityIdentifier("intelligence.section.codegraph")
    }

    private var codeGraphToolbar: some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.muted)
            TextField("Filter symbols…", text: $store.codeGraphQuery)
                .textFieldStyle(.plain)
                .font(Theme.mono(12))
                .foregroundStyle(Theme.text)
                .onSubmit { Task { await store.runCodeGraphQuery() } }
                .accessibilityIdentifier("intelligence.codegraph.query")
            Text("\(store.codeGraphTotal) symbols")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .accessibilityIdentifier("intelligence.codegraph.total")
        }
        .padding(.horizontal, Theme.s10)
        .padding(.vertical, Theme.s6)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .fill(Theme.input)
        )
    }

    private var codeGraphPager: some View {
        HStack(spacing: Theme.s8) {
            Button {
                Task { await store.previousCodeGraphPage() }
            } label: {
                Label("Previous", systemImage: "chevron.left")
            }
            .buttonStyle(.quietAction)
            .fixedSize()
            .disabled(!store.hasPreviousCodeGraphPage)
            .accessibilityIdentifier("intelligence.codegraph.prev")

            Text("Page \(store.codeGraphPageIndex) of \(store.codeGraphPageCount)")
                .font(Theme.ui(11, .medium))
                .foregroundStyle(Theme.textSoft)
                .accessibilityIdentifier("intelligence.codegraph.pageLabel")

            Button {
                Task { await store.nextCodeGraphPage() }
            } label: {
                Label("Next", systemImage: "chevron.right")
            }
            .buttonStyle(.quietAction)
            .fixedSize()
            .disabled(!store.hasNextCodeGraphPage)
            .accessibilityIdentifier("intelligence.codegraph.next")

            Spacer(minLength: 0)

            Text("Showing \(store.codeGraphRecords.count) of \(store.codeGraphTotal)")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
        }
    }

    // MARK: - Glossary

    private var glossarySection: some View {
        SectionCard(
            title: "Glossary",
            systemImage: "character.book.closed",
            badge: store.glossaryBadge,
            badgeIdentifier: "intelligence.badge.glossary"
        ) {
            if store.glossary.isEmpty {
                EmptyRow(text: "No glossary terms yet.")
            } else {
                VStack(spacing: Theme.s8) {
                    ForEach(store.glossary) { term in
                        GlossaryRow(term: term)
                    }
                }
            }
        }
        .accessibilityIdentifier("intelligence.section.glossary")
    }

    // MARK: - Source navigation

    /// A compact index of every deep-link target currently on screen (architecture
    /// refs + code-graph page records), each a full-tile button into the matching
    /// existing route.
    private var sourceNavigationSection: some View {
        SectionCard(
            title: "Source navigation",
            systemImage: "arrow.uturn.forward",
            badge: nil,
            badgeIdentifier: nil
        ) {
            let targets = sourceTargets
            if targets.isEmpty {
                EmptyRow(text: "No source links on screen.")
            } else {
                VStack(spacing: 4) {
                    ForEach(Array(targets.enumerated()), id: \.offset) { _, item in
                        SourceLinkRow(label: item.label, target: item.target, onOpen: onOpen)
                    }
                }
            }
        }
        .accessibilityIdentifier("intelligence.section.sources")
    }

    private var sourceTargets: [(label: String, target: IntelDeepLinkTarget)] {
        var items: [(String, IntelDeepLinkTarget)] = []
        for record in store.architecture {
            if let target = store.deepLinkTarget(forRecord: record.id) {
                items.append((record.title, target))
            }
        }
        for record in store.codeGraphRecords.prefix(12) {
            items.append(("\(record.symbol) · \(record.path)", store.deepLinkTarget(forCodeGraph: record)))
        }
        return items
    }
}

// MARK: - Section card

/// A titled card with an optional freshness badge in its header. The badge is
/// rendered by `FreshnessBadgeView` (glyph + tint + label) so STALE is never
/// colour-alone and never reads as "current".
private struct SectionCard<Content: View>: View {
    let title: String
    let systemImage: String
    let badge: IntelFreshnessBadge?
    let badgeIdentifier: String?
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            HStack(spacing: Theme.s8) {
                Image(systemName: systemImage)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text(title)
                    .font(Theme.ui(14, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer(minLength: 0)
                if let badge {
                    FreshnessBadgeView(badge: badge)
                        .accessibilityIdentifier(badgeIdentifier ?? "intelligence.badge")
                }
            }
            content()
        }
        .padding(Theme.s16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
    }
}

/// The freshness badge: a glyph + tint + TEXT label. The label always names the
/// state ("Fresh" / "Stale · …"), so a stale badge can never be mistaken for a
/// fresh one even ignoring colour.
struct FreshnessBadgeView: View {
    let badge: IntelFreshnessBadge

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: badge.symbol)
                .font(.system(size: 9, weight: .bold))
            Text(badge.label)
                .font(Theme.ui(11, .semibold))
        }
        .foregroundStyle(badge.tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(badge.tint.opacity(0.14)))
        .overlay(Capsule().strokeBorder(badge.tint.opacity(0.3), lineWidth: 1))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(badge.isFresh ? "Freshness: fresh" : "Freshness: stale. \(badge.label)")
    }
}

// MARK: - Rows

private struct EmptyRow: View {
    let text: String
    var body: some View {
        Text(text)
            .font(Theme.ui(12))
            .foregroundStyle(Theme.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, Theme.s8)
    }
}

private struct ArchitectureRow: View {
    let record: IntelArchitectureRecord
    let target: IntelDeepLinkTarget?
    let onOpen: (IntelDeepLinkTarget) -> Void

    var body: some View {
        Button {
            if let target { onOpen(target) }
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: Theme.s8) {
                    Text(record.title)
                        .font(Theme.ui(13, .semibold))
                        .foregroundStyle(Theme.text)
                    Spacer(minLength: 0)
                    if let target {
                        Label(targetLabel(target), systemImage: targetSymbol(target))
                            .labelStyle(.titleAndIcon)
                            .font(Theme.ui(10, .semibold))
                            .foregroundStyle(Theme.accent)
                    }
                }
                Text(record.detail)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.textSoft)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                if !record.refs.isEmpty {
                    Text(record.refs.joined(separator: "  ·  "))
                        .font(Theme.mono(10))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(1)
                }
            }
            .padding(Theme.s12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.quietAction)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .fill(Theme.input.opacity(0.5))
        )
        .disabled(target == nil)
        .accessibilityIdentifier("intelligence.architecture.row.\(record.id)")
    }

    private func targetLabel(_ target: IntelDeepLinkTarget) -> String {
        switch target {
        case .conversation: return "Open chat"
        case .run: return "Open run"
        case .file: return "Open file"
        }
    }

    private func targetSymbol(_ target: IntelDeepLinkTarget) -> String {
        switch target {
        case .conversation: return "bubble.left.and.bubble.right"
        case .run: return "point.3.connected.trianglepath.dotted"
        case .file: return "chevron.left.forwardslash.chevron.right"
        }
    }
}

private struct GlossaryRow: View {
    let term: IntelGlossaryTerm
    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(term.term)
                .font(Theme.ui(13, .semibold))
                .foregroundStyle(Theme.text)
            Text(term.definition)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.textSoft)
                .fixedSize(horizontal: false, vertical: true)
            if !term.refs.isEmpty {
                Text(term.refs.joined(separator: "  ·  "))
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
            }
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .fill(Theme.input.opacity(0.5))
        )
        .accessibilityIdentifier("intelligence.glossary.row.\(term.term)")
    }
}

private struct SourceLinkRow: View {
    let label: String
    let target: IntelDeepLinkTarget
    let onOpen: (IntelDeepLinkTarget) -> Void

    var body: some View {
        Button {
            onOpen(target)
        } label: {
            HStack(spacing: Theme.s8) {
                Image(systemName: symbol)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text(label)
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textSoft)
                    .lineLimit(1)
                Spacer(minLength: 0)
                Image(systemName: "arrow.up.right")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Theme.muted)
            }
            .padding(.horizontal, Theme.s10)
        }
        .buttonStyle(ListRowButtonStyle(minHeight: 30))
        .accessibilityIdentifier("intelligence.source.row.\(target.targetId)")
    }

    private var symbol: String {
        switch target {
        case .conversation: return "bubble.left.and.bubble.right"
        case .run: return "point.3.connected.trianglepath.dotted"
        case .file: return "chevron.left.forwardslash.chevron.right"
        }
    }
}

// MARK: - CodeGraph explorer (single-Canvas, pan/zoom, LOD)

/// The code-graph explorer canvas. It mirrors the PR-030 performance contract: the
/// WHOLE page is drawn into ONE SwiftUI `Canvas` (not one subview per symbol), so a
/// 100-symbol page pans/zooms by mutating two scalars. Because the store PAGES the
/// graph (limit/offset), this canvas only ever draws the current window — never the
/// full 5,000-symbol corpus. Labels/glyphs drop below an LOD zoom threshold so a
/// zoomed-out page stays cheap and legible. A tap maps back to the nearest symbol
/// and deep-links to its source file.
struct CodeGraphExplorer: View {
    let records: [IntelCodeGraphRecord]
    var onSelect: (IntelCodeGraphRecord) -> Void = { _ in }

    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @GestureState private var gestureScale: CGFloat = 1
    @GestureState private var gestureOffset: CGSize = .zero
    @State private var didFit = false
    @State private var lastViewSize = CGSize(width: 1, height: 1)
    @State private var selectedId: String?

    private let minScale: CGFloat = 0.05
    private let maxScale: CGFloat = 4
    private let labelLODThreshold: CGFloat = 0.6
    private let glyphLODThreshold: CGFloat = 0.35
    private let metrics = GraphLayout.Metrics.default

    private var effectiveScale: CGFloat {
        min(max(scale * gestureScale, minScale), maxScale)
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topTrailing) {
                canvas(in: geo.size)
                overlayControls
                if records.isEmpty {
                    Text("No symbols on this page.")
                        .font(Theme.ui(12))
                        .foregroundStyle(Theme.muted)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .contentShape(Rectangle())
            .gesture(panGesture)
            .simultaneousGesture(zoomGesture)
            .onTapGesture { location in selectSymbol(at: location, viewSize: geo.size) }
            .onAppear { fitIfNeeded(viewSize: geo.size, force: true) }
            .onChange(of: geo.size) { _ in fitIfNeeded(viewSize: geo.size, force: !didFit) }
            .onChange(of: records) { _ in
                // A new page → refit so the new window is framed.
                didFit = false
                fitIfNeeded(viewSize: lastViewSize, force: true)
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("intelligence.codegraph.canvas")
            .accessibilityLabel("Code graph, \(records.count) symbols on this page")
        }
    }

    // MARK: Canvas

    private func canvas(in viewSize: CGSize) -> some View {
        let currentScale = effectiveScale
        let currentOffset = CGSize(
            width: offset.width + gestureOffset.width,
            height: offset.height + gestureOffset.height
        )
        let showGlyphs = currentScale >= glyphLODThreshold
        let showLabels = currentScale >= labelLODThreshold
        let positions = layout().positions

        return Canvas { context, size in
            guard !records.isEmpty else { return }
            for position in positions {
                let record = records[position.index]
                let center = CGPoint(
                    x: position.center.x * currentScale + currentOffset.width,
                    y: position.center.y * currentScale + currentOffset.height
                )
                let radius = metrics.nodeRadius * currentScale
                // Cull anything fully outside the viewport (bounded draw cost).
                if center.x + radius < 0 || center.x - radius > size.width
                    || center.y + radius < 0 || center.y - radius > size.height {
                    continue
                }
                drawSymbol(
                    record: record,
                    center: center,
                    radius: radius,
                    isSelected: record.id == selectedId,
                    showGlyph: showGlyphs,
                    showLabel: showLabels,
                    context: &context
                )
            }
        }
        .drawingGroup()
    }

    private func drawSymbol(
        record: IntelCodeGraphRecord,
        center: CGPoint,
        radius: CGFloat,
        isSelected: Bool,
        showGlyph: Bool,
        showLabel: Bool,
        context: inout GraphicsContext
    ) {
        let tint = Self.kindTint(record.kind)
        let rect = CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)
        let circle = Path(ellipseIn: rect)
        context.fill(circle, with: .color(tint.opacity(0.9)))
        context.stroke(
            circle,
            with: .color(isSelected ? Theme.text : tint),
            lineWidth: isSelected ? 2.5 : 1
        )
        // LOD: glyph only when zoomed in enough to read it.
        if showGlyph, radius >= 6 {
            let glyph = Image(systemName: Self.kindGlyph(record.kind))
            var resolved = context.resolve(glyph)
            resolved.shading = .color(Theme.accentInk)
            let glyphSize = min(radius * 1.1, 13)
            context.draw(resolved, in: CGRect(
                x: center.x - glyphSize / 2, y: center.y - glyphSize / 2,
                width: glyphSize, height: glyphSize
            ))
        }
        // LOD: label only at high zoom.
        if showLabel {
            var resolvedText = context.resolve(Text(record.symbol).font(Theme.mono(9)))
            resolvedText.shading = .color(Theme.muted)
            context.draw(resolvedText, at: CGPoint(x: center.x + radius + 4, y: center.y), anchor: .leading)
        }
    }

    // MARK: Overlay controls

    private var overlayControls: some View {
        VStack(spacing: 6) {
            graphButton("Fit", systemImage: "arrow.up.left.and.arrow.down.right") {
                let transform = fitTransform(in: lastViewSize)
                scale = transform.scale
                offset = transform.offset
            }
            graphButton("Zoom in", systemImage: "plus.magnifyingglass") { zoomBy(1.25) }
            graphButton("Zoom out", systemImage: "minus.magnifyingglass") { zoomBy(0.8) }
        }
        .padding(8)
    }

    private func graphButton(_ label: String, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 12, weight: .semibold))
        }
        .buttonStyle(IconTileButtonStyle(size: 30))
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .fill(Theme.panel.opacity(0.9))
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityLabel(label)
        .accessibilityIdentifier("intelligence.codegraph.control.\(label.replacingOccurrences(of: " ", with: "").lowercased())")
    }

    // MARK: Gestures

    private var panGesture: some Gesture {
        DragGesture()
            .updating($gestureOffset) { value, state, _ in state = value.translation }
            .onEnded { value in
                offset.width += value.translation.width
                offset.height += value.translation.height
            }
    }

    private var zoomGesture: some Gesture {
        MagnificationGesture()
            .updating($gestureScale) { value, state, _ in state = value }
            .onEnded { value in scale = min(max(scale * value, minScale), maxScale) }
    }

    private func zoomBy(_ factor: CGFloat) {
        scale = min(max(scale * factor, minScale), maxScale)
    }

    // MARK: Selection (canvas hit-testing → deep link)

    private func selectSymbol(at point: CGPoint, viewSize: CGSize) {
        guard !records.isEmpty else { return }
        let s = effectiveScale
        let o = CGSize(
            width: offset.width + gestureOffset.width,
            height: offset.height + gestureOffset.height
        )
        let layoutPoint = CGPoint(x: (point.x - o.width) / s, y: (point.y - o.height) / s)
        let hitRadius = max(metrics.nodeRadius, 14 / s)
        var best: (record: IntelCodeGraphRecord, dist: CGFloat)?
        let positions = layout().positions
        for position in positions {
            let record = records[position.index]
            let dx = position.center.x - layoutPoint.x
            let dy = position.center.y - layoutPoint.y
            let dist = (dx * dx + dy * dy).squareRoot()
            if dist <= hitRadius, best == nil || dist < best!.dist {
                best = (record, dist)
            }
        }
        if let best {
            selectedId = best.record.id
            onSelect(best.record)
        }
    }

    // MARK: Layout (same columnar arithmetic as GraphLayout, on the page window)

    /// Place each record on the SAME layered grid `GraphLayout` uses, computed
    /// purely from the page's record order so it is deterministic and O(n).
    private func layout() -> CodeGraphPageLayout {
        CodeGraphPageLayout(count: records.count, metrics: metrics)
    }

    private func fitTransform(in viewSize: CGSize) -> (scale: CGFloat, offset: CGSize) {
        layout().fitTransform(in: viewSize, minScale: minScale, maxScale: maxScale)
    }

    private func fitIfNeeded(viewSize: CGSize, force: Bool = false) {
        lastViewSize = viewSize
        guard force || !didFit else { return }
        let transform = fitTransform(in: viewSize)
        scale = transform.scale
        offset = transform.offset
        didFit = true
    }

    // MARK: Kind → token tint / glyph

    static func kindTint(_ kind: String) -> Color {
        switch kind {
        case "type", "struct", "class", "enum", "protocol":
            return GeneratedDesignTokens.colorStatusRunning
        case "func", "method", "function":
            return GeneratedDesignTokens.colorAccentPrimary
        case "var", "let", "property", "field":
            return GeneratedDesignTokens.colorAccentSecondary
        default:
            return Theme.muted
        }
    }

    static func kindGlyph(_ kind: String) -> String {
        switch kind {
        case "type", "struct", "class", "enum", "protocol": return "cube"
        case "func", "method", "function": return "function"
        case "var", "let", "property", "field": return "f.cursive"
        default: return "circle"
        }
    }
}

/// A purely-arithmetic layout for one code-graph PAGE — the same layered/columnar
/// grid `GraphLayout` uses, but indexed by position so it depends only on the page
/// size. Kept tiny + value-typed so the canvas recomputes it cheaply.
struct CodeGraphPageLayout {
    struct Placed { let index: Int; let center: CGPoint }
    let positions: [Placed]
    let contentBounds: CGRect

    init(count: Int, metrics: GraphLayout.Metrics) {
        let rowsPerColumn = max(1, metrics.rowsPerColumn)
        var placed: [Placed] = []
        placed.reserveCapacity(count)
        for index in 0..<count {
            let column = index / rowsPerColumn
            let row = index % rowsPerColumn
            let x = metrics.padding + CGFloat(column) * metrics.horizontalSpacing
            let y = metrics.padding + CGFloat(row) * metrics.verticalSpacing
            placed.append(Placed(index: index, center: CGPoint(x: x, y: y)))
        }
        self.positions = placed
        if placed.isEmpty {
            self.contentBounds = CGRect(x: 0, y: 0, width: 1, height: 1)
        } else {
            var minX = CGFloat.greatestFiniteMagnitude, minY = CGFloat.greatestFiniteMagnitude
            var maxX = -CGFloat.greatestFiniteMagnitude, maxY = -CGFloat.greatestFiniteMagnitude
            for p in placed {
                minX = min(minX, p.center.x); minY = min(minY, p.center.y)
                maxX = max(maxX, p.center.x); maxY = max(maxY, p.center.y)
            }
            let inset = metrics.nodeRadius + metrics.padding
            self.contentBounds = CGRect(
                x: minX - inset, y: minY - inset,
                width: (maxX - minX) + inset * 2, height: (maxY - minY) + inset * 2
            )
        }
    }

    func fitTransform(in viewSize: CGSize, minScale: CGFloat, maxScale: CGFloat) -> (scale: CGFloat, offset: CGSize) {
        guard contentBounds.width > 0, contentBounds.height > 0,
              viewSize.width > 0, viewSize.height > 0 else { return (1, .zero) }
        let rawScale = min(viewSize.width / contentBounds.width, viewSize.height / contentBounds.height)
        let scale = min(max(rawScale, minScale), maxScale)
        let scaledWidth = contentBounds.width * scale
        let scaledHeight = contentBounds.height * scale
        let offsetX = (viewSize.width - scaledWidth) / 2 - contentBounds.minX * scale
        let offsetY = (viewSize.height - scaledHeight) / 2 - contentBounds.minY * scale
        return (scale, CGSize(width: offsetX, height: offsetY))
    }
}
