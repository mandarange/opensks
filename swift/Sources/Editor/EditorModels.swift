// EditorModels.swift — the editable-workspace domain model (PR-032).
//
// The editor is a REAL editable surface: every open document carries a stable
// identity, an immutable on-open snapshot (the optimistic-concurrency baseline),
// and a mutable @MainActor state holding the live text + dirty/save/conflict
// status. Dirtiness is a pure function of `currentContentHash != baseline hash`
// so it is impossible for a save to silently lose edits.
//
// Codable models here decode the SHARED FILE JSON CONTRACT (snake_case) that the
// bundled `opensks file open|save|stat` CLI emits. Decoding is intentionally
// tolerant of encoding spelling (`utf-8` / `utf8`) so the two halves can not
// drift over a cosmetic difference.

import Foundation

// MARK: - Identity

/// Stable identity for an open document. Created once when a path is first
/// opened and reused for the lifetime of the tab; it is NOT regenerated on every
/// `open(...)` so focusing an already-open path keeps the same tab + cursor.
struct EditorDocumentID: Hashable, Identifiable, Sendable {
    let raw: UUID
    init(_ raw: UUID = UUID()) { self.raw = raw }
    var id: UUID { raw }
}

// MARK: - Content hashing

/// FNV-1a 64-bit over UTF-8 bytes, rendered as the CLI's `fnv1a64:<hex>` form so
/// the locally-computed dirty hash is directly comparable to a server baseline
/// hash of the same shape. The hash is only ever used for equality, never for
/// security.
enum EditorContentHash {
    static let prefix = "fnv1a64:"

    static func compute(_ text: String) -> String {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        let prime: UInt64 = 0x0000_0100_0000_01B3
        for byte in text.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* prime
        }
        return prefix + String(hash, radix: 16, uppercase: false)
    }
}

// MARK: - Open-time snapshot (immutable baseline)

/// The immutable facts captured when a document is opened. `baselineContentHash`
/// + `onDiskModificationMs` together are the optimistic-concurrency baseline
/// echoed back on save.
struct EditorDocumentSnapshot: Sendable, Equatable {
    let workspaceRelativePath: String
    let displayName: String
    let language: CodeLang
    let encoding: String
    let lineEnding: EditorLineEnding
    let baselineContentHash: String
    let byteSize: Int
    let onDiskModificationMs: UInt64
    let isSecretRestricted: Bool
    let isBinary: Bool

    /// A document can only be edited if it is plain, readable, non-secret text.
    var isEditable: Bool { !isSecretRestricted && !isBinary }
}

enum EditorLineEnding: String, Sendable, Equatable {
    case lf
    case crlf

    var label: String { self == .crlf ? "CRLF" : "LF" }
}

// MARK: - Save / conflict state

/// The lifecycle state of an open document's persistence.
enum EditorSaveState: Equatable, Sendable {
    case clean        // no unsaved edits
    case editing      // dirty, not yet saving
    case saving       // a save is in flight
    case saved        // a save just completed successfully
    case saveFailed(String)
    case conflict     // on-disk changed since baseline; needs resolution
    case readOnly     // binary/oversized — viewer only
    case restricted   // secret path — viewer only

    var isBusy: Bool { self == .saving }
}

/// Captured when the service reports `file_changed_on_disk`: the user's edits are
/// preserved verbatim and a conflict is surfaced rather than silently overwriting.
struct EditorConflictState: Equatable, Sendable {
    let message: String
}

// MARK: - Live mutable document state

/// The mutable, observable state of one open document. Owned by the store and
/// mutated only on the main actor.
@MainActor
final class EditorDocumentState: ObservableObject, Identifiable {
    let id: EditorDocumentID
    let snapshot: EditorDocumentSnapshot

    /// Live editor text.
    @Published var text: String
    /// Hash of `text`, kept in sync on every edit; compared to the baseline.
    @Published private(set) var currentContentHash: String
    /// The baseline this document is currently reconciled against. Advances after
    /// each successful save so the next edit cycle compares to fresh bytes.
    @Published private(set) var baselineContentHash: String
    @Published private(set) var onDiskModificationMs: UInt64
    @Published var saveState: EditorSaveState
    @Published var conflictState: EditorConflictState?

    init(id: EditorDocumentID, snapshot: EditorDocumentSnapshot, text: String) {
        self.id = id
        self.snapshot = snapshot
        self.text = text
        self.currentContentHash = EditorContentHash.compute(text)
        self.baselineContentHash = snapshot.baselineContentHash
        self.onDiskModificationMs = snapshot.onDiskModificationMs
        if snapshot.isSecretRestricted {
            self.saveState = .restricted
        } else if snapshot.isBinary {
            self.saveState = .readOnly
        } else {
            self.saveState = .clean
        }
    }

    var displayName: String { snapshot.displayName }
    var workspaceRelativePath: String { snapshot.workspaceRelativePath }
    var language: CodeLang { snapshot.language }
    var isEditable: Bool { snapshot.isEditable }

    /// A document is dirty when its live hash diverges from the baseline it was
    /// last reconciled against. Pure function — never set by hand.
    var isDirty: Bool { isEditable && currentContentHash != baselineContentHash }

    /// Recompute the live hash and advance the save state after an edit.
    func textDidChange(_ newText: String) {
        guard isEditable else { return }
        if text != newText { text = newText }
        currentContentHash = EditorContentHash.compute(newText)
        if conflictState == nil {
            saveState = isDirty ? .editing : .clean
        }
    }

    func markSaving() {
        guard isEditable else { return }
        saveState = .saving
    }

    /// Adopt the post-save baseline returned by the service: edits are now clean.
    func adoptSavedBaseline(newHash: String, newMtimeMs: UInt64) {
        baselineContentHash = newHash
        onDiskModificationMs = newMtimeMs
        currentContentHash = EditorContentHash.compute(text)
        conflictState = nil
        saveState = isDirty ? .editing : .saved
    }

    func markSaveFailed(_ message: String) {
        saveState = .saveFailed(message)
    }

    func markConflict(_ message: String) {
        conflictState = EditorConflictState(message: message)
        saveState = .conflict
    }
}

// MARK: - SHARED FILE JSON CONTRACT (snake_case)

/// `opensks.text-document.v1` — the open result.
struct EditorOpenResponse: Decodable, Sendable {
    let schema: String
    let workspaceRelativePath: String
    let content: String
    let contentHash: String
    let encoding: String
    let lineEnding: String
    let byteSize: Int
    let isSecretRestricted: Bool
    let isBinary: Bool
    let onDiskModificationMs: UInt64
    let permissionsMode: Int?

    enum CodingKeys: String, CodingKey {
        case schema
        case workspaceRelativePath = "workspace_relative_path"
        case content
        case contentHash = "content_hash"
        case encoding
        case lineEnding = "line_ending"
        case byteSize = "byte_size"
        case isSecretRestricted = "is_secret_restricted"
        case isBinary = "is_binary"
        case onDiskModificationMs = "on_disk_modification_ms"
        case permissionsMode = "permissions_mode"
    }
}

/// `opensks.save-result.v1` — the save result.
struct EditorSaveResponse: Decodable, Sendable {
    let schema: String
    let newHash: String
    let newMtimeMs: UInt64

    enum CodingKeys: String, CodingKey {
        case schema
        case newHash = "new_hash"
        case newMtimeMs = "new_mtime_ms"
    }
}

/// `opensks.workspace-entry.v1` — the stat result (metadata only).
struct EditorStatResponse: Decodable, Sendable {
    let schema: String
    let workspaceRelativePath: String
    let byteSize: Int
    let modificationMs: UInt64
    let contentHash: String?
    let isSecretRestricted: Bool

    enum CodingKeys: String, CodingKey {
        case schema
        case workspaceRelativePath = "workspace_relative_path"
        case byteSize = "byte_size"
        case modificationMs = "modification_ms"
        case contentHash = "content_hash"
        case isSecretRestricted = "is_secret_restricted"
    }
}

/// `opensks.file-error.v1` — the error envelope. Never carries file contents.
struct EditorErrorResponse: Decodable, Sendable {
    let schema: String
    let error: Payload

    struct Payload: Decodable, Sendable {
        let code: String
        let message: String
    }
}

// MARK: - Mapping responses to domain

extension EditorOpenResponse {
    /// Normalize the encoding label so `utf-8` and `utf8` are treated as one.
    var normalizedEncoding: String {
        let lower = encoding.lowercased()
        return (lower == "utf8" || lower == "utf-8") ? "utf-8" : encoding
    }

    var resolvedLineEnding: EditorLineEnding {
        EditorLineEnding(rawValue: lineEnding.lowercased()) ?? .lf
    }

    func makeSnapshot(displayName: String, language: CodeLang) -> EditorDocumentSnapshot {
        EditorDocumentSnapshot(
            workspaceRelativePath: workspaceRelativePath,
            displayName: displayName,
            language: language,
            encoding: normalizedEncoding,
            lineEnding: resolvedLineEnding,
            baselineContentHash: contentHash,
            byteSize: byteSize,
            onDiskModificationMs: onDiskModificationMs,
            isSecretRestricted: isSecretRestricted,
            isBinary: isBinary
        )
    }
}
