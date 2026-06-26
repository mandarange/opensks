import Foundation

enum ProviderKind: String, CaseIterable, Codable, Identifiable, Sendable {
    case openRouter = "open_router"
    case openAI = "open_ai"
    case codexLB = "codex_lb"
    case openAICompatible = "open_ai_compatible"
    case localOpenAICompatible = "local_open_ai_compatible"
    case anthropicCompatible = "anthropic_compatible"
    case googleCompatible = "google_compatible"
    case custom

    var id: String { rawValue }

    var displayLabel: String {
        switch self {
        case .openRouter: return "OpenRouter"
        case .openAI: return "OpenAI"
        case .codexLB: return "codex-lb"
        case .openAICompatible: return "OpenAI compatible"
        case .localOpenAICompatible: return "Local OpenAI compatible"
        case .anthropicCompatible: return "Anthropic compatible"
        case .googleCompatible: return "Google compatible"
        case .custom: return "Custom"
        }
    }

    var defaultEndpoint: String {
        switch self {
        case .openRouter: return "https://openrouter.ai/api/v1"
        case .openAI: return "https://api.openai.com/v1"
        case .codexLB, .openAICompatible, .custom: return ""
        case .localOpenAICompatible: return "http://127.0.0.1:11434/v1"
        case .anthropicCompatible, .googleCompatible: return ""
        }
    }

    func codexLBEndpoint(for domain: String) -> String {
        let trimmed = domain.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "" }
        let lowercased = trimmed.lowercased()
        if lowercased.hasPrefix("http://") || lowercased.hasPrefix("https://") {
            let base = trimmed.hasSuffix("/") ? String(trimmed.dropLast()) : trimmed
            if base.lowercased().hasSuffix("/backend-api/codex") {
                return base
            }
            return "\(base)/backend-api/codex"
        }
        let scheme = lowercased.hasPrefix("localhost") || lowercased.hasPrefix("127.0.0.1")
            ? "http"
            : "https"
        return "\(scheme)://\(trimmed)/backend-api/codex"
    }
}

enum ProviderHealthState: String, Sendable {
    case unknown
    case healthy
    case degraded
    case disabled
    case needsCredential
    case needsProbe

    var label: String {
        switch self {
        case .unknown: return "Unknown"
        case .healthy: return "Connected"
        case .degraded: return "Degraded"
        case .disabled: return "Disabled"
        case .needsCredential: return "Needs credential"
        case .needsProbe: return "Needs probe"
        }
    }

    var pillKind: StatusPill.Kind {
        switch self {
        case .healthy: return .success
        case .degraded, .needsCredential, .needsProbe: return .warning
        case .disabled: return .neutral
        case .unknown: return .neutral
        }
    }
}

enum ProviderModelCapability: String, CaseIterable, Identifiable, Sendable {
    case code = "Code"
    case tools = "Tools"
    case vision = "Vision"
    case image = "Image"
    case longContext = "Long context"

    var id: String { rawValue }
}

struct ProviderSecretRef: Codable, Equatable, Sendable {
    var schema: String
    var store: String
    var service: String
    var account: String
    var version: UInt64

    init(
        schema: String = "opensks.secret-ref.v1",
        store: String,
        service: String,
        account: String,
        version: UInt64
    ) {
        self.schema = schema
        self.store = store
        self.service = service
        self.account = account
        self.version = version
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schema = try container.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.secret-ref.v1"
        store = try container.decode(String.self, forKey: .store)
        service = try container.decode(String.self, forKey: .service)
        account = try container.decode(String.self, forKey: .account)
        version = try container.decode(UInt64.self, forKey: .version)
    }
}

struct ProviderConnectionViewModel: Identifiable, Equatable, Sendable {
    var id: String
    var kind: ProviderKind
    var displayName: String
    var endpoint: String
    var enabled: Bool
    var secretRef: ProviderSecretRef
    var health: ProviderHealthState
    var enabledModelCount: Int
    var activeRequests: Int
    var maxConcurrentRequests: Int
    var lastDiagnostic: String
    var circuitOpen: Bool = false
    var lastCheckedAtMs: UInt64? = nil
    var diagnosticRef: String? = nil
    var adapterDiagnostic: String? = nil
    var adapterBlockers: [String] = []
    var adapterCheckGeneratedAt: ProviderAdapterCheckGeneratedAt? = nil
    var adapterCheckDetail: String? = nil
    var revision: UInt64

    var statusLabel: String {
        if enabled && circuitOpen { return "Circuit open" }
        return enabled ? health.label : ProviderHealthState.disabled.label
    }

    var statusPillKind: StatusPill.Kind {
        guard enabled else { return .neutral }
        if circuitOpen { return .warning }
        return health.pillKind
    }

    var circuitLabel: String {
        circuitOpen ? "Open" : "Closed"
    }

    var lastCheckedLabel: String {
        guard let lastCheckedAtMs else { return "Not checked" }
        let checkedAt = Date(timeIntervalSince1970: TimeInterval(lastCheckedAtMs) / 1000)
        return checkedAt.formatted(.dateTime.year().month().day().hour().minute())
    }

    var diagnosticReferenceLabel: String? {
        let trimmed = diagnosticRef?.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let trimmed, !trimmed.isEmpty else { return nil }
        return trimmed
    }

    var adapterCheckGeneratedAtLabel: String? {
        guard let adapterCheckGeneratedAt else { return nil }
        return "unix \(adapterCheckGeneratedAt.unixSeconds)"
    }
}

struct ProviderModelViewModel: Identifiable, Equatable, Sendable {
    var id: String
    var providerID: String
    var remoteModelID: String
    var displayName: String
    var enabled: Bool
    var health: ProviderHealthState
    var capabilities: Set<ProviderModelCapability>
    var contextWindow: Int?
    var priceSummary: String?

    var isEligibleForCode: Bool {
        enabled && capabilities.contains(.code) && health == .healthy
    }

    var isEligibleForImage: Bool {
        enabled && capabilities.contains(.image) && health == .healthy
    }

    var isEligibleForVision: Bool {
        enabled && capabilities.contains(.vision) && health == .healthy
    }
}

struct ProviderDraft: Equatable, Sendable {
    var kind: ProviderKind = .openRouter
    var displayName = "OpenRouter"
    var endpoint = ProviderKind.openRouter.defaultEndpoint
    var codexLbDomain = ""
    var organizationRef = ""
    var projectRef = ""
    var enabled = true
    var maxConcurrentRequests: Int = 2

    var resolvedEndpoint: String {
        switch kind {
        case .codexLB:
            return kind.codexLBEndpoint(for: codexLbDomain)
        default:
            return endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }
}

struct SecureCredential: Equatable, Sendable {
    var value: String
}

enum ProviderSyncState: Equatable, Sendable {
    case idle
    case saving
    case probing
    case syncing
    case failed(String)
}
