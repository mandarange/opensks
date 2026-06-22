// DesignImportStore.swift — the @MainActor owner of the LOCAL design-import flow
// (PR-039).
//
// Holds the list of quarantined entries and the most recent import result. The
// flow is QUARANTINE → HUMAN REVIEW → PROMOTE:
//   • `import(source:kind:)` quarantines + validates a LOCAL dir / .zip. The
//     result (quarantined OR rejected) is added to the list. This NEVER promotes
//     and NEVER calls approve.
//   • `approve(id:)` is the ONLY promotion path. It is invoked solely from an
//     explicit operator approval in the view, promoting the quarantined package
//     to the registry. A REJECTED entry can never be approved (the store refuses).
//   • `reject(id:)` deletes the quarantine and drops it from the list.
//   • `refreshStatus()` re-reads the quarantine listing from the CLI.
//
// There is NO upload / network method here — the store only ever drives the four
// LOCAL service calls. The user's data is never sent anywhere automatically; the
// only outward action is the view opening the documented Open Design site in the
// browser, which is user-initiated and lives in the view, not here.

import SwiftUI

@MainActor
final class DesignImportStore: ObservableObject {
    // MARK: Published state

    /// The current quarantine list (validated-but-unpromoted + rejected entries).
    @Published private(set) var entries: [DesignQuarantineEntry] = []
    /// The most recent import result, surfaced so the view can immediately show
    /// the freshly-quarantined package's provenance (or its rejection reason).
    @Published private(set) var lastImport: DesignImportResult?
    /// The most recent successful promotion, surfaced as a receipt.
    @Published private(set) var lastPromotion: DesignImportApproveResult?

    /// True while a service call is in flight (the view disables actions / dims).
    @Published private(set) var isBusy = false
    /// A non-fatal banner for the last failed operation.
    @Published var lastError: String?

    private var service: DesignImportService

    init(service: DesignImportService) {
        self.service = service
    }

    // MARK: - Rebinding

    /// Swap the live service (e.g. once the real workspace + bundled CLI are
    /// resolved) and re-read the quarantine listing.
    func rebind(service: DesignImportService) {
        self.service = service
        Task { await refreshStatus() }
    }

    // MARK: - Import (quarantine only — NEVER promotes)

    /// Quarantine + validate a LOCAL dir / .zip. The result is added to the list:
    /// a `quarantined` package awaits explicit human approval; a `rejected` package
    /// is shown with its reason and can NEVER be approved. This NEVER promotes and
    /// NEVER calls approve — promotion is a separate, explicit operator action.
    @discardableResult
    func `import`(source: String, kind: DesignImportKind) async -> DesignImportResult? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let result = try await service.importLocal(source: source, kind: kind)
            lastImport = result
            upsert(DesignQuarantineEntry(from: result))
            return result
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Approve (the ONLY promotion path — explicit human review)

    /// Promote a QUARANTINED package to the registry. This is the sole promotion
    /// path and is only ever called from an explicit operator approval. A package
    /// that is NOT approvable (rejected / unknown / missing) is refused here — the
    /// store never promotes a refused candidate.
    @discardableResult
    func approve(id: String) async -> DesignImportApproveResult? {
        guard let entry = entries.first(where: { $0.quarantineId == id }) else {
            lastError = "That quarantined package is no longer available."
            return nil
        }
        guard entry.status.isApprovable else {
            // A rejected (or unknown) package can NEVER be promoted.
            lastError = "This package was rejected by a safety check and cannot be approved."
            return nil
        }
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let result = try await service.approve(quarantineId: id)
            if result.promoted {
                lastPromotion = result
                // The package left quarantine for the registry — drop it from the list.
                entries.removeAll { $0.quarantineId == id }
                if lastImport?.quarantineId == id { lastImport = nil }
            }
            await refreshStatus()
            return result
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Reject (delete the quarantine)

    /// Safely delete a quarantine directory and drop the entry from the list. The
    /// candidate is discarded — it was never promoted.
    @discardableResult
    func reject(id: String) async -> DesignImportRejectResult? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let result = try await service.reject(quarantineId: id)
            if result.rejected {
                entries.removeAll { $0.quarantineId == id }
                if lastImport?.quarantineId == id { lastImport = nil }
            }
            return result
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Status

    /// Re-read the quarantine listing from the CLI (state recovered after relaunch).
    func refreshStatus() async {
        do {
            let status = try await service.status()
            entries = status.quarantined
        } catch {
            // A status read failure is non-fatal — keep the last known list.
            lastError = Self.describe(error)
        }
    }

    /// Dismiss the most recent promotion receipt after the operator has read it.
    func dismissPromotion() {
        lastPromotion = nil
    }

    // MARK: - Derived

    /// The quarantined-and-approvable entries (awaiting human review).
    var quarantined: [DesignQuarantineEntry] { entries.filter(\.isQuarantined) }
    /// The rejected entries (shown with their reason; not approvable).
    var rejected: [DesignQuarantineEntry] { entries.filter(\.isRejected) }

    // MARK: - Internals

    /// Insert-or-replace an entry by its quarantine id (so a re-import of the same
    /// id updates in place rather than duplicating).
    private func upsert(_ entry: DesignQuarantineEntry) {
        if let index = entries.firstIndex(where: { $0.quarantineId == entry.quarantineId }) {
            entries[index] = entry
        } else {
            entries.insert(entry, at: 0)
        }
    }

    private static func describe(_ error: Error) -> String {
        if let importError = error as? DesignImportServiceError {
            switch importError {
            case .transport(let m), .service(let m):
                return m
            }
        }
        return error.localizedDescription
    }
}
