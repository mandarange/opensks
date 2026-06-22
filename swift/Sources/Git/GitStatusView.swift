// GitStatusView.swift — the READ-ONLY Git studio surface (PR-034).
//
// Three columns inside one dark, token-driven, full-width region (no letterbox):
//   • the branch list  — current branch marked; upstream + ahead/behind shown;
//     a branch checked out in ANOTHER worktree is rendered occupied/disabled;
//   • the changes list — grouped staged / unstaged / untracked / conflicted with
//     rename arrows; selecting a file loads its diff;
//   • the diff pane    — reuses PR-033's `DiffHunkView` to render the git diff.
//
// There are NO commit/stage/switch/push controls anywhere here — this PR is
// read-only. Status is conveyed by icon + label + a semantic token, never colour
// alone. Full-tile rows fill the width; the region never letterboxes.

import SwiftUI

struct GitStatusView: View {
    @ObservedObject var store: GitStudioStore

    var body: some View {
        HStack(spacing: 0) {
            branchColumn
                .frame(width: 248)
            Divider().overlay(Theme.stroke)
            changesColumn
                .frame(width: 320)
            Divider().overlay(Theme.stroke)
            diffColumn
                .frame(maxWidth: .infinity)
                .layoutPriority(1)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .opacity(store.stale ? 0.55 : 1)
        .animation(.easeInOut(duration: 0.15), value: store.stale)
        .overlay(alignment: .top) { errorBanner }
        .accessibilityIdentifier("git.studio.view")
    }

    // MARK: - Branch column

    private var branchColumn: some View {
        VStack(alignment: .leading, spacing: 0) {
            sectionHeader(title: "Branches", systemImage: "arrow.triangle.branch") { stalePill }
            Divider().overlay(Theme.stroke)
            if store.branches.branches.isEmpty {
                emptyHint("No local branches", systemImage: "arrow.triangle.branch")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(store.branches.branches) { branch in
                            branchRow(branch)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.sidebar)
    }

    private func branchRow(_ branch: GitBranchInfo) -> some View {
        let occupied = branch.isOccupiedElsewhere
        return HStack(alignment: .center, spacing: Theme.s8) {
            Image(systemName: branch.isCurrent ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(branch.isCurrent ? Theme.accent : Theme.faint)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: Theme.s6) {
                    Text(branch.name)
                        .font(Theme.ui(12, branch.isCurrent ? .semibold : .regular))
                        .foregroundStyle(occupied ? Theme.muted : Theme.text)
                        .lineLimit(1)
                    if occupied {
                        occupiedBadge
                    }
                }
                branchTracking(branch)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(branch.isCurrent ? Theme.accentTint : Color.clear)
        .opacity(occupied ? 0.7 : 1)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(branchAccessibility(branch))
    }

    private var occupiedBadge: some View {
        HStack(spacing: 3) {
            Image(systemName: "lock.fill")
                .font(.system(size: 8, weight: .bold))
            Text("in use")
                .font(Theme.ui(9.5, .semibold))
        }
        .foregroundStyle(Theme.muted)
        .padding(.horizontal, 5)
        .padding(.vertical, 1)
        .background(Capsule().fill(Theme.muted.opacity(0.14)))
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private func branchTracking(_ branch: GitBranchInfo) -> some View {
        HStack(spacing: Theme.s8) {
            if let upstream = branch.upstream {
                Text(upstream)
                    .font(Theme.mono(10))
                    .foregroundStyle(Theme.faint)
                    .lineLimit(1)
            } else {
                Text("no upstream")
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.faint)
            }
            if branch.ahead > 0 {
                aheadBehind(symbol: "arrow.up", count: branch.ahead)
            }
            if branch.behind > 0 {
                aheadBehind(symbol: "arrow.down", count: branch.behind)
            }
        }
    }

    private func aheadBehind(symbol: String, count: Int) -> some View {
        HStack(spacing: 1) {
            Image(systemName: symbol).font(.system(size: 8, weight: .bold))
            Text("\(count)").font(Theme.mono(9.5, .semibold))
        }
        .foregroundStyle(Theme.textSoft)
    }

    private func branchAccessibility(_ branch: GitBranchInfo) -> String {
        var parts: [String] = ["Branch \(branch.name)"]
        if branch.isCurrent { parts.append("current") }
        if let upstream = branch.upstream { parts.append("tracking \(upstream)") }
        if branch.ahead > 0 { parts.append("\(branch.ahead) ahead") }
        if branch.behind > 0 { parts.append("\(branch.behind) behind") }
        if branch.isOccupiedElsewhere { parts.append("checked out in another worktree, occupied") }
        return parts.joined(separator: ", ")
    }

    // MARK: - Changes column

    private var changesColumn: some View {
        VStack(alignment: .leading, spacing: 0) {
            sectionHeader(title: changesTitle, systemImage: "doc.text.magnifyingglass") { dirtyPill }
            Divider().overlay(Theme.stroke)
            if !store.status.inRepo {
                emptyHint("Not a Git repository", systemImage: "questionmark.folder")
            } else if store.groups.isEmpty {
                emptyHint("Working tree clean", systemImage: "checkmark.seal")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        group("Conflicted", store.groups.conflicted)
                        group("Staged", store.groups.staged)
                        group("Unstaged", store.groups.unstaged)
                        group("Untracked", store.groups.untracked)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.explorer)
    }

    private var changesTitle: String {
        guard store.status.inRepo else { return "Changes" }
        let branch = store.status.detached
            ? "detached"
            : (store.status.branch ?? "—")
        return "Changes · \(branch)"
    }

    @ViewBuilder
    private func group(_ title: String, _ entries: [GitStatusEntry]) -> some View {
        if !entries.isEmpty {
            HStack(spacing: Theme.s6) {
                Text(title.uppercased())
                    .font(Theme.ui(9.5, .bold))
                    .foregroundStyle(Theme.muted)
                Text("\(entries.count)")
                    .font(Theme.mono(9.5, .semibold))
                    .foregroundStyle(Theme.faint)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.s12)
            .padding(.top, Theme.s10)
            .padding(.bottom, Theme.s4)
            ForEach(entries) { entry in
                changeRow(entry)
            }
        }
    }

    private func changeRow(_ entry: GitStatusEntry) -> some View {
        let selected = store.selectedPath == entry.path
        return Button {
            store.select(entry)
        } label: {
            HStack(alignment: .center, spacing: Theme.s8) {
                Image(systemName: entry.kind.symbol)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(entry.kind.tint)
                    .frame(width: 16)
                VStack(alignment: .leading, spacing: 1) {
                    fileName(entry)
                    Text(entry.kind.label)
                        .font(Theme.ui(9.5))
                        .foregroundStyle(Theme.faint)
                }
                Spacer(minLength: 0)
                Text("\(entry.indexStatus)\(entry.worktreeStatus)")
                    .font(Theme.mono(9.5, .semibold))
                    .foregroundStyle(Theme.faint)
            }
            .padding(.horizontal, Theme.s12)
            .padding(.vertical, Theme.s6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? Theme.accentTint : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(changeAccessibility(entry))
        .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
    }

    @ViewBuilder
    private func fileName(_ entry: GitStatusEntry) -> some View {
        if entry.isRename, let orig = entry.origPath {
            HStack(spacing: Theme.s4) {
                Text((orig as NSString).lastPathComponent)
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
                Image(systemName: "arrow.right")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundStyle(entry.kind.tint)
                Text((entry.path as NSString).lastPathComponent)
                    .font(Theme.ui(11.5, .medium))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
            }
        } else {
            Text((entry.path as NSString).lastPathComponent)
                .font(Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
        }
    }

    private func changeAccessibility(_ entry: GitStatusEntry) -> String {
        var parts: [String] = [entry.kind.label]
        if entry.isRename, let orig = entry.origPath {
            parts.append("\(orig) renamed to \(entry.path)")
        } else {
            parts.append(entry.path)
        }
        return parts.joined(separator: ", ")
    }

    // MARK: - Diff column

    private var diffColumn: some View {
        Group {
            if let file = store.selectedDiffFile {
                if file.isBinary {
                    emptyHint("Binary file — no text diff", systemImage: "doc.badge.ellipsis")
                } else {
                    DiffHunkView(title: diffTitle(file), lines: file.displayLines)
                }
            } else if store.selectedPath != nil {
                emptyHint("No textual changes", systemImage: "text.alignleft")
            } else {
                EmptyStateView(
                    headline: "Select a change",
                    detail: "Pick a file from the changes list to view its diff. This studio is read-only — there are no commit or push controls here.",
                    systemImage: "arrow.left.doc.text"
                )
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.panel)
    }

    private func diffTitle(_ file: GitDiffFile) -> String {
        if let orig = file.origPath, orig != file.path {
            return "\(orig) → \(file.path)"
        }
        return file.path
    }

    // MARK: - Shared chrome

    private func sectionHeader<Trailing: View>(
        title: String,
        systemImage: String,
        @ViewBuilder trailing: () -> Trailing
    ) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: systemImage)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.muted)
            Text(title)
                .font(Theme.ui(11.5, .semibold))
                .foregroundStyle(Theme.textSoft)
                .lineLimit(1)
            Spacer(minLength: 0)
            trailing()
        }
        .padding(.horizontal, Theme.s12)
        .padding(.vertical, Theme.s8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.titlebarBottom)
    }

    @ViewBuilder
    private var stalePill: some View {
        if store.stale {
            StatusPill(kind: .running, label: "Refreshing")
        }
    }

    @ViewBuilder
    private var dirtyPill: some View {
        if store.status.inRepo {
            if store.status.isDirty {
                StatusPill(kind: .warning, label: "Dirty")
            } else {
                StatusPill(kind: .success, label: "Clean")
            }
        }
    }

    private func emptyHint(_ text: String, systemImage: String) -> some View {
        VStack(spacing: Theme.s8) {
            Image(systemName: systemImage)
                .font(.system(size: 22))
                .foregroundStyle(Theme.faint)
            Text(text)
                .font(Theme.ui(11.5))
                .foregroundStyle(Theme.muted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.s20)
    }

    @ViewBuilder
    private var errorBanner: some View {
        if let error = store.loadError {
            HStack(spacing: Theme.s8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(GeneratedDesignTokens.colorStatusDanger)
                Text(error)
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.textSoft)
                    .lineLimit(2)
            }
            .padding(.horizontal, Theme.s12)
            .padding(.vertical, Theme.s8)
            .background(
                RoundedRectangle(cornerRadius: Theme.rMd)
                    .fill(GeneratedDesignTokens.colorStatusDanger.opacity(0.12))
            )
            .padding(Theme.s12)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Git error: \(error)")
        }
    }
}
