// PushOutboxModels.swift — Codable mirrors of the SHARED PUSH JSON CONTRACT
// (snake_case) introduced in PR-036. A push is a multi-step, explicitly-approved
// flow — NEVER a silent network call. Each step is a SUBCOMMAND of the existing
// `git` verb (push-enqueue / push-approve / push-execute / push-status); there is
// no new root dispatch arm on either half.
//
// THE INVARIANT: a push always requires the operator to approve the EXACT effect.
//   1. push-enqueue persists an INTENT (remote, ref, local oid, the remote's
//      expected oid, a stable `effect_digest`) durably in SQLite so it survives
//      relaunch.
//   2. push-approve records an APPROVAL only if the supplied `effect_digest`
//      still matches the intent's CURRENT digest. A wrong oid/ref ⇒ the digest
//      no longer matches ⇒ `digest_mismatch`, and NO usable approval is recorded.
//   3. push-execute runs the real `git push` ONLY with a matching approval whose
//      digest has not changed since; a protected branch additionally needs an
//      explicit `--ack-protected`. It is idempotent: a repeat execute with the
//      same evidence reports `already_done:true` and pushes exactly once. A push
//      that fails (e.g. unreachable remote) preserves the LOCAL commit + the
//      pending intent for retry.
//   4. push-status recovers pending / approved / completed from SQLite.
//
// Decoding is tolerant (`.unknown` fallbacks, optional fields default) so a
// future server value never crashes the decoder. There is no secret material on
// the wire: the remote URL is always REDACTED (`remote_url_redacted`).

import SwiftUI

// MARK: - Push intent (push-enqueue result)

/// `opensks.push-intent.v1` — the durable record of a requested push. The
/// `effectDigest` is a stable hash over {redacted remote url, ref, local oid,
/// remote expected oid}; the approval echoes it, and the execute refuses if it
/// has changed (the local oid or ref moved out from under the reviewed effect).
struct GitPushIntent: Codable, Sendable, Equatable, Identifiable {
    let schema: String
    let intentId: String
    let effectDigest: String
    let remote: String
    /// The remote URL with any credentials/host detail REDACTED — what the
    /// operator sees in the approval prompt. Never the raw URL.
    let remoteUrlRedacted: String
    let ref: String
    /// The local commit the push would publish.
    let localOid: String
    /// The oid the remote ref is expected to be at (nil ⇒ a new/unknown remote
    /// ref). Part of the digest so a moved remote invalidates the reviewed effect.
    let remoteExpectedOid: String?
    /// True when `ref` is a PROTECTED branch: approval requires an explicit extra
    /// confirmation (`--ack-protected`), surfaced as a distinct warning.
    let protected: Bool

    var id: String { intentId }

    enum CodingKeys: String, CodingKey {
        case schema
        case intentId = "intent_id"
        case effectDigest = "effect_digest"
        case remote
        case remoteUrlRedacted = "remote_url_redacted"
        case ref
        case localOid = "local_oid"
        case remoteExpectedOid = "remote_expected_oid"
        case protected
    }

    init(
        schema: String = "opensks.push-intent.v1",
        intentId: String,
        effectDigest: String,
        remote: String,
        remoteUrlRedacted: String,
        ref: String,
        localOid: String,
        remoteExpectedOid: String?,
        protected: Bool
    ) {
        self.schema = schema
        self.intentId = intentId
        self.effectDigest = effectDigest
        self.remote = remote
        self.remoteUrlRedacted = remoteUrlRedacted
        self.ref = ref
        self.localOid = localOid
        self.remoteExpectedOid = remoteExpectedOid
        self.protected = protected
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.push-intent.v1"
        intentId = try c.decode(String.self, forKey: .intentId)
        effectDigest = try c.decodeIfPresent(String.self, forKey: .effectDigest) ?? ""
        remote = try c.decodeIfPresent(String.self, forKey: .remote) ?? "origin"
        remoteUrlRedacted = try c.decodeIfPresent(String.self, forKey: .remoteUrlRedacted) ?? ""
        ref = try c.decodeIfPresent(String.self, forKey: .ref) ?? ""
        localOid = try c.decodeIfPresent(String.self, forKey: .localOid) ?? ""
        remoteExpectedOid = try c.decodeIfPresent(String.self, forKey: .remoteExpectedOid)
        protected = try c.decodeIfPresent(Bool.self, forKey: .protected) ?? false
    }

    /// First 8 chars of the local oid for a compact, honest reference.
    var shortLocalOid: String { String(localOid.prefix(8)) }

    /// First 8 chars of the remote-expected oid, or a clear "new ref" word.
    var remoteExpectedLabel: String {
        guard let remoteExpectedOid, !remoteExpectedOid.isEmpty else { return "new ref" }
        return String(remoteExpectedOid.prefix(8))
    }
}

// MARK: - Push approval (push-approve result)

/// `opensks.push-approval.v1` — the record that the operator approved the EXACT
/// effect named by `intentId` (the supplied digest matched the intent's current
/// digest). Only a `matched:true` approval lets a later execute run.
struct GitPushApproval: Codable, Sendable, Equatable, Identifiable {
    let schema: String
    let approvalId: String
    let intentId: String
    let matched: Bool

    var id: String { approvalId }

    enum CodingKeys: String, CodingKey {
        case schema
        case approvalId = "approval_id"
        case intentId = "intent_id"
        case matched
    }

    init(
        schema: String = "opensks.push-approval.v1",
        approvalId: String,
        intentId: String,
        matched: Bool
    ) {
        self.schema = schema
        self.approvalId = approvalId
        self.intentId = intentId
        self.matched = matched
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.push-approval.v1"
        approvalId = try c.decodeIfPresent(String.self, forKey: .approvalId) ?? ""
        intentId = try c.decode(String.self, forKey: .intentId)
        matched = try c.decodeIfPresent(Bool.self, forKey: .matched) ?? false
    }
}

// MARK: - Push receipt (push-execute result)

/// `opensks.push-receipt.v1` — the result of a SUCCESSFUL execute. Carries the
/// remote oid the ref now points at, the idempotency key, and `alreadyDone` (true
/// on a repeat execute with the same evidence — the push happened exactly once).
struct GitPushReceipt: Codable, Sendable, Equatable {
    let schema: String
    let pushed: Bool
    let remoteOid: String
    let idempotencyKey: String
    let alreadyDone: Bool

    enum CodingKeys: String, CodingKey {
        case schema, pushed
        case remoteOid = "remote_oid"
        case idempotencyKey = "idempotency_key"
        case alreadyDone = "already_done"
    }

    init(
        schema: String = "opensks.push-receipt.v1",
        pushed: Bool,
        remoteOid: String,
        idempotencyKey: String,
        alreadyDone: Bool
    ) {
        self.schema = schema
        self.pushed = pushed
        self.remoteOid = remoteOid
        self.idempotencyKey = idempotencyKey
        self.alreadyDone = alreadyDone
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.push-receipt.v1"
        pushed = try c.decodeIfPresent(Bool.self, forKey: .pushed) ?? false
        remoteOid = try c.decodeIfPresent(String.self, forKey: .remoteOid) ?? ""
        idempotencyKey = try c.decodeIfPresent(String.self, forKey: .idempotencyKey) ?? ""
        alreadyDone = try c.decodeIfPresent(Bool.self, forKey: .alreadyDone) ?? false
    }

    /// First 8 chars of the remote oid for a compact, honest reference.
    var shortRemoteOid: String { String(remoteOid.prefix(8)) }
}

// MARK: - Push status (push-status result)

/// One completed push in the status surface — the intent it satisfied plus the
/// remote oid it landed at. Recovered from SQLite so it survives relaunch.
struct GitPushCompleted: Codable, Sendable, Equatable, Identifiable {
    let intentId: String
    let ref: String
    let remote: String
    let remoteOid: String

    var id: String { intentId }

    enum CodingKeys: String, CodingKey {
        case intentId = "intent_id"
        case ref, remote
        case remoteOid = "remote_oid"
    }

    init(intentId: String, ref: String, remote: String, remoteOid: String) {
        self.intentId = intentId
        self.ref = ref
        self.remote = remote
        self.remoteOid = remoteOid
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        intentId = try c.decode(String.self, forKey: .intentId)
        ref = try c.decodeIfPresent(String.self, forKey: .ref) ?? ""
        remote = try c.decodeIfPresent(String.self, forKey: .remote) ?? "origin"
        remoteOid = try c.decodeIfPresent(String.self, forKey: .remoteOid) ?? ""
    }

    var shortRemoteOid: String { String(remoteOid.prefix(8)) }
}

/// `opensks.push-status.v1` — the push outbox recovered from SQLite: intents that
/// are still pending approval, intents that are approved but not yet executed,
/// and completed pushes. Survives relaunch, so the studio can show in-flight work
/// after a restart.
struct GitPushStatus: Codable, Sendable, Equatable {
    let schema: String
    let pending: [GitPushIntent]
    let approved: [GitPushIntent]
    let completed: [GitPushCompleted]

    enum CodingKeys: String, CodingKey {
        case schema, pending, approved, completed
    }

    init(
        schema: String = "opensks.push-status.v1",
        pending: [GitPushIntent] = [],
        approved: [GitPushIntent] = [],
        completed: [GitPushCompleted] = []
    ) {
        self.schema = schema
        self.pending = pending
        self.approved = approved
        self.completed = completed
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decodeIfPresent(String.self, forKey: .schema) ?? "opensks.push-status.v1"
        pending = try c.decodeIfPresent([GitPushIntent].self, forKey: .pending) ?? []
        approved = try c.decodeIfPresent([GitPushIntent].self, forKey: .approved) ?? []
        completed = try c.decodeIfPresent([GitPushCompleted].self, forKey: .completed) ?? []
    }

    static let empty = GitPushStatus()

    var isEmpty: Bool { pending.isEmpty && approved.isEmpty && completed.isEmpty }
}

// MARK: - Store-side view state (not wire types)

/// The visible state of the EXPLICIT push approval prompt. Non-nil ⇒ a push has
/// been enqueued and is awaiting the operator's approval of the exact effect; the
/// prompt names the redacted remote, the ref, the local oid → remote-expected
/// oid, and a PROTECTED-BRANCH warning (which requires the extra ack toggle). The
/// push is NEVER executed until the operator approves from here.
struct GitPushPrompt: Equatable, Identifiable {
    /// The enqueued intent under review (the exact effect).
    let intent: GitPushIntent
    /// The operator's explicit acknowledgement of a protected-branch push. Must be
    /// true before a protected push can be approved (never auto-set).
    var ackProtected: Bool = false
    /// True while approve+execute are in flight (the buttons disable so one
    /// approval = one push attempt).
    var isWorking: Bool = false
    /// A non-fatal message surfaced inside the prompt (a digest mismatch, a failed
    /// push). The prompt stays open so the operator can re-review / retry.
    var notice: String?

    var id: String { intent.intentId }

    /// Approve is allowed only when a protected push has been explicitly ack'd and
    /// no approve/execute is already in flight.
    var canApprove: Bool {
        guard !isWorking else { return false }
        return intent.protected ? ackProtected : true
    }
}

/// Why a push approval/execute could not proceed. Distinct from the commit/stage
/// errors so the store reacts precisely: a digest mismatch must re-review (the oid
/// or ref moved); a protected branch needs the ack; a failed push is retryable
/// with the LOCAL commit preserved.
enum GitPushError: Error, Equatable {
    /// `digest_mismatch` — the supplied digest no longer matches the intent's
    /// current digest (the local oid or ref moved). NO usable approval is
    /// recorded; the operator must re-review the (new) effect.
    case digestMismatch
    /// `no_matching_approval` — execute was called without a recorded matching
    /// approval for the intent.
    case noMatchingApproval
    /// `protected_branch` — a protected ref was pushed without `--ack-protected`.
    case protectedBranch
    /// `push_failed` — the real `git push` failed (e.g. unreachable remote). The
    /// LOCAL commit + the pending intent are preserved for retry.
    case pushFailed(message: String?)
}

/// The push-outbox state owned by the store: the active approval prompt, the most
/// recent successful push receipt, the recovered push status (pending / approved
/// / completed), and the last non-fatal push error. Commit and push are SEPARATE:
/// this state is independent of `GitCommitComposerState` so a commit receipt can
/// stand while a push is still pending or failed.
struct GitPushOutboxState: Equatable {
    /// The active approval prompt, if a push is awaiting approval. Non-nil ⇒ the
    /// operator must approve the exact effect before any push runs.
    var prompt: GitPushPrompt?
    /// The most recent successful push receipt (the pushed remote oid).
    var receipt: GitPushReceipt?
    /// The intent of a push that FAILED at execute and is retryable (the local
    /// commit is preserved). Non-nil ⇒ the studio offers a Retry that re-opens the
    /// approval prompt for the same effect.
    var retryable: GitPushIntent?
    /// The push outbox recovered from SQLite (survives relaunch).
    var status: GitPushStatus = .empty
    /// A non-fatal banner for the last failed push step.
    var error: String?

    /// True when there is in-flight push work to surface (a prompt, a retryable
    /// failure, or recovered pending/approved intents).
    var hasOutbox: Bool {
        prompt != nil || retryable != nil || !status.pending.isEmpty || !status.approved.isEmpty
    }
}
