// ConflictDiffTests.swift — PR-033: safe + observable user/agent/external edits.
//
// Drives the editor store through a MockEditorFileService returning canned
// stat/diff/working-change JSON (no disk, no process). Asserts:
//   • an external on-disk change flips the doc to conflict; Reload adopts disk +
//     clears conflict; saving over a conflicted doc never silently overwrites.
//   • a save triggers EXACTLY one single-file codegraphUpdate(path:) — not a
//     workspace re-index.
//   • diff hunks decode and produce gutter markers for added/removed lines.
//   • a ContextRef whose contentHash no longer matches reports isStale == true;
//     a matching one is fresh.
//   • ConflictResolutionView + the diff/agent-patch views render (ImageRenderer
//     non-nil) and fill the requested width (no letterbox) at 1024/1440.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class ConflictDiffTests: XCTestCase {

    private func makeStore(
        seed: [(path: String, content: String)] = [("src/main.rs", "fn main() {}\n")]
    ) -> (EditorWorkspaceStore, MockEditorFileService) {
        let service = MockEditorFileService()
        for entry in seed {
            service.seed(path: entry.path, content: entry.content)
        }
        return (EditorWorkspaceStore(service: service), service)
    }

    private func openDoc(
        _ store: EditorWorkspaceStore,
        _ path: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async throws -> EditorDocumentState {
        let opened = await store.open(path: path)
        return try XCTUnwrap(opened, "expected to open \(path)", file: file, line: line)
    }

    // MARK: - External change → conflict; Reload adopts disk; no silent overwrite

    func testExternalChangeFlipsDocumentToConflictViaWatcher() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        XCTAssertNil(doc.conflictState, "freshly opened document is not conflicted")

        // An external process rewrites the file on disk (different hash).
        service.simulateExternalChange(path: "src/main.rs", newContent: "fn main() { /* theirs */ }\n")

        // The watcher poll observes the divergence and flags a conflict.
        await store.pollExternalChanges()

        XCTAssertEqual(doc.saveState, .conflict, "an external change must surface as a conflict")
        XCTAssertNotNil(doc.conflictState, "a conflict state is recorded")
    }

    func testReloadAdoptsDiskAndClearsConflict() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("fn main() { /* mine */ }\n")

        let disk = "fn main() { /* theirs */ }\n"
        service.simulateExternalChange(path: "src/main.rs", newContent: disk)
        await store.pollExternalChanges()
        XCTAssertEqual(doc.saveState, .conflict)

        await store.resolveConflictTakingDisk(doc)

        XCTAssertNil(doc.conflictState, "Reload clears the conflict")
        XCTAssertFalse(doc.isDirty, "Reload discards local edits and re-baselines clean")
        XCTAssertEqual(doc.text, disk, "Reload adopts the on-disk content")
        XCTAssertEqual(doc.text, service.currentContent(path: "src/main.rs"))
    }

    func testSavingOverConflictedDocDoesNotSilentlyOverwrite() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("fn main() { /* mine */ }\n")

        // External change lands, watcher flags it.
        service.simulateExternalChange(path: "src/main.rs", newContent: "fn main() { /* theirs */ }\n")
        await store.pollExternalChanges()
        XCTAssertEqual(doc.saveState, .conflict)

        // A plain save while conflicted must route through resolution — the mock
        // records no overwrite because the optimistic-concurrency hash mismatches.
        let theirs = service.currentContent(path: "src/main.rs")
        let ok = await store.save(doc)
        XCTAssertFalse(ok, "a plain save over a changed file does not succeed silently")
        XCTAssertEqual(service.currentContent(path: "src/main.rs"), theirs,
                       "the external content is NOT silently overwritten")
        XCTAssertEqual(doc.saveState, .conflict, "the document stays in conflict")
        XCTAssertNil(service.savedContent["src/main.rs"],
                     "no buffer was persisted over the external change")
    }

    func testKeepMineForcesDeliberateOverwriteAndClearsConflict() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        let mine = "fn main() { /* mine wins */ }\n"
        doc.textDidChange(mine)

        service.simulateExternalChange(path: "src/main.rs", newContent: "fn main() { /* theirs */ }\n")
        await store.pollExternalChanges()
        XCTAssertEqual(doc.saveState, .conflict)

        // Keep Mine is an EXPLICIT, deliberate overwrite (not silent).
        let ok = await store.resolveConflictKeepingMine(doc)
        XCTAssertTrue(ok, "Keep Mine force-saves the buffer")
        XCTAssertNil(doc.conflictState, "Keep Mine clears the conflict")
        XCTAssertEqual(service.currentContent(path: "src/main.rs"), mine,
                       "the buffer deliberately overwrote the external change")
        XCTAssertFalse(doc.isDirty, "after the forced save the document is clean")
    }

    // MARK: - Incremental index on save: exactly one single-file update

    func testSaveTriggersExactlyOneSingleFileCodegraphUpdate() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("fn main() { let x = 1; }\n")

        let ok = await store.save(doc)
        XCTAssertTrue(ok)

        XCTAssertEqual(service.codegraphUpdateCalls, ["src/main.rs"],
                       "a save performs exactly one codegraphUpdate for that path")
        XCTAssertEqual(service.codegraphUpdateCalls.count, 1,
                       "the incremental index is a SINGLE call, not a workspace re-index")
    }

    func testCodegraphUpdateIsSingleFileNotWorkspaceScan() async throws {
        let service = MockEditorFileService()
        service.seed(path: "a.rs", content: "fn a() {}\n")
        let response = try await service.codegraphUpdate(path: "a.rs")
        XCTAssertFalse(response.fullScan, "an incremental update never reports a full workspace scan")
        XCTAssertEqual(response.path, "a.rs", "the update targets the single saved path")
        XCTAssertEqual(response.schema, "opensks.codegraph-update.v1")
    }

    func testCleanSaveDoesNotReindex() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        XCTAssertFalse(doc.isDirty)
        _ = await store.save(doc)
        XCTAssertTrue(service.codegraphUpdateCalls.isEmpty,
                      "a clean (no-op) save performs no re-index")
    }

    // MARK: - Diff hunks decode → gutter markers for added/removed lines

    func testTextDiffJSONDecodesToHunks() throws {
        let json = """
        {
          "schema": "opensks.text-diff.v1",
          "path": "src/main.rs",
          "changed": true,
          "hunks": [
            {
              "kind": "changed",
              "old_start": 1, "old_lines": 1,
              "new_start": 1, "new_lines": 2,
              "lines": ["-fn main() {}", "+fn main() {", "+    let x = 1;"]
            }
          ],
          "added_lines": 2,
          "removed_lines": 1
        }
        """
        let decoded = try JSONDecoder().decode(TextDiffResponse.self, from: Data(json.utf8))
        XCTAssertTrue(decoded.changed)
        XCTAssertEqual(decoded.addedLines, 2)
        XCTAssertEqual(decoded.removedLines, 1)
        XCTAssertEqual(decoded.hunks.count, 1)
        XCTAssertEqual(decoded.hunks[0].kind, .changed)
        XCTAssertEqual(decoded.hunks[0].newStart, 1)
        XCTAssertEqual(decoded.hunks[0].lines.count, 3)
    }

    func testDiffHunksProduceAddedAndRemovedGutterMarkers() throws {
        // A pure-add hunk (lines 2-3 added) and a pure-remove hunk anchored at
        // line 5 must produce added + removed gutter markers.
        let addHunk = TextDiffHunk(kind: .added, oldStart: 1, oldLines: 0,
                                   newStart: 2, newLines: 2,
                                   lines: ["+inserted one", "+inserted two"])
        let removeHunk = TextDiffHunk(kind: .removed, oldStart: 5, oldLines: 1,
                                      newStart: 5, newLines: 0,
                                      lines: ["-deleted line"])
        let response = TextDiffResponse(
            schema: "opensks.text-diff.v1", path: "f.txt", changed: true,
            hunks: [addHunk, removeHunk], addedLines: 2, removedLines: 1
        )

        let markers = DiffGutter.markers(from: response)
        XCTAssertEqual(markers[2], .added, "line 2 is an added line")
        XCTAssertEqual(markers[3], .added, "line 3 is an added line")
        XCTAssertEqual(markers[5], .removed, "a deletion is marked on its anchor line")
    }

    func testChangedHunkProducesChangedMarker() throws {
        let changedHunk = TextDiffHunk(kind: .changed, oldStart: 1, oldLines: 1,
                                       newStart: 1, newLines: 1,
                                       lines: ["-old", "+new"])
        let response = TextDiffResponse(
            schema: "opensks.text-diff.v1", path: "f.txt", changed: true,
            hunks: [changedHunk], addedLines: 1, removedLines: 1
        )
        let markers = DiffGutter.markers(from: response)
        XCTAssertEqual(markers[1], .changed, "a hunk with both +/- is a change")
    }

    func testStoreRefreshDiffPublishesMarkersForEditedDocument() async throws {
        let (store, _) = makeStore(seed: [("f.txt", "alpha\nbeta\n")])
        let doc = try await openDoc(store, "f.txt")
        doc.textDidChange("alpha\nbeta\ngamma\n")

        let response = await store.refreshDiff(doc)
        XCTAssertNotNil(response, "the diff is computed against the on-disk file")
        XCTAssertTrue(response?.changed ?? false, "an edited buffer differs from disk")
        XCTAssertNotNil(store.diff(for: doc.id), "the store publishes the diff for the gutter")
        let markers = DiffGutter.markers(from: try XCTUnwrap(store.diff(for: doc.id)))
        XCTAssertFalse(markers.isEmpty, "an edited document yields gutter markers")
    }

    // MARK: - Context refs: stale vs fresh

    func testContextRefIsFreshWhenFileUnchanged() throws {
        let text = "line one\nline two\nline three\n"
        let ref = try XCTUnwrap(EditorContextRef.capture(
            workspaceRelativePath: "f.txt", displayName: "f.txt",
            fullText: text, lineRange: EditorLineRange(start: 1, end: 2)
        ))
        XCTAssertFalse(ref.isStale(against: text), "an unchanged file keeps the ref fresh")
    }

    func testContextRefBecomesStaleWhenSelectedLinesChange() throws {
        let original = "line one\nline two\nline three\n"
        let ref = try XCTUnwrap(EditorContextRef.capture(
            workspaceRelativePath: "f.txt", displayName: "f.txt",
            fullText: original, lineRange: EditorLineRange(start: 1, end: 2)
        ))
        // Edit within the referenced lines → the captured hash no longer matches.
        let edited = "line ONE changed\nline two\nline three\n"
        XCTAssertTrue(ref.isStale(against: edited),
                      "a ref whose contentHash no longer matches the file is stale")
    }

    func testContextRefStaleWhenRangeNoLongerFits() throws {
        let original = "a\nb\nc\nd\n"
        let ref = try XCTUnwrap(EditorContextRef.capture(
            workspaceRelativePath: "f.txt", displayName: "f.txt",
            fullText: original, lineRange: EditorLineRange(start: 3, end: 4)
        ))
        // File shrank: the referenced range no longer exists → stale.
        XCTAssertTrue(ref.isStale(against: "a\nb\n"),
                      "a ref pointing past the end of a shrunken file is stale")
    }

    func testContextRefStableHashIsOverSelectedLinesOnly() throws {
        let text = "head\nSELECTED\ntail\n"
        let ref = try XCTUnwrap(EditorContextRef.capture(
            workspaceRelativePath: "f.txt", displayName: "f.txt",
            fullText: text, lineRange: EditorLineRange(start: 2, end: 2)
        ))
        // Editing OUTSIDE the selection keeps the ref fresh (hash is over the
        // selected lines only).
        let editedElsewhere = "HEAD changed\nSELECTED\ntail\n"
        XCTAssertFalse(ref.isStale(against: editedElsewhere),
                       "an edit outside the selected lines does not stale the ref")
    }

    // MARK: - Views render + fill width (no letterbox)

    func testConflictResolutionViewRenders() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("fn main() { /* mine */ }\n")
        service.simulateExternalChange(path: "src/main.rs", newContent: "fn main() { /* theirs */ }\n")
        await store.pollExternalChanges()
        XCTAssertEqual(doc.saveState, .conflict)

        let view = ConflictResolutionView(store: store, document: doc)
            .frame(width: 1024, height: 700)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the conflict resolution view renders non-nil")
    }

    func testConflictResolutionViewFillsWidthNoLetterbox() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("mine\n")
        service.simulateExternalChange(path: "src/main.rs", newContent: "theirs\n")
        await store.pollExternalChanges()

        for width in [1024.0, 1440.0] {
            let view = ConflictResolutionView(store: store, document: doc)
                .frame(width: width, height: 700)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "conflict view rendered at \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "conflict view must fill the width (no letterbox) at \(width)")
        }
    }

    func testDiffHunkViewRendersAndFillsWidth() throws {
        let response = TextDiffResponse(
            schema: "opensks.text-diff.v1", path: "f.txt", changed: true,
            hunks: [TextDiffHunk(kind: .changed, oldStart: 1, oldLines: 1,
                                 newStart: 1, newLines: 2,
                                 lines: ["-old", "+new one", "+new two"])],
            addedLines: 2, removedLines: 1
        )
        for width in [1024.0, 1440.0] {
            let view = DiffHunkView(title: "Compare", response: response)
                .frame(width: width, height: 600)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "diff view rendered at \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0,
                           "diff view must fill the width (no letterbox) at \(width)")
        }
    }

    func testAgentPatchViewRendersUnifiedPatch() throws {
        let patch = """
        @@ -1,2 +1,3 @@
         fn main() {
        -    old();
        +    new_one();
        +    new_two();
        """
        let lines = DiffPresentation.lines(fromUnifiedPatch: patch)
        XCTAssertTrue(lines.contains { $0.kind == .added }, "patch parsing finds added lines")
        XCTAssertTrue(lines.contains { $0.kind == .removed }, "patch parsing finds removed lines")
        XCTAssertTrue(lines.contains { $0.kind == .meta }, "patch parsing finds the hunk header")

        let view = AgentPatchView(patch: patch).frame(width: 1024, height: 600)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        let image = try XCTUnwrap(renderer.nsImage, "agent patch view renders non-nil")
        XCTAssertEqual(image.size.width, 1024, accuracy: 1.0,
                       "agent patch view fills the width (no letterbox)")
    }
}
