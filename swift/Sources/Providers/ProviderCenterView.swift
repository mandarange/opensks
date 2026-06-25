import SwiftUI

struct ProviderCenterView: View {
    @ObservedObject var store: ProviderStore

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            header
            if store.connections.isEmpty {
                emptyState
            } else {
                providerList
                selectedDetail
            }
        }
        .sheet(isPresented: $store.showingAddProvider) {
            ProviderConnectionWizard(store: store)
                .frame(width: 560, height: 620)
        }
        .task { await store.refresh() }
        .accessibilityIdentifier("settings.providers")
    }

    private var header: some View {
        HStack(spacing: Theme.s10) {
            Image(systemName: "server.rack")
                .foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 2) {
                Text("Providers")
                    .font(Theme.ui(15, .semibold))
                    .foregroundStyle(Theme.text)
                Text("Connect model providers, store credentials in Keychain, and control enabled models.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
            Button {
                store.showingAddProvider = true
            } label: {
                Label("Add", systemImage: "plus")
            }
            .buttonStyle(.secondaryAction)
            .accessibilityIdentifier("settings.providers.add")
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Theme.s10) {
            StatusPill(kind: .warning, label: "No provider")
            Text(store.providerReadinessDetail)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                store.showingAddProvider = true
            } label: {
                Label("Connect a provider", systemImage: "key")
            }
            .buttonStyle(.primaryAction)
            .frame(width: 210)
            .accessibilityIdentifier("settings.providers.connect")
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.input))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private var providerList: some View {
        VStack(spacing: Theme.s8) {
            ForEach(store.connections) { provider in
                Button {
                    store.selectedProviderID = provider.id
                } label: {
                    providerRow(provider)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("settings.providers.card.\(provider.id)")
            }
        }
    }

    private func providerRow(_ provider: ProviderConnectionViewModel) -> some View {
        HStack(spacing: Theme.s10) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: Theme.s8) {
                    Text(provider.displayName)
                        .font(Theme.ui(13, .semibold))
                        .foregroundStyle(Theme.text)
                    StatusPill(kind: provider.statusPillKind, label: provider.statusLabel)
                }
                Text(provider.kind.displayLabel)
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 0)
            Text("\(provider.enabledModelCount) models")
                .font(Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.textSoft)
            Toggle("", isOn: providerEnabledBinding(provider.id))
                .labelsHidden()
                .accessibilityIdentifier("settings.providers.enabled.\(provider.id)")
        }
        .padding(.horizontal, Theme.s12)
        .frame(height: 56)
        .background(
            RoundedRectangle(cornerRadius: Theme.rMd)
                .fill(provider.id == store.selectedProviderID ? Theme.accentTint : Theme.input)
        )
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private var selectedDetail: some View {
        let provider = store.connections.first { $0.id == store.selectedProviderID } ?? store.connections.first
        return VStack(alignment: .leading, spacing: Theme.s12) {
            if let provider {
                ProviderDetailView(store: store, provider: provider)
            }
        }
    }

    private func providerEnabledBinding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { store.connections.first { $0.id == id }?.enabled ?? false },
            set: { enabled in Task { try? await store.setProviderEnabled(id, enabled) } }
        )
    }
}

private struct ProviderDetailView: View {
    @ObservedObject var store: ProviderStore
    let provider: ProviderConnectionViewModel
    @State private var modelSearchText = ""
    @State private var selectedTab: ProviderDetailTab = .connection

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            Picker("Provider detail", selection: $selectedTab) {
                ForEach(ProviderDetailTab.allCases) { tab in
                    Text(tab.label).tag(tab)
                }
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier("settings.providers.detail.tabs")

            switch selectedTab {
            case .connection:
                connectionSection
            case .models:
                ProviderModelCatalogView(
                    store: store,
                    provider: provider,
                    modelSearchText: $modelSearchText
                )
            case .routing:
                routingSection
            case .limits:
                limitsSection
            case .diagnostics:
                ProviderDiagnosticsView(store: store, provider: provider)
            }
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.input))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private var connectionSection: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            LazyVGrid(columns: infoColumns, alignment: .leading, spacing: Theme.s12) {
                infoBlock("Endpoint", provider.endpoint, "network")
                infoBlock("Credential", "\(provider.secretRef.store) ref v\(provider.secretRef.version)", "key")
                infoBlock("Provider ID", provider.id, "number")
                infoBlock("Revision", "\(provider.revision)", "arrow.triangle.2.circlepath")
            }
            HStack(spacing: Theme.s10) {
                Button {
                    Task { try? await store.probeProvider(provider.id) }
                } label: {
                    Label("Test & sync", systemImage: "arrow.triangle.2.circlepath")
                }
                .buttonStyle(.secondaryAction)
                .frame(width: 132)
                .disabled(!provider.enabled)
                .accessibilityIdentifier("settings.providers.probe.\(provider.id)")
                Text(provider.lastDiagnostic)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("settings.providers.diagnostic")
                Spacer(minLength: 0)
            }
        }
        .accessibilityIdentifier("settings.providers.connection")
    }

    private var routingSection: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            Label("Routing", systemImage: "point.3.connected.trianglepath.dotted")
                .font(Theme.ui(13, .semibold))
                .foregroundStyle(Theme.text)
            let codeModels = store.models(for: provider.id).filter { $0.capabilities.contains(.code) }
            if codeModels.isEmpty {
                Text("No code-capable models are available for this provider.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.faint)
            } else {
                ForEach(codeModels) { model in
                    providerRouteRow(model)
                }
            }
        }
        .accessibilityIdentifier("settings.providers.routing")
    }

    private var limitsSection: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            LazyVGrid(columns: infoColumns, alignment: .leading, spacing: Theme.s12) {
                infoBlock("Active requests", "\(provider.activeRequests)", "bolt")
                infoBlock("Max concurrency", "\(provider.maxConcurrentRequests)", "gauge.with.dots.needle.33percent")
                infoBlock("Circuit", provider.circuitLabel, provider.circuitOpen ? "bolt.trianglebadge.exclamationmark" : "checkmark.seal")
                infoBlock("Enabled models", "\(provider.enabledModelCount)", "cpu")
            }
        }
        .accessibilityIdentifier("settings.providers.limits")
    }

    private func infoBlock(_ label: String, _ value: String, _ icon: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(label, systemImage: icon)
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.muted)
            Text(value)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var infoColumns: [GridItem] {
        [GridItem(.adaptive(minimum: 132), spacing: Theme.s12, alignment: .top)]
    }

    private func providerRouteRow(_ model: ProviderModelViewModel) -> some View {
        HStack(spacing: Theme.s10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(model.displayName)
                    .font(Theme.ui(12.5, .semibold))
                    .foregroundStyle(model.isEligibleForCode ? Theme.text : Theme.muted)
                Text(routeReason(for: model))
                    .font(Theme.ui(11))
                    .foregroundStyle(model.isEligibleForCode ? Theme.muted : Theme.gold)
            }
            Spacer(minLength: 0)
            StatusPill(kind: model.isEligibleForCode ? .success : .warning, label: model.health.label)
        }
        .padding(.horizontal, Theme.s10)
        .frame(height: 48)
        .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Theme.panel))
        .accessibilityIdentifier("settings.providers.routing.model.\(model.id)")
    }

    private func routeReason(for model: ProviderModelViewModel) -> String {
        if !provider.enabled { return "Provider disabled" }
        if !model.enabled { return "Model disabled" }
        if provider.health != .healthy { return "Provider \(provider.health.label.lowercased())" }
        if model.health != .healthy { return "Model \(model.health.label.lowercased())" }
        if !model.capabilities.contains(.code) { return "Missing code capability" }
        return "Eligible for code routing"
    }
}

private enum ProviderDetailTab: String, CaseIterable, Identifiable {
    case connection
    case models
    case routing
    case limits
    case diagnostics

    var id: String { rawValue }

    var label: String {
        switch self {
        case .connection: return "Connection"
        case .models: return "Models"
        case .routing: return "Routing"
        case .limits: return "Limits"
        case .diagnostics: return "Diagnostics"
        }
    }
}
