// GitStatusView.swift — the Git studio surface (PR-034 reads + PR-035 LOCAL
// mutations).
//
// An ADAPTIVE region (no letterbox, no clipping): wide windows show flexible
// branch | changes | diff | commit columns; narrow windows (< 1200pt) collapse
// to a single segmented pane (Changes / Diff / Commit / Branches). The columns:
//   • the branch list  — current branch marked; upstream + ahead/behind shown;
//     a branch checked out in ANOTHER worktree is rendered occupied/disabled; a
//     full-tile Switch action runs a dirty-aware preflight (PR-035);
//   • the changes list — grouped staged / unstaged / untracked / conflicted with
//     rename arrows; each row carries a full-tile Stage / Unstage control, and a
//     secret / data-plane path is rendered NON-stageable with a clear reason;
//   • the diff pane    — reuses PR-033's `DiffHunkView` to render the git diff;
//   • the commit pane  — the reviewed-hash `CommitComposerView` (PR-035).
//
// There is NO push control anywhere — this PR is read-only-plus-LOCAL. Status is
// conveyed by icon + label + a semantic token, never colour alone. Full-tile
// rows fill the width; the region never letterboxes.

import SwiftUI

struct GitStatusView: View {
    @ObservedObject var store: GitStudioStore

    /// New-branch field state (the create-branch affordance in the branch column).
    @State private var newBranchName: String = ""
    /// Which single pane is shown in the compact (narrow) layout.
    @State private var compactPane: CompactPane = .changes

    /// Below this width the four columns would clip, so the view collapses to a
    /// single-pane tabbed layout (Appendix C rule 7: no fixed 4-column Git under
    /// a wide window).
    private let wideThreshold: CGFloat = 1200

    enum CompactPane: String, CaseIterable, Identifiable {
        case changes = "Changes"
        case diff = "Diff"
        case commit = "Commit"
        case branches = "Branches"
        var id: String { rawValue }
    }

    var body: some View {
        GeometryReader { geo in
            Group {
                if geo.size.width >= wideThreshold {
                    wideLayout
                } else {
                    compactLayout
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .opacity(store.stale ? 0.55 : 1)
        .animation(.easeInOut(duration: 0.15), value: store.stale)
        .overlay(alignment: .top) { errorBanner }
        .overlay(alignment: .top) { switchBlockBanner }
        .accessibilityIdentifier("git.studio.view")
        .task { await store.refreshPushStatus() }
    }

    /// Wide: branch | changes | diff | commit. Columns are flexible (min/ideal/
    /// max) rather than hard-fixed so they compress instead of clipping; the diff
    /// fills the remaining space.
    private var wideLayout: some View {
        HStack(spacing: 0) {
            branchColumn
                .frame(minWidth: 200, idealWidth: 256, maxWidth: 300)
            Divider().overlay(Theme.stroke)
            changesColumn
                .frame(minWidth: 260, idealWidth: 340, maxWidth: 380)
            Divider().overlay(Theme.stroke)
            diffColumn
                .frame(maxWidth: .infinity)
                .layoutPriority(1)
            Divider().overlay(Theme.stroke)
            CommitComposerView(store: store)
                .frame(minWidth: 280, idealWidth: 320, maxWidth: 340)
        }
    }

    /// Compact: one pane at a time, selected by a segmented control, each filling
    /// the full width so nothing clips at narrow window sizes.
    private var compactLayout: some View {
        VStack(spacing: 0) {
            Picker("Git pane", selection: $compactPane) {
                ForEach(CompactPane.allCases) { pane in
                    Text(pane.rawValue).tag(pane)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(8)
            .accessibilityIdentifier("git.studio.compact.tabs")
            Divider().overlay(Theme.stroke)
            if compactPane != .commit, !store.push.status.failures.isEmpty {
                PushFailureDiagnosticsPanel(failures: store.push.status.failures) {
                    compactPane = .commit
                }
                .padding(.horizontal, Theme.s8)
                .padding(.bottom, Theme.s8)
            }
            switch compactPane {
            case .changes: changesColumn
            case .diff: diffColumn
            case .commit: CommitComposerView(store: store)
            case .branches: branchColumn
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    // MARK: - Branch column

    private var branchColumn: some View {
        VStack(alignment: .leading, spacing: 0) {
            sectionHeader(title: "Branches", systemImage: "arrow.triangle.branch") { stalePill }
            Divider().overlay(Theme.stroke)
            newBranchField
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

    /// Create a LOCAL branch off the current HEAD (no push). The button is
    /// disabled until a non-empty name is typed.
    private var newBranchField: some View {
        HStack(spacing: Theme.s6) {
            TextField("New branch…", text: $newBranchName)
                .textFieldStyle(.plain)
                .font(Theme.ui(11.5))
                .foregroundStyle(Theme.text)
                .padding(.horizontal, Theme.s8)
                .padding(.vertical, Theme.s6)
                .background(
                    RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                        .fill(Theme.input)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                        .strokeBorder(Theme.stroke, lineWidth: 1)
                )
                .accessibilityIdentifier("git.branch.newField")
            Button {
                let name = newBranchName
                newBranchName = ""
                Task { await store.createBranch(name: name) }
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(Theme.accent)
            }
            .buttonStyle(IconTileButtonStyle(size: 28))
            .disabled(newBranchName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel("Create branch")
            .accessibilityIdentifier("git.branch.createButton")
        }
        .padding(.horizontal, Theme.s10)
        .padding(.vertical, Theme.s8)
    }

    private func branchRow(_ branch: GitBranchInfo) -> some View {
        let occupied = branch.isOccupiedElsewhere
        // The whole non-current, non-occupied tile is a Switch action. The current
        // branch and an occupied branch are not switch targets (the latter is
        // disabled, never silently forced).
        return Button {
            Task { await store.attemptSwitch(to: branch.name) }
        } label: {
            HStack(alignment: .center, spacing: Theme.s8) {
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
                if !branch.isCurrent && !occupied {
                    Image(systemName: "arrow.left.arrow.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.muted)
                }
            }
            .padding(.horizontal, Theme.s12)
            .padding(.vertical, Theme.s8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(branch.isCurrent ? Theme.accentTint : Color.clear)
            .opacity(occupied ? 0.7 : 1)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(branch.isCurrent || occupied)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(branchAccessibility(branch))
        .accessibilityHint(branch.isCurrent || occupied ? "" : "Switch to this branch")
        .accessibilityIdentifier("git.branch.row.\(branch.name)")
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
                        group("Conflicted", store.groups.conflicted, .conflicted)
                        group("Staged", store.groups.staged, .staged)
                        group("Unstaged", store.groups.unstaged, .unstaged)
                        group("Untracked", store.groups.untracked, .untracked)
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

    /// The kind of stage action a change group offers per row.
    private enum ChangeGroup {
        case staged    // row offers Unstage
        case unstaged  // row offers Stage
        case untracked // row offers Stage
        case conflicted // no stage action until resolved
    }

    @ViewBuilder
    private func group(_ title: String, _ entries: [GitStatusEntry], _ kind: ChangeGroup) -> some View {
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
                changeRow(entry, group: kind)
            }
        }
    }

    private func changeRow(_ entry: GitStatusEntry, group: ChangeGroup) -> some View {
        let selected = store.selectedPath == entry.path
        let rejection = store.rejection(for: entry.path)
        return HStack(alignment: .center, spacing: Theme.s4) {
            // The label tile selects the file + loads its diff (full-tile hit area).
            Button {
                store.select(entry)
            } label: {
                HStack(alignment: .center, spacing: Theme.s8) {
                    Image(systemName: entry.kind.symbol)
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(entry.kind.tint)
                        .frame(width: 16)
                    VStack(alignment: .leading, spacing: 1) {
                        fileName(entry)
                        if let rejection {
                            Text(rejection.reason.label)
                                .font(Theme.ui(9.5, .semibold))
                                .foregroundStyle(GeneratedDesignTokens.colorStatusDanger)
                        } else {
                            Text(entry.kind.label)
                                .font(Theme.ui(9.5))
                                .foregroundStyle(Theme.faint)
                        }
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

            stageControl(entry, group: group, rejection: rejection)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The per-row Stage / Unstage control. A secret / data-plane path (a
    /// recorded rejection) is rendered NON-stageable: the stage action is
    /// unavailable and a clear, non-actionable reason badge replaces it.
    @ViewBuilder
    private func stageControl(_ entry: GitStatusEntry, group: ChangeGroup, rejection: GitStageRejection?) -> some View {
        if let rejection {
            // Non-stageable: never offer a stage action; show the locked reason.
            HStack(spacing: 3) {
                Image(systemName: rejection.reason.symbol)
                    .font(.system(size: 9, weight: .bold))
                Text("Locked")
                    .font(Theme.ui(9.5, .semibold))
            }
            .foregroundStyle(GeneratedDesignTokens.colorStatusDanger)
            .padding(.horizontal, Theme.s6)
            .frame(minHeight: 30)
            .help(rejection.reason.explanation)
            .accessibilityIdentifier("git.change.locked.\(entry.path)")
            .accessibilityLabel("\(entry.path) is \(rejection.reason.label) and can never be staged")
        } else {
            switch group {
            case .staged:
                changeActionButton(
                    title: "Unstage", systemImage: "minus.circle",
                    tint: Theme.muted, id: "git.change.unstage.\(entry.path)"
                ) {
                    Task { await store.unstage(entry.path) }
                }
            case .unstaged, .untracked:
                changeActionButton(
                    title: "Stage", systemImage: "plus.circle",
                    tint: Theme.accent, id: "git.change.stage.\(entry.path)"
                ) {
                    Task { await store.stage(entry.path) }
                }
            case .conflicted:
                EmptyView()
            }
        }
    }

    private func changeActionButton(
        title: String, systemImage: String, tint: Color, id: String, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .labelStyle(.iconOnly)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(tint)
        }
        .buttonStyle(IconTileButtonStyle(size: 30))
        .help(title)
        .accessibilityLabel(title)
        .accessibilityIdentifier(id)
        .padding(.trailing, Theme.s8)
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

    /// The visible explanation when a branch switch was BLOCKED (a dirty
    /// worktree, a conflict, OR unsaved editor buffers). The operator must save /
    /// commit first and retry; there is deliberately no force button here.
    @ViewBuilder
    private var switchBlockBanner: some View {
        if let block = store.switchBlock {
            VStack(alignment: .leading, spacing: Theme.s6) {
                HStack(spacing: Theme.s8) {
                    Image(systemName: "hand.raised.fill")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(GeneratedDesignTokens.colorStatusWarning)
                    Text(block.summary)
                        .font(Theme.ui(11.5, .semibold))
                        .foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                    Button {
                        store.dismissSwitchBlock()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundStyle(Theme.muted)
                    }
                    .buttonStyle(IconTileButtonStyle(size: 24))
                    .accessibilityLabel("Dismiss")
                }
                ForEach(block.blockers) { blocker in
                    HStack(alignment: .top, spacing: Theme.s6) {
                        Image(systemName: blocker.kind.symbol)
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(Theme.muted)
                            .frame(width: 14)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(blocker.kind.label)
                                .font(Theme.ui(10.5, .semibold))
                                .foregroundStyle(Theme.textSoft)
                            Text(blocker.explanation)
                                .font(Theme.ui(10))
                                .foregroundStyle(Theme.faint)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Spacer(minLength: 0)
                    }
                }
                Text("Save or commit your changes, then try the switch again. (No forced switch.)")
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.muted)
            }
            .padding(Theme.s12)
            .frame(maxWidth: 520, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard)
                    .fill(Theme.panel)
            )
            .overlay(
                RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard)
                    .strokeBorder(GeneratedDesignTokens.colorStatusWarning.opacity(0.4), lineWidth: 1)
            )
            .padding(Theme.s12)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("git.switch.blockBanner")
        }
    }
}
