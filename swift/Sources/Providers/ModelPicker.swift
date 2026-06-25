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

    private var models: [ProviderModelViewModel] {
        switch kind {
        case .text: return providers.eligibleTextModels
        case .image: return providers.eligibleImageModels
        }
    }

    var body: some View {
        Menu {
            Button {
                onSelectAuto()
            } label: {
                Label("Auto route", systemImage: autoSelected ? "checkmark" : "circle")
            }
            if models.isEmpty {
                Divider()
                Button {
                    providers.showingAddProvider = true
                } label: {
                    Label(kind.setupActionLabel, systemImage: "key")
                }
            } else {
                Divider()
                ForEach(models) { model in
                    Button {
                        onSelectModel(model)
                    } label: {
                        Label(
                            menuTitle(model),
                            systemImage: selectedModelID == model.id ? "checkmark" : kind.icon
                        )
                    }
                    .disabled(!isSelectable(model))
                }
            }
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
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityIdentifier(kind == .text ? "conversation.composer.settings.model" : "conversation.composer.settings.image-model")
    }

    private func menuTitle(_ model: ProviderModelViewModel) -> String {
        let provider = providers.providerDisplayName(for: model.providerID)
        let capabilitySummary = model.capabilities.map(\.rawValue).sorted().joined(separator: ", ")
        let context = model.contextWindow.map { " · \($0 / 1000)k" } ?? ""
        return "\(provider) · \(model.displayName) · \(capabilitySummary)\(context)"
    }

    private func isSelectable(_ model: ProviderModelViewModel) -> Bool {
        switch kind {
        case .text: return model.isEligibleForCode
        case .image: return model.isEligibleForImage
        }
    }
}
