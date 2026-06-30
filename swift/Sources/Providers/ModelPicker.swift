import SwiftUI

enum ModelPickerKind: Equatable {
    case text
    case image

    var icon: String {
        switch self {
        case .text: return "cpu"
        case .image: return "photo"
        }
    }

    var label: String {
        switch self {
        case .text: return "Model"
        case .image: return "Image"
        }
    }

    var setupActionLabel: String {
        switch self {
        case .text: return "Connect provider"
        case .image: return "Add provider"
        }
    }
}

struct ModelPicker: View {
    @ObservedObject var providers: ProviderStore
    let kind: ModelPickerKind
    let selectedModelID: String?
    let autoSelected: Bool
    let chipText: String
    let onSelectAuto: () -> Void
    let onSelectModel: (ProviderModelViewModel) -> Void
    @State private var showingPicker = false
    @State private var searchText = ""

    private var models: [ProviderModelViewModel] {
        switch kind {
        case .text: return providers.eligibleTextModels
        case .image: return providers.eligibleImageModels
        }
    }

    private var filteredModels: [ProviderModelViewModel] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return models }
        return models.filter { modelMatchesSearch($0, query: query) }
    }

    var body: some View {
        Button {
            showingPicker.toggle()
        } label: {
            Label("\(kind.label) \(chipText)", systemImage: kind.icon)
                .labelStyle(.titleAndIcon)
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.textSoft)
                .padding(.horizontal, 8)
                .frame(height: 24)
                .background(
                    RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                        .fill(Theme.panel)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                        .strokeBorder(Theme.stroke, lineWidth: 1)
                )
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showingPicker, arrowEdge: .top) {
            pickerPopover
        }
        .onChange(of: showingPicker) { isPresented in
            if !isPresented {
                searchText = ""
            }
        }
        .fixedSize()
        .accessibilityIdentifier(kind == .text ? "conversation.composer.settings.model" : "conversation.composer.settings.image-model")
    }

    private var pickerPopover: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(Theme.muted)
                TextField("Search models", text: $searchText)
                    .textFieldStyle(.plain)
                    .font(Theme.ui(12))
            }
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .fill(Theme.input)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                    .strokeBorder(Theme.stroke, lineWidth: 1)
            )
            .accessibilityIdentifier(kind == .text ? "conversation.composer.settings.model.search" : "conversation.composer.settings.image-model.search")

            Button {
                onSelectAuto()
                showingPicker = false
            } label: {
                pickerRow(
                    title: "Auto route",
                    subtitle: "Let OpenSKS choose",
                    systemImage: autoSelected ? "checkmark" : "circle"
                )
            }
            .buttonStyle(.plain)

            Divider()

            if models.isEmpty {
                Button {
                    providers.showingAddProvider = true
                    showingPicker = false
                } label: {
                    pickerRow(
                        title: kind.setupActionLabel,
                        subtitle: "Provider setup",
                        systemImage: "key"
                    )
                }
                .buttonStyle(.plain)
            } else if filteredModels.isEmpty {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass")
                    Text("No matching models")
                }
                .font(Theme.ui(12, .medium))
                .foregroundStyle(Theme.muted)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 8)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(filteredModels) { model in
                            Button {
                                onSelectModel(model)
                                showingPicker = false
                            } label: {
                                pickerRow(
                                    title: model.displayName,
                                    subtitle: modelSubtitle(model),
                                    systemImage: selectedModelID == model.id ? "checkmark" : kind.icon
                                )
                            }
                            .buttonStyle(.plain)
                            .disabled(!isSelectable(model))
                        }
                    }
                }
                .frame(maxHeight: 280)
            }
        }
        .padding(12)
        .frame(width: 360)
        .background(Theme.panel)
    }

    private func pickerRow(title: String, subtitle: String, systemImage: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: systemImage)
                .frame(width: 16)
                .foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(Theme.ui(12, .semibold))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text(subtitle)
                    .font(Theme.ui(10))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
        }
        .padding(.horizontal, 8)
        .frame(height: 40)
        .contentShape(Rectangle())
    }

    private func modelSubtitle(_ model: ProviderModelViewModel) -> String {
        let provider = providers.providerDisplayName(for: model.providerID)
        let capabilitySummary = model.capabilities.map(\.rawValue).sorted().joined(separator: ", ")
        let context = model.contextWindow.map { " · \($0 / 1000)k" } ?? ""
        return "\(provider) · \(capabilitySummary)\(context)"
    }

    private func modelMatchesSearch(_ model: ProviderModelViewModel, query: String) -> Bool {
        let provider = providers.providerDisplayName(for: model.providerID)
        return model.id.lowercased().contains(query)
            || model.remoteModelID.lowercased().contains(query)
            || model.displayName.lowercased().contains(query)
            || provider.lowercased().contains(query)
            || model.health.label.lowercased().contains(query)
            || model.capabilities.contains { $0.rawValue.lowercased().contains(query) }
    }

    private func isSelectable(_ model: ProviderModelViewModel) -> Bool {
        switch kind {
        case .text: return providers.modelIsSelectable(model, requiring: .code)
        case .image: return providers.modelIsSelectable(model, requiring: .image)
        }
    }
}
