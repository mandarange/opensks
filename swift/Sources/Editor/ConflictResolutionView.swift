// ConflictResolutionView.swift — non-destructive resolution of an external edit
// (PR-033).
//
// Shown when a document is in a conflict state: the on-disk file moved past the
// editor's baseline (an external edit, an agent write, or a branch switch),
// surfaced proactively by the store's watcher poll. An external edit is NEVER
// silently overwritten — every save over a changed file routes through here.
//
// The view is a three-way compare (BASELINE the file when opened, ON-DISK what
// it is now, CURRENT the editor buffer) with three explicit full-tile actions:
//   • Reload    — take disk, discard local edits, re-baseline (clean).
//   • Keep Mine — keep the buffer, force a save that re-baselines + overwrites.
//   • Compare   — reveal the on-disk-vs-buffer diff inline.
// All colours are semantic tokens; the surface is dark and fills its width.

import SwiftUI

struct ConflictResolutionView: View {
    @ObservedObject var store: EditorWorkspaceStore
    @ObservedObject var document: EditorDocumentState

    /// When true the inline diff (on-disk vs current buffer) is revealed.
    @State private var showingCompare = false
    /// The fetched on-disk content for the three-way panes (lazy).
    @State private var onDiskText: String = ""
    @State private var diffResponse: TextDiffResponse?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            banner
            Divider().overlay(Theme.stroke)
            if showingCompare {
                compareSection
                Divider().overlay(Theme.stroke)
            } else {
                threeWaySummary
                Divider().overlay(Theme.stroke)
            }
            actions
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.panel)
        .accessibilityIdentifier("editor.conflict.resolution")
        .task(id: document.id) { await loadOnDisk() }
    }

    // MARK: - Banner

    private var banner: some View {
        HStack(spacing: Theme.s10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Theme.coral)
            VStack(alignment: .leading, spacing: 2) {
                Text("This file changed on disk since you opened it.")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Text(document.conflictState?.message
                     ?? "Your edits are preserved. Choose how to resolve — nothing is overwritten until you decide.")
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.s16)
        .padding(.vertical, Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.coral.opacity(0.12))
    }

    // MARK: - Three-way summary (baseline / on-disk / current)

    private var threeWaySummary: some View {
        HStack(spacing: 0) {
            paneColumn(
                title: "Baseline",
                caption: "When opened",
                hash: document.snapshotBaselineLabel,
                tint: Theme.muted
            )
            Divider().overlay(Theme.stroke)
            paneColumn(
                title: "On disk",
                caption: "Now",
                hash: EditorContentHash.compute(onDiskText),
                tint: Theme.gold
            )
            Divider().overlay(Theme.stroke)
            paneColumn(
                title: "Your buffer",
                caption: "Unsaved edits",
                hash: document.currentContentHash,
                tint: Theme.accent
            )
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.editor)
    }

    private func paneColumn(title: String, caption: String, hash: String, tint: Color) -> some View {
        VStack(alignment: .leading, spacing: Theme.s4) {
            HStack(spacing: Theme.s6) {
                Circle().fill(tint).frame(width: 6, height: 6)
                Text(title).font(Theme.ui(11.5, .semibold)).foregroundStyle(Theme.textSoft)
            }
            Text(caption).font(Theme.ui(10)).foregroundStyle(Theme.muted)
            Text(shortHash(hash))
                .font(Theme.mono(10))
                .foregroundStyle(Theme.faint)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func shortHash(_ hash: String) -> String {
        let body = hash.hasPrefix(EditorContentHash.prefix)
            ? String(hash.dropFirst(EditorContentHash.prefix.count))
            : hash
        return "#" + String(body.prefix(10))
    }

    // MARK: - Compare (inline diff)

    @ViewBuilder
    private var compareSection: some View {
        if let response = diffResponse, response.changed {
            DiffHunkView(title: "On disk → your buffer", response: response)
        } else {
            DiffHunkView(
                title: "On disk → your buffer",
                lines: [DiffDisplayLine(id: 0, kind: .context,
                                        text: "No textual difference between the on-disk file and your buffer.")]
            )
        }
    }

    // MARK: - Actions (full-tile, token-driven)

    private var actions: some View {
        HStack(spacing: Theme.s12) {
            Button {
                Task { await store.resolveConflictTakingDisk(document) }
            } label: {
                Label("Reload", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.secondaryAction)
            .accessibilityIdentifier("editor.conflict.reload")

            Button {
                Task { await store.resolveConflictKeepingMine(document) }
            } label: {
                Label("Keep Mine", systemImage: "checkmark.shield")
            }
            .buttonStyle(.primaryAction)
            .accessibilityIdentifier("editor.conflict.keepMine")

            Button {
                showingCompare.toggle()
                if showingCompare { Task { await loadDiff() } }
            } label: {
                Label(showingCompare ? "Hide Compare" : "Compare",
                      systemImage: "rectangle.split.2x1")
            }
            .buttonStyle(.quietAction)
            .accessibilityIdentifier("editor.conflict.compare")
        }
        .padding(.horizontal, Theme.s16)
        .padding(.vertical, Theme.s12)
        .frame(maxWidth: .infinity)
        .background(Theme.sidebar)
    }

    // MARK: - Loading

    private func loadOnDisk() async {
        if let response = try? await store.service.open(path: document.workspaceRelativePath) {
            onDiskText = response.content
        }
    }

    private func loadDiff() async {
        diffResponse = await store.refreshDiff(document)
    }
}

// MARK: - Document baseline label helper

extension EditorDocumentState {
    /// The baseline hash this document is reconciled against (label form).
    var snapshotBaselineLabel: String { baselineContentHash }
}
