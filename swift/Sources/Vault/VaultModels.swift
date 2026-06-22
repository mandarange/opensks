// VaultModels.swift — Codable mirrors of the SHARED VAULT JSON CONTRACT
// (snake_case) for PR-042. Every model is the result of a SUBCOMMAND of the NEW
// `vault` verb on `opensks-cli`:
//
//   • `opensks vault export-summary` → `opensks.vault-summary.v1`
//        a SANITIZED, git-trackable summary — decisions + run links, with an
//        explicit `contains_raw_transcript:false` / `redacted:true` indicator. No
//        raw transcript, no secrets.
//   • `opensks vault encrypt`        → `opensks.vault-encrypt.v1`
//        the OPT-IN encryption of the FULL transcript into a `.age` vault FOR a
//        recipient (an age x25519 PUBLIC key). The result names the `.age` path,
//        the (redacted) recipient, and the byte size — never any plaintext.
//   • `opensks vault decrypt`        → `opensks.vault-decrypt.v1`
//        the IMPORT — only possible WITH the matching age IDENTITY (the private
//        key, supplied as an on-disk identity file the app NEVER reads or stores).
//        The result is just the conversation id + byte size — never plaintext.
//   • `opensks vault status`         → `opensks.vault-status.v1`
//        the per-workspace inventory: sanitized summaries + the `.age` vaults
//        present (each with a REDACTED recipient — never a raw key).
//
// On ANY encryption / decryption error the CLI emits `opensks.vault-error.v1`
// with a typed `code` (`encrypt_failed` / `bad_recipient` / `decrypt_failed`);
// these map to `VaultServiceError` and a clear human message. A failure NEVER
// carries plaintext or ciphertext — only the code (+ an optional non-sensitive
// message), so nothing secret can leak through the error path.
//
// Decoding is tolerant (lenient enums, optional fields default) so a future
// server value never crashes the decoder.

import Foundation

// MARK: - Sanitized summary (export-summary)

/// Mirrors `opensks.vault-summary.v1` — a SANITIZED, git-trackable summary of a
/// conversation: the count of decisions captured and the linked run ids, plus the
/// path to the written summary file. It carries an EXPLICIT honesty indicator:
/// `containsRawTranscript` is always false and `redacted` is true — this summary
/// is safe to commit because it holds NO raw transcript and NO secrets.
struct VaultSummary: Codable, Sendable, Equatable, Identifiable {
    let schema: String
    let conversationId: String
    let summaryPath: String
    let decisions: Int
    let runLinks: [String]
    /// Honesty indicator — a sanitized summary NEVER contains the raw transcript.
    let containsRawTranscript: Bool
    /// Honesty indicator — the summary is redacted (secrets stripped) before write.
    let redacted: Bool

    /// Stable identity for `ForEach` — one summary per conversation in a workspace.
    var id: String { conversationId }

    enum CodingKeys: String, CodingKey {
        case schema
        case conversationId = "conversation_id"
        case summaryPath = "summary_path"
        case decisions
        case runLinks = "run_links"
        case containsRawTranscript = "contains_raw_transcript"
        case redacted
    }

    init(
        schema: String,
        conversationId: String,
        summaryPath: String,
        decisions: Int,
        runLinks: [String],
        containsRawTranscript: Bool = false,
        redacted: Bool = true
    ) {
        self.schema = schema
        self.conversationId = conversationId
        self.summaryPath = summaryPath
        self.decisions = decisions
        self.runLinks = runLinks
        self.containsRawTranscript = containsRawTranscript
        self.redacted = redacted
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        conversationId = try c.decode(String.self, forKey: .conversationId)
        summaryPath = try c.decode(String.self, forKey: .summaryPath)
        decisions = try c.decodeIfPresent(Int.self, forKey: .decisions) ?? 0
        runLinks = try c.decodeIfPresent([String].self, forKey: .runLinks) ?? []
        // Default to the SAFE values if a field is absent: a summary is, by
        // definition, redacted and free of any raw transcript.
        containsRawTranscript = try c.decodeIfPresent(Bool.self, forKey: .containsRawTranscript) ?? false
        redacted = try c.decodeIfPresent(Bool.self, forKey: .redacted) ?? true
    }

    /// The honesty label surfaced beside a summary: a summary is git-trackable
    /// precisely because it contains no raw transcript and is redacted.
    var safetyLabel: String {
        containsRawTranscript
            ? "Contains raw transcript — NOT safe to commit"
            : "No raw transcript · redacted · safe to commit"
    }

    /// True only when this summary is provably safe to git-track (no raw
    /// transcript AND redacted). The view shows a green pill only in this case.
    var isGitTrackable: Bool { !containsRawTranscript && redacted }
}

// MARK: - Encrypt result (encrypt)

/// Mirrors `opensks.vault-encrypt.v1` — the OPT-IN encryption of the full
/// transcript into a `.age` vault for a recipient. Carries the `.age` path, the
/// recipient's age PUBLIC key, and the encrypted byte size. NO plaintext is ever
/// present in this result.
struct VaultEncryptResult: Codable, Sendable, Equatable {
    let schema: String
    let vaultPath: String
    /// The age x25519 PUBLIC recipient the vault was encrypted for (`age1…`).
    let recipient: String
    let bytes: Int

    enum CodingKeys: String, CodingKey {
        case schema
        case vaultPath = "vault_path"
        case recipient
        case bytes
    }

    /// A redacted form of the recipient for display in receipts (only the prefix
    /// + a short tail). Never shows the full key in a casual receipt line.
    var recipientRedacted: String { VaultRecipient.redact(recipient) }
}

// MARK: - Decrypt result (decrypt / import)

/// Mirrors `opensks.vault-decrypt.v1` — the IMPORT of a `.age` vault, only
/// possible WITH the matching age identity. Carries just the conversation id that
/// was recovered + the decrypted byte size. NO plaintext is ever present here —
/// the recovered transcript stays out of this DTO entirely.
struct VaultDecryptResult: Codable, Sendable, Equatable {
    let schema: String
    let conversationId: String
    let bytes: Int

    enum CodingKeys: String, CodingKey {
        case schema
        case conversationId = "conversation_id"
        case bytes
    }
}

// MARK: - Status (status)

/// One `.age` vault present in a workspace, as surfaced by `vault status`. The
/// recipient is ALREADY redacted server-side (`recipient_redacted`) so a raw key
/// is never carried over the boundary or shown in the inventory.
struct VaultFile: Codable, Sendable, Equatable, Identifiable {
    let path: String
    let recipientRedacted: String

    /// Stable identity for `ForEach` — the `.age` path is unique in a workspace.
    var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path
        case recipientRedacted = "recipient_redacted"
    }
}

/// Mirrors `opensks.vault-status.v1` — the per-workspace inventory: the sanitized
/// (git-trackable) summaries and the `.age` vaults present (each with a redacted
/// recipient).
struct VaultStatus: Codable, Sendable, Equatable {
    let schema: String
    let summaries: [VaultSummary]
    let vaults: [VaultFile]

    enum CodingKeys: String, CodingKey {
        case schema, summaries, vaults
    }

    init(schema: String, summaries: [VaultSummary], vaults: [VaultFile]) {
        self.schema = schema
        self.summaries = summaries
        self.vaults = vaults
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        summaries = try c.decodeIfPresent([VaultSummary].self, forKey: .summaries) ?? []
        vaults = try c.decodeIfPresent([VaultFile].self, forKey: .vaults) ?? []
    }

    static let empty = VaultStatus(schema: "opensks.vault-status.v1", summaries: [], vaults: [])
}

// MARK: - Error envelope (vault-error)

/// `opensks.vault-error.v1` — the structured error emitted on a non-zero exit
/// from `encrypt` / `decrypt`. Carries a typed `code` and, at most, a
/// NON-SENSITIVE message. It NEVER carries plaintext or ciphertext — by contract
/// a failure leaves nothing secret on the wire (or on disk).
struct VaultErrorEnvelope: Codable, Sendable, Equatable {
    let schema: String
    let error: Payload

    struct Payload: Codable, Sendable, Equatable {
        let code: String
        let message: String?

        enum CodingKeys: String, CodingKey {
            case code, message
        }

        init(code: String, message: String? = nil) {
            self.code = code
            self.message = message
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            code = try c.decode(String.self, forKey: .code)
            message = try c.decodeIfPresent(String.self, forKey: .message)
        }
    }

    enum CodingKeys: String, CodingKey {
        case schema, error
    }
}

// MARK: - Typed errors

/// The typed errors surfaced by the vault service. The first two are transport /
/// generic-service failures; the rest are the typed vault errors decoded from
/// `opensks.vault-error.v1`. Each maps to a clear, NON-SENSITIVE human message —
/// no error path ever surfaces plaintext or ciphertext.
enum VaultServiceError: Error, Equatable {
    /// The process could not be launched / produced unparseable output.
    case transport(message: String)
    /// A non-zero exit with a decodable (non-sensitive) message.
    case service(message: String)
    /// `encrypt_failed` — encryption did not complete. By contract NO plaintext
    /// and NO partial `.age` is left on disk.
    case encryptFailed
    /// `bad_recipient` — the supplied recipient was not a valid age public key.
    /// Nothing was encrypted.
    case badRecipient
    /// `decrypt_failed` — the identity was missing or wrong; the vault could not
    /// be opened. NO plaintext was produced or leaked.
    case decryptFailed

    /// A clear, human-facing message. Crucially these are GENERIC — they describe
    /// the failure mode WITHOUT echoing any key material, transcript, or cipher.
    var humanMessage: String {
        switch self {
        case .transport(let message):
            return message
        case .service(let message):
            return message
        case .encryptFailed:
            return "Encryption failed. Nothing was written — no plaintext and no partial vault was left on disk."
        case .badRecipient:
            return "That recipient is not a valid age public key. Nothing was encrypted."
        case .decryptFailed:
            return "Could not open the vault: the identity is missing or does not match. No transcript was recovered."
        }
    }
}
