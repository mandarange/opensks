import SwiftUI

struct ProviderDiagnosticsView: View {
    @ObservedObject var store: ProviderStore
    let provider: ProviderConnectionViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s10) {
            HStack(spacing: Theme.s8) {
                Label("Diagnostics", systemImage: "stethoscope")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer(minLength: 0)
                Button {
                    Task { try? await store.probeProvider(provider.id) }
                } label: {
                    Label("Run probe", systemImage: "waveform.path.ecg")
                }
                .buttonStyle(.secondaryAction)
                .frame(width: 128)
                .disabled(!provider.enabled)
                .accessibilityIdentifier("settings.providers.diagnostics.probe.\(provider.id)")
            }
            diagnosticRow("Health", provider.statusLabel, provider.statusPillKind)
            diagnosticRow("Last check", provider.lastCheckedLabel, .neutral)
            diagnosticRow("Circuit", provider.circuitLabel, provider.circuitOpen ? .warning : .success)
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
            if !provider.adapterBlockers.isEmpty {
                VStack(alignment: .leading, spacing: Theme.s6) {
                    ForEach(provider.adapterBlockers, id: \.self) { blocker in
                        Label(blocker, systemImage: "exclamationmark.circle")
                            .font(Theme.mono(11))
                            .foregroundStyle(Theme.gold)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .accessibilityIdentifier("settings.providers.adapterBlockers")
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
        .accessibilityIdentifier("settings.providers.diagnostics")
    }

    private func diagnosticRow(_ label: String, _ value: String, _ kind: StatusPill.Kind) -> some View {
        HStack(spacing: Theme.s8) {
            Text(label)
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.muted)
                .frame(width: 92, alignment: .leading)
            StatusPill(kind: kind, label: value)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }
}
