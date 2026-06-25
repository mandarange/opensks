import SwiftUI

struct ProviderModelCatalogView: View {
    @ObservedObject var store: ProviderStore
    let provider: ProviderConnectionViewModel
    @Binding var modelSearchText: String

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            header
            let allProviderModels = store.models(for: provider.id)
            let providerModels = store.models(for: provider.id, matching: modelSearchText)
            if allProviderModels.isEmpty {
                Text("Model catalog has not been synced for this provider.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.faint)
                    .accessibilityIdentifier("settings.providers.models.empty")
            } else if providerModels.isEmpty {
                Text("No models match this search.")
                    .font(Theme.ui(12))
                    .foregroundStyle(Theme.faint)
                    .accessibilityIdentifier("settings.providers.models.noResults")
            } else {
                ForEach(providerModels) { model in
                    modelRow(model)
                }
            }
        }
        .accessibilityIdentifier("settings.providers.models")
    }

    private var header: some View {
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
    }

    private func modelRow(_ model: ProviderModelViewModel) -> some View {
        HStack(spacing: Theme.s10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(model.displayName)
                    .font(Theme.ui(12.5, .semibold))
                    .foregroundStyle(model.enabled ? Theme.text : Theme.muted)
                Text(model.capabilities.map(\.rawValue).sorted().joined(separator: " · "))
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 0)
            if let contextWindow = model.contextWindow {
                Text("\(contextWindow / 1000)k")
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textSoft)
            }
            StatusPill(kind: model.health.pillKind, label: model.health.label)
            Toggle("", isOn: modelEnabledBinding(model.id))
                .labelsHidden()
                .disabled(!provider.enabled)
                .accessibilityIdentifier("settings.providers.model.enabled.\(model.id)")
        }
        .padding(.horizontal, Theme.s10)
        .frame(height: 50)
        .background(RoundedRectangle(cornerRadius: Theme.rSm).fill(Theme.panel))
        .accessibilityIdentifier("settings.providers.model.\(model.id)")
    }

    private func modelEnabledBinding(_ id: String) -> Binding<Bool> {
        Binding(
            get: { store.models.first { $0.id == id }?.enabled ?? false },
            set: { enabled in Task { try? await store.setModelEnabled(id, enabled) } }
        )
    }
}
