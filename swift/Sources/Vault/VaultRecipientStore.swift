// VaultRecipientStore.swift — persists the configured age RECIPIENT (a PUBLIC
// x25519 key, `age1…`) and a NON-SECRET reference to where the matching identity
// (the PRIVATE key) lives on disk. PR-042.
//
// SECURITY INVARIANT: only the PUBLIC recipient is ever persisted. The private
// identity (the age secret key) is NEVER stored by this app — not in the
// Keychain, not in app state, not in a tracked file, and it is never logged. The
// most we keep is a path STRING the operator points us at when they import a
// vault; that path is a reference, not key material, and the file it points to is
// read only transiently by the CLI at decrypt time (this app never opens it).
//
// The recipient (public key) is stored via the Security framework as a
// `kSecClassGenericPassword` item so it persists across launches without living
// in plaintext app preferences. An in-memory store backs tests so they never
// touch the real Keychain.

import Foundation
import Security

// MARK: - Recipient value type

/// A configured age recipient: the PUBLIC key plus an optional NON-SECRET pointer
/// to where the operator keeps the matching identity file. There is deliberately
/// no field that could hold a private key — this type can only ever carry public,
/// non-sensitive data.
struct VaultRecipient: Codable, Sendable, Equatable {
    /// The age x25519 PUBLIC key (`age1…`). This is safe to persist and display.
    let publicKey: String
    /// An OPTIONAL, NON-SECRET reference to where the matching identity lives on
    /// disk (e.g. `~/.config/age/keys.txt`). It is a path string the operator
    /// chooses — NOT the key itself. The app never reads this file; it is only
    /// passed to the CLI's `--identity-file` at decrypt time.
    let identityFileReference: String?

    init(publicKey: String, identityFileReference: String? = nil) {
        self.publicKey = publicKey
        self.identityFileReference = identityFileReference
    }

    /// A lightweight sanity check that a string LOOKS like an age public key.
    /// (The authoritative validation happens in the CLI; this only gates obvious
    /// junk so we never store something that clearly is not a public recipient.)
    static func looksLikePublicKey(_ candidate: String) -> Bool {
        let trimmed = candidate.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.hasPrefix("age1") && trimmed.count >= 8
    }

    /// True when this value carries a plausible public recipient (and, by
    /// construction, NO private key — there is no field for one).
    var isPublicRecipient: Bool { VaultRecipient.looksLikePublicKey(publicKey) }

    /// Redact a recipient key for display: keep the `age1` prefix + a short tail.
    static func redact(_ key: String) -> String {
        let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count > 12 else { return trimmed }
        let prefix = trimmed.prefix(8)
        let suffix = trimmed.suffix(4)
        return "\(prefix)…\(suffix)"
    }

    /// The redacted form of this recipient's public key for inline display.
    var redactedPublicKey: String { VaultRecipient.redact(publicKey) }
}

// MARK: - Store protocol

/// The persistence boundary for the configured recipient. ONLY a public recipient
/// (+ a non-secret identity-file reference) can be stored; there is no method that
/// accepts or returns a private key.
protocol VaultRecipientStoring: AnyObject, Sendable {
    /// Load the configured recipient, if one has been set.
    func load() -> VaultRecipient?
    /// Persist the PUBLIC recipient (+ optional non-secret identity reference).
    /// Returns false if the candidate does not look like a public key (a guard so
    /// a private key can never be saved here).
    @discardableResult
    func save(_ recipient: VaultRecipient) -> Bool
    /// Remove the configured recipient.
    func clear()
}

// MARK: - Keychain-backed store (Security framework)

/// Stores the recipient as a `kSecClassGenericPassword` Keychain item. The stored
/// blob is a JSON encoding of `VaultRecipient` — which contains ONLY the public
/// key + an optional non-secret path reference. No private key material is ever
/// passed to `save`, and the type has no field that could carry one.
final class KeychainVaultRecipientStore: VaultRecipientStoring, @unchecked Sendable {
    private let service: String
    private let account: String

    init(service: String = "com.opensks.studio.vault", account: String = "age-recipient") {
        self.service = service
        self.account = account
    }

    func load() -> VaultRecipient? {
        var query = baseQuery()
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else { return nil }
        return try? JSONDecoder().decode(VaultRecipient.self, from: data)
    }

    @discardableResult
    func save(_ recipient: VaultRecipient) -> Bool {
        // GUARD: refuse to persist anything that is not a public recipient. This is
        // the last line of defence ensuring a private key can never be written here.
        guard recipient.isPublicRecipient else { return false }
        guard let data = try? JSONEncoder().encode(recipient) else { return false }

        var query = baseQuery()
        // Upsert: delete any existing item, then add the fresh one.
        SecItemDelete(query as CFDictionary)
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        let status = SecItemAdd(query as CFDictionary, nil)
        return status == errSecSuccess
    }

    func clear() {
        SecItemDelete(baseQuery() as CFDictionary)
    }

    private func baseQuery() -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }
}

// MARK: - In-memory store (tests / previews)

/// A hermetic recipient store for tests + previews: it never touches the real
/// Keychain. It applies the SAME public-key guard as the Keychain store, so a
/// test can prove that only a public recipient round-trips (a private key is
/// refused — and the type has no field to carry one anyway).
final class InMemoryVaultRecipientStore: VaultRecipientStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: VaultRecipient?

    init(initial: VaultRecipient? = nil) {
        self.stored = initial
    }

    func load() -> VaultRecipient? {
        lock.lock(); defer { lock.unlock() }
        return stored
    }

    @discardableResult
    func save(_ recipient: VaultRecipient) -> Bool {
        guard recipient.isPublicRecipient else { return false }
        lock.lock(); defer { lock.unlock() }
        stored = recipient
        return true
    }

    func clear() {
        lock.lock(); defer { lock.unlock() }
        stored = nil
    }
}
