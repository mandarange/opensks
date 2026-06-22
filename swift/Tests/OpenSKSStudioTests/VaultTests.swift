// VaultTests.swift — the Vault import + provenance + recipient config surface
// (PR-042).
//
// Drives VaultModels / VaultService / VaultRecipientStore / VaultStore / VaultView
// through a MockVaultService + an in-memory recipient store (no disk, no process,
// NO crypto, NO network). Asserts the security invariants:
//   • export-summary surfaces a summary marked contains_raw_transcript:false and
//     lists its decisions + run links;
//   • a full-transcript import REQUIRES an identity: decrypt without / with a wrong
//     identity surfaces the failure and presents NO plaintext;
//   • an encrypt failure is shown as a failure and the UI never presents any
//     plaintext/ciphertext as success;
//   • the recipient store round-trips ONLY a PUBLIC recipient (no private key
//     persisted; the store exposes only a public recipient);
//   • the vault view + an imported summary render (ImageRenderer non-nil) and fill
//     width at 1024 / 1440 (no letterbox).

import SwiftUI
import XCTest
@testable import OpenSKSStudio

@MainActor
final class VaultTests: XCTestCase {

    // MARK: - Canned JSON fixtures (the shared snake_case contract)

    private static let summaryJSON = """
    {
      "schema": "opensks.vault-summary.v1",
      "conversation_id": "conv-42",
      "summary_path": ".opensks/vault/conv-42.summary.md",
      "decisions": 3,
      "run_links": ["run-aaa", "run-bbb"],
      "contains_raw_transcript": false,
      "redacted": true
    }
    """

    private static let encryptJSON = """
    {
      "schema": "opensks.vault-encrypt.v1",
      "vault_path": ".opensks/vault/conv-42.age",
      "recipient": "age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqsxytrs",
      "bytes": 8192
    }
    """

    private static let decryptJSON = """
    {
      "schema": "opensks.vault-decrypt.v1",
      "conversation_id": "conv-imported",
      "bytes": 8192
    }
    """

    private static let statusJSON = """
    {
      "schema": "opensks.vault-status.v1",
      "summaries": [
        {
          "schema": "opensks.vault-summary.v1",
          "conversation_id": "conv-42",
          "summary_path": ".opensks/vault/conv-42.summary.md",
          "decisions": 3,
          "run_links": ["run-aaa"],
          "contains_raw_transcript": false,
          "redacted": true
        }
      ],
      "vaults": [
        {"path": ".opensks/vault/conv-42.age", "recipient_redacted": "age1qqqq…trs"}
      ]
    }
    """

    private static let errorJSON = """
    {"schema": "opensks.vault-error.v1", "error": {"code": "decrypt_failed"}}
    """

    /// A valid-looking age PUBLIC key used across the tests.
    private static let publicRecipient =
        "age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqsxytrs"

    private func decode<T: Decodable>(_ json: String, as type: T.Type) throws -> T {
        try JSONDecoder().decode(T.self, from: Data(json.utf8))
    }

    // MARK: - Decode

    func testSummaryDecodesMarkedNoRawTranscript() throws {
        let summary = try decode(Self.summaryJSON, as: VaultSummary.self)
        XCTAssertEqual(summary.schema, "opensks.vault-summary.v1")
        XCTAssertEqual(summary.conversationId, "conv-42")
        XCTAssertEqual(summary.decisions, 3)
        XCTAssertEqual(summary.runLinks, ["run-aaa", "run-bbb"])
        XCTAssertFalse(summary.containsRawTranscript, "a summary is never a raw transcript")
        XCTAssertTrue(summary.redacted)
        XCTAssertTrue(summary.isGitTrackable, "no raw transcript + redacted ⇒ safe to commit")
    }

    func testVaultErrorEnvelopeMapsToTypedError() throws {
        let envelope = try decode(Self.errorJSON, as: VaultErrorEnvelope.self)
        XCTAssertEqual(LiveVaultService.mapError(envelope), .decryptFailed)

        for (code, expected) in [
            ("encrypt_failed", VaultServiceError.encryptFailed),
            ("bad_recipient", VaultServiceError.badRecipient),
            ("decrypt_failed", VaultServiceError.decryptFailed)
        ] {
            let env = VaultErrorEnvelope(schema: "opensks.vault-error.v1", error: .init(code: code))
            XCTAssertEqual(LiveVaultService.mapError(env), expected)
        }
        // Each typed error has a clear, non-sensitive human message.
        for error in [VaultServiceError.encryptFailed, .badRecipient, .decryptFailed] {
            XCTAssertFalse(error.humanMessage.isEmpty)
        }
    }

    // MARK: - export-summary surfaces a sanitized summary (no raw transcript)

    func testExportSummarySurfacesSanitizedSummaryWithDecisionsAndRunLinks() async throws {
        let service = MockVaultService()
        service.setSummary(try decode(Self.summaryJSON, as: VaultSummary.self))
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())

        let result = await store.exportSummary(conversationID: "conv-42")
        let summary = try XCTUnwrap(result)

        // The summary is marked free of any raw transcript and lists its decisions
        // + run links.
        XCTAssertFalse(summary.containsRawTranscript, "export-summary must carry contains_raw_transcript:false")
        XCTAssertTrue(summary.isGitTrackable)
        XCTAssertEqual(summary.decisions, 3)
        XCTAssertEqual(summary.runLinks, ["run-aaa", "run-bbb"])
        XCTAssertEqual(store.lastSummary?.conversationId, "conv-42")
        XCTAssertEqual(service.exportSummaryCalls, ["conv-42"])
    }

    // MARK: - a full-transcript import REQUIRES an identity

    func testDecryptWithoutIdentityRefusesAndPresentsNoPlaintext() async throws {
        let service = MockVaultService()
        service.setDecryptResult(try decode(Self.decryptJSON, as: VaultDecryptResult.self))
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())

        // An EMPTY identity is refused BEFORE any service call — no import happens.
        let result = await store.decrypt(vault: ".opensks/vault/conv-42.age", identityFile: "   ")
        XCTAssertNil(result, "an import with no identity must fail")
        XCTAssertTrue(service.decryptCalls.isEmpty, "no decrypt is attempted without an identity")
        XCTAssertNil(store.lastImport, "no provenance — and therefore no plaintext — is presented on failure")
        XCTAssertNotNil(store.lastError, "the missing-identity failure is surfaced")
    }

    func testDecryptWithWrongIdentitySurfacesFailureAndNoPlaintext() async throws {
        let service = MockVaultService()
        // The mock returns decrypt_failed for a wrong key, regardless of the vault.
        service.armDecryptFailure(.decryptFailed)
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())

        let result = await store.decrypt(
            vault: ".opensks/vault/conv-42.age",
            identityFile: "~/wrong/identity.txt"
        )
        XCTAssertNil(result, "a wrong identity cannot open the vault")
        XCTAssertEqual(service.decryptCalls.count, 1, "the decrypt was attempted with the supplied identity")
        XCTAssertNil(store.lastImport, "NO plaintext / provenance is presented on a failed decrypt")
        let error = try XCTUnwrap(store.lastError)
        XCTAssertEqual(error, VaultServiceError.decryptFailed.humanMessage)
        // The surfaced message is the generic typed message — it never echoes a key
        // or transcript.
        XCTAssertFalse(error.lowercased().contains("age1"))
    }

    func testDecryptWithCorrectIdentitySurfacesProvenanceOnly() async throws {
        let service = MockVaultService()
        service.registerValidIdentity("~/.config/age/keys.txt")
        service.setDecryptResult(try decode(Self.decryptJSON, as: VaultDecryptResult.self))
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())

        let result = await store.decrypt(
            vault: ".opensks/vault/conv-42.age",
            identityFile: "~/.config/age/keys.txt"
        )
        let decrypted = try XCTUnwrap(result, "the correct identity opens the vault")
        XCTAssertEqual(decrypted.conversationId, "conv-imported")

        // The store surfaces PROVENANCE — which vault, which conversation, the size —
        // and NOT any recovered transcript text.
        let provenance = try XCTUnwrap(store.lastImport)
        XCTAssertEqual(provenance.vaultPath, ".opensks/vault/conv-42.age")
        XCTAssertEqual(provenance.conversationId, "conv-imported")
        XCTAssertEqual(provenance.bytes, 8192)
        XCTAssertNil(store.lastError)
    }

    // MARK: - an encrypt failure never presents plaintext/ciphertext as success

    func testEncryptFailureIsShownAndNoCiphertextPresentedAsSuccess() async throws {
        let service = MockVaultService()
        service.armEncryptFailure(.encryptFailed)
        let recipientStore = InMemoryVaultRecipientStore(
            initial: VaultRecipient(publicKey: Self.publicRecipient)
        )
        let store = VaultStore(service: service, recipientStore: recipientStore)
        XCTAssertTrue(store.canEncrypt, "a configured public recipient enables opt-in encryption")

        let result = await store.encrypt(conversationID: "conv-42")
        XCTAssertNil(result, "an encrypt failure produces no receipt")
        XCTAssertNil(store.lastEncrypt, "no half-success: the receipt is never set on failure")
        let error = try XCTUnwrap(store.lastError)
        XCTAssertEqual(error, VaultServiceError.encryptFailed.humanMessage)
        // The failure message is generic — it carries no plaintext/ciphertext/key.
        XCTAssertFalse(error.lowercased().contains("age1"))
        XCTAssertEqual(service.encryptCalls.count, 1, "the encrypt was attempted")
    }

    func testEncryptWithoutConfiguredRecipientIsRefused() async throws {
        let service = MockVaultService()
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())
        XCTAssertFalse(store.canEncrypt)

        let result = await store.encrypt(conversationID: "conv-42")
        XCTAssertNil(result)
        XCTAssertTrue(service.encryptCalls.isEmpty, "no encryption is attempted without a recipient")
        XCTAssertNotNil(store.lastError)
    }

    func testEncryptSuccessSurfacesRedactedRecipientReceipt() async throws {
        let service = MockVaultService()
        service.setEncryptResult(try decode(Self.encryptJSON, as: VaultEncryptResult.self))
        let recipientStore = InMemoryVaultRecipientStore(
            initial: VaultRecipient(publicKey: Self.publicRecipient)
        )
        let store = VaultStore(service: service, recipientStore: recipientStore)

        let result = await store.encrypt(conversationID: "conv-42")
        let receipt = try XCTUnwrap(result)
        XCTAssertEqual(receipt.vaultPath, ".opensks/vault/conv-42.age")
        XCTAssertEqual(receipt.bytes, 8192)
        // The receipt shows a REDACTED recipient (never the full key inline).
        XCTAssertTrue(receipt.recipientRedacted.contains("…"))
        XCTAssertNotEqual(receipt.recipientRedacted, receipt.recipient)
    }

    // MARK: - the recipient store round-trips ONLY a PUBLIC recipient

    func testRecipientStoreRoundTripsPublicRecipientOnly() {
        let store = InMemoryVaultRecipientStore()

        // A PUBLIC recipient round-trips.
        XCTAssertTrue(store.save(VaultRecipient(publicKey: Self.publicRecipient,
                                                identityFileReference: "~/.config/age/keys.txt")))
        let loaded = store.load()
        XCTAssertEqual(loaded?.publicKey, Self.publicRecipient)
        XCTAssertTrue(loaded?.isPublicRecipient == true, "the store exposes only a public recipient")
        // The identity reference is a non-secret PATH, not a key.
        XCTAssertEqual(loaded?.identityFileReference, "~/.config/age/keys.txt")

        // A value that is NOT a public key (e.g. an age SECRET key) is refused — a
        // private key can never be persisted here.
        let privateKeyLike = "AGE-SECRET-KEY-1QQQQQQQQQQQQQQQQQQQQQQQQQQQQQQ"
        XCTAssertFalse(VaultRecipient.looksLikePublicKey(privateKeyLike))
        XCTAssertFalse(store.save(VaultRecipient(publicKey: privateKeyLike)),
                       "a non-public (e.g. private) key is never saved")
        // The previously-saved PUBLIC recipient is still what is exposed.
        XCTAssertEqual(store.load()?.publicKey, Self.publicRecipient)

        // The VaultRecipient type itself has NO field that could carry a private key.
        let mirror = Mirror(reflecting: VaultRecipient(publicKey: Self.publicRecipient))
        for child in mirror.children {
            let label = (child.label ?? "").lowercased()
            XCTAssertFalse(label.contains("private"), "no private-key field on VaultRecipient")
            XCTAssertFalse(label.contains("secret"), "no secret-key field on VaultRecipient")
        }
    }

    func testConfigureRecipientRefusesNonPublicKeyOnTheStore() {
        let store = VaultStore(service: MockVaultService(), recipientStore: InMemoryVaultRecipientStore())

        // A junk / private-looking value is refused with an error and not stored.
        XCTAssertFalse(store.configureRecipient(publicKey: "AGE-SECRET-KEY-1XXXX"))
        XCTAssertNil(store.recipient)
        XCTAssertNotNil(store.lastError)

        // A valid PUBLIC key is accepted and exposed.
        XCTAssertTrue(store.configureRecipient(publicKey: Self.publicRecipient))
        XCTAssertEqual(store.recipient?.publicKey, Self.publicRecipient)
        XCTAssertTrue(store.recipient?.isPublicRecipient == true)
        XCTAssertNil(store.lastError)
    }

    // MARK: - refreshStatus reads the workspace inventory

    func testRefreshStatusReadsInventory() async throws {
        let service = MockVaultService()
        service.setStatus(try decode(Self.statusJSON, as: VaultStatus.self))
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())

        await store.refreshStatus()
        XCTAssertEqual(service.statusCallCount, 1)
        XCTAssertEqual(store.status.summaries.count, 1)
        XCTAssertEqual(store.status.summaries.first?.conversationId, "conv-42")
        XCTAssertFalse(store.status.summaries.first?.containsRawTranscript ?? true)
        XCTAssertEqual(store.status.vaults.count, 1)
        // The inventory carries only a REDACTED recipient — never a raw key.
        XCTAssertEqual(store.status.vaults.first?.recipientRedacted, "age1qqqq…trs")
    }

    // MARK: - Rendering: non-nil + fills width (no letterbox)

    func testVaultViewRendersNonNilWithImportedSummary() async throws {
        let service = MockVaultService()
        service.setSummary(try decode(Self.summaryJSON, as: VaultSummary.self))
        service.setStatus(try decode(Self.statusJSON, as: VaultStatus.self))
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())
        await store.exportSummary(conversationID: "conv-42")
        await store.refreshStatus()

        let view = VaultView(
            store: store,
            activeConversationID: "conv-42",
            pickVaultFile: { nil },
            pickIdentityFile: { nil }
        )
        .frame(width: 1280, height: 800)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "the vault view renders non-nil with an imported summary")
    }

    func testVaultViewFillsWidthNoLetterbox() async throws {
        let service = MockVaultService()
        service.setSummary(try decode(Self.summaryJSON, as: VaultSummary.self))
        service.setStatus(try decode(Self.statusJSON, as: VaultStatus.self))
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())
        await store.exportSummary(conversationID: "conv-42")
        await store.refreshStatus()

        for width in [1024.0, 1440.0] {
            let view = VaultView(
                store: store,
                activeConversationID: "conv-42",
                pickVaultFile: { nil },
                pickIdentityFile: { nil }
            )
            .frame(width: width, height: 800)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "vault view rendered at width \(width)")
            XCTAssertEqual(
                image.size.width, width, accuracy: 1.0,
                "vault view must fill the requested width (no letterbox) at \(width)"
            )
        }
    }

    /// A failed-decrypt state also renders (the error is surfaced) and fills width —
    /// and crucially shows no recovered transcript.
    func testVaultViewRendersFailureStateWithoutPlaintextAndFillsWidth() async throws {
        let service = MockVaultService()
        service.armDecryptFailure(.decryptFailed)
        let store = VaultStore(service: service, recipientStore: InMemoryVaultRecipientStore())
        _ = await store.decrypt(vault: ".opensks/vault/x.age", identityFile: "~/wrong.txt")
        XCTAssertNotNil(store.lastError)
        XCTAssertNil(store.lastImport)

        for width in [1024.0, 1440.0] {
            let view = VaultView(
                store: store,
                activeConversationID: "conv-42",
                pickVaultFile: { nil },
                pickIdentityFile: { nil }
            )
            .frame(width: width, height: 800)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 1
            let image = try XCTUnwrap(renderer.nsImage, "failure-state vault view rendered at width \(width)")
            XCTAssertEqual(image.size.width, width, accuracy: 1.0)
        }
    }
}
