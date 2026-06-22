// VaultView.swift — the opt-in encrypted-vault panel (PR-042).
//
// The panel exposes the FOUR vault operations, each clearly labelled:
//   1. Configure the age PUBLIC recipient (stored via the Keychain). Only a public
//      key is ever accepted — never a private identity.
//   2. Export a SANITIZED, git-trackable summary (decisions + run links, NO raw
//      transcript). The receipt shows the explicit `contains_raw_transcript:false`
//      honesty indicator.
//   3. Encrypt the FULL transcript into a `.age` vault for the recipient — clearly
//      labelled OPT-IN. A failure is shown plainly and NEVER reveals any
//      plaintext/ciphertext.
//   4. IMPORT a `.age` vault: a file picker for the vault + the identity file. The
//      import REQUIRES the identity (private key on disk the app never reads). On
//      success the recovered vault's PROVENANCE is shown; on failure the error is
//      surfaced and NO plaintext is presented.
//
// Dark, token-driven, full-tile hit areas, fills width (no letterbox). Status is
// conveyed by icon + label + a semantic token, never colour alone. The pickers +
// URL open are injected so tests/previews can stub them.

import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct VaultView: View {
    @ObservedObject var store: VaultStore

    /// The conversation whose summary/transcript the encrypt + export actions act
    /// on. Optional: with no active conversation those actions are disabled with an
    /// honest hint.
    var activeConversationID: String?

    /// Injected so tests / previews can stub the file pickers. In the app these
    /// default to the real NSOpenPanel.
    var pickVaultFile: () -> String? = VaultView.defaultPickVaultFile
    var pickIdentityFile: () -> String? = VaultView.defaultPickIdentityFile

    /// Local draft for the recipient public-key field.
    @State private var recipientDraft: String = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                header
                Divider().overlay(Theme.stroke)
                if let error = store.lastError {
                    errorBanner(error)
                }
                VStack(alignment: .leading, spacing: Theme.s16) {
                    recipientSection
                    summarySection
                    encryptSection
                    importSection
                    inventorySection
                }
                .padding(Theme.s16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .opacity(store.isBusy ? 0.7 : 1)
        .animation(.easeInOut(duration: 0.15), value: store.isBusy)
        .accessibilityIdentifier("vault.view")
        .onAppear { recipientDraft = store.recipient?.publicKey ?? "" }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "lock.shield")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text("Encrypted Vault & History")
                    .font(Theme.ui(15, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
            }
            Text("Export a sanitized, git-trackable summary (no raw transcript), or opt in to encrypt the full transcript into an age vault for a recipient. Importing a vault always requires the matching identity key — without it, nothing can be read.")
                .font(Theme.ui(11.5))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Theme.s16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.sidebar)
    }

    // MARK: - Recipient (PUBLIC key only)

    private var recipientSection: some View {
        SectionCard(
            title: "Recipient (public key)",
            systemImage: "key",
            subtitle: "The age x25519 PUBLIC key a vault is encrypted for. Stored in your Keychain. Only a PUBLIC recipient is kept here — never a private identity."
        ) {
            VStack(alignment: .leading, spacing: Theme.s10) {
                if let recipient = store.recipient {
                    HStack(spacing: Theme.s8) {
                        StatusPill(kind: .success, label: "Configured")
                        Text(recipient.redactedPublicKey)
                            .font(Theme.mono(11))
                            .foregroundStyle(Theme.textSoft)
                            .textSelection(.enabled)
                        Spacer()
                        Button("Forget") { store.clearRecipient(); recipientDraft = "" }
                            .buttonStyle(.quietAction)
                            .frame(maxWidth: 110)
                            .accessibilityIdentifier("vault.recipient.forget")
                    }
                } else {
                    StatusPill(kind: .neutral, label: "Not configured")
                }

                TextField("age1…", text: $recipientDraft)
                    .textFieldStyle(.plain)
                    .font(Theme.mono(12))
                    .foregroundStyle(Theme.text)
                    .padding(Theme.s10)
                    .background(
                        RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous).fill(Theme.input)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                            .strokeBorder(Theme.stroke, lineWidth: 1)
                    )
                    .accessibilityIdentifier("vault.recipient.field")

                Button {
                    _ = store.configureRecipient(publicKey: recipientDraft)
                } label: {
                    Label("Save public recipient", systemImage: "checkmark.seal")
                }
                .buttonStyle(.primaryAction)
                .disabled(recipientDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("vault.recipient.save")
                .help("Save the age PUBLIC key. A private key is never accepted here.")
            }
        }
    }

    // MARK: - Export summary (sanitized)

    private var summarySection: some View {
        SectionCard(
            title: "Sanitized summary",
            systemImage: "doc.text",
            subtitle: "Decisions + run links only — no raw transcript, no secrets. Safe to commit to git."
        ) {
            VStack(alignment: .leading, spacing: Theme.s10) {
                Button {
                    guard let id = activeConversationID else { return }
                    Task { await store.exportSummary(conversationID: id) }
                } label: {
                    Label("Export git-trackable summary", systemImage: "square.and.arrow.up")
                }
                .buttonStyle(.secondaryAction)
                .disabled(activeConversationID == nil || store.isBusy)
                .accessibilityIdentifier("vault.summary.export")
                .help(activeConversationID == nil ? "Open a conversation to export its summary." : "Write a sanitized, git-trackable summary.")

                if let summary = store.lastSummary {
                    summaryReceipt(summary)
                }
            }
        }
    }

    private func summaryReceipt(_ summary: VaultSummary) -> some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                // The honesty indicator: no raw transcript ⇒ safe to commit.
                StatusPill(
                    kind: summary.isGitTrackable ? .success : .danger,
                    label: summary.containsRawTranscript ? "Raw transcript present" : "No raw transcript"
                )
                Spacer()
                Button {
                    store.dismissSummary()
                } label: {
                    Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.muted)
                }
                .buttonStyle(.plain)
            }
            metaRow(label: "Summary", value: summary.summaryPath, systemImage: "doc")
            metaRow(label: "Decisions", value: String(summary.decisions), systemImage: "checklist")
            metaRow(
                label: "Run links",
                value: summary.runLinks.isEmpty ? "—" : summary.runLinks.joined(separator: ", "),
                systemImage: "link"
            )
            Text(summary.safetyLabel)
                .font(Theme.ui(11, .medium))
                .foregroundStyle(summary.isGitTrackable ? Theme.accent : Theme.coral)
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous).fill(Theme.accent.opacity(0.08))
        )
        .accessibilityIdentifier("vault.summary.receipt")
    }

    // MARK: - Encrypt (opt-in, full transcript)

    private var encryptSection: some View {
        SectionCard(
            title: "Encrypt full transcript (opt-in)",
            systemImage: "lock.fill",
            subtitle: "Optionally encrypt the ENTIRE transcript into an .age vault for the configured recipient. This is opt-in and writes an encrypted file only."
        ) {
            VStack(alignment: .leading, spacing: Theme.s10) {
                Button {
                    guard let id = activeConversationID else { return }
                    Task { await store.encrypt(conversationID: id) }
                } label: {
                    Label("Encrypt transcript to vault", systemImage: "lock.rectangle.stack")
                }
                .buttonStyle(.primaryAction)
                .disabled(activeConversationID == nil || !store.canEncrypt || store.isBusy)
                .accessibilityIdentifier("vault.encrypt.run")
                .help(encryptHint)

                if let receipt = store.lastEncrypt {
                    encryptReceipt(receipt)
                }
            }
        }
    }

    private var encryptHint: String {
        if activeConversationID == nil { return "Open a conversation to encrypt its transcript." }
        if !store.canEncrypt { return "Configure an age PUBLIC recipient first." }
        return "Encrypt the full transcript into an .age vault for the configured recipient."
    }

    private func encryptReceipt(_ receipt: VaultEncryptResult) -> some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                StatusPill(kind: .success, label: "Encrypted")
                Spacer()
                Button {
                    store.dismissEncryptReceipt()
                } label: {
                    Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.muted)
                }
                .buttonStyle(.plain)
            }
            metaRow(label: "Vault", value: receipt.vaultPath, systemImage: "lock.doc")
            metaRow(label: "Recipient", value: receipt.recipientRedacted, systemImage: "key")
            metaRow(label: "Size", value: "\(receipt.bytes) bytes", systemImage: "number")
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous).fill(Theme.accent.opacity(0.08))
        )
        .accessibilityIdentifier("vault.encrypt.receipt")
    }

    // MARK: - Import (.age vault + identity file)

    private var importSection: some View {
        SectionCard(
            title: "Import a vault",
            systemImage: "tray.and.arrow.down",
            subtitle: "Open an .age vault using your identity (private age key). Without the matching identity, nothing can be read."
        ) {
            VStack(alignment: .leading, spacing: Theme.s10) {
                HStack(spacing: Theme.s10) {
                    Button {
                        importVault()
                    } label: {
                        Label("Choose vault + identity to import…", systemImage: "tray.and.arrow.down")
                    }
                    .buttonStyle(.secondaryAction)
                    .disabled(store.isBusy)
                    .accessibilityIdentifier("vault.import.choose")
                    .help("Pick a .age vault and your identity file. The identity is required to read it.")
                }

                if let provenance = store.lastImport {
                    importProvenance(provenance)
                }
            }
        }
    }

    private func importProvenance(_ provenance: VaultImportProvenance) -> some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                StatusPill(kind: .success, label: "Imported")
                Spacer()
                Button {
                    store.dismissImport()
                } label: {
                    Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.muted)
                }
                .buttonStyle(.plain)
            }
            metaRow(label: "From vault", value: provenance.vaultName, systemImage: "lock.doc")
            metaRow(label: "Conversation", value: provenance.conversationId, systemImage: "bubble.left.and.bubble.right")
            metaRow(label: "Size", value: "\(provenance.bytes) bytes", systemImage: "number")
            Text("Provenance only — the recovered transcript is imported into the workspace, not shown here.")
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Theme.s12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous).fill(Theme.accent.opacity(0.08))
        )
        .accessibilityIdentifier("vault.import.provenance")
    }

    // MARK: - Inventory (status)

    @ViewBuilder
    private var inventorySection: some View {
        SectionCard(
            title: "Workspace inventory",
            systemImage: "archivebox",
            subtitle: "Sanitized summaries and the .age vaults present in this workspace (recipients redacted)."
        ) {
            if store.status.summaries.isEmpty && store.status.vaults.isEmpty {
                Text("No summaries or vaults yet.")
                    .font(Theme.ui(11.5))
                    .foregroundStyle(Theme.muted)
            } else {
                VStack(alignment: .leading, spacing: Theme.s8) {
                    ForEach(store.status.summaries) { summary in
                        HStack(spacing: Theme.s8) {
                            Image(systemName: "doc.text").font(.system(size: 11)).foregroundStyle(Theme.muted)
                            Text(summary.conversationId)
                                .font(Theme.mono(11)).foregroundStyle(Theme.textSoft)
                            Spacer()
                            StatusPill(
                                kind: summary.isGitTrackable ? .success : .danger,
                                label: summary.containsRawTranscript ? "raw" : "sanitized"
                            )
                        }
                    }
                    ForEach(store.status.vaults) { vault in
                        HStack(spacing: Theme.s8) {
                            Image(systemName: "lock.doc").font(.system(size: 11)).foregroundStyle(Theme.muted)
                            Text((vault.path as NSString).lastPathComponent)
                                .font(Theme.mono(11)).foregroundStyle(Theme.textSoft)
                            Spacer()
                            Text(vault.recipientRedacted)
                                .font(Theme.mono(10.5)).foregroundStyle(Theme.muted)
                        }
                    }
                }
                .accessibilityIdentifier("vault.inventory.list")
            }
        }
    }

    // MARK: - Shared rows / banners

    private func metaRow(label: String, value: String, systemImage: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.s8) {
            Label {
                Text(label).font(Theme.ui(10.5, .semibold)).foregroundStyle(Theme.muted)
            } icon: {
                Image(systemName: systemImage).font(.system(size: 10)).foregroundStyle(Theme.muted)
            }
            .frame(width: 96, alignment: .leading)
            Text(value)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Theme.coral)
            Text(message)
                .font(Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Button {
                store.dismissError()
            } label: {
                Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.muted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Theme.s16)
        .padding(.vertical, Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.coral.opacity(0.12))
        .accessibilityIdentifier("vault.error")
    }

    // MARK: - Actions

    /// Pick a `.age` vault + an identity file, then import. Either picker returning
    /// nil (operator cancelled) aborts without a call. The identity is REQUIRED —
    /// the store also refuses a missing one.
    private func importVault() {
        guard let vaultPath = pickVaultFile() else { return }
        guard let identityPath = pickIdentityFile() else { return }
        Task { await store.decrypt(vault: vaultPath, identityFile: identityPath) }
    }

    // MARK: - Default pickers

    /// Pick a single `.age` vault file.
    static func defaultPickVaultFile() -> String? {
        let panel = NSOpenPanel()
        panel.title = "Choose an .age vault"
        panel.prompt = "Choose Vault"
        panel.message = "Select the encrypted .age vault to import."
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        if let ageType = UTType(filenameExtension: "age") {
            panel.allowedContentTypes = [ageType]
        }
        guard panel.runModal() == .OK, let url = panel.url else { return nil }
        return url.path
    }

    /// Pick the identity FILE (the private age key on disk). The app passes its PATH
    /// to the CLI's `--identity-file`; it never reads or stores the file's contents.
    static func defaultPickIdentityFile() -> String? {
        let panel = NSOpenPanel()
        panel.title = "Choose your identity file"
        panel.prompt = "Choose Identity"
        panel.message = "Select your age identity file (your private key). It is used only to open this vault and is never stored by the app."
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return nil }
        return url.path
    }
}

// MARK: - Section card

/// A titled card section — title + subtitle + content. The whole card fills the
/// width; content is leading-aligned. Mirrors the QuarantineCard surface treatment.
private struct SectionCard<Content: View>: View {
    let title: String
    let systemImage: String
    let subtitle: String
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            HStack(spacing: Theme.s8) {
                Image(systemName: systemImage)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text(title)
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
            }
            Text(subtitle)
                .font(Theme.ui(11))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)
            content()
        }
        .padding(Theme.s16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .fill(Theme.panel)
        )
        .overlay(
            RoundedRectangle(cornerRadius: GeneratedDesignTokens.radiusCard, style: .continuous)
                .strokeBorder(Theme.stroke, lineWidth: 1)
        )
    }
}
