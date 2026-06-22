// EditorTests.swift — the editable code workspace (PR-032).
//
// Drives the store + document state through a MockEditorFileService (no disk, no
// process) and renders EditorWorkspaceView offscreen to assert it shows the
// active document and fills the width with no letterbox.

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class EditorTests: XCTestCase {

    private func makeStore(
        seed: [(path: String, content: String)] = [("src/main.rs", "fn main() {}\n")]
    ) -> (EditorWorkspaceStore, MockEditorFileService) {
        let service = MockEditorFileService()
        for entry in seed {
            service.seed(path: entry.path, content: entry.content)
        }
        return (EditorWorkspaceStore(service: service), service)
    }

    /// Open + unwrap in two steps: `XCTUnwrap`'s autoclosure can't be `async`.
    private func openDoc(
        _ store: EditorWorkspaceStore,
        _ path: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async throws -> EditorDocumentState {
        let opened = await store.open(path: path)
        return try XCTUnwrap(opened, "expected to open \(path)", file: file, line: line)
    }

    // MARK: - Editing → dirty → save persists + clears dirty

    func testEditingMakesDocumentDirtyByHashDivergence() async throws {
        let (store, _) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")

        XCTAssertFalse(doc.isDirty, "freshly opened document is clean")
        XCTAssertEqual(doc.currentContentHash, doc.baselineContentHash)
        XCTAssertEqual(doc.saveState, .clean)

        doc.textDidChange("fn main() { println!(\"hi\"); }\n")

        XCTAssertTrue(doc.isDirty, "edited document must be dirty")
        XCTAssertNotEqual(doc.currentContentHash, doc.baselineContentHash,
                          "dirtiness is hash divergence from baseline")
        XCTAssertEqual(doc.saveState, .editing)
    }

    func testSavePersistsContentAndClearsDirty() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        let edited = "fn main() { let x = 1; }\n"
        doc.textDidChange(edited)
        XCTAssertTrue(doc.isDirty)

        let ok = await store.save()
        XCTAssertTrue(ok, "save of a dirty editable document succeeds")

        // The mock recorded the saved content + advanced the hash.
        XCTAssertEqual(service.savedContent["src/main.rs"], edited,
                       "the service persisted the exact edited content")
        XCTAssertEqual(service.currentContent(path: "src/main.rs"), edited)

        XCTAssertFalse(doc.isDirty, "saving clears dirty")
        XCTAssertEqual(doc.currentContentHash, doc.baselineContentHash,
                       "baseline advanced to the saved content")
        XCTAssertEqual(doc.saveState, .saved)
    }

    func testSaveOfCleanDocumentDoesNotCallService() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        XCTAssertFalse(doc.isDirty)
        _ = await store.save()
        XCTAssertNil(service.saveCount["src/main.rs"], "no save call for a clean document")
    }

    // MARK: - Same path focuses one tab; dirty tabs survive

    func testOpeningSamePathTwiceFocusesSameTabNoDuplicate() async throws {
        let (store, _) = makeStore(seed: [
            ("a.txt", "alpha\n"),
            ("b.txt", "beta\n")
        ])
        let first = try await openDoc(store, "a.txt")
        _ = try await openDoc(store, "b.txt")
        XCTAssertEqual(store.documents.count, 2)

        let firstAgain = try await openDoc(store, "a.txt")
        XCTAssertEqual(store.documents.count, 2, "re-opening a path must not duplicate the tab")
        XCTAssertEqual(first.id, firstAgain.id, "the same stable identity is reused")
        XCTAssertEqual(store.activeDocumentID, first.id, "re-opening focuses the existing tab")
    }

    func testOpeningSamePathPreservesIdentityAcrossPathNormalization() async throws {
        let (store, _) = makeStore(seed: [("src/main.rs", "fn main() {}\n")])
        let a = try await openDoc(store, "src/main.rs")
        let b = try await openDoc(store, "./src/main.rs")
        XCTAssertEqual(a.id, b.id, "redundant ./ prefix resolves to the same document")
        XCTAssertEqual(store.documents.count, 1)
    }

    func testDirtyTabIsNotEvictedWhenOpeningOthers() async throws {
        // maxTabs = 2: open a dirty tab, then open enough others to overflow.
        let service = MockEditorFileService()
        service.seed(path: "dirty.txt", content: "keep me\n")
        service.seed(path: "x.txt", content: "x\n")
        service.seed(path: "y.txt", content: "y\n")
        let store = EditorWorkspaceStore(service: service, maxTabs: 2)

        let dirty = try await openDoc(store, "dirty.txt")
        dirty.textDidChange("keep me — edited\n")
        XCTAssertTrue(dirty.isDirty)

        _ = await store.open(path: "x.txt")
        _ = await store.open(path: "y.txt")

        XCTAssertTrue(
            store.documents.contains { $0.id == dirty.id },
            "a dirty tab is never silently evicted by the tab cap"
        )
    }

    // MARK: - Conflict, not silent overwrite

    func testOutOfBandChangeProducesConflictNotSilentOverwrite() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("fn main() { /* mine */ }\n")

        // Arm a file_changed_on_disk for the next save.
        service.armConflict(path: "src/main.rs")
        let ok = await store.save()

        XCTAssertFalse(ok, "a conflicting save reports failure")
        XCTAssertEqual(doc.saveState, .conflict, "the document enters a conflict state")
        XCTAssertNotNil(doc.conflictState, "a conflict banner is available")
        XCTAssertTrue(doc.isDirty, "the user's edits are preserved (still dirty)")
        // The service never persisted the conflicting write.
        XCTAssertNil(service.savedContent["src/main.rs"],
                     "a conflict must NOT silently overwrite the on-disk file")
    }

    func testConflictTakingDiskReloadsAndClears() async throws {
        let (store, service) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("mine\n")
        service.armConflict(path: "src/main.rs")
        _ = await store.save()
        XCTAssertEqual(doc.saveState, .conflict)

        await store.resolveConflictTakingDisk(doc)
        XCTAssertNil(doc.conflictState, "resolving clears the conflict")
        XCTAssertFalse(doc.isDirty, "taking disk discards local edits")
        XCTAssertEqual(doc.text, service.currentContent(path: "src/main.rs"))
    }

    // MARK: - Close with dirty protection

    func testCloseWithDirtyProtectionRefusesDirtyTab() async throws {
        let (store, _) = makeStore()
        let doc = try await openDoc(store, "src/main.rs")
        doc.textDidChange("edited\n")

        let closed = store.close(doc.id, dirtyProtection: true)
        XCTAssertFalse(closed, "dirty protection refuses to close unsaved work")
        XCTAssertTrue(store.documents.contains { $0.id == doc.id })

        let forced = store.close(doc.id, dirtyProtection: true, force: true)
        XCTAssertTrue(forced, "forcing overrides dirty protection")
        XCTAssertFalse(store.documents.contains { $0.id == doc.id })
    }

    // MARK: - Restricted / binary render read-only

    func testSecretRestrictedDocumentIsReadOnly() async throws {
        let service = MockEditorFileService()
        service.seed(path: ".env", content: "SECRET=1\n", isSecretRestricted: true)
        let store = EditorWorkspaceStore(service: service)
        let doc = try await openDoc(store, ".env")
        XCTAssertEqual(doc.saveState, .restricted)
        XCTAssertFalse(doc.isEditable)
        doc.textDidChange("SECRET=2\n")
        XCTAssertFalse(doc.isDirty, "a restricted document can never become dirty")
    }

    // MARK: - View renders the active document + fills width (no letterbox)

    func testWorkspaceViewRendersActiveDocument() async throws {
        let (store, _) = makeStore()
        _ = try await openDoc(store, "src/main.rs")
        let view = EditorWorkspaceView(store: store)
            .frame(width: 1024, height: 700)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the workspace view renders the active document non-nil")
    }

    func testWorkspaceViewFillsWidthWithNoLetterbox() async throws {
        let (store, _) = makeStore()
        _ = try await openDoc(store, "src/main.rs")
        for width in [1024.0, 1440.0, 1920.0] {
            let view = EditorWorkspaceView(store: store)
                .frame(width: width, height: 700)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "editor rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "editor must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    func testEmptyWorkspaceRendersNoFileOpenState() throws {
        let service = MockEditorFileService()
        let store = EditorWorkspaceStore(service: service)
        let view = EditorWorkspaceView(store: store)
            .frame(width: 1024, height: 700)
        XCTAssertNotNil(ImageRenderer(content: view).nsImage)
    }
}
