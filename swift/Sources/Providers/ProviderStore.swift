import Foundation

@MainActor
final class ProviderStore: ObservableObject {
    @Published private(set) var connections: [ProviderConnectionViewModel] = []
    @Published private(set) var models: [ProviderModelViewModel] = []
    @Published private(set) var adapterCheckReport: ProviderAdapterCheckReport?
    @Published private(set) var syncState: ProviderSyncState = .idle
    @Published var selectedProviderID: String?
    @Published var showingAddProvider = false

    private let secretStore: ProviderSecretStoring
    private var service: ProviderRegistryService?

    init(
        secretStore: ProviderSecretStoring = KeychainSecretStore(),
        service: ProviderRegistryService? = nil
    ) {
        self.secretStore = secretStore
        self.service = service
    }

    func updateService(_ service: ProviderRegistryService) {
        self.service = service
    }

    var hasEligibleTextModel: Bool {
        !eligibleTextModels.isEmpty
    }

    var hasEligibleImageModel: Bool {
        !eligibleImageModels.isEmpty
    }

    var eligibleTextModels: [ProviderModelViewModel] {
        eligibleModels(requiring: .code)
    }

    func textModelSelection(pinning modelID: String) -> ModelSelection {
        ModelSelection(
            mode: .pinned,
            modelId: modelID,
            fallbackModelIds: fallbackTextModelIDs(excluding: modelID)
        )
    }

    func normalizedTextModelSelection(from selection: ModelSelection) -> ModelSelection {
        guard selection.mode == .pinned else { return selection }
        guard let rawModelID = selection.modelId?.trimmingCharacters(in: .whitespacesAndNewlines),
              !rawModelID.isEmpty
        else {
            return ModelSelection(mode: .auto, modelId: nil, fallbackModelIds: [])
        }
        if let model = model(id: rawModelID), modelIsSelectable(model, requiring: .code) {
            return textModelSelection(pinning: model.id)
        }
        if let model = preferredTextModel(forProviderAlias: rawModelID) {
            return textModelSelection(pinning: model.id)
        }
        return selection
    }

    func fallbackTextModelIDs(excluding modelID: String?) -> [String] {
        eligibleTextModels
            .filter { $0.id != modelID }
            .map(\.id)
    }

    var eligibleImageModels: [ProviderModelViewModel] {
        eligibleModels(requiring: .image)
    }

    var eligibleVisionModels: [ProviderModelViewModel] {
        eligibleModels(requiring: .vision)
    }

    func modelIsSelectable(
        _ model: ProviderModelViewModel,
        requiring capability: ProviderModelCapability
    ) -> Bool {
        guard let provider = connections.first(where: { $0.id == model.providerID }) else {
            return false
        }
        return modelIsSelectable(model, provider: provider, requiring: capability)
    }

    private func eligibleModels(requiring capability: ProviderModelCapability) -> [ProviderModelViewModel] {
        let providersByID = Dictionary(uniqueKeysWithValues: connections.map { ($0.id, $0) })
        return models
            .filter { model in
                guard let provider = providersByID[model.providerID] else { return false }
                return modelIsSelectable(model, provider: provider, requiring: capability)
            }
            .sorted { left, right in
                if left.displayName == right.displayName { return left.id < right.id }
                return left.displayName < right.displayName
            }
    }

    private func modelIsSelectable(
        _ model: ProviderModelViewModel,
        provider: ProviderConnectionViewModel,
        requiring capability: ProviderModelCapability
    ) -> Bool {
        guard provider.enabled, model.enabled, model.capabilities.contains(capability) else {
            return false
        }
        if provider.health == .healthy, model.health == .healthy {
            return true
        }
        let codexLbModelCanRun = model.health == .needsProbe || model.health == .healthy
        return provider.kind == .codexLB
            && !provider.circuitOpen
            && provider.health != .needsCredential
            && codexLbModelCanRun
    }

    private func preferredTextModel(forProviderAlias alias: String) -> ProviderModelViewModel? {
        let normalizedAlias = normalizedProviderAlias(alias)
        guard let provider = connections.first(where: { connection in
            [
                connection.id,
                connection.displayName,
                connection.kind.rawValue,
                connection.kind.displayLabel
            ].contains { normalizedProviderAlias($0) == normalizedAlias }
        }) else {
            return nil
        }
        return eligibleTextModels.first { $0.providerID == provider.id }
    }

    private func normalizedProviderAlias(_ value: String) -> String {
        value
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .replacingOccurrences(of: " ", with: "")
            .replacingOccurrences(of: "_", with: "")
            .replacingOccurrences(of: "-", with: "")
    }

    var enabledProviderCount: Int {
        connections.filter(\.enabled).count
    }

    var providerReadinessDetail: String {
        if let adapterDetail = ProviderAdapterReadiness.summary(for: adapterCheckReport) {
            return adapterDetail
        }
        if enabledProviderCount == 0 {
            return "Connect and enable a provider before starting a turn."
        }
        return "Enable a healthy code-capable model before starting a turn."
    }

    var adapterCheckReportGeneratedAtLabel: String? {
        guard let generatedAt = adapterCheckReport?.generatedAt else { return nil }
        return "unix \(generatedAt.unixSeconds)"
    }

    var adapterCheckReportSummaryDetail: String? {
        guard let report = adapterCheckReport else { return nil }
        let remoteProbe = report.remoteProbeOptIn ? "true" : "false"
        return "\(report.summary.reachable)/\(report.summary.total) reachable · attempted \(report.summary.attempted) · remote probe \(remoteProbe)"
    }

    var adapterCheckActionDetail: String? {
        ProviderAdapterReadiness.summary(for: adapterCheckReport)
    }

    var adapterCheckRemediationActions: [ProviderAdapterRemediationAction] {
        adapterCheckReport?.remediationActions ?? []
    }

    func refresh() async {
        guard let service else {
            syncState = .idle
            return
        }
        syncState = .syncing
        do {
            let state = try await service.registryState()
            applyRegistryState(state)
            if try await backfillSeededCodexLbModelsIfNeeded(state, service: service) {
                applyRegistryState(try await service.registryState())
            }
            syncState = .idle
        } catch {
            syncState = .failed(error.localizedDescription)
        }
    }

    @discardableResult
    func connect(_ draft: ProviderDraft, credential: SecureCredential) async throws -> String {
        syncState = .saving
        let draft = normalizedDraft(draft)
        guard endpointAllowed(draft.endpoint) else {
            syncState = .failed(ProviderStoreError.invalidEndpoint.localizedDescription)
            throw ProviderStoreError.invalidEndpoint
        }
        let providerID = "provider-\(UUID().uuidString.lowercased())"
        let service = "ai.opensks.provider.\(draft.kind.rawValue)"
        let version = try secretStore.saveOrReplace(
            service: service,
            account: providerID,
            credential: credential
        )
        let secretRef = ProviderSecretRef(
            store: "macos_keychain",
            service: service,
            account: providerID,
            version: version
        )
        let now = currentTimeMillis()
        let connection = ProviderConnectionViewModel(
            id: providerID,
            kind: draft.kind,
            displayName: draft.displayName.trimmingCharacters(in: .whitespacesAndNewlines),
            endpoint: draft.endpoint.trimmingCharacters(in: .whitespacesAndNewlines),
            enabled: draft.enabled,
            secretRef: secretRef,
            health: .needsProbe,
            enabledModelCount: 0,
            activeRequests: 0,
            maxConcurrentRequests: max(1, draft.maxConcurrentRequests),
            lastDiagnostic: "Credential saved to Keychain. Run connection test and model sync.",
            circuitOpen: false,
            lastCheckedAtMs: nil,
            diagnosticRef: nil,
            revision: 1
        )
        if let registry = self.service {
            do {
                _ = try await registry.upsertConnection(
                    connection.record(
                        createdAtMs: now,
                        updatedAtMs: now,
                        organizationRef: draft.organizationRef,
                        projectRef: draft.projectRef
                    ),
                    expectedRevision: nil
                )
                let records = seededModelRecords(providerID: providerID, kind: draft.kind)
                if !records.isEmpty {
                    _ = try await registry.syncModels(providerID: providerID, models: records)
                }
                await refresh()
            } catch {
                try? secretStore.delete(service: service, account: providerID)
                syncState = .failed(error.localizedDescription)
                throw error
            }
        } else {
            connections.append(connection)
            selectedProviderID = providerID
            syncLocalCatalog(providerID: providerID, kind: draft.kind)
        }
        syncState = .idle
        return providerID
    }

    @discardableResult
    func connectAndProbe(_ draft: ProviderDraft, credential: SecureCredential) async throws -> String {
        let providerID = try await connect(draft, credential: credential)
        try await probeProvider(providerID)
        return providerID
    }

    func setProviderEnabled(_ id: String, _ enabled: Bool) async throws {
        guard let index = connections.firstIndex(where: { $0.id == id }) else {
            throw ProviderStoreError.providerNotFound
        }
        if let service {
            _ = try await service.setProviderEnabled(
                id: id,
                enabled: enabled,
                expectedRevision: connections[index].revision
            )
            await refresh()
            return
        }
        connections[index].enabled = enabled
        connections[index].revision += 1
        if !enabled {
            for modelIndex in models.indices where models[modelIndex].providerID == id {
                models[modelIndex].health = .disabled
            }
        } else {
            for modelIndex in models.indices where models[modelIndex].providerID == id {
                models[modelIndex].health = .needsProbe
            }
        }
    }

    func probeProvider(_ id: String) async throws {
        guard connections.contains(where: { $0.id == id }) else {
            throw ProviderStoreError.providerNotFound
        }
        syncState = .probing
        if let service {
            do {
                _ = try await service.probeProvider(id: id)
                await refresh()
            } catch {
                syncState = .failed(error.localizedDescription)
                throw error
            }
            return
        }
        try await applySuccessfulProbe(providerID: id)
        syncState = .idle
    }

    func runAdapterCheck() async throws {
        guard let service else {
            syncState = .idle
            return
        }
        syncState = .probing
        do {
            adapterCheckReport = try await service.runAdapterCheck()
            await refresh()
        } catch {
            syncState = .failed(error.localizedDescription)
            throw error
        }
    }

    func setModelEnabled(_ id: String, _ enabled: Bool) async throws {
        guard let index = models.firstIndex(where: { $0.id == id }) else {
            throw ProviderStoreError.modelNotFound
        }
        if let service {
            _ = try await service.setModelEnabled(id: id, enabled: enabled)
            await refresh()
            return
        }
        models[index].enabled = enabled
        updateEnabledModelCount(for: models[index].providerID)
    }

    func deleteProvider(_ id: String) async throws {
        guard let connection = connections.first(where: { $0.id == id }) else {
            throw ProviderStoreError.providerNotFound
        }
        if let service {
            _ = try await service.deleteProvider(id: id, expectedRevision: connection.revision)
            try secretStore.delete(service: connection.secretRef.service, account: connection.secretRef.account)
            await refresh()
            return
        }
        try secretStore.delete(service: connection.secretRef.service, account: connection.secretRef.account)
        connections.removeAll { $0.id == id }
        models.removeAll { $0.providerID == id }
        if selectedProviderID == id {
            selectedProviderID = connections.first?.id
        }
    }

    func applySuccessfulProbe(providerID: String, diagnostic: String = "Connection test passed.") async throws {
        guard let providerIndex = connections.firstIndex(where: { $0.id == providerID }) else {
            throw ProviderStoreError.providerNotFound
        }
        connections[providerIndex].health = .healthy
        connections[providerIndex].lastDiagnostic = diagnostic
        connections[providerIndex].circuitOpen = false
        connections[providerIndex].lastCheckedAtMs = currentTimeMillis()
        connections[providerIndex].diagnosticRef = nil
        connections[providerIndex].revision += 1
        for modelIndex in models.indices where models[modelIndex].providerID == providerID {
            models[modelIndex].health = models[modelIndex].enabled ? .healthy : .disabled
        }
    }

    func model(id: String) -> ProviderModelViewModel? {
        models.first { $0.id == id }
    }

    func modelDisplayLabel(for id: String) -> String? {
        guard let model = model(id: id) else { return nil }
        return "\(providerDisplayName(for: model.providerID)) / \(model.displayName)"
    }

    func providerDisplayName(for id: String) -> String {
        connections.first { $0.id == id }?.displayName ?? id
    }

    func models(for providerID: String) -> [ProviderModelViewModel] {
        models.filter { $0.providerID == providerID }
    }

    func models(for providerID: String, matching query: String) -> [ProviderModelViewModel] {
        let providerModels = models(for: providerID)
        let normalizedQuery = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !normalizedQuery.isEmpty else { return providerModels }
        return providerModels.filter { $0.matchesModelSearch(normalizedQuery) }
    }

    private func syncLocalCatalog(providerID: String, kind: ProviderKind) {
        syncState = .syncing
        let seeded = seededLocalModels(providerID: providerID, kind: kind)
        models.removeAll { $0.providerID == providerID }
        models.append(contentsOf: seeded)
        updateEnabledModelCount(for: providerID)
        syncState = .idle
    }

    private func backfillSeededCodexLbModelsIfNeeded(
        _ state: ProviderRegistryState,
        service: ProviderRegistryService
    ) async throws -> Bool {
        let modelsByProvider = Dictionary(grouping: state.models, by: \.providerId)
        var backfilled = false
        for provider in state.providers where provider.kind == .codexLB {
            let existingIDs = Set(modelsByProvider[provider.id, default: []].map(\.id))
            let missing = seededModelRecords(providerID: provider.id, kind: provider.kind)
                .filter { !existingIDs.contains($0.id) }
            if !missing.isEmpty {
                _ = try await service.syncModels(providerID: provider.id, models: missing)
                backfilled = true
            }
        }
        return backfilled
    }

    private func applyRegistryState(_ state: ProviderRegistryState) {
        adapterCheckReport = state.adapterCheckReport
        let modelsByProvider = Dictionary(grouping: state.models, by: \.providerId)
        let latestProbeByProvider = Dictionary(uniqueKeysWithValues: state.latestProbes.map { ($0.providerId, $0) })
        let adapterChecksByName = ProviderAdapterReadiness.rowsByProviderName(state.adapterCheckReport)
        connections = state.providers.map { record in
            record.viewModel(
                enabledModelCount: modelsByProvider[record.id, default: []].filter(\.enabled).count,
                latestProbe: latestProbeByProvider[record.id],
                adapterCheck: adapterChecksByName[
                    ProviderAdapterReadiness.normalizedName(record.displayName)
                ] ?? adapterChecksByName[
                    ProviderAdapterReadiness.normalizedName(record.kind.displayLabel)
                ],
                adapterCheckGeneratedAt: state.adapterCheckReport?.generatedAt
            )
        }
        let providerEnabled = Dictionary(uniqueKeysWithValues: connections.map { ($0.id, $0.enabled) })
        models = state.models.map { $0.viewModel(providerEnabled: providerEnabled[$0.providerId] ?? false) }
        if let selectedProviderID, connections.contains(where: { $0.id == selectedProviderID }) {
            return
        }
        selectedProviderID = connections.first?.id
    }

    private func seededLocalModels(providerID: String, kind: ProviderKind) -> [ProviderModelViewModel] {
        switch kind {
        case .codexLB:
            return [
                seededCodeModel(
                    providerID: providerID,
                    remoteModelID: "auto-code",
                    displayName: "Default code model",
                    contextWindow: 128_000
                ),
                seededCodeModel(
                    providerID: providerID,
                    remoteModelID: "gpt-5.5",
                    displayName: "GPT-5.5",
                    contextWindow: 400_000
                ),
                seededCodeModel(
                    providerID: providerID,
                    remoteModelID: "gpt-5.4-mini",
                    displayName: "GPT-5.4 mini",
                    contextWindow: 400_000
                ),
                seededCodeModel(
                    providerID: providerID,
                    remoteModelID: "gpt-5.4-nano",
                    displayName: "GPT-5.4 nano",
                    contextWindow: 400_000
                ),
                seededImageModel(providerID: providerID)
            ]
        case .openRouter, .openAI, .openAICompatible, .localOpenAICompatible:
            return [
                seededCodeModel(
                    providerID: providerID,
                    remoteModelID: "auto-code",
                    displayName: "Auto code model",
                    contextWindow: 128_000
                ),
                seededImageModel(providerID: providerID)
            ]
        case .anthropicCompatible, .googleCompatible, .custom:
            return []
        }
    }

    private func seededCodeModel(
        providerID: String,
        remoteModelID: String,
        displayName: String,
        contextWindow: Int
    ) -> ProviderModelViewModel {
        ProviderModelViewModel(
            id: "\(providerID)/\(remoteModelID)",
            providerID: providerID,
            remoteModelID: remoteModelID,
            displayName: displayName,
            enabled: true,
            health: .needsProbe,
            capabilities: [.code, .tools, .longContext],
            contextWindow: contextWindow,
            priceSummary: nil
        )
    }

    private func seededImageModel(providerID: String) -> ProviderModelViewModel {
        ProviderModelViewModel(
            id: "\(providerID)/auto-image",
            providerID: providerID,
            remoteModelID: "auto-image",
            displayName: "Auto image model",
            enabled: true,
            health: .needsProbe,
            capabilities: [.vision, .image],
            contextWindow: nil,
            priceSummary: nil
        )
    }

    private func seededModelRecords(providerID: String, kind: ProviderKind) -> [ProviderModelRecord] {
        seededLocalModels(providerID: providerID, kind: kind).map { model in
            let imageOutput = model.capabilities.contains(.image)
            let roleScores = seededRoleScores(for: model.capabilities)
            return ProviderModelRecord(
                schema: "opensks.model-catalog-entry.v1",
                id: model.id,
                providerId: model.providerID,
                remoteModelId: model.remoteModelID,
                displayName: model.displayName,
                enabled: model.enabled,
                capabilities: ProviderCapabilitiesRecord(
                    text: !imageOutput,
                    code: model.capabilities.contains(.code),
                    visionInput: model.capabilities.contains(.vision),
                    imageOutput: imageOutput,
                    imageEdit: false,
                    toolUse: model.capabilities.contains(.tools),
                    structuredOutput: !imageOutput,
                    longContext: model.capabilities.contains(.longContext),
                    streaming: !imageOutput
                ),
                limits: ProviderLimitsRecord(
                    maxInputTokens: model.contextWindow.map(UInt64.init),
                    maxOutputTokens: nil,
                    requestsPerMinute: nil,
                    tokensPerMinute: nil,
                    maxConcurrency: nil
                ),
                health: "unknown",
                roleScores: roleScores,
                catalogRevision: "local-seed-v1"
            )
        }
    }

    private func seededRoleScores(
        for capabilities: Set<ProviderModelCapability>
    ) -> [String: ProviderRoleScoreRecord] {
        var scores: [String: ProviderRoleScoreRecord] = [:]
        if capabilities.contains(.code) {
            scores["code"] = ProviderRoleScoreRecord(score: 0.5, evidenceRefs: ["local_seed_requires_probe"])
        }
        if capabilities.contains(.image) {
            scores["image"] = ProviderRoleScoreRecord(score: 0.5, evidenceRefs: ["local_seed_requires_probe"])
        }
        if capabilities.contains(.vision) {
            scores["vision"] = ProviderRoleScoreRecord(score: 0.5, evidenceRefs: ["local_seed_requires_probe"])
        }
        return scores
    }

    private func updateEnabledModelCount(for providerID: String) {
        guard let index = connections.firstIndex(where: { $0.id == providerID }) else { return }
        connections[index].enabledModelCount = models.filter {
            $0.providerID == providerID && $0.enabled
        }.count
    }

    private func endpointAllowed(_ endpoint: String) -> Bool {
        let trimmed = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        if trimmed.lowercased().hasPrefix("https://") { return true }
        return trimmed.lowercased().hasPrefix("http://127.0.0.1")
            || trimmed.lowercased().hasPrefix("http://localhost")
    }

    private func currentTimeMillis() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }

    private func normalizedDraft(_ draft: ProviderDraft) -> ProviderDraft {
        var normalized = draft
        normalized.endpoint = draft.resolvedEndpoint
        return normalized
    }
}

private extension ProviderConnectionViewModel {
    func record(
        createdAtMs: UInt64,
        updatedAtMs: UInt64,
        organizationRef: String,
        projectRef: String
    ) -> ProviderConnectionRecord {
        ProviderConnectionRecord(
            schema: "opensks.provider-connection.v1",
            id: id,
            kind: kind,
            displayName: displayName,
            enabled: enabled,
            endpoint: ProviderEndpointRecord(
                baseUrl: endpoint,
                allowInsecureHttp: endpoint.lowercased().hasPrefix("http://127.0.0.1")
                    || endpoint.lowercased().hasPrefix("http://localhost")
            ),
            auth: secretRef,
            organizationRef: organizationRef.nilIfBlank,
            projectRef: projectRef.nilIfBlank,
            health: ProviderHealthSnapshotRecord(
                state: "unknown",
                circuitOpen: false,
                checkedAtMs: nil,
                reasonCode: "not_probed",
                diagnosticRef: nil
            ),
            concurrency: ProviderConcurrencyRecord(
                maxConcurrentRequests: UInt32(maxConcurrentRequests),
                requestsPerMinute: nil,
                tokensPerMinute: nil
            ),
            createdAtMs: createdAtMs,
            updatedAtMs: updatedAtMs,
            revision: revision
        )
    }
}

private extension String {
    var nilIfBlank: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

private extension ProviderModelViewModel {
    func matchesModelSearch(_ query: String) -> Bool {
        id.lowercased().contains(query)
            || remoteModelID.lowercased().contains(query)
            || displayName.lowercased().contains(query)
            || health.label.lowercased().contains(query)
            || capabilities.contains { $0.rawValue.lowercased().contains(query) }
    }
}

private extension ProviderConnectionRecord {
    func viewModel(
        enabledModelCount: Int,
        latestProbe: ProviderProbeReceiptRecord?,
        adapterCheck: ProviderAdapterCheckRow?,
        adapterCheckGeneratedAt: ProviderAdapterCheckGeneratedAt?
    ) -> ProviderConnectionViewModel {
        ProviderConnectionViewModel(
            id: id,
            kind: kind,
            displayName: displayName,
            endpoint: endpoint.baseUrl,
            enabled: enabled,
            secretRef: auth,
            health: ProviderHealthState.providerState(
                health.state,
                reasonCode: health.reasonCode,
                enabled: enabled
            ),
            enabledModelCount: enabledModelCount,
            activeRequests: 0,
            maxConcurrentRequests: Int(concurrency.maxConcurrentRequests),
            lastDiagnostic: latestProbe?.reasonCode ?? health.reasonCode,
            circuitOpen: health.circuitOpen || health.state == "open_circuit",
            lastCheckedAtMs: health.checkedAtMs ?? latestProbe?.occurredAtMs,
            diagnosticRef: health.diagnosticRef ?? latestProbe?.diagnosticRef,
            adapterDiagnostic: ProviderAdapterReadiness.diagnostic(for: adapterCheck),
            adapterBlockers: ProviderAdapterReadiness.safeBlockers(adapterCheck?.blockers ?? []),
            adapterCheckGeneratedAt: adapterCheck == nil ? nil : adapterCheckGeneratedAt,
            adapterCheckDetail: ProviderAdapterReadiness.detail(for: adapterCheck),
            revision: revision
        )
    }
}

private enum ProviderAdapterReadiness {
    static func normalizedName(_ name: String) -> String {
        name
            .lowercased()
            .replacingOccurrences(of: " ", with: "")
            .replacingOccurrences(of: "_", with: "")
            .replacingOccurrences(of: "-", with: "")
    }

    static func rowsByProviderName(_ report: ProviderAdapterCheckReport?) -> [String: ProviderAdapterCheckRow] {
        var rows: [String: ProviderAdapterCheckRow] = [:]
        for row in report?.adapters ?? [] {
            rows[normalizedName(row.name)] = row
        }
        return rows
    }

    static func summary(for report: ProviderAdapterCheckReport?) -> String? {
        guard let report else { return nil }
        if report.secretValueExposed {
            return "Provider adapter check raised a secret-leak guard; review the local report before probing."
        }
        if let blockerSummary = summary(for: report.blockers) {
            return blockerSummary
        }
        if report.summary.reachable < report.summary.total {
            return "Provider adapter check has not confirmed every required provider models endpoint."
        }
        return nil
    }

    static func diagnostic(for row: ProviderAdapterCheckRow?) -> String? {
        guard let row else { return nil }
        if row.secretValueExposed {
            return "Adapter check raised a secret-leak guard for this provider."
        }
        if let blockerSummary = summary(for: row.blockers) {
            return blockerSummary
        }
        switch row.status {
        case "adapter_models_endpoint_reachable":
            return "Adapter check reached the models endpoint."
        case "adapter_auth_failed":
            return "Adapter check reached the endpoint but authentication was rejected."
        case "not_configured":
            return "Configure this provider credential before probing."
        default:
            if row.attempted {
                return "Adapter check attempted this provider and needs review."
            }
            return nil
        }
    }

    static func detail(for row: ProviderAdapterCheckRow?) -> String? {
        guard let row else { return nil }
        let credential = safeDiagnostic(row.credentialSource)
        let transport = safeDiagnostic(row.transport ?? "unknown")
        let http = safeDiagnostic(row.httpCode ?? "none")
        let duration = row.durationMs.map { "\($0)ms" } ?? "unknown"
        return "credential \(credential) · transport \(transport) · http \(http) · duration \(duration)"
    }

    static func safeBlockers(_ blockers: [String]) -> [String] {
        blockers.map { blocker in
            switch blocker {
            case "set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1",
                "configure_OPENROUTER_API_KEY_credential",
                "configure_OPENAI_API_KEY_credential",
                "replace_OPENROUTER_API_KEY_credential",
                "replace_OPENAI_API_KEY_credential",
                "resolve_OpenRouter_models_endpoint",
                "resolve_OpenAI_models_endpoint",
                "resolve_OpenRouter_adapter_check_error",
                "resolve_OpenAI_adapter_check_error":
                return message(for: blocker)
            default:
                return message(for: "redacted_provider_check_blocker")
            }
        }
    }

    private static func summary(for blockers: [String]) -> String? {
        guard !blockers.isEmpty else { return nil }
        let messages = blockers.prefix(3).map(message(for:))
        let remainingCount = blockers.count - messages.count
        let tail = remainingCount > 0
            ? " \(remainingCount) more provider check blocker\(remainingCount == 1 ? "" : "s") remain."
            : ""
        return messages.joined(separator: " ") + tail
    }

    private static func message(for blocker: String) -> String {
        switch blocker {
        case "set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1":
            return "Set OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1 before running live provider checks."
        case "configure_OPENROUTER_API_KEY_credential":
            return "Add an OpenRouter API key credential in Provider Center or Keychain."
        case "configure_OPENAI_API_KEY_credential":
            return "Add an OpenAI API key credential in Provider Center or Keychain."
        case "replace_OPENROUTER_API_KEY_credential":
            return "Replace the OpenRouter API key credential; authentication was rejected."
        case "replace_OPENAI_API_KEY_credential":
            return "Replace the OpenAI API key credential; authentication was rejected."
        case "resolve_OpenRouter_models_endpoint":
            return "OpenRouter models endpoint did not return a reachable response."
        case "resolve_OpenAI_models_endpoint":
            return "OpenAI models endpoint did not return a reachable response."
        case "resolve_OpenRouter_adapter_check_error":
            return "OpenRouter adapter check failed before reachability could be confirmed."
        case "resolve_OpenAI_adapter_check_error":
            return "OpenAI adapter check failed before reachability could be confirmed."
        case "redacted_provider_check_blocker":
            return "Provider check has a redacted blocker in the local report."
        default:
            return "Provider check has a redacted blocker in the local report."
        }
    }

    private static func safeDiagnostic(_ value: String) -> String {
        if value.hasPrefix("provider_registry_keychain:") {
            return "provider_registry_keychain"
        }
        let lower = value.lowercased()
        if lower.contains("bearer ") || lower.contains("sk-") || lower.contains("token=") || lower.contains("key=") {
            return "[redacted]"
        }
        return value
    }
}

private extension ProviderModelRecord {
    func viewModel(providerEnabled: Bool) -> ProviderModelViewModel {
        ProviderModelViewModel(
            id: id,
            providerID: providerId,
            remoteModelID: remoteModelId,
            displayName: displayName,
            enabled: enabled,
            health: ProviderHealthState.modelState(health, enabled: enabled && providerEnabled),
            capabilities: capabilities.viewModelCapabilities,
            contextWindow: limits.maxInputTokens.map(Int.init),
            priceSummary: nil
        )
    }
}

private extension ProviderCapabilitiesRecord {
    var viewModelCapabilities: Set<ProviderModelCapability> {
        var output: Set<ProviderModelCapability> = []
        if code { output.insert(.code) }
        if toolUse { output.insert(.tools) }
        if visionInput { output.insert(.vision) }
        if imageOutput { output.insert(.image) }
        if longContext { output.insert(.longContext) }
        return output
    }
}

private extension ProviderHealthState {
    static func providerState(_ state: String, reasonCode: String, enabled: Bool) -> ProviderHealthState {
        guard enabled else { return .disabled }
        if reasonCode.contains("credential") || reasonCode.contains("auth_rejected") {
            return .needsCredential
        }
        return contractState(state)
    }

    static func modelState(_ state: String, enabled: Bool) -> ProviderHealthState {
        guard enabled else { return .disabled }
        return contractState(state)
    }

    static func contractState(_ state: String) -> ProviderHealthState {
        switch state {
        case "healthy": return .healthy
        case "degraded", "unavailable", "open_circuit": return .degraded
        case "unknown": return .needsProbe
        default: return .unknown
        }
    }
}

enum ProviderStoreError: LocalizedError, Equatable {
    case invalidEndpoint
    case providerNotFound
    case modelNotFound

    var errorDescription: String? {
        switch self {
        case .invalidEndpoint: return "Endpoint must be HTTPS or a local HTTP endpoint."
        case .providerNotFound: return "Provider was not found."
        case .modelNotFound: return "Model was not found."
        }
    }
}
