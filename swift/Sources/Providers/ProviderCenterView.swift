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

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            LazyVGrid(columns: infoColumns, alignment: .leading, spacing: Theme.s12) {
                infoBlock("Endpoint", provider.endpoint, "network")
                infoBlock("Credential", "\(provider.secretRef.store) ref v\(provider.secretRef.version)", "key")
                infoBlock("Concurrency", "\(provider.activeRequests)/\(provider.maxConcurrentRequests)", "gauge.with.dots.needle.33percent")
                infoBlock(
                    "Circuit",
                    provider.circuitLabel,
                    provider.circuitOpen ? "bolt.trianglebadge.exclamationmark" : "checkmark.seal"
                )
                infoBlock("Last check", provider.lastCheckedLabel, "clock")
            }
            HStack(alignment: .top, spacing: Theme.s10) {
                VStack(alignment: .leading, spacing: Theme.s6) {
                    Text(provider.lastDiagnostic)
                        .font(Theme.ui(12))
                        .foregroundStyle(Theme.muted)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("settings.providers.diagnostic")
                    if let adapterDiagnostic = provider.adapterDiagnostic {
                        Label(adapterDiagnostic, systemImage: "exclamationmark.triangle.fill")
                            .font(Theme.ui(11.5))
                            .foregroundStyle(provider.adapterBlockers.isEmpty ? Theme.muted : Theme.gold)
                            .fixedSize(horizontal: false, vertical: true)
                            .accessibilityIdentifier("settings.providers.adapterDiagnostic")
                    }
                    if let diagnosticRef = provider.diagnosticReferenceLabel {
                        Label("Diagnostic ref \(diagnosticRef)", systemImage: "doc.text.magnifyingglass")
                            .font(Theme.ui(11.5))
                            .foregroundStyle(Theme.muted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .fixedSize(horizontal: false, vertical: true)
                            .accessibilityIdentifier("settings.providers.diagnosticRef")
                    }
                }
                Spacer(minLength: 0)
                Button {
                    Task { try? await store.probeProvider(provider.id) }
                } label: {
                    Label("Test & sync", systemImage: "arrow.triangle.2.circlepath")
                }
                .buttonStyle(.secondaryAction)
                .frame(width: 132)
                .disabled(!provider.enabled)
                .accessibilityIdentifier("settings.providers.probe.\(provider.id)")
            }
            modelSection
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Theme.rMd).fill(Theme.input))
        .overlay(RoundedRectangle(cornerRadius: Theme.rMd).strokeBorder(Theme.stroke, lineWidth: 1))
    }

    private var infoColumns: [GridItem] {
        [GridItem(.adaptive(minimum: 132), spacing: Theme.s12, alignment: .top)]
    }

    private var modelSection: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Text("Models")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer(minLength: 0)
                TextField("Search models", text: $modelSearchText)
                    .textFieldStyle(.roundedBorder)
                    .font(Theme.ui(12))
                    .frame(maxWidth: 220)
                    .accessibilityIdentifier("settings.providers.models.search")
            }
            let allProviderModels = store.models(for: provider.id)
            let providerModels = store.models(for: provider.id, matching: modelSearchText)
            if allProviderModels.isEmpty {
                Text("Model catalog has not been synced for this provider.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.faint)
            } else if providerModels.isEmpty {
                Text("No models match this search.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.faint)
            } else {
                ForEach(providerModels) { model in
                    HStack(spacing: Theme.s10) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(model.displayName)
                                .font(Theme.ui(12.5, .semibold))
                                .foregroundStyle(Theme.text)
                            Text(model.capabilities.map(\.rawValue).sorted().joined(separator: " · "))
                                .font(Theme.ui(11))
                                .foregroundStyle(Theme.muted)
                        }
                        Spacer(minLength: 0)
                        if let contextWindow = model.contextWindow {
                            Text("\(contextWindow / 1000)k")
                                .font(Theme.mono(11))
                                .foregroundStyle(Theme.textSoft)
                        }
                        Toggle("", isOn: modelEnabledBinding(model.id))
                            .labelsHidden()
                            .disabled(!provider.enabled)
                            .accessibilityIdentifier("settings.providers.model.enabled.\(model.id)")
                    }
                    .padding(.horizontal, Theme.s10)
                    .frame(height: 48)
                    .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Theme.panel))
                }
            }
        }
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

    private func modelEnabledBinding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { store.models.first { $0.id == id }?.enabled ?? false },
            set: { enabled in Task { try? await store.setModelEnabled(id, enabled) } }
        )
    }
}

struct ProviderConnectionWizard: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var store: ProviderStore
    @StateObject private var flightGuard = ProviderConnectionFlightGuard()
    @State private var draft = ProviderDraft()
    @State private var credential = ""
    @State private var savedProviderID: String?
    @State private var errorMessage: String?
    @State private var statusMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s16) {
            HStack {
                Text("Add Provider")
                    .font(Theme.ui(18, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(.secondaryAction)
                .frame(width: 36)
                .accessibilityIdentifier("providers.wizard.close")
            }
            form
            if let errorMessage {
                Text(errorMessage)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.coral)
                    .accessibilityIdentifier("providers.wizard.error")
            } else if let statusMessage {
                Text(statusMessage)
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.green)
                    .accessibilityIdentifier("providers.wizard.status")
            }
            Spacer(minLength: 0)
            HStack {
                Button {
                    Task { await saveAndTest() }
                } label: {
                    Label(
                        flightGuard.inFlight ? "Testing..." : "Save & test",
                        systemImage: "waveform.path.ecg"
                    )
                }
                .buttonStyle(.secondaryAction)
                .disabled(flightGuard.inFlight || (!canSave && savedProviderID == nil))
                .accessibilityIdentifier("providers.wizard.probe")
                Spacer()
                Button("Cancel") { dismiss() }
                    .buttonStyle(.secondaryAction)
                    .disabled(flightGuard.inFlight)
                Button {
                    Task { await save() }
                } label: {
                    Label(savedProviderID == nil ? "Save" : "Done", systemImage: "checkmark")
                }
                .buttonStyle(.primaryAction)
                .disabled(flightGuard.inFlight || (!canSave && savedProviderID == nil))
                .accessibilityIdentifier("providers.wizard.save")
            }
        }
        .padding(24)
        .background(Theme.bg)
    }

    private var form: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            Picker("Provider", selection: $draft.kind) {
                ForEach(ProviderKind.allCases) { kind in
                    Text(kind.displayLabel).tag(kind)
                }
            }
            .onChange(of: draft.kind) { kind in
                draft.displayName = kind.displayLabel
                draft.endpoint = kind.defaultEndpoint
            }
            .accessibilityIdentifier("providers.wizard.kind")

            TextField("Display name", text: $draft.displayName)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("providers.wizard.name")

            TextField("Endpoint", text: $draft.endpoint)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("providers.wizard.endpoint")

            SecureField("API key", text: $credential)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("providers.wizard.credential")

            HStack {
                TextField("Organization", text: $draft.organizationRef)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("providers.wizard.organization")
                TextField("Project", text: $draft.projectRef)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("providers.wizard.project")
            }

            Stepper("Max parallel requests: \(draft.maxConcurrentRequests)", value: $draft.maxConcurrentRequests, in: 1...16)
                .accessibilityIdentifier("providers.wizard.concurrency")

            Toggle("Enable provider after saving", isOn: $draft.enabled)
                .accessibilityIdentifier("providers.wizard.enabled")
        }
        .font(Theme.ui(12))
    }

    private var canSave: Bool {
        !draft.displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !draft.endpoint.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !credential.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func save() async {
        do {
            _ = try await flightGuard.run {
                if savedProviderID == nil {
                    savedProviderID = try await store.connect(
                        draft,
                        credential: SecureCredential(value: credential)
                    )
                }
                dismiss()
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func saveAndTest() async {
        do {
            _ = try await flightGuard.run {
                errorMessage = nil
                statusMessage = nil
                let providerID: String
                if let savedProviderID {
                    providerID = savedProviderID
                } else {
                    providerID = try await store.connect(
                        draft,
                        credential: SecureCredential(value: credential)
                    )
                    savedProviderID = providerID
                }
                try await store.probeProvider(providerID)
                statusMessage = "Connection test passed and model catalog synced."
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

@MainActor
final class ProviderConnectionFlightGuard: ObservableObject {
    @Published private(set) var inFlight = false

    func run(_ operation: () async throws -> Void) async throws -> Bool {
        guard !inFlight else { return false }
        inFlight = true
        defer { inFlight = false }
        try await operation()
        return true
    }
}
