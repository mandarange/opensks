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
                runProbeButton
            }
            diagnosticRow("Health", provider.statusLabel, provider.statusPillKind)
            diagnosticRow("Last check", provider.lastCheckedLabel, .neutral)
            diagnosticRow("Circuit", provider.circuitLabel, provider.circuitOpen ? .warning : .success)
            Text(provider.lastDiagnostic)
                .font(Theme.ui(12))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("settings.providers.diagnostic")
            runAdapterCheckButton
            if let adapterReportGeneratedAt = store.adapterCheckReportGeneratedAtLabel {
                diagnosticRow("Live check", adapterReportGeneratedAt, .neutral)
                    .accessibilityIdentifier("settings.providers.adapterReportGeneratedAt")
            }
            if let adapterReportSummary = store.adapterCheckReportSummaryDetail {
                Label(adapterReportSummary, systemImage: "antenna.radiowaves.left.and.right")
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textSoft)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("settings.providers.adapterReportSummary")
            }
            if let adapterCheckActionDetail = store.adapterCheckActionDetail {
                Label(adapterCheckActionDetail, systemImage: "exclamationmark.circle")
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.gold)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("settings.providers.adapterActionSummary")
            }
            if !store.adapterCheckRemediationActions.isEmpty {
                VStack(alignment: .leading, spacing: Theme.s6) {
                    Text("Provider check actions")
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(Theme.muted)
                    ForEach(store.adapterCheckRemediationActions, id: \.blocker) { action in
                        remediationRow(action)
                    }
                }
                .accessibilityIdentifier("settings.providers.adapterRemediationActions")
            }
            if let adapterDiagnostic = provider.adapterDiagnostic {
                Label(adapterDiagnostic, systemImage: "exclamationmark.triangle.fill")
                    .font(Theme.ui(11.5))
                    .foregroundStyle(provider.adapterBlockers.isEmpty ? Theme.muted : Theme.gold)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("settings.providers.adapterDiagnostic")
            }
            if let adapterCheckGeneratedAt = provider.adapterCheckGeneratedAtLabel {
                diagnosticRow("Adapter check", adapterCheckGeneratedAt, .neutral)
                    .accessibilityIdentifier("settings.providers.adapterCheckGeneratedAt")
            }
            if let adapterCheckDetail = provider.adapterCheckDetail {
                Label(adapterCheckDetail, systemImage: "network")
                    .font(Theme.mono(11))
                    .foregroundStyle(Theme.textSoft)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("settings.providers.adapterCheckDetail")
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

    private var runAdapterCheckButton: some View {
        Button {
            Task { try? await store.runAdapterCheck() }
        } label: {
            Label("Run provider check", systemImage: "powerplug")
        }
        .buttonStyle(.secondaryAction)
        .frame(width: 190)
        .disabled(store.syncState != .idle)
        .accessibilityIdentifier("settings.providers.diagnostics.adapterCheck")
        .help("Run the live provider adapter check and refresh this diagnostics card.")
    }

    private var runProbeButton: some View {
        Button {
            Task { try? await store.probeProvider(provider.id) }
        } label: {
            Label("Run probe", systemImage: "waveform.path.ecg")
        }
        .buttonStyle(.secondaryAction)
        .frame(width: 128)
        .disabled(!provider.enabled || store.syncState != .idle)
        .accessibilityIdentifier("settings.providers.diagnostics.probe.\(provider.id)")
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

    private func remediationRow(_ action: ProviderAdapterRemediationAction) -> some View {
        HStack(alignment: .top, spacing: Theme.s8) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.accent)
                .frame(width: 14, height: 16)
            VStack(alignment: .leading, spacing: 2) {
                Text(action.scope)
                    .font(Theme.ui(10.5, .medium))
                    .foregroundStyle(Theme.muted)
                Text(action.action)
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.textSoft)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(action.scope): \(action.action)")
    }
}
