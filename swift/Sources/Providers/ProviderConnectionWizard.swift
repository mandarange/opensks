import SwiftUI

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
            statusArea
            Spacer(minLength: 0)
            actions
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

            SecureCredentialField(credential: $credential)

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

    @ViewBuilder
    private var statusArea: some View {
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
    }

    private var actions: some View {
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
