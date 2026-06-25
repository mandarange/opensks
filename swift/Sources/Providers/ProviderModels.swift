import Foundation

enum ProviderKind: String, CaseIterable, Codable, Identifiable, Sendable {
    case openRouter = "open_router"
    case openAI = "open_ai"
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
        case .openAICompatible, .custom: return ""
        case .localOpenAICompatible: return "http://127.0.0.1:11434/v1"
        case .anthropicCompatible, .googleCompatible: return ""
        }
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
    var store: String
    var service: String
    var account: String
    var version: UInt64
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
    var organizationRef = ""
    var projectRef = ""
    var enabled = true
    var maxConcurrentRequests: Int = 2
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
