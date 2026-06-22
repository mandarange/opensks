// DesignStudioView.swift — the Design Studio route (PR-040).
//
// A Design route with a SIDEBAR (the catalog of packages) + detail TABS:
//   • Tokens     — a token editor listing token paths/values, editable inline.
//   • Components — a NATIVE component STATE MATRIX previewing the app's real
//                  controls across default / hover / pressed / disabled / focused.
//   • Audit      — the audit findings grouped by kind/severity, with a CLEAR
//                  blocked indicator when activation is blocked.
//   • Revisions  — the proof-linked revision lifecycle (propose / accept / reject /
//                  rollback). Each revision shows its proof_ref.
//
// Activation is ATOMIC: activating a package that FAILS its audit shows the failure
// and KEEPS the previously active package selected (the shown active package does
// not change). Dark, token-driven, full-tile hit areas, fills width (no letterbox).
// Status is conveyed by icon + label + a semantic token, never colour alone.

import SwiftUI

// MARK: - Detail tabs

enum DesignStudioTab: String, CaseIterable, Identifiable {
    case tokens, components, audit, revisions

    var id: String { rawValue }

    var label: String {
        switch self {
        case .tokens: return "Tokens"
        case .components: return "Components"
        case .audit: return "Audit"
        case .revisions: return "Revisions"
        }
    }

    var symbol: String {
        switch self {
        case .tokens: return "number.square"
        case .components: return "square.on.square"
        case .audit: return "checklist"
        case .revisions: return "clock.arrow.circlepath"
        }
    }
}

// MARK: - Root

struct DesignStudioView: View {
    @ObservedObject var store: DesignStudioStore
    @State private var tab: DesignStudioTab = .tokens

    var body: some View {
        HStack(spacing: 0) {
            DesignCatalogSidebar(store: store)
                .frame(width: 248)
            Divider().overlay(Theme.stroke)
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .opacity(store.isBusy ? 0.7 : 1)
        .animation(.easeInOut(duration: 0.15), value: store.isBusy)
        .accessibilityIdentifier("design.studio.view")
    }

    @ViewBuilder
    private var detail: some View {
        VStack(alignment: .leading, spacing: 0) {
            detailHeader
            Divider().overlay(Theme.stroke)
            tabBar
            Divider().overlay(Theme.stroke)
            if let error = store.lastError {
                banner(error, symbol: "exclamationmark.triangle.fill", tint: Theme.coral) {
                    store.lastError = nil
                }
                .accessibilityIdentifier("design.studio.error")
            }
            tabContent
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    // MARK: Header (active status + activate action)

    private var detailHeader: some View {
        HStack(alignment: .center, spacing: Theme.s12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(store.selectedPackage?.title ?? "No package selected")
                    .font(Theme.ui(15, .semibold))
                    .foregroundStyle(Theme.text)
                activeStatusRow
            }
            Spacer()
            if let package = store.selectedPackage {
                Button {
                    Task { await store.audit(package: package.packageId) }
                } label: {
                    Label("Run Audit", systemImage: "checklist")
                }
                .buttonStyle(.secondaryAction)
                .frame(maxWidth: 140)
                .disabled(store.isBusy)
                .accessibilityIdentifier("design.studio.run-audit")

                Button {
                    tab = .audit
                    Task { await store.activate(package: package.packageId) }
                } label: {
                    Label(store.isActive(package.packageId) ? "Active" : "Activate", systemImage: "bolt.fill")
                }
                .buttonStyle(.primaryAction)
                .frame(maxWidth: 140)
                .disabled(store.isBusy || store.isActive(package.packageId))
                .accessibilityIdentifier("design.studio.activate")
                .help("Activation is atomic: a failing audit blocks it and keeps the current active package.")
            }
        }
        .padding(Theme.s16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.sidebar)
    }

    private var activeStatusRow: some View {
        HStack(spacing: 6) {
            Image(systemName: store.active.activePackage == nil ? "circle.dashed" : "bolt.circle.fill")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(store.active.activePackage == nil ? Theme.muted : Theme.accent)
            Text("Active: \(store.active.activePackageDisplay)")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
            Text("·")
                .foregroundStyle(Theme.faint)
            Text("Revision: \(store.active.activatedRevisionDisplay)")
                .font(Theme.mono(10.5))
                .foregroundStyle(Theme.muted)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("design.studio.active-status")
    }

    private var tabBar: some View {
        HStack(spacing: Theme.s6) {
            ForEach(DesignStudioTab.allCases) { item in
                Button {
                    tab = item
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: item.symbol)
                            .font(.system(size: 11, weight: .semibold))
                        Text(item.label)
                            .font(Theme.ui(12, .semibold))
                        if item == .audit, let audit = store.selectedAudit, !audit.passed {
                            Image(systemName: "exclamationmark.circle.fill")
                                .font(.system(size: 9, weight: .bold))
                                .foregroundStyle(Theme.coral)
                        }
                    }
                    .foregroundStyle(tab == item ? Theme.accent : Theme.muted)
                    .padding(.horizontal, Theme.s12)
                    .frame(minHeight: 34)
                    .background(
                        RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                            .fill(tab == item ? Theme.accentTint : Color.clear)
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("design.studio.tab.\(item.rawValue)")
            }
            Spacer()
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.panelDeep)
    }

    @ViewBuilder
    private var tabContent: some View {
        if let package = store.selectedPackage {
            switch tab {
            case .tokens:
                TokenEditorView(store: store, package: package)
            case .components:
                ComponentStateMatrixView()
            case .audit:
                AuditFindingsView(store: store, package: package)
            case .revisions:
                RevisionsView(store: store, package: package)
            }
        } else {
            EmptyStateView(
                headline: "Select a package",
                detail: "Choose a design package from the catalog to inspect its tokens, components, audit, and revisions.",
                systemImage: "paintpalette"
            )
        }
    }

    // MARK: Banner

    private func banner(
        _ message: String,
        symbol: String,
        tint: Color,
        onDismiss: @escaping () -> Void
    ) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: symbol)
                .foregroundStyle(tint)
            Text(message)
                .font(Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Button(action: onDismiss) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(Theme.muted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Theme.s16)
        .padding(.vertical, Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.12))
    }
}

// MARK: - Sidebar (catalog of packages)

struct DesignCatalogSidebar: View {
    @ObservedObject var store: DesignStudioStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "paintpalette")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text("Design Packages")
                    .font(Theme.ui(12.5, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
            }
            .padding(Theme.s12)
            Divider().overlay(Theme.stroke)

            if store.catalog.isEmpty {
                EmptyStateView(
                    headline: "No packages",
                    detail: "Import and promote a design package to see it here.",
                    systemImage: "tray"
                )
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(store.catalog) { package in
                            CatalogRow(
                                package: package,
                                selected: store.selectedPackageId == package.packageId,
                                active: store.isActive(package.packageId),
                                action: { store.select(package.packageId) }
                            )
                        }
                    }
                    .padding(Theme.s8)
                }
            }
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.sidebar)
        .accessibilityIdentifier("design.studio.catalog")
    }
}

private struct CatalogRow: View {
    let package: DesignPackage
    let selected: Bool
    let active: Bool
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "square.stack.3d.up")
                    .font(.system(size: 12))
                    .foregroundStyle(selected ? Theme.accent : Theme.muted)
                VStack(alignment: .leading, spacing: 1) {
                    Text(package.title)
                        .font(Theme.ui(12, selected ? .semibold : .regular))
                        .foregroundStyle(selected ? Theme.text : Theme.textSoft)
                        .lineLimit(1)
                    Text("\(package.tokens.count) token\(package.tokens.count == 1 ? "" : "s")")
                        .font(Theme.mono(9.5))
                        .foregroundStyle(Theme.faint)
                }
                Spacer(minLength: 0)
                if active {
                    // Active is shown by an icon + label, never colour alone.
                    HStack(spacing: 3) {
                        Image(systemName: "bolt.fill")
                            .font(.system(size: 8, weight: .bold))
                        Text("Active")
                            .font(Theme.ui(9, .semibold))
                    }
                    .foregroundStyle(Theme.accent)
                    .accessibilityLabel("Active package")
                }
            }
            .padding(.horizontal, Theme.s10)
            .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                    .fill(selected ? Theme.accentTint : (hovering ? Theme.panel : Color.clear))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityIdentifier("design.studio.catalog.row.\(package.packageId)")
    }
}

// MARK: - Tokens tab (the editor)

struct TokenEditorView: View {
    @ObservedObject var store: DesignStudioStore
    let package: DesignPackage

    var body: some View {
        let tokens = store.tokenDraftsByPackage[package.packageId] ?? package.tokens
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.s6) {
                toolbar
                if let compile = store.compileByPackage[package.packageId] {
                    compileStatus(compile)
                }
                HStack {
                    Text("Tokens")
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(Theme.muted)
                    Spacer()
                    Text("\(tokens.count) entries")
                        .font(Theme.mono(10))
                        .foregroundStyle(Theme.faint)
                }
                .padding(.bottom, 2)

                if tokens.isEmpty {
                    EmptyStateView(
                        headline: "No tokens",
                        detail: "This package ships no editable tokens.",
                        systemImage: "number.square"
                    )
                } else {
                    ForEach(tokens) { token in
                        TokenRow(
                            token: token,
                            onChange: { newValue in
                                store.setTokenValue(newValue, forPath: token.path, package: package.packageId)
                            }
                        )
                    }
                    Divider().overlay(Theme.stroke).padding(.vertical, Theme.s8)
                    DesignTokenPreview(tokens: tokens)
                }
            }
            .padding(Theme.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityIdentifier("design.studio.tokens")
    }

    /// Save / Compile actions + the save/dirty status (DESIGN-002). Save persists
    /// the draft to disk; Compile validates it in isolation; applying to the app is
    /// the header's audit-gated Activate.
    private var toolbar: some View {
        HStack(spacing: Theme.s8) {
            Button {
                Task { await store.saveDraft(package: package.packageId) }
            } label: {
                Label("Save Draft", systemImage: "tray.and.arrow.down")
            }
            .buttonStyle(.primaryAction)
            .frame(maxWidth: 150)
            .disabled(store.isBusy || !store.dirtyPackages.contains(package.packageId))
            .accessibilityIdentifier("design.studio.save-tokens")
            .help("Persist the edited token values to this package's tokens.json.")

            Button {
                Task { await store.compile(package: package.packageId) }
            } label: {
                Label("Compile", systemImage: "hammer")
            }
            .buttonStyle(.secondaryAction)
            .frame(maxWidth: 130)
            .disabled(store.isBusy)
            .accessibilityIdentifier("design.studio.compile-tokens")
            .help("Validate the tokens compile, without activating them.")

            if store.dirtyPackages.contains(package.packageId) {
                Label("Unsaved edits", systemImage: "pencil.circle.fill")
                    .font(Theme.ui(10.5, .semibold))
                    .foregroundStyle(Theme.coral)
                    .accessibilityIdentifier("design.studio.tokens-dirty")
            } else if let save = store.lastSave, save.packageId == package.packageId {
                Label(save.summary, systemImage: "checkmark.circle")
                    .font(Theme.ui(10.5))
                    .foregroundStyle(Theme.muted)
                    .accessibilityIdentifier("design.studio.tokens-saved")
            }
            Spacer()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, 2)
        .accessibilityIdentifier("design.studio.tokens-toolbar")
    }

    private func compileStatus(_ compile: DesignCompileResult) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: compile.ok ? "checkmark.seal.fill" : "xmark.octagon.fill")
                .foregroundStyle(compile.ok ? Theme.accent : Theme.coral)
            Text(compile.ok
                ? "Compiles cleanly · \(compile.swiftBytes) bytes generated"
                : (compile.error ?? "Compile failed"))
                .font(Theme.ui(11))
                .foregroundStyle(compile.ok ? Theme.muted : Theme.coral)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.panel)
        )
        .accessibilityIdentifier("design.studio.compile-status")
    }
}

// MARK: - Live token preview (§16.4)

/// A sandboxed preview of the DRAFT token values: it renders sample surfaces and a
/// colour-swatch grid using the edited values DIRECTLY (not the global app Theme),
/// so the operator sees the effect of edits before applying via Activate.
private struct DesignTokenPreview: View {
    let tokens: [DesignTokenEntry]

    private func value(_ path: String, _ fallback: String) -> String {
        tokens.first { $0.path == path }?.value ?? fallback
    }
    private func color(_ path: String, _ fallback: String) -> Color {
        Color(hex: value(path, fallback))
    }

    var body: some View {
        let colorTokens = tokens.filter { $0.isColor }
        VStack(alignment: .leading, spacing: Theme.s10) {
            Text("Live Preview")
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.muted)

            sampleSurface

            if !colorTokens.isEmpty {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 130), spacing: Theme.s8)],
                    alignment: .leading,
                    spacing: Theme.s8
                ) {
                    ForEach(colorTokens) { token in
                        HStack(spacing: 6) {
                            RoundedRectangle(cornerRadius: 4, style: .continuous)
                                .fill(Color(hex: token.value))
                                .frame(width: 16, height: 16)
                                .overlay(
                                    RoundedRectangle(cornerRadius: 4, style: .continuous)
                                        .strokeBorder(Theme.stroke, lineWidth: 1)
                                )
                            VStack(alignment: .leading, spacing: 0) {
                                Text(token.path)
                                    .font(Theme.mono(9))
                                    .foregroundStyle(Theme.textSoft)
                                    .lineLimit(1)
                                Text(token.value)
                                    .font(Theme.mono(8.5))
                                    .foregroundStyle(Theme.faint)
                            }
                            Spacer(minLength: 0)
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
        .padding(.top, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("design.studio.token-preview")
    }

    private var sampleSurface: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            Text("Aa Sample surface")
                .font(Theme.ui(13, .semibold))
                .foregroundStyle(color("color.text.primary", "#E9EDF3"))
            Text("Secondary text rendered on the draft surface.")
                .font(Theme.ui(11))
                .foregroundStyle(color("color.text.secondary", "#BCC4D0"))
            HStack(spacing: Theme.s8) {
                Text("Accent")
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(color("color.canvas", "#0E1015"))
                    .padding(.horizontal, 12)
                    .frame(height: 30)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(color("color.accent.primary", "#5EDEC4"))
                    )
                Text("Bordered")
                    .font(Theme.ui(11))
                    .foregroundStyle(color("color.text.muted", "#7E8796"))
                    .padding(.horizontal, 12)
                    .frame(height: 30)
                    .overlay(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(color("color.border.strong", "#2C313A"), lineWidth: 1)
                    )
            }
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(color("color.surface.base", "#13161B"))
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(color("color.border.subtle", "#262A32"), lineWidth: 1)
        )
        .accessibilityIdentifier("design.studio.token-preview.surface")
    }
}

private struct TokenRow: View {
    let token: DesignTokenEntry
    let onChange: (String) -> Void
    @State private var draft: String

    init(token: DesignTokenEntry, onChange: @escaping (String) -> Void) {
        self.token = token
        self.onChange = onChange
        _draft = State(initialValue: token.value)
    }

    var body: some View {
        HStack(spacing: Theme.s10) {
            Text(token.path)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.textSoft)
                .frame(width: 200, alignment: .leading)
                .textSelection(.enabled)
                .lineLimit(1)
            if token.isColor {
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(Color(hex: draft))
                    .frame(width: 18, height: 18)
                    .overlay(
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .strokeBorder(Theme.stroke, lineWidth: 1)
                    )
                    .accessibilityHidden(true)
            }
            TextField("value", text: $draft)
                .textFieldStyle(.plain)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.text)
                .padding(.horizontal, Theme.s8)
                .frame(minHeight: 28)
                .background(
                    RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                        .fill(Theme.input)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                        .strokeBorder(Theme.stroke, lineWidth: 1)
                )
                .onChange(of: draft) { newValue in onChange(newValue) }
                .accessibilityLabel("\(token.path) value")
            Spacer(minLength: 0)
        }
        .padding(.vertical, 3)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("design.studio.token.\(token.path)")
    }
}

// MARK: - Components tab (the native state matrix)

/// Previews the app's REAL native controls across the interaction states
/// (default / hover / pressed / disabled / focused). Each control is rendered in a
/// fixed visual state so the matrix is deterministic (no live interaction needed) —
/// the previews are honest snapshots of how each control looks in that state.
struct ComponentStateMatrixView: View {
    private let states = DesignControlState.allCases

    var body: some View {
        ScrollView([.vertical, .horizontal]) {
            VStack(alignment: .leading, spacing: Theme.s16) {
                Text("Component State Matrix")
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(Theme.muted)

                headerRow
                Divider().overlay(Theme.stroke)
                controlRow(name: "Primary button") { state in
                    PreviewPrimaryButton(state: state)
                }
                controlRow(name: "Secondary button") { state in
                    PreviewSecondaryButton(state: state)
                }
                controlRow(name: "Toggle") { state in
                    PreviewToggle(state: state)
                }
                controlRow(name: "Text field") { state in
                    PreviewTextField(state: state)
                }
                controlRow(name: "Status pill") { state in
                    PreviewStatusPill(state: state)
                }
            }
            .padding(Theme.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityIdentifier("design.studio.components")
    }

    private var headerRow: some View {
        HStack(spacing: Theme.s12) {
            Text("Control")
                .font(Theme.ui(10.5, .semibold))
                .foregroundStyle(Theme.muted)
                .frame(width: 130, alignment: .leading)
            ForEach(states) { state in
                Text(state.label)
                    .font(Theme.ui(10.5, .semibold))
                    .foregroundStyle(Theme.muted)
                    .frame(width: 132, alignment: .leading)
            }
        }
    }

    private func controlRow<Cell: View>(
        name: String,
        @ViewBuilder cell: @escaping (DesignControlState) -> Cell
    ) -> some View {
        HStack(alignment: .center, spacing: Theme.s12) {
            Text(name)
                .font(Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.textSoft)
                .frame(width: 130, alignment: .leading)
            ForEach(states) { state in
                cell(state)
                    .frame(width: 132, alignment: .leading)
                    .accessibilityIdentifier("design.studio.component.\(name.replacingOccurrences(of: " ", with: "-")).\(state.rawValue)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, Theme.s4)
    }
}

// Each preview cell renders a control in a FIXED visual state. The styling mirrors
// the app's real control surfaces (SurfaceButtonStyle / StatusPill / token colours)
// so the matrix is an honest snapshot, with the state shown by label (above) not
// colour alone.

private struct PreviewPrimaryButton: View {
    let state: DesignControlState
    var body: some View {
        previewControlChip(
            fill: state == .pressed ? Theme.accent.opacity(0.85)
                : state == .hover ? Theme.accent.opacity(0.92)
                : Theme.accent,
            foreground: Theme.accentInk,
            stroke: state == .focused ? Theme.focusRing : Color.clear,
            strokeWidth: state == .focused ? 2 : 0,
            opacity: state == .disabled ? 0.45 : 1,
            label: "Action"
        )
    }
}

private struct PreviewSecondaryButton: View {
    let state: DesignControlState
    var body: some View {
        previewControlChip(
            fill: state == .pressed ? Theme.input.opacity(0.85)
                : state == .hover ? Theme.input.opacity(0.92)
                : Theme.input,
            foreground: Theme.textSoft,
            stroke: state == .focused ? Theme.focusRing
                : state == .hover ? Theme.strokeSoft : Theme.stroke,
            strokeWidth: state == .focused ? 2 : 1,
            opacity: state == .disabled ? 0.45 : 1,
            label: "Action"
        )
    }
}

private struct PreviewToggle: View {
    let state: DesignControlState
    var body: some View {
        let on = state != .disabled
        HStack(spacing: 6) {
            Capsule()
                .fill(on ? Theme.accent.opacity(state == .pressed ? 0.85 : 1) : Theme.input)
                .frame(width: 34, height: 20)
                .overlay(
                    Circle()
                        .fill(Theme.text)
                        .frame(width: 16, height: 16)
                        .padding(2),
                    alignment: on ? .trailing : .leading
                )
                .overlay(
                    Capsule()
                        .strokeBorder(state == .focused ? Theme.focusRing : Theme.stroke, lineWidth: state == .focused ? 2 : 1)
                )
            Text(on ? "On" : "Off")
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.muted)
        }
        .opacity(state == .disabled ? 0.45 : 1)
    }
}

private struct PreviewTextField: View {
    let state: DesignControlState
    var body: some View {
        Text(state == .disabled ? "" : "value")
            .font(Theme.mono(11))
            .foregroundStyle(state == .disabled ? Theme.faint : Theme.text)
            .padding(.horizontal, Theme.s8)
            .frame(width: 110, height: 28, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .fill(state == .pressed ? Theme.currentLine : Theme.input)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .strokeBorder(
                        state == .focused ? Theme.focusRing
                            : state == .hover ? Theme.strokeSoft : Theme.stroke,
                        lineWidth: state == .focused ? 2 : 1
                    )
            )
            .opacity(state == .disabled ? 0.5 : 1)
    }
}

private struct PreviewStatusPill: View {
    let state: DesignControlState
    var body: some View {
        StatusPill(kind: pillKind, label: state.label)
            .opacity(state == .disabled ? 0.45 : 1)
            .overlay(
                Capsule()
                    .strokeBorder(state == .focused ? Theme.focusRing : Color.clear, lineWidth: 2)
            )
    }
    private var pillKind: StatusPill.Kind {
        switch state {
        case .defaultState: return .neutral
        case .hover: return .running
        case .pressed: return .success
        case .disabled: return .neutral
        case .focused: return .warning
        }
    }
}

/// A shared control chip used by the button previews — mirrors SurfaceButtonStyle's
/// shape, radii, and token colours so the matrix reflects the real control surface.
private func previewControlChip(
    fill: Color,
    foreground: Color,
    stroke: Color,
    strokeWidth: CGFloat,
    opacity: Double,
    label: String
) -> some View {
    Text(label)
        .font(Theme.ui(11.5, .semibold))
        .foregroundStyle(foreground)
        .padding(.horizontal, 12)
        .frame(width: 96, height: 32)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .fill(fill)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusControl, style: .continuous)
                .strokeBorder(stroke, lineWidth: strokeWidth)
        )
        .opacity(opacity)
}

// MARK: - Audit tab

struct AuditFindingsView: View {
    @ObservedObject var store: DesignStudioStore
    let package: DesignPackage

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.s12) {
                if let block = store.activationBlock, block.blockedPackageId == package.packageId {
                    blockedIndicator(block)
                }
                if let audit = store.selectedAudit {
                    auditSummary(audit)
                    if audit.findings.isEmpty {
                        EmptyStateView(
                            headline: "No findings",
                            detail: "The audit found nothing to report for this package.",
                            systemImage: "checkmark.seal"
                        )
                    } else {
                        ForEach(audit.groupedByKind, id: \.kind) { group in
                            AuditKindGroup(kind: group.kind, findings: group.findings)
                        }
                    }
                } else {
                    EmptyStateView(
                        headline: "No audit yet",
                        detail: "Run the audit to check this package's tokens and components against the rules.",
                        systemImage: "checklist",
                        actionTitle: "Run Audit",
                        action: { Task { await store.audit(package: package.packageId) } }
                    )
                }
            }
            .padding(Theme.s16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityIdentifier("design.studio.audit")
    }

    /// A CLEAR blocked indicator: the activation was refused and the previous active
    /// package is kept. Shown with an icon + label + a semantic token (not colour
    /// alone), naming the package that REMAINS active.
    private func blockedIndicator(_ block: DesignActivationBlock) -> some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "hand.raised.fill")
                    .foregroundStyle(Theme.coral)
                Text("Activation blocked")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
                Button { store.dismissActivationBlock() } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(Theme.muted)
                }
                .buttonStyle(.plain)
            }
            Text("The audit failed, so “\(block.blockedPackageId)” was NOT activated. “\(block.keptActiveDisplay)” remains the active package.")
                .font(Theme.ui(11.5))
                .foregroundStyle(Theme.textSoft)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.coral.opacity(0.12))
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(Theme.coral.opacity(0.4), lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("design.studio.activation-blocked")
    }

    private func auditSummary(_ audit: DesignAuditReport) -> some View {
        HStack(spacing: Theme.s8) {
            // Pass/fail shown by icon + label + token, never colour alone.
            Image(systemName: audit.passed ? "checkmark.seal.fill" : "xmark.octagon.fill")
                .foregroundStyle(audit.passed ? Theme.accent : Theme.coral)
            Text(audit.passed ? "Audit passed" : "Audit failed")
                .font(Theme.ui(12.5, .semibold))
                .foregroundStyle(Theme.text)
            if audit.blocksActivation && !audit.passed {
                Text("· blocks activation")
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.coral)
            }
            Spacer()
            Text("\(audit.errors.count) error\(audit.errors.count == 1 ? "" : "s") · \(audit.warnings.count) warning\(audit.warnings.count == 1 ? "" : "s")")
                .font(Theme.mono(10.5))
                .foregroundStyle(Theme.muted)
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .accessibilityIdentifier("design.studio.audit-summary")
    }
}

private struct AuditKindGroup: View {
    let kind: DesignAuditFindingKind
    let findings: [DesignAuditFinding]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            HStack(spacing: 6) {
                Image(systemName: kind.symbol)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.muted)
                Text(kind.label)
                    .font(Theme.ui(11.5, .semibold))
                    .foregroundStyle(Theme.textSoft)
                Text("(\(findings.count))")
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.faint)
                Spacer()
            }
            ForEach(findings) { finding in
                AuditFindingRow(finding: finding)
            }
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityIdentifier("design.studio.audit-group.\(kind.rawValue)")
    }
}

private struct AuditFindingRow: View {
    let finding: DesignAuditFinding

    var body: some View {
        HStack(alignment: .top, spacing: Theme.s8) {
            // Severity by icon + label + token, never colour alone.
            HStack(spacing: 4) {
                Image(systemName: finding.severity.symbol)
                    .font(.system(size: 10, weight: .bold))
                Text(finding.severity.label)
                    .font(Theme.ui(9.5, .semibold))
            }
            .foregroundStyle(finding.severity.tint)
            .frame(width: 72, alignment: .leading)
            VStack(alignment: .leading, spacing: 2) {
                Text(finding.detail)
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.text)
                    .fixedSize(horizontal: false, vertical: true)
                Text(finding.refDisplay)
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.muted)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(finding.severity.label) \(finding.kind.label): \(finding.detail)")
        .accessibilityIdentifier("design.studio.finding.\(finding.id)")
    }
}

// MARK: - Revisions tab

struct RevisionsView: View {
    @ObservedObject var store: DesignStudioStore
    let package: DesignPackage

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Theme.s10) {
                Text("Revisions")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
                Button {
                    Task { await store.proposeRevision(package: package.packageId) }
                } label: {
                    Label("Propose Revision", systemImage: "plus.circle")
                }
                .buttonStyle(.primaryAction)
                .frame(maxWidth: 180)
                .disabled(store.isBusy)
                .accessibilityIdentifier("design.studio.propose-revision")
                .help("Propose a revision. Each revision is linked to a proof.")
            }
            .padding(Theme.s16)

            Divider().overlay(Theme.stroke)

            let revisions = store.revisionsByPackage[package.packageId] ?? []
            if revisions.isEmpty {
                EmptyStateView(
                    headline: "No revisions",
                    detail: "Propose a revision to start. Each revision is linked to a proof and can be accepted, rejected, or rolled back.",
                    systemImage: "clock.arrow.circlepath"
                )
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: Theme.s12) {
                        ForEach(revisions) { revision in
                            RevisionCard(
                                revision: revision,
                                isBusy: store.isBusy,
                                onAccept: { Task { await store.acceptRevision(revision.revisionId, package: package.packageId) } },
                                onReject: { Task { await store.rejectRevision(revision.revisionId, package: package.packageId) } },
                                onRollback: { Task { await store.rollbackRevision(revision.revisionId, package: package.packageId) } }
                            )
                        }
                    }
                    .padding(Theme.s16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityIdentifier("design.studio.revisions")
    }
}

private struct RevisionCard: View {
    let revision: DesignRevision
    let isBusy: Bool
    let onAccept: () -> Void
    let onReject: () -> Void
    let onRollback: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s10) {
            HStack(spacing: Theme.s8) {
                statePill
                Spacer()
                Text(revision.revisionId)
                    .font(Theme.mono(10.5))
                    .foregroundStyle(Theme.muted)
                    .textSelection(.enabled)
            }
            // Each revision shows its proof_ref.
            HStack(alignment: .firstTextBaseline, spacing: Theme.s8) {
                Label {
                    Text("Proof")
                        .font(Theme.ui(10.5, .semibold))
                        .foregroundStyle(Theme.muted)
                } icon: {
                    Image(systemName: "checkmark.seal")
                        .font(.system(size: 10))
                        .foregroundStyle(Theme.muted)
                }
                .frame(width: 70, alignment: .leading)
                Text(revision.proofRefDisplay)
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textSoft)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .accessibilityIdentifier("design.studio.revision.proof.\(revision.revisionId)")

            actions
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
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("design.studio.revision.\(revision.revisionId)")
    }

    private var statePill: some View {
        HStack(spacing: 5) {
            Image(systemName: revision.state.symbol)
                .font(.system(size: 9, weight: .bold))
            Text(revision.state.label)
                .font(Theme.ui(11, .semibold))
        }
        .foregroundStyle(revision.state.tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(revision.state.tint.opacity(0.14)))
        .overlay(Capsule().strokeBorder(revision.state.tint.opacity(0.3), lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(revision.state.label) revision")
    }

    @ViewBuilder
    private var actions: some View {
        HStack(spacing: Theme.s10) {
            if revision.state.isPending {
                Button(action: onAccept) {
                    Label("Accept", systemImage: "checkmark.circle")
                }
                .buttonStyle(.primaryAction)
                .disabled(isBusy)
                .accessibilityIdentifier("design.studio.revision.accept.\(revision.revisionId)")

                Button(action: onReject) {
                    Label("Reject", systemImage: "xmark.circle")
                }
                .buttonStyle(.destructiveAction)
                .disabled(isBusy)
                .accessibilityIdentifier("design.studio.revision.reject.\(revision.revisionId)")
            }
            if revision.state == .accepted {
                Button(action: onRollback) {
                    Label("Roll Back", systemImage: "arrow.uturn.backward")
                }
                .buttonStyle(.secondaryAction)
                .disabled(isBusy)
                .accessibilityIdentifier("design.studio.revision.rollback.\(revision.revisionId)")
            }
        }
    }
}
