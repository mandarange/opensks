// EditorWorkspaceView.swift — the editable code workspace (PR-032).
//
// Three stacked regions: a TAB BAR whose every tab is a full-tile Button (the
// whole tile — background included — selects the tab, fixing the "only the label
// is clickable" defect), the ACTIVE document's editor filling the center (fixing
// the "tab exists but center empty" bug — the center always renders the active
// document), and a STATUS BAR (line/col, encoding, dirty). There is NO fixed
// maxWidth anywhere on the editor, so the surface fills the window with no
// letterbox. Secret / binary / oversized documents render read-only behind a
// clear banner.

import SwiftUI
import AppKit

struct EditorWorkspaceView: View {
    @ObservedObject var store: EditorWorkspaceStore

    var body: some View {
        VStack(spacing: 0) {
            tabBar
            Divider().overlay(Theme.stroke)
            center
            Divider().overlay(Theme.stroke)
            statusBar
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.editor)
        .accessibilityIdentifier("editor.workspace")
    }

    // MARK: - Tab bar

    private var tabBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(store.documents) { doc in
                    EditorTabView(
                        document: doc,
                        isActive: store.activeDocumentID == doc.id,
                        onSelect: { store.activeDocumentID = doc.id },
                        onClose: { _ = store.close(doc.id) }
                    )
                }
            }
            .padding(.horizontal, Theme.s8)
            .padding(.vertical, Theme.s6)
        }
        .frame(height: 38)
        .frame(maxWidth: .infinity)
        .background(Theme.sidebar)
    }

    // MARK: - Center (the active document's editor)

    @ViewBuilder
    private var center: some View {
        if let doc = store.activeDocument {
            EditorDocumentPane(store: store, document: doc)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("editor.center.\(doc.id.raw.uuidString)")
        } else {
            EmptyStateView(
                headline: "No file open",
                detail: "Open a file from the Explorer to start editing. Cmd-S saves, Cmd-F finds, Cmd-W closes the tab.",
                systemImage: "doc.text"
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityIdentifier("editor.center.empty")
        }
    }

    // MARK: - Status bar

    private var statusBar: some View {
        Group {
            if let doc = store.activeDocument {
                EditorStatusBar(document: doc)
            } else {
                HStack {
                    Text("Ready")
                        .font(Theme.ui(10.5))
                        .foregroundStyle(Theme.muted)
                    Spacer()
                }
                .padding(.horizontal, Theme.s12)
                .frame(height: 24)
            }
        }
        .frame(maxWidth: .infinity)
        .background(Theme.sidebar)
    }
}

// MARK: - One tab (full-tile button)

private struct EditorTabView: View {
    @ObservedObject var document: EditorDocumentState
    let isActive: Bool
    let onSelect: () -> Void
    let onClose: () -> Void

    var body: some View {
        // The whole tile is one Button so clicking the background — not just the
        // label — selects the tab. The close control is a sibling Button so both
        // are independently hittable (no onTapGesture anywhere).
        Button(action: onSelect) {
            HStack(spacing: 7) {
                Circle()
                    .fill(document.language.dotColor)
                    .frame(width: 6, height: 6)
                Text(document.displayName)
                    .font(Theme.ui(11.5, isActive ? .medium : .regular))
                    .foregroundStyle(isActive ? Theme.text : Theme.muted)
                if document.isDirty {
                    Circle()
                        .fill(Theme.accent)
                        .frame(width: 6, height: 6)
                        .accessibilityLabel("unsaved changes")
                }
                Button(action: onClose) {
                    Image(systemName: "xmark")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(Theme.muted)
                        .frame(width: 16, height: 16)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Close \(document.displayName)")
            }
            .padding(.horizontal, Theme.s10)
            .padding(.vertical, Theme.s6)
            .frame(maxHeight: .infinity)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm)
                    .fill(isActive ? Theme.editor : Color.clear)
            )
            .overlay(alignment: .top) {
                if isActive {
                    Rectangle().fill(Theme.accent).frame(height: 2)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Open \(document.displayName)")
        .accessibilityAddTraits(isActive ? [.isSelected] : [])
    }
}

// MARK: - The editor pane for one document (editor + banner)

private struct EditorDocumentPane: View {
    @ObservedObject var store: EditorWorkspaceStore
    @ObservedObject var document: EditorDocumentState

    private var gutterMarkers: [Int: DiffGutterMarker] {
        guard let response = store.diff(for: document.id) else { return [:] }
        return DiffGutter.markers(from: response)
    }

    var body: some View {
        VStack(spacing: 0) {
            if let banner = readOnlyBanner {
                bannerView(banner.text, systemImage: banner.symbol, tint: banner.tint)
            }
            if document.conflictState != nil {
                // An external edit is NEVER silently overwritten: the full
                // resolution surface replaces the editor until the user decides.
                ConflictResolutionView(store: store, document: document)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                CodeEditorRepresentable(document: document, diffMarkers: gutterMarkers)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .task(id: document.currentContentHash) {
                        // Recompute gutter markers whenever the buffer changes.
                        await store.refreshDiff(document)
                    }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.editor)
    }

    private var readOnlyBanner: (text: String, symbol: String, tint: Color)? {
        if document.snapshot.isSecretRestricted {
            return ("Read-only — this path may contain credentials and is restricted from editing.",
                    "lock.fill", Theme.gold)
        }
        if document.snapshot.isBinary {
            return ("Read-only — binary file, editing is not available.",
                    "doc.badge.ellipsis", Theme.muted)
        }
        return nil
    }

    private func bannerView(_ text: String, systemImage: String, tint: Color) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: systemImage).font(.system(size: 11, weight: .semibold))
            Text(text).font(Theme.ui(11)).foregroundStyle(Theme.textSoft)
            Spacer(minLength: 0)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.12))
    }

}

// MARK: - Status bar for the active document

private struct EditorStatusBar: View {
    @ObservedObject var document: EditorDocumentState

    var body: some View {
        HStack(spacing: Theme.s12) {
            Text(document.workspaceRelativePath)
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.faint)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: Theme.s12)
            Text(saveLabel)
                .font(Theme.ui(10.5, .medium))
                .foregroundStyle(saveTint)
            Text(document.language.label)
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.muted)
            Text(document.snapshot.encoding.uppercased())
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.muted)
            Text(document.snapshot.lineEnding.label)
                .font(Theme.ui(10.5))
                .foregroundStyle(Theme.muted)
        }
        .padding(.horizontal, Theme.s12)
        .frame(height: 24)
    }

    private var saveLabel: String {
        switch document.saveState {
        case .clean: return "Saved"
        case .editing: return "Modified"
        case .saving: return "Saving…"
        case .saved: return "Saved"
        case .saveFailed: return "Save failed"
        case .conflict: return "Conflict"
        case .readOnly: return "Read-only"
        case .restricted: return "Restricted"
        }
    }

    private var saveTint: Color {
        switch document.saveState {
        case .clean, .saved: return Theme.muted
        case .editing, .saving: return Theme.accent
        case .saveFailed, .conflict: return Theme.coral
        case .readOnly, .restricted: return Theme.gold
        }
    }
}
