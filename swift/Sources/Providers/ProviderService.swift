import Foundation

protocol ProviderRegistryService: Sendable {
    func registryState() async throws -> ProviderRegistryState
    func upsertConnection(
        _ connection: ProviderConnectionRecord,
        expectedRevision: UInt64?
    ) async throws -> ProviderRegistryCommandResult
    func deleteProvider(id: String, expectedRevision: UInt64) async throws -> ProviderRegistryCommandResult
    func setProviderEnabled(
        id: String,
        enabled: Bool,
        expectedRevision: UInt64
    ) async throws -> ProviderRegistryCommandResult
    func probeProvider(id: String) async throws -> ProviderRegistryProbeResult
    func syncModels(providerID: String, models: [ProviderModelRecord]) async throws -> ProviderRegistryCommandResult
    func setModelEnabled(id: String, enabled: Bool) async throws -> ProviderRegistryModelResult
    func recordProbe(_ receipt: ProviderProbeReceiptRecord) async throws -> ProviderRegistryCommandResult
}

enum ProviderRegistryServiceError: LocalizedError {
    case emptyOutput(String)
    case decodeFailed(String, underlying: String)
    case nonZeroExit(Int32, stderr: String)
    case launchFailed(String)

    var errorDescription: String? {
        switch self {
        case .emptyOutput(let verb):
            return "opensks provider \(verb) returned no output"
        case .decodeFailed(let verb, let underlying):
            return "could not decode provider \(verb): \(underlying)"
        case .nonZeroExit(let code, let stderr):
            let trimmed = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            return "provider command exited \(code)\(trimmed.isEmpty ? "" : ": \(trimmed)")"
        case .launchFailed(let message):
            return "could not start provider command: \(message)"
        }
    }
}

struct LiveProviderRegistryService: ProviderRegistryService {
    let cli: URL
    let workspace: URL

    func registryState() async throws -> ProviderRegistryState {
        var state: ProviderRegistryState = try await run(
            ["provider", "registry-list", "--workspace", workspace.path],
            verb: "registry-list"
        )
        state.adapterCheckReport = loadAdapterCheckReport()
        return state
    }

    func upsertConnection(
        _ connection: ProviderConnectionRecord,
        expectedRevision: UInt64?
    ) async throws -> ProviderRegistryCommandResult {
        var args = [
            "provider", "registry-upsert",
            "--workspace", workspace.path,
            "--connection", String(decoding: try JSONEncoder.opensks.encode(connection), as: UTF8.self)
        ]
        if let expectedRevision {
            args += ["--expected-revision", String(expectedRevision)]
        }
        return try await run(args, verb: "registry-upsert")
    }

    func deleteProvider(id: String, expectedRevision: UInt64) async throws -> ProviderRegistryCommandResult {
        try await run([
            "provider", "registry-delete",
            "--workspace", workspace.path,
            "--provider", id,
            "--expected-revision", String(expectedRevision)
        ], verb: "registry-delete")
    }

    func setProviderEnabled(
        id: String,
        enabled: Bool,
        expectedRevision: UInt64
    ) async throws -> ProviderRegistryCommandResult {
        try await run([
            "provider", "registry-set-enabled",
            "--workspace", workspace.path,
            "--provider", id,
            "--enabled", enabled ? "true" : "false",
            "--expected-revision", String(expectedRevision)
        ], verb: "registry-set-enabled")
    }

    func probeProvider(id: String) async throws -> ProviderRegistryProbeResult {
        try await run([
            "provider", "registry-probe",
            "--workspace", workspace.path,
            "--provider", id
        ], verb: "registry-probe")
    }

    func syncModels(providerID: String, models: [ProviderModelRecord]) async throws -> ProviderRegistryCommandResult {
        try await run([
            "provider", "registry-sync-models",
            "--workspace", workspace.path,
            "--provider", providerID,
            "--models", String(decoding: try JSONEncoder.opensks.encode(models), as: UTF8.self)
        ], verb: "registry-sync-models")
    }

    func setModelEnabled(id: String, enabled: Bool) async throws -> ProviderRegistryModelResult {
        try await run([
            "provider", "registry-set-model-enabled",
            "--workspace", workspace.path,
            "--model", id,
            "--enabled", enabled ? "true" : "false"
        ], verb: "registry-set-model-enabled")
    }

    func recordProbe(_ receipt: ProviderProbeReceiptRecord) async throws -> ProviderRegistryCommandResult {
        try await run([
            "provider", "registry-record-probe",
            "--workspace", workspace.path,
            "--receipt", String(decoding: try JSONEncoder.opensks.encode(receipt), as: UTF8.self)
        ], verb: "registry-record-probe")
    }

    private func run<T: Decodable>(_ args: [String], verb: String) async throws -> T {
        let result: ProcessSupervisor.RunResult
        do {
            result = try await ProcessSupervisor().run(
                ProcessSupervisor.Spec(
                    executable: cli,
                    arguments: args,
                    workingDirectory: workspace
                )
            )
        } catch {
            throw ProviderRegistryServiceError.launchFailed(error.localizedDescription)
        }
        if result.exitCode != 0 {
            throw ProviderRegistryServiceError.nonZeroExit(
                result.exitCode,
                stderr: String(decoding: result.stderr, as: UTF8.self)
            )
        }
        guard !result.stdout.isEmpty else {
            throw ProviderRegistryServiceError.emptyOutput(verb)
        }
        do {
            return try JSONDecoder.opensks.decode(T.self, from: result.stdout)
        } catch {
            throw ProviderRegistryServiceError.decodeFailed(verb, underlying: error.localizedDescription)
        }
    }

    private func loadAdapterCheckReport() -> ProviderAdapterCheckReport? {
        let url = workspace
            .appendingPathComponent(".opensks", isDirectory: true)
            .appendingPathComponent("providers", isDirectory: true)
            .appendingPathComponent("provider-adapter-check.json")
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder.opensks.decode(ProviderAdapterCheckReport.self, from: data)
    }
}

struct ProviderRegistryState: Codable, Equatable, Sendable {
    var schema: String
    var providers: [ProviderConnectionRecord]
    var models: [ProviderModelRecord]
    var latestProbes: [ProviderProbeReceiptRecord]
    var adapterCheckReport: ProviderAdapterCheckReport? = nil
}

struct ProviderAdapterCheckReport: Codable, Equatable, Sendable {
    var schema: String
    var remoteProbeOptIn: Bool
    var secretValueExposed: Bool
    var summary: ProviderAdapterCheckSummary
    var blockers: [String]
    var adapters: [ProviderAdapterCheckRow]

    init(
        schema: String,
        remoteProbeOptIn: Bool,
        secretValueExposed: Bool,
        summary: ProviderAdapterCheckSummary,
        blockers: [String],
        adapters: [ProviderAdapterCheckRow]
    ) {
        self.schema = schema
        self.remoteProbeOptIn = remoteProbeOptIn
        self.secretValueExposed = secretValueExposed
        self.summary = summary
        self.blockers = blockers
        self.adapters = adapters
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schema = try container.decode(String.self, forKey: .schema)
        remoteProbeOptIn = try container.decode(Bool.self, forKey: .remoteProbeOptIn)
        secretValueExposed = try container.decode(Bool.self, forKey: .secretValueExposed)
        summary = try container.decode(ProviderAdapterCheckSummary.self, forKey: .summary)
        blockers = try container.decodeIfPresent([String].self, forKey: .blockers) ?? []
        adapters = try container.decode([ProviderAdapterCheckRow].self, forKey: .adapters)
    }
}

struct ProviderAdapterCheckSummary: Codable, Equatable, Sendable {
    var total: Int
    var attempted: Int
    var reachable: Int
}

struct ProviderAdapterCheckRow: Codable, Equatable, Sendable {
    var name: String
    var configured: Bool
    var attempted: Bool
    var status: String
    var blockers: [String]
    var credentialSource: String
    var endpoint: String
    var httpCode: String?
    var secretValueExposed: Bool

    init(
        name: String,
        configured: Bool,
        attempted: Bool,
        status: String,
        blockers: [String],
        credentialSource: String,
        endpoint: String,
        httpCode: String?,
        secretValueExposed: Bool
    ) {
        self.name = name
        self.configured = configured
        self.attempted = attempted
        self.status = status
        self.blockers = blockers
        self.credentialSource = credentialSource
        self.endpoint = endpoint
        self.httpCode = httpCode
        self.secretValueExposed = secretValueExposed
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        configured = try container.decode(Bool.self, forKey: .configured)
        attempted = try container.decode(Bool.self, forKey: .attempted)
        status = try container.decode(String.self, forKey: .status)
        blockers = try container.decodeIfPresent([String].self, forKey: .blockers) ?? []
        credentialSource = try container.decode(String.self, forKey: .credentialSource)
        endpoint = try container.decode(String.self, forKey: .endpoint)
        httpCode = try container.decodeIfPresent(String.self, forKey: .httpCode)
        secretValueExposed = try container.decode(Bool.self, forKey: .secretValueExposed)
    }
}

struct ProviderRegistryCommandResult: Codable, Equatable, Sendable {
    var schema: String
    var receipt: ProviderMutationReceiptRecord
}

struct ProviderRegistryModelResult: Codable, Equatable, Sendable {
    var schema: String
    var model: ProviderModelRecord
}

struct ProviderRegistryProbeResult: Codable, Equatable, Sendable {
    var schema: String
    var provider: ProviderConnectionRecord
    var probeReceipt: ProviderProbeReceiptRecord
    var models: [ProviderModelRecord]
    var syncReceipt: ProviderMutationReceiptRecord?
}

struct ProviderConnectionRecord: Codable, Equatable, Sendable {
    var schema: String
    var id: String
    var kind: ProviderKind
    var displayName: String
    var enabled: Bool
    var endpoint: ProviderEndpointRecord
    var auth: ProviderSecretRef
    var organizationRef: String?
    var projectRef: String?
    var health: ProviderHealthSnapshotRecord
    var concurrency: ProviderConcurrencyRecord
    var createdAtMs: UInt64
    var updatedAtMs: UInt64
    var revision: UInt64
}

struct ProviderEndpointRecord: Codable, Equatable, Sendable {
    var baseUrl: String
    var allowInsecureHttp: Bool
}

struct ProviderHealthSnapshotRecord: Codable, Equatable, Sendable {
    var state: String
    var circuitOpen: Bool
    var checkedAtMs: UInt64?
    var reasonCode: String
    var diagnosticRef: String?
}

struct ProviderConcurrencyRecord: Codable, Equatable, Sendable {
    var maxConcurrentRequests: UInt32
    var requestsPerMinute: UInt32?
    var tokensPerMinute: UInt64?
}

struct ProviderModelRecord: Codable, Equatable, Sendable {
    var schema: String
    var id: String
    var providerId: String
    var remoteModelId: String
    var displayName: String
    var enabled: Bool
    var capabilities: ProviderCapabilitiesRecord
    var limits: ProviderLimitsRecord
    var health: String
    var roleScores: [String: ProviderRoleScoreRecord]
    var catalogRevision: String
}

struct ProviderCapabilitiesRecord: Codable, Equatable, Sendable {
    var text: Bool
    var code: Bool
    var visionInput: Bool
    var imageOutput: Bool
    var imageEdit: Bool
    var toolUse: Bool
    var structuredOutput: Bool
    var longContext: Bool
    var streaming: Bool
}

struct ProviderLimitsRecord: Codable, Equatable, Sendable {
    var maxInputTokens: UInt64?
    var maxOutputTokens: UInt64?
    var requestsPerMinute: UInt32?
    var tokensPerMinute: UInt64?
    var maxConcurrency: UInt32?
}

struct ProviderRoleScoreRecord: Codable, Equatable, Sendable {
    var score: Double
    var evidenceRefs: [String]
}

struct ProviderMutationReceiptRecord: Codable, Equatable, Sendable {
    var schema: String
    var providerId: String
    var mutation: String
    var revision: UInt64
    var secretRef: ProviderSecretRef?
    var secretValueExposed: Bool
    var occurredAtMs: UInt64
    var reasonCode: String
}

struct ProviderProbeReceiptRecord: Codable, Equatable, Sendable {
    var schema: String
    var providerId: String
    var endpointHostRedacted: String
    var httpCategory: String
    var latencyBucket: String
    var authAccepted: Bool
    var modelListAvailable: Bool
    var catalogCount: UInt32?
    var occurredAtMs: UInt64
    var reasonCode: String
    var diagnosticRef: String?
}
