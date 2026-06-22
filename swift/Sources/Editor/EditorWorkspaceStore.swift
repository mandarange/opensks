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

    private let service: EditorFileService
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
        guard doc.isEditable else { return false }
        guard doc.isDirty else { return true }
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

    // MARK: - Conflict handling

    /// Keep the editor's version: re-baseline to the current text so the next
    /// save force-overwrites is NOT performed silently here — instead we clear
    /// the conflict and let the user re-save explicitly against a fresh stat.
    func resolveConflictKeepingMine(_ doc: EditorDocumentState) async {
        // Re-stat to learn the new on-disk baseline, then keep the user's text as
        // dirty against it so an explicit save overwrites with full intent.
        if let stat = try? await service.stat(path: doc.workspaceRelativePath) {
            doc.adoptSavedBaseline(
                newHash: stat.contentHash ?? doc.baselineContentHash,
                newMtimeMs: stat.modificationMs
            )
            // adoptSavedBaseline recomputes dirtiness: divergent text stays dirty.
        }
        doc.conflictState = nil
        doc.saveState = doc.isDirty ? .editing : .clean
    }

    /// Discard the editor's edits and reload the on-disk version.
    func resolveConflictTakingDisk(_ doc: EditorDocumentState) async {
        if let response = try? await service.open(path: doc.workspaceRelativePath) {
            doc.text = response.content
            doc.adoptSavedBaseline(newHash: response.contentHash, newMtimeMs: response.onDiskModificationMs)
            doc.conflictState = nil
            doc.saveState = .clean
        }
    }

    // MARK: - Internals

    private func removeDocument(at idx: Int) {
        let doc = documents[idx]
        idByPath[doc.workspaceRelativePath] = nil
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
