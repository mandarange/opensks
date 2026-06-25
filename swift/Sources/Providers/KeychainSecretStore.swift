import Foundation
import Security

protocol ProviderSecretStoring: Sendable {
    func saveOrReplace(service: String, account: String, credential: SecureCredential) throws -> UInt64
    func delete(service: String, account: String) throws
}

enum ProviderSecretStoreError: LocalizedError, Equatable {
    case emptyCredential
    case keychain(OSStatus)

    var errorDescription: String? {
        switch self {
        case .emptyCredential:
            return "Credential is required."
        case .keychain(let status):
            return "Keychain operation failed (\(status))."
        }
    }
}

struct KeychainSecretStore: ProviderSecretStoring {
    func saveOrReplace(service: String, account: String, credential: SecureCredential) throws -> UInt64 {
        guard !credential.value.isEmpty else { throw ProviderSecretStoreError.emptyCredential }
        let data = Data(credential.value.utf8)
        let query = baseQuery(service: service, account: account)
        let update: [String: Any] = [
            kSecValueData as String: data
        ]
        let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            let addStatus = SecItemAdd(add as CFDictionary, nil)
            guard addStatus == errSecSuccess else { throw ProviderSecretStoreError.keychain(addStatus) }
        } else if status != errSecSuccess {
            throw ProviderSecretStoreError.keychain(status)
        }
        return UInt64(Date().timeIntervalSince1970 * 1000)
    }

    func delete(service: String, account: String) throws {
        let status = SecItemDelete(baseQuery(service: service, account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw ProviderSecretStoreError.keychain(status)
        }
    }

    private func baseQuery(service: String, account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }
}

final class InMemoryProviderSecretStore: ProviderSecretStoring, @unchecked Sendable {
    private var values: [String: String] = [:]
    private var version: UInt64 = 1

    func saveOrReplace(service: String, account: String, credential: SecureCredential) throws -> UInt64 {
        guard !credential.value.isEmpty else { throw ProviderSecretStoreError.emptyCredential }
        values["\(service)|\(account)"] = credential.value
        version += 1
        return version
    }

    func delete(service: String, account: String) throws {
        values.removeValue(forKey: "\(service)|\(account)")
    }

    func contains(service: String, account: String) -> Bool {
        values["\(service)|\(account)"] != nil
    }
}
