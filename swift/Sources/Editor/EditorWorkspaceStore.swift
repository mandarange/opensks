// EditorWorkspaceStore.swift — the @MainActor owner of open documents/tabs
// (PR-032).
//
// Tabs are keyed by a stable `EditorDocumentID`. Opening a path that is already
// open FOCUSES the existing tab (same identity, no duplicate, cursor preserved)
// rather than re-reading it. A dirty tab is never silently evicted: the tab cap
// only drops the oldest CLEAN tab, and close() with dirty protection refuses to
// drop unsaved work unless the caller forces it. Save reconciles the document's
// baseline; a `file_changed_on_disk` from the service becomes a conflict on the
// document, never a silent overwrite.

import SwiftUI

@MainActor
final class EditorWorkspaceStore: ObservableObject {
    /// Open documents in tab order (left → right).
    @Published private(set) var documents: [EditorDocumentState] = []
    /// The focused document's identity.
    @Published var activeDocumentID: EditorDocumentID?
    /// A non-fatal banner for the last failed open (e.g. binary/secret/missing).
    @Published var openError: String?

    /// Per-document diff hunks (added/removed gutter markers), keyed by document
    /// identity. Recomputed on demand from the editor buffer vs the on-disk file.
    @Published private(set) var diffByDocument: [EditorDocumentID: TextDiffResponse] = [:]

    let service: EditorFileService
    private let maxTabs: Int
    /// Maps a canonical workspace-relative path → its stable document identity so
    /// re-opening focuses instead of duplicating.
    private var idByPath: [String: EditorDocumentID] = [:]

    init(service: EditorFileService, maxTabs: Int = 12) {
        self.service = service
        self.maxTabs = maxTabs
    }

    var activeDocument: EditorDocumentState? {
        guard let id = activeDocumentID else { return nil }
        return documents.first { $0.id == id }
    }

    func document(for id: EditorDocumentID) -> EditorDocumentState? {
        documents.first { $0.id == id }
    }

    // MARK: - Open

    /// Open (or focus) a workspace-relative path. Returns the document on success.
    @discardableResult
    func open(path: String) async -> EditorDocumentState? {
        let canonical = Self.canonicalize(path)

        // Already open → focus the existing tab; do NOT re-read or duplicate.
        if let existingID = idByPath[canonical],
           let existing = documents.first(where: { $0.id == existingID }) {
            activeDocumentID = existingID
            return existing
        }

        do {
            let response = try await service.open(path: canonical)
            let snapshot = response.makeSnapshot(
                displayName: Self.displayName(for: canonical),
                language: CodeLang.detect(canonical)
            )
            let id = EditorDocumentID()
            let doc = EditorDocumentState(id: id, snapshot: snapshot, text: response.content)
            idByPath[canonical] = id
            documents.append(doc)
            activeDocumentID = id
            openError = nil
            evictExcessCleanTabs()
            return doc
        } catch let error as EditorFileServiceError {
            openError = "\(Self.displayName(for: canonical)): \(error.message)"
            return nil
        } catch {
            openError = "\(Self.displayName(for: canonical)): \(error.localizedDescription)"
            return nil
        }
    }

    // MARK: - Save

    /// Save the active document.
    @discardableResult
    func save() async -> Bool {
        guard let doc = activeDocument else { return false }
        return await save(doc)
    }

    /// Save every dirty, editable document.
    func saveAll() async {
        for doc in documents where doc.isDirty {
            _ = await save(doc)
        }
    }

    @discardableResult
    func save(_ doc: EditorDocumentState) async -> Bool {
        await save(doc, force: false)
    }

    /// Save `doc`. With `force`, the save re-baselines to the current on-disk
    /// state first (Keep Mine resolution) so the editor's buffer deliberately
    /// overwrites the external change. A normal (`force: false`) save NEVER
    /// silently overwrites: a divergent on-disk file routes to a conflict.
    @discardableResult
    func save(_ doc: EditorDocumentState, force: Bool) async -> Bool {
        guard doc.isEditable else { return false }
        if force {
            // Re-baseline to the current on-disk hash so the optimistic-concurrency
            // check passes and the deliberate overwrite goes through.
            if let stat = try? await service.stat(path: doc.workspaceRelativePath) {
                doc.adoptForcedBaseline(
                    newHash: stat.contentHash ?? doc.baselineContentHash,
                    newMtimeMs: stat.modificationMs
                )
            }
        }
        guard doc.isDirty else {
            doc.conflictState = nil
            return true
        }
        let path = doc.workspaceRelativePath
        let content = doc.text
        let expectedHash = doc.baselineContentHash
        let expectedMtime = doc.onDiskModificationMs
        doc.markSaving()
        do {
            let result = try await service.save(
                path: path,
                content: content,
                expectedHash: expectedHash,
                expectedMtime: expectedMtime
            )
            doc.adoptSavedBaseline(newHash: result.newHash, newMtimeMs: result.newMtimeMs)
            // Incremental, single-file re-index — NOT a workspace re-scan. Failure
            // here is non-fatal: the save already succeeded.
            _ = try? await service.codegraphUpdate(path: path)
            // A clean save clears any stale diff markers.
            diffByDocument[doc.id] = nil
            return true
        } catch let error as EditorFileServiceError {
            switch error {
            case .conflict(let message):
                // The user's edits are preserved verbatim; surface a conflict.
                doc.markConflict(message)
            default:
                doc.markSaveFailed(error.message)
            }
            return false
        } catch {
            doc.markSaveFailed(error.localizedDescription)
            return false
        }
    }

    // MARK: - Close

    /// Close a tab. With `dirtyProtection`, a dirty tab is refused (returns
    /// false) so the caller can prompt; `force` overrides.
    @discardableResult
    func close(_ id: EditorDocumentID, dirtyProtection: Bool = true, force: Bool = false) -> Bool {
        guard let idx = documents.firstIndex(where: { $0.id == id }) else { return false }
        let doc = documents[idx]
        if dirtyProtection && !force && doc.isDirty {
            return false
        }
        removeDocument(at: idx)
        return true
    }

    func closeActive(dirtyProtection: Bool = true) -> Bool {
        guard let id = activeDocumentID else { return false }
        return close(id, dirtyProtection: dirtyProtection)
    }

    // MARK: - Conflict handling (PR-033: never a silent overwrite)

    /// KEEP MINE: keep the editor buffer and force a save that deliberately
    /// overwrites the external change. Re-baselines to the current disk hash,
    /// then saves so the optimistic-concurrency check passes with full intent.
    @discardableResult
    func resolveConflictKeepingMine(_ doc: EditorDocumentState) async -> Bool {
        await save(doc, force: true)
    }

    /// RELOAD (take disk): discard the editor's edits and adopt the on-disk
    /// version, re-baselining so the doc is clean.
    func resolveConflictTakingDisk(_ doc: EditorDocumentState) async {
        if let response = try? await service.open(path: doc.workspaceRelativePath) {
            doc.text = response.content
            doc.adoptSavedBaseline(newHash: response.contentHash, newMtimeMs: response.onDiskModificationMs)
            doc.conflictState = nil
            doc.saveState = .clean
            diffByDocument[doc.id] = nil
        }
    }

    // MARK: - Diff gutter

    /// Recompute the on-disk-vs-buffer diff for `doc` and publish its hunks so
    /// the gutter can render added/removed markers. Returns the response.
    @discardableResult
    func refreshDiff(_ doc: EditorDocumentState) async -> TextDiffResponse? {
        guard doc.isEditable else { return nil }
        guard let response = try? await service.diff(
            path: doc.workspaceRelativePath,
            currentBuffer: doc.text
        ) else { return nil }
        diffByDocument[doc.id] = response.changed ? response : nil
        return response
    }

    func diff(for id: EditorDocumentID) -> TextDiffResponse? {
        diffByDocument[id]
    }

    // MARK: - External-change watcher (branch switch / external edit)

    /// Poll the on-disk state of every open, editable, non-conflicted document.
    /// A doc whose on-disk hash diverged from its baseline (an external edit or a
    /// branch switch) is flagged as a conflict so the divergence is made visible
    /// BEFORE the user tries to save over it. Best-effort: a stat/working-change
    /// failure leaves the doc untouched.
    func pollExternalChanges() async {
        for doc in documents where doc.isEditable {
            // A doc already in conflict, or with no save target, is skipped.
            if doc.conflictState != nil { continue }
            await checkExternalChange(doc)
        }
    }

    /// Check a single document for an external/working-tree divergence.
    func checkExternalChange(_ doc: EditorDocumentState) async {
        let baseline = doc.baselineContentHash
        // Prefer the git working-change signal (catches branch switches); fall
        // back to a plain stat hash comparison when not in a repo.
        if let wc = try? await service.workingChange(
            path: doc.workspaceRelativePath, baselineHash: baseline
        ), wc.inRepo {
            if wc.changed, let current = wc.currentHash, current != baseline {
                doc.markConflict("This file changed on disk since you opened it.")
            }
            return
        }
        if let stat = try? await service.stat(path: doc.workspaceRelativePath),
           let onDisk = stat.contentHash, onDisk != baseline {
            doc.markConflict("This file changed on disk since you opened it.")
        }
    }

    // MARK: - Internals

    private func removeDocument(at idx: Int) {
        let doc = documents[idx]
        idByPath[doc.workspaceRelativePath] = nil
        diffByDocument[doc.id] = nil
        let wasActive = activeDocumentID == doc.id
        documents.remove(at: idx)
        if wasActive {
            // Focus the neighbour that took this slot, else the new last tab.
            if documents.indices.contains(idx) {
                activeDocumentID = documents[idx].id
            } else {
                activeDocumentID = documents.last?.id
            }
        }
    }

    /// Enforce the tab cap WITHOUT ever dropping a dirty tab. Only the oldest
    /// clean, non-active tabs are evicted.
    private func evictExcessCleanTabs() {
        guard documents.count > maxTabs else { return }
        var overflow = documents.count - maxTabs
        var i = 0
        while overflow > 0 && i < documents.count {
            let doc = documents[i]
            if !doc.isDirty && doc.id != activeDocumentID {
                idByPath[doc.workspaceRelativePath] = nil
                documents.remove(at: i)
                overflow -= 1
            } else {
                i += 1
            }
        }
    }

    static func canonicalize(_ path: String) -> String {
        // Normalize redundant separators; keep workspace-relative semantics.
        let trimmed = path.hasPrefix("./") ? String(path.dropFirst(2)) : path
        return (trimmed as NSString).standardizingPath
    }

    static func displayName(for path: String) -> String {
        (path as NSString).lastPathComponent
    }
}
