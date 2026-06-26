// ConversationComposer.swift — the ONLY primary send path for a conversation
// (PR-027). A multiline text field bound to the per-conversation draft plus a
// compact thread-settings bar and a single primary Send button. Send is disabled
// when the trimmed draft is empty or a send is already in flight, so one Send
// starts exactly one turn. The whole button tile is the hit target
// (SurfaceButtonStyle .primaryAction). This composer does NOT call the legacy
// engine-run path.

import SwiftUI

struct ConversationComposer: View {
    @ObservedObject var store: ConversationStore
    @ObservedObject var providers: ProviderStore
    let conversationID: String

    /// Draft text bound through the store so it survives selection changes and
    /// is cleared on a successful send.
    private var draftBinding: Binding<String> {
        Binding(
            get: { store.draft(for: conversationID) },
            set: { store.setDraft($0, for: conversationID) }
        )
    }

    private var trimmedDraft: String {
        store.draft(for: conversationID).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSend: Bool {
        !trimmedDraft.isEmpty && !store.isSending(conversationID: conversationID) && providers.hasEligibleTextModel
    }

    private var settings: ConversationThreadSettings {
        store.threadSettings(for: conversationID)
    }

    private var contextAttachments: [ConversationContextAttachment] {
        store.contextAttachments(for: conversationID)
    }

    private var staleContextCount: Int {
        contextAttachments.filter(\.isStale).count
    }

    var body: some View {
        VStack(spacing: 0) {
            Divider().overlay(Theme.stroke)
            VStack(alignment: .leading, spacing: 8) {
                settingsBar
                if !providers.hasEligibleTextModel {
                    providerReadinessBar
                }
                if !contextAttachments.isEmpty {
                    contextBar
                }
                HStack(alignment: .bottom, spacing: 10) {
                    TextField("Message the engine…", text: draftBinding, axis: .vertical)
                        .textFieldStyle(.plain)
                        .font(Theme.ui(13))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1...6)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(
                            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                                .fill(Theme.input)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: Theme.rMd, style: .continuous)
                                .strokeBorder(Theme.stroke, lineWidth: 1)
                        )
                        .accessibilityIdentifier("conversation.composer.field")

                    Button(action: send) {
                        Label("Send", systemImage: "paperplane.fill")
                            .labelStyle(.titleAndIcon)
                    }
                    .buttonStyle(.primaryAction)
                    .frame(width: 110)
                    .disabled(!canSend)
                    .help(sendHelp)
                    .accessibilityIdentifier("conversation.composer.send")
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
        }
        .background(Theme.bg)
        .task(id: conversationID) {
            await store.loadThreadSettings(for: conversationID)
        }
    }

    private var settingsBar: some View {
        HStack(spacing: 8) {
            ModelPicker(
                providers: providers,
                kind: .text,
                selectedModelID: settings.modelSelection.modelId,
                autoSelected: settings.modelSelection.mode == .auto,
                chipText: modelLabel,
                onSelectAuto: {
                    updateSettings {
                        $0.modelSelection = ModelSelection(mode: .auto, modelId: nil, fallbackModelIds: [])
                    }
                },
                onSelectModel: { model in
                    updateSettings {
                        $0.modelSelection = providers.textModelSelection(pinning: model.id)
                    }
                }
            )
            ModelPicker(
                providers: providers,
                kind: .image,
                selectedModelID: settings.imageModelId,
                autoSelected: settings.imageModelId == nil,
                chipText: imageModelLabel,
                onSelectAuto: {
                    updateSettings {
                        $0.imageModelId = nil
                    }
                },
                onSelectModel: { model in
                    updateSettings {
                        $0.imageModelId = model.id
                    }
                }
            )
            executionModeMenu
            reasoningMenu
            pipelineMenu
            parallelismMenu
            toolPolicyMenu
            Spacer(minLength: 0)
        }
        .disabled(
            store.isSavingThreadSettings(for: conversationID)
                || store.isSending(conversationID: conversationID)
        )
        .accessibilityIdentifier("conversation.composer.settings")
    }

    private var contextBar: some View {
        HStack(spacing: Theme.s8) {
            StatusPill(
                kind: staleContextCount > 0 ? .warning : .neutral,
                label: staleContextCount > 0 ? "\(staleContextCount) stale" : "Context \(contextAttachments.count)"
            )
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: Theme.s6) {
                    ForEach(contextAttachments) { attachment in
                        contextChip(attachment)
                    }
                }
            }
        }
        .padding(.horizontal, Theme.s10)
        .padding(.vertical, Theme.s8)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.input)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityIdentifier("conversation.composer.context")
    }

    private func contextChip(_ attachment: ConversationContextAttachment) -> some View {
        HStack(spacing: Theme.s6) {
            Image(systemName: attachment.isStale ? "exclamationmark.triangle.fill" : "doc.text.magnifyingglass")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(attachment.isStale ? Theme.gold : Theme.accent)
            Text(attachment.displayLabel)
                .font(Theme.ui(11, .medium))
                .foregroundStyle(attachment.isStale ? Theme.gold : Theme.textSoft)
                .lineLimit(1)
                .truncationMode(.middle)
            Button {
                store.removeContextAttachment(attachment.id, from: conversationID)
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 8, weight: .bold))
                    .frame(width: 16, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.muted)
            .help("Remove context")
            .accessibilityIdentifier("conversation.composer.context.remove.\(attachment.id.uuidString)")
        }
        .padding(.leading, Theme.s8)
        .padding(.trailing, Theme.s4)
        .frame(height: 24)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(attachment.isStale ? Theme.gold.opacity(0.10) : Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(attachment.isStale ? Theme.gold.opacity(0.35) : Theme.stroke, lineWidth: 1)
        )
        .help(attachment.isStale ? "Attached context changed since capture." : attachment.wireRef)
    }

    private var providerReadinessBar: some View {
        HStack(spacing: Theme.s8) {
            StatusPill(kind: .warning, label: providerReadinessLabel)
            Text(providerReadinessDetail)
                .font(Theme.ui(11.5))
                .foregroundStyle(Theme.muted)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: Theme.s8)
            Button {
                providers.showingAddProvider = true
            } label: {
                Label(providerSetupActionLabel, systemImage: "key")
            }
            .buttonStyle(.secondaryAction)
            .frame(width: 170)
            .accessibilityIdentifier("conversation.composer.providers.connect")
        }
        .padding(.horizontal, Theme.s10)
        .padding(.vertical, Theme.s8)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.input)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
        .accessibilityIdentifier("conversation.composer.providers.readiness")
    }

    private var executionModeMenu: some View {
        Menu {
            ForEach([ExecutionMode.worktree, .local, .readOnly], id: \.self) { mode in
                Button {
                    updateSettings { $0.executionMode = mode }
                } label: {
                    Label(mode.displayLabel, systemImage: settings.executionMode == mode ? "checkmark" : mode.systemImage)
                }
            }
        } label: {
            settingChip(icon: settings.executionMode.systemImage, text: settings.executionMode.displayLabel)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityIdentifier("conversation.composer.settings.mode")
    }

    private var reasoningMenu: some View {
        Menu {
            ForEach(ReasoningEffort.allCases, id: \.self) { effort in
                Button {
                    updateSettings { $0.reasoningEffort = effort }
                } label: {
                    Label(effort.displayLabel, systemImage: settings.reasoningEffort == effort ? "checkmark" : "brain")
                }
            }
        } label: {
            settingChip(icon: "brain", text: settings.reasoningEffort.displayLabel)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityIdentifier("conversation.composer.settings.reasoning")
    }

    private var pipelineMenu: some View {
        Menu {
            ForEach(["auto", "parallel-build"], id: \.self) { pipeline in
                Button {
                    updateSettings { $0.pipelineId = pipeline }
                } label: {
                    Label(pipelineLabel(pipeline), systemImage: settings.pipelineId == pipeline ? "checkmark" : "point.3.connected.trianglepath.dotted")
                }
            }
        } label: {
            settingChip(icon: "point.3.connected.trianglepath.dotted", text: pipelineLabel(settings.pipelineId))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityIdentifier("conversation.composer.settings.pipeline")
    }

    private var parallelismMenu: some View {
        Menu {
            ForEach([1, 2, 4, 8, 16], id: \.self) { count in
                Button {
                    updateSettings { $0.maxParallelism = UInt32(count) }
                } label: {
                    Label("\(count)", systemImage: settings.maxParallelism == UInt32(count) ? "checkmark" : "square.stack.3d.up")
                }
            }
        } label: {
            settingChip(icon: "square.stack.3d.up", text: "x\(settings.maxParallelism)")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityIdentifier("conversation.composer.settings.parallelism")
    }

    private var toolPolicyMenu: some View {
        Menu {
            ForEach(["project-default", "read-only"], id: \.self) { policy in
                Button {
                    updateSettings { $0.toolPolicyId = policy }
                } label: {
                    Label(toolPolicyLabel(policy), systemImage: settings.toolPolicyId == policy ? "checkmark" : "shield")
                }
            }
        } label: {
            settingChip(icon: "shield", text: toolPolicyLabel(settings.toolPolicyId))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityIdentifier("conversation.composer.settings.tools")
    }

    private func settingChip(icon: String, text: String) -> some View {
        Label(text, systemImage: icon)
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

    private var modelLabel: String {
        switch settings.modelSelection.mode {
        case .auto: return "Auto"
        case .pinned:
            guard let id = settings.modelSelection.modelId, !id.isEmpty else { return "Pinned" }
            return providers.model(id: id)?.displayName ?? id
        }
    }

    private var imageModelLabel: String {
        guard let id = settings.imageModelId, !id.isEmpty else { return "Auto" }
        return providers.model(id: id)?.displayName ?? id
    }

    private var providerReadinessLabel: String {
        providers.enabledProviderCount == 0 ? "No provider" : "No ready model"
    }

    private var providerSetupActionLabel: String {
        providers.enabledProviderCount == 0 ? "Connect provider" : "Add provider"
    }

    private var providerReadinessDetail: String {
        providers.providerReadinessDetail
    }

    private var sendHelp: String {
        if trimmedDraft.isEmpty { return "Enter a message before sending." }
        if store.isSending(conversationID: conversationID) { return "A send is already in flight." }
        if !providers.hasEligibleTextModel { return providerReadinessDetail }
        if staleContextCount > 0 { return "Attached context changed since capture." }
        return "Start a conversation turn."
    }

    private func pipelineLabel(_ id: String) -> String {
        switch id {
        case "auto": return "Auto"
        case "parallel-build": return "Parallel"
        default: return id
        }
    }

    private func toolPolicyLabel(_ id: String) -> String {
        switch id {
        case "project-default": return "Default"
        case "read-only": return "Read-only"
        default: return id
        }
    }

    private func updateSettings(_ mutate: @escaping (inout ConversationThreadSettings) -> Void) {
        Task { await store.updateThreadSettings(for: conversationID, mutate: mutate) }
    }

    private func send() {
        let text = store.draft(for: conversationID)
        Task { await store.send(conversationID: conversationID, text: text) }
    }
}

private extension ExecutionMode {
    var systemImage: String {
        switch self {
        case .local: return "laptopcomputer"
        case .worktree: return "arrow.triangle.branch"
        case .readOnly: return "lock"
        case .cloud: return "cloud"
        }
    }
}
