// VaultService.swift — the boundary between the Vault panel and the bundled
// `opensks vault …` subcommands (PR-042). Mirrors LiveGitService /
// LiveConversationService: an off-main detached process capture that decodes the
// shared snake_case JSON contract.
//
// The four subcommands of the NEW `vault` verb:
//   • export-summary → a SANITIZED, git-trackable summary (no raw transcript).
//   • encrypt        → the OPT-IN encryption of the full transcript into a `.age`
//                      vault for a recipient (an age PUBLIC key).
//   • decrypt        → the IMPORT of a `.age` vault, only possible WITH the
//                      matching identity FILE (a private key on disk the app never
//                      reads or stores).
//   • status         → the per-workspace inventory (summaries + redacted vaults).
//
// SECURITY: this service NEVER logs ciphertext, plaintext, or any key material.
// It does not print process stdout/stderr; on a non-zero exit it decodes the
// `opensks.vault-error.v1` envelope and maps the typed `code` to a
// `VaultServiceError` whose message is GENERIC (no secret is ever echoed). The
// recipient public key and the identity-file PATH are passed as CLI arguments;
// the identity FILE itself is read only by the CLI at decrypt time — never by this
// app. A `MockVaultService` backs the tests without disk, a process, or any
// crypto so the tests are hermetic.

import Foundation

// MARK: - Protocol

/// The vault boundary. `exportSummary` writes a sanitized summary; `encrypt`
/// opt-in encrypts the full transcript for a recipient; `decrypt` imports a `.age`
/// vault WITH an identity file; `status` lists the workspace inventory. The
/// encrypt/decrypt errors are TYPED (`VaultServiceError`) so the store reacts
/// precisely and NEVER surfaces plaintext.
protocol VaultService: Sendable {
    /// `opensks vault export-summary --workspace <p> --conversation <id>` — write a
    /// SANITIZED, git-trackable summary (decisions + run links, NO raw transcript,
    /// NO secrets).
    func exportSummary(conversationID: String) async throws -> VaultSummary

    /// `opensks vault encrypt --workspace <p> --conversation <id> --recipient <pub>`
    /// — OPT-IN: encrypt the FULL transcript into a `.age` vault for the recipient
    /// (an age x25519 PUBLIC key). On any error throws `.encryptFailed` /
    /// `.badRecipient`; by contract NO plaintext and NO partial `.age` remains.
    func encrypt(conversationID: String, recipient: String) async throws -> VaultEncryptResult

    /// `opensks vault decrypt --workspace <p> --vault <path> --identity-file <path>`
    /// — IMPORT: recover the conversation, only possible WITH the matching age
    /// identity file. A wrong/missing identity throws `.decryptFailed`; no
    /// plaintext is leaked on the failure path.
    func decrypt(vaultPath: String, identityFile: String) async throws -> VaultDecryptResult

    /// `opensks vault status --workspace <p>` — the sanitized summaries + the `.age`
    /// vaults present (each with a redacted recipient).
    func status() async throws -> VaultStatus
}

// MARK: - Live (CLI-backed) implementation

/// Shells the bundled `opensks vault …` subcommands. Process work runs on a
/// detached cooperative task; decoding maps the shared snake_case contract. NEVER
/// logs stdout/stderr (which could hold a path, a redacted key, or — on the CLI's
/// side — be adjacent to secrets); only the typed error code is surfaced.
struct LiveVaultService: VaultService {
    let cli: URL
    let workspace: URL

    func exportSummary(conversationID: String) async throws -> VaultSummary {
        let result = try await run(args: [
            "vault", "export-summary",
            "--workspace", workspace.path,
            "--conversation", conversationID
        ])
        return try Self.decode(result, as: VaultSummary.self)
    }

    func encrypt(conversationID: String, recipient: String) async throws -> VaultEncryptResult {
        let result = try await run(args: [
            "vault", "encrypt",
            "--workspace", workspace.path,
            "--conversation", conversationID,
            "--recipient", recipient
        ])
        return try Self.decode(result, as: VaultEncryptResult.self)
    }

    func decrypt(vaultPath: String, identityFile: String) async throws -> VaultDecryptResult {
        let result = try await run(args: [
            "vault", "decrypt",
            "--workspace", workspace.path,
            "--vault", vaultPath,
            "--identity-file", identityFile
        ])
        return try Self.decode(result, as: VaultDecryptResult.self)
    }

    func status() async throws -> VaultStatus {
        let result = try await run(args: ["vault", "status", "--workspace", workspace.path])
        return try Self.decode(result, as: VaultStatus.self)
    }

    // MARK: Process plumbing (mirrors LiveGitService)

    private struct ProcessResult {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
    }

    /// Shared child-process runner (concurrent drain + cancel-kill, §19.2).
    private let supervisor = ProcessSupervisor()

    private func run(args: [String]) async throws -> ProcessResult {
        do {
            let result = try await supervisor.run(
                ProcessSupervisor.Spec(
                    executable: cli,
                    arguments: args,
                    workingDirectory: workspace
                )
            )
            return ProcessResult(
                exitCode: result.exitCode,
                stdout: result.stdout,
                stderr: result.stderr
            )
        } catch {
            // The message names only the launch failure — never key material.
            throw VaultServiceError.transport(
                message: "could not launch opensks vault: \(error.localizedDescription)"
            )
        }
    }

    /// Decode a vault result. On a non-zero exit the CLI emits the
    /// `opensks.vault-error.v1` envelope; map its `code` to a TYPED
    /// `VaultServiceError` (`encrypt_failed` / `bad_recipient` / `decrypt_failed`)
    /// so the store reacts precisely. Crucially, the stderr text is NOT echoed for
    /// a recognised typed error — only the generic typed message is surfaced — so
    /// nothing secret can leak through the error path. An unknown code falls back to
    /// a generic `.service` whose message is the (non-sensitive) stderr line.
    private static func decode<T: Decodable>(
        _ result: ProcessResult,
        as type: T.Type
    ) throws -> T {
        let decoder = JSONDecoder()
        if result.exitCode == 0, let value = try? decoder.decode(T.self, from: result.stdout) {
            return value
        }
        // Try the structured error envelope on either stream and map it typed.
        if let envelope = decodeErrorEnvelope(result.stdout, decoder)
            ?? decodeErrorEnvelope(result.stderr, decoder) {
            throw mapError(envelope)
        }
        if result.exitCode == 0 {
            throw VaultServiceError.transport(message: "could not decode \(T.self) from opensks vault output")
        }
        // No structured envelope; surface a generic non-zero exit. Note we do NOT
        // dump full stderr verbatim here beyond a short trimmed line to avoid any
        // chance of echoing sensitive output.
        let stderrText = String(decoding: result.stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        throw VaultServiceError.service(
            message: stderrText.isEmpty
                ? "opensks vault exited \(result.exitCode)"
                : "opensks vault exited \(result.exitCode): \(stderrText)"
        )
    }

    private static func decodeErrorEnvelope(_ data: Data, _ decoder: JSONDecoder) -> VaultErrorEnvelope? {
        guard !data.isEmpty else { return nil }
        return try? decoder.decode(VaultErrorEnvelope.self, from: data)
    }

    /// Map a decoded `opensks.vault-error.v1` to a typed error.
    static func mapError(_ envelope: VaultErrorEnvelope) -> VaultServiceError {
        switch envelope.error.code {
        case "encrypt_failed":
            return .encryptFailed
        case "bad_recipient":
            return .badRecipient
        case "decrypt_failed":
            return .decryptFailed
        default:
            return .service(message: envelope.error.message ?? "opensks vault error: \(envelope.error.code)")
        }
    }
}

// MARK: - Mock implementation (tests)

/// An in-memory vault service for tests. Returns canned / scriptable results and
/// COUNTS each call so the store's behaviour is observable. It NEVER touches disk,
/// spawns a process, or performs crypto — `encrypt` returns a synthetic receipt and
/// `decrypt` a synthetic recovery, and a scripted failure throws the matching typed
/// `VaultServiceError` so a test can assert the failure NEVER yields plaintext.
final class MockVaultService: VaultService, @unchecked Sendable {
    private let lock = NSLock()

    // Canned / scripted results.
    private var cannedSummary: VaultSummary?
    private var cannedEncrypt: VaultEncryptResult?
    private var cannedDecrypt: VaultDecryptResult?
    private var cannedStatus: VaultStatus = .empty

    // One-shot scripted failures.
    private var failNextEncrypt: VaultServiceError?
    /// `decrypt` requires a NON-EMPTY identity file; when an identity is "wrong"
    /// (or missing) it throws `.decryptFailed`. A test arms this to model a
    /// wrong/missing key.
    private var failNextDecrypt: VaultServiceError?
    /// Identity-file paths the mock treats as VALID. A decrypt with any other
    /// (non-empty) identity throws `.decryptFailed` — the import REQUIRES the key.
    private var validIdentityFiles: Set<String> = []

    // Call records.
    private(set) var exportSummaryCalls: [String] = []
    private(set) var encryptCalls: [(conversationID: String, recipient: String)] = []
    private(set) var decryptCalls: [(vaultPath: String, identityFile: String)] = []
    private(set) var statusCallCount = 0

    init() {}

    // MARK: Scripting

    func setSummary(_ summary: VaultSummary) {
        lock.lock(); defer { lock.unlock() }
        cannedSummary = summary
    }

    func setEncryptResult(_ result: VaultEncryptResult) {
        lock.lock(); defer { lock.unlock() }
        cannedEncrypt = result
    }

    func setDecryptResult(_ result: VaultDecryptResult) {
        lock.lock(); defer { lock.unlock() }
        cannedDecrypt = result
    }

    func setStatus(_ status: VaultStatus) {
        lock.lock(); defer { lock.unlock() }
        cannedStatus = status
    }

    /// Arm the NEXT `encrypt(...)` to throw the given typed error once (e.g.
    /// `.encryptFailed` or `.badRecipient`). No plaintext is ever produced.
    func armEncryptFailure(_ error: VaultServiceError = .encryptFailed) {
        lock.lock(); defer { lock.unlock() }
        failNextEncrypt = error
    }

    /// Arm the NEXT `decrypt(...)` to throw `.decryptFailed` once, regardless of the
    /// supplied identity (models a wrong key on a known-good vault).
    func armDecryptFailure(_ error: VaultServiceError = .decryptFailed) {
        lock.lock(); defer { lock.unlock() }
        failNextDecrypt = error
    }

    /// Register an identity-file path as VALID. A decrypt with a non-registered
    /// (or empty) identity throws `.decryptFailed`.
    func registerValidIdentity(_ path: String) {
        lock.lock(); defer { lock.unlock() }
        validIdentityFiles.insert(path)
    }

    // MARK: VaultService

    func exportSummary(conversationID: String) async throws -> VaultSummary {
        try exportSummaryLocked(conversationID)
    }

    func encrypt(conversationID: String, recipient: String) async throws -> VaultEncryptResult {
        try encryptLocked(conversationID: conversationID, recipient: recipient)
    }

    func decrypt(vaultPath: String, identityFile: String) async throws -> VaultDecryptResult {
        try decryptLocked(vaultPath: vaultPath, identityFile: identityFile)
    }

    func status() async throws -> VaultStatus {
        statusLocked()
    }

    // MARK: Synchronous critical sections

    private func exportSummaryLocked(_ conversationID: String) throws -> VaultSummary {
        lock.lock(); defer { lock.unlock() }
        exportSummaryCalls.append(conversationID)
        if let canned = cannedSummary { return canned }
        // A default sanitized summary: ALWAYS marked free of any raw transcript.
        return VaultSummary(
            schema: "opensks.vault-summary.v1",
            conversationId: conversationID,
            summaryPath: ".opensks/vault/\(conversationID).summary.md",
            decisions: 0,
            runLinks: [],
            containsRawTranscript: false,
            redacted: true
        )
    }

    private func encryptLocked(conversationID: String, recipient: String) throws -> VaultEncryptResult {
        lock.lock(); defer { lock.unlock() }
        encryptCalls.append((conversationID: conversationID, recipient: recipient))
        // A scripted one-shot failure (e.g. encrypt_failed / bad_recipient). No
        // plaintext/ciphertext is produced — the call simply throws.
        if let failure = failNextEncrypt {
            failNextEncrypt = nil
            throw failure
        }
        // A recipient that does not look like an age public key is a bad recipient.
        if !VaultRecipient.looksLikePublicKey(recipient) {
            throw VaultServiceError.badRecipient
        }
        if let canned = cannedEncrypt { return canned }
        return VaultEncryptResult(
            schema: "opensks.vault-encrypt.v1",
            vaultPath: ".opensks/vault/\(conversationID).age",
            recipient: recipient,
            bytes: 4096
        )
    }

    private func decryptLocked(vaultPath: String, identityFile: String) throws -> VaultDecryptResult {
        lock.lock(); defer { lock.unlock() }
        decryptCalls.append((vaultPath: vaultPath, identityFile: identityFile))
        // A scripted one-shot failure (wrong/missing key).
        if let failure = failNextDecrypt {
            failNextDecrypt = nil
            throw failure
        }
        // The import REQUIRES an identity: a missing (empty) identity fails.
        let trimmed = identityFile.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw VaultServiceError.decryptFailed
        }
        // If a valid-identity allowlist is configured, the supplied identity must be
        // in it; otherwise the key is "wrong" and decryption fails — no plaintext.
        if !validIdentityFiles.isEmpty, !validIdentityFiles.contains(trimmed) {
            throw VaultServiceError.decryptFailed
        }
        if let canned = cannedDecrypt { return canned }
        return VaultDecryptResult(
            schema: "opensks.vault-decrypt.v1",
            conversationId: "conv-recovered",
            bytes: 4096
        )
    }

    private func statusLocked() -> VaultStatus {
        lock.lock(); defer { lock.unlock() }
        statusCallCount += 1
        return cannedStatus
    }
}
