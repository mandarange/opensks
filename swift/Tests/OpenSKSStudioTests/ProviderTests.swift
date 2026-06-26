import SwiftUI
import XCTest

@testable import OpenSKSStudio

@MainActor
final class ProviderTests: XCTestCase {
    func testProviderConnectionFlightGuardRejectsConcurrentOperations() async throws {
        let guardrail = ProviderConnectionFlightGuard()
        let started = expectation(description: "first operation started")
        var releaseFirst: CheckedContinuation<Void, Never>?

        let first = Task {
            try await guardrail.run {
                started.fulfill()
                await withCheckedContinuation { continuation in
                    releaseFirst = continuation
                }
            }
        }

        await fulfillment(of: [started], timeout: 1)
        XCTAssertTrue(guardrail.inFlight)
        let second = try await guardrail.run {
            XCTFail("second operation should not start while the wizard is already saving or testing")
        }
        XCTAssertFalse(second)

        releaseFirst?.resume()
        let firstResult = try await first.value
        XCTAssertTrue(firstResult)
        XCTAssertFalse(guardrail.inFlight)
    }

    func testProviderStoreRefreshLoadsRegistryStateAndEligibility() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "healthy")]
        service.state.models = [Self.modelRecord(health: "healthy")]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        XCTAssertEqual(store.connections.map(\.id), ["provider-1"])
        XCTAssertEqual(store.models.map(\.id), ["provider-1/code-model"])
        XCTAssertTrue(store.hasEligibleTextModel)
    }

    func testLiveProviderRegistryServiceLoadsAdapterCheckFromAppDataFallback() async throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent("opensks-provider-service-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: workspace) }

        let cli = workspace.appendingPathComponent("fake-opensks")
        let script = """
        #!/bin/sh
        if [ "$1" = "provider" ] && [ "$2" = "registry-list" ]; then
        cat <<'JSON'
        {
          "schema": "opensks.provider-registry-state.v1",
          "providers": [
            {
              "schema": "opensks.provider-connection.v1",
              "id": "provider-1",
              "kind": "codex_lb",
              "display_name": "codex-lb",
              "enabled": true,
              "endpoint": {"base_url": "https://codex.hyper-lab.xyz/backend-api/codex", "allow_insecure_http": false},
              "auth": {"schema": "opensks.secret-ref.v1", "store": "macos_keychain", "service": "ai.opensks.provider.codex_lb", "account": "provider-1", "version": 1},
              "health": {"state": "unknown", "circuit_open": false, "reason_code": "not_probed"},
              "concurrency": {"max_concurrent_requests": 16},
              "created_at_ms": 1,
              "updated_at_ms": 1,
              "revision": 1
            }
          ],
          "models": [],
          "latest_probes": []
        }
        JSON
        elif [ "$1" = "app-data" ]; then
        cat <<'JSON'
        {
          "provider_adapter_check": {
            "schema": "opensks.provider-adapter-check.v1",
            "generated_at": {"unix_seconds": 1782400000, "nanos": 0},
            "remote_probe_opt_in": false,
            "secret_value_exposed": false,
            "summary": {"total": 2, "attempted": 0, "reachable": 0},
            "blockers": ["set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1"],
            "remediation_actions": [
              {"blocker": "set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1", "action": "Set OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1.", "scope": "operator_environment"}
            ],
            "adapters": [
              {
                "name": "OpenRouter",
                "configured": false,
                "attempted": false,
                "status": "not_configured",
                "blockers": ["configure_OPENROUTER_API_KEY_credential"],
                "credential_source": "none",
                "endpoint": "https://openrouter.ai/api/v1/models",
                "http_code": null,
                "duration_ms": 0,
                "transport": "native_reqwest_blocking_http",
                "secret_value_exposed": false,
                "stderr": ""
              }
            ]
          }
        }
        JSON
        else
          echo "unexpected args: $@" >&2
          exit 64
        fi
        """
        try script.write(to: cli, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: cli.path)

        let service = LiveProviderRegistryService(cli: cli, workspace: workspace)
        let state = try await service.registryState()

        XCTAssertEqual(state.providers.map(\.displayName), ["codex-lb"])
        XCTAssertEqual(state.adapterCheckReport?.generatedAt?.unixSeconds, 1_782_400_000)
        XCTAssertEqual(state.adapterCheckReport?.summary.total, 2)
        XCTAssertEqual(state.adapterCheckReport?.remediationActions.first?.scope, "operator_environment")
        XCTAssertEqual(state.adapterCheckReport?.adapters.first?.transport, "native_reqwest_blocking_http")
    }

    func testTextModelSelectionCarriesOrderedHealthyFallbacks() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [
            Self.providerRecord(health: "healthy", id: "provider-b"),
            Self.providerRecord(health: "healthy", id: "provider-a"),
            Self.providerRecord(health: "degraded", id: "provider-c")
        ]
        service.state.models = [
            Self.modelRecord(health: "healthy", providerID: "provider-b"),
            Self.modelRecord(health: "healthy", providerID: "provider-a"),
            Self.modelRecord(health: "healthy", providerID: "provider-c"),
            Self.imageModelRecord(health: "healthy", providerID: "provider-a")
        ]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        let selection = store.textModelSelection(pinning: "provider-b/code-model")
        XCTAssertEqual(selection.mode, .pinned)
        XCTAssertEqual(selection.modelId, "provider-b/code-model")
        XCTAssertEqual(selection.fallbackModelIds, ["provider-a/code-model"])
    }

    func testProviderAdapterCheckReportDecodesOldReportsWithoutBlockers() throws {
        let json = """
        {
          "schema": "opensks.provider-adapter-check.v1",
          "remote_probe_opt_in": false,
          "secret_value_exposed": false,
          "summary": {"total":2,"attempted":0,"reachable":0},
          "adapters": [
            {
              "name":"OpenRouter",
              "configured":false,
              "attempted":false,
              "status":"not_configured",
              "credential_source":"none",
              "endpoint":"https://openrouter.ai/api/v1/models",
              "http_code":null,
              "secret_value_exposed":false,
              "stderr":""
            }
          ]
        }
        """

        let report = try JSONDecoder.opensks.decode(ProviderAdapterCheckReport.self, from: Data(json.utf8))

        XCTAssertEqual(report.blockers, [])
        XCTAssertEqual(report.remediationActions, [])
        XCTAssertNil(report.generatedAt)
        XCTAssertEqual(report.adapters.first?.blockers, [])
        XCTAssertNil(report.adapters.first?.durationMs)
        XCTAssertNil(report.adapters.first?.transport)
        XCTAssertEqual(report.adapters.first?.stderr, "")
    }

    func testProviderAdapterCheckReportDecodesRemediationActions() throws {
        let json = """
        {
          "schema": "opensks.provider-adapter-check.v1",
          "generated_at": {"unix_seconds": 1782400000, "nanos": 42},
          "remote_probe_opt_in": false,
          "secret_value_exposed": false,
          "summary": {"total":2,"attempted":0,"reachable":0},
          "blockers": ["configure_OPENROUTER_API_KEY_credential"],
          "remediation_actions": [
            {
              "blocker": "configure_OPENROUTER_API_KEY_credential",
              "action": "Add an OpenRouter API key credential through Provider Center or the configured secret store.",
              "scope": "provider_credential"
            }
          ],
          "adapters": [
            {
              "name": "OpenRouter",
              "configured": true,
              "attempted": true,
              "status": "adapter_models_endpoint_reachable",
              "blockers": [],
              "credential_source": "macos_keychain",
              "endpoint": "https://openrouter.ai/api/v1/models",
              "http_code": 200,
              "duration_ms": 42,
              "transport": "native_reqwest_blocking_http",
              "secret_value_exposed": false,
              "stderr": ""
            }
          ]
        }
        """

        let report = try JSONDecoder.opensks.decode(ProviderAdapterCheckReport.self, from: Data(json.utf8))

        XCTAssertEqual(report.generatedAt?.unixSeconds, 1_782_400_000)
        XCTAssertEqual(report.generatedAt?.nanos, 42)
        XCTAssertEqual(report.remediationActions.count, 1)
        XCTAssertEqual(report.remediationActions.first?.blocker, "configure_OPENROUTER_API_KEY_credential")
        XCTAssertEqual(
            report.remediationActions.first?.action,
            "Add an OpenRouter API key credential through Provider Center or the configured secret store."
        )
        XCTAssertEqual(report.remediationActions.first?.scope, "provider_credential")
        XCTAssertEqual(report.adapters.first?.httpCode, "200")
        XCTAssertEqual(report.adapters.first?.durationMs, 42)
        XCTAssertEqual(report.adapters.first?.transport, "native_reqwest_blocking_http")
    }

    func testProviderStoreSurfacesAdapterCheckReadinessWithoutSecretValues() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "unknown")]
        service.state.adapterCheckReport = ProviderAdapterCheckReport(
            schema: "opensks.provider-adapter-check.v1",
            generatedAt: ProviderAdapterCheckGeneratedAt(unixSeconds: 1_782_400_000, nanos: 0),
            remoteProbeOptIn: false,
            secretValueExposed: false,
            summary: ProviderAdapterCheckSummary(total: 2, attempted: 0, reachable: 0),
            blockers: [
                "set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1",
                "configure_OPENROUTER_API_KEY_credential",
                "Bearer sk-test-secret"
            ],
            adapters: [
                ProviderAdapterCheckRow(
                    name: "OpenRouter",
                    configured: false,
                    attempted: false,
                    status: "not_configured",
                    blockers: [
                        "configure_OPENROUTER_API_KEY_credential",
                        "Bearer sk-test-secret"
                    ],
                    credentialSource: "none",
                    endpoint: "https://openrouter.ai/api/v1/models",
                    httpCode: nil,
                    secretValueExposed: false,
                    durationMs: 0,
                    transport: "native_reqwest_blocking_http"
                )
            ]
        )
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        let provider = try XCTUnwrap(store.connections.first)
        XCTAssertEqual(store.adapterCheckReportGeneratedAtLabel, "unix 1782400000")
        XCTAssertEqual(
            store.adapterCheckReportSummaryDetail,
            "0/2 reachable · attempted 0 · remote probe false"
        )
        XCTAssertEqual(provider.adapterCheckGeneratedAtLabel, "unix 1782400000")
        XCTAssertEqual(
            provider.adapterCheckDetail,
            "credential none · transport native_reqwest_blocking_http · http none · duration 0ms"
        )
        XCTAssertEqual(
            provider.adapterBlockers,
            [
                "Add an OpenRouter API key credential in Provider Center or Keychain.",
                "Provider check has a redacted blocker in the local report."
            ]
        )
        XCTAssertTrue(provider.adapterDiagnostic?.contains("OpenRouter API key") == true)
        XCTAssertFalse(provider.adapterDiagnostic?.contains("sk-test-secret") == true)
        XCTAssertTrue(store.providerReadinessDetail.contains("OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1"))
        XCTAssertFalse(store.providerReadinessDetail.contains("sk-test-secret"))
        XCTAssertFalse(provider.adapterBlockers.contains { $0.contains("sk-test-secret") })
        XCTAssertFalse(provider.adapterBlockers.contains("configure_OPENROUTER_API_KEY_credential"))
        XCTAssertFalse(provider.adapterCheckDetail?.contains("sk-test-secret") == true)
    }

    func testProviderStoreSurfacesHealthCircuitAndDiagnosticReference() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [
            Self.providerRecord(
                health: "degraded",
                circuitOpen: true,
                checkedAtMs: 1_782_351_000_000,
                reasonCode: "open_circuit",
                diagnosticRef: "artifact://provider/provider-1/probe.json"
            )
        ]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        let provider = try XCTUnwrap(store.connections.first)
        XCTAssertEqual(provider.health, .degraded)
        XCTAssertTrue(provider.circuitOpen)
        XCTAssertEqual(provider.statusLabel, "Circuit open")
        XCTAssertEqual(provider.circuitLabel, "Open")
        XCTAssertEqual(provider.lastCheckedAtMs, 1_782_351_000_000)
        XCTAssertNotEqual(provider.lastCheckedLabel, "Not checked")
        XCTAssertEqual(provider.diagnosticRef, "artifact://provider/provider-1/probe.json")
        XCTAssertEqual(provider.diagnosticReferenceLabel, "artifact://provider/provider-1/probe.json")

        let view = ProviderCenterView(store: store)
            .frame(width: 360, height: 640)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "provider center renders circuit and diagnostic reference in narrow width")
    }

    func testProviderCenterRendersAdapterDiagnostic() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "unknown")]
        service.state.adapterCheckReport = ProviderAdapterCheckReport(
            schema: "opensks.provider-adapter-check.v1",
            generatedAt: ProviderAdapterCheckGeneratedAt(unixSeconds: 1_782_400_000, nanos: 0),
            remoteProbeOptIn: false,
            secretValueExposed: false,
            summary: ProviderAdapterCheckSummary(total: 2, attempted: 0, reachable: 0),
            blockers: ["set_OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE_1"],
            adapters: [
                ProviderAdapterCheckRow(
                    name: "OpenRouter",
                    configured: false,
                    attempted: false,
                    status: "not_configured",
                    blockers: ["configure_OPENROUTER_API_KEY_credential"],
                    credentialSource: "none",
                    endpoint: "https://openrouter.ai/api/v1/models",
                    httpCode: nil,
                    secretValueExposed: false,
                    durationMs: 0,
                    transport: "native_reqwest_blocking_http"
                )
            ]
        )
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)
        await store.refresh()

        let provider = try XCTUnwrap(store.connections.first)
        XCTAssertEqual(store.adapterCheckReportGeneratedAtLabel, "unix 1782400000")
        XCTAssertEqual(
            store.adapterCheckReportSummaryDetail,
            "0/2 reachable · attempted 0 · remote probe false"
        )
        XCTAssertNotNil(provider.adapterDiagnostic)
        XCTAssertEqual(provider.adapterCheckGeneratedAtLabel, "unix 1782400000")
        XCTAssertEqual(
            provider.adapterCheckDetail,
            "credential none · transport native_reqwest_blocking_http · http none · duration 0ms"
        )
        let view = ProviderCenterView(store: store)
            .frame(width: 720, height: 560)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        XCTAssertNotNil(renderer.nsImage, "provider center renders with adapter diagnostic")
    }

    func testProviderComponentSurfacesRenderIndependently() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "healthy")]
        service.state.models = [
            Self.modelRecord(health: "healthy"),
            Self.imageModelRecord(health: "healthy"),
            Self.visionModelRecord(health: "healthy")
        ]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)
        await store.refresh()
        let provider = try XCTUnwrap(store.connections.first)

        let catalog = ProviderModelCatalogView(
            store: store,
            provider: provider,
            modelSearchText: .constant("code")
        )
        .frame(width: 520, height: 240)
        let diagnostics = ProviderDiagnosticsView(store: store, provider: provider)
            .frame(width: 520, height: 240)
        let textPicker = ModelPicker(
            providers: store,
            kind: .text,
            selectedModelID: "provider-1/code-model",
            autoSelected: false,
            chipText: "Code Model",
            onSelectAuto: {},
            onSelectModel: { _ in }
        )
        .frame(width: 180, height: 40)
        let secureField = SecureCredentialField(credential: .constant("secret-value"))
            .frame(width: 360, height: 80)

        XCTAssertNotNil(ImageRenderer(content: catalog).nsImage)
        XCTAssertNotNil(ImageRenderer(content: diagnostics).nsImage)
        XCTAssertNotNil(ImageRenderer(content: textPicker).nsImage)
        XCTAssertNotNil(ImageRenderer(content: secureField).nsImage)
    }

    func testProviderStoreConnectPersistsThroughRegistryServiceWithoutSecretValue() async throws {
        let secrets = InMemoryProviderSecretStore()
        let service = RecordingProviderRegistryService()
        let store = ProviderStore(secretStore: secrets, service: service)

        try await store.connect(
            ProviderDraft(
                kind: .openRouter,
                displayName: "OpenRouter",
                endpoint: "https://openrouter.ai/api/v1",
                organizationRef: "",
                projectRef: "",
                enabled: true,
                maxConcurrentRequests: 3
            ),
            credential: SecureCredential(value: "sk-live-secret-never-persist")
        )

        let saved = try XCTUnwrap(service.upsertedConnection)
        XCTAssertEqual(saved.auth.schema, "opensks.secret-ref.v1")
        XCTAssertEqual(saved.auth.store, "macos_keychain")
        XCTAssertTrue(secrets.contains(service: saved.auth.service, account: saved.auth.account))
        let encoded = String(decoding: try JSONEncoder.opensks.encode(saved), as: UTF8.self)
        XCTAssertTrue(encoded.contains("\"schema\":\"opensks.secret-ref.v1\""))
        XCTAssertFalse(String(describing: saved).contains("sk-live-secret-never-persist"))
        XCTAssertEqual(service.syncedModels.count, 2)
        XCTAssertEqual(store.connections.count, 1)
        XCTAssertFalse(store.hasEligibleTextModel, "seeded registry models remain unavailable until a successful probe")
        XCTAssertFalse(store.hasEligibleImageModel, "seeded image models remain unavailable until a successful probe")
    }

    func testProviderStoreConnectAndProbeUsesSavedProviderAndPersistsScope() async throws {
        let secrets = InMemoryProviderSecretStore()
        let service = RecordingProviderRegistryService()
        let store = ProviderStore(secretStore: secrets, service: service)

        let providerID = try await store.connectAndProbe(
            ProviderDraft(
                kind: .openRouter,
                displayName: "OpenRouter",
                endpoint: "https://openrouter.ai/api/v1",
                organizationRef: " org-live ",
                projectRef: " project-live ",
                enabled: true,
                maxConcurrentRequests: 3
            ),
            credential: SecureCredential(value: "sk-live-secret-never-persist")
        )

        let saved = try XCTUnwrap(service.upsertedConnection)
        XCTAssertEqual(saved.organizationRef, "org-live")
        XCTAssertEqual(saved.projectRef, "project-live")
        XCTAssertEqual(service.probedProviderIDs, [providerID])
        XCTAssertEqual(store.connections.first?.id, providerID)
        XCTAssertTrue(secrets.contains(service: saved.auth.service, account: saved.auth.account))
        XCTAssertFalse(String(describing: saved).contains("sk-live-secret-never-persist"))
        XCTAssertTrue(store.hasEligibleTextModel)
        XCTAssertTrue(store.hasEligibleImageModel)
    }

    func testProviderStoreResolvesCodexLbDomainToBackendApiCodexEndpoint() async throws {
        let secrets = InMemoryProviderSecretStore()
        let service = RecordingProviderRegistryService()
        let store = ProviderStore(secretStore: secrets, service: service)

        try await store.connect(
            ProviderDraft(
                kind: .codexLB,
                displayName: "codex-lb",
                codexLbDomain: "codex-lb.example.com",
                enabled: true,
                maxConcurrentRequests: 2
            ),
            credential: SecureCredential(value: "sk-live-secret-never-persist")
        )

        let provider = try XCTUnwrap(store.connections.first)
        XCTAssertEqual(provider.kind, .codexLB)
        XCTAssertEqual(provider.endpoint, "https://codex-lb.example.com/backend-api/codex")
        let saved = try XCTUnwrap(service.upsertedConnection)
        XCTAssertEqual(saved.kind, .codexLB)
        XCTAssertEqual(saved.endpoint.baseUrl, "https://codex-lb.example.com/backend-api/codex")
        XCTAssertEqual(saved.auth.schema, "opensks.secret-ref.v1")
        let encoded = String(decoding: try JSONEncoder.opensks.encode(saved), as: UTF8.self)
        XCTAssertTrue(encoded.contains("\"kind\":\"codex_lb\""))
        XCTAssertTrue(encoded.contains("\"schema\":\"opensks.secret-ref.v1\""))
        XCTAssertEqual(store.models(for: provider.id).count, 2)
        XCTAssertTrue(store.models(for: provider.id).allSatisfy { $0.health == .needsProbe })
        XCTAssertTrue(store.hasEligibleTextModel)
        XCTAssertTrue(store.hasEligibleImageModel)
        XCTAssertEqual(store.eligibleTextModels.map(\.id), ["\(provider.id)/auto-code"])
        XCTAssertEqual(store.eligibleImageModels.map(\.id), ["\(provider.id)/auto-image"])
    }

    func testProviderStoreMakesSavedCodexLbRegistryModelsSelectableBeforeProbe() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [
            Self.providerRecord(
                kind: .codexLB,
                health: "unknown",
                displayName: "codex-lb",
                endpoint: "https://codex-lb.example.com/backend-api/codex"
            )
        ]
        service.state.models = [
            Self.modelRecord(health: "unknown"),
            Self.imageModelRecord(health: "unknown")
        ]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        XCTAssertEqual(store.connections.first?.health, .needsProbe)
        XCTAssertTrue(store.hasEligibleTextModel)
        XCTAssertTrue(store.hasEligibleImageModel)
        XCTAssertEqual(store.eligibleTextModels.map(\.id), ["provider-1/code-model"])
        XCTAssertEqual(store.eligibleImageModels.map(\.id), ["provider-1/image-model"])
    }

    func testProviderSecretRefDefaultsSchemaWhenDecodingOlderRecords() throws {
        let json = """
        {
          "store": "macos_keychain",
          "service": "ai.opensks.provider.open_router",
          "account": "provider-1",
          "version": 1
        }
        """

        let secretRef = try JSONDecoder.opensks.decode(ProviderSecretRef.self, from: Data(json.utf8))

        XCTAssertEqual(secretRef.schema, "opensks.secret-ref.v1")
        XCTAssertEqual(secretRef.store, "macos_keychain")
    }

    func testProviderStoreSavesCredentialAsSecretRefOnlyAndSeedsModelAwaitingProbe() async throws {
        let secrets = InMemoryProviderSecretStore()
        let store = ProviderStore(secretStore: secrets)
        let draft = ProviderDraft(
            kind: .openRouter,
            displayName: "OpenRouter",
            endpoint: "https://openrouter.ai/api/v1",
            organizationRef: "",
            projectRef: "",
            enabled: true,
            maxConcurrentRequests: 4
        )

        try await store.connect(draft, credential: SecureCredential(value: "sk-test-secret-value"))

        XCTAssertEqual(store.connections.count, 1)
        let provider = try XCTUnwrap(store.connections.first)
        XCTAssertEqual(provider.secretRef.store, "macos_keychain")
        XCTAssertTrue(secrets.contains(service: provider.secretRef.service, account: provider.secretRef.account))
        XCTAssertFalse(provider.lastDiagnostic.contains("sk-test-secret-value"))
        XCTAssertEqual(provider.health, .needsProbe)
        XCTAssertFalse(store.hasEligibleTextModel)
        XCTAssertFalse(store.hasEligibleImageModel)
        XCTAssertEqual(store.models(for: provider.id).count, 2)
        XCTAssertTrue(store.models(for: provider.id).allSatisfy { $0.health == .needsProbe })
    }

    func testProviderProbeSyncsLiveModelsAndUnlocksEligibility() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "unknown")]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()
        XCTAssertFalse(store.hasEligibleTextModel)

        try await store.probeProvider("provider-1")

        XCTAssertEqual(service.probedProviderIDs, ["provider-1"])
        XCTAssertEqual(store.connections.first?.health, .healthy)
        XCTAssertEqual(store.connections.first?.circuitOpen, false)
        XCTAssertEqual(store.connections.first?.lastCheckedAtMs, 40)
        XCTAssertEqual(store.connections.first?.diagnosticRef, "artifact://provider/provider-1/probe.json")
        XCTAssertEqual(store.models.map(\.id).sorted(), ["provider-1/code-model", "provider-1/image-model"])
        XCTAssertTrue(store.models.allSatisfy { $0.health == .healthy })
        XCTAssertTrue(store.hasEligibleTextModel)
        XCTAssertTrue(store.hasEligibleImageModel)
    }

    func testProviderAndModelEnablementAffectEligibility() async throws {
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore())
        try await store.connect(
            ProviderDraft(endpoint: "https://api.openai.com/v1"),
            credential: SecureCredential(value: "secret-value")
        )
        let provider = try XCTUnwrap(store.connections.first)
        let model = try XCTUnwrap(store.models(for: provider.id).first)

        XCTAssertFalse(store.hasEligibleTextModel)
        try await store.applySuccessfulProbe(providerID: provider.id)
        XCTAssertTrue(store.hasEligibleTextModel)
        try await store.setModelEnabled(model.id, false)
        XCTAssertFalse(store.hasEligibleTextModel)
        try await store.setModelEnabled(model.id, true)
        XCTAssertTrue(store.hasEligibleTextModel)
        try await store.setProviderEnabled(provider.id, false)
        XCTAssertFalse(store.hasEligibleTextModel)
        try await store.setProviderEnabled(provider.id, true)
        XCTAssertFalse(store.hasEligibleTextModel, "a re-enabled provider needs a fresh successful probe")
        try await store.applySuccessfulProbe(providerID: provider.id)
        XCTAssertTrue(store.hasEligibleTextModel)
    }

    func testProviderImageModelEligibilityUsesImageCapability() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "healthy")]
        service.state.models = [
            Self.modelRecord(health: "healthy"),
            Self.imageModelRecord(health: "healthy"),
            Self.visionModelRecord(health: "healthy")
        ]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        XCTAssertEqual(store.eligibleImageModels.map(\.id), ["provider-1/image-model"])
        XCTAssertEqual(
            store.eligibleVisionModels.map(\.id).sorted(),
            ["provider-1/image-model", "provider-1/vision-model"]
        )
        XCTAssertTrue(store.hasEligibleImageModel)

        try await store.setModelEnabled("provider-1/image-model", false)

        XCTAssertFalse(store.hasEligibleImageModel)
        XCTAssertEqual(store.eligibleVisionModels.map(\.id), ["provider-1/vision-model"])
    }

    func testProviderStoreFiltersProviderModelsBySearchText() async throws {
        let service = RecordingProviderRegistryService()
        service.state.providers = [Self.providerRecord(health: "healthy")]
        service.state.models = [
            Self.modelRecord(health: "healthy"),
            Self.imageModelRecord(health: "healthy"),
            Self.visionModelRecord(health: "healthy")
        ]
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore(), service: service)

        await store.refresh()

        XCTAssertEqual(store.models(for: "provider-1", matching: "code").map(\.id), ["provider-1/code-model"])
        XCTAssertEqual(store.models(for: "provider-1", matching: "image").map(\.id), ["provider-1/image-model"])
        XCTAssertEqual(
            store.models(for: "provider-1", matching: "vision").map(\.id).sorted(),
            ["provider-1/image-model", "provider-1/vision-model"]
        )
        XCTAssertEqual(store.models(for: "provider-1", matching: "missing"), [])
        XCTAssertEqual(store.models(for: "provider-1", matching: "   ").count, 3)
    }

    func testProviderStoreRejectsNonLocalHttpEndpoint() async {
        let store = ProviderStore(secretStore: InMemoryProviderSecretStore())
        do {
            try await store.connect(
                ProviderDraft(endpoint: "http://example.com/v1"),
                credential: SecureCredential(value: "secret-value")
            )
            XCTFail("non-local HTTP endpoints must be rejected")
        } catch {
            XCTAssertEqual(error as? ProviderStoreError, .invalidEndpoint)
        }
    }

    nonisolated fileprivate static func providerRecord(
        kind: ProviderKind = .openRouter,
        health: String = "unknown",
        id: String = "provider-1",
        displayName: String = "OpenRouter",
        endpoint: String = "https://openrouter.ai/api/v1",
        circuitOpen: Bool = false,
        checkedAtMs: UInt64? = nil,
        reasonCode: String? = nil,
        diagnosticRef: String? = nil
    ) -> ProviderConnectionRecord {
        ProviderConnectionRecord(
            schema: "opensks.provider-connection.v1",
            id: id,
            kind: kind,
            displayName: displayName,
            enabled: true,
            endpoint: ProviderEndpointRecord(
                baseUrl: endpoint,
                allowInsecureHttp: false
            ),
            auth: ProviderSecretRef(
                store: "macos_keychain",
                service: "ai.opensks.provider.\(kind.rawValue)",
                account: id,
                version: 1
            ),
            organizationRef: nil,
            projectRef: nil,
            health: ProviderHealthSnapshotRecord(
                state: health,
                circuitOpen: circuitOpen,
                checkedAtMs: checkedAtMs,
                reasonCode: reasonCode ?? (health == "healthy" ? "probe_ok" : "not_probed"),
                diagnosticRef: diagnosticRef
            ),
            concurrency: ProviderConcurrencyRecord(
                maxConcurrentRequests: 3,
                requestsPerMinute: nil,
                tokensPerMinute: nil
            ),
            createdAtMs: 10,
            updatedAtMs: 20,
            revision: 1
        )
    }

    nonisolated fileprivate static func modelRecord(
        health: String = "unknown",
        enabled: Bool = true,
        providerID: String = "provider-1"
    ) -> ProviderModelRecord {
        ProviderModelRecord(
            schema: "opensks.model-catalog-entry.v1",
            id: "\(providerID)/code-model",
            providerId: providerID,
            remoteModelId: "code-model",
            displayName: "Code Model",
            enabled: enabled,
            capabilities: ProviderCapabilitiesRecord(
                text: true,
                code: true,
                visionInput: false,
                imageOutput: false,
                imageEdit: false,
                toolUse: true,
                structuredOutput: true,
                longContext: true,
                streaming: true
            ),
            limits: ProviderLimitsRecord(
                maxInputTokens: 128_000,
                maxOutputTokens: nil,
                requestsPerMinute: nil,
                tokensPerMinute: nil,
                maxConcurrency: nil
            ),
            health: health,
            roleScores: ["code": ProviderRoleScoreRecord(score: 0.9, evidenceRefs: ["test"])],
            catalogRevision: "catalog-1"
        )
    }

    nonisolated fileprivate static func imageModelRecord(
        health: String = "unknown",
        enabled: Bool = true,
        providerID: String = "provider-1"
    ) -> ProviderModelRecord {
        ProviderModelRecord(
            schema: "opensks.model-catalog-entry.v1",
            id: "\(providerID)/image-model",
            providerId: providerID,
            remoteModelId: "image-model",
            displayName: "Image Model",
            enabled: enabled,
            capabilities: ProviderCapabilitiesRecord(
                text: false,
                code: false,
                visionInput: true,
                imageOutput: true,
                imageEdit: false,
                toolUse: false,
                structuredOutput: false,
                longContext: false,
                streaming: false
            ),
            limits: ProviderLimitsRecord(
                maxInputTokens: nil,
                maxOutputTokens: nil,
                requestsPerMinute: nil,
                tokensPerMinute: nil,
                maxConcurrency: nil
            ),
            health: health,
            roleScores: [
                "image": ProviderRoleScoreRecord(score: 0.9, evidenceRefs: ["test"]),
                "vision": ProviderRoleScoreRecord(score: 0.7, evidenceRefs: ["test"])
            ],
            catalogRevision: "catalog-1"
        )
    }

    nonisolated fileprivate static func visionModelRecord(
        health: String = "unknown",
        enabled: Bool = true,
        providerID: String = "provider-1"
    ) -> ProviderModelRecord {
        ProviderModelRecord(
            schema: "opensks.model-catalog-entry.v1",
            id: "\(providerID)/vision-model",
            providerId: providerID,
            remoteModelId: "vision-model",
            displayName: "Vision Model",
            enabled: enabled,
            capabilities: ProviderCapabilitiesRecord(
                text: true,
                code: false,
                visionInput: true,
                imageOutput: false,
                imageEdit: false,
                toolUse: false,
                structuredOutput: true,
                longContext: false,
                streaming: true
            ),
            limits: ProviderLimitsRecord(
                maxInputTokens: 32_000,
                maxOutputTokens: nil,
                requestsPerMinute: nil,
                tokensPerMinute: nil,
                maxConcurrency: nil
            ),
            health: health,
            roleScores: ["vision": ProviderRoleScoreRecord(score: 0.8, evidenceRefs: ["test"])],
            catalogRevision: "catalog-1"
        )
    }
}

private final class RecordingProviderRegistryService: ProviderRegistryService, @unchecked Sendable {
    var state = ProviderRegistryState(
        schema: "opensks.provider-registry-state.v1",
        providers: [],
        models: [],
        latestProbes: []
    )
    var upsertedConnection: ProviderConnectionRecord?
    var syncedModels: [ProviderModelRecord] = []
    var probedProviderIDs: [String] = []

    func registryState() async throws -> ProviderRegistryState {
        state
    }

    func upsertConnection(
        _ connection: ProviderConnectionRecord,
        expectedRevision _: UInt64?
    ) async throws -> ProviderRegistryCommandResult {
        upsertedConnection = connection
        state.providers.removeAll { $0.id == connection.id }
        state.providers.append(connection)
        return result(providerID: connection.id, mutation: "created", revision: connection.revision)
    }

    func deleteProvider(id: String, expectedRevision: UInt64) async throws -> ProviderRegistryCommandResult {
        state.providers.removeAll { $0.id == id }
        state.models.removeAll { $0.providerId == id }
        return result(providerID: id, mutation: "deleted", revision: expectedRevision)
    }

    func setProviderEnabled(
        id: String,
        enabled: Bool,
        expectedRevision: UInt64
    ) async throws -> ProviderRegistryCommandResult {
        if let index = state.providers.firstIndex(where: { $0.id == id }) {
            state.providers[index].enabled = enabled
            state.providers[index].revision = expectedRevision + 1
        }
        return result(providerID: id, mutation: enabled ? "enabled" : "disabled", revision: expectedRevision + 1)
    }

    func probeProvider(id: String) async throws -> ProviderRegistryProbeResult {
        probedProviderIDs.append(id)
        let provider = ProviderTests.providerRecord(
            health: "healthy",
            id: id,
            checkedAtMs: 40,
            diagnosticRef: "artifact://provider/\(id)/probe.json"
        )
        let model = ProviderTests.modelRecord(health: "healthy", providerID: id)
        let imageModel = ProviderTests.imageModelRecord(health: "healthy", providerID: id)
        let receipt = ProviderProbeReceiptRecord(
            schema: "opensks.provider-probe-receipt.v1",
            providerId: id,
            endpointHostRedacted: "openrouter.ai",
            httpCategory: "success",
            latencyBucket: "under250_ms",
            authAccepted: true,
            modelListAvailable: true,
            catalogCount: 1,
            occurredAtMs: 40,
            reasonCode: "probe_ok",
            diagnosticRef: "artifact://provider/\(id)/probe.json"
        )
        state.providers.removeAll { $0.id == id }
        state.providers.append(provider)
        state.models.removeAll { $0.providerId == id }
        state.models.append(model)
        state.models.append(imageModel)
        state.latestProbes.removeAll { $0.providerId == id }
        state.latestProbes.append(receipt)
        return ProviderRegistryProbeResult(
            schema: "opensks.provider-registry-probe-result.v1",
            provider: provider,
            probeReceipt: receipt,
            models: [model, imageModel],
            syncReceipt: result(providerID: id, mutation: "models_synced", revision: 2).receipt
        )
    }

    func syncModels(providerID: String, models: [ProviderModelRecord]) async throws -> ProviderRegistryCommandResult {
        syncedModels = models
        state.models.removeAll { $0.providerId == providerID }
        state.models.append(contentsOf: models)
        return result(providerID: providerID, mutation: "models_synced", revision: 1)
    }

    func setModelEnabled(id: String, enabled: Bool) async throws -> ProviderRegistryModelResult {
        if let index = state.models.firstIndex(where: { $0.id == id }) {
            state.models[index].enabled = enabled
        }
        return ProviderRegistryModelResult(
            schema: "opensks.provider-registry-model-result.v1",
            model: state.models.first { $0.id == id }!
        )
    }

    func recordProbe(_ receipt: ProviderProbeReceiptRecord) async throws -> ProviderRegistryCommandResult {
        state.latestProbes.removeAll { $0.providerId == receipt.providerId }
        state.latestProbes.append(receipt)
        return result(providerID: receipt.providerId, mutation: "updated", revision: 1)
    }

    private func result(providerID: String, mutation: String, revision: UInt64) -> ProviderRegistryCommandResult {
        ProviderRegistryCommandResult(
            schema: "opensks.provider-registry-command-result.v1",
            receipt: ProviderMutationReceiptRecord(
                schema: "opensks.provider-mutation.v1",
                providerId: providerID,
                mutation: mutation,
                revision: revision,
                secretRef: nil,
                secretValueExposed: false,
                occurredAtMs: 30,
                reasonCode: "test"
            )
        )
    }
}
