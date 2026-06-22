// DesignImportView.swift — the LOCAL design-import surface (PR-039).
//
// The import is LOCAL and HUMAN-REVIEWED. The surface shows:
//   • two header actions —
//       "Open Open Design in Browser" opens the documented Open Design SITE in the
//       user's browser. This is the ONLY outward action, it is USER-INITIATED, and
//       it merely opens a URL (NSWorkspace) — it is NOT an API call and uploads
//       nothing;
//       "Import from File…" opens a file picker for a LOCAL folder or a `.zip`. The
//       chosen path is quarantined + validated; it is NOT promoted.
//   • a list of quarantined imports, each showing its PROVENANCE (source, license,
//     commit). A cleanly-quarantined package offers explicit Approve (promote) or
//     Reject (delete); a REJECTED import shows its reason and offers ONLY Reject —
//     it can never be approved.
//
// Promotion to the registry happens ONLY when the operator clicks Approve. Dark,
// token-driven, full-tile hit areas, fills width (no letterbox). Status + reasons
// are conveyed by icon + label + a semantic token, never colour alone.

import SwiftUI
import AppKit

struct DesignImportView: View {
    @ObservedObject var store: DesignImportStore

    /// Injected so tests / previews can stub the file picker and the URL opener.
    /// In the app these default to the real NSOpenPanel + NSWorkspace.
    var pickLocalSource: () -> (source: String, kind: DesignImportKind)? = DesignImportView.defaultPickLocalSource
    var openURL: (URL) -> Void = { NSWorkspace.shared.open($0) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().overlay(Theme.stroke)
            if let error = store.lastError {
                errorBanner(error)
            }
            if let promotion = store.lastPromotion {
                promotionReceipt(promotion)
            }
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg)
        .opacity(store.isBusy ? 0.7 : 1)
        .animation(.easeInOut(duration: 0.15), value: store.isBusy)
        .accessibilityIdentifier("design.import.view")
    }

    // MARK: - Header (the two actions)

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Image(systemName: "paintpalette")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                Text("Import a Design System")
                    .font(Theme.ui(15, .semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
            }
            Text("Import a local design package (a folder or a .zip). It is quarantined and validated first, then promoted to your registry only after you review and approve it. Nothing is uploaded.")
                .font(Theme.ui(11.5))
                .foregroundStyle(Theme.muted)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: Theme.s10) {
                Button(action: openOpenDesignSite) {
                    Label("Open Open Design in Browser", systemImage: "safari")
                }
                .buttonStyle(.secondaryAction)
                .accessibilityIdentifier("design.import.open-browser")
                .help("Opens the Open Design website in your browser. This only opens a link — nothing is uploaded.")

                Button(action: importFromFile) {
                    Label("Import from File…", systemImage: "square.and.arrow.down")
                }
                .buttonStyle(.primaryAction)
                .disabled(store.isBusy)
                .accessibilityIdentifier("design.import.import-file")
                .help("Choose a local folder or .zip to quarantine and validate.")
            }
        }
        .padding(Theme.s16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.sidebar)
    }

    // MARK: - Content (quarantine list / empty state)

    @ViewBuilder
    private var content: some View {
        if store.entries.isEmpty {
            EmptyStateView(
                headline: "No imports yet",
                detail: "Choose a local folder or .zip to import a design system. It will appear here, quarantined, for you to review before it is promoted.",
                systemImage: "tray"
            )
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.s12) {
                    ForEach(store.entries) { entry in
                        QuarantineCard(
                            entry: entry,
                            isBusy: store.isBusy,
                            onApprove: { Task { await store.approve(id: entry.quarantineId) } },
                            onReject: { Task { await store.reject(id: entry.quarantineId) } }
                        )
                    }
                }
                .padding(Theme.s16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: - Banners

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
                store.lastError = nil
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(Theme.muted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Theme.s16)
        .padding(.vertical, Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.coral.opacity(0.12))
        .accessibilityIdentifier("design.import.error")
    }

    private func promotionReceipt(_ promotion: DesignImportApproveResult) -> some View {
        HStack(spacing: Theme.s8) {
            Image(systemName: "checkmark.seal.fill")
                .foregroundStyle(Theme.accent)
            Text("Promoted to the registry as \(promotion.packageId).")
                .font(Theme.ui(11.5, .medium))
                .foregroundStyle(Theme.text)
            Spacer()
            Button {
                store.dismissPromotion()
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(Theme.muted)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Theme.s16)
        .padding(.vertical, Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.accent.opacity(0.10))
        .accessibilityIdentifier("design.import.promotion")
    }

    // MARK: - Actions

    /// USER-INITIATED: open the documented Open Design SITE in the browser. This
    /// only opens a URL — it is NOT an API call and uploads nothing.
    private func openOpenDesignSite() {
        openURL(DesignImportLinks.openDesignURL)
    }

    /// Open the LOCAL file picker, then quarantine + validate the chosen path. No
    /// promotion happens here — the result lands in the list for review.
    private func importFromFile() {
        guard let picked = pickLocalSource() else { return }
        Task { await store.import(source: picked.source, kind: picked.kind) }
    }

    // MARK: - Default NSOpenPanel picker

    /// The real file picker: a single LOCAL folder OR a `.zip`. Returns the chosen
    /// path + the matching kind (a directory ⇒ `.local`, a `.zip` ⇒ `.archive`),
    /// or nil if the operator cancelled.
    static func defaultPickLocalSource() -> (source: String, kind: DesignImportKind)? {
        let panel = NSOpenPanel()
        panel.title = "Import a Design System"
        panel.prompt = "Quarantine"
        panel.message = "Choose a local folder or a .zip archive to import."
        panel.canChooseDirectories = true
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.zip, .folder, .directory]
        guard panel.runModal() == .OK, let url = panel.url else { return nil }
        var isDir: ObjCBool = false
        FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir)
        let kind: DesignImportKind = isDir.boolValue ? .local : .archive
        return (source: url.path, kind: kind)
    }
}

// MARK: - Quarantine card

/// One quarantined import. Shows its status (icon + label + token), its
/// PROVENANCE (source / license / commit), and — for a rejected import — the
/// rejection reason. A cleanly-quarantined package offers Approve (promote) +
/// Reject (delete); a rejected package offers ONLY Reject. The whole card fills
/// the width; every action is a full-tile hit target.
private struct QuarantineCard: View {
    let entry: DesignQuarantineEntry
    let isBusy: Bool
    let onApprove: () -> Void
    let onReject: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s12) {
            header
            provenanceGrid
            if entry.isRejected { rejectedReasonRow }
            actions
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
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("design.import.card.\(entry.quarantineId)")
    }

    private var header: some View {
        HStack(spacing: Theme.s8) {
            statusPill
            Spacer()
            Text("\(entry.fileCount) file\(entry.fileCount == 1 ? "" : "s") · \(entry.byteSizeDisplay)")
                .font(Theme.mono(10.5))
                .foregroundStyle(Theme.muted)
        }
    }

    private var statusPill: some View {
        HStack(spacing: 5) {
            Image(systemName: entry.status.symbol)
                .font(.system(size: 9, weight: .bold))
            Text(entry.status.label)
                .font(Theme.ui(11, .semibold))
        }
        .foregroundStyle(entry.status.tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(entry.status.tint.opacity(0.14)))
        .overlay(Capsule().strokeBorder(entry.status.tint.opacity(0.3), lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(entry.status.label) status")
    }

    private var provenanceGrid: some View {
        VStack(alignment: .leading, spacing: Theme.s6) {
            provenanceRow(label: "Source", value: entry.provenance.source, systemImage: "shippingbox")
            provenanceRow(label: "License", value: entry.provenance.licenseDisplay, systemImage: "checkmark.shield")
            provenanceRow(label: "Commit", value: entry.provenance.commitDisplay, systemImage: "number")
        }
        .accessibilityIdentifier("design.import.provenance.\(entry.quarantineId)")
    }

    private func provenanceRow(label: String, value: String, systemImage: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.s8) {
            Label {
                Text(label)
                    .font(Theme.ui(10.5, .semibold))
                    .foregroundStyle(Theme.muted)
            } icon: {
                Image(systemName: systemImage)
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.muted)
            }
            .frame(width: 84, alignment: .leading)
            Text(value)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.textSoft)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    private var rejectedReasonRow: some View {
        let reason = entry.rejectedReason ?? .unknown
        return HStack(alignment: .top, spacing: Theme.s8) {
            Image(systemName: reason.symbol)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.coral)
            VStack(alignment: .leading, spacing: 2) {
                Text(reason.label)
                    .font(Theme.ui(11.5, .semibold))
                    .foregroundStyle(Theme.text)
                Text(reason.message)
                    .font(Theme.ui(11))
                    .foregroundStyle(Theme.textSoft)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(Theme.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.rSm, style: .continuous)
                .fill(Theme.coral.opacity(0.10))
        )
        .accessibilityIdentifier("design.import.rejected-reason.\(entry.quarantineId)")
    }

    private var actions: some View {
        HStack(spacing: Theme.s10) {
            if entry.status.isApprovable {
                Button(action: onApprove) {
                    Label("Approve & Promote", systemImage: "checkmark.seal")
                }
                .buttonStyle(.primaryAction)
                .disabled(isBusy)
                .accessibilityIdentifier("design.import.approve.\(entry.quarantineId)")
                .help("Promote this reviewed package to your design-system registry.")
            }
            Button(action: onReject) {
                Label("Reject", systemImage: "trash")
            }
            .buttonStyle(.destructiveAction)
            .disabled(isBusy)
            .accessibilityIdentifier("design.import.reject.\(entry.quarantineId)")
            .help("Delete this quarantined package. It is never promoted.")
        }
    }
}
