// VaultStore.swift — the @MainActor owner of the Vault panel state (PR-042).
//
// Drives four operations, each a SUBCOMMAND of the `vault` verb:
//   • exportSummary(conversationID:) — write a SANITIZED, git-trackable summary
//     (decisions + run links, NO raw transcript). The result is surfaced with its
//     explicit `contains_raw_transcript:false` honesty indicator.
//   • encrypt(conversationID:recipient:) — OPT-IN encryption of the FULL transcript
//     into a `.age` vault for the configured PUBLIC recipient. On failure
//     (`encrypt_failed` / `bad_recipient`) NO receipt is produced and the failure
//     is surfaced clearly — NEVER any plaintext/ciphertext.
//   • decrypt(vault:identityFile:) — IMPORT a `.age` vault, only possible WITH the
//     matching identity FILE (a private key on disk the store never reads/stores).
//     On `decrypt_failed` (wrong/missing identity) the failure is surfaced and NO
//     plaintext is presented; on success the recovered conversation's PROVENANCE
//     (which vault, recipient, byte size) is surfaced.
//   • refreshStatus() — read the workspace inventory (summaries + redacted vaults).
//
// SECURITY: the store holds NO transcript bytes and NO key material. It persists
// only the PUBLIC recipient (via the injected `VaultRecipientStoring`). The
// identity-file path supplied at import time is used transiently for the one
// decrypt call and is NOT retained as secret state. No method logs ciphertext,
// plaintext, or keys.

import SwiftUI

@MainActor
final class VaultStore: ObservableObject {
    // MARK: Published state

    /// The workspace inventory: sanitized (git-trackable) summaries + the `.age`
    /// vaults present (each with a redacted recipient).
    @Published private(set) var status: VaultStatus = .empty

    /// The configured age PUBLIC recipient (loaded from the recipient store). Only
    /// ever a public key — there is no private-key state anywhere in this store.
    @Published private(set) var recipient: VaultRecipient?

    /// The most recent sanitized summary export — surfaced with its
    /// `contains_raw_transcript:false` indicator so the operator can see it is safe
    /// to commit.
    @Published private(set) var lastSummary: VaultSummary?

    /// The most recent encryption receipt (the `.age` path + redacted recipient +
    /// byte size). NEVER any plaintext/ciphertext.
    @Published private(set) var lastEncrypt: VaultEncryptResult?

    /// The PROVENANCE of the most recent successful import: which vault was opened,
    /// the recovered conversation id, and the byte size. Surfaced so an imported
    /// vault's origin is visible. NEVER any recovered transcript text.
    @Published private(set) var lastImport: VaultImportProvenance?

    /// True while a service call is in flight (the view disables actions / dims).
    @Published private(set) var isBusy = false

    /// A non-fatal banner for the last failed operation. By construction this only
    /// ever holds a GENERIC, non-sensitive message — never plaintext/ciphertext/keys.
    @Published var lastError: String?

    private var service: VaultService
    private let recipientStore: VaultRecipientStoring

    init(service: VaultService, recipientStore: VaultRecipientStoring) {
        self.service = service
        self.recipientStore = recipientStore
        self.recipient = recipientStore.load()
    }

    // MARK: - Rebinding

    /// Swap the live service (e.g. once the real workspace + bundled CLI are
    /// resolved) and re-read the workspace inventory.
    func rebind(service: VaultService) {
        self.service = service
        Task { await refreshStatus() }
    }

    // MARK: - Recipient configuration (PUBLIC key only)

    /// Configure the age PUBLIC recipient (+ an optional NON-SECRET identity-file
    /// reference). Refuses anything that does not look like a public key — a private
    /// key can NEVER be stored here (the store guards it, and the type cannot carry
    /// one). Returns false (and sets an error) on a refused candidate.
    @discardableResult
    func configureRecipient(publicKey: String, identityFileReference: String? = nil) -> Bool {
        let trimmed = publicKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard VaultRecipient.looksLikePublicKey(trimmed) else {
            lastError = "That does not look like an age public key (it should start with \"age1\"). Only a PUBLIC recipient can be configured — never a private key."
            return false
        }
        let candidate = VaultRecipient(publicKey: trimmed, identityFileReference: identityFileReference)
        guard recipientStore.save(candidate) else {
            lastError = "Could not save the recipient."
            return false
        }
        recipient = candidate
        lastError = nil
        return true
    }

    /// Forget the configured recipient (removes it from the recipient store).
    func clearRecipient() {
        recipientStore.clear()
        recipient = nil
    }

    /// True when a recipient is configured so an opt-in encryption is possible.
    var canEncrypt: Bool { recipient?.isPublicRecipient == true }

    // MARK: - Export summary (sanitized, git-trackable)

    /// Write a SANITIZED, git-trackable summary for `conversationID` — decisions +
    /// run links, NO raw transcript, NO secrets. The result is surfaced with its
    /// `contains_raw_transcript:false` indicator.
    @discardableResult
    func exportSummary(conversationID: String) async -> VaultSummary? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let summary = try await service.exportSummary(conversationID: conversationID)
            lastSummary = summary
            await refreshStatus()
            return summary
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Encrypt (opt-in, full transcript)

    /// OPT-IN: encrypt the FULL transcript of `conversationID` into a `.age` vault
    /// for the configured PUBLIC recipient. On `encrypt_failed` / `bad_recipient`
    /// the failure is surfaced and NO receipt is produced — the view never presents
    /// any plaintext/ciphertext as success. No-op (with an error) if no recipient is
    /// configured.
    @discardableResult
    func encrypt(conversationID: String) async -> VaultEncryptResult? {
        guard let recipient, recipient.isPublicRecipient else {
            lastError = "Configure an age PUBLIC recipient before encrypting."
            return nil
        }
        return await encrypt(conversationID: conversationID, recipient: recipient.publicKey)
    }

    /// Encrypt for an explicit recipient public key. Kept internal so the recipient
    /// always flows from the configured PUBLIC key.
    @discardableResult
    func encrypt(conversationID: String, recipient publicKey: String) async -> VaultEncryptResult? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let result = try await service.encrypt(conversationID: conversationID, recipient: publicKey)
            lastEncrypt = result
            await refreshStatus()
            return result
        } catch {
            // A failure surfaces a GENERIC message; lastEncrypt is left untouched so
            // the UI never shows a half-success.
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Decrypt / import (requires the identity FILE)

    /// IMPORT a `.age` vault: recover the conversation, only possible WITH the
    /// matching age identity FILE. On `decrypt_failed` (wrong/missing identity) the
    /// failure is surfaced and NO plaintext is presented; on success the recovered
    /// PROVENANCE (vault path + conversation id + byte size) is surfaced. The
    /// identity-file path is used transiently for this call and not retained.
    @discardableResult
    func decrypt(vault vaultPath: String, identityFile: String) async -> VaultDecryptResult? {
        lastError = nil
        let trimmedIdentity = identityFile.trimmingCharacters(in: .whitespacesAndNewlines)
        // The import REQUIRES an identity: refuse a missing one BEFORE any call.
        guard !trimmedIdentity.isEmpty else {
            lastError = "An identity file (your private age key) is required to import a vault."
            return nil
        }
        isBusy = true
        defer { isBusy = false }
        do {
            let result = try await service.decrypt(vaultPath: vaultPath, identityFile: trimmedIdentity)
            // Surface the imported vault's PROVENANCE — never any recovered text.
            lastImport = VaultImportProvenance(
                vaultPath: vaultPath,
                conversationId: result.conversationId,
                bytes: result.bytes
            )
            await refreshStatus()
            return result
        } catch {
            // A wrong/missing identity surfaces a GENERIC failure; lastImport is left
            // untouched so the UI never presents a recovered transcript on failure.
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Status

    /// Re-read the workspace inventory (summaries + redacted vaults).
    func refreshStatus() async {
        do {
            status = try await service.status()
        } catch {
            // A status read failure is non-fatal — keep the last known inventory.
            lastError = Self.describe(error)
        }
    }

    // MARK: - Dismissals

    func dismissSummary() { lastSummary = nil }
    func dismissEncryptReceipt() { lastEncrypt = nil }
    func dismissImport() { lastImport = nil }
    func dismissError() { lastError = nil }

    // MARK: - Internals

    /// Map a thrown error to a GENERIC, non-sensitive message. A typed
    /// `VaultServiceError` carries its own clear human message; nothing here echoes
    /// any plaintext/ciphertext/key.
    private static func describe(_ error: Error) -> String {
        if let vaultError = error as? VaultServiceError {
            return vaultError.humanMessage
        }
        return error.localizedDescription
    }
}

// MARK: - Import provenance

/// The PROVENANCE of an imported (decrypted) vault: which `.age` file was opened,
/// the recovered conversation id, and the byte size. This is surfaced so an
/// imported vault's origin is visible — it carries NO recovered transcript text.
struct VaultImportProvenance: Sendable, Equatable {
    let vaultPath: String
    let conversationId: String
    let bytes: Int

    /// The `.age` file's display name (last path component).
    var vaultName: String {
        (vaultPath as NSString).lastPathComponent
    }
}
