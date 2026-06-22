// DesignStudioStore.swift — the @MainActor owner of the Design Studio state
// (PR-040).
//
// Holds the catalog of packages (the sidebar), the selected package, the active
// package + revision (from `active-status`), the most recent audit report, and the
// per-package revisions. The flow is HONEST and ATOMIC:
//   • `audit(package:)`    — runs the audit rules and surfaces the report
//                            (grouped findings, a clear blocked indicator).
//   • `activate(package:)` — ATOMIC: the CLI audits first. A FAILING audit BLOCKS
//                            the activation: the store surfaces a BLOCKED state with
//                            the blocking findings, leaves the SHOWN active package
//                            unchanged, and does NOT report success. Only a PASSING
//                            audit moves the active package (then `active-status` is
//                            re-read to confirm).
//   • the revision lifecycle (propose / accept / reject / rollback) transitions the
//     shown revision state; each revision is LINKED TO A PROOF (`proof_ref`).
//
// State is observable so the view (and tests) can read the active package, the
// audit, the blocked state, and the revisions directly.

import SwiftUI

@MainActor
final class DesignStudioStore: ObservableObject {
    // MARK: Published state

    /// The catalog of packages shown in the sidebar.
    @Published private(set) var catalog: [DesignPackage] = []
    /// The package id currently selected in the sidebar (drives the detail tabs).
    @Published var selectedPackageId: String?

    /// Which package (if any) is currently active, and the revision it was
    /// activated at. This is the SHOWN active package — a blocked activation never
    /// changes it.
    @Published private(set) var active: DesignActiveStatus = .none

    /// The most recent audit report, keyed by package id, so the Audit tab shows
    /// the report for the selected package.
    @Published private(set) var auditByPackage: [String: DesignAuditReport] = [:]

    /// A BLOCKED-activation state: set when an `activate` was refused because its
    /// audit failed. Carries the package the operator TRIED to activate + the
    /// blocking findings, so the view shows the failure clearly. Non-nil ⇒ the
    /// activation did NOT happen and the shown active package is unchanged.
    @Published private(set) var activationBlock: DesignActivationBlock?

    /// The revisions per package id (newest first), so the Revisions tab lists them.
    @Published private(set) var revisionsByPackage: [String: [DesignRevision]] = [:]

    /// The editable token drafts per package id (the Tokens tab edits these). Seeded
    /// from the catalog package's tokens; edits stay local to the draft.
    @Published private(set) var tokenDraftsByPackage: [String: [DesignTokenEntry]] = [:]

    /// True while a service call is in flight (the view dims / disables actions).
    @Published private(set) var isBusy = false
    /// A non-fatal banner for the last failed operation.
    @Published var lastError: String?

    private var service: DesignStudioService

    init(service: DesignStudioService, catalog: [DesignPackage] = []) {
        self.service = service
        self.catalog = catalog
        self.selectedPackageId = catalog.first?.packageId
        for package in catalog {
            tokenDraftsByPackage[package.packageId] = package.tokens
        }
    }

    // MARK: - Rebinding / catalog

    /// Swap the live service (e.g. once the real workspace + bundled CLI resolve)
    /// and re-read the active status.
    func rebind(service: DesignStudioService) {
        self.service = service
        Task { await refreshActiveStatus() }
    }

    /// Replace the catalog (e.g. once the registry listing is known) and seed token
    /// drafts. Preserves the selection if it still exists.
    func setCatalog(_ catalog: [DesignPackage]) {
        self.catalog = catalog
        for package in catalog where tokenDraftsByPackage[package.packageId] == nil {
            tokenDraftsByPackage[package.packageId] = package.tokens
        }
        if let selected = selectedPackageId,
           !catalog.contains(where: { $0.packageId == selected }) {
            selectedPackageId = catalog.first?.packageId
        } else if selectedPackageId == nil {
            selectedPackageId = catalog.first?.packageId
        }
    }

    /// Select a package in the sidebar (drives the detail tabs).
    func select(_ packageId: String) {
        selectedPackageId = packageId
    }

    // MARK: - Derived

    /// The currently-selected package, if any.
    var selectedPackage: DesignPackage? {
        guard let selectedPackageId else { return nil }
        return catalog.first { $0.packageId == selectedPackageId }
    }

    /// The audit report for the selected package, if one has been run.
    var selectedAudit: DesignAuditReport? {
        guard let selectedPackageId else { return nil }
        return auditByPackage[selectedPackageId]
    }

    /// The revisions for the selected package (newest first).
    var selectedRevisions: [DesignRevision] {
        guard let selectedPackageId else { return [] }
        return revisionsByPackage[selectedPackageId] ?? []
    }

    /// The editable token drafts for the selected package.
    var selectedTokens: [DesignTokenEntry] {
        guard let selectedPackageId else { return [] }
        return tokenDraftsByPackage[selectedPackageId] ?? []
    }

    /// True when the given package is the SHOWN active one.
    func isActive(_ packageId: String) -> Bool {
        active.activePackage == packageId
    }

    // MARK: - Active status

    /// Re-read the active package + revision from the CLI (state recovered after a
    /// relaunch). A read failure is non-fatal — keep the last known status.
    func refreshActiveStatus() async {
        do {
            active = try await service.activeStatus()
        } catch {
            lastError = Self.describe(error)
        }
    }

    // MARK: - Audit

    /// Run the audit rules over a package and surface the report (grouped findings +
    /// a clear blocked indicator). This does NOT activate.
    @discardableResult
    func audit(package packageId: String) async -> DesignAuditReport? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let report = try await service.audit(packageId: packageId)
            auditByPackage[packageId] = report
            return report
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Activate (ATOMIC — a failing audit BLOCKS and keeps the prev active)

    /// Attempt to activate a package. The CLI is ATOMIC: it audits first. A FAILING
    /// audit BLOCKS the activation — this surfaces a `activationBlock` with the
    /// blocking findings, records the failing audit so the Audit tab shows it,
    /// leaves the SHOWN active package UNCHANGED, and returns nil (NOT a success).
    /// Only a PASSING audit moves the active package; `active-status` is re-read to
    /// confirm the new active package.
    @discardableResult
    func activate(package packageId: String) async -> DesignActivateResult? {
        lastError = nil
        activationBlock = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let result = try await service.activate(packageId: packageId)
            // PASSED: the package activated. Reflect it immediately, then confirm
            // from active-status.
            active = DesignActiveStatus(
                activePackage: result.packageId,
                activatedRevision: active.activatedRevision
            )
            await refreshActiveStatus()
            return result
        } catch let error as DesignStudioServiceError {
            if case .auditFailed(let findings) = error {
                // BLOCKED: the audit failed. The activation did NOT happen — the
                // shown active package is untouched. Surface the blocked state +
                // record the failing audit for the Audit tab.
                let report = DesignAuditReport(
                    packageId: packageId,
                    passed: false,
                    blocksActivation: true,
                    findings: findings
                )
                auditByPackage[packageId] = report
                activationBlock = DesignActivationBlock(
                    blockedPackageId: packageId,
                    keptActivePackageId: active.activePackage,
                    findings: findings
                )
                return nil
            }
            lastError = Self.describe(error)
            return nil
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    /// Dismiss the blocked-activation banner after the operator has read it (they
    /// fix the package + re-audit; there is no force path).
    func dismissActivationBlock() {
        activationBlock = nil
    }

    // MARK: - Revisions (proof-linked lifecycle)

    /// Propose a revision of a package — the new revision is `proposed` and LINKED
    /// TO A PROOF. It is inserted at the front of the package's revision list.
    @discardableResult
    func proposeRevision(package packageId: String) async -> DesignRevision? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let revision = try await service.proposeRevision(packageId: packageId)
            upsertRevision(revision, package: packageId)
            return revision
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    /// Accept a proposed revision — transitions the shown state to `accepted`.
    @discardableResult
    func acceptRevision(_ revisionId: String, package packageId: String) async -> DesignRevision? {
        await transitionRevision(revisionId, package: packageId) {
            try await self.service.acceptRevision(revisionId: revisionId)
        }
    }

    /// Reject a proposed revision — transitions the shown state to `rejected`.
    @discardableResult
    func rejectRevision(_ revisionId: String, package packageId: String) async -> DesignRevision? {
        await transitionRevision(revisionId, package: packageId) {
            try await self.service.rejectRevision(revisionId: revisionId)
        }
    }

    /// Roll back a revision — transitions the shown state to `rolled_back`.
    @discardableResult
    func rollbackRevision(_ revisionId: String, package packageId: String) async -> DesignRevision? {
        await transitionRevision(revisionId, package: packageId) {
            try await self.service.rollbackRevision(revisionId: revisionId)
        }
    }

    private func transitionRevision(
        _ revisionId: String,
        package packageId: String,
        _ call: @escaping () async throws -> DesignRevision
    ) async -> DesignRevision? {
        lastError = nil
        isBusy = true
        defer { isBusy = false }
        do {
            let updated = try await call()
            // The service may not echo the package id on a transition; keep the one
            // we already know so the revision stays grouped under its package.
            let merged = DesignRevision(
                revisionId: updated.revisionId.isEmpty ? revisionId : updated.revisionId,
                packageId: updated.packageId.isEmpty ? packageId : updated.packageId,
                state: updated.state,
                proofRef: updated.proofRef
            )
            upsertRevision(merged, package: packageId)
            return merged
        } catch {
            lastError = Self.describe(error)
            return nil
        }
    }

    // MARK: - Tokens (the Tokens-tab editor)

    /// Edit a token's value in the selected package's draft. The studio is the
    /// source of truth; this edits the in-UI draft so the editor is live.
    func setTokenValue(_ value: String, forPath path: String, package packageId: String) {
        var drafts = tokenDraftsByPackage[packageId] ?? []
        if let index = drafts.firstIndex(where: { $0.path == path }) {
            drafts[index].value = value
            tokenDraftsByPackage[packageId] = drafts
        }
    }

    // MARK: - Internals

    /// Insert-or-replace a revision by id at the front of its package's list.
    private func upsertRevision(_ revision: DesignRevision, package packageId: String) {
        var list = revisionsByPackage[packageId] ?? []
        if let index = list.firstIndex(where: { $0.revisionId == revision.revisionId }) {
            list[index] = revision
        } else {
            list.insert(revision, at: 0)
        }
        revisionsByPackage[packageId] = list
    }

    private static func describe(_ error: Error) -> String {
        if let studioError = error as? DesignStudioServiceError {
            switch studioError {
            case .transport(let m), .service(let m):
                return m
            case .auditFailed(let findings):
                let names = findings.compactMap(\.ref).joined(separator: ", ")
                return names.isEmpty
                    ? "The audit failed; activation is blocked until the findings are resolved."
                    : "The audit failed (\(names)); activation is blocked until the findings are resolved."
            }
        }
        return error.localizedDescription
    }
}

// MARK: - Blocked activation state

/// A blocked activation: the package the operator TRIED to activate, the package
/// that REMAINS active (unchanged), and the blocking findings. Surfaced by the view
/// as a clear failure — the activation did not happen.
struct DesignActivationBlock: Sendable, Equatable {
    let blockedPackageId: String
    let keptActivePackageId: String?
    let findings: [DesignAuditFinding]

    /// The kept active package to display, falling back to an honest "None".
    var keptActiveDisplay: String { keptActivePackageId ?? "None" }
    /// The blocking error findings (what made the audit fail).
    var errors: [DesignAuditFinding] { findings.filter { $0.severity == .error } }
}
